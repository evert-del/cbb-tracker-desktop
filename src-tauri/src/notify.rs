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
    #[serde(default)]
    href: Option<String>,
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
    /// Latest unread items (max 3) for the tray inbox. Refreshed every poll,
    /// even when no banner is warranted, so the tray is always current.
    pub inbox: Vec<InboxEntry>,
}

/// Latest unread count, shared so the mini bar can show its pill.
pub(crate) struct Unread(pub Mutex<u32>);

/// Actionable snapshot for the mini panel: the newest unread items that
/// link somewhere (tapping one opens its exact page), plus the total.
/// Refreshed every poll alongside the tray inbox.
pub(crate) struct Snapshot {
    pub unread: Mutex<u32>,
    pub needs: Mutex<Vec<InboxEntry>>,
    /// Unread items by kind ("Phase", "Approval", …), most first.
    pub summary: Mutex<Vec<LabelCount>>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { unread: Mutex::new(0), needs: Mutex::new(Vec::new()), summary: Mutex::new(Vec::new()) }
    }
}

/// How many unread items of one kind the latest poll held.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct LabelCount {
    pub label: String,
    pub count: u32,
}

/// Unread items of the poll counted by label, most first (ties keep the
/// order they first appear in, newest first). Unlabelled items count as
/// "Update", as in the panel's rows. Covers only the items the poll
/// returned, so the panel shows any rest of `unread_count` as "more".
pub(crate) fn summarize(poll: &Poll) -> Vec<LabelCount> {
    let mut counts: Vec<LabelCount> = Vec::new();
    for item in poll.items.iter().filter(|item| item.unread) {
        let label = if item.label.is_empty() { "Update" } else { item.label.as_str() };
        match counts.iter_mut().find(|c| c.label == label) {
            Some(c) => c.count += 1,
            None => counts.push(LabelCount { label: label.into(), count: 1 }),
        }
    }
    counts.sort_by(|a, b| b.count.cmp(&a.count));
    counts
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InboxEntry {
    pub id: String,
    pub title: String,
    pub body: String,
    pub href: Option<String>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Banner {
    pub title: String,
    pub body: String,
    pub href: Option<String>,
    pub id: String,
}

/// Parse the page's answer. `None` for "", "null" and anything malformed.
pub(crate) fn parse(raw: &str) -> Option<Poll> {
    serde_json::from_str(raw).ok()
}

/// The tray icon's hover text: the unread count with its summary by kind
/// ("CoolerBox Tracker — 5 unread: Approval 3, Phase 2"). Windows' tray
/// guidance puts this summary in the tooltip; the quick panel shows the
/// same. At most three kinds, so it stays one short line.
pub(crate) fn tooltip_text(count: u32, summary: &[LabelCount]) -> String {
    if count == 0 {
        return "CoolerBox Tracker".into();
    }
    let kinds: Vec<String> =
        summary.iter().take(3).map(|c| format!("{} {}", c.label, c.count)).collect();
    if kinds.is_empty() {
        format!("CoolerBox Tracker — {count} unread")
    } else {
        format!("CoolerBox Tracker — {count} unread: {}", kinds.join(", "))
    }
}

/// Decide which banners a poll warrants and remember what was seen.
/// Always refreshes `state.inbox` with the 3 newest unread items so the
/// tray inbox stays current even when nothing new arrived.
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

    // Tray inbox: newest unread first, cap 3. `href` comes straight from
    // the web API (`linkFor` server-side), so no URL map is duplicated here.
    state.inbox = poll
        .items
        .iter()
        .filter(|item| item.unread)
        .take(3)
        .map(|item| InboxEntry {
            id: item.id.to_string(),
            title: if item.label.is_empty() {
                "CoolerBox Tracker".into()
            } else {
                item.label.clone()
            },
            body: item.body.clone(),
            href: item.href.clone().filter(|h| h.starts_with('/')),
        })
        .collect();

    if !state.seeded {
        state.seeded = true;
        return Vec::new();
    }
    if fresh.len() > MAX_BANNERS {
        return vec![Banner {
            title: "CoolerBox Tracker".into(),
            body: format!("{} new notifications", fresh.len()),
            href: None,
            id: String::new(),
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
            href: item.href.clone().filter(|h| h.starts_with('/')),
            id: item.id.to_string(),
        })
        .collect()
}

/// Marks one notification read through the page's own session, mirroring
/// the bell (`POST /api/notifications/read {id}`). Fire-and-forget.
const MARK_READ_JS: &str = r#"(function (id) {
  try {
    var num = Number(id); var payload = isNaN(num) ? id : num;
    fetch('/api/notifications/read', { method: 'POST', credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ id: payload }) })
      .catch(function () {});
  } catch (e) {}
})"#;

