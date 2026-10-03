//! Opening links and files in the user's own apps.
//!
//! Inside the Linux AppImage the shell runs with the bundle's libraries and
//! GTK/GIO settings (`LD_LIBRARY_PATH`, `GIO_MODULE_DIR`, `XDG_DATA_DIRS`, …,
//! set by AppRun and the linuxdeploy GTK hook), and every child inherits them.
//! `xdg-open` then runs the *system* `gio` against the *bundled* GLib, which
//! fails ("undefined symbol: g_unix_mount_entry_get_options" on Ubuntu 24.04+),
//! and xdg-open falls back to its own browser list — Firefox, not the user's
//! default. Found live, 2026-10-03: Chrome was the default, Firefox opened.
//!
//! So in an AppImage we start `xdg-open` with the bundle stripped out of the
//! environment. Everywhere else (deb/rpm, macOS, Windows) the opener plugin is
//! used unchanged, and it stays the fallback if the spawn fails.

use tauri::{AppHandle, Runtime};
use tauri_plugin_opener::OpenerExt;

/// Variables only the AppImage sets; the host never needs them.
const BUNDLE_ONLY_VARS: &[&str] = &[
    "APPDIR",
    "APPIMAGE",
    "ARGV0",
    "OWD",
    "LD_LIBRARY_PATH",
    "GIO_MODULE_DIR",
    "GSETTINGS_SCHEMA_DIR",
    "GI_TYPELIB_PATH",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GTK_PATH",
    "GTK_THEME",
    "GDK_PIXBUF_MODULE_FILE",
];

/// The environment the host had before the AppImage changed it: bundle-only
/// variables dropped, and every `:`-separated entry inside `appdir` removed
/// from the rest (XDG_DATA_DIRS, PATH, …). A list left empty is dropped.
pub(crate) fn host_env(
    vars: impl IntoIterator<Item = (String, String)>,
    appdir: &str,
) -> Vec<(String, String)> {
    let appdir = appdir.trim_end_matches('/');
    vars.into_iter()
        .filter(|(key, _)| !BUNDLE_ONLY_VARS.contains(&key.as_str()))
        .filter_map(|(key, value)| {
            if appdir.is_empty() || !value.contains(appdir) {
                return Some((key, value));
            }
            let kept: Vec<&str> = value
                .split(':')
                .filter(|entry| !entry.is_empty() && !entry.starts_with(appdir))
                .collect();
            (!kept.is_empty()).then(|| (key, kept.join(":")))
        })
        .collect()
}

/// The AppImage's mount point, when running from one on Linux.
pub(crate) fn appimage_dir() -> Option<String> {
    if !cfg!(target_os = "linux") || std::env::var_os("APPIMAGE").is_none() {
        return None;
    }
    std::env::var("APPDIR").ok().filter(|dir| !dir.is_empty())
}

/// Starts the host's `xdg-open` on `target` with the bundle stripped out.
fn spawn_host_xdg_open(target: &str, appdir: &str) -> std::io::Result<()> {
    let mut child = std::process::Command::new("xdg-open")
        .arg(target)
        .env_clear()
        .envs(host_env(std::env::vars(), appdir))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // xdg-open hands off and exits; reap it so it never lingers as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Opens `url` in the user's default app for it (browser, mail, calendar).
pub(crate) fn open_url<R: Runtime>(app: &AppHandle<R>, url: &str) {
    if let Some(appdir) = appimage_dir() {
        if spawn_host_xdg_open(url, &appdir).is_ok() {
            return;
        }
    }
    let _ = app.opener().open_url(url, None::<&str>);
}

/// Opens a local file in the user's default app for it.
pub(crate) fn open_path<R: Runtime>(app: &AppHandle<R>, path: &str) -> Result<(), String> {
    if let Some(appdir) = appimage_dir() {
        if spawn_host_xdg_open(path, &appdir).is_ok() {
            return Ok(());
        }
    }
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn strips_the_bundle_from_the_environment() {
        let dir = "/tmp/.mount_CoolerLgjMlg";
        let out = host_env(
            env(&[
                ("HOME", "/home/crew"),
                ("APPDIR", dir),
                ("APPIMAGE", "/opt/CoolerBoxTracker/CoolerBoxTracker.AppImage"),
                ("LD_LIBRARY_PATH", "/tmp/.mount_CoolerLgjMlg/usr/lib/:/tmp/.mount_CoolerLgjMlg/usr/lib64"),
                ("GIO_MODULE_DIR", "/tmp/.mount_CoolerLgjMlg//usr/lib/gio/modules"),
                ("GTK_THEME", "Adwaita:light"),
                (
                    "XDG_DATA_DIRS",
                    "/tmp/.mount_CoolerLgjMlg/usr/share/:/tmp/.mount_CoolerLgjMlg/usr/share:/usr/share:/usr/share/ubuntu",
                ),
                ("PATH", "/tmp/.mount_CoolerLgjMlg/usr/bin:/usr/local/bin:/usr/bin"),
                ("DISPLAY", ":0"),
            ]),
            dir,
        );
        assert_eq!(
            out,
            env(&[
                ("HOME", "/home/crew"),
                ("XDG_DATA_DIRS", "/usr/share:/usr/share/ubuntu"),
                ("PATH", "/usr/local/bin:/usr/bin"),
                ("DISPLAY", ":0"),
            ])
        );
    }

    #[test]
    fn a_list_made_only_of_bundle_entries_is_dropped() {
        let dir = "/tmp/.mount_X";
        let out = host_env(env(&[("XDG_CONFIG_DIRS", "/tmp/.mount_X/etc/xdg")]), dir);
        assert!(out.is_empty());
    }

    #[test]
    fn unrelated_values_are_untouched() {
        let out = host_env(env(&[("LANG", "en_ZA.UTF-8"), ("DISPLAY", ":0")]), "/tmp/.mount_X/");
        assert_eq!(out, env(&[("LANG", "en_ZA.UTF-8"), ("DISPLAY", ":0")]));
    }
}
