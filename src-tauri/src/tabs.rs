//! Making extra tracker tabs and windows easy to find without knowing any
//! shortcut (they exist to compare one review page with another).
//!
//! - macOS: every tracker window always shows the native tab bar, whose "+"
//!   opens a new tab (`newWindowForTab:`); tabs are grouped there, dragged
//!   out to stand side by side and merged back from the Window menu. The app
//!   menu adds File ▸ New Tab ⌘T / New Window ⌘N, and its Window menu is
//!   NSApp's windows menu, so macOS adds Merge All Windows, Move Tab to New
//!   Window and Show All Tabs.
//! - Everywhere: "New Window" in the quick panel and the tray menu, and the
//!   webview's own right-click "Open link in new window".
//! - The first time a second tracker window opens, a one-time message in it
//!   says how to put two pages side by side (and group them again).

use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

/// The one-time "side by side" message has been shown (settings.json).
const EXPLAINED: &str = "windows_explained";

/// The tracker window people are looking at: the focused one, else main.
pub(crate) fn front_tracker_window<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    let windows = app.webview_windows();
    windows
        .iter()
        .filter(|(label, _)| label.as_str() == "main" || crate::is_extra_tracker_window(label))
        .find(|(_, window)| window.is_focused().unwrap_or(false))
        .map(|(_, window)| window.clone())
        .or_else(|| app.get_webview_window("main"))
}

/// The page a new tab or window starts on: the same tracker page as
/// `window` (then pick the other version), or the tracker's home.
pub(crate) fn start_page<R: Runtime>(window: Option<&WebviewWindow<R>>) -> Option<url::Url> {
    window
        .and_then(|window| window.url().ok())
        .filter(|page| page.scheme() == "https" && page.host_str() == Some(crate::APP_HOST))
        .or_else(|| crate::APP_ORIGIN.parse().ok())
}

/// A new tab (macOS) or window next to the tracker window in front.
pub(crate) fn open_from_front<R: Runtime>(app: &AppHandle<R>, tab: bool) {
    let front = front_tracker_window(app);
    if let Some(page) = start_page(front.as_ref()) {
        let from = front.as_ref().map(|window| window.label().to_string());
        crate::open_tracker_window(app, from.as_deref(), page, tab);
    }
}

/// The one-time message for a second tracker window, in each system's words.
pub(crate) fn side_by_side_text(os: &str) -> (&'static str, &'static str) {
    match os {
        "macos" => (
            "Compare two pages side by side",
            "Drag a tab out of the tab bar to give it its own window, then put the windows next to each other. To group them again, choose Window ▸ Merge All Windows.",
        ),
        "windows" => (
            "Compare two pages side by side",
            "Drag this window to the left or right edge of the screen (or press Windows+Left / Right) and pick the other tracker window for the other half.",
        ),
        _ => (
            "Compare two pages side by side",
            "Drag this window to the left or right edge of the screen to fill half of it, then do the same with the other tracker window.",
        ),
    }
}

/// When a second tracker window first shows a tracker page: the one-time
/// side-by-side message in that window.
pub(crate) fn maybe_explain<R: Runtime>(app: &AppHandle<R>, label: &str) {
    if crate::close::flag(app, EXPLAINED) {
        return;
    }
    crate::close::set_flag(app, EXPLAINED, true);
    let (title, detail) = side_by_side_text(std::env::consts::OS);
    crate::download::toast_in(app, label, serde_json::json!({ "kind": "ok", "title": title, "detail": detail }));
}

