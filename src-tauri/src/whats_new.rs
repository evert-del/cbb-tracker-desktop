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
/// Empty for a version with nothing to tell (no page is shown then). The
/// project timer's notes are only for people who bill by the hour (`hourly`):
/// nobody else has it, so nobody else hears about it.
pub(crate) fn notes(version: &str, os: &str, hourly: bool) -> Vec<Item> {
    let mac = os == "macos";
    let tray = if mac { "menu bar" } else { "system tray" };
    let dock = if mac { "Dock" } else { "taskbar" };
    let item = |title: &str, how: String| Item { title: title.into(), how };
    let panel_key = if mac { "⌘⇧M" } else { "Ctrl+Alt+M" };
    let timer_key = if mac { "⌘⇧Y" } else { "Ctrl+Alt+Y" };
    match version {
        "0.2.19" => {
            let mut items = Vec::new();
            if hourly {
                items.push(item(
                    "A project timer, if you bill by the hour",
                    format!(
                        "Start, switch or stop a project in one tap from the quick panel ({panel_key}), the {tray}{}, or {timer_key} from anywhere. Forgot to start it? Add time…, or Just start and pick the project when you stop. Away from the computer with it running, it asks what to do with the idle time.",
                        if mac { " or the Dock icon's menu" } else { "" }
                    ),
                ));
                items.push(item(
                    "Suggests time you forgot to log",
                    "Switch on Settings › Suggest time from my work, pick your apps (design, video, audio, 3D, code and AI, browsers, or add any app that's open) and link folders, or your whole Work folder or drive as \"Any project\". It asks \"Worked on this, log it?\" and never logs time by itself. Right now in Settings shows what it's seeing. It never records your screen or reads your files, and nobody at CoolerBox or your company can see what you work on: only time you say yes to reaches the tracker, as a project and two times."
                        .into(),
                ));
            }
            if hourly {
                items.push(item(
                    "Start a project, and notes for the client",
                    "Make a new project (and client) from the project list's New project…, then start it, file time to it or link a folder to it. Tick \"Show the client this note\" on an entry to put its note on their Hours page and invoice."
                        .into(),
                ));
            }
            items.extend([
                item(
                    "Choose what pops up",
                    "Settings › Notifications that pop up: tracker notifications, walkie messages and Channel 1, each on or off. Do not disturb quiets the app and the tracker website together. Call alerts still ring."
                        .into(),
                ),
                item(
                    "Click a notification to open it",
                    format!(
                        "A notification opens what it's about: the page, or the walkie chat.{}",
                        if mac { " Reply to a walkie message right from the notification." } else { "" }
                    ),
                ),
                item(
                    "Search and your next call",
                    "Start typing in the quick panel to search projects, deliverables and ideas. Your next booking shows there too, with its place and call sheet, and the next two days' call sheets are saved for offline."
                        .into(),
                ),
            ]);
            if !mac && os == "windows" {
                items.push(item(
                    "Drop files and right-click the taskbar",
                    "Drag files from Explorer onto the tracker's upload areas. Right-click the taskbar \
                     icon for the quick panel, a new window and Saved for offline."
                        .into(),
                ));
            }
            items
        }
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
    // Whether there's anything to tell at all; what's told is worked out
    // when it's shown, once the tracker has said whether they bill by the hour.
    let any = !notes(VERSION, std::env::consts::OS, true).is_empty();
    if !should_show(last.as_deref(), existing, any) {
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
                let items = notes(VERSION, std::env::consts::OS, crate::hourly::info_snapshot(&app).is_some());
                if items.is_empty() {
                    set_seen(&app);
                } else {
                    crate::mini::show_whats_new(&app, VERSION, &items);
                }
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
    let items = notes(VERSION, std::env::consts::OS, crate::hourly::info_snapshot(app).is_some());
    crate::mini::show_whats_new(app, VERSION, &items);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_version_has_notes_in_each_systems_words() {
        let mac = notes("0.2.18", "macos", false);
        let win = notes("0.2.18", "windows", false);
        assert!(mac.len() >= 5 && win.len() >= 5);
        assert!(mac.iter().any(|i| i.how.contains("Dock")));
        assert!(!win.iter().any(|i| i.how.contains("Dock") || i.how.contains("menu bar")));
        assert!(notes("0.0.1", "macos", true).is_empty());
        assert!(!notes(VERSION, "macos", false).is_empty(), "add notes for {VERSION} in whats_new.rs");
    }

    #[test]
    fn the_timer_release_names_each_systems_keys() {
        let mac = notes("0.2.19", "macos", true);
        let win = notes("0.2.19", "windows", true);
        assert!(mac.iter().any(|i| i.how.contains("⌘⇧Y") && i.how.contains("Dock")));
        assert!(win.iter().any(|i| i.how.contains("Ctrl+Alt+Y")));
        assert!(!win.iter().any(|i| i.how.contains("Dock") || i.how.contains("menu bar")));
        assert!(win.iter().any(|i| i.title.contains("taskbar")));
        assert!(!mac.iter().any(|i| i.title.contains("taskbar")));
    }

    #[test]
    fn only_people_who_bill_by_the_hour_hear_about_the_timer() {
        for os in ["macos", "windows", "linux"] {
            let everyone = notes("0.2.19", os, false);
            assert!(!everyone.is_empty());
            assert!(
                !everyone.iter().any(|i| {
                    let text = format!("{} {}", i.title, i.how).to_lowercase();
                    text.contains("timer") || text.contains("suggest time") || text.contains("by the hour")
                }),
                "{os}: {everyone:?}"
            );
            assert_eq!(notes("0.2.19", os, true).len(), everyone.len() + 3);
        }
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
