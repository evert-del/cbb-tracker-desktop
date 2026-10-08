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

/// What the page shows, for its tab: its title, its main heading (a review
/// page's version label, else its h1) and its path.
const PAGE_LABEL_JS: &str = "JSON.stringify({t: document.title || '', \
    h: ((document.querySelector('.gd-title') || document.querySelector('main h1') || document.querySelector('h1') || {}).textContent || '').trim().slice(0, 80), \
    p: location.pathname})";

#[derive(serde::Deserialize, Default)]
struct PageLabel {
    #[serde(default)]
    t: String,
    #[serde(default)]
    h: String,
    #[serde(default)]
    p: String,
}

/// A tab / window title for what a tracker window shows. Most tracker pages
/// keep the site name as their title, so then the page's heading (a review
/// page's version label) names the tab; without one, its first path segment.
pub(crate) fn tab_title(page_title: &str, heading: &str, path: &str) -> String {
    const SITE: [&str; 2] = ["CoolerBox Production Tracker", "Production Tracker"];
    let title = page_title.trim();
    let title = SITE
        .iter()
        .find_map(|site| {
            ["|", "·", "-"]
                .iter()
                .find_map(|sep| title.strip_suffix(&format!("{sep} {site}")))
        })
        .map(str::trim_end)
        .unwrap_or(title);
    if !title.is_empty() && !SITE.contains(&title) {
        return title.to_string();
    }
    let heading = heading.split_whitespace().collect::<Vec<_>>().join(" ");
    if !heading.is_empty() {
        return heading;
    }
    let first = path.trim_matches('/').split('/').next().unwrap_or("");
    if first.is_empty() || first == "sign-in" {
        return "CoolerBox Tracker".to_string();
    }
    let words = first.replace('-', " ");
    let mut chars = words.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Keeps every tracker window's title (its tab's label on macOS, the
/// taskbar entry elsewhere) on the page it shows, so two tabs of two review
/// versions can be told apart. The tracker changes pages without a full
/// load, so this checks `document.title` every 1.5 s.
pub(crate) fn follow_titles<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        for (label, window) in app.webview_windows() {
            if label != "main" && !crate::is_extra_tracker_window(&label) {
                continue;
            }
            let target = window.clone();
            let _ = window.eval_with_callback(PAGE_LABEL_JS, move |raw| {
                // The page returns a JSON string; the callback gets it JSON-encoded.
                let json: String = serde_json::from_str(&raw).unwrap_or_default();
                let page: PageLabel = serde_json::from_str(&json).unwrap_or_default();
                let title = tab_title(&page.t, &page.h, &page.p);
                if target.title().ok().as_deref() != Some(title.as_str()) {
                    let _ = target.set_title(&title);
                }
            });
        }
    });
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

/// macOS: allow window tabbing app-wide again and keep the quick panel and
/// the Saved-for-offline library out of tab groups (NSWindowTabbingMode
/// Disallowed). Tauri switches tabbing off for every window whenever it
/// builds one without a tabbing identifier.
#[cfg(target_os = "macos")]
pub(crate) fn allow_tabs<R: Runtime>(app: &AppHandle<R>) {
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::msg_send;
    /// NSWindowTabbingModeDisallowed.
    const DISALLOWED: isize = 2;
    let Some(class) = AnyClass::get(c"NSWindow") else { return };
    // SAFETY: class and instance methods of NSWindow, on the main thread.
    unsafe {
        let _: () = msg_send![class, setAllowsAutomaticWindowTabbing: Bool::YES];
        for label in ["mini", "library"] {
            if let Some(ns_window) = app.get_webview_window(label).and_then(|w| w.ns_window().ok()) {
                let ns_window = ns_window.cast::<AnyObject>();
                if !ns_window.is_null() {
                    let _: () = msg_send![ns_window, setTabbingMode: DISALLOWED];
                }
            }
        }
    }
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
    /// NSWindowStyleMaskFullSizeContentView.
    const FULL_SIZE_CONTENT: usize = 1 << 15;
    // SAFETY: a live NSWindow owned by Tauri, on the main thread.
    unsafe {
        // Tauri's default title-bar style stretches the content under the
        // title bar and places the page below it once; the tab bar then grows
        // the title-bar area and the page covers it. Without the stretch, the
        // tab bar pushes the page down, as in Safari.
        let style: usize = msg_send![ns_window, styleMask];
        if style & FULL_SIZE_CONTENT != 0 {
            let _: () = msg_send![ns_window, setStyleMask: style & !FULL_SIZE_CONTENT];
        }
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
        // The window's real class (TaoWindow). object_getClass would give the
        // key-value-observing subclass AppKit slips in, which respondsToSelector:
        // doesn't consult, so the + stayed hidden (found live).
        let class: *const AnyClass = objc2::msg_send![ns_window, class];
        let class = class as *mut AnyClass;
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
    fn tab_titles_name_what_the_page_shows() {
        // A real page title wins, without the site name.
        assert_eq!(tab_title("Leave | CoolerBox Production Tracker", "", "/leave"), "Leave");
        // Most pages keep the site name: the heading names the tab.
        assert_eq!(
            tab_title("CoolerBox Production Tracker", "V3 ·  Final   cut", "/viewing/version/abc"),
            "V3 · Final cut"
        );
        assert_eq!(tab_title("Production Tracker", "Projects", "/projects"), "Projects");
        // No heading: the first path segment.
        assert_eq!(tab_title("CoolerBox Production Tracker", "", "/call-sheets/12"), "Call sheets");
        assert_eq!(tab_title("CoolerBox Production Tracker", "", "/sign-in"), "CoolerBox Tracker");
        assert_eq!(tab_title("", "", "/"), "CoolerBox Tracker");
    }

    #[test]
    fn side_by_side_uses_each_systems_words() {
        assert!(side_by_side_text("macos").1.contains("Merge All Windows"));
        assert!(side_by_side_text("windows").1.contains("Windows+Left"));
        assert!(side_by_side_text("linux").1.contains("edge of the screen"));
    }
}