/// The tray inbox rows, kept so each poll can update them.
pub(crate) struct InboxItems<R: Runtime> {
    pub rows: [tauri::menu::MenuItem<R>; 3],
    /// Hrefs parallel to `rows`, for click handling. `None` = disabled row.
    pub hrefs: Mutex<[Option<String>; 3]>,
    /// Notification ids parallel to `rows`, for mark-read on open.
    pub ids: Mutex<[Option<String>; 3]>,
}

fn apply<R: Runtime>(app: &AppHandle<R>, state: &Mutex<State>, poll: &Poll) {
    let (banners, inbox) = match state.lock() {
        Ok(mut state) => (plan(&mut state, poll), state.inbox.clone()),
        Err(_) => return,
    };
    for banner in banners {
        let _ = app
            .notification()
            .builder()
            .title(banner.title)
            .body(banner.body)
            .show();
        // NOTE: tauri-plugin-notification v2 has no reliable banner-click
        // callback on macOS (notify-rust backend), so the click path is the
        // tray inbox below, not the banner itself.
    }

    // Refresh the tray inbox rows (label truncated, href stored for click).
    if let Some(items) = app.try_state::<InboxItems<R>>() {
        for (i, row) in items.rows.iter().enumerate() {
            if let Some(entry) = inbox.get(i) {
                let mut text = format!("{} — {}", entry.title, entry.body);
                // Tray rows get unwieldy past ~60 chars.
                if text.chars().count() > 60 {
                    text = format!("{}…", text.chars().take(59).collect::<String>());
                }
                let _ = row.set_text(text);
                let _ = row.set_enabled(true);
            } else {
                let _ = row.set_text(if i == 0 { "No unread notifications".to_string() } else { "—".to_string() });
                let _ = row.set_enabled(false);
            }
        }
        if let (Ok(mut hrefs), Ok(mut ids)) = (items.hrefs.lock(), items.ids.lock()) {
            for i in 0..3 {
                hrefs[i] = inbox.get(i).and_then(|e| e.href.clone());
                ids[i] = inbox.get(i).map(|e| e.id.clone());
            }
        }
    }

    let count = poll.unread_count;
    if let Some(unread) = app.try_state::<Unread>() {
        if let Ok(mut slot) = unread.0.lock() {
            *slot = count;
        }
    }
    if let Some(snap) = app.try_state::<Snapshot>() {
        if let Ok(mut summary) = snap.summary.lock() {
            *summary = summarize(&poll);
        }
        if let (Ok(mut unread), Ok(mut needs)) = (snap.unread.lock(), snap.needs.lock()) {
            *unread = count;
            *needs = poll
                .items
                .iter()
                .filter(|item| item.unread)
                .filter_map(|item| {
                    item.href.clone().filter(|h| h.starts_with('/')).map(|href| InboxEntry {
                        id: item.id.to_string(),
                        title: if item.label.is_empty() {
                            "Update".into()
                        } else {
                            item.label.clone()
                        },
                        body: item.body.clone(),
                        href: Some(href),
                    })
                })
                .take(3)
                .collect();
        }
    }
    crate::mini::push(app);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_badge_count(if count > 0 { Some(i64::from(count)) } else { None });
    }
    if let Some(tray) = app.tray_by_id(crate::TRAY_ID) {
        let _ = tray.set_tooltip(Some(tooltip_text(count, &summarize(&poll))));
    }
}

