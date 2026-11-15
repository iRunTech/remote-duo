//! Windows' sign-in screen. It lives on a separate, secure desktop that only
//! SYSTEM processes can type into, so the installer adds a small service
//! (`--service`) whose one job is to keep a sign-in helper (`--sign-in-helper`)
//! running in the session on the PC's screen. The tray app stays in charge of
//! the network and pairing; while the sign-in screen is up it hands input to
//! the helper and takes raw frames back, over a local pipe both ends verify.

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

use crate::protocol::ClientMsg;

pub const SERVICE_NAME: &str = "RemoteDuoSignIn";
const PIPE: &str = r"\\.\pipe\RemoteDuoSignIn";
/// First byte a client sends: what the connection is for.
const MODE_INPUT: u8 = b'I';
const MODE_VIDEO: u8 = b'V';
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
        Self { signing_in: false, checked: Instant::now() - Duration::from_secs(1) }
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
    }
}

pub fn open_frames(display_id: u32) -> anyhow::Result<FrameStream> {
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
fn log(msg: &str) {
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
