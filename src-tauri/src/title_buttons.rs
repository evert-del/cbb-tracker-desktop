//! The host's title-bar buttons in the Linux AppImage.
//!
//! The linuxdeploy GTK hook points `GSETTINGS_SCHEMA_DIR` at the bundle's own
//! schemas only. Those carry upstream GNOME's default `button-layout`
//! (`appmenu:close`) and none of the distro's overrides, so on Ubuntu, whose
//! own override is `:minimize,maximize,close`, the window showed only a close
//! button. Found live, 2026-10-03.
//!
//! So in an AppImage we ask the host's `gsettings` (run with the bundle
//! stripped out, like `system_open`) for the real layout and set GTK's
//! `gtk-decoration-layout` to it. Anywhere else, or if the host has no answer
//! (not GNOME, no `gsettings`), nothing changes. macOS and Windows draw their
//! own title bars and never get here.

#[cfg(target_os = "linux")]
pub(crate) fn follow_host<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Some(appdir) = crate::system_open::appimage_dir() else {
        return;
    };
    let app = app.clone();
    // Off the main thread so a slow `gsettings` never holds up the window.
    std::thread::spawn(move || {
        let Ok(output) = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.wm.preferences", "button-layout"])
            .env_clear()
            .envs(crate::system_open::host_env(std::env::vars(), &appdir))
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            return;
        };
        if !output.status.success() {
            return;
        }
        let Some(layout) = parse_layout(&String::from_utf8_lossy(&output.stdout)) else {
            return;
        };
        let _ = app.run_on_main_thread(move || {
            use gtk::prelude::*;
            if let Some(settings) = gtk::Settings::default() {
                settings.set_property("gtk-decoration-layout", layout);
            }
        });
    });
}

/// `gsettings get` prints a GVariant string, e.g. `':minimize,maximize,close'`.
/// Returns the layout inside it, or `None` for anything that isn't one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_layout(printed: &str) -> Option<String> {
    let inner = printed.trim().strip_prefix('\'')?.strip_suffix('\'')?;
    let well_formed = inner.contains(':')
        && inner
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == ',' || c == ':' || c == '-' || c == '_');
    well_formed.then(|| inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::parse_layout;

    #[test]
    fn reads_ubuntus_layout() {
        assert_eq!(
            parse_layout("':minimize,maximize,close'\n").as_deref(),
            Some(":minimize,maximize,close")
        );
    }

    #[test]
    fn reads_buttons_on_the_left() {
        assert_eq!(
            parse_layout("'close,minimize,maximize:'").as_deref(),
            Some("close,minimize,maximize:")
        );
    }

    #[test]
    fn rejects_anything_else() {
        assert_eq!(parse_layout(""), None);
        assert_eq!(parse_layout("No such schema"), None);
        assert_eq!(parse_layout(":minimize,close"), None); // not quoted
        assert_eq!(parse_layout("'minimize,close'"), None); // no colon
        assert_eq!(parse_layout("'x:close; rm'"), None);
    }
}
