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
    webview::WebviewWindowBuilder,
    AppHandle, Manager, Runtime, WebviewUrl,
};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_opener::OpenerExt;

mod offline;

/// Hosted production tracker. The only remote content the WebView loads.
const APP_ORIGIN: &str = "https://tracker.coolerboxbrothers.com";

/// Hosts allowed to load inside the WebView: the app itself, Supabase Auth,
/// and the OAuth/billing providers' sign-in pages. Everything else opens in
/// the system browser. Deliberately explicit (allow-list, not suffix match).
const IN_APP_HOSTS: &[&str] = &[
    "tracker.coolerboxbrothers.com",
    // Supabase Auth (project-ref host of the public anon URL, not a secret).
    "wlwdhorybvelwbmhtftw.supabase.co",
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
        .plugin(tauri_plugin_opener::init())
        .manage(std::sync::Mutex::new(None::<offline::PendingCapture>))
        .invoke_handler(tauri::generate_handler![
            offline::offline_save_pdf,
            offline::offline_list,
            offline::offline_open,
            offline::offline_delete,
            offline::offline_storage_info
        ])
        .setup(|app| {
            let opener_handle = app.handle().clone();
            let download_handle = app.handle().clone();
            WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External(APP_ORIGIN.parse().expect("APP_ORIGIN is a valid URL")),
            )
            .title("CoolerBox Tracker")
            .inner_size(1280.0, 800.0)
            .min_inner_size(1024.0, 640.0)
            .on_navigation(move |url| {
                if webview_may_load(url) {
                    return true;
                }
                // `tracker://` URLs are delivered to route_deep_link by the
                // deep-link plugin; anything else foreign leaves the app.
                match url.scheme() {
                    "https" | "http" | "mailto" | "tel" => {
                        let _ = opener_handle.opener().open_url(url.as_str(), None::<&str>);
                    }
                    _ => {}
                }
                false
            })
            .on_download(move |_webview, event| {
                use tauri::webview::DownloadEvent;
                match event {
                    DownloadEvent::Requested { url, destination } => {
                        offline::handle_download(&download_handle, &url, destination)
                    }
                    DownloadEvent::Finished { url, success, .. } => {
                        offline::finish_download(&download_handle, &url, success);
                        true
                    }
                    _ => true,
                }
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
            let tray_offline =
                MenuItem::with_id(app, "tray-offline", "Saved for offline", true, None::<&str>)?;
            let tray_quit = MenuItem::with_id(app, "tray-quit", "Quit", true, None::<&str>)?;
            let tray_menu = Menu::with_items(app, &[&tray_show, &tray_offline, &tray_quit])?;
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().unwrap_or_else(|| {
                    tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))
                        .expect("bundled tray icon parses")
                }))
                .menu(&tray_menu)
                .tooltip("CoolerBox Tracker")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "tray-show" => show_main(app),
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
}
