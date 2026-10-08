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
//!
//! It also remembers the tracker window's place (size, position, maximised),
//! only while signed in, so the small sign-in window is never what comes
//! back; and whether someone was signed in, so the window opens at the right
//! size straight away (built hidden, placed, then shown: no jump). This
//! replaced tauri-plugin-window-state, which saved the sign-in size too.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

use tauri::menu::MenuItem;
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, Position, Runtime, Size, WindowEvent};
use tauri_plugin_store::StoreExt;

const STORE: &str = "settings.json";
/// Whether someone was signed in when the app last knew.
const SIGNED_IN_KEY: &str = "signed_in";
/// The tracker window's place: {w, h, x, y, max} in logical px.
const PLACE_KEY: &str = "main_window";

/// The tracker window's place while signed in.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Place {
    pub w: f64,
    pub h: f64,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub max: bool,
}

static PLACE: Mutex<Option<Place>> = Mutex::new(None);

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
    if let Ok(store) = app.store(STORE) {
        store.set(SIGNED_IN_KEY, now_signed_in);
        let _ = store.save();
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
    if main.is_maximized().unwrap_or(false) {
        let _ = main.unmaximize();
    }
    as_sign_in_window(&main);
    #[cfg(target_os = "macos")]
    crate::tabs::hide_tab_bar(&main);
}

fn as_sign_in_window<R: Runtime>(main: &tauri::WebviewWindow<R>) {
    let _ = main.set_min_size(Some(Size::Logical(LogicalSize::new(SIGN_IN_MIN.0, SIGN_IN_MIN.1))));
    let _ = main.set_size(Size::Logical(LogicalSize::new(SIGN_IN_SIZE.0, SIGN_IN_SIZE.1)));
    let _ = main.center();
}

/// Whether `place`'s top-left corner (and some of the window) is on one of
/// `screens` (logical x, y, width, height): a place on a screen that has
/// since gone is not restored.
pub(crate) fn on_a_screen(place: &Place, screens: &[(f64, f64, f64, f64)]) -> bool {
    screens.iter().any(|(sx, sy, sw, sh)| {
        place.x + 100.0 > *sx && place.x + 100.0 < sx + sw && place.y + 20.0 >= *sy && place.y + 20.0 < sy + sh
    })
}

/// At launch, with the main window built hidden: give it the place it should
/// have (the sign-in window if whoever was here last signed out, else the
/// tracker where it was left), start keeping track, then show it.
pub(crate) fn place_main_at_launch<R: Runtime>(main: &tauri::WebviewWindow<R>) {
    let app = main.app_handle().clone();
    let (was_signed_in, saved) = app
        .store(STORE)
        .ok()
        .map(|store| {
            (
                store.get(SIGNED_IN_KEY).and_then(|v| v.as_bool()),
                store.get(PLACE_KEY).and_then(|v| serde_json::from_value::<Place>(v).ok()),
            )
        })
        .unwrap_or((None, None));
    if let Some(place) = saved {
        if let Ok(mut full) = FULL_SIZE.lock() {
            *full = Some((place.w, place.h));
        }
        if let Ok(mut slot) = PLACE.lock() {
            *slot = Some(place);
        }
    }
    if was_signed_in == Some(false) {
        STATE.store(OUT, Ordering::Relaxed);
        tray_rows(&app, false);
        as_sign_in_window(main);
    } else if let Some(place) = saved {
        let screens: Vec<(f64, f64, f64, f64)> = main
            .available_monitors()
            .unwrap_or_default()
            .iter()
            .map(|m| {
                let scale = m.scale_factor();
                let p = m.position().to_logical::<f64>(scale);
                let s = m.size().to_logical::<f64>(scale);
                (p.x, p.y, s.width, s.height)
            })
            .collect();
        let _ = main.set_size(Size::Logical(LogicalSize::new(
            place.w.max(TRACKER_MIN.0),
            place.h.max(TRACKER_MIN.1),
        )));
        if on_a_screen(&place, &screens) {
            let _ = main.set_position(Position::Logical(LogicalPosition::new(place.x, place.y)));
        } else {
            let _ = main.center();
        }
        if place.max {
            let _ = main.maximize();
        }
    }
    track_main(main);
    let _ = main.show();
    maybe_welcome(main);
}

/// The one-time welcome has been shown on this computer (settings.json).
const WELCOMED_KEY: &str = "welcomed";

