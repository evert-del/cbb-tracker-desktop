//! Desktop notifications you can click. Clicking one opens what it is about:
//! the tracker page of a notification, or the quick panel's chat for a walkie
//! message. On macOS a walkie banner also has a Reply field, so a message can
//! be answered from the banner itself.
//!
//! tauri-plugin-notification has no click callback on desktop, so banners go
//! through the crates it already uses underneath, which do report clicks:
//! - macOS: mac-notification-sys (`wait_for_click`, inline reply). Each
//!   banner waits for its answer on its own thread.
//! - Windows: tauri-winrt-notification (`on_activated`).
//! - Linux: notify-rust (`wait_for_action` on the "default" action).
//!
//! If a platform call fails, the banner is shown through the plugin as
//! before (without the click), so a banner is never lost.

use std::sync::atomic::{AtomicUsize, Ordering};

use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

/// What clicking a banner opens.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    /// A tracker page (a path starting with "/"), and the notification to
    /// mark read on the way, as the bell does.
    Page { path: String, id: Option<String> },
    /// A walkie conversation in the quick panel's chat.
    Walkie { key: String, kind: String, name: String },
    /// The quick panel (a summary of several walkie conversations).
    Panel,
    /// The Saved for offline window.
    Library,
}

/// What the person did with a banner.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Click {
    Open,
    /// macOS: typed into the banner's Reply field.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Reply(String),
}

/// Banners still waiting for a click on their own thread (macOS, Linux).
/// Past this many, new banners are shown without waiting, so a day of
/// ignored banners can't pile up threads.
static WAITING: AtomicUsize = AtomicUsize::new(0);
const MAX_WAITING: usize = 24;

/// Show a banner; clicking it opens `target`.
pub(crate) fn show<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str, target: Target) {
    let reply = matches!(target, Target::Walkie { .. });
    let on_click = {
        let app = app.clone();
        let target = target.clone();
        Box::new(move |click: Click| {
            let app_for_main = app.clone();
            let _ = app.run_on_main_thread(move || act(&app_for_main, &target, click));
        }) as Box<dyn FnOnce(Click) + Send>
    };
    let fallback = {
        let app = app.clone();
        let (title, body) = (title.to_string(), body.to_string());
        Box::new(move || {
            let _ = app.notification().builder().title(title).body(body).show();
        }) as Box<dyn FnOnce() + Send>
    };
    let wait = WAITING.load(Ordering::Relaxed) < MAX_WAITING;
    platform::show(&app_id(app), title, body, reply && wait, wait, on_click, fallback);
}

/// The identity banners are shown under: the app's own once installed.
/// While developing (an unbundled build), the same stand-ins the plugin uses.
fn app_id<R: Runtime>(app: &AppHandle<R>) -> String {
    let identifier = app.config().identifier.clone();
    if cfg!(target_os = "macos") && tauri::is_dev() {
        return "com.apple.Terminal".into();
    }
    #[cfg(windows)]
    {
        let installed = tauri::utils::platform::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.display().to_string()))
            .is_some_and(|dir| {
                let sep = std::path::MAIN_SEPARATOR;
                !(dir.ends_with(&format!("{sep}target{sep}debug")) || dir.ends_with(&format!("{sep}target{sep}release")))
            });
        if !installed {
            return tauri_winrt_notification::Toast::POWERSHELL_APP_ID.into();
        }
    }
    identifier
}

/// Open what a banner was about (on the main thread).
fn act<R: Runtime>(app: &AppHandle<R>, target: &Target, click: Click) {
    match (target, click) {
        (Target::Walkie { key, .. }, Click::Reply(text)) => crate::walkie::reply_from_banner(app, key, &text),
        (Target::Page { path, id }, _) => crate::notify::open_item(app, Some(path.clone()), id.clone()),
        (Target::Walkie { key, kind, name }, Click::Open) => crate::mini::open_chat(app, key, kind, name),
        (Target::Panel, _) => crate::mini::show(app),
        (Target::Library, _) => crate::show_library(app),
    }
    crate::analytics::action(app, "banner_click", "banner");
}

/// Counts a waiting banner for as long as it waits.
struct Waiting;

impl Waiting {
    fn start() -> Self {
        WAITING.fetch_add(1, Ordering::Relaxed);
        Waiting
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITING.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Click, Waiting};
    use mac_notification_sys::{MainButton, Notification, NotificationResponse};

