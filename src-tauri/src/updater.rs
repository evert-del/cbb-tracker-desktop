//! Self-update of the desktop shell (the website itself updates on load).
//!
//! The endpoint and the minisign public key live in `tauri.conf.json`; every
//! update bundle is signature-checked against that key before it is
//! installed. Nothing is installed without the user saying yes. Automatic
//! checks stay silent on failure (offline, manifest missing, bad signature) —
//! the app keeps working and tries again at the next interval. The tray's
//! "Check for updates" runs the same flow but always tells the user the
//! outcome ("up to date", "could not check").

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use tauri::{AppHandle, Runtime};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::UpdaterExt;

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// One check/prompt at a time, so the timer and the tray item cannot stack
/// two dialogs.
static CHECKING: AtomicBool = AtomicBool::new(false);

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

pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        // A version the user said "Later" to is not asked about again until
        // the next launch.
        let mut declined: Option<String> = None;
        std::thread::sleep(FIRST_CHECK_DELAY);
        loop {
            tauri::async_runtime::block_on(check_once(&app, &mut declined, false));
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// Tray item: check now and always report the outcome.
pub(crate) fn check_now<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        tauri::async_runtime::block_on(check_once(&app, &mut None, true));
    });
}

fn info<R: Runtime>(app: &AppHandle<R>, title: &str, message: String) {
    let _ = app.dialog().message(message).title(title).show(|_| {});
}

async fn check_once<R: Runtime>(app: &AppHandle<R>, declined: &mut Option<String>, manual: bool) {
    let Some(_guard) = CheckGuard::acquire() else {
        return;
    };
    let Ok(updater) = app.updater() else {
        if manual {
            info(app, "Updates", "Updates are not available in this build.".into());
        }
        return;
    };
    let update = match updater.check().await {
        Ok(Some(update)) => update,
        Ok(None) => {
            if manual {
                info(
                    app,
                    "You're up to date",
                    format!(
                        "CoolerBox Tracker {} is the latest version.",
                        app.package_info().version
                    ),
                );
            }
            return;
        }
        Err(_) => {
            if manual {
                info(
                    app,
                    "Could not check for updates",
                    "Check your internet connection and try again.".into(),
                );
            }
            return;
        }
    };
    if !manual && declined.as_deref() == Some(update.version.as_str()) {
        return;
    }

    let ask = app
        .dialog()
        .message(format!(
            "CoolerBox Tracker {} is available (you have {}).\n\nUpdate now? The app will restart.",
            update.version, update.current_version
        ))
        .title("Update available")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Update and restart".into(),
            "Later".into(),
        ));
    let accepted = tauri::async_runtime::spawn_blocking(move || ask.blocking_show())
        .await
        .unwrap_or(false);
    if !accepted {
        *declined = Some(update.version.clone());
        return;
    }

    match update.download_and_install(|_, _| {}, || {}).await {
        Ok(()) => app.restart(),
        Err(_) => info(
            app,
            "Update failed",
            "The update could not be installed. You can keep using the current version and try again later.".into(),
        ),
    }
}
