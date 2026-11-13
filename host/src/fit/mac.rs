//! A virtual monitor shaped like the phone's screen pane, with the computer's
//! real displays mirroring it. Built on CoreGraphics' CGVirtualDisplay
//! classes, which Apple ships but doesn't document, so everything goes
//! through the Objective-C runtime and fails soft if they ever disappear.
//!
//! The display lives in a helper process (this same binary, run with
//! `--fit-helper`). Once a process has reconfigured displays, macOS stops
//! updating its view of them, so the host itself never does; and when the
//! helper exits, for any reason, macOS removes the display and undoes the
//! mirroring (it's `ConfigureForAppOnly`) by itself.

// `msg!` wraps itself in `unsafe` so it works anywhere, including inside other unsafe blocks.
#![allow(unused_unsafe)]

use std::ffi::{CString, c_char, c_void};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_graphics::display::{
    CGConfigureOption, CGDirectDisplayID, CGDisplay, kCGDisplayShowDuplicateLowResolutionModes,
};
use core_graphics::geometry::CGSize;

type Id = *mut c_void;
type Sel = *const c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    fn objc_release(obj: Id);
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

/// Runs `f` inside an autorelease pool. Our worker thread has none of its
/// own, and without one the objects CoreGraphics autoreleases (including
/// references to the display itself) would never go away.
pub fn autoreleasing<R>(f: impl FnOnce() -> R) -> R {
    let pool = unsafe { objc_autoreleasePoolPush() };
    let result = f();
    unsafe { objc_autoreleasePoolPop(pool) };
    result
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetOnlineDisplayList(max: u32, displays: *mut CGDirectDisplayID, count: *mut u32) -> i32;
    fn CGDisplayCopyAllDisplayModes(display: CGDirectDisplayID, options: *const c_void) -> *const c_void;
    fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> *const c_void;
    fn CGDisplayModeRelease(mode: *const c_void);
    fn CGDisplayModeGetWidth(mode: *const c_void) -> usize;
    fn CGDisplayModeGetHeight(mode: *const c_void) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize;
    fn CGBeginDisplayConfiguration(config: *mut *mut c_void) -> i32;
    fn CGConfigureDisplayWithDisplayMode(
        config: *mut c_void,
        display: CGDirectDisplayID,
        mode: *const c_void,
        options: *const c_void,
    ) -> i32;
    fn CGCancelDisplayConfiguration(config: *mut c_void) -> i32;
    fn CGCompleteDisplayConfiguration(config: *mut c_void, option: u32) -> i32;
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
    fn dispatch_release(obj: *mut c_void);
}

fn sel(name: &str) -> Sel {
    let name = CString::new(name).unwrap();
    unsafe { sel_registerName(name.as_ptr()) }
}

fn class(name: &str) -> anyhow::Result<Id> {
    let c = CString::new(name).unwrap();
    let class = unsafe { objc_getClass(c.as_ptr()) };
    if class.is_null() {
        bail!("this version of macOS doesn't support virtual displays");
    }
    Ok(class)
}

/// objc_msgSend, cast to the signature of the method being called.
macro_rules! msg {
    ($obj:expr, $sel:expr $(, $arg:expr => $ty:ty)* ; -> $ret:ty) => {{
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel $(, $ty)*) -> $ret =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            f($obj, sel($sel) $(, $arg)*)
        }
    }};
}

unsafe fn new_object(class_name: &str) -> anyhow::Result<Id> {
    let class = class(class_name)?;
    let obj: Id = msg!(class, "alloc"; -> Id);
    let obj: Id = msg!(obj, "init"; -> Id);
    if obj.is_null() {
        bail!("couldn't create {class_name}");
    }
    Ok(obj)
}

pub struct VirtualDisplay {
    display: Id,
    queue: *mut c_void,
    pub id: u32,
    /// Current size in points; pixels are double that.
    size: (u32, u32),
}

// Only ever touched from the fit worker thread.
unsafe impl Send for VirtualDisplay {}

/// Largest side in points we'll make; keeps the pixel buffer sane.
const MAX_POINTS: u32 = 3200;

