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

mod analytics;
mod clock;
mod close;
mod desktop_entry;
mod diagnostics;
mod download;
mod google_sign_in;
mod idle;
mod location;
mod mini;
mod notify;
mod offline;
mod session;
mod system_open;
mod tabs;
mod title_buttons;
mod updater;

/// Tray icon id, so the poller can update its tooltip.
const TRAY_ID: &str = "main-tray";

/// Hosted production tracker. The only remote content the WebView loads.
const APP_ORIGIN: &str = "https://tracker.coolerboxbrothers.com";

/// Bare host of the tracker, for exact-match comparisons.
const APP_HOST: &str = "tracker.coolerboxbrothers.com";

/// Supabase Auth (project-ref host of the public anon URL, not a secret).
const SUPABASE_HOST: &str = "wlwdhorybvelwbmhtftw.supabase.co";

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
    SUPABASE_HOST,
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

/// Hosts the tracker embeds in an iframe: Frame.io's review player (the
/// tracker's own `frame-src` lists exactly these two; f.io short links are
/// resolved to them server-side first). They may load in the main window so
/// the embedded player plays, but a link that opens one in a new window
/// (the tracker's "Comment in Frame.io ↗") still goes to the system
/// browser: Frame.io's sign-in, needed to comment, fails when embedded.
const EMBED_HOSTS: &[&str] = &["app.frame.io", "next.frame.io"];

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
        // The website's own pages open in the browser (is_website_page).
        Some(APP_HOST) => {
            *handed_off = false;
            !is_website_page(url)
        }
        Some(host) if HAND_OFF_HOSTS.contains(&host) => {
            *handed_off = true;
            true
        }
        Some(host) if EMBED_HOSTS.contains(&host) => true,
        _ => *handed_off || webview_may_load(url),
    }
}

/// Schemes handed to the OS when the WebView may not load them itself.
/// `webcal` is My Schedule's "Open in Apple Calendar or Outlook" feed link.
/// Anything else (javascript:, file:, data:, …) is dropped.
const EXTERNAL_SCHEMES: &[&str] = &["https", "http", "mailto", "tel", "webcal"];

/// The website's own pages (marketing and legal): the app is the tracker, so
/// these open in the person's browser, never in a tracker window. Everything
/// else on the tracker host stays in the app, including the sign-in screens
/// (sign in / up, passwords, two-step, invitations) and the pages people open
/// from emails (quotes, invoices, viewings). app_only.js mirrors this list for
/// links the tracker's router follows without loading a page.
const WEBSITE_PATHS: &[&str] = &[
    "pricing", "watch", "privacy", "terms", "blog", "compare", "templates",
    "affiliates", "questions", "landing", "download", "cbb-casting", "dev-ui-kit", "unsubscribe",
];

/// Whether `url` is one of the website's own pages (see WEBSITE_PATHS).
pub(crate) fn is_website_page(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(APP_HOST)
        && url
            .path_segments()
            .and_then(|mut segments| segments.next())
            .is_some_and(|first| WEBSITE_PATHS.contains(&first))
}

/// True when a link the WebView will not load should open in the OS instead.
pub(crate) fn opens_externally(url: &url::Url) -> bool {
    EXTERNAL_SCHEMES.contains(&url.scheme())
}

/// What to do with a page-requested new window (`target="_blank"`,
/// `window.open`, "Open Link in New Window"). First-party files
/// (attachments, Cooler Box items) are saved to Downloads through the shell,
/// with the window's own session cookies — the tracker page itself never
/// navigates away. Other tracker pages open in a new tracker tab, as a
/// browser would; the sign-in hosts load in the window that asked (so the
/// session cookie lands in the app's jar); everything else goes to the
/// system browser.
#[derive(Debug, PartialEq)]
pub(crate) enum NewWindowAction {
    /// First-party file (attachment, Cooler Box item): open it from Downloads.
    SaveFile,
    /// A tracker page: a new tracker tab / window.
    NewTrackerWindow,
    /// A sign-in / billing host: load it in the window that asked.
    LoadInMain,
    OpenExternal,
    Ignore,
}

