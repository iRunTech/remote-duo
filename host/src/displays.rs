//! The host's displays and the cursor, in one global coordinate space
//! (points on macOS, pixels elsewhere) so the phone can work in 0...1
//! fractions of whichever display it's viewing.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Display {
    pub id: u32,
    pub name: String,
    #[serde(skip)]
    pub x: f64,
    #[serde(skip)]
    pub y: f64,
    #[serde(rename = "w")]
    pub width: f64,
    #[serde(rename = "h")]
    pub height: f64,
    pub primary: bool,
}

impl Display {
    /// A 0...1 position on this display, in global coordinates.
    pub fn point(&self, fx: f64, fy: f64) -> (f64, f64) {
        (self.x + fx.clamp(0.0, 1.0) * (self.width - 1.0), self.y + fy.clamp(0.0, 1.0) * (self.height - 1.0))
    }

    /// Where a global position falls on this display, as 0...1 fractions.
    pub fn fraction(&self, (gx, gy): (f64, f64)) -> (f64, f64) {
        ((gx - self.x) / self.width, (gy - self.y) / self.height)
    }
}

pub fn list() -> Vec<Display> {
    let monitors = xcap::Monitor::all().unwrap_or_default();
    let mut out: Vec<Display> = monitors
        .iter()
        .filter_map(|m| {
            let id = m.id().ok()?;
            let name = m.name().unwrap_or_else(|_| format!("Display {id}"));
            let primary = m.is_primary().unwrap_or(false);
            #[cfg(target_os = "macos")]
            {
                let cg = core_graphics::display::CGDisplay::new(id);
                // A display mirroring another shows nothing of its own.
                if cg.mirrors_display() != 0 {
                    return None;
                }
                // Points, matching CGEvent's cursor location.
                let b = cg.bounds();
                Some(Display { id, name, x: b.origin.x, y: b.origin.y, width: b.size.width, height: b.size.height, primary })
            }
            #[cfg(not(target_os = "macos"))]
            {
                Some(Display {
                    id,
                    name,
                    x: m.x().ok()? as f64,
                    y: m.y().ok()? as f64,
                    width: m.width().ok()? as f64,
                    height: m.height().ok()? as f64,
                    primary,
                })
            }
        })
        .collect();
    out.sort_by_key(|d| (!d.primary, d.id));
    out
}

/// The display with this id, else the primary one.
pub fn find(id: Option<u32>) -> Option<Display> {
    let all = list();
    id.and_then(|id| all.iter().find(|d| d.id == id).cloned())
        .or_else(|| all.into_iter().next())
}

/// Cursor position in global coordinates, if the platform can tell us.
#[cfg(target_os = "macos")]
pub fn cursor() -> Option<(f64, f64)> {
    use core_graphics::event::CGEvent;
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok()?;
    let p = CGEvent::new(source).ok()?.location();
    Some((p.x, p.y))
}

#[cfg(not(target_os = "macos"))]
pub fn cursor() -> Option<(f64, f64)> {
    use enigo::{Enigo, Mouse, Settings};
    // On Windows' sign-in screen only the sign-in helper can see the cursor.
    #[cfg(windows)]
    if let Some(p) = crate::secure::sign_in_cursor() {
        return Some(p);
    }
    use std::cell::RefCell;
    thread_local! {
        static ENIGO: RefCell<Option<Enigo>> = const { RefCell::new(None) };
    }
    ENIGO.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            *cell = Enigo::new(&Settings::default()).ok();
        }
        let (x, y) = cell.as_ref()?.location().ok()?;
        Some((x as f64, y as f64))
    })
}
