//! Windows' sign-in screen. It lives on a separate, secure desktop that only
//! SYSTEM processes can type into, so the installer adds a small service
//! (`--service`) whose one job is to keep a sign-in helper (`--sign-in-helper`)
//! running in the session on the PC's screen. The tray app stays in charge of
//! the network and pairing; while the sign-in screen is up it hands input to
//! the helper and takes raw frames back, over a local pipe both ends verify.
//!
//! With "Start Automatically" on, the tray also hands the helper its identity
//! and paired phones, and while no one is signed in at all (after a restart)
//! the service runs a network server (`--sign-in-server`) of its own, so
//! phones can reach the sign-in screen before the tray exists.

mod agent;
mod capture;
mod service;

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows::Win32::Security::{
    IsWellKnownSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, WinBuiltinAdministratorsSid,
    WinLocalSystemSid,
};
use windows::Win32::Storage::FileSystem::{FILE_READ_DATA, FILE_WRITE_DATA, READ_CONTROL, SYNCHRONIZE};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetUserObjectInformationW, HDESK, OpenInputDesktop, UOI_NAME,
};
use xcap::Frame;

pub use agent::run as run_helper;
pub use service::run as run_service;

use windows::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_SESSIONSTATE_LOCK, WTSFreeMemory, WTSINFOEXW, WTSQuerySessionInformationW,
    WTSSessionInfoEx,
};
use windows::core::PWSTR;

use crate::protocol::ClientMsg;

pub const SERVICE_NAME: &str = "RemoteDuoSignIn";
const PIPE: &str = r"\\.\pipe\RemoteDuoSignIn";
/// First byte a client sends: what the connection is for.
const MODE_INPUT: u8 = b'I';
const MODE_VIDEO: u8 = b'V';
/// Store (or forget) the tray's identity and paired phones for the server
/// that runs before anyone signs in.
const MODE_KEYS: u8 = b'K';
/// Bigger than any frame (8K RGBA is 132 MB, but we cap capture well below).
const MAX_MSG: usize = 64 << 20;

/// Whether input currently goes to the user's own desktop. An ordinary
/// process can't open the sign-in desktop at all, so failing counts as "no".
pub fn on_default_desktop() -> bool {
    unsafe {
        match OpenInputDesktop(Default::default(), false, DESKTOP_READOBJECTS) {
            Ok(desk) => {
                let name = desktop_name(desk);
                let _ = CloseDesktop(desk);
                name.is_some_and(|n| n.eq_ignore_ascii_case("Default"))
            }
            Err(_) => false,
        }
    }
}

fn desktop_name(desk: HDESK) -> Option<String> {
    let mut name = [0u16; 64];
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            size_of_val(&name) as u32,
            None,
        )
        .ok()?;
    }
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    Some(String::from_utf16_lossy(&name[..len]))
}

pub struct SessionState {
    pub locked: bool,
    /// False at the sign-in screen after a restart, before anyone signs in.
    pub signed_in: bool,
}

pub fn session_state(session: u32) -> Option<SessionState> {
    unsafe {
        let mut buf = PWSTR::null();
        let mut len = 0;
        WTSQuerySessionInformationW(Some(WTS_CURRENT_SERVER_HANDLE), session, WTSSessionInfoEx, &mut buf, &mut len)
            .ok()?;
        let info = &*(buf.0 as *const WTSINFOEXW);
        // Level 1 is the only level.
        let state = (info.Level == 1).then(|| {
            let l1 = &info.Data.WTSInfoExLevel1;
            SessionState {
                locked: l1.SessionFlags as u32 == WTS_SESSIONSTATE_LOCK,
                signed_in: l1.UserName[0] != 0,
            }
        });
        WTSFreeMemory(buf.0.cast());
        state
    }
}

/// Which desktop the screen and keyboard belong to right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// The sign-in screen (or a UAC prompt) took over.
    ToSignIn,
    /// The user's own desktop is back.
    ToUser,
}

/// Polls the input desktop, at most twice a second, and reports changes.
pub struct Watch {
    signing_in: bool,
    checked: Instant,
}

impl Watch {
    pub fn new() -> Self {
        Self { signing_in: !on_default_desktop(), checked: Instant::now() }
    }

    /// Whether the sign-in screen was up at the last check.
    pub fn signing_in(&self) -> bool {
        self.signing_in
    }

