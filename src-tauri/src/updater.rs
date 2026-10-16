//! Self-update of the desktop shell (the website itself updates on load).
//!
//! The endpoint and the minisign public key live in `tauri.conf.json`; every
//! update bundle is signature-checked against that key when it is downloaded.
//!
//! Automatic: everyone gets a new version within minutes, without being
//! asked. The app checks 30 s after launch and every 15 minutes, downloads a
//! new version quietly, then installs it and restarts at the first safe
//! moment (`safe_now`): nobody at the computer for 5 minutes, or no tracker
//! window open, and never while a page would warn before leaving (an unsaved
//! call sheet, a cut uploading) or project-timer taps made offline are still
//! waiting to be sent (they live in memory only). Failures stay silent: the
//! app keeps working and tries again later.
//!
//! The tray's "Check for updates" asks instead, and always says the outcome
//! ("up to date", "could not check"); a version already downloaded is
//! offered straight away.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    time::{Duration, Instant},
};

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::{Update, UpdaterExt};

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(15 * 60);
/// How often a downloaded update looks for its safe moment.
const WAIT_STEP: Duration = Duration::from_secs(30);
/// Nobody at the computer this long: a restart interrupts no one.
const AWAY_FOR: u64 = 5 * 60;
/// How long a tracker page has to say whether it would warn before leaving.
const PAGE_ANSWER: Duration = Duration::from_secs(3);

/// Whether a page would ask "Leave site?" now: the tracker's own warnings
/// (an unsaved call sheet, an upload) cancel `beforeunload`. A made-up event
/// shows no dialog; hand_over.js lets it through.
const WOULD_WARN_JS: &str = "(function(){try{var e=new Event('beforeunload',{cancelable:true});window.dispatchEvent(e);return e.defaultPrevented;}catch(x){return true;}})()";

/// One check/prompt at a time, so the timer and the tray item cannot stack
/// two dialogs or two downloads.
static CHECKING: AtomicBool = AtomicBool::new(false);

/// A downloaded, signature-checked update waiting for its safe moment.
static READY: Mutex<Option<(Update, Vec<u8>)>> = Mutex::new(None);

struct CheckGuard;

impl CheckGuard {
    fn acquire() -> Option<Self> {
        (!CHECKING.swap(true, Ordering::SeqCst)).then_some(Self)
    }
}

impl Drop for CheckGuard {
    fn drop(&mut self) {
        CHECKING.store(false, Ordering::SeqCst);
    }
}

fn ready_version() -> Option<String> {
    READY.lock().ok()?.as_ref().map(|(update, _)| update.version.clone())
}

pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK_DELAY);
        let mut next_check = Instant::now();
        loop {
            if ready_version().is_some() {
                if safe_now(&app) {
                    install_ready(&app);
                }
            } else if Instant::now() >= next_check {
                tauri::async_runtime::block_on(download_quietly(&app));
                next_check = Instant::now() + CHECK_EVERY;
            }
            std::thread::sleep(WAIT_STEP);
        }
    });
}

/// Check, and download a new version without asking. Silent on failure.
async fn download_quietly<R: Runtime>(app: &AppHandle<R>) {
    let Some(_guard) = CheckGuard::acquire() else {
        return;
    };
    let Ok(updater) = app.updater() else { return };
    let Ok(Some(update)) = updater.check().await else { return };
    if let Ok(bytes) = update.download(|_, _| {}, || {}).await {
        if let Ok(mut ready) = READY.lock() {
            *ready = Some((update, bytes));
        }
    }
}

/// Whether a restart now would interrupt nobody and lose nothing.
fn safe_now<R: Runtime>(app: &AppHandle<R>) -> bool {
    if CHECKING.load(Ordering::SeqCst) || crate::hourly::has_unsent(app) {
        return false;
    }
    let windows: Vec<_> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| label == "main" || crate::is_extra_tracker_window(label))
        .map(|(_, window)| window)
        .collect();
    if windows.iter().any(would_warn) {
        return false;
    }
    let none_open = !windows.iter().any(|window| window.is_visible().unwrap_or(false));
    none_open || crate::idle::idle_seconds().is_some_and(|idle| idle >= AWAY_FOR)
}

/// The page's own answer; no answer counts as a warning.
fn would_warn<R: Runtime>(window: &tauri::WebviewWindow<R>) -> bool {
    let (tx, rx) = mpsc::channel();
    if window
        .eval_with_callback(WOULD_WARN_JS, move |answer| {
            let _ = tx.send(answer.trim() == "true");
        })
        .is_err()
    {
        return true;
    }
    rx.recv_timeout(PAGE_ANSWER).unwrap_or(true)
}

/// Install the downloaded update and restart.
fn install_ready<R: Runtime>(app: &AppHandle<R>) {
    let Some((update, bytes)) = READY.lock().ok().and_then(|mut ready| ready.take()) else {
        return;
    };
    if update.install(bytes).is_ok() {
        app.restart();
    }
}

/// Tray item: check now and always report the outcome.
pub(crate) fn check_now<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        tauri::async_runtime::block_on(check_once(&app));
    });
}

fn info<R: Runtime>(app: &AppHandle<R>, title: &str, message: String) {
    let _ = app.dialog().message(message).title(title).show(|_| {});
}

async fn ask_restart<R: Runtime>(app: &AppHandle<R>, version: &str, current: &str) -> bool {
    let ask = app
        .dialog()
        .message(format!(
            "CoolerBox Tracker {version} is available (you have {current}).\n\nUpdate now? The app will restart. Otherwise it updates by itself when you're away."
        ))
        .title("Update available")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Update and restart".into(),
            "Later".into(),
        ));
    tauri::async_runtime::spawn_blocking(move || ask.blocking_show())
        .await
        .unwrap_or(false)
}

async fn check_once<R: Runtime>(app: &AppHandle<R>) {
    let Some(guard) = CheckGuard::acquire() else {
        return;
    };
    let current = app.package_info().version.to_string();
    // Already downloaded: offer it straight away.
    if let Some(version) = ready_version() {
        let accepted = ask_restart(app, &version, &current).await;
        drop(guard);
        if accepted {
            install_ready(app);
        }
        return;
    }
    let Ok(updater) = app.updater() else {
        info(app, "Updates", "Updates are not available in this build.".into());
        return;
    };
    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => {
            info(
                app,
                "You're up to date",
                format!("CoolerBox Tracker {current} is the latest version."),
            );
            return;
        }
        Err(_) => {
            info(
                app,
                "Could not check for updates",
                "Check your internet connection and try again.".into(),
            );
            return;
        }
    };
    let accepted = ask_restart(app, &update.version, &update.current_version).await;
    match update.download(|_, _| {}, || {}).await {
        Ok(bytes) => {
            if let Ok(mut ready) = READY.lock() {
                *ready = Some((update, bytes));
            }
            drop(guard);
            if accepted {
                install_ready(app);
            }
        }
        Err(_) if accepted => info(
            app,
            "Update failed",
            "The update could not be installed. You can keep using the current version; it tries again by itself.".into(),
        ),
        Err(_) => {}
    }
}
