//! "Fit to Duo": give the computer a screen shaped like the phone's screen
//! pane, so the screen view fills it with no black bars.
//!
//! One worker thread owns the virtual display. Phones (by connection id)
//! ask for a size; the display stays while any of them wants it, and lingers
//! briefly after the last one drops so a phone reconnecting from the
//! background doesn't make every window jump around.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use tokio::sync::oneshot;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub use mac::helper_main;

/// How long the display outlives a dropped connection.
const GRACE: Duration = Duration::from_secs(30);

pub const AVAILABLE: bool = cfg!(target_os = "macos");

/// Real pixel size of the phone-shaped display. The host learns about display
/// changes late, so screen capture asks here instead of trusting CoreGraphics.
static PIXELS: Mutex<Option<(u32, (u32, u32))>> = Mutex::new(None);

pub fn pixel_size(display: u32) -> Option<(u32, u32)> {
    PIXELS.lock().unwrap().filter(|(id, _)| *id == display).map(|(_, px)| px)
}

enum Cmd {
    Set {
        owner: u64,
        width: u32,
        height: u32,
        reply: oneshot::Sender<Result<u32, String>>,
    },
    /// `done`: the phone turned it off (rather than going away); answered
    /// once the display is gone.
    Clear {
        owner: u64,
        done: Option<oneshot::Sender<()>>,
    },
}

pub struct Fit {
    tx: Mutex<mpsc::Sender<Cmd>>,
}

impl Fit {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("fit".into())
            .spawn(move || worker(rx))
            .expect("spawn fit worker");
        Self { tx: Mutex::new(tx) }
    }

    /// Make (or resize) the phone-shaped display; returns its display id.
    /// Sizes are in points; the display renders at 2x.
    pub async fn set(&self, owner: u64, width: u32, height: u32) -> Result<u32, String> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Set {
            owner,
            width,
            height,
            reply,
        });
        rx.await.unwrap_or_else(|_| Err("Fit to Duo stopped working".into()))
    }

    /// The phone went away; the display lingers for `GRACE`.
    pub fn release(&self, owner: u64) {
        self.send(Cmd::Clear { owner, done: None });
    }

    /// The phone turned it off; returns once the real displays are back.
    pub async fn clear(&self, owner: u64) {
        let (done, rx) = oneshot::channel();
        self.send(Cmd::Clear {
            owner,
            done: Some(done),
        });
        let _ = rx.await;
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.lock().unwrap().send(cmd);
    }
}

#[cfg(target_os = "macos")]
fn worker(rx: mpsc::Receiver<Cmd>) {
    let mut display: Option<mac::Helper> = None;
    let mut owners = HashSet::new();
    loop {
        let cmd = if display.is_some() && owners.is_empty() {
            match rx.recv_timeout(GRACE) {
                Ok(cmd) => cmd,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    mac::autoreleasing(|| display = None);
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(cmd) => cmd,
                Err(_) => return,
            }
        };
        mac::autoreleasing(|| handle(cmd, &mut display, &mut owners));
    }
}

#[cfg(target_os = "macos")]
fn handle(cmd: Cmd, display: &mut Option<mac::Helper>, owners: &mut HashSet<u64>) {
    match cmd {
        Cmd::Set {
            owner,
            width,
            height,
            reply,
        } => {
            let result = match display.as_mut() {
                Some(d) => d.resize(width, height).map(|_| d.id),
                None => mac::Helper::new(width, height).map(|d| {
                    let id = d.id;
                    *display = Some(d);
                    id
                }),
            };
            *PIXELS.lock().unwrap() = display.as_ref().map(|d| (d.id, d.pixels));
            if result.is_ok() {
                owners.insert(owner);
            } else if owners.is_empty() {
                *display = None;
            }
            let _ = reply.send(result.map_err(|e| {
                eprintln!("Fit to Duo: {e:#}");
                format!("{e:#}")
            }));
        }
        Cmd::Clear { owner, done } => {
            if owners.remove(&owner) && done.is_some() && owners.is_empty() {
                *display = None;
                *PIXELS.lock().unwrap() = None;
            }
            if let Some(done) = done {
                let _ = done.send(());
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn worker(rx: mpsc::Receiver<Cmd>) {
    let _ = (&GRACE, HashSet::<u64>::new());
    while let Ok(cmd) = rx.recv() {
        if let Cmd::Set { reply, .. } = cmd {
            let _ = reply.send(Err(format!(
                "Fit to Duo isn't available on {} yet",
                std::env::consts::OS
            )));
        }
    }
}
