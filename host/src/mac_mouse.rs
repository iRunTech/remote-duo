//! macOS pointer injection via CoreGraphics. enigo 0.6 flips Y using the
//! display's pixel height while reading the cursor in points, which makes
//! relative moves and clicks drift off-screen on Retina and multi-display
//! setups, so we drive the mouse ourselves here.

use std::time::{Duration, Instant};

use core_graphics::display::{CGDisplay, CGPoint, CGRect};
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton, EventField};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

use crate::protocol::MouseButton;

const DOUBLE_CLICK: Duration = Duration::from_millis(450);

pub struct Mouse {
    held: Option<MouseButton>,
    last_down: Option<(Instant, MouseButton)>,
    clicks: i64,
}

impl Mouse {
    pub fn new() -> Self {
        Self { held: None, last_down: None, clicks: 0 }
    }

    pub fn move_rel(&mut self, dx: i32, dy: i32) {
        let cur = location();
        let dest = clamp_to_displays(CGPoint::new(cur.x + dx as f64, cur.y + dy as f64));
        let (ty, button) = match self.held {
            Some(MouseButton::Left) => (CGEventType::LeftMouseDragged, CGMouseButton::Left),
            Some(MouseButton::Right) => (CGEventType::RightMouseDragged, CGMouseButton::Right),
            Some(MouseButton::Middle) => (CGEventType::OtherMouseDragged, CGMouseButton::Center),
            None => (CGEventType::MouseMoved, CGMouseButton::Left),
        };
        if let Some(event) = mouse_event(ty, dest, button) {
            event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, dx as i64);
            event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, dy as i64);
            event.post(CGEventTapLocation::HID);
        }
    }

    /// Jump to a global position (points).
    pub fn move_to(&mut self, x: f64, y: f64) {
        let cur = location();
        self.move_rel((x - cur.x).round() as i32, (y - cur.y).round() as i32);
    }

    pub fn button(&mut self, b: MouseButton, down: bool) {
        if down {
            // macOS only reports a double click if we count consecutive clicks.
            let now = Instant::now();
            self.clicks = match self.last_down {
                Some((t, prev)) if prev == b && now - t < DOUBLE_CLICK => self.clicks + 1,
                _ => 1,
            };
            self.last_down = Some((now, b));
            self.held = Some(b);
        } else {
            self.held = None;
        }
        let (ty, button) = match (b, down) {
            (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
            (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
            (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
            (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
            (MouseButton::Middle, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (MouseButton::Middle, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        };
        if let Some(event) = mouse_event(ty, location(), button) {
            event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, self.clicks.max(1));
            event.post(CGEventTapLocation::HID);
        }
    }

    pub fn click(&mut self, b: MouseButton) {
        self.button(b, true);
        self.button(b, false);
    }
}

fn source() -> Option<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()
}

fn mouse_event(ty: CGEventType, at: CGPoint, button: CGMouseButton) -> Option<CGEvent> {
    CGEvent::new_mouse_event(source()?, ty, at, button).ok()
}

/// Cursor position in global display points, origin top-left of the main display.
fn location() -> CGPoint {
    source()
        .and_then(|s| CGEvent::new(s).ok())
        .map(|e| e.location())
        .unwrap_or(CGPoint::new(0.0, 0.0))
}

/// Keep the cursor on some display rather than in the gaps between them.
fn clamp_to_displays(p: CGPoint) -> CGPoint {
    let rects: Vec<CGRect> = CGDisplay::active_displays()
        .unwrap_or_default()
        .into_iter()
        .map(|id| CGDisplay::new(id).bounds())
        .collect();
    let clamp = |r: &CGRect| {
        CGPoint::new(
            p.x.clamp(r.origin.x, r.origin.x + r.size.width - 1.0),
            p.y.clamp(r.origin.y, r.origin.y + r.size.height - 1.0),
        )
    };
    rects
        .iter()
        .map(clamp)
        .min_by(|a, b| {
            let da = (a.x - p.x).powi(2) + (a.y - p.y).powi(2);
            let db = (b.x - p.x).powi(2) + (b.y - p.y).powi(2);
            da.total_cmp(&db)
        })
        .unwrap_or(p)
}
