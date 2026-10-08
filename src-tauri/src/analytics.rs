//! Usage of the app's own features (quick panel, tray menu, shortcuts) for
//! the tracker's PostHog.
//!
//! The shell holds no analytics key and sends nothing itself. It hands each
//! event to the tracker page in the main window, which sends it with its
//! own `track()`: the same consent rules, scrubbing and signed-in identity
//! as every other tracker event, and nothing at all when nobody is signed
//! in. No IPC: an `eval` appends `{event, properties}` to
//! `window.cbbDesktopEvents` and dispatches `cbb:desktop-event`; the page
//! empties that queue (on the event and when it loads, so nothing fired
//! during a navigation is lost). The queue is capped, so a page that never
//! listens cannot grow it without bound.
//!
//! Events and their properties are fixed labels only, never content:
//!
//! - `desktop_panel_opened`  via: tray_icon | tray_menu | shortcut
//! - `desktop_clock_tapped`  action: in | break | back | wrap;
//!   from: panel | mini_timer | tray_menu | shortcut
//! - `desktop_action`        action: open_tracker | see_all_notifications |
//!   notification | saved_offline | check_updates | diagnostics;
//!   from: panel | tray_menu
//! - `desktop_panel_mode`    pinned, compact (booleans)
//! - `desktop_setting_changed` setting: launch_at_login | close_quits; value

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

/// Most events the page may hold before it reads them.
const QUEUE_CAP: usize = 50;

/// The script that hands one event to the page.
pub(crate) fn script(event: &str, properties: &Value) -> String {
    let item = json!({ "event": event, "properties": properties });
    format!(
        "(function (e) {{ var q = window.cbbDesktopEvents = window.cbbDesktopEvents || []; \
         if (q.length < {QUEUE_CAP}) q.push(e); \
         window.dispatchEvent(new CustomEvent('cbb:desktop-event')); }})({item})"
    )
}

fn send<R: Runtime>(app: &AppHandle<R>, event: &str, properties: Value) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval(script(event, &properties).as_str());
    }
}

/// The quick panel was opened.
pub(crate) fn panel_opened<R: Runtime>(app: &AppHandle<R>, via: &'static str) {
    send(app, "desktop_panel_opened", json!({ "via": via }));
}

/// A time-sheet clock tap from outside the tracker page.
pub(crate) fn clock_tapped<R: Runtime>(app: &AppHandle<R>, action: &'static str, from: &'static str) {
    send(app, "desktop_clock_tapped", json!({ "action": action, "from": from }));
}

/// A panel or tray shortcut was used.
pub(crate) fn action<R: Runtime>(app: &AppHandle<R>, action: &'static str, from: &'static str) {
    send(app, "desktop_action", json!({ "action": action, "from": from }));
}

/// The panel was pinned / unpinned or shrunk / grown.
pub(crate) fn panel_mode<R: Runtime>(app: &AppHandle<R>, pinned: bool, compact: bool) {
    send(app, "desktop_panel_mode", json!({ "pinned": pinned, "compact": compact }));
}

/// A setting changed from the panel or the tray.
pub(crate) fn setting_changed<R: Runtime>(app: &AppHandle<R>, setting: &'static str, value: bool) {
    send(app, "desktop_setting_changed", json!({ "setting": setting, "value": value }));
}

/// The fixed label for a clock action id, so only known labels are sent.
pub(crate) fn clock_label(action: &str) -> Option<&'static str> {
    match action {
        "in" => Some("in"),
        "break" => Some("break"),
        "back" => Some("back"),
        "wrap" => Some("wrap"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_queues_the_event_and_wakes_the_page() {
        let js = script("desktop_clock_tapped", &json!({ "action": "wrap", "from": "mini_timer" }));
        assert!(js.contains("window.cbbDesktopEvents"));
        assert!(js.contains("q.length < 50"));
        assert!(js.contains("new CustomEvent('cbb:desktop-event')"));
        assert!(js.contains(r#""event":"desktop_clock_tapped""#));
        assert!(js.contains(r#""from":"mini_timer""#));
    }

    #[test]
    fn only_known_clock_actions_have_labels() {
        for action in ["in", "break", "back", "wrap"] {
            assert_eq!(clock_label(action), Some(action));
        }
        for action in ["", "admin", "in;x"] {
            assert_eq!(clock_label(action), None);
        }
    }
}