    pub fn poll(&mut self) -> Option<Switch> {
        if self.checked.elapsed() < Duration::from_millis(500) {
            return None;
        }
        self.checked = Instant::now();
        let signing_in = !on_default_desktop();
        if signing_in == self.signing_in {
            return None;
        }
        self.signing_in = signing_in;
        Some(if signing_in { Switch::ToSignIn } else { Switch::ToUser })
    }
}

/// Sends input to the sign-in helper while the sign-in screen is up.
#[derive(Default)]
pub struct InputForwarder {
    pipe: Option<File>,
    checked: Option<Instant>,
    signing_in: bool,
    /// When connecting last failed, so a missing service isn't retried per keystroke.
    failed: Option<Instant>,
}

impl InputForwarder {
    /// Hands `msg` to the helper if it should go there; otherwise gives it
    /// back for the caller to apply as usual.
    pub fn forward(&mut self, msg: ClientMsg) -> Option<ClientMsg> {
        if self.checked.is_none_or(|t| t.elapsed() > Duration::from_millis(250)) {
            self.checked = Some(Instant::now());
            self.signing_in = !on_default_desktop();
        }
        if !self.signing_in {
            self.pipe = None;
            return Some(msg);
        }
        if self.pipe.is_none() && self.failed.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
            match connect(MODE_INPUT) {
                Ok(pipe) => self.pipe = Some(pipe),
                Err(e) => {
                    eprintln!("sign-in helper unavailable: {e}");
                    self.failed = Some(Instant::now());
                }
            }
        }
        let Some(pipe) = &mut self.pipe else { return Some(msg) };
        let json = serde_json::to_vec(&msg).expect("serializable");
        if write_msg(pipe, &json).is_err() {
            self.pipe = None;
            return Some(msg);
        }
        None
    }
}

/// Hands the helper what the server that runs before anyone signs in needs
/// to be this same computer to paired phones: the TLS identity and the paired
/// devices. `false` makes it forget them. They're kept in SYSTEM's own
/// profile, which other users can't read.
pub fn share_start_automatically(on: bool) -> anyhow::Result<()> {
    let mut pipe = connect(MODE_KEYS)?;
    if on {
        let dir = crate::identity::config_dir();
        write_msg(&mut pipe, &[1])?;
        for file in KEY_FILES {
            write_msg(&mut pipe, &std::fs::read(dir.join(file)).unwrap_or_default())?;
        }
    } else {
        write_msg(&mut pipe, &[0])?;
    }
    let mut ok = [0u8];
    pipe.read_exact(&mut ok)?;
    anyhow::ensure!(ok[0] == 1, "the sign-in helper couldn't store them");
    Ok(())
}

/// What `share_start_automatically` copies, in order.
const KEY_FILES: [&str; 3] = ["identity.der", "identity-key.der", "devices.json"];

/// Whether the tray shared its identity, i.e. "Start Automatically" is on
/// (as seen by SYSTEM, whose profile it lives in).
pub fn shared_keys_exist() -> bool {
    let dir = crate::identity::config_dir();
    KEY_FILES[..2].iter().all(|f| dir.join(f).exists())
}

/// Frames of the sign-in screen from the helper.
pub struct FrameStream {
    frames: Receiver<Frame>,
    stop: StopOnDrop,
}

impl FrameStream {
    /// The receiver, and a guard that ends the stream when dropped.
    pub fn split(self) -> (StopOnDrop, Receiver<Frame>) {
        (self.stop, self.frames)
    }
}

pub struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
        *SIGN_IN_CURSOR.lock().unwrap() = None;
    }
}

/// Where the helper says the cursor is, in global coordinates, while a frame
/// stream is open. Our own process can't read it on the sign-in desktop.
static SIGN_IN_CURSOR: std::sync::Mutex<Option<(f64, f64)>> = std::sync::Mutex::new(None);

pub fn sign_in_cursor() -> Option<(f64, f64)> {
    *SIGN_IN_CURSOR.lock().unwrap()
}