/// A tray-inbox click: navigate the main window to the item's `href`
/// (validated `https` + tracker host only), mark it read like the bell,
///
/// then bring the window forward. Unknown index or missing href falls
/// back to `/notifications`.
pub(crate) fn open_inbox<R: Runtime>(app: &AppHandle<R>, index: usize) {
    use tauri::Manager;
    let (href, id) = app
        .try_state::<InboxItems<R>>()
        .map(|items| {
            let href = items.hrefs.lock().ok().and_then(|h| h[index].clone());
            let id = items.ids.lock().ok().and_then(|v| v[index].clone());
            (href, id)
        })
        .unwrap_or((None, None));
    let path = href.filter(|h| h.starts_with('/')).unwrap_or_else(|| "/notifications".into());
    if let Some(window) = app.get_webview_window("main") {
        if let Ok(url) = format!("{}{}", crate::APP_ORIGIN, path).parse::<url::Url>() {
            if crate::webview_may_load(&url) {
                let _ = window.navigate(url);
            }
        }
        if let Some(id) = id {
            let script = format!("({MARK_READ_JS})({id:?})");
            let _ = window.eval(script.as_str());
        }
    }
    crate::show_main(app);
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
        format!(r#"{{"id":"n{id}","body":"body {id}","unread":{unread},"label":"Approval","href":"/approvals"}}"#)
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
                body: "body 2".into(),
                href: Some("/approvals".into()),
                id: "\"n2\"".into(),
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

    #[test]
    fn inbox_holds_three_newest_unread_with_hrefs() {
        let mut state = State::default();
        let items: Vec<String> = (1..=4).map(|i| item(i, true)).collect();
        plan(&mut state, &poll(&payload(&items, 4)));
        assert_eq!(state.inbox.len(), 3);
        assert_eq!(state.inbox[0].href.as_deref(), Some("/approvals"));
        assert_eq!(state.inbox[0].id, "\"n1\"");
    }

    #[test]
    fn evil_href_never_reaches_the_tray() {
        let mut state = State::default();
        let evil = r#"{"id":"x","body":"pwn","unread":true,"label":"Approval","href":"https://evil.example/phish"}"#;
        plan(&mut state, &poll(&payload(&[evil.to_string()], 1)));
        assert_eq!(state.inbox.len(), 1);
        assert_eq!(state.inbox[0].href, None);
    }

    #[test]
    fn summary_counts_unread_by_label_most_first() {
        let raw = r#"{"unreadCount":6,"items":[
            {"id":1,"body":"a","unread":true,"label":"Phase","href":"/a"},
            {"id":2,"body":"b","unread":true,"label":"Approval","href":"/b"},
            {"id":3,"body":"c","unread":true,"label":"Approval","href":"/c"},
            {"id":4,"body":"d","unread":false,"label":"Phase","href":"/d"},
            {"id":5,"body":"e","unread":true,"label":"","href":null},
            {"id":6,"body":"f","unread":true,"label":"Phase","href":"/f"},
            {"id":7,"body":"g","unread":true,"label":"Approval","href":"/g"}
        ]}"#;
        let poll = parse(raw).expect("poll parses");
        let summary: Vec<(String, u32)> =
            summarize(&poll).into_iter().map(|c| (c.label, c.count)).collect();
        assert_eq!(
            summary,
            [("Approval".to_string(), 3), ("Phase".to_string(), 2), ("Update".to_string(), 1)]
        );
    }

    #[test]
    fn tooltip_names_up_to_three_kinds() {
        let kind = |label: &str, count| LabelCount { label: label.into(), count };
        assert_eq!(tooltip_text(0, &[kind("Phase", 1)]), "CoolerBox Tracker");
        assert_eq!(tooltip_text(4, &[]), "CoolerBox Tracker — 4 unread");
        assert_eq!(
            tooltip_text(9, &[kind("Approval", 3), kind("Phase", 2), kind("Quote", 1), kind("Update", 1)]),
            "CoolerBox Tracker — 9 unread: Approval 3, Phase 2, Quote 1"
        );
    }
}
