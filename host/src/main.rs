// Release builds on Windows are tray apps with no console window; terminal
// mode reattaches to the console it was started from.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod displays;
mod fit;
mod identity;
mod input;
#[cfg(target_os = "macos")]
mod mac_mouse;
mod pairing;
mod permissions;
mod protocol;
#[cfg(windows)]
mod secure;
mod server;
mod tray;
mod update;
mod video;

use std::sync::Arc;

use server::{Events, Host};

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    if let [_, flag, w, h] = &std::env::args().collect::<Vec<_>>()[..]
        && flag == "--fit-helper"
    {
        fit::helper_main(w.parse()?, h.parse()?);
    }
    if std::env::args().any(|a| a == "--version") {
        attach_console();
        println!("remote-duo-host {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    #[cfg(windows)]
    match std::env::args().nth(1).as_deref() {
        Some("--service") => return secure::run_service(),
        Some("--sign-in-helper") => return secure::run_helper(),
        Some("--sign-in-server") => return run_sign_in_server(),
        _ => {}
    }
    let host = Arc::new(Host::new()?);
    let runtime = tokio::runtime::Runtime::new()?;
    if std::env::args().any(|a| a == "--headless") || !tray::available() {
        return run_headless(host, runtime);
    }
    tray::run(host, runtime)
}

/// Terminal mode: print the PIN and QR code, log events, Ctrl+C to quit.
fn run_headless(host: Arc<Host>, runtime: tokio::runtime::Runtime) -> anyhow::Result<()> {
    attach_console();
    println!("Remote Duo host {} \"{}\" on port {}", env!("CARGO_PKG_VERSION"), host.name, host.port);
    if video::uses_cisco() {
        let state = if video::cisco::enabled() { "on" } else { "off" };
        println!("Screen view: {} ({state}; REMOTE_DUO_NO_OPENH264=1 turns it off)", video::cisco::NOTICE);
    }
    print_pairing(&host);
    std::thread::spawn(|| {
        if let Ok(Some(release)) = update::check() {
            println!("Remote Duo Host {} is available: {}", release.version, update::RELEASES_PAGE);
        }
    });
    let events: Events = Arc::new(|e| println!("{e}"));
    runtime.block_on(async {
        tokio::select! {
            _ = server::run(host.clone(), events) => {}
            _ = tokio::signal::ctrl_c() => {}
            _ = terminated() => {}
        }
    });
    host.shutdown();
    Ok(())
}

/// Release builds on Windows have no console of their own; print to the one
/// we were started from.
fn attach_console() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
}

/// Windows, before anyone signs in: the service runs this as SYSTEM so paired
/// phones can reach the sign-in screen. Same identity as the tray, which
/// takes over once someone signs in and the service ends this.
#[cfg(windows)]
fn run_sign_in_server() -> anyhow::Result<()> {
    let host = Arc::new(Host::with_pairing(pairing::Pairing::paired_only())?);
    secure::log(&format!("sign-in server listening on port {}", host.port));
    let events: Events = Arc::new(|e| secure::log(&format!("sign-in server: {e}")));
    tokio::runtime::Runtime::new()?.block_on(server::run(host, events));
    Ok(())
}

/// `kill` and service managers send SIGTERM; treat it like ctrl-c so we
/// still say goodbye on the network instead of lingering in phones' lists.
#[cfg(unix)]
async fn terminated() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut s) => {
            s.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn terminated() {
    std::future::pending().await
}

/// Print the PIN and a QR code the iPhone camera can scan to pair instantly.
fn print_pairing(host: &Host) {
    println!("Pairing PIN: {}", host.pairing.pin);
    let Some(url) = host.pairing_url() else { return };
    if let Ok(code) = qrcode::QrCode::new(url.as_bytes()) {
        let qr = code
            .render::<qrcode::render::unicode::Dense1x2>()
            .dark_color(qrcode::render::unicode::Dense1x2::Light)
            .light_color(qrcode::render::unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build();
        println!("\nScan with your iPhone camera to pair:\n{qr}");
    }
}
