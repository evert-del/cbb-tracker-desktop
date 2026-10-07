//! CoolerBox Tracker desktop shell.
//!
//! The main window loads the hosted production tracker as a remote URL, so
//! website deploys land here with no desktop rebuild. This module owns the
//! two URL boundaries:
//!
//! - Outbound: first-party app and authentication hosts stay in the WebView
//!   (the Supabase session cookies must land in the app's own jar); anything
//!   else opens in the system browser. OAuth *starts* stay in-app in v1
//!   because the server's `redirectTo` is the https callback, which only the
//!   app's jar can complete — Google may refuse embedded WebViews, in which
//!   case password/magic-link sign-in in the same window is the fallback.
//! - Inbound: `tracker://` deep links (email buttons on machines with the app)
//!   are translated to their hosted https equivalents and loaded in the main
//!   window. Unknown shapes are ignored, never navigated blindly.

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::{DownloadEvent, PageLoadEvent, WebviewWindowBuilder},
    AppHandle, Manager, Runtime, WebviewUrl,
};
use tauri_plugin_deep_link::DeepLinkExt;

mod clock;
mod desktop_entry;
mod diagnostics;
mod download;
mod idle;
mod location;
mod mini;
mod notify;
mod offline;
mod system_open;
mod title_buttons;
mod updater;

/// Tray icon id, so the poller can update its tooltip.
const TRAY_ID: &str = "main-tray";

/// Hosted production tracker. The only remote content the WebView loads.
const APP_ORIGIN: &str = "https://tracker.coolerboxbrothers.com";

/// Bare host of the tracker, for exact-match comparisons.
const APP_HOST: &str = "tracker.coolerboxbrothers.com";

/// First screen on launch: the sign-in form, never the marketing homepage.
/// A persisted session signs straight through from here; anyone signed out
/// sees the login form immediately.
const START_URL: &str = "https://tracker.coolerboxbrothers.com/sign-in";

/// Hosts allowed to load inside the WebView: the app itself, Supabase Auth,
/// and the OAuth/billing providers' sign-in pages. Everything else opens in
/// the system browser. Deliberately explicit (allow-list, not suffix match).
///
/// This doubles as the iframe policy: `on_navigation` cannot tell a subframe
/// load from a top-level one, so any host a first-party page legitimately
/// embeds must be listed here — otherwise the frame is cancelled in-page AND
/// flung at the system browser. Found live: cancelling the Turnstile iframe
/// (`challenges.cloudflare.com`) silently kills every sign-in with BotCheck
/// while the challenge URL keeps popping open externally.
const IN_APP_HOSTS: &[&str] = &[
    APP_HOST,
    // Supabase Auth (project-ref host of the public anon URL, not a secret).
    "wlwdhorybvelwbmhtftw.supabase.co",
    // Turnstile bot-check widget + challenge frames (sign-in, password
    // reset, invitation ask, sign-up email step).
    "challenges.cloudflare.com",
    // OAuth + billing sign-in pages.
    "accounts.google.com",
    "login.xero.com",
    "auth.services.adobe.com",
    // Adobe's sign-in page POSTs here (`/ims/fromSusi`); handed to the
    // system browser it becomes a GET, which Adobe refuses.
    "adobeid-na1.services.adobe.com",
    "ims-na1.adobelogin.com",
    "checkout.paystack.com",
];

/// First path segments of `tracker://` URLs that map to hosted pages.
/// Anything else is ignored.
const DEEP_LINK_SEGMENTS: &[&str] = &["auth", "view", "quote", "invoice", "invite", "cb", "r"];

/// True when the WebView itself may load `url`.
pub(crate) fn webview_may_load(url: &url::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    matches!(url.host_str(), Some(host) if IN_APP_HOSTS.contains(&host))
}

