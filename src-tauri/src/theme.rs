//! The tracker in dark mode when the computer is. The tracker site has only a
//! light design (`color-scheme: light`), so the app darkens it itself:
//!
//! - Windows: Edge WebView2's own automatic dark mode, switched on with
//!   browser flags before the first webview starts. Flags are fixed for the
//!   app's run, so a change of Windows theme while it is open asks for a
//!   restart (`theme_changed`).
//! - macOS: WebKit has no such mode, so dark.js flips the page's colours
//!   (media and the header flipped back). It follows the Mac live: `start`
//!   watches the setting and tells every tracker window.
//!
//! The app's own pages (the quick panel, Saved for offline) have their own
//! light and dark designs and are left to them.

/// Tauri's own WebView2 flags, which the environment variable replaces, so
/// they are kept alongside the dark ones.
#[cfg(any(windows, test))]
const TAURI_DEFAULT_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";

#[cfg(any(windows, test))]
const ENV: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

/// The WebView2 flags for a dark tracker, or none for a light one.
#[cfg(any(windows, test))]
pub(crate) fn browser_args(dark: bool) -> Option<String> {
    dark.then(|| format!("--force-dark-mode --enable-features=WebContentsForceDark {TAURI_DEFAULT_ARGS}"))
}

/// Whether the tracker was started dark (Windows), so a later change of theme
/// knows whether a restart would change anything.
#[cfg(windows)]
static STARTED_DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this app chose the WebView2 flags (and not someone by hand).
#[cfg(windows)]
static MANAGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Windows: before any webview exists, switch on WebView2's dark mode when
/// Windows is set to dark ("Choose your app mode: Dark"). Only for this
/// process; nothing is written to the system. A value someone set by hand
/// is left as it is.
#[cfg(windows)]
pub(crate) fn apply_at_launch() {
    use std::sync::atomic::Ordering::Relaxed;
    if std::env::var_os(ENV).is_some() {
        return;
    }
    MANAGED.store(true, Relaxed);
    let dark = windows_prefers_dark();
    STARTED_DARK.store(dark, Relaxed);
    if let Some(args) = browser_args(dark) {
        std::env::set_var(ENV, args);
    }
}

/// Windows' app mode: `AppsUseLightTheme` is 0 when apps are set to dark.
/// Unreadable (older Windows, policy) counts as light.
#[cfg(windows)]
pub(crate) fn windows_prefers_dark() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = wide("AppsUseLightTheme");
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: a DWORD read into a u32 of the stated size.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    status == 0 && value == 0
}

/// Windows: the theme changed while the app is open. WebView2's flags can't
/// change until the next launch, so say so in the tracker, once per change.
#[cfg(windows)]
pub(crate) fn theme_changed<R: tauri::Runtime>(app: &tauri::AppHandle<R>, dark: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    static TOLD: std::sync::Mutex<Option<bool>> = std::sync::Mutex::new(None);
    if !MANAGED.load(Relaxed) {
        return;
    }
    let Ok(mut told) = TOLD.lock() else { return };
    if *told == Some(dark) {
        return;
    }
    *told = Some(dark);
    if dark == STARTED_DARK.load(Relaxed) {
        crate::download::toast(app, serde_json::json!({ "kind": "hide" }));
        return;
    }
    let mode = if dark { "dark" } else { "light" };
    crate::download::toast(
        app,
        serde_json::json!({
            "kind": "ok",
            "title": format!("Restart to switch to {mode} mode"),
            "detail": format!("Exit CoolerBox Tracker from the tray and open it again to see the tracker in {mode} mode."),
        }),
    );
}

/// macOS: the Mac's appearance ("AppleInterfaceStyle" is "Dark" in dark
/// mode, and absent in light). The system setting, not a window's: the
/// tracker windows' own appearance is always dark (tabs.rs).
#[cfg(target_os = "macos")]
pub(crate) fn macos_prefers_dark() -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    let (Some(defaults_class), Some(string_class)) = (AnyClass::get(c"NSUserDefaults"), AnyClass::get(c"NSString")) else {
        return false;
    };
    objc2::rc::autoreleasepool(|_| {
        // SAFETY: NSUserDefaults is thread-safe; every object is checked for
        // nil before use, and the pool releases the autoreleased ones.
        unsafe {
            let defaults: *mut AnyObject = msg_send![defaults_class, standardUserDefaults];
            if defaults.is_null() {
                return false;
            }
            let key: *mut AnyObject =
                msg_send![string_class, stringWithUTF8String: c"AppleInterfaceStyle".as_ptr()];
            if key.is_null() {
                return false;
            }
            let value: *mut AnyObject = msg_send![defaults, stringForKey: key];
            if value.is_null() {
                return false;
            }
            let utf8: *const std::ffi::c_char = msg_send![value, UTF8String];
            !utf8.is_null() && std::ffi::CStr::from_ptr(utf8).to_bytes().eq_ignore_ascii_case(b"dark")
        }
    })
}

/// The page script that turns the tracker dark or light in a window.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn set_dark_js(dark: bool) -> String {
    format!("window.__cbbSetDark&&window.__cbbSetDark({dark})")
}

/// dark.js for a new tracker window, starting as the Mac is now.
#[cfg(target_os = "macos")]
pub(crate) fn init_script() -> String {
    format!("window.__cbbDarkAtLaunch={};\n{}", macos_prefers_dark(), include_str!("dark.js"))
}

/// macOS: follow the Mac's appearance live in every tracker window.
#[cfg(target_os = "macos")]
pub(crate) fn start<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    use tauri::Manager;
    std::thread::spawn(move || {
        let mut last = macos_prefers_dark();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let dark = macos_prefers_dark();
            if dark == last {
                continue;
            }
            last = dark;
            for (label, window) in app.webview_windows() {
                if label == "main" || crate::is_extra_tracker_window(&label) {
                    let _ = window.eval(set_dark_js(dark));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_flags_keep_tauris_own() {
        let args = browser_args(true).expect("dark has flags");
        assert!(args.contains("--force-dark-mode"));
        assert!(args.contains("--enable-features=WebContentsForceDark"));
        assert!(args.contains(TAURI_DEFAULT_ARGS));
        assert_eq!(args.matches("--disable-features").count(), 1);
        assert_eq!(browser_args(false), None);
        assert_eq!(ENV, "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS");
    }

    #[test]
    fn set_dark_is_a_plain_boolean() {
        assert_eq!(set_dark_js(true), "window.__cbbSetDark&&window.__cbbSetDark(true)");
        assert_eq!(set_dark_js(false), "window.__cbbSetDark&&window.__cbbSetDark(false)");
    }

    #[test]
    fn dark_js_flips_the_page_and_back_for_media() {
        let js = include_str!("dark.js");
        assert!(js.contains("html.cbb-dark{filter:invert(.92) hue-rotate(180deg)"));
        assert!(js.contains(".gd-header"));
        assert!(js.contains("window.__cbbSetDark = set"));
        assert!(!js.contains("localStorage"));
    }
}