    pub(super) fn show(
        app_id: &str,
        title: &str,
        body: &str,
        reply: bool,
        wait: bool,
        on_click: Box<dyn FnOnce(Click) + Send>,
        fallback: Box<dyn FnOnce() + Send>,
    ) {
        // Once per run; later calls (also the plugin's) are no-ops.
        let _ = mac_notification_sys::set_application(app_id);
        let (title, body) = (title.to_string(), body.to_string());
        let _ = std::thread::Builder::new().name("banner".into()).spawn(move || {
            let _waiting = wait.then(Waiting::start);
            let mut banner = Notification::new();
            banner.title(&title).message(&body).wait_for_click(wait);
            if reply {
                banner.main_button(MainButton::Response("Reply"));
            }
            match banner.send() {
                Ok(NotificationResponse::Click | NotificationResponse::ActionButton(_)) => on_click(Click::Open),
                Ok(NotificationResponse::Reply(text)) => on_click(Click::Reply(text)),
                Ok(_) => {}
                Err(_) => fallback(),
            }
        });
    }
}

#[cfg(windows)]
mod platform {
    use super::Click;
    use std::sync::Mutex;
    use tauri_winrt_notification::Toast;

    pub(super) fn show(
        app_id: &str,
        title: &str,
        body: &str,
        _reply: bool,
        _wait: bool,
        on_click: Box<dyn FnOnce(Click) + Send>,
        fallback: Box<dyn FnOnce() + Send>,
    ) {
        // The toast calls back on a system thread when it is clicked.
        let on_click = Mutex::new(Some(on_click));
        let shown = Toast::new(app_id)
            .title(title)
            .text1(body)
            .on_activated(move |_| {
                if let Some(open) = on_click.lock().ok().and_then(|mut slot| slot.take()) {
                    open(Click::Open);
                }
                Ok(())
            })
            .show();
        if shown.is_err() {
            fallback();
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::{Click, Waiting};

    pub(super) fn show(
        _app_id: &str,
        title: &str,
        body: &str,
        _reply: bool,
        wait: bool,
        on_click: Box<dyn FnOnce(Click) + Send>,
        fallback: Box<dyn FnOnce() + Send>,
    ) {
        let (title, body) = (title.to_string(), body.to_string());
        let _ = std::thread::Builder::new().name("banner".into()).spawn(move || {
            let mut banner = notify_rust::Notification::new();
            banner.summary(&title).body(&body).appname("CoolerBox Tracker").auto_icon();
            if wait {
                // "default" is a click on the banner itself.
                banner.action("default", "Open");
            }
            match banner.show() {
                Ok(handle) if wait => {
                    let _waiting = Waiting::start();
                    handle.wait_for_action(|action| {
                        if action == "default" {
                            on_click(Click::Open);
                        }
                    });
                }
                Ok(_) => {}
                Err(_) => fallback(),
            }
        });
    }
}

/// Where a bell notification's banner leads: its own page when it has one
/// (a tracker path), else the notifications list.
pub(crate) fn page_target(href: Option<&str>, id: &str) -> Target {
    let path = href.filter(|href| href.starts_with('/')).unwrap_or("/notifications").to_string();
    let id = (!id.is_empty()).then(|| id.to_string());
    Target::Page { path, id }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bell_banner_opens_its_own_page_or_the_list() {
        assert_eq!(
            page_target(Some("/approvals/12"), "\"n1\""),
            Target::Page { path: "/approvals/12".into(), id: Some("\"n1\"".into()) }
        );
        assert_eq!(page_target(None, ""), Target::Page { path: "/notifications".into(), id: None });
        // Only tracker paths, never another site.
        assert_eq!(
            page_target(Some("https://evil.example/x"), "7"),
            Target::Page { path: "/notifications".into(), id: Some("7".into()) }
        );
    }

    #[test]
    fn waiting_banners_are_counted_while_they_wait() {
        let before = WAITING.load(Ordering::Relaxed);
        {
            let _one = Waiting::start();
            assert_eq!(WAITING.load(Ordering::Relaxed), before + 1);
        }
        assert_eq!(WAITING.load(Ordering::Relaxed), before);
    }
}