/// The tracker's privacy policy, opened in the browser from the welcome.
const PRIVACY_URL: &str = "https://tracker.coolerboxbrothers.com/privacy";

/// The welcome's words.
pub(crate) const WELCOME_TITLE: &str = "Welcome to CoolerBox Tracker";
pub(crate) const WELCOME_TEXT: &str = "Sign in with your CoolerBox Production Tracker account to get started.\n\n\
The app works under the same Terms and Privacy Policy as your tracker account. Once you're signed in, \
the tracker measures how its pages and features are used so it can improve them. That stays with \
CoolerBox: no advertising and nothing sold.\n\nThe Terms and Privacy Policy are always a click away \
in the app, and open in your browser.";

/// Once per computer, the first time the app opens: a short welcome about
/// signing in and privacy, with the privacy policy a button away.
pub(crate) fn maybe_welcome<R: Runtime>(main: &tauri::WebviewWindow<R>) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let app = main.app_handle().clone();
    if crate::close::flag(&app, WELCOMED_KEY) {
        return;
    }
    crate::close::set_flag(&app, WELCOMED_KEY, true);
    let opener = app.clone();
    app.dialog()
        .message(WELCOME_TEXT)
        .title(WELCOME_TITLE)
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Continue".into(),
            "Read the Privacy Policy".into(),
        ))
        .parent(main)
        .show(move |continued| {
            if !continued {
                crate::system_open::open_url(&opener, PRIVACY_URL);
            }
        });
}

/// Keeps the tracker window's place up to date while signed in.
fn track_main<R: Runtime>(main: &tauri::WebviewWindow<R>) {
    let window = main.clone();
    main.on_window_event(move |event| {
        if !matches!(event, WindowEvent::Resized(_) | WindowEvent::Moved(_)) {
            return;
        }
        if STATE.load(Ordering::Relaxed) == OUT || window.is_minimized().unwrap_or(false) {
            return;
        }
        let max = window.is_maximized().unwrap_or(false);
        let scale = window.scale_factor().unwrap_or(1.0);
        let (Ok(size), Ok(pos)) = (window.inner_size(), window.outer_position()) else { return };
        let size = size.to_logical::<f64>(scale);
        let pos = pos.to_logical::<f64>(scale);
        let Ok(mut slot) = PLACE.lock() else { return };
        if max {
            // Keep the size it had before being maximised.
            if let Some(place) = slot.as_mut() {
                place.max = true;
            }
            return;
        }
        if size.width < TRACKER_MIN.0 || size.height < TRACKER_MIN.1 {
            return;
        }
        *slot = Some(Place { w: size.width, h: size.height, x: pos.x, y: pos.y, max: false });
    });
}

/// On quit: remember the tracker window's place.
pub(crate) fn save<R: Runtime>(app: &AppHandle<R>) {
    let place = PLACE.lock().ok().and_then(|slot| *slot);
    if let (Some(place), Ok(store)) = (place, app.store(STORE)) {
        if let Ok(value) = serde_json::to_value(place) {
            store.set(PLACE_KEY, value);
            let _ = store.save();
        }
    }
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
    fn a_place_off_every_screen_is_not_restored() {
        let screens = [(0.0, 0.0, 1512.0, 982.0)];
        let here = Place { w: 1280.0, h: 800.0, x: 100.0, y: 60.0, max: false };
        assert!(on_a_screen(&here, &screens));
        let gone = Place { x: 2600.0, ..here };
        assert!(!on_a_screen(&gone, &screens));
        let above = Place { y: -900.0, ..here };
        assert!(!on_a_screen(&above, &screens));
        // A second screen above the laptop's.
        assert!(on_a_screen(&above, &[(0.0, 0.0, 1512.0, 982.0), (0.0, -1080.0, 1920.0, 1080.0)]));
    }

    #[test]
    fn welcome_points_at_the_privacy_policy() {
        assert!(WELCOME_TEXT.contains("Privacy Policy"));
        assert!(WELCOME_TEXT.contains("open in your browser"));
        assert!(PRIVACY_URL.starts_with("https://tracker.coolerboxbrothers.com/"));
    }

    #[test]
    fn tray_says_sign_in_when_signed_out() {
        assert_eq!(show_label(true), "Show Tracker");
        assert_eq!(show_label(false), "Sign In…");
    }
}
