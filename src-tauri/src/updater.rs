//! Self-update of the desktop shell (the website itself updates on load).
//!
//! The endpoint and the minisign public key live in `tauri.conf.json`; every
//! update bundle is signature-checked against that key before it is
//! installed. Nothing is installed without the user saying yes. Failures
//! (offline, manifest missing, bad signature) are silent — the app keeps
//! working on the current version and tries again at the next interval.

use std::time::Duration;

use tauri::{AppHandle, Runtime};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::UpdaterExt;

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        // A version the user said "Later" to is not asked about again until
        // the next launch.
        let mut declined: Option<String> = None;
        std::thread::sleep(FIRST_CHECK_DELAY);
        loop {
            tauri::async_runtime::block_on(check_once(&app, &mut declined));
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

async fn check_once<R: Runtime>(app: &AppHandle<R>, declined: &mut Option<String>) {
    let Ok(updater) = app.updater() else { return };
    let Ok(Some(update)) = updater.check().await else {
        return;
    };
    if declined.as_deref() == Some(update.version.as_str()) {
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
        Err(_) => {
            let _ = app
                .dialog()
                .message("The update could not be installed. You can keep using the current version and try again later.")
                .title("Update failed")
                .show(|_| {});
        }
    }
}
