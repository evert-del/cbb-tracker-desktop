//! What the main window's close button does.
//!
//! By default closing keeps the app running in the menu bar / system tray,
//! so the time sheet's clock, the 08:30 reminder and notifications keep
//! working (like Slack or Toggl). The first time someone closes the window
//! a short message says so and offers to quit on close instead; the quick
//! panel's Settings change it either way afterwards. Both answers live in
//! the store plugin's `settings.json` in app data.

use tauri::{AppHandle, Manager, Runtime, Window};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_store::StoreExt;

const STORE: &str = "settings.json";
/// Closing the main window quits the app instead of hiding it.
const QUITS: &str = "close_quits";
/// The one-time "still running" message has been shown.
const EXPLAINED: &str = "close_explained";

/// A true/false setting in `settings.json` (false when unset).
pub(crate) fn flag<R: Runtime>(app: &AppHandle<R>, key: &str) -> bool {
    app.store(STORE)
        .ok()
        .and_then(|store| store.get(key))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn set_flag<R: Runtime>(app: &AppHandle<R>, key: &str, on: bool) {
    if let Ok(store) = app.store(STORE) {
        store.set(key, on);
        let _ = store.save();
    }
}

pub(crate) fn quits<R: Runtime>(app: &AppHandle<R>) -> bool {
    flag(app, QUITS)
}

pub(crate) fn set_quits<R: Runtime>(app: &AppHandle<R>, on: bool) {
    set_flag(app, QUITS, on);
}

/// The one-time message, in each system's own words for where the app
/// keeps running and how to quit it.
pub(crate) fn explanation(os: &str) -> String {
    let (place, quit, shortcut, open_panel) = match os {
        "macos" => ("menu bar", "Quit", " (or press Cmd+Q)", "Click its icon"),
        "windows" => ("system tray", "Exit", "", "Click its icon"),
        _ => ("system tray", "Quit", "", "Choose Quick panel from its icon"),
    };
    format!(
        "Closing the window keeps CoolerBox Tracker running in the {place}, so your \
         time sheet, reminders and notifications keep working. {open_panel} any time \
         for the quick panel: your timer, clock buttons and notifications.\n\nTo close it \
         completely, choose {quit} from its icon in the {place} or from the quick \
         panel{shortcut}. You can change what closing does in the quick panel's Settings."
    )
}

/// The main window's close button: quit, or hide to the menu bar / tray,
/// explaining that once.
pub(crate) fn main_window_closed<R: Runtime>(window: &Window<R>) {
    let app = window.app_handle().clone();
    if quits(&app) {
        app.exit(0);
        return;
    }
    if flag(&app, EXPLAINED) {
        crate::hide_to_tray(window);
        return;
    }
    set_flag(&app, EXPLAINED, true);
    let quit = if cfg!(target_os = "windows") { "Exit" } else { "Quit" };
    let window = window.clone();
    app.dialog()
        .message(explanation(std::env::consts::OS))
        .title("CoolerBox Tracker keeps running")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Keep running".into(),
            format!("{quit} when I close"),
        ))
        .parent(&window)
        .show(move |keep| {
            if keep {
                crate::hide_to_tray(&window);
            } else {
                let app = window.app_handle();
                set_quits(app, true);
                app.exit(0);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explanation_uses_each_systems_words() {
        let mac = explanation("macos");
        assert!(mac.contains("menu bar") && mac.contains("Quit") && mac.contains("Cmd+Q"));
        let win = explanation("windows");
        assert!(win.contains("system tray") && win.contains("Exit") && !win.contains("Cmd+Q"));
        let linux = explanation("linux");
        assert!(linux.contains("system tray") && linux.contains("Quit") && !linux.contains("Cmd+Q"));
    }
}