pub fn open_frames(display_id: u32) -> anyhow::Result<FrameStream> {
    let display = crate::displays::find(Some(display_id));
    let mut pipe = connect(MODE_VIDEO)?;
    pipe.write_all(&display_id.to_le_bytes())?;
    let (tx, frames) = sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    std::thread::Builder::new().name("sign-in-frames".into()).spawn(move || {
        // The helper sends a frame when the screen changes and an empty
        // heartbeat otherwise, so this notices `stop` within a second.
        while !stopped.load(Ordering::Relaxed) {
            let Ok(msg) = read_msg(&mut pipe) else { break };
            if let (Some((fx, fy)), Some(d)) = (decode_cursor(&msg), &display) {
                *SIGN_IN_CURSOR.lock().unwrap() = Some(d.point(fx as f64, fy as f64));
                continue;
            }
            if let Some(frame) = decode_frame(&msg)
                && tx.send(frame).is_err()
            {
                break;
            }
        }
    })?;
    Ok(FrameStream { frames, stop: StopOnDrop(stop) })
}

/// Frame message: width and height (u32 LE) then RGBA rows. Zero size is a heartbeat.
fn encode_frame(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(8 + rgba.len());
    msg.extend_from_slice(&width.to_le_bytes());
    msg.extend_from_slice(&height.to_le_bytes());
    msg.extend_from_slice(rgba);
    msg
}

/// Cursor message: a zero-size frame header, then x and y as f32 LE
/// fractions of the display.
fn encode_cursor(x: f32, y: f32) -> Vec<u8> {
    let mut msg = encode_frame(0, 0, &[]);
    msg.extend_from_slice(&x.to_le_bytes());
    msg.extend_from_slice(&y.to_le_bytes());
    msg
}

fn decode_cursor(msg: &[u8]) -> Option<(f32, f32)> {
    if msg.len() != 16 || msg[..8] != [0; 8] {
        return None;
    }
    Some((f32::from_le_bytes(msg[8..12].try_into().ok()?), f32::from_le_bytes(msg[12..16].try_into().ok()?)))
}

fn decode_frame(msg: &[u8]) -> Option<Frame> {
    let width = u32::from_le_bytes(msg.get(0..4)?.try_into().ok()?);
    let height = u32::from_le_bytes(msg.get(4..8)?.try_into().ok()?);
    let raw = &msg[8..];
    (width > 0 && raw.len() == (width * height * 4) as usize).then(|| Frame::new(width, height, raw.to_vec()))
}

/// Opens the helper's pipe, refusing one that SYSTEM didn't create: anyone
/// could squat on the name, and what flows through it includes passwords.
fn connect(mode: u8) -> anyhow::Result<File> {
    let mut pipe = std::fs::OpenOptions::new()
        // Exactly these rights: the helper grants users no more.
        .access_mode((FILE_READ_DATA | FILE_WRITE_DATA | READ_CONTROL | SYNCHRONIZE).0)
        .open(PIPE)?;
    if !owned_by_system(&pipe) {
        anyhow::bail!("the sign-in pipe isn't owned by SYSTEM");
    }
    pipe.write_all(&[mode])?;
    Ok(pipe)
}

fn owned_by_system(pipe: &File) -> bool {
    unsafe {
        let mut owner = PSID::default();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let err = GetSecurityInfo(
            HANDLE(pipe.as_raw_handle()),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut sd),
        );
        if err.is_err() {
            return false;
        }
        let ok = IsWellKnownSid(owner, WinLocalSystemSid).as_bool()
            || IsWellKnownSid(owner, WinBuiltinAdministratorsSid).as_bool();
        LocalFree(Some(HLOCAL(sd.0)));
        ok
    }
}

fn write_msg(w: &mut impl Write, msg: &[u8]) -> std::io::Result<()> {
    w.write_all(&(msg.len() as u32).to_le_bytes())?;
    w.write_all(msg)
}

fn read_msg(r: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MSG {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message too large"));
    }
    let mut msg = vec![0u8; len];
    r.read_exact(&mut msg)?;
    Ok(msg)
}

/// The service and helper have no console; they log next to the exe, in
/// Program Files, which only administrators (and SYSTEM) can write.
pub fn log(msg: &str) {
    let Some(path) = std::env::current_exe().ok().and_then(|p| Some(p.parent()?.join("sign-in.log"))) else {
        return;
    };
    let fresh = std::fs::metadata(&path).is_ok_and(|m| m.len() > 512 * 1024);
    let file = std::fs::OpenOptions::new().create(true).append(!fresh).write(true).truncate(fresh).open(&path);
    if let Ok(mut file) = file {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let _ = writeln!(file, "{secs} [{}] {msg}", std::process::id());
    }
}
