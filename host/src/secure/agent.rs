//! The sign-in helper: a SYSTEM process in the user's session, started by the
//! service. Serves the tray app over a local pipe: input to apply and frames
//! to capture, on whichever desktop has the screen (including sign-in).

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTS_CURRENT_SERVER_HANDLE, WTS_SESSIONSTATE_LOCK, WTSFreeMemory, WTSINFOEXW,
    WTSQuerySessionInformationW, WTSSessionInfoEx,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, HDESK, OpenInputDesktop, SetThreadDesktop,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{HSTRING, PWSTR, w};

use super::capture::{Capture, Duplicator};
use super::{MODE_INPUT, MODE_VIDEO, PIPE, encode_frame, log, read_msg, write_msg};
use crate::input::Injector;
use crate::protocol::ClientMsg;

/// Frames per second while the sign-in screen is up; it barely moves.
const FPS: u64 = 15;

pub fn run() -> anyhow::Result<()> {
    log("sign-in helper started");
    let me = std::env::current_exe()?;
    let session = session_of(std::process::id()).unwrap_or(u32::MAX);
    let mut first = true;
    loop {
        let pipe = match create_pipe(first) {
            Ok(pipe) => pipe,
            Err(e) => {
                // On the first instance this means someone else holds the name.
                log(&format!("can't create the sign-in pipe: {e}"));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        first = false;
        if let Err(e) = unsafe { ConnectNamedPipe(pipe, None) }
            && e.code() != ERROR_PIPE_CONNECTED.to_hresult()
        {
            unsafe { CloseHandle(pipe).ok() };
            continue;
        }
        let file = unsafe { File::from_raw_handle(pipe.0) };
        let me = me.clone();
        std::thread::spawn(move || {
            if let Err(e) = serve(file, &me, session) {
                log(&format!("sign-in connection ended: {e}"));
            }
        });
    }
}

/// Only SYSTEM gets full control. Signed-in users may read and write, but not
/// create instances of their own (so no one can impersonate this end).
fn create_pipe(first: bool) -> windows::core::Result<HANDLE> {
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            // Owner SYSTEM; SYSTEM all access; interactive users read + write data.
            w!("O:SYG:SYD:(A;;GA;;;SY)(A;;0x12008b;;;IU)"),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )?;
        let attrs = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: false.into(),
        };
        let mut open_mode = PIPE_ACCESS_DUPLEX;
        if first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let pipe = CreateNamedPipeW(
            &HSTRING::from(PIPE),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            1 << 20,
            64 << 10,
            0,
            Some(&attrs),
        );
        LocalFree(Some(HLOCAL(sd.0)));
        if pipe == INVALID_HANDLE_VALUE {
            return Err(windows::core::Error::from_thread());
        }
        Ok(pipe)
    }
}

fn serve(mut pipe: File, me: &Path, session: u32) -> anyhow::Result<()> {
    check_client(&pipe, me, session)?;
    let mut mode = [0u8];
    pipe.read_exact(&mut mode)?;
    match mode[0] {
        MODE_INPUT => input(pipe, session),
        MODE_VIDEO => video(pipe),
        other => anyhow::bail!("unknown mode {other}"),
    }
}

/// The other end must be this same program (installed in Program Files,
/// which only administrators can change), running in this session.
fn check_client(pipe: &File, me: &Path, session: u32) -> anyhow::Result<()> {
    let mut pid = 0;
    unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid)? };
    let exe = image_path(pid)?;
    let same_exe = exe.as_os_str().to_string_lossy().eq_ignore_ascii_case(&me.as_os_str().to_string_lossy());
    if !same_exe || session_of(pid) != Some(session) {
        anyhow::bail!("refused {} (pid {pid})", exe.display());
    }
    Ok(())
}

