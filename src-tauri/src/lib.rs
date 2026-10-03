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
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::{DownloadEvent, PageLoadEvent, WebviewWindowBuilder},
    AppHandle, Manager, Runtime, WebviewUrl,
};
use tauri_plugin_deep_link::DeepLinkExt;

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
/// `window.open`). The shell never opens extra windows: first-party links
/// load in the main window (so the session cookie is sent and attachment
/// links like `/api/coolerbox/<id>` can redirect to their file), everything
/// else goes to the system browser.
#[derive(Debug, PartialEq)]
pub(crate) enum NewWindowAction {
    LoadInMain,
    OpenExternal,
    Ignore,
}

pub(crate) fn new_window_action(url: &url::Url) -> NewWindowAction {
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
            title_buttons::follow_host(app.handle());
            let opener_handle = app.handle().clone();
            let download_handle = app.handle().clone();
            WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External(START_URL.parse().expect("START_URL is a valid URL")),
            )
            .title("CoolerBox Tracker")
            .inner_size(1280.0, 800.0)
            .min_inner_size(1024.0, 640.0)
            .on_navigation(move |url| {
                // about:blank / about:srcdoc frames are created by widgets
                // like Turnstile and carry no network content.
                if url.scheme() == "about" || webview_may_load(url) {
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
            // Live message/approval toasts are deliberately not here: the
            // shell cannot see page state without web-side cooperation, so
            // screen-scraping the remote DOM is off the table. The tray menu
            // and the offline save/fail toasts below are the v1 surface.
            let tray_show =
                MenuItem::with_id(app, "tray-show", "Show Tracker", true, None::<&str>)?;
            let tray_notifications =
                MenuItem::with_id(app, "tray-notifications", "Notifications", true, None::<&str>)?;
            let tray_offline =
                MenuItem::with_id(app, "tray-offline", "Saved for offline", true, None::<&str>)?;
            let tray_quit = MenuItem::with_id(app, "tray-quit", "Quit", true, None::<&str>)?;
            let tray_menu = Menu::with_items(
                app,
                &[&tray_show, &tray_notifications, &tray_offline, &tray_quit],
            )?;
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
                    "tray-offline" => show_library(app),
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
    fn new_windows_route_by_host() {
        assert_eq!(
            new_window_action(&parsed("https://tracker.coolerboxbrothers.com/api/coolerbox/abc")),
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
