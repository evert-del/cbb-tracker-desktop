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

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::{DownloadEvent, PageLoadEvent, WebviewWindowBuilder},
    AppHandle, Manager, Runtime, WebviewUrl,
};
use tauri_plugin_deep_link::DeepLinkExt;

mod clock;
mod desktop_entry;
mod idle;
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

/// Schemes handed to the OS when the WebView may not load them itself.
/// `webcal` is My Schedule's "Open in Apple Calendar or Outlook" feed link.
/// Anything else (javascript:, file:, data:, …) is dropped.
const EXTERNAL_SCHEMES: &[&str] = &["https", "http", "mailto", "tel", "webcal"];

/// True when a link the WebView will not load should open in the OS instead.
pub(crate) fn opens_externally(url: &url::Url) -> bool {
    EXTERNAL_SCHEMES.contains(&url.scheme())
}

/// What to do with a page-requested new window (`target="_blank"`,
/// `window.open`). First-party links open in a viewer window of our own
/// (`open_viewer`), the app's stand-in for a browser tab: it shares the
/// app's cookie jar, so attachment links like `/api/coolerbox/<id>` still
/// redirect to their file, and closing it leaves the tracker where it was.
/// Loading them in the main window instead replaced the tracker with the
/// file and left no way back (a PDF from the Coolerbox, a referral QR's SVG).
/// Everything else goes to the system browser.
#[derive(Debug, PartialEq)]
pub(crate) enum NewWindowAction {
    OpenViewer,
    OpenExternal,
    Ignore,
}

pub(crate) fn new_window_action(url: &url::Url) -> NewWindowAction {
    if webview_may_load(url) {
        return NewWindowAction::OpenViewer;
    }
    if opens_externally(url) {
        NewWindowAction::OpenExternal
    } else {
        NewWindowAction::Ignore
    }
}

/// Navigation policy shared by every window showing the hosted tracker.
fn allow_navigation<R: Runtime>(app: &AppHandle<R>, url: &url::Url) -> bool {
    // about:blank / about:srcdoc frames are created by widgets like
    // Turnstile and carry no network content.
    if url.scheme() == "about" || webview_may_load(url) {
        return true;
    }
    // `tracker://` URLs are delivered to route_deep_link by the deep-link
    // plugin; anything else foreign leaves the app.
    if opens_externally(url) {
        system_open::open_url(app, url.as_str());
    }
    false
}

/// Act on a page-requested new window from any of our windows.
fn route_new_window<R: Runtime>(app: &AppHandle<R>, url: url::Url) {
    match new_window_action(&url) {
        NewWindowAction::OpenViewer => {
            // Never build a window inside the WebView's own callback: on
            // Windows that deadlocks. Hand it to another thread.
            let app = app.clone();
            tauri::async_runtime::spawn(async move { open_viewer(&app, url) });
        }
        NewWindowAction::OpenExternal => system_open::open_url(app, url.as_str()),
        NewWindowAction::Ignore => {}
    }
}

/// Tell the user where a download landed, or finalize an offline capture.
fn finish_any_download<R: Runtime>(
    app: &AppHandle<R>,
    url: &url::Url,
    path: Option<std::path::PathBuf>,
    success: bool,
) {
    if offline::is_capture_url(app, url) {
        offline::finish_download(app, url, success);
    } else {
        offline::finish_regular_download(app, path, success);
    }
}

/// Window label for the `n`th viewer.
fn viewer_label(n: usize) -> String {
    format!("viewer-{n}")
}

static NEXT_VIEWER: AtomicUsize = AtomicUsize::new(1);

