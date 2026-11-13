//! Wire protocol: JSON text frames over a WebSocket, tagged by `t`.
//! See /protocol/PROTOCOL.md for the human-readable spec.

use serde::{Deserialize, Serialize};

use crate::displays::Display;

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// First frame on every connection. Send `pin` to pair, `token` to reconnect.
    Hello {
        name: String,
        pin: Option<String>,
        token: Option<String>,
        /// `"video"` opens a screen-stream connection; absent means control.
        #[serde(default)]
        channel: Option<String>,
        /// Video only: widest frame the client wants, in pixels.
        #[serde(default)]
        max_width: Option<u32>,
        /// Video only: which display to stream (id from `welcome`); default primary.
        #[serde(default)]
        display: Option<u32>,
    },
    /// Relative pointer motion in points.
    Move { dx: f64, dy: f64 },
    /// Put the pointer at a spot on a display, 0...1 from its top-left.
    MoveAbs {
        x: f64,
        y: f64,
        /// Display id from `welcome`; default primary.
        #[serde(default)]
        display: Option<u32>,
    },
    /// Full click(s). `n` = 2 for double click.
    Click {
        #[serde(default)]
        b: MouseButton,
        #[serde(default = "one")]
        n: u8,
    },
    /// Press or release a button, used for drag.
    Button {
        #[serde(default)]
        b: MouseButton,
        down: bool,
    },
    /// Scroll in points. Positive dy reveals content below, positive dx content to the right.
    Scroll { dx: f64, dy: f64 },
    /// Type a string of text.
    Text { s: String },
    /// Press a named key, optionally with modifiers held.
    Key {
        k: String,
        #[serde(default)]
        mods: Vec<String>,
    },
    /// Give the computer a screen this size, in points (rendered at 2x),
    /// with its real displays mirroring it. Answered with `fitted`.
    Fit { w: u32, h: u32 },
    /// Undo `fit`. Answered with `fitted` with no display.
    Unfit,
    /// Video channel: send a keyframe now (decoder lost sync).
    Keyframe,
    Ping,
}

#[derive(Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostMsg {
    Welcome {
        token: String,
        host: String,
        os: &'static str,
        displays: Vec<Display>,
        /// Whether this host can do `fit`.
        fit: bool,
    },
    /// Result of `fit`/`unfit`: the display to stream (none when unfit or
    /// on failure) and the displays as they are now.
    Fitted {
        display: Option<u32>,
        displays: Vec<Display>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Error {
        msg: String,
    },
    Pong,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

fn one() -> u8 {
    1
}
