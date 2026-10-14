//! The project timer's set-up guide: the first time the tracker says someone
//! bills by the hour on this computer (an admin just ticked them, or they
//! signed in here for the first time), the quick panel walks them through it,
//! a step at a time, each one skippable:
//!
//! 1. what the timer is (their own stopwatch; the project decides billing);
//! 2. starting, switching and stopping it (panel, menu, shortcut), and
//!    pinning the projects they work on most;
//! 3. what happens when they step away (the idle minutes);
//! 4. not forgetting to log: "Suggest time from my work" (switched on only
//!    by them, in Settings, after its privacy page) and "Still to confirm"
//!    on My Hours;
//! 5. opening the app at login, so the timer is always there.
//!
//! The steps only use the panel's own settings (`mini_menu`, pins, the
//! detector's own switch), so the guide never changes anything they didn't
//! tap. Shown once ("Done" or "Skip set-up" marks it done), after What's new
//! when both are due, and any time from Settings › Project timer.

use tauri::{AppHandle, Runtime};

/// The guide was finished or skipped on this computer.
const DONE: &str = "hourly_setup_done";

static SCHEDULED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn is_done<R: Runtime>(app: &AppHandle<R>) -> bool {
    crate::close::flag(app, DONE)
}

/// Whether to offer the guide now: they bill by the hour, it isn't done,
/// and it isn't already waiting to be shown.
pub(crate) fn should_offer(hourly: bool, done: bool, scheduled: bool) -> bool {
    hourly && !done && !scheduled
}

/// The tracker has just said they bill by the hour (hourly.rs `set_info`):
/// show the guide once they're signed in and looking, and What's new (if
/// it's up) has been read.
pub(crate) fn maybe_offer<R: Runtime>(app: &AppHandle<R>) {
    let scheduled = SCHEDULED.load(std::sync::atomic::Ordering::Relaxed);
    if !should_offer(crate::hourly::info_snapshot(app).is_some(), is_done(app), scheduled) {
        return;
    }
    SCHEDULED.store(true, std::sync::atomic::Ordering::Relaxed);
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(8));
        for _ in 0..60 {
            let looking = tauri::Manager::get_webview_window(&app, "main")
                .and_then(|main| main.is_visible().ok())
                .unwrap_or(false);
            if is_done(&app) || crate::hourly::info_snapshot(&app).is_none() {
                break;
            }
            if looking && crate::session::signed_in() && !crate::mini::held_open() {
                crate::mini::show_setup(&app);
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(10));
        }
        // Not now: offered again on the next read that says they're hourly.
        SCHEDULED.store(false, std::sync::atomic::Ordering::Relaxed);
    });
}

/// "Done" or "Skip set-up": not offered again on this computer.
pub(crate) fn mark_done<R: Runtime>(app: &AppHandle<R>) {
    crate::close::set_flag(app, DONE, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offered_once_only_to_people_who_bill_by_the_hour() {
        assert!(should_offer(true, false, false));
        assert!(!should_offer(false, false, false));
        assert!(!should_offer(true, true, false));
        assert!(!should_offer(true, false, true));
    }
}
