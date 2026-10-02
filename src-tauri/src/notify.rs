//! Live notifications: native banners + an unread badge, fed by the web
//! app's own `GET /api/notifications/recent` (the bell's endpoint).
//!
//! The shell holds no credentials. Every poll asks the *page* to fetch the
//! endpoint with its own session cookie, so token refresh and tenant scoping
//! stay the web app's job. The remote window gets no IPC: Rust triggers the
//! fetch with `eval` and collects the answer on the next tick through a
//! window variable, so a hidden (tray-only) window keeps working.
//!
//! The first successful poll after launch only seeds the "already seen" set,
//! so opening the app never replays a backlog of old unread items.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Deserialize;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;

const POLL_EVERY: Duration = Duration::from_secs(45);

/// More fresh items than this collapse into one summary banner.
const MAX_BANNERS: usize = 3;

/// Cap on remembered ids so a long-running session cannot grow without bound.
const MAX_SEEN: usize = 500;

/// Starts the fetch; the result lands in `window.__cbbNotif` ("null" on any
/// failure, e.g. signed out).
const START_JS: &str = r#"(function () {
  fetch('/api/notifications/recent', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.ok ? r.text() : 'null'; })
    .then(function (t) { window.__cbbNotif = t; })
    .catch(function () { window.__cbbNotif = 'null'; });
})()"#;

/// Returns and clears the last result ("" when none is waiting).
const TAKE_JS: &str =
    "(function () { var v = window.__cbbNotif || ''; window.__cbbNotif = ''; return v; })()";

#[derive(Debug, Deserialize)]
pub(crate) struct Item {
    id: serde_json::Value,
    body: String,
    unread: bool,
    #[serde(default)]
    label: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Poll {
    items: Vec<Item>,
    unread_count: u32,
}

#[derive(Default)]
pub(crate) struct State {
    seeded: bool,
    seen: HashSet<String>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Banner {
    pub title: String,
    pub body: String,
}

/// Parse the page's answer. `None` for "", "null" and anything malformed.
pub(crate) fn parse(raw: &str) -> Option<Poll> {
    serde_json::from_str(raw).ok()
}

/// Decide which banners a poll warrants and remember what was seen.
pub(crate) fn plan(state: &mut State, poll: &Poll) -> Vec<Banner> {
    let fresh: Vec<&Item> = poll
        .items
        .iter()
        .filter(|item| item.unread && !state.seen.contains(&item.id.to_string()))
        .collect();

    if state.seen.len() > MAX_SEEN {
        state.seen.clear();
    }
    for item in poll.items.iter().filter(|item| item.unread) {
        state.seen.insert(item.id.to_string());
    }

    if !state.seeded {
        state.seeded = true;
        return Vec::new();
    }
    if fresh.len() > MAX_BANNERS {
        return vec![Banner {
            title: "CoolerBox Tracker".into(),
            body: format!("{} new notifications", fresh.len()),
        }];
    }
    fresh
        .into_iter()
        .map(|item| Banner {
            title: if item.label.is_empty() {
                "CoolerBox Tracker".into()
            } else {
                item.label.clone()
            },
            body: item.body.clone(),
        })
        .collect()
}

fn apply<R: Runtime>(app: &AppHandle<R>, state: &Mutex<State>, poll: &Poll) {
    let banners = match state.lock() {
        Ok(mut state) => plan(&mut state, poll),
        Err(_) => return,
    };
    for banner in banners {
        let _ = app
            .notification()
            .builder()
            .title(banner.title)
            .body(banner.body)
            .show();
    }

    let count = poll.unread_count;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_badge_count(if count > 0 { Some(i64::from(count)) } else { None });
    }
    if let Some(tray) = app.tray_by_id(crate::TRAY_ID) {
        let tooltip = match count {
            0 => "CoolerBox Tracker".to_string(),
            n => format!("CoolerBox Tracker — {n} unread"),
        };
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

/// Start the background poller. Safe to leave running for the app's life; it
/// does nothing while the main window is off the tracker (e.g. mid sign-in).
pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    let state = Arc::new(Mutex::new(State::default()));
    std::thread::spawn(move || loop {
        std::thread::sleep(POLL_EVERY);
        let Some(window) = app.get_webview_window("main") else {
            continue;
        };
        let on_tracker = window
            .url()
            .map(|url| url.host_str() == Some(crate::APP_HOST))
            .unwrap_or(false);
        if !on_tracker {
            continue;
        }
        let app_for_cb = app.clone();
        let state_for_cb = state.clone();
        // Collect last tick's answer, then ask for a fresh one.
        let _ = window.eval_with_callback(TAKE_JS, move |raw| {
            let raw: String = serde_json::from_str(&raw).unwrap_or_default();
            if let Some(poll) = parse(&raw) {
                apply(&app_for_cb, &state_for_cb, &poll);
            }
        });
        let _ = window.eval(START_JS);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll(json: &str) -> Poll {
        parse(json).expect("test payload parses")
    }

    fn item(id: u32, unread: bool) -> String {
        format!(r#"{{"id":"n{id}","body":"body {id}","unread":{unread},"label":"Approval"}}"#)
    }

    fn payload(items: &[String], unread_count: u32) -> String {
        format!(r#"{{"items":[{}],"unreadCount":{unread_count}}}"#, items.join(","))
    }

    #[test]
    fn unusable_answers_are_ignored() {
        for raw in ["", "null", "{\"error\":\"x\"}", "<html>"] {
            assert!(parse(raw).is_none(), "{raw}");
        }
    }

    #[test]
    fn first_poll_seeds_without_banners() {
        let mut state = State::default();
        let p = poll(&payload(&[item(1, true), item(2, true)], 2));
        assert!(plan(&mut state, &p).is_empty());
        // Same items again: still nothing.
        assert!(plan(&mut state, &p).is_empty());
    }

    #[test]
    fn new_unread_item_gets_one_banner() {
        let mut state = State::default();
        plan(&mut state, &poll(&payload(&[item(1, true)], 1)));
        let banners = plan(&mut state, &poll(&payload(&[item(2, true), item(1, true)], 2)));
        assert_eq!(
            banners,
            vec![Banner {
                title: "Approval".into(),
                body: "body 2".into()
            }]
        );
    }

    #[test]
    fn read_items_never_banner() {
        let mut state = State::default();
        plan(&mut state, &poll(&payload(&[], 0)));
        assert!(plan(&mut state, &poll(&payload(&[item(3, false)], 0))).is_empty());
    }

    #[test]
    fn bursts_collapse_into_a_summary() {
        let mut state = State::default();
        plan(&mut state, &poll(&payload(&[], 0)));
        let items: Vec<String> = (1..=5).map(|i| item(i, true)).collect();
        let banners = plan(&mut state, &poll(&payload(&items, 5)));
        assert_eq!(banners.len(), 1);
        assert_eq!(banners[0].body, "5 new notifications");
    }
}