pub(crate) fn new_window_action(url: &url::Url) -> NewWindowAction {
    if download::is_file_url(url) {
        return NewWindowAction::SaveFile;
    }
    if is_website_page(url) {
        return NewWindowAction::OpenExternal;
    }
    if url.scheme() == "https" && url.host_str() == Some(APP_HOST) {
        return NewWindowAction::NewTrackerWindow;
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
    // Google sign-in coming back from the browser (google_sign_in.rs).
    if url.scheme() == "tracker" && url.host_str() == Some("signed-in") {
        google_sign_in::finish(app, url);
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
    session::has_session_cookie(cookies)
}

/// Bring the main window forward, with any tracker tabs / windows the quick
/// panel put away (mini.rs). Restores the dock icon on macOS (see the close
/// handler: a hidden app is a pure menu-bar app, like Toggl).
pub(crate) fn show_main<R: Runtime>(app: &AppHandle<R>) {
    mini::bring_tracker_back(app);
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// The tray's "Launch at login" row, kept so its tick follows the setting
/// when it is changed from the quick panel too.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
struct AutostartItem(MenuItem<tauri::Wry>);

/// Whether the app starts at login.
pub(crate) fn autostart_enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        use tauri_plugin_autostart::ManagerExt;
        app.autolaunch().is_enabled().unwrap_or(false)
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = app;
        false
    }
}

/// Turn launch at login on or off (tray menu or quick panel), tick the tray
/// row to match and confirm with a banner. Returns whether it is now on.
pub(crate) fn toggle_autostart<R: Runtime>(app: &AppHandle<R>) -> bool {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        use tauri_plugin_autostart::ManagerExt;
        use tauri_plugin_notification::NotificationExt;
        let manager = app.autolaunch();
        let turn_on = !manager.is_enabled().unwrap_or(false);
        let _ = if turn_on { manager.enable() } else { manager.disable() };
        let on = manager.is_enabled().unwrap_or(false);
        if let Some(item) = app.try_state::<AutostartItem>() {
            let _ = item.0.set_text(if on { "✓ Launch at login" } else { "Launch at login" });
        }
        let _ = app
            .notification()
            .builder()
            .title("CoolerBox Tracker")
            .body(if on { "Launch at login turned on." } else { "Launch at login turned off." })
            .show();
        on
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = app;
        false
    }
}

/// Hide a window; the app keeps running in the menu bar / tray. With the
/// last visible window gone the dock icon goes too (pure menu-bar app). It
/// returns in show_main / show_library, so there is never a dead dock icon.
pub(crate) fn hide_to_tray<R: Runtime>(window: &tauri::Window<R>) {
    let _ = window.hide();
    drop_dock_icon_if_alone(window.app_handle(), window.label());
}

/// macOS: with no window but `gone` showing (main, extra tracker windows,
/// the library, the panel), the app becomes a pure menu-bar app again.
#[allow(unused_variables)]
pub(crate) fn drop_dock_icon_if_alone<R: Runtime>(app: &AppHandle<R>, gone: &str) {
    #[cfg(target_os = "macos")]
    {
        let any_other = app
            .webview_windows()
            .iter()
            .filter(|(label, _)| label.as_str() != gone)
            .any(|(_, window)| window.is_visible().unwrap_or(false));
        if !any_other {
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
        }
    }
}

/// Bring the Saved-for-offline library forward.
pub(crate) fn show_library<R: Runtime>(app: &AppHandle<R>) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(library) = app.get_webview_window("library") {
        let _ = library.show();
        let _ = library.unminimize();
        let _ = library.set_focus();
    }
}

/// Tracker windows beyond the main one are labelled `tracker-<n>`.
const EXTRA_PREFIX: &str = "tracker-";

/// macOS tab group shared by every tracker window (native window tabs).
#[cfg(target_os = "macos")]
const TAB_GROUP: &str = "cbb-tracker";

