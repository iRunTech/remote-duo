//! Owns the OS input injector on a dedicated thread and turns protocol
//! commands into synthetic mouse/keyboard events.

use std::sync::mpsc;
use std::thread;

#[cfg(not(target_os = "macos"))]
use enigo::Coordinate;
use enigo::{Axis, Button, Direction, Enigo, Key, Keyboard, Mouse, Settings};

use crate::protocol::{ClientMsg, MouseButton};

/// Points of scroll that make one wheel "line".
const SCROLL_STEP: f64 = 10.0;

pub type InputTx = mpsc::Sender<ClientMsg>;

pub fn spawn() -> anyhow::Result<InputTx> {
    let (tx, rx) = mpsc::channel::<ClientMsg>();
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            let mut inj = match Injector::new() {
                Ok(inj) => {
                    let _ = ready_tx.send(Ok(()));
                    inj
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            #[cfg(windows)]
            let mut sign_in = crate::secure::InputForwarder::default();
            for msg in rx {
                // Windows' sign-in screen only takes input from the sign-in
                // helper; hand it over while that screen is up.
                #[cfg(windows)]
                let msg = match sign_in.forward(msg) {
                    Some(msg) => msg,
                    None => continue,
                };
                inj.apply(msg);
            }
        })?;
    ready_rx.recv()??;
    Ok(tx)
}

pub struct Injector {
    enigo: Enigo,
    #[cfg(target_os = "macos")]
    mouse: crate::mac_mouse::Mouse,
    /// Sub-pixel remainders so slow finger motion isn't lost to rounding.
    rem: (f64, f64),
    scroll_rem: (f64, f64),
}

impl Injector {
    pub fn new() -> anyhow::Result<Self> {
        let enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow::anyhow!("input init failed: {e}"))?;
        Ok(Self {
            enigo,
            #[cfg(target_os = "macos")]
            mouse: crate::mac_mouse::Mouse::new(),
            rem: (0.0, 0.0),
            scroll_rem: (0.0, 0.0),
        })
    }

    pub fn apply(&mut self, msg: ClientMsg) {
        if let Err(e) = self.handle(msg) {
            eprintln!("input error: {e}");
        }
    }

    fn handle(&mut self, msg: ClientMsg) -> enigo::InputResult<()> {
        match msg {
            ClientMsg::Move { dx, dy } => {
                let (x, y) = take_whole(&mut self.rem, dx, dy, 1.0);
                if x != 0 || y != 0 {
                    self.move_rel(x, y)?;
                }
            }
            ClientMsg::MoveAbs { x, y, display } => {
                if let Some(d) = crate::displays::find(display) {
                    let (gx, gy) = d.point(x, y);
                    self.move_abs(gx, gy)?;
                }
            }
            ClientMsg::Click { b, n } => {
                for _ in 0..n.clamp(1, 3) {
                    self.click(b)?;
                }
            }
            ClientMsg::Button { b, down } => self.press(b, down)?,
            ClientMsg::Scroll { dx, dy } => {
                let (x, y) = take_whole(&mut self.scroll_rem, dx, dy, SCROLL_STEP);
                if y != 0 {
                    self.enigo.scroll(y, Axis::Vertical)?;
                }
                if x != 0 {
                    self.enigo.scroll(x, Axis::Horizontal)?;
                }
            }
            ClientMsg::Text { s } => self.enigo.text(&s)?,
            ClientMsg::Key { k, mods } => {
                let Some(key) = named_key(&k) else {
                    eprintln!("unknown key: {k}");
                    return Ok(());
                };
                let mods: Vec<Key> = mods.iter().filter_map(|m| modifier(m)).collect();
                for m in &mods {
                    self.enigo.key(*m, Direction::Press)?;
                }
                let res = self.enigo.key(key, Direction::Click);
                for m in mods.iter().rev() {
                    self.enigo.key(*m, Direction::Release)?;
                }
                res?;
            }
            ClientMsg::Hello { .. }
            | ClientMsg::Keyframe
            | ClientMsg::Ping
            | ClientMsg::Fit { .. }
            | ClientMsg::Unfit => {}
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl Injector {
    fn move_rel(&mut self, x: i32, y: i32) -> enigo::InputResult<()> {
        self.mouse.move_rel(x, y);
        Ok(())
    }
    /// Global coordinates (points), as from `Display::point`.
    fn move_abs(&mut self, x: f64, y: f64) -> enigo::InputResult<()> {
        self.mouse.move_to(x, y);
        Ok(())
    }
    fn click(&mut self, b: MouseButton) -> enigo::InputResult<()> {
        self.mouse.click(b);
        Ok(())
    }
    fn press(&mut self, b: MouseButton, down: bool) -> enigo::InputResult<()> {
        self.mouse.button(b, down);
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
impl Injector {
    fn move_rel(&mut self, x: i32, y: i32) -> enigo::InputResult<()> {
        self.enigo.move_mouse(x, y, Coordinate::Rel)
    }
    /// Global coordinates (pixels), as from `Display::point`.
    fn move_abs(&mut self, x: f64, y: f64) -> enigo::InputResult<()> {
        self.enigo.move_mouse(x.round() as i32, y.round() as i32, Coordinate::Abs)
    }
    fn click(&mut self, b: MouseButton) -> enigo::InputResult<()> {
        self.enigo.button(button(b), Direction::Click)
    }
    fn press(&mut self, b: MouseButton, down: bool) -> enigo::InputResult<()> {
        let dir = if down { Direction::Press } else { Direction::Release };
        self.enigo.button(button(b), dir)
    }
}

fn take_whole(rem: &mut (f64, f64), dx: f64, dy: f64, step: f64) -> (i32, i32) {
    rem.0 += dx / step;
    rem.1 += dy / step;
    let (x, y) = (rem.0.trunc(), rem.1.trunc());
    rem.0 -= x;
    rem.1 -= y;
    (x as i32, y as i32)
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn button(b: MouseButton) -> Button {
    match b {
        MouseButton::Left => Button::Left,
        MouseButton::Right => Button::Right,
        MouseButton::Middle => Button::Middle,
    }
}

/// `cmd` is the platform's primary shortcut key: ⌘ on macOS, Ctrl elsewhere.
fn modifier(m: &str) -> Option<Key> {
    Some(match m {
        "cmd" if cfg!(target_os = "macos") => Key::Meta,
        "cmd" => Key::Control,
        "ctrl" => Key::Control,
        "alt" | "option" => Key::Alt,
        "shift" => Key::Shift,
        "meta" | "win" | "super" => Key::Meta,
        _ => return None,
    })
}

fn named_key(k: &str) -> Option<Key> {
    Some(match k {
        "return" | "enter" => Key::Return,
        "tab" => Key::Tab,
        "space" => Key::Space,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "escape" | "esc" => Key::Escape,
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        "home" => Key::Home,
        "end" => Key::End,
        "page_up" => Key::PageUp,
        "page_down" => Key::PageDown,
        "volume_up" => Key::VolumeUp,
        "volume_down" => Key::VolumeDown,
        "mute" => Key::VolumeMute,
        "play_pause" => Key::MediaPlayPause,
        "next_track" => Key::MediaNextTrack,
        "prev_track" => Key::MediaPrevTrack,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        s if s.chars().count() == 1 => Key::Unicode(s.chars().next()?),
        _ => return None,
    })
}
