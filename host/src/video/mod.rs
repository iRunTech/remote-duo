//! Screen streaming: capture the display, encode H.264 in hardware, and hand
//! packets to the WebSocket as binary frames. Wire format (big-endian):
//!
//! - config: `0x01 | u16 width | u16 height | u16 len | SPS | u16 len | PPS`
//! - frame:  `0x02 | u8 keyframe | u64 pts_us | AVCC NAL units (4-byte lengths)`
//! - cursor: `0x03 | f32 x | f32 y | u8 draw` (0...1 on the display; `draw`
//!   means the video doesn't include the cursor, so the client should)
//!
//! A config packet precedes every keyframe so a client can join or recover
//! at any keyframe.

mod generic;
#[cfg(target_os = "macos")]
mod mac;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::displays::{self, Display};

pub enum Packet {
    Config { width: u16, height: u16, sps: Vec<u8>, pps: Vec<u8> },
    Frame { key: bool, pts_us: u64, data: Vec<u8> },
    Cursor { x: f32, y: f32, draw: bool },
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Packet::Config { width, height, sps, pps } => {
                let mut out = Vec::with_capacity(9 + sps.len() + pps.len());
                out.push(0x01);
                out.extend_from_slice(&width.to_be_bytes());
                out.extend_from_slice(&height.to_be_bytes());
                out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
                out.extend_from_slice(sps);
                out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
                out.extend_from_slice(pps);
                out
            }
            Packet::Frame { key, pts_us, data } => {
                let mut out = Vec::with_capacity(10 + data.len());
                out.push(0x02);
                out.push(*key as u8);
                out.extend_from_slice(&pts_us.to_be_bytes());
                out.extend_from_slice(data);
                out
            }
            Packet::Cursor { x, y, draw } => {
                let mut out = Vec::with_capacity(10);
                out.push(0x03);
                out.extend_from_slice(&x.to_be_bytes());
                out.extend_from_slice(&y.to_be_bytes());
                out.push(*draw as u8);
                out
            }
        }
    }
}

/// Shared between the encoder and the session that owns it.
#[derive(Default)]
pub struct Control {
    /// Next encoded frame should be a keyframe.
    pub force_key: AtomicBool,
}

/// A running capture. Dropping it stops capture and the encoder.
pub struct VideoSource {
    pub rx: mpsc::Receiver<Packet>,
    /// The display being streamed.
    pub display: Display,
    /// Whether the cursor is drawn into the video (otherwise the client draws it).
    pub cursor_in_video: bool,
    ctl: Arc<Control>,
    _running: Box<dyn Send>,
}

impl VideoSource {
    pub fn request_keyframe(&self) {
        self.ctl.force_key.store(true, Ordering::Relaxed);
    }
}

/// Starts capturing a display (the primary one if `display` is None or
/// unknown), scaled down to at most `max_width` pixels wide.
pub fn start(max_width: u32, display: Option<u32>) -> anyhow::Result<VideoSource> {
    let display = displays::find(display).ok_or_else(|| anyhow::anyhow!("no displays found"))?;

    #[cfg(target_os = "macos")]
    let (started, cursor_in_video) = if std::env::var_os("REMOTE_DUO_GENERIC_CAPTURE").is_some() {
        (generic::start(max_width, display.id), false)
    } else {
        (mac::start(max_width, display.id), true)
    };
    #[cfg(not(target_os = "macos"))]
    let (started, cursor_in_video) = (generic::start(max_width, display.id), false);

    let (rx, ctl, running) = started?;
    Ok(VideoSource { rx, display, cursor_in_video, ctl, _running: running })
}

/// Cursor position on `display`, ~20 times a second while it moves. Used to
/// draw the cursor when the video lacks one, and to follow it when zoomed.
pub fn cursor_updates(display: Display, draw: bool) -> mpsc::Receiver<Packet> {
    let (tx, rx) = mpsc::channel(8);
    std::thread::spawn(move || {
        let mut last = None;
        while !tx.is_closed() {
            if let Some(global) = displays::cursor() {
                let (x, y) = display.fraction(global);
                let p = (x as f32, y as f32);
                if last != Some(p) {
                    last = Some(p);
                    let _ = tx.try_send(Packet::Cursor { x: p.0, y: p.1, draw });
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    rx
}
