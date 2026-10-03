//! The Linux AppImage sets itself up as an app.
//!
//! An AppImage is one file; it never installs a launcher. Without one, the
//! dock can't match the window to an icon (it shows a gear) and nothing on
//! the computer sends `tracker://` links (the "Open in the app" buttons in
//! emails) to the app. Found live, 2026-10-03: a launcher made by hand had
//! neither the window's id nor the link scheme.
//!
//! So on every start from an AppImage we keep a launcher in the person's own
//! `~/.local/share/applications`, named after the window's id
//! (`cbb-tracker-desktop`), pointing at wherever the AppImage is now, with
//! the app's icons beside it, and make it the handler for `tracker://`.
//! Nothing is written when it is already up to date. A launcher of that name
//! we didn't write is left alone. deb/rpm installs bring their own launcher;
//! macOS and Windows never get here.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

/// The window's id on Linux (the binary name), and so the launcher's name.
const APP_ID: &str = "cbb-tracker-desktop";

/// Marks a launcher as ours, so we only ever rewrite our own.
const MARKER: &str = "X-CoolerBox-Written-By-App=true";

/// The launcher for an AppImage at `appimage`.
pub(crate) fn launcher(appimage: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=CoolerBox Tracker\n\
         Comment=CoolerBox Production Tracker\n\
         Exec={exec} %u\n\
         TryExec={try_exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Office;ProjectManagement;\n\
         MimeType=x-scheme-handler/tracker;\n\
         StartupWMClass={APP_ID}\n\
         StartupNotify=true\n\
         {MARKER}\n",
        exec = exec_arg(appimage),
        try_exec = appimage.replace('\\', "\\\\"),
    )
}

/// `path` as one argument of a launcher's Exec line: quoted, with the
/// characters the desktop-entry spec reserves escaped, and `%` doubled so it
/// is never read as a field code.
pub(crate) fn exec_arg(path: &str) -> String {
    let mut quoted = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    // The file's own string escaping applies on top of the Exec quoting.
    quoted.replace('\\', "\\\\")
}

/// Whether to write `wanted` over whatever is at the launcher's path now.
pub(crate) fn should_write(existing: Option<&str>, wanted: &str) -> bool {
    match existing {
        None => true,
        Some(current) => current != wanted && current.lines().any(|line| line == MARKER),
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn keep_installed() {
    use std::path::PathBuf;

    let Some(appdir) = crate::system_open::appimage_dir() else {
        return;
    };
    let Some(appimage) = std::env::var("APPIMAGE").ok().filter(|p| p.starts_with('/')) else {
        return;
    };
    let Some(data_home) = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|d| d.starts_with('/'))
        .map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share")))
    else {
        return;
    };

    // Off the main thread: file work and two small host tools.
    std::thread::spawn(move || {
        let wanted = launcher(&appimage);
        let applications = data_home.join("applications");
        let path = applications.join(format!("{APP_ID}.desktop"));
        let existing = std::fs::read_to_string(&path).ok();
        if !should_write(existing.as_deref(), &wanted) {
            return;
        }

        // Icons first, so the launcher never points at a missing one.
        for size in ["32x32", "128x128", "256x256@2"] {
            let from = PathBuf::from(&appdir)
                .join("usr/share/icons/hicolor")
                .join(size)
                .join(format!("apps/{APP_ID}.png"));
            let to_dir = data_home.join("icons/hicolor").join(size).join("apps");
            if from.is_file() && std::fs::create_dir_all(&to_dir).is_ok() {
                let _ = std::fs::copy(&from, to_dir.join(format!("{APP_ID}.png")));
            }
        }

        // Write beside it and rename, so a half-written launcher is never read.
        let partial = applications.join(format!(".{APP_ID}.desktop.partial"));
        if std::fs::create_dir_all(&applications).is_err()
            || std::fs::write(&partial, &wanted).is_err()
            || std::fs::rename(&partial, &path).is_err()
        {
            let _ = std::fs::remove_file(&partial);
            return;
        }

        let host = crate::system_open::host_env(std::env::vars(), &appdir);
        let run = |program: &str, args: &[&str]| {
            let _ = std::process::Command::new(program)
                .args(args)
                .env_clear()
                .envs(host.clone())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        };
        let launcher_name = format!("{APP_ID}.desktop");
        run("xdg-mime", &["default", &launcher_name, "x-scheme-handler/tracker"]);
        if let Some(dir) = applications.to_str() {
            run("update-desktop-database", &[dir]);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_points_at_the_appimage_and_claims_tracker_links() {
        let entry = launcher("/home/crew/Applications/CoolerBoxTracker.AppImage");
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nExec=\"/home/crew/Applications/CoolerBoxTracker.AppImage\" %u\n"));
        assert!(entry.contains("\nTryExec=/home/crew/Applications/CoolerBoxTracker.AppImage\n"));
        assert!(entry.contains("\nIcon=cbb-tracker-desktop\n"));
        assert!(entry.contains("\nStartupWMClass=cbb-tracker-desktop\n"));
        assert!(entry.contains("\nMimeType=x-scheme-handler/tracker;\n"));
        assert!(entry.lines().any(|l| l == MARKER));
    }

    #[test]
    fn exec_quotes_spaces_and_escapes_reserved_characters() {
        assert_eq!(exec_arg("/opt/My Apps/Tracker.AppImage"), "\"/opt/My Apps/Tracker.AppImage\"");
        assert_eq!(exec_arg("/home/a$b/100%.AppImage"), "\"/home/a\\\\$b/100%%.AppImage\"");
        assert_eq!(exec_arg("/x/\"q\".AppImage"), "\"/x/\\\\\"q\\\\\".AppImage\"");
    }

    #[test]
    fn writes_a_missing_launcher_and_refreshes_its_own() {
        let wanted = launcher("/new/Tracker.AppImage");
        assert!(should_write(None, &wanted));
        assert!(should_write(Some(&launcher("/old/Tracker.AppImage")), &wanted));
        assert!(!should_write(Some(&wanted), &wanted));
    }

    #[test]
    fn never_overwrites_a_launcher_someone_else_made() {
        let theirs = "[Desktop Entry]\nName=CoolerBox Tracker\nExec=/opt/Tracker.AppImage\n";
        assert!(!should_write(Some(theirs), &launcher("/opt/Tracker.AppImage")));
    }
}