/// Builds a tracker window: the main one at launch, or another one for a
/// second tab / window (`open_tracker_window`). Every tracker window gets the
/// same page helpers, navigation policy, downloads and location handling;
/// what a window asks for is answered in that window. The background work
/// (notification poll, clock, idle, analytics hand-over) stays on "main".
pub(crate) fn build_tracker_window<R: Runtime>(
    app: &AppHandle<R>,
    label: &str,
    start: url::Url,
) -> tauri::Result<tauri::WebviewWindow<R>> {
    let opener_handle = app.clone();
    let own_label = label.to_string();
    // Whether time away can be read from input here (not only from
    // sleep), so the tracker knows its "you were away" prompt is real.
    let idle_supported = idle::supported();
    let download_handle = app.clone();
    let handed_off = std::sync::Mutex::new(false);
    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::External(start))
    .title("CoolerBox Tracker")
    // Lets the tracker know it is inside the app (it hides "Get the
    // desktop app"), whether the shell reports time away for the time sheet
    // (idle.rs), and which app features its own buttons can ask for:
    // "new-tab" (cbb-window://open?u=<page>&tab=1) and "quick-panel"
    // (cbb-panel://open). Plain values on the page, not IPC.
    .initialization_script(&format!(
        "window.cbbDesktopApp=Object.freeze({{version:{:?},idleSupported:{},features:Object.freeze([\"new-tab\",\"quick-panel\"])}});",
        env!("CARGO_PKG_VERSION"),
        idle_supported
    ))
    // Download helpers (nav_bar.js): file links are intercepted in
    // the page and handed to the shell, which saves them with the
    // window's own session. No on-page buttons: the tracker page is
    // left exactly as the website made it.
    .initialization_script(include_str!("nav_bar.js"))
    // New tabs / windows (tracker_windows.js): Cmd/Ctrl+T, Cmd/Ctrl+N and
    // Cmd/Ctrl- or middle-click on a tracker link, as cbb-window:// hand-overs.
    .initialization_script(include_str!("tracker_windows.js"))
    // The app is the tracker, not the website (app_only.js): website pages
    // open in the browser, and signed out only the sign-in screens show.
    .initialization_script(include_str!("app_only.js"))
    // macOS: navigator.geolocation for the tracker's pages, answered
    // by Core Location through cbb-geo:// hand-overs (location.rs).
    .initialization_script(if cfg!(target_os = "macos") { include_str!("geo_bridge.js") } else { "" })
    // Location for the time sheet: the tracker's own pages only
    // (location.rs). Windows and Linux; macOS uses geo_bridge.js.
    .on_permission_request(|webview, kind| {
        location::decide(webview.url().ok().as_ref(), kind)
    })
    // Keep the tracker page running at full speed while the window is
    // closed to the menu bar. By default WebKit throttles a hidden
    // page's timers and may suspend it after ~5 minutes, so PostHog's
    // batched sends (analytics.rs), the notification poll and the
    // clock stalled until the window came back; found live: panel
    // events queued and were lost on sign-out. macOS 14+; Windows and
    // Linux don't support the setting and keep their own behaviour.
    .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
    .inner_size(1280.0, 800.0)
    .min_inner_size(1024.0, 640.0)
    // The main window is placed before it is shown (session.rs), so it never
    // opens at one size and jumps to another.
    .visible(label != "main")
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
                download::start_in(&opener_handle, &own_label, target, save_as);
            }
            return false;
        }
        // app_only.js: one of the website's own pages, for the browser.
        if url.scheme() == "cbb-browser" {
            if let Some(page) = url
                .query_pairs()
                .find(|(key, _)| key == "u")
                .and_then(|(_, value)| value.parse::<url::Url>().ok())
                .filter(is_website_page)
            {
                system_open::open_url(&opener_handle, page.as_str());
            }
            return false;
        }
        // tracker_windows.js: open a tracker page in a new tab / window.
        if url.scheme() == "cbb-window" {
            if let Some((target, tab)) = window_request(url) {
                open_tracker_window(&opener_handle, Some(&own_label), target, tab);
            }
            return false;
        }
        // geo_bridge.js (macOS) asks for the location as
        // cbb-geo://get?id=N; answered only on the tracker's pages.
        if url.scheme() == "cbb-geo" {
            #[cfg(target_os = "macos")]
            if let Some(id) = location::request_id(url) {
                let on_tracker = opener_handle
                    .get_webview_window(&own_label)
                    .and_then(|window| window.url().ok())
                    .is_some_and(|page| location::page_may_locate(&page));
                if on_tracker {
                    location::request(&opener_handle, &own_label, id);
                }
            }
            return false;
        }
        // "Show me" in the one-time quick-panel tip.
        if url.scheme() == "cbb-panel" {
            mini::show(&opener_handle);
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
        // Google refuses embedded sign-in: that step finishes in the
        // system browser and returns via tracker://signed-in.
        if google_sign_in::start(&opener_handle, url) {
            return false;
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
        let app = app.clone();
        let own_label = label.to_string();
        move |url, _features| {
            match new_window_action(&url) {
                NewWindowAction::SaveFile => download::start_in(&app, &own_label, url, false),
                // A tracker page asked for a new tab (target="_blank", or the
                // "Open Link in New Window" menu): a new tracker tab.
                NewWindowAction::NewTrackerWindow => {
                    open_tracker_window(&app, Some(&own_label), url, true);
                }
                NewWindowAction::LoadInMain => {
                    if let Some(window) = app.get_webview_window(&own_label) {
                        let _ = window.navigate(url);
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
        if page.host_str() != Some(APP_HOST) {
            return;
        }
        // Signed out, "/" is the website's homepage: show sign in instead
        // (signed in, it is the dashboard). Covers the first launch and
        // signing out; app_only.js covers the tracker's own page changes.
        if page.path() == "/" {
            let probe = window.clone();
            let _ = window.eval_with_callback("document.cookie", move |cookies_json| {
                let cookies: String = serde_json::from_str(&cookies_json).unwrap_or_default();
                if probe.label() == "main" {
                    session::update(probe.app_handle(), has_session_cookie(&cookies));
                }
                if !has_session_cookie(&cookies) {
                    if let Ok(sign_in) = START_URL.parse::<url::Url>() {
                        let _ = probe.navigate(sign_in);
                    }
                }
            });
            return;
        }
        // A signed-in page in the main window: time for the one-time
        // quick-panel tip, if it hasn't been shown yet.
        if page.path() != "/sign-in" {
            if window.label() == "main" {
                mini::maybe_introduce(window.app_handle());
            } else if is_extra_tracker_window(window.label()) {
                // A second tracker window: how to compare side by side, once.
                tabs::maybe_explain(window.app_handle(), window.label());
            }
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
            if probe.label() == "main" {
                session::update(probe.app_handle(), has_session_cookie(&cookies));
            }
            if has_session_cookie(&cookies) {
                if let Ok(home) = APP_ORIGIN.parse::<url::Url>() {
                    let _ = probe.navigate(home);
                }
            }
        });
    });
    #[cfg(target_os = "macos")]
    let builder = builder.tabbing_identifier(TAB_GROUP);
    builder.build()
}

/// The target of a `cbb-window://open?u=<tracker url>[&tab=1]` hand-over:
/// only https pages on the tracker itself.
pub(crate) fn window_request(url: &url::Url) -> Option<(url::Url, bool)> {
    if url.scheme() != "cbb-window" {
        return None;
    }
    let target = url
        .query_pairs()
        .find(|(key, _)| key == "u")
        .and_then(|(_, value)| value.parse::<url::Url>().ok())
        .filter(|target| target.scheme() == "https" && target.host_str() == Some(APP_HOST))?;
    let tab = url.query_pairs().any(|(key, value)| key == "tab" && value == "1");
    Some((target, tab))
}

/// Next free number for an extra tracker window's label.
static NEXT_WINDOW: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Opens a tracker page in another window, for comparing one review page
/// with another side by side. On macOS `tab` adds it as a native tab of the
/// window it came from (drag the tab out for side by side); elsewhere, and
/// for a new window, it is its own window. Runs on the main thread.
pub(crate) fn open_tracker_window<R: Runtime>(
    app: &AppHandle<R>,
    from: Option<&str>,
    url: url::Url,
    tab: bool,
) {
    let app = app.clone();
    let from = from.map(str::to_string);
    let _ = app.clone().run_on_main_thread(move || {
        #[cfg(target_os = "macos")]
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        let n = NEXT_WINDOW.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let label = format!("{EXTRA_PREFIX}{n}");
        let Ok(window) = build_tracker_window(&app, &label, url) else { return };
        #[cfg(target_os = "macos")]
        {
            if tab {
                if let Some(parent) = from.as_deref().and_then(|label| app.get_webview_window(label)) {
                    add_as_tab(&parent, &window);
                }
            }
            tabs::show_tab_bar(&window);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (tab, &from);
        let _ = window.set_focus();
    });
}

/// macOS: put `child` in `parent`'s tab bar, right after it.
#[cfg(target_os = "macos")]
fn add_as_tab<R: Runtime>(parent: &tauri::WebviewWindow<R>, child: &tauri::WebviewWindow<R>) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    let (Ok(parent), Ok(child)) = (parent.ns_window(), child.ns_window()) else { return };
    let (parent, child) = (parent.cast::<AnyObject>(), child.cast::<AnyObject>());
    if parent.is_null() || child.is_null() {
        return;
    }
    /// NSWindowAbove.
    const ABOVE: isize = 1;
    // SAFETY: both are live NSWindows owned by Tauri, on the main thread.
    unsafe {
        let _: () = msg_send![parent, addTabbedWindow: child, ordered: ABOVE];
    }
}

/// Whether a window label is an extra tracker window (`tracker-<n>`).
pub(crate) fn is_extra_tracker_window(label: &str) -> bool {
    label.strip_prefix(EXTRA_PREFIX).is_some_and(|n| n.parse::<u32>().is_ok())
}

/// Modifiers of the two global shortcuts (I: clock, M: quick panel).
/// Cmd+Shift on macOS. Ctrl+Alt on Windows and Linux: Win+Shift+M is
/// Windows' own "restore minimised windows", and Ctrl+Shift+I would take
/// the browsers' developer tools from every other app.
#[cfg(desktop)]
fn shortcut_modifiers() -> tauri_plugin_global_shortcut::Modifiers {
    use tauri_plugin_global_shortcut::Modifiers;
    if cfg!(target_os = "macos") {
        Modifiers::SUPER | Modifiers::SHIFT
    } else {
        Modifiers::CONTROL | Modifiers::ALT
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's DMA-BUF renderer blanks the whole page to black for a
    // few frames at a time on Wayland and on Intel + NVIDIA laptops
    // (seen on WebKitGTK 2.52, GNOME). It must be off before GTK starts.
    // Only when unset, so anyone can still turn it back on with =0.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

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
                    use tauri_plugin_global_shortcut::{Code, ShortcutState};
                    if !matches!(event.state(), ShortcutState::Pressed) {
                        return;
                    }
                    // Cmd+Shift+I (Ctrl+Alt+I on Windows/Linux): smart
                    // clock toggle — in when out, wrap when in, back when
                    // on a break. Notice-gating stays in `clock::act`.
                    let toggle = tauri_plugin_global_shortcut::Shortcut::new(
                        Some(shortcut_modifiers()),
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
                        if let Some(label) = analytics::clock_label(action) {
                            analytics::clock_tapped(app, label, "shortcut");
                        }
                        return;
                    }
                    // Cmd+Shift+M (Ctrl+Alt+M on Windows/Linux): the quick panel.
                    let panel = tauri_plugin_global_shortcut::Shortcut::new(
                        Some(shortcut_modifiers()),
                        Code::KeyM,
                    );
                    if shortcut == &panel {
                        mini::toggle(app, "shortcut");
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
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                match window.label() {
                    // Hide to the menu bar / tray, or quit (close.rs).
                    "main" => {
                        api.prevent_close();
                        close::main_window_closed(window);
                    }
                    "mini" => {
                        api.prevent_close();
                        hide_to_tray(window);
                    }
                    // Extra tracker windows (tracker-<n>) really close.
                    _ => {}
                }
            }
            // macOS: keep the tab bar showing, also on a window whose tab was
            // just dragged out into its own window (tabs.rs).
            #[cfg(target_os = "macos")]
            if matches!(event, tauri::WindowEvent::Focused(true))
                && (window.label() == "main" || is_extra_tracker_window(window.label()))
            {
                if let Some(tracker) = window.app_handle().get_webview_window(window.label()) {
                    tabs::show_tab_bar(&tracker);
                }
            }
            if matches!(event, tauri::WindowEvent::Destroyed) && is_extra_tracker_window(window.label()) {
                drop_dock_icon_if_alone(window.app_handle(), window.label());
            }
            // Drop-down behaviour: the quick panel hides when it loses focus,
            // unless it is pinned as a floating timer.
            if window.label() == "mini"
                && matches!(event, tauri::WindowEvent::Focused(false))
                && !mini::is_pinned()
            {
                mini::hide(window.app_handle());
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
            mini::mini_menu,
            mini::mini_pin,
            mini::mini_compact,
            mini::mini_mood,
            mini::mini_mood_done
        ])
        .setup(|app| {
            #[cfg(target_os = "linux")]
            desktop_entry::keep_installed();
            #[cfg(target_os = "linux")]
            title_buttons::follow_host(app.handle());
            let main_window = build_tracker_window(
                app.handle(),
                "main",
                START_URL.parse().expect("START_URL is a valid URL"),
            )?;
            // Tab / window titles follow the page each window shows (tabs.rs).
            tabs::follow_titles(app.handle());
            // macOS: tabs you can see and click (tabs.rs): File ▸ New Tab /
            // New Window, the Window menu's tab commands, the tab bar and +.
            #[cfg(target_os = "macos")]
            {
                tabs::install_menu(app.handle())?;
                tabs::enable_plus_button(&main_window);
            }

            // Mini bar (mini.rs + mini.html): hidden until the tray toggle.
            // Frameless, transparent, always on top, out of Alt-Tab.
            // The quick panel (mini.rs): drawn edge to edge by mini.html, with
            // its own rounded card and shadow inside the transparent window.
            WebviewWindowBuilder::new(app, "mini", WebviewUrl::App("mini.html".into()))
                .title("CoolerBox Tracker")
                .inner_size(372.0, 576.0) // mini::FULL_SIZE
                .resizable(false)
                .shadow(false)
                .decorations(false)
                .transparent(true)
                .always_on_top(true)
                .skip_taskbar(true)
                .visible(false)
                .focused(false)
                .build()?;
            // macOS: Tauri turns window tabbing off app-wide whenever it
            // builds a window without a tab group (the panel, the library),
            // which hid the tracker's tab bar. Turn it back on now that every
            // startup window exists, keep those two out of tab groups (tabs.rs).
            #[cfg(target_os = "macos")]
            tabs::allow_tabs(app.handle());
            // Size and place the main window for whoever is here (the sign-in
            // window or the tracker as it was left), then show it (session.rs).
            session::place_main_at_launch(&main_window);
            // The tab bar, once the window is on screen (hidden when signed out).
            #[cfg(target_os = "macos")]
            tabs::show_tab_bar(&main_window);

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
            let tray_new_window =
                MenuItem::with_id(app, "tray-new-window", "New Window", true, None::<&str>)?;
            let tray_notifications =
                MenuItem::with_id(app, "tray-notifications", "Notifications", true, None::<&str>)?;
            let inbox_1 =
                MenuItem::with_id(app, "inbox-1", "No unread notifications", false, None::<&str>)?;
            let inbox_2 = MenuItem::with_id(app, "inbox-2", "—", false, None::<&str>)?;
            let inbox_3 = MenuItem::with_id(app, "inbox-3", "—", false, None::<&str>)?;
            let tray_offline =
                MenuItem::with_id(app, "tray-offline", "Saved for offline", true, None::<&str>)?;
            let tray_mini =
                MenuItem::with_id(app, "tray-mini", "Quick panel", true, None::<&str>)?;
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
            // Windows says "Exit" in tray menus, macOS and Linux "Quit".
            let quit_label = if cfg!(target_os = "windows") { "Exit" } else { "Quit" };
            let tray_quit = MenuItem::with_id(app, "tray-quit", quit_label, true, None::<&str>)?;
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
                    &clock_items.wrap, &separator, &tray_show, &tray_new_window, &tray_notifications, &inbox_1, &inbox_2, &inbox_3,
                    &separator2, &tray_offline, &tray_mini, &tray_update, &tray_diagnostics,
                    #[cfg(not(any(target_os = "android", target_os = "ios")))]
                    &tray_autostart,
                    &tray_quit,
                ],
            )?;
            app.manage(clock_items);
            // Rows that change with signing in (session.rs).
            app.manage(session::TrayRows {
                show: tray_show.clone(),
                mini: tray_mini.clone(),
                new_window: tray_new_window.clone(),
            });
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
                if autostart_enabled(app.handle()) {
                    let _ = tray_autostart.set_text("✓ Launch at login");
                }
                app.manage(AutostartItem(tray_autostart.clone()));
            }
            // Cmd+Shift+I (Ctrl+Alt+I on Windows/Linux) toggles the clock
            // from anywhere, Cmd+Shift+M (Ctrl+Alt+M) the quick panel.
            // Best-effort: macOS may need Accessibility
            // permission; failure just means no hotkey, the tray items keep
            // working.
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Shortcut};
                let toggle =
                    Shortcut::new(Some(shortcut_modifiers()), Code::KeyI);
                let _ = app.global_shortcut().register(toggle);
                let panel =
                    Shortcut::new(Some(shortcut_modifiers()), Code::KeyM);
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
                // Left-click opens the quick panel (on_tray_icon_event); the
                // menu is the right-click fallback. Linux reports no tray
                // clicks, so there the menu is all there is.
                .show_menu_on_left_click(false)
                .tooltip("CoolerBox Tracker")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    // Another tracker window, on the page the main one shows.
                    "tray-new-window" => tabs::open_from_front(app, false),
                    "tray-show" => {
                        show_main(app);
                        analytics::action(app, "open_tracker", "tray_menu");
                    }
                    "tray-notifications" => {
                        // Handed over before the page navigates away.
                        analytics::action(app, "see_all_notifications", "tray_menu");
                        if let (Some(window), Ok(url)) = (
                            app.get_webview_window("main"),
                            format!("{APP_ORIGIN}/notifications").parse::<url::Url>(),
                        ) {
                            let _ = window.navigate(url);
                        }
                        show_main(app);
                    }
                    "inbox-1" | "inbox-2" | "inbox-3" => {
                        let row = match event.id.as_ref() { "inbox-1" => 0, "inbox-2" => 1, _ => 2 };
                        analytics::action(app, "notification", "tray_menu");
                        notify::open_inbox(app, row);
                    }
                    "tray-offline" => {
                        show_library(app);
                        analytics::action(app, "saved_offline", "tray_menu");
                    }
                    "tray-mini" => mini::toggle(app, "tray_menu"),
                    "clock-in" | "clock-break" | "clock-back" | "clock-wrap" => {
                        let action = event.id.as_ref().trim_start_matches("clock-");
                        clock::act(app, action);
                        if let Some(label) = analytics::clock_label(action) {
                            analytics::clock_tapped(app, label, "tray_menu");
                        }
                    }
                    "tray-update" => {
                        updater::check_now(app.clone());
                        analytics::action(app, "check_updates", "tray_menu");
                    }
                    "tray-diagnostics" => {
                        diagnostics::show(app);
                        analytics::action(app, "diagnostics", "tray_menu");
                    }
                    "tray-autostart" => {
                        let on = toggle_autostart(app);
                        analytics::setting_changed(app, "launch_at_login", on);
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
                        mini::toggle(tray.app_handle(), "tray_icon");
                    }
                })
                .build(app)?;

            notify::start(app.handle().clone());
            idle::start(app.handle().clone());
            clock::start(app.handle().clone());
            updater::start(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Remember the tracker window's place for next time (session.rs).
            if let tauri::RunEvent::Exit = event {
                session::save(app);
            }
        });
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
            NewWindowAction::NewTrackerWindow
        );
        assert_eq!(
            new_window_action(&parsed("https://accounts.google.com/o/oauth2/v2/auth")),
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

    #[test]
    fn frame_io_player_loads_in_the_main_window() {
        for raw in [
            "https://next.frame.io/share/abc/view/def",
            "https://app.frame.io/reviews/abc123",
        ] {
            let mut handed_off = false;
            assert!(main_window_may_load(&parsed(raw), &mut handed_off), "{raw}");
            assert!(!handed_off, "{raw}");
        }
    }

    #[test]
    fn frame_io_in_a_new_window_opens_in_the_browser() {
        for raw in [
            "https://next.frame.io/share/abc/view/def",
            "https://app.frame.io/reviews/abc123",
            "https://f.io/aBc12345",
        ] {
            assert_eq!(new_window_action(&parsed(raw)), NewWindowAction::OpenExternal, "{raw}");
        }
    }

    #[test]
    fn frame_io_lookalikes_stay_out() {
        let mut handed_off = false;
        for raw in [
            "https://app.frame.io.attacker.example/x",
            "https://frame.io.evil.example/x",
            "http://next.frame.io/share/abc",
        ] {
            assert!(!main_window_may_load(&parsed(raw), &mut handed_off), "{raw}");
        }
    }

    #[test]
    fn window_hand_over_opens_only_tracker_pages() {
        let (url, tab) = window_request(&parsed(
            "cbb-window://open?u=https%3A%2F%2Ftracker.coolerboxbrothers.com%2Fviewing%2Fversion%2Fabc&tab=1",
        ))
        .expect("tracker page");
        assert_eq!(url.as_str(), "https://tracker.coolerboxbrothers.com/viewing/version/abc");
        assert!(tab);
        let (_, tab) = window_request(&parsed(
            "cbb-window://open?u=https%3A%2F%2Ftracker.coolerboxbrothers.com%2F",
        ))
        .expect("tracker page");
        assert!(!tab);
        for raw in [
            "cbb-window://open?u=https%3A%2F%2Fevil.example%2F",
            "cbb-window://open?u=http%3A%2F%2Ftracker.coolerboxbrothers.com%2F",
            "cbb-window://open",
            "cbb-download://open?u=https%3A%2F%2Ftracker.coolerboxbrothers.com%2F",
        ] {
            assert!(window_request(&parsed(raw)).is_none(), "{raw}");
        }
    }

    #[test]
    fn extra_window_labels() {
        assert!(is_extra_tracker_window("tracker-1"));
        assert!(is_extra_tracker_window("tracker-12"));
        for label in ["main", "mini", "library", "tracker-", "tracker-x"] {
            assert!(!is_extra_tracker_window(label), "{label}");
        }
    }

    #[test]
    fn website_pages_open_in_the_browser() {
        for raw in [
            "https://tracker.coolerboxbrothers.com/privacy",
            "https://tracker.coolerboxbrothers.com/terms",
            "https://tracker.coolerboxbrothers.com/pricing",
            "https://tracker.coolerboxbrothers.com/watch",
            "https://tracker.coolerboxbrothers.com/blog/some-post",
            "https://tracker.coolerboxbrothers.com/compare/toggl",
        ] {
            let url = parsed(raw);
            assert!(is_website_page(&url), "{raw}");
            let mut handed_off = false;
            assert!(!main_window_may_load(&url, &mut handed_off), "{raw}");
            assert!(opens_externally(&url), "{raw}");
            assert_eq!(new_window_action(&url), NewWindowAction::OpenExternal, "{raw}");
        }
    }

    #[test]
    fn sign_in_and_app_pages_stay_in_the_app() {
        for raw in [
            "https://tracker.coolerboxbrothers.com/",
            "https://tracker.coolerboxbrothers.com/sign-in",
            "https://tracker.coolerboxbrothers.com/sign-up",
            "https://tracker.coolerboxbrothers.com/forgot-password",
            "https://tracker.coolerboxbrothers.com/reset-password",
            "https://tracker.coolerboxbrothers.com/verify-2fa",
            "https://tracker.coolerboxbrothers.com/invite/abc",
            "https://tracker.coolerboxbrothers.com/quote/abc",
            "https://tracker.coolerboxbrothers.com/projects",
            "https://tracker.coolerboxbrothers.com/privacy-settings",
        ] {
            let url = parsed(raw);
            assert!(!is_website_page(&url), "{raw}");
            let mut handed_off = false;
            assert!(main_window_may_load(&url, &mut handed_off), "{raw}");
        }
    }
}
