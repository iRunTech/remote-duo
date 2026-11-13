//! macOS privacy permissions: Accessibility (to move the mouse and type) and
//! Screen Recording (for screen view and sound). Other systems don't gate
//! these, so they always report granted.

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub accessibility: bool,
    pub screen: bool,
}

impl Status {
    pub fn all(self) -> bool {
        self.accessibility && self.screen
    }
}

pub const ACCESSIBILITY_SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
pub const SCREEN_SETTINGS: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

#[cfg(target_os = "macos")]
mod ffi {
    use core_foundation::string::CFStringRef;
    use core_foundation::dictionary::CFDictionaryRef;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        pub fn AXIsProcessTrusted() -> bool;
        pub fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
        pub static kAXTrustedCheckOptionPrompt: CFStringRef;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        pub fn CGPreflightScreenCaptureAccess() -> bool;
        pub fn CGRequestScreenCaptureAccess() -> bool;
    }
}

/// Note: macOS only notices a new Screen Recording grant after the app
/// restarts, which is why System Settings offers "Quit & Reopen".
#[cfg(target_os = "macos")]
pub fn status() -> Status {
    unsafe {
        Status {
            accessibility: ffi::AXIsProcessTrusted(),
            screen: ffi::CGPreflightScreenCaptureAccess(),
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn status() -> Status {
    Status { accessibility: true, screen: true }
}

/// Shows macOS's own prompt (which adds the app to the list) and opens the
/// matching page of System Settings.
#[cfg(target_os = "macos")]
pub fn request_accessibility() {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    unsafe {
        let key = CFString::wrap_under_get_rule(ffi::kAXTrustedCheckOptionPrompt);
        let options = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::true_value())]);
        ffi::AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef());
    }
    let _ = open::that(ACCESSIBILITY_SETTINGS);
}

#[cfg(target_os = "macos")]
pub fn request_screen() {
    unsafe {
        ffi::CGRequestScreenCaptureAccess();
    }
    let _ = open::that(SCREEN_SETTINGS);
}

#[cfg(not(target_os = "macos"))]
pub fn request_accessibility() {}

#[cfg(not(target_os = "macos"))]
pub fn request_screen() {}