/// Open a first-party link in its own window, like a browser tab would.
/// Same navigation policy and download hook as the main window, plus the
/// Back / Download bar (nav_bar.js), but none of main's start-up work.
fn open_viewer<R: Runtime>(app: &AppHandle<R>, url: url::Url) {
    let label = viewer_label(NEXT_VIEWER.fetch_add(1, Ordering::Relaxed));
    // A link served as an attachment (audio, video, an SVG from the
    // Coolerbox) downloads instead of showing, which would leave this
    // window blank. Whether anything had finished loading is read when the
    // download is *requested*, because WebKitGTK reports the interrupted
    // load as finished after that. The bar's own Download, on a shown file,
    // leaves the window open.
    let shown = Arc::new(AtomicBool::new(false));
    let blank_download = Arc::new(AtomicBool::new(false));
    let nav_app = app.clone();
    let popup_app = app.clone();
    let download_app = app.clone();
    let shown_on_load = shown.clone();
    let own_label = label.clone();
    let built = WebviewWindowBuilder::new(app, label, WebviewUrl::External(url.clone()))
        .title("CoolerBox Tracker")
        .initialization_script(include_str!("nav_bar.js"))
        .inner_size(1100.0, 800.0)
        .min_inner_size(480.0, 360.0)
        .focused(true)
        .on_navigation(move |url| allow_navigation(&nav_app, url))
        .on_new_window(move |url, _features| {
            route_new_window(&popup_app, url);
            tauri::webview::NewWindowResponse::Deny
        })
        .on_page_load(move |_window, payload| {
            if matches!(payload.event(), PageLoadEvent::Finished) {
                shown_on_load.store(true, Ordering::Relaxed);
            }
        })
        .on_download(move |_webview, event| match event {
            DownloadEvent::Requested { url, destination } => {
                if !shown.load(Ordering::Relaxed) {
                    blank_download.store(true, Ordering::Relaxed);
                }
                offline::handle_download(&download_app, &url, destination)
            }
            DownloadEvent::Finished { url, path, success } => {
                finish_any_download(&download_app, &url, path, success);
                if blank_download.load(Ordering::Relaxed) {
                    if let Some(viewer) = download_app.get_webview_window(&own_label) {
                        let _ = viewer.close();
                    }
                }
                true
            }
            _ => true,
        })
        .build();
    // If a window cannot be made, fall back to the old behaviour rather
    // than dropping the click.
    if built.is_err() {
        if let Some(main) = app.get_webview_window("main") {
            let _ = main.navigate(url);
        }
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

/// Bring the main window forward.
pub(crate) fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Bring the Saved-for-offline library forward.
pub(crate) fn show_library<R: Runtime>(app: &AppHandle<R>) {
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
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    use tauri_plugin_global_shortcut::{Code, Modifiers, ShortcutState};
                    // Cmd+Shift+I (Ctrl+Shift+I on Windows/Linux): smart
                    // clock toggle — in when out, wrap when in, back when
                    // on a break. Notice-gating stays in `clock::act`.
                    let toggle = tauri_plugin_global_shortcut::Shortcut::new(
                        Some(Modifiers::SUPER | Modifiers::SHIFT),
                        Code::KeyI,
                    );
                    if shortcut == &toggle
                        && matches!(event.state(), ShortcutState::Pressed)
                    {
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
                    }
                })
                .build(),
        )
        // Closing the main window hides it; the app keeps running in the
        // tray/menu bar so notifications keep arriving. Quit is in the tray
        // menu (or Cmd+Q).
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
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
            offline::offline_storage_info
        ])
        .setup(|app| {
            #[cfg(target_os = "linux")]
            desktop_entry::keep_installed();
            #[cfg(target_os = "linux")]
            title_buttons::follow_host(app.handle());
            let nav_handle = app.handle().clone();
            // Whether time away can be read from input here (not only from
            // sleep), so the tracker knows its "you were away" prompt is real.
            let idle_supported = idle::supported();
            let download_handle = app.handle().clone();
            WebviewWindowBuilder::new(
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
            // Back / Download buttons (nav_bar.js): the app has no browser
            // chrome. Files open in a viewer window now, but Back still
            // helps on the tracker's own pages.
            .initialization_script(include_str!("nav_bar.js"))
            // Clock pill (clock_pill.js): the site only paints the trigger's
            // light background on :hover, leaving the elapsed text invisible
            // on the dark header. Force the hover look until the site fix
            // lands. Desktop-shell only; harmless afterwards.
            .initialization_script(include_str!("clock_pill.js"))
            .inner_size(1280.0, 800.0)
            .min_inner_size(1024.0, 640.0)
            .on_navigation(move |url| allow_navigation(&nav_handle, url))
            .on_new_window({
                let app = app.handle().clone();
                move |url, _features| {
                    route_new_window(&app, url);
                    tauri::webview::NewWindowResponse::Deny
                }
            })
            .on_download(move |_webview, event| match event {
                DownloadEvent::Requested { url, destination } => {
                    offline::handle_download(&download_handle, &url, destination)
                }
                DownloadEvent::Finished { url, path, success } => {
                    finish_any_download(&download_handle, &url, path, success);
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
            let tray_update =
                MenuItem::with_id(app, "tray-update", "Check for updates", true, None::<&str>)?;
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
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
                    &separator2, &tray_offline, &tray_update,
                    #[cfg(not(any(target_os = "android", target_os = "ios")))]
                    &tray_autostart,
                    &tray_quit,
                ],
            )?;
            app.manage(clock_items);
            app.manage(clock::Last::default());
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
            // Cmd+Shift+I toggles the clock from anywhere. Best-effort:
            // macOS may need Accessibility permission; failure just means
            // no hotkey, the tray items keep working.
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};
                let toggle =
                    Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyI);
                let _ = app.global_shortcut().register(toggle);
            }
            let _tray = TrayIconBuilder::with_id(TRAY_ID)
                .icon(app.default_window_icon().cloned().unwrap_or_else(|| {
                    tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))
                        .expect("bundled tray icon parses")
                }))
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
                    "clock-in" => clock::act(app, "in"),
                    "clock-break" => clock::act(app, "break"),
                    "clock-back" => clock::act(app, "back"),
                    "clock-wrap" => clock::act(app, "wrap"),
                    "tray-update" => updater::check_now(app.clone()),
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
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
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
            "https://challenges.cloudflare.com/turnstile/v0/api.js",
            "https://challenges.cloudflare.com/cdn-cgi/challenge-platform/h/b/turnstile/f/av0/rch/ifstq/sitekey/light/fbE/new/flexible?lang=auto",
        ] {
            assert!(webview_may_load(&parsed(raw)), "{raw}");
        }
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
    fn viewer_labels_are_distinct() {
        assert_eq!(viewer_label(1), "viewer-1");
        assert_ne!(viewer_label(1), viewer_label(2));
        assert_ne!(viewer_label(1), "main");
    }

    #[test]
    fn new_windows_route_by_host() {
        assert_eq!(
            new_window_action(&parsed("https://tracker.coolerboxbrothers.com/api/coolerbox/abc")),
            NewWindowAction::OpenViewer
        );
        assert_eq!(
            new_window_action(&parsed("https://tracker.coolerboxbrothers.com/api/media/k%2Fx.png")),
            NewWindowAction::OpenViewer
        );
        assert_eq!(
            new_window_action(&parsed("https://tracker.coolerboxbrothers.com/projects/1")),
            NewWindowAction::OpenViewer
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
