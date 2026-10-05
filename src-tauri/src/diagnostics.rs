//! Diagnostics dialog: one screen of support-useful facts (versions,
//! platform, clock state, unread count, connectivity). Read-only, built from
//! state the shell already keeps. Opened from the tray's Diagnostics item.

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_dialog::DialogExt;

/// Plain-text support summary. Pure: unit-tested below.
pub(crate) fn text(
    shell_version: &str,
    os: &str,
    arch: &str,
    clock: &str,
    unread: u32,
    online: bool,
    offline_saved: &str,
) -> String {
    format!(
        "CoolerBox Tracker {shell_version} ({os}/{arch})\nClock: {clock}\nUnread: {unread}\nNetwork: {}\nSaved for offline: {offline_saved}",
        if online { "online" } else { "OFFLINE" },
    )
}

pub(crate) fn show<R: Runtime>(app: &AppHandle<R>) {
    let clock = app
        .try_state::<crate::clock::Last>()
        .and_then(|last| last.0.lock().ok().and_then(|slot| slot.clone()))
        .map(|c| {
            if !c.available {
                "time sheet not in use".to_string()
            } else if c.since.is_empty() {
                format!("{} (off the clock)", c.state)
            } else {
                format!("{} since {}", c.state, c.since)
            }
        })
        .unwrap_or_else(|| "unknown yet (first poll pending)".to_string());
    let unread = app
        .try_state::<crate::notify::Unread>()
        .and_then(|u| u.0.lock().ok().map(|slot| *slot))
        .unwrap_or(0);
    let online = app
        .try_state::<crate::clock::Net>()
        .and_then(|n| n.online.lock().ok().map(|slot| *slot))
        .unwrap_or(true);
    let offline_saved = app
        .try_state::<crate::notify::Snapshot>()
        .and_then(|snap| {
            snap.needs.lock().ok().map(|rows| {
                if rows.is_empty() {
                    "nothing unread with a link".to_string()
                } else {
                    format!("{} item(s) with links", rows.len())
                }
            })
        })
        .unwrap_or_else(|| "unknown".to_string());
    let body = text(
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        &clock,
        unread,
        online,
        &offline_saved,
    );
    app.dialog().message(body).title("Diagnostics").show(|_| {});
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_names_everything_support_needs() {
        let body = text("0.2.9", "macos", "aarch64", "in since 08:02", 3, true, "2 item(s) with links");
        for needle in ["0.2.9", "macos", "aarch64", "in since 08:02", "Unread: 3", "online", "2 item(s)"] {
            assert!(body.contains(needle), "{needle} missing in:\n{body}");
        }
        let offline = text("0.2.9", "windows", "x86_64", "out", 0, false, "nothing unread with a link");
        assert!(offline.contains("OFFLINE"));
    }
}
