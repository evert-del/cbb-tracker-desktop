//! Signed in or not, and what the app looks like either way.
//!
//! Signed out, the app is a sign-in app: a compact sign-in window, no quick
//! panel (the tray icon and the panel / clock shortcuts open sign in), the
//! tray offers "Sign In…" with the panel and New Window greyed out, extra
//! tracker windows close, and on macOS the tab bar is hidden. Signed in,
//! everything comes back.
//!
//! The state comes from the same check the app uses at launch to skip the
//! sign-in form: whether the tracker page holds a Supabase session cookie,
//! read every 1.5 s by tabs::follow_titles (and on every page load).

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

use tauri::menu::MenuItem;
use tauri::{AppHandle, LogicalSize, Manager, Runtime, Size};

const UNKNOWN: u8 = 0;
const IN: u8 = 1;
const OUT: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

/// The main window's size before it became the sign-in window.
static FULL_SIZE: Mutex<Option<(f64, f64)>> = Mutex::new(None);

/// The sign-in window (logical px): room for the tracker's sign-in form.
const SIGN_IN_SIZE: (f64, f64) = (520.0, 760.0);
const SIGN_IN_MIN: (f64, f64) = (440.0, 600.0);
/// The tracker window's own size and minimum (build_tracker_window).
const TRACKER_SIZE: (f64, f64) = (1280.0, 800.0);
const TRACKER_MIN: (f64, f64) = (1024.0, 640.0);

/// The tray rows that change with signing in (lib.rs setup).
pub(crate) struct TrayRows<R: Runtime> {
    pub show: MenuItem<R>,
    pub mini: MenuItem<R>,
    pub new_window: MenuItem<R>,
}

/// Whether someone is signed in. Until the first check it counts as yes,
/// so nothing is hidden by a guess at launch.
pub(crate) fn signed_in() -> bool {
    STATE.load(Ordering::Relaxed) != OUT
}

/// Whether a `document.cookie` string holds a Supabase session cookie.
pub(crate) fn has_session_cookie(cookies: &str) -> bool {
    cookies.split("; ").any(|pair| {
        let name = pair.split('=').next().unwrap_or("");
        name.starts_with("sb-") && name.contains("-auth-token")
    })
}

/// The tray's first row: "Show Tracker" signed in, "Sign In…" signed out.
pub(crate) fn show_label(signed_in: bool) -> &'static str {
    if signed_in { "Show Tracker" } else { "Sign In…" }
}

/// The latest check of the main window's page. Only a change does anything.
pub(crate) fn update<R: Runtime>(app: &AppHandle<R>, now_signed_in: bool) {
    let next = if now_signed_in { IN } else { OUT };
    let before = STATE.swap(next, Ordering::Relaxed);
    if before == next {
        return;
    }
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if now_signed_in {
            become_tracker(&app, before == OUT);
        } else {
            become_sign_in(&app);
        }
    });
}

fn tray_rows<R: Runtime>(app: &AppHandle<R>, signed_in: bool) {
    if let Some(rows) = app.try_state::<TrayRows<R>>() {
        let _ = rows.show.set_text(show_label(signed_in));
        let _ = rows.mini.set_enabled(signed_in);
        let _ = rows.new_window.set_enabled(signed_in);
    }
}

/// Signed out: a sign-in app.
fn become_sign_in<R: Runtime>(app: &AppHandle<R>) {
    tray_rows(app, false);
    crate::mini::sign_out(app);
    for (label, window) in app.webview_windows() {
        if crate::is_extra_tracker_window(&label) {
            let _ = window.close();
        }
    }
    let Some(main) = app.get_webview_window("main") else { return };
    if let Ok(size) = main.inner_size() {
        let scale = main.scale_factor().unwrap_or(1.0);
        let logical = size.to_logical::<f64>(scale);
        if logical.width > SIGN_IN_SIZE.0 + 1.0 {
            if let Ok(mut full) = FULL_SIZE.lock() {
                *full = Some((logical.width, logical.height));
            }
        }
    }
    let _ = main.set_min_size(Some(Size::Logical(LogicalSize::new(SIGN_IN_MIN.0, SIGN_IN_MIN.1))));
    let _ = main.set_size(Size::Logical(LogicalSize::new(SIGN_IN_SIZE.0, SIGN_IN_SIZE.1)));
    let _ = main.center();
    #[cfg(target_os = "macos")]
    crate::tabs::hide_tab_bar(&main);
}

/// Signed in: the tracker. `was_sign_in`: the window had become the
/// sign-in window, so it grows back.
fn become_tracker<R: Runtime>(app: &AppHandle<R>, was_sign_in: bool) {
    tray_rows(app, true);
    let Some(main) = app.get_webview_window("main") else { return };
    let _ = main.set_min_size(Some(Size::Logical(LogicalSize::new(TRACKER_MIN.0, TRACKER_MIN.1))));
    let small = main
        .inner_size()
        .ok()
        .map(|size| size.to_logical::<f64>(main.scale_factor().unwrap_or(1.0)))
        .is_some_and(|size| size.width < TRACKER_MIN.0 || size.height < TRACKER_MIN.1);
    if was_sign_in || small {
        let (w, h) = FULL_SIZE.lock().ok().and_then(|full| *full).unwrap_or(TRACKER_SIZE);
        let _ = main.set_size(Size::Logical(LogicalSize::new(w, h)));
        let _ = main.center();
    }
    #[cfg(target_os = "macos")]
    crate::tabs::show_tab_bar(&main);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookie_is_the_supabase_auth_token() {
        assert!(has_session_cookie("cbb_consent=x; sb-abc-auth-token=1"));
        assert!(has_session_cookie("sb-abc-auth-token.0=a; sb-abc-auth-token.1=b"));
        assert!(!has_session_cookie("cbb_consent=necessary.2.1"));
        assert!(!has_session_cookie(""));
    }

    #[test]
    fn tray_says_sign_in_when_signed_out() {
        assert_eq!(show_label(true), "Show Tracker");
        assert_eq!(show_label(false), "Sign In…");
    }
}
