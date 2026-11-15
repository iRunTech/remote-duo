//! The Windows service: runs as SYSTEM and keeps one sign-in helper alive in
//! whichever session owns the PC's screen. It does nothing else.

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

use super::{SERVICE_NAME, log};

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

    let mut helper: Option<Helper> = None;
    while !stop.load(Ordering::Relaxed) {
        // Changes when someone else signs in at the PC, or with fast user switching.
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        let current = helper.as_ref().is_some_and(|h| h.session == session && h.running());
        if !current && session != u32::MAX {
            helper = None;
            match Helper::spawn(session) {
                Ok(h) => {
                    log(&format!("started sign-in helper in session {session}"));
                    helper = Some(h);
                }
                Err(e) => log(&format!("can't start sign-in helper in session {session}: {e}")),
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    drop(helper);
    log("service stopping");
    report(ServiceState::Stopped, ServiceControlAccept::empty())?;
    Ok(())
}

/// The helper process, ended when this is dropped.
struct Helper {
    process: HANDLE,
    session: u32,
}

impl Helper {
    /// Starts this exe as the helper in `session`, with the service's own
    /// SYSTEM token moved into that session.
    fn spawn(session: u32) -> windows::core::Result<Self> {
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
                let mut cmd = wide(&format!("\"{}\" --sign-in-helper", exe.display()));
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

impl Drop for Helper {
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