/// macOS: the app menu (Tauri's default plus File ▸ New Tab / New Window,
/// with the Window menu as NSApp's windows menu), and its events.
#[cfg(target_os = "macos")]
pub(crate) fn install_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};

    let info = app.package_info();
    let about = AboutMetadata {
        name: Some(info.name.clone()),
        version: Some(info.version.to_string()),
        ..Default::default()
    };
    let window_menu = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;
    let menu = Menu::with_items(
        app,
        &[
            &Submenu::with_items(
                app,
                info.name.clone(),
                true,
                &[
                    &PredefinedMenuItem::about(app, None, Some(about))?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::services(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::quit(app, None)?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "File",
                true,
                &[
                    &MenuItem::with_id(app, "file-new-tab", "New Tab", true, Some("CmdOrCtrl+T"))?,
                    &MenuItem::with_id(app, "file-new-window", "New Window", true, Some("CmdOrCtrl+N"))?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::close_window(app, None)?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?,
            &Submenu::with_items(app, "View", true, &[&PredefinedMenuItem::fullscreen(app, None)?])?,
            &window_menu,
        ],
    )?;
    app.set_menu(menu)?;
    // macOS adds its tab commands (Merge All Windows, Move Tab to New
    // Window, Show All Tabs, Show/Hide Tab Bar) to the windows menu.
    window_menu.set_as_windows_menu_for_nsapp()?;
    app.on_menu_event(|app, event| match event.id().as_ref() {
        "file-new-tab" => open_from_front(app, true),
        "file-new-window" => open_from_front(app, false),
        _ => {}
    });
    Ok(())
}

/// macOS: show `window`'s tab bar if it is hidden (it is the clickable way
/// to see and open tabs; macOS hides it for a lone window by default).
#[cfg(target_os = "macos")]
pub(crate) fn show_tab_bar<R: Runtime>(window: &tauri::WebviewWindow<R>) {
    use objc2::msg_send;
    use objc2::runtime::{AnyObject, Bool};
    let Ok(ns_window) = window.ns_window() else { return };
    let ns_window = ns_window.cast::<AnyObject>();
    if ns_window.is_null() {
        return;
    }
    // SAFETY: a live NSWindow owned by Tauri, on the main thread.
    unsafe {
        let group: *mut AnyObject = msg_send![ns_window, tabGroup];
        if group.is_null() {
            return;
        }
        let visible: Bool = msg_send![group, isTabBarVisible];
        if !visible.as_bool() {
            let _: () = msg_send![ns_window, toggleTabBar: std::ptr::null_mut::<AnyObject>()];
        }
    }
}

/// macOS: the tab bar's "+" calls `newWindowForTab:` on the window; without
/// it the button isn't shown. Added once to the window class Tauri uses
/// (other windows have no tab group, so it never shows for them).
#[cfg(target_os = "macos")]
pub(crate) fn enable_plus_button<R: Runtime>(main: &tauri::WebviewWindow<R>) {
    use std::sync::OnceLock;

    use objc2::ffi;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::sel;

    /// Opens a tab next to the window whose "+" was clicked.
    static PLUS: OnceLock<Box<dyn Fn(*const AnyObject) + Send + Sync>> = OnceLock::new();

    unsafe extern "C-unwind" fn new_window_for_tab(this: *mut AnyObject, _sel: Sel, _sender: *mut AnyObject) {
        if let Some(plus) = PLUS.get() {
            plus(this);
        }
    }

    let app = main.app_handle().clone();
    let _ = PLUS.set(Box::new(move |clicked: *const AnyObject| {
        let windows = app.webview_windows();
        let from = windows.iter().find(|(label, window)| {
            (label.as_str() == "main" || crate::is_extra_tracker_window(label))
                && window.ns_window().is_ok_and(|ns| ns.cast_const().cast::<AnyObject>() == clicked)
        });
        let from_window = from.map(|(_, window)| window);
        if let Some(page) = start_page(from_window) {
            let label = from.map(|(label, _)| label.clone());
            crate::open_tracker_window(&app, label.as_deref(), page, true);
        }
    }));

    let Ok(ns_window) = main.ns_window() else { return };
    let ns_window = ns_window.cast::<AnyObject>();
    if ns_window.is_null() {
        return;
    }
    let imp: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) = new_window_for_tab;
    // SAFETY: adds a method with the documented signature to the window's
    // class; harmless if it already exists (then nothing changes).
    unsafe {
        let class = ffi::object_getClass(ns_window) as *mut AnyClass;
        let _ = ffi::class_addMethod(
            class,
            sel!(newWindowForTab:),
            std::mem::transmute::<_, Imp>(imp),
            c"v@:@".as_ptr(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_by_side_uses_each_systems_words() {
        assert!(side_by_side_text("macos").1.contains("Merge All Windows"));
        assert!(side_by_side_text("windows").1.contains("Windows+Left"));
        assert!(side_by_side_text("linux").1.contains("edge of the screen"));
    }
}
