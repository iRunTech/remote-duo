//! The Windows service: runs as SYSTEM and keeps one sign-in helper alive in
//! whichever session owns the PC's screen. With "Start Automatically" on, it
//! also runs the network server there until someone signs in.

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS, TokenPrimary, TokenSessionId,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken, PROCESS_INFORMATION,
    STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use super::{SERVICE_NAME, log, session_state, shared_keys_exist};

define_windows_service!(ffi_service_main, service_main);

pub fn run() -> anyhow::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = supervise() {
        log(&format!("service stopped: {e}"));
    }
}

fn supervise() -> anyhow::Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let status = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            stopping.store(true, Ordering::Relaxed);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    let report = |state, accepted| {
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accepted,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
    };
    report(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN)?;
    log("service started");

    let mut helper: Option<Child> = None;
    let mut server: Option<Child> = None;
    while !stop.load(Ordering::Relaxed) {
        // Changes when someone else signs in at the PC, or with fast user switching.
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        if session != u32::MAX {
            keep(&mut helper, session, HELPER);
            // After a restart, phones can reach the sign-in screen through this
            // server. Once someone signs in, their tray app takes over.
            let before_sign_in = session_state(session).is_some_and(|s| !s.signed_in);
            if before_sign_in && shared_keys_exist() {
                keep(&mut server, session, SERVER);
            } else if server.take().is_some() {
                log("stopped the sign-in server: someone signed in");
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    drop(server);
    drop(helper);
    log("service stopping");
    report(ServiceState::Stopped, ServiceControlAccept::empty())?;
    Ok(())
}

const HELPER: &str = "--sign-in-helper";
const SERVER: &str = "--sign-in-server";

/// Makes sure `child` is this exe running with `arg` in `session`.
fn keep(child: &mut Option<Child>, session: u32, arg: &str) {
    if child.as_ref().is_some_and(|c| c.session == session && c.running()) {
        return;
    }
    *child = None;
    match Child::spawn(session, arg) {
        Ok(c) => {
            log(&format!("started {arg} in session {session}"));
            *child = Some(c);
        }
        Err(e) => log(&format!("can't start {arg} in session {session}: {e}")),
    }
}

/// A process of ours in the console session, ended when this is dropped.
struct Child {
    process: HANDLE,
    session: u32,
}

impl Child {
    /// Starts this exe with `arg` in `session`, with the service's own
    /// SYSTEM token moved into that session.
    fn spawn(session: u32, arg: &str) -> windows::core::Result<Self> {
        unsafe {
            let mut own = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own)?;
            let mut token = HANDLE::default();
            let duplicated = DuplicateTokenEx(own, TOKEN_ALL_ACCESS, None, SecurityImpersonation, TokenPrimary, &mut token);
            let _ = CloseHandle(own);
            duplicated?;
            let result = (|| {
                SetTokenInformation(token, TokenSessionId, (&raw const session).cast(), size_of::<u32>() as u32)?;
                let exe = std::env::current_exe().map_err(|_| windows::core::Error::from_thread())?;
                let mut cmd = wide(&format!("\"{}\" {arg}", exe.display()));
                let mut desktop = wide(r"winsta0\default");
                let startup = STARTUPINFOW {
                    cb: size_of::<STARTUPINFOW>() as u32,
                    lpDesktop: PWSTR(desktop.as_mut_ptr()),
                    ..Default::default()
                };
                let mut info = PROCESS_INFORMATION::default();
                CreateProcessAsUserW(
                    Some(token),
                    PCWSTR::null(),
                    Some(PWSTR(cmd.as_mut_ptr())),
                    None,
                    None,
                    false,
                    CREATE_NO_WINDOW,
                    None,
                    PCWSTR::null(),
                    &startup,
                    &mut info,
                )?;
                let _ = CloseHandle(info.hThread);
                Ok(Self { process: info.hProcess, session })
            })();
            let _ = CloseHandle(token);
            result
        }
    }

    fn running(&self) -> bool {
        unsafe { WaitForSingleObject(self.process, 0) == WAIT_TIMEOUT }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            let _ = TerminateProcess(self.process, 0);
            let _ = CloseHandle(self.process);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