impl VirtualDisplay {
    pub fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        unsafe {
            let desc = new_object("CGVirtualDisplayDescriptor")?;
            let queue = dispatch_queue_create(c"remote-duo.virtual-display".as_ptr(), std::ptr::null());
            let name = CFString::new("Remote Duo");
            let _: () = msg!(desc, "setQueue:", queue => *mut c_void; -> ());
            let _: () = msg!(desc, "setName:", name.as_concrete_TypeRef() as Id => Id; -> ());
            let _: () = msg!(desc, "setMaxPixelsWide:", MAX_POINTS * 2 => u32; -> ());
            let _: () = msg!(desc, "setMaxPixelsHigh:", MAX_POINTS * 2 => u32; -> ());
            // A Retina-laptop-like density, so macOS treats it as a HiDPI panel.
            let mm = |points: u32| f64::from(points * 2) / 220.0 * 25.4;
            let _: () = msg!(desc, "setSizeInMillimeters:", CGSize::new(mm(width), mm(height)) => CGSize; -> ());
            let _: () = msg!(desc, "setVendorID:", 0x5244 => u32; -> ());
            let _: () = msg!(desc, "setProductID:", 0x0001 => u32; -> ());
            let _: () = msg!(desc, "setSerialNum:", 0x0001 => u32; -> ());

            let class = class("CGVirtualDisplay")?;
            let display: Id = msg!(class, "alloc"; -> Id);
            let display: Id = msg!(display, "initWithDescriptor:", desc => Id; -> Id);
            objc_release(desc);
            if display.is_null() {
                dispatch_release(queue);
                bail!("macOS refused to create a virtual display");
            }
            let id: u32 = msg!(display, "displayID"; -> u32);
            let mut vd = Self {
                display,
                queue,
                id,
                size: (0, 0),
            };
            vd.resize(width, height)?;
            Ok(vd)
        }
    }

    /// Switch to a new size (in points), rendered at 2x.
    pub fn resize(&mut self, width: u32, height: u32) -> anyhow::Result<()> {
        let (width, height) = clamp_size(width, height);
        if self.size == (width, height) {
            return Ok(());
        }
        unsafe {
            let settings = new_object("CGVirtualDisplaySettings")?;
            let _: () = msg!(settings, "setHiDPI:", 1 => u32; -> ());
            let mode_class = class("CGVirtualDisplayMode")?;
            let mode: Id = msg!(mode_class, "alloc"; -> Id);
            let mode: Id = msg!(mode, "initWithWidth:height:refreshRate:",
                width => u32, height => u32, 60.0 => f64; -> Id);
            let array: Id = msg!(class("NSArray")?, "arrayWithObject:", mode => Id; -> Id);
            let _: () = msg!(settings, "setModes:", array => Id; -> ());
            let ok: u8 = msg!(self.display, "applySettings:", settings => Id; -> u8);
            objc_release(mode);
            objc_release(settings);
            if ok == 0 {
                bail!("macOS rejected a {width}×{height} virtual display");
            }
        }
        self.use_retina_mode(width, height)?;
        self.size = (width, height);
        Ok(())
    }

    /// macOS offers the size both at 1x and 2x; pick 2x so text is crisp.
    /// The display shows up a moment after it's created, so wait for it.
    fn use_retina_mode(&self, width: u32, height: u32) -> anyhow::Result<()> {
        // Raw calls: core-graphics' mode list over-releases its modes.
        let options = CFDictionary::from_CFType_pairs(&[(
            unsafe { CFString::wrap_under_get_rule(kCGDisplayShowDuplicateLowResolutionModes) },
            CFBoolean::true_value(),
        )]);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            unsafe {
                let current = CGDisplayCopyDisplayMode(self.id);
                if !current.is_null() {
                    let (w, h, pw) = (
                        CGDisplayModeGetWidth(current),
                        CGDisplayModeGetHeight(current),
                        CGDisplayModeGetPixelWidth(current),
                    );
                    CGDisplayModeRelease(current);
                    if (w, h, pw) == (width as usize, height as usize, width as usize * 2) {
                        return Ok(());
                    }
                }
                let modes = CGDisplayCopyAllDisplayModes(self.id, options.as_concrete_TypeRef() as *const c_void);
                if !modes.is_null() {
                    let modes = CFArray::<*const c_void>::wrap_under_create_rule(modes as _);
                    let retina = modes.iter().map(|m| *m).find(|&m| {
                        CGDisplayModeGetWidth(m) == width as usize
                            && CGDisplayModeGetHeight(m) == height as usize
                            && CGDisplayModeGetPixelWidth(m) == width as usize * 2
                    });
                    if let Some(mode) = retina {
                        let mut config = std::ptr::null_mut();
                        check(CGBeginDisplayConfiguration(&mut config)).context("begin mode change")?;
                        let set = check(CGConfigureDisplayWithDisplayMode(
                            config,
                            self.id,
                            mode,
                            std::ptr::null(),
                        ));
                        // Session scope: the setting belongs to our display alone and dies with it.
                        let done = match set {
                            Ok(()) => check(CGCompleteDisplayConfiguration(
                                config,
                                CGConfigureOption::ConfigureForSession as u32,
                            )),
                            Err(e) => {
                                CGCancelDisplayConfiguration(config);
                                Err(e)
                            }
                        };
                        match done {
                            Ok(()) => return Ok(()),
                            // A brand-new display can refuse changes for a moment.
                            Err(e) if Instant::now() > deadline => return Err(e).context("set Retina mode"),
                            Err(e) => eprintln!("virtual display {}: {e}, retrying", self.id),
                        }
                    }
                }
            }
            if Instant::now() > deadline {
                bail!("the virtual display never offered a Retina {width}×{height} mode");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Make every other display show this one.
    pub fn mirror_others(&self) -> anyhow::Result<()> {
        let master = CGDisplay::new(self.id);
        let config = master
            .begin_configuration()
            .map_err(cg_error)
            .context("begin mirroring")?;
        for id in online_displays().into_iter().filter(|&d| d != self.id) {
            CGDisplay::new(id)
                .configure_display_mirror_of_display(&config, &master)
                .map_err(cg_error)
                .with_context(|| format!("mirror display {id}"))?;
        }
        master
            .complete_configuration(&config, CGConfigureOption::ConfigureForAppOnly)
            .map_err(cg_error)
            .context("apply mirroring")?;
        Ok(())
    }

    fn unmirror_others(&self) {
        let none = CGDisplay::new(0);
        let Ok(config) = none.begin_configuration() else { return };
        for id in online_displays() {
            let d = CGDisplay::new(id);
            if d.mirrors_display() == self.id {
                let _ = d.configure_display_mirror_of_display(&config, &none);
            }
        }
        let _ = none.complete_configuration(&config, CGConfigureOption::ConfigureForAppOnly);
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        self.unmirror_others();
        unsafe {
            objc_release(self.display);
            dispatch_release(self.queue);
        }
    }
}

/// macOS won't run a desktop on a 2x mode under 800×500 points, so grow
/// small requests (keeping their shape) and cap huge ones.
fn clamp_size(width: u32, height: u32) -> (u32, u32) {
    let (w, h) = (f64::from(width.max(1)), f64::from(height.max(1)));
    let grow = (800.0 / w).max(500.0 / h).max(1.0);
    let shrink = (f64::from(MAX_POINTS) / (w * grow))
        .min(f64::from(MAX_POINTS) / (h * grow))
        .min(1.0);
    let scale = grow * shrink;
    // Even sizes keep the 2x pixel buffer and the video encoder happy.
    let even = |v: f64| ((v * scale / 2.0).round() as u32 * 2).max(2);
    (even(w), even(h))
}

fn online_displays() -> Vec<CGDirectDisplayID> {
    let mut ids = [0u32; 16];
    let mut count = 0;
    if unsafe { CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
        return Vec::new();
    }
    ids[..count as usize].to_vec()
}

fn check(code: i32) -> anyhow::Result<()> {
    if code == 0 { Ok(()) } else { Err(cg_error(code)) }
}

fn cg_error(code: i32) -> anyhow::Error {
    anyhow::anyhow!("display configuration failed (CoreGraphics error {code})")
}

/// The host's handle on the helper process that owns the display.
pub struct Helper {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pub id: u32,
    /// Size in real pixels, as the helper set it.
    pub pixels: (u32, u32),
}

impl Helper {
    pub fn new(width: u32, height: u32) -> anyhow::Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .args(["--fit-helper", &width.to_string(), &height.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("start the display helper")?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut helper = Self { child, stdin, stdout, id: 0, pixels: (0, 0) };
        helper.reply()?;
        Ok(helper)
    }

    pub fn resize(&mut self, width: u32, height: u32) -> anyhow::Result<()> {
        writeln!(self.stdin, "{width} {height}").context("talk to the display helper")?;
        self.reply()
    }

    /// "ok <display id> <pixel width> <pixel height>" or "err <message>".
    fn reply(&mut self) -> anyhow::Result<()> {
        let mut line = String::new();
        self.stdout.read_line(&mut line).context("display helper")?;
        match line.trim().split_once(' ') {
            Some(("ok", rest)) => {
                let n: Vec<u32> = rest.split(' ').filter_map(|v| v.parse().ok()).collect();
                let [id, pw, ph] = n[..] else { bail!("odd reply from the display helper: {line}") };
                self.id = id;
                self.pixels = (pw, ph);
                Ok(())
            }
            Some(("err", msg)) => bail!("{msg}"),
            _ => bail!("the display helper quit unexpectedly"),
        }
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The display leaves a moment after the helper does.
        let deadline = Instant::now() + Duration::from_secs(2);
        while online_displays().contains(&self.id) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Entry point for `--fit-helper W H`: make the display, mirror onto it,
/// then take "W H" resize requests on stdin until the host closes it.
pub fn helper_main(width: u32, height: u32) -> ! {
    let mut out = std::io::stdout();
    let display = autoreleasing(|| VirtualDisplay::new(width, height).and_then(|d| d.mirror_others().map(|_| d)));
    let mut display = match display {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(out, "err {e:#}");
            std::process::exit(1);
        }
    };
    let ok = |d: &VirtualDisplay| format!("ok {} {} {}", d.id, d.size.0 * 2, d.size.1 * 2);
    let _ = writeln!(out, "{}", ok(&display));
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let mut parts = line.split_whitespace().filter_map(|p| p.parse::<u32>().ok());
        let (Some(w), Some(h)) = (parts.next(), parts.next()) else { continue };
        let reply = match autoreleasing(|| display.resize(w, h)) {
            Ok(()) => ok(&display),
            Err(e) => format!("err {e:#}"),
        };
        let _ = writeln!(out, "{reply}");
    }
    std::process::exit(0);
}
