//! "What's new": after an update, the quick panel says what changed and how
//! to use it, once. Shown the first time someone is signed in and looking at
//! the tracker on the new version (like the one-time tip in mini.rs), and any
//! time from the panel's Settings.
//!
//! The notes live here, written per version in each system's own words.
//! Add the next version's entry at the top of `notes` when releasing.

use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

const STORE: &str = "settings.json";
/// The version whose notes were last shown (or that was first installed).
const SEEN: &str = "whats_new_seen";

const VERSION: &str = env!("CARGO_PKG_VERSION");

static SCHEDULED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// One new thing: what it is, and how to use it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct Item {
    pub title: String,
    pub how: String,
}

/// The notes for `version`, in `os`'s words ("macos", "windows", "linux").
/// Empty for a version with nothing to tell (no page is shown then).
pub(crate) fn notes(version: &str, os: &str) -> Vec<Item> {
    let mac = os == "macos";
    let tray = if mac { "menu bar" } else { "system tray" };
    let dock = if mac { "Dock" } else { "taskbar" };
    let item = |title: &str, how: String| Item { title: title.into(), how };
    match version {
        "0.2.18" => {
            let mut items = vec![
                item(
                    "Walkie in the quick panel",
                    format!(
                        "New walkie messages show straight away: a notification, a count next to the \
                         timer in the {tray}, and the conversation in the quick panel. Tap it to read \
                         and reply without opening the tracker. Stickers and files show too."
                    ),
                ),
                item(
                    "Dark mode",
                    "The app follows your computer's light or dark setting.".into(),
                ),
                item(
                    "Minimise to the mini timer",
                    format!(
                        "Minimise the tracker window and it turns into a thin floating timer with your \
                         clock buttons. Open tracker, or the {dock} icon, brings the window back."
                    ),
                ),
                item(
                    "Pin it where you want it",
                    "Unpinned, drag the panel by its top to move it. Pin it and it stays there. The \
                     arrows switch between the full panel and the mini timer, pinned or not."
                        .into(),
                ),
                item(
                    "A friendly hello",
                    format!(
                        "Call in, take a break, come back or wrap from the quick panel or the {tray} \
                         and the tracker says hello, or wishes you a good evening."
                    ),
                ),
            ];
            if mac {
                items.push(item(
                    "Closing keeps it in the Dock",
                    "Closing the window keeps the app running and in the Dock. Click the Dock icon \
                     to bring the tracker back."
                        .into(),
                ));
            }
            items
        }
        _ => Vec::new(),
    }
}

fn seen<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    app.store(STORE)
        .ok()
        .and_then(|store| store.get(SEEN))
        .and_then(|value| value.as_str().map(str::to_string))
}

/// "Got it" on the What's new page: this version's notes have been read.
pub(crate) fn mark_seen<R: Runtime>(app: &AppHandle<R>) {
    set_seen(app);
}

fn set_seen<R: Runtime>(app: &AppHandle<R>) {
    if let Ok(store) = app.store(STORE) {
        store.set(SEEN, VERSION);
        let _ = store.save();
    }
}

/// Whether to show this version's notes. `seen` is the last version noted;
/// `existing` says the app was used before (so a missing `seen` means an
/// update from a version before this feature, not a new install).
pub(crate) fn should_show(seen: Option<&str>, existing: bool, has_notes: bool) -> bool {
    has_notes
        && match seen {
            Some(version) => version != VERSION,
            None => existing,
        }
}

/// On a signed-in tracker page: show the notes once per update, after a
/// short pause, when someone is looking. New installs just note the version.
pub(crate) fn maybe_show<R: Runtime>(app: &AppHandle<R>) {
    let last = seen(app);
    if last.as_deref() == Some(VERSION) || SCHEDULED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    // Used before this feature existed: the one-time panel tip was shown.
    let existing = crate::mini::was_introduced(app);
    let items = notes(VERSION, std::env::consts::OS);
    if !should_show(last.as_deref(), existing, !items.is_empty()) {
        set_seen(app);
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        // A moment after the page settles, then as soon as someone is signed
        // in and the tracker is on screen (sign-in is confirmed shortly after
        // launch, and the tracker rarely reloads a whole page to try again).
        std::thread::sleep(std::time::Duration::from_secs(6));
        for _ in 0..24 {
            let looking = tauri::Manager::get_webview_window(&app, "main")
                .and_then(|main| main.is_visible().ok())
                .unwrap_or(false);
            if looking && crate::session::signed_in() {
                // Marked read on "Got it" (mini_whats_new_done), so a page
                // nobody got to read comes back on the next launch.
                crate::mini::show_whats_new(&app, VERSION, &items);
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
        // Not now: try again on a later page load.
        SCHEDULED.store(false, std::sync::atomic::Ordering::Relaxed);
    });
}

/// Settings ▸ What's new.
pub(crate) fn show_now<R: Runtime>(app: &AppHandle<R>) {
    let items = notes(VERSION, std::env::consts::OS);
    crate::mini::show_whats_new(app, VERSION, &items);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_version_has_notes_in_each_systems_words() {
        let mac = notes("0.2.18", "macos");
        let win = notes("0.2.18", "windows");
        assert!(mac.len() >= 5 && win.len() >= 5);
        assert!(mac.iter().any(|i| i.how.contains("Dock")));
        assert!(!win.iter().any(|i| i.how.contains("Dock") || i.how.contains("menu bar")));
        assert!(notes("0.0.1", "macos").is_empty());
        assert!(!notes(VERSION, "macos").is_empty(), "add notes for {VERSION} in whats_new.rs");
    }

    #[test]
    fn shown_once_per_update_never_on_a_new_install() {
        assert!(should_show(Some("0.2.17"), true, true));
        assert!(should_show(None, true, true), "updated from before this feature");
        assert!(!should_show(None, false, true), "a new install");
        assert!(!should_show(Some(VERSION), true, true), "already shown");
        assert!(!should_show(Some("0.2.17"), true, false), "nothing to tell");
    }
}