/// Sign-in and payment providers that send the window on through hosts of
/// their own: Adobe hops through its other sites to set its session
/// (adobeid-na1.services.adobe.com, sso.behance.net, ...), Google through
/// accounts.youtube.com, a card's 3-D Secure through the bank. Those hops
/// carry the provider's session, so a browser opened on one gets an error
/// ("Bad cdscKey", "unsupported method GET"). Once the window reaches one of
/// these hosts it follows any https page until it is back on the tracker,
/// rather than chasing each provider's hops in `IN_APP_HOSTS`.
const HAND_OFF_HOSTS: &[&str] = &[
    "accounts.google.com",
    "login.xero.com",
    "auth.services.adobe.com",
    "ims-na1.adobelogin.com",
    "checkout.paystack.com",
];

/// Whether the main window may load `url`, tracking in `handed_off` whether
/// it is out at a provider (see `HAND_OFF_HOSTS`). Back on the tracker ends
/// the hand-off. The tracker embeds no page from these hosts, so a hand-off
/// only starts when the window itself goes there.
pub(crate) fn main_window_may_load(url: &url::Url, handed_off: &mut bool) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    match url.host_str() {
        Some(APP_HOST) => {
            *handed_off = false;
            true
        }
        Some(host) if HAND_OFF_HOSTS.contains(&host) => {
            *handed_off = true;
            true
        }
        _ => *handed_off || webview_may_load(url),
    }
}

/// Schemes handed to the OS when the WebView may not load them itself.
/// `webcal` is My Schedule's "Open in Apple Calendar or Outlook" feed link.
/// Anything else (javascript:, file:, data:, …) is dropped.
const EXTERNAL_SCHEMES: &[&str] = &["https", "http", "mailto", "tel", "webcal"];

/// True when a link the WebView will not load should open in the OS instead.
pub(crate) fn opens_externally(url: &url::Url) -> bool {
    EXTERNAL_SCHEMES.contains(&url.scheme())
}

/// What to do with a page-requested new window (`target="_blank"`,
/// `window.open`). First-party files (attachments, Cooler Box items) are
/// saved to Downloads through the shell, with the window's own session
/// cookies — the tracker page itself never navigates away. Other
/// first-party links load in the main window (so the session cookie is
/// sent); everything else goes to the system browser.
#[derive(Debug, PartialEq)]
pub(crate) enum NewWindowAction {
    /// First-party file (attachment, Cooler Box item): open it from Downloads.
    SaveFile,
    LoadInMain,
    OpenExternal,
    Ignore,
}

pub(crate) fn new_window_action(url: &url::Url) -> NewWindowAction {
    if download::is_file_url(url) {
        return NewWindowAction::SaveFile;
    }
    if webview_may_load(url) {
        return NewWindowAction::LoadInMain;
    }
    if opens_externally(url) {
        NewWindowAction::OpenExternal
    } else {
        NewWindowAction::Ignore
    }
}

/// Translate an inbound `tracker://<segment>/...` URL into its hosted https
/// equivalent. Returns `None` for anything outside `DEEP_LINK_SEGMENTS`.
fn hosted_url_for_deep_link(url: &url::Url) -> Option<url::Url> {
    if url.scheme() != "tracker" {
        return None;
    }
    let segment = url.host_str()?;
    if !DEEP_LINK_SEGMENTS.contains(&segment) {
        return None;
    }
    let mut hosted = format!("{APP_ORIGIN}/{segment}{}", url.path());
    if let Some(query) = url.query() {
        hosted.push('?');
        hosted.push_str(query);
    }
    hosted.parse().ok()
}