fn image_path(pid: u32) -> windows::core::Result<PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        result?;
        Ok(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

fn session_of(pid: u32) -> Option<u32> {
    let mut session = 0;
    unsafe { ProcessIdToSessionId(pid, &mut session).ok()? };
    Some(session)
}

/// Input is only applied while the session is locked. The secure desktop
/// also hosts UAC prompts, and SYSTEM input on the user's own desktop could
/// drive elevated windows; either would let any program running as the user
/// (which can take over the tray app) approve its own elevation.
fn input(mut pipe: File, session: u32) -> anyhow::Result<()> {
    let mut locked = (false, Instant::now() - Duration::from_secs(1));
    // Reading the cursor position (for acceleration-free moves) fails on the
    // sign-in desktop, which dropped every mouse move.
    let mut injector = Injector::new(true)?;
    let mut last_error: Option<String> = None;
    let mut desktop = InputDesktop::default();
    loop {
        let msg: ClientMsg = serde_json::from_slice(&read_msg(&mut pipe)?)?;
        // Only input; nothing else the protocol carries means anything here.
        if !matches!(
            msg,
            ClientMsg::Move { .. }
                | ClientMsg::MoveAbs { .. }
                | ClientMsg::Click { .. }
                | ClientMsg::Button { .. }
                | ClientMsg::Scroll { .. }
                | ClientMsg::Text { .. }
                | ClientMsg::Key { .. }
        ) {
            continue;
        }
        if locked.1.elapsed() > Duration::from_millis(250) {
            locked = (session_locked(session), Instant::now());
        }
        if !locked.0 {
            let e = "ignoring input: the session isn't locked".to_string();
            if last_error.as_ref() != Some(&e) {
                log(&e);
                last_error = Some(e);
            }
            continue;
        }
        desktop.follow();
        if let Err(e) = injector.apply(msg) {
            // Once per distinct error, not per mouse move.
            let e = e.to_string();
            if last_error.as_ref() != Some(&e) {
                log(&format!("input error: {e}"));
                last_error = Some(e);
            }
        }
    }
}

fn session_locked(session: u32) -> bool {
    unsafe {
        let mut buf = PWSTR::null();
        let mut len = 0;
        if WTSQuerySessionInformationW(Some(WTS_CURRENT_SERVER_HANDLE), session, WTSSessionInfoEx, &mut buf, &mut len)
            .is_err()
        {
            return false;
        }
        let info = &*(buf.0 as *const WTSINFOEXW);
        // Level 1 is the only level; its flags say locked or unlocked.
        let locked = info.Level == 1 && info.Data.WTSInfoExLevel1.SessionFlags as u32 == WTS_SESSIONSTATE_LOCK;
        WTSFreeMemory(buf.0.cast());
        locked
    }
}

fn video(mut pipe: File) -> anyhow::Result<()> {
    let mut id = [0u8; 4];
    pipe.read_exact(&mut id)?;
    let monitor = u32::from_le_bytes(id);
    let mut desktop = InputDesktop::default();
    let interval = Duration::from_millis(1000 / FPS);
    let mut last_sent = Instant::now();
    loop {
        // Duplication only works from a thread on the desktop being shown.
        desktop.follow();
        let mut dup = match Duplicator::new(monitor) {
            Ok(dup) => dup,
            Err(e) => {
                log(&format!("can't capture the sign-in screen: {e}"));
                write_msg(&mut pipe, &encode_frame(0, 0, &[]))?;
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        loop {
            match dup.next(250) {
                Capture::Frame { width, height, rgba } => {
                    write_msg(&mut pipe, &encode_frame(width, height, &rgba))?;
                    last_sent = Instant::now();
                    std::thread::sleep(interval);
                }
                Capture::Unchanged => {
                    if last_sent.elapsed() > Duration::from_secs(1) {
                        write_msg(&mut pipe, &encode_frame(0, 0, &[]))?;
                        last_sent = Instant::now();
                    }
                }
                // The desktop switched under us (sign-in, UAC, or back).
                Capture::Lost => break,
            }
            if desktop.follow() {
                break;
            }
        }
        pipe.flush()?;
    }
}

/// Keeps a thread attached to whichever desktop currently gets input.
#[derive(Default)]
struct InputDesktop {
    current: Option<(HDESK, String)>,
    checked: Option<Instant>,
}

impl InputDesktop {
    /// Moves the thread to the input desktop if it changed; true if it did.
    fn follow(&mut self) -> bool {
        if self.checked.is_some_and(|t| t.elapsed() < Duration::from_millis(200)) {
            return false;
        }
        self.checked = Some(Instant::now());
        // GENERIC_ALL: reading, switching to, and sending input on it.
        let Ok(desk) = (unsafe { OpenInputDesktop(Default::default(), false, DESKTOP_ACCESS_FLAGS(0x1000_0000)) })
        else {
            return false;
        };
        let name = super::desktop_name(desk).unwrap_or_default();
        if self.current.as_ref().is_some_and(|(_, n)| *n == name) {
            unsafe { CloseDesktop(desk).ok() };
            return false;
        }
        if let Err(e) = unsafe { SetThreadDesktop(desk) } {
            log(&format!("can't switch to the {name} desktop: {e}"));
            unsafe { CloseDesktop(desk).ok() };
            return false;
        }
        if let Some((old, _)) = self.current.replace((desk, name)) {
            unsafe { CloseDesktop(old).ok() };
        }
        true
    }
}