/// Load an inbound deep link in the main window and bring it forward.
/// `tracker://offline` opens the Saved-for-offline library instead.
fn route_deep_link<R: Runtime>(app: &AppHandle<R>, url: &url::Url) {
    if url.scheme() == "tracker" && url.host_str() == Some("offline") {
        show_library(app);
        return;
    }
    let Some(hosted) = hosted_url_for_deep_link(url) else {
        return;
    };
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.navigate(hosted);
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// True when a `document.cookie` string holds a Supabase auth-token cookie.
/// Chunked jars (`.0`, `.1` suffixes) share the same `sb-<ref>-auth-token`
/// name prefix, so one check covers both.
fn has_session_cookie(cookies: &str) -> bool {
    cookies.split("; ").any(|pair| {
        let name = pair.split('=').next().unwrap_or("");
        name.starts_with("sb-") && name.contains("-auth-token")
    })
}

/// Bring the main window forward. Restores the dock icon on macOS (see
/// the close handler: a hidden app is a pure menu-bar app, like Toggl).
pub(crate) fn show_main<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Bring the Saved-for-offline library forward.
pub(crate) fn show_library<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(library) = app.get_webview_window("library") {
        let _ = library.show();
        let _ = library.unminimize();
        let _ = library.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // A second launch carrying a tracker:// URL (Windows/Linux)
            // lands here; forward it to the running window.
            let mut routed = false;
            for arg in args {
                if let Ok(url) = arg.parse::<url::Url>() {
                    if url.scheme() == "tracker" {
                        route_deep_link(app, &url);
                        routed = true;
                    }
                }
            }
            if !routed {
                show_main(app);
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_notification::init())
        // No injected click handler: it cancels every target="_blank" link
        // and asks the page to invoke plugin:opener|open_url, which the
        // remote window may not (no IPC), so those links did nothing. With
        // it off they reach on_new_window / on_navigation, as intended.
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    use tauri_plugin_global_shortcut::{Code, Modifiers, ShortcutState};
                    if !matches!(event.state(), ShortcutState::Pressed) {
                        return;
                    }
                    // Cmd+Shift+I (Ctrl+Shift+I on Windows/Linux): smart
                    // clock toggle — in when out, wrap when in, back when
                    // on a break. Notice-gating stays in `clock::act`.
                    let toggle = tauri_plugin_global_shortcut::Shortcut::new(
                        Some(Modifiers::SUPER | Modifiers::SHIFT),
                        Code::KeyI,
                    );
                    if shortcut == &toggle {
                        let action = app
                            .try_state::<clock::Last>()
                            .and_then(|last| {
                                last.0.lock().ok().and_then(|slot| slot.clone())
                            })
                            .map(|c| match c.state.as_str() {
                                "in" => "wrap",
                                "break" => "back",
                                _ => "in",
                            })
                            .unwrap_or("in");
                        clock::act(app, action);
                        return;
                    }
                    // Cmd+Shift+M: toggle the mini panel.
                    let panel = tauri_plugin_global_shortcut::Shortcut::new(
                        Some(Modifiers::SUPER | Modifiers::SHIFT),
                        Code::KeyM,
                    );
                    if shortcut == &panel {
                        mini::toggle(app);
                    }
                })
                .build(),
        )
        // Closing a window hides it; the app keeps running in the tray /
        // menu bar so notifications keep arriving. On macOS the dock icon
        // goes away with the last visible window (pure menu-bar app, like
        // Toggl) and comes back in show_main/show_library — so there is
        // never a dead dock icon. Quit is in the tray menu (or Cmd+Q).
        .on_window_event(|window, event| {
            if window.label() == "main" || window.label() == "mini" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                    // With the last visible window gone the dock icon goes
                    // too (pure menu-bar app). It returns in show_main /
                    // show_library, so there is never a dead dock icon.
                    #[cfg(target_os = "macos")]
                    {
                        let app = window.app_handle();
                        let hidden = window.label().to_string();
                        let any_other = ["main", "mini", "library"]
                            .iter()
                            .filter(|label| label.to_string() != hidden)
                            .any(|label| {
                                app.get_webview_window(label)
                                    .and_then(|w| w.is_visible().ok())
                                    .unwrap_or(false)
                            });
                        if !any_other {
                            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                        }
                    }
                }
            }
            // Panel behavior: the mini hides when it loses focus.
            if window.label() == "mini" {
                if matches!(event, tauri::WindowEvent::Focused(false)) {
                    let _ = window.hide();
                }
            }
        })
        .manage(std::sync::Mutex::new(None::<offline::PendingCapture>))
        .invoke_handler(tauri::generate_handler![
            offline::offline_save_pdf,
            offline::offline_list,
            offline::offline_open,
            offline::offline_delete,
            offline::offline_storage_info,
            mini::mini_action,
            mini::mini_expand,
            mini::mini_expand_notifications,
            mini::mini_hide,
            mini::mini_open,
            mini::mini_drag_start,
            mini::mini_drag_move,
            mini::mini_drag_end
        ])
        .setup(|app| {
            #[cfg(target_os = "linux")]
            desktop_entry::keep_installed();
            #[cfg(target_os = "linux")]
            title_buttons::follow_host(app.handle());
            let opener_handle = app.handle().clone();
            // Whether time away can be read from input here (not only from
            // sleep), so the tracker knows its "you were away" prompt is real.
            let idle_supported = idle::supported();
            let download_handle = app.handle().clone();
            let handed_off = std::sync::Mutex::new(false);
            let main_window = WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External(START_URL.parse().expect("START_URL is a valid URL")),
            )
            .title("CoolerBox Tracker")
            // Lets the tracker know it is inside the app (it hides "Get the
            // desktop app"), and whether the shell reports time away for the
            // time sheet (idle.rs). Plain values on the page, not IPC.
            .initialization_script(&format!(
                "window.cbbDesktopApp=Object.freeze({{version:{:?},idleSupported:{}}});",
                env!("CARGO_PKG_VERSION"),
                idle_supported
            ))
            // Download helpers (nav_bar.js): file links are intercepted in
            // the page and handed to the shell, which saves them with the
            // window's own session. No on-page buttons: the tracker page is
            // left exactly as the website made it.
            .initialization_script(include_str!("nav_bar.js"))
            // Location for the time sheet: the tracker's own pages only
            // (location.rs; macOS is wired up after the window is built).
            .on_permission_request(|webview, kind| {
                location::decide(webview.url().ok().as_ref(), kind)
            })
            .inner_size(1280.0, 800.0)
            .min_inner_size(1024.0, 640.0)
            .on_navigation(move |url| {
                // nav_bar.js hands a clicked file link over as
                // cbb-download://go?u=<https url>[&save=1]: open it (or, for
                // a `download` link, save it), staying on the page.
                if url.scheme() == "cbb-download" {
                    let save_as = url.query_pairs().any(|(key, value)| key == "save" && value == "1");
                    if let Some(target) = url
                        .query_pairs()
                        .find(|(key, _)| key == "u")
                        .and_then(|(_, value)| value.parse::<url::Url>().ok())
                    {
                        download::start(&opener_handle, target, save_as);
                    }
                    return false;
                }
                // "Show in folder" button of the download message.
                if url.scheme() == "cbb-reveal" {
                    if let Some(path) = url
                        .query_pairs()
                        .find(|(key, _)| key == "p")
                        .map(|(_, value)| value.into_owned())
                    {
                        download::reveal(&opener_handle, &path);
                    }
                    return false;
                }
                // about:blank / about:srcdoc frames are created by widgets
                // like Turnstile and carry no network content.
                if url.scheme() == "about" {
                    return true;
                }
                let allowed = match handed_off.lock() {
                    Ok(mut state) => main_window_may_load(url, &mut state),
                    Err(_) => webview_may_load(url),
                };
                if allowed {
                    return true;
                }
                // `tracker://` URLs are delivered to route_deep_link by the
                // deep-link plugin; anything else foreign leaves the app.
                if opens_externally(url) {
                    system_open::open_url(&opener_handle, url.as_str());
                }
                false
            })
            .on_new_window({
                let app = app.handle().clone();
                move |url, _features| {
                    match new_window_action(&url) {
                        NewWindowAction::SaveFile => download::start(&app, url, false),
                        NewWindowAction::LoadInMain => {
                            if let Some(main) = app.get_webview_window("main") {
                                let _ = main.navigate(url);
                            }
                        }
                        NewWindowAction::OpenExternal => {
                            system_open::open_url(&app, url.as_str());
                        }
                        NewWindowAction::Ignore => {}
                    }
                    tauri::webview::NewWindowResponse::Deny
                }
            })
            .on_download(move |_webview, event| match event {
                DownloadEvent::Requested { url, destination } => {
                    offline::handle_download(&download_handle, &url, destination)
                }
                DownloadEvent::Finished { url, path, success } => {
                    if offline::is_capture_url(&download_handle, &url) {
                        offline::finish_download(&download_handle, &url, success);
                    } else {
                        offline::finish_regular_download(&download_handle, path, success);
                    }
                    true
                }
                _ => true,
            })
            .on_page_load(|window, payload| {
                if !matches!(payload.event(), PageLoadEvent::Finished) {
                    return;
                }
                let Ok(page) = payload.url().to_string().parse::<url::Url>() else {
                    return;
                };
                if page.host_str() != Some(APP_HOST) || page.path() != "/sign-in" {
                    return;
                }
                // Cold start with a persisted session: the Supabase
                // auth-token cookie is script-readable, so peek at the jar
                // and skip the form straight to the dashboard. No cookie —
                // stay on the login screen. An expired session bounces back
                // here through the app's own auth gate.
                let probe = window.clone();
                let _ = window.eval_with_callback("document.cookie", move |cookies_json| {
                    let cookies: String = serde_json::from_str(&cookies_json).unwrap_or_default();
                    if has_session_cookie(&cookies) {
                        if let Ok(home) = APP_ORIGIN.parse::<url::Url>() {
                            let _ = probe.navigate(home);
                        }
                    }
                });
            })
            .build()?;
            #[cfg(target_os = "macos")]
            location::enable(&main_window);
            #[cfg(not(target_os = "macos"))]
            let _ = main_window;

            // Mini bar (mini.rs + mini.html): hidden until the tray toggle.
            // Frameless, transparent, always on top, out of Alt-Tab.
            WebviewWindowBuilder::new(app, "mini", WebviewUrl::App("mini.html".into()))
                .title("Tracker Mini")
                .inner_size(340.0, 320.0)
                .min_inner_size(300.0, 240.0)
                .resizable(false)
                .decorations(false)
                .transparent(true)
                .always_on_top(true)
                .skip_taskbar(true)
                .visible(false)
                .focused(false)
                .build()?;

            // Cold start through a tracker:// URL.
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                for url in urls {
                    route_deep_link(app.handle(), &url);
                }
            }
            // Warm tracker:// URLs while running.
            let link_handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    route_deep_link(&link_handle, &url);
                }
            });

            // System tray: quick access without a dock/taskbar window.
            // The inbox rows below are the click path for notifications:
            // banners themselves have no reliable click callback on macOS,
            // so each poll refreshes these 3 rows (label + href) instead.
            let tray_show =
                MenuItem::with_id(app, "tray-show", "Show Tracker", true, None::<&str>)?;
            let tray_notifications =
                MenuItem::with_id(app, "tray-notifications", "Notifications", true, None::<&str>)?;
            let inbox_1 =
                MenuItem::with_id(app, "inbox-1", "No unread notifications", false, None::<&str>)?;
            let inbox_2 = MenuItem::with_id(app, "inbox-2", "—", false, None::<&str>)?;
            let inbox_3 = MenuItem::with_id(app, "inbox-3", "—", false, None::<&str>)?;
            let tray_offline =
                MenuItem::with_id(app, "tray-offline", "Saved for offline", true, None::<&str>)?;
            let tray_mini =
                MenuItem::with_id(app, "tray-mini", "Mini bar", true, None::<&str>)?;
            let tray_update =
                MenuItem::with_id(app, "tray-update", "Check for updates", true, None::<&str>)?;
            let tray_diagnostics =
                MenuItem::with_id(app, "tray-diagnostics", "Diagnostics", true, None::<&str>)?;            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            let tray_autostart = {
                MenuItem::with_id(
                    app,
                    "tray-autostart",
                    "Launch at login",
                    true,
                    None::<&str>,
                )?
            };
            let tray_quit = MenuItem::with_id(app, "tray-quit", "Quit", true, None::<&str>)?;
            // The time sheet's clock (clock.rs): a line saying where you are,
            // then the four taps, enabled as they make sense.
            let clock_items = clock::Items {
                status: MenuItem::with_id(app, "clock-status", "Time sheet: not in use", false, None::<&str>)?,
                call_in: MenuItem::with_id(app, "clock-in", "Call in", false, None::<&str>)?,
                take_break: MenuItem::with_id(app, "clock-break", "Break", false, None::<&str>)?,
                back: MenuItem::with_id(app, "clock-back", "Back from break", false, None::<&str>)?,
                wrap: MenuItem::with_id(app, "clock-wrap", "Wrap", false, None::<&str>)?,
            };
            let separator = PredefinedMenuItem::separator(app)?;
            let separator2 = PredefinedMenuItem::separator(app)?;
            let tray_menu = Menu::with_items(
                app,
                &[
                    &clock_items.status, &clock_items.call_in, &clock_items.take_break, &clock_items.back,
                    &clock_items.wrap, &separator, &tray_show, &tray_notifications, &inbox_1, &inbox_2, &inbox_3,
                    &separator2, &tray_offline, &tray_mini, &tray_update, &tray_diagnostics,
                    #[cfg(not(any(target_os = "android", target_os = "ios")))]
                    &tray_autostart,
                    &tray_quit,
                ],
            )?;
            app.manage(clock_items);
            app.manage(clock::Last::default());
            app.manage(clock::Net::default());
            app.manage(notify::Unread(std::sync::Mutex::new(0)));
            app.manage(notify::Snapshot::default());
            app.manage(notify::InboxItems {
                rows: [inbox_1, inbox_2, inbox_3],
                hrefs: std::sync::Mutex::new([None, None, None]),
                ids: std::sync::Mutex::new([None, None, None]),
            });
            // Autostart checkbox reflects actual state at launch.
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            {
                use tauri_plugin_autostart::ManagerExt;
                if app.autolaunch().is_enabled().unwrap_or(false) {
                    let _ = tray_autostart.set_text("✓ Launch at login");
                }
            }
            // Cmd+Shift+I toggles the clock from anywhere, Cmd+Shift+M the
            // mini panel. Best-effort: macOS may need Accessibility
            // permission; failure just means no hotkey, the tray items keep
            // working.
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};
                let toggle =
                    Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyI);
                let _ = app.global_shortcut().register(toggle);
                let panel =
                    Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyM);
                let _ = app.global_shortcut().register(panel);
            }
            let _tray = TrayIconBuilder::with_id(TRAY_ID)
                // Monochrome template icon (tray-icon.png): no backdrop, the
                // OS tints it white/black for the user's light/dark menu bar.
                .icon(
                    tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))
                        .expect("bundled tray icon parses"),
                )
                .icon_as_template(true)
                .menu(&tray_menu)
                .tooltip("CoolerBox Tracker")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "tray-show" => show_main(app),
                    "tray-notifications" => {
                        if let (Some(window), Ok(url)) = (
                            app.get_webview_window("main"),
                            format!("{APP_ORIGIN}/notifications").parse::<url::Url>(),
                        ) {
                            let _ = window.navigate(url);
                        }
                        show_main(app);
                    }
                    "inbox-1" => notify::open_inbox(app, 0),
                    "inbox-2" => notify::open_inbox(app, 1),
                    "inbox-3" => notify::open_inbox(app, 2),
                    "tray-offline" => show_library(app),
                    "tray-mini" => mini::toggle(app),                    "clock-in" => clock::act(app, "in"),
                    "clock-break" => clock::act(app, "break"),
                    "clock-back" => clock::act(app, "back"),
                    "clock-wrap" => clock::act(app, "wrap"),
                    "tray-update" => updater::check_now(app.clone()),
                    "tray-diagnostics" => diagnostics::show(app),
                    "tray-autostart" => {
                        #[cfg(not(any(target_os = "android", target_os = "ios")))]
                        {
                            use tauri_plugin_autostart::ManagerExt;
                            use tauri_plugin_notification::NotificationExt;
                            let manager = app.autolaunch();
                            let enabled = manager.is_enabled().unwrap_or(false);
                            if enabled {
                                let _ = manager.disable();
                                let _ = app
                                    .notification()
                                    .builder()
                                    .title("CoolerBox Tracker")
                                    .body("Launch at login turned off.")
                                    .show();
                            } else {
                                let _ = manager.enable();
                                let _ = app
                                    .notification()
                                    .builder()
                                    .title("CoolerBox Tracker")
                                    .body("Launch at login turned on.")
                                    .show();
                            }
                        }
                    }
                    "tray-quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // Menu-bar-app feel: left-click toggles the mini panel.
                    // The full menu (with Show Tracker) is on right-click.
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        mini::toggle(tray.app_handle());
                    }
                })
                .build(app)?;

            notify::start(app.handle().clone());
            idle::start(app.handle().clone());
            clock::start(app.handle().clone());
            updater::start(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(raw: &str) -> url::Url {
        url::Url::parse(raw).expect("test URL parses")
    }

    #[test]
    fn app_origin_stays_in_webview() {
        assert!(webview_may_load(&parsed(
            "https://tracker.coolerboxbrothers.com/projects"
        )));
    }

    #[test]
    fn auth_hosts_stay_in_webview() {
        for raw in [
            "https://wlwdhorybvelwbmhtftw.supabase.co/auth/v1/authorize",
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://checkout.paystack.com/",
            "https://ims-na1.adobelogin.com/ims/authorize/v2",
            "https://auth.services.adobe.com/en_US/index.html",
            "https://adobeid-na1.services.adobe.com/ims/fromSusi",
            "https://challenges.cloudflare.com/turnstile/v0/api.js",
            "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/b/turnstile/f/av0/rch/ifstq/sitekey/light/fbE/new/flexible?lang=auto",
        ] {
            assert!(webview_may_load(&parsed(raw)), "{raw}");
        }
    }

    #[test]
    fn a_sign_in_follows_the_provider_until_back_on_the_tracker() {
        let mut handed_off = false;
        // Ruan's Frame.io link, 2026-10-06: both hops were thrown out to the
        // browser, which cannot finish them.
        for raw in [
            "https://tracker.coolerboxbrothers.com/settings/frameio",
            "https://ims-na1.adobelogin.com/ims/authorize/v2?client_id=x",
            "https://auth.services.adobe.com/en_US/index.html",
            "https://adobeid-na1.services.adobe.com/ims/fromSusi",
            "https://sso.behance.net/ims/cdsc_redirect/abc",
            "https://tracker.coolerboxbrothers.com/api/integrations/frameio/callback?code=c",
        ] {
            assert!(main_window_may_load(&parsed(raw), &mut handed_off), "{raw}");
        }
        assert!(!handed_off, "back on the tracker ends the hand-off");
        assert!(!main_window_may_load(&parsed("https://sso.behance.net/x"), &mut handed_off));
    }

    #[test]
    fn foreign_pages_leave_the_app_when_not_handed_off() {
        let mut handed_off = false;
        for raw in [
            "https://tracker.coolerboxbrothers.com/projects",
            "https://evil.example/phish",
            "http://accounts.google.com/",
        ] {
            main_window_may_load(&parsed(raw), &mut handed_off);
        }
        assert!(!handed_off);
        assert!(!main_window_may_load(&parsed("https://evil.example/phish"), &mut handed_off));
        // Even mid-sign-in, nothing but https.
        main_window_may_load(&parsed("https://accounts.google.com/o/oauth2/v2/auth"), &mut handed_off);
        assert!(!main_window_may_load(&parsed("http://example.com/"), &mut handed_off));
        assert!(main_window_may_load(&parsed("https://accounts.youtube.com/accounts/SetSID"), &mut handed_off));
    }

    #[test]
    fn foreign_https_and_plain_http_leave_the_app() {
        for raw in [
            "https://coolerboxbrothers.com/",
            "https://evil.example/phish",
            "http://tracker.coolerboxbrothers.com/",
        ] {
            assert!(!webview_may_load(&parsed(raw)), "{raw}");
        }
    }

    #[test]
    fn deep_link_maps_known_segments() {
        for (raw, expected) in [
            (
                "tracker://auth/callback?code=abc&next=/",
                "https://tracker.coolerboxbrothers.com/auth/callback?code=abc&next=/",
            ),
            (
                "tracker://view/some-token",
                "https://tracker.coolerboxbrothers.com/view/some-token",
            ),
            (
                "tracker://quote/abc123",
                "https://tracker.coolerboxbrothers.com/quote/abc123",
            ),
        ] {
            assert_eq!(
                hosted_url_for_deep_link(&parsed(raw)).map(|u| u.to_string()),
                Some(expected.to_string()),
                "{raw}"
            );
        }
    }

    #[test]
    fn deep_link_rejects_anything_unknown() {
        for raw in [
            "tracker://evil/pwn",
            "tracker:///view/no-host",
            "https://tracker.coolerboxbrothers.com/",
            "mailto:crew@example.com",
        ] {
            assert!(hosted_url_for_deep_link(&parsed(raw)).is_none(), "{raw}");
        }
    }

    #[test]
    fn session_cookie_probe() {
        assert!(has_session_cookie(
            "sb-wlwdhorybvelwbmhtftw-auth-token=eyJh; cbb_client=x"
        ));
        assert!(has_session_cookie(
            "sb-wlwdhorybvelwbmhtftw-auth-token.0=aaa; sb-wlwdhorybvelwbmhtftw-auth-token.1=bbb"
        ));
        for bare in ["", "cbb_client=x; theme=dark", "sb-other=value"] {
            assert!(!has_session_cookie(bare), "{bare}");
        }
    }

    #[test]
    fn new_windows_route_by_host() {
        assert_eq!(
            new_window_action(&parsed("https://tracker.coolerboxbrothers.com/api/coolerbox/abc")),
            NewWindowAction::SaveFile
        );
        assert_eq!(
            new_window_action(&parsed("https://tracker.coolerboxbrothers.com/api/media/k%2Fx.png")),
            NewWindowAction::SaveFile
        );
        assert_eq!(
            new_window_action(&parsed("https://tracker.coolerboxbrothers.com/api/attachments/xyz/download")),
            NewWindowAction::SaveFile
        );
        assert_eq!(
            new_window_action(&parsed("https://tracker.coolerboxbrothers.com/projects/1")),
            NewWindowAction::LoadInMain
        );
        assert_eq!(
            new_window_action(&parsed("https://files.example.com/signed?x=1")),
            NewWindowAction::OpenExternal
        );
        assert_eq!(
            new_window_action(&parsed("mailto:crew@example.com")),
            NewWindowAction::OpenExternal
        );
        assert_eq!(
            new_window_action(&parsed("webcal://tracker.coolerboxbrothers.com/api/calendar/abc.ics")),
            NewWindowAction::OpenExternal
        );
        assert_eq!(new_window_action(&parsed("javascript:alert(1)")), NewWindowAction::Ignore);
        assert_eq!(new_window_action(&parsed("file:///etc/passwd")), NewWindowAction::Ignore);
    }
}
