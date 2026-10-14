//! "Still to confirm" in the quick panel, for people who bill by the hour:
//! the same list as My Hours on the website (`GET
//! /api/me/project-time-suggestions`), so time they seem to have worked but
//! didn't log isn't forgotten: their bookings, days of task work, time cut
//! off while they were away, and what "Suggest time from my work" noticed
//! on another computer.
//!
//! - Each row: "Log it" (as it stands, when it has a project and times),
//!   "Fill in…" (opens it on My Hours, where it can be changed), "Not this".
//!   Nothing is ever logged without that tap.
//! - What this computer's own helper is still asking about (detector.rs)
//!   isn't listed twice.
//! - Once a morning (from 08:00 in Johannesburg, the tracker's day), with
//!   anything from before today waiting: one banner, "2 hours to confirm
//!   from before today", that opens the panel. Held back while quiet (Do not
//!   disturb), not used up. It names only a count.
//!
//! Read with the timer's poll (hourly.rs), at most every few minutes, and
//! afresh after an answer. Nothing of it is stored on the computer but the
//! day of the last reminder.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Runtime};

/// Read again after this long (the poll is every 90 s).
const READ_EVERY_SECS: u64 = 5 * 60;
/// The panel shows this many; the rest are on My Hours.
const SHOWN: usize = 5;
/// The day (yyyy-mm-dd) the morning reminder was last given.
const REMINDED: &str = "to_confirm_reminded";
/// Not before 08:00 in Johannesburg.
const REMIND_FROM_HOUR: u64 = 8;

const READ_JS: &str = r#"(function () {
  fetch('/api/me/project-time-suggestions', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.text().catch(function () { return ''; }).then(function (t) { return { s: r.status, b: t }; }); })
    .then(function (x) { window.__cbbToConfirm = JSON.stringify(x); })
    .catch(function () { window.__cbbToConfirm = JSON.stringify({ s: 0, b: '' }); });
})()"#;

const TAKE_JS: &str =
    "(function () { var v = window.__cbbToConfirm || ''; window.__cbbToConfirm = ''; return v; })()";

/// One thing to confirm, as the tracker sent it (checked).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Item {
    pub key: String,
    pub why: String,
    /// yyyy-mm-dd, the tracker's day.
    pub day: String,
    pub project_id: Option<String>,
    pub project_label: Option<String>,
    pub billable: bool,
    /// Unix seconds; both or neither.
    pub span: Option<(u64, u64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct List {
    pub today: String,
    pub items: Vec<Item>,
}

/// A key the tracker makes ("desktop:…", "away:…", "booking:…", "work:…").
pub(crate) fn valid_key(key: &str) -> bool {
    let Some((kind, rest)) = key.split_once(':') else { return false };
    matches!(kind, "desktop" | "away" | "booking" | "work")
        && (1..=90).contains(&rest.len())
        && rest.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-'))
}

fn valid_day(day: &str) -> bool {
    day.len() == 10 && day.chars().enumerate().all(|(i, ch)| if i == 4 || i == 7 { ch == '-' } else { ch.is_ascii_digit() })
}

/// What `GET /api/me/project-time-suggestions` says, or `None` for anything
/// else (not hourly, signed out, offline: keep what's known).
pub(crate) fn parse(raw: &str) -> Option<List> {
    #[derive(Deserialize)]
    struct Answer {
        s: u16,
        #[serde(default)]
        b: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Row {
        key: String,
        #[serde(default)]
        why: String,
        day: String,
        #[serde(default)]
        project_id: Option<String>,
        #[serde(default)]
        project_label: Option<String>,
        #[serde(default)]
        billable: bool,
        #[serde(default)]
        started_at: Option<String>,
        #[serde(default)]
        ended_at: Option<String>,
    }
    #[derive(Deserialize)]
    struct Body {
        today: String,
        suggestions: Vec<Row>,
    }
    let answer: Answer = serde_json::from_str(raw).ok()?;
    if answer.s != 200 {
        return None;
    }
    let body: Body = serde_json::from_str(&answer.b).ok()?;
    if !valid_day(&body.today) {
        return None;
    }
    let items = body
        .suggestions
        .into_iter()
        .filter(|row| valid_key(&row.key) && valid_day(&row.day))
        .map(|row| {
            let project_id = row.project_id.filter(|id| crate::hourly::valid_id(id));
            let span = match (row.started_at.as_deref().and_then(crate::clock::parse_iso), row.ended_at.as_deref().and_then(crate::clock::parse_iso)) {
                (Some(from), Some(to)) if to > from => Some((from, to)),
                _ => None,
            };
            Item {
                key: row.key,
                why: row.why.chars().take(120).collect(),
                day: row.day,
                project_label: project_id.as_ref().and(row.project_label).map(|label| label.chars().take(120).collect()),
                billable: project_id.is_some() && row.billable,
                project_id,
                span,
            }
        })
        .collect();
    Some(List { today: body.today, items })
}

/// The rows to show: less what this computer's helper is still asking about.
pub(crate) fn shown(list: &List, asking_here: &[u64]) -> Vec<Item> {
    list.items
        .iter()
        .filter(|item| !crate::detector::id_of(&item.key).is_some_and(|id| asking_here.contains(&id)))
        .cloned()
        .collect()
}

/// Whether to give the morning reminder now: from 08:00 on a day it wasn't
/// given yet, with something from before today waiting. `hour` is the
/// tracker's (Johannesburg) hour.
pub(crate) fn should_remind(items: &[Item], today: &str, hour: u64, reminded: Option<&str>) -> usize {
    if hour < REMIND_FROM_HOUR || reminded == Some(today) {
        return 0;
    }
    items.iter().filter(|item| item.day.as_str() < today).count()
}

pub(crate) fn reminder_text(count: usize) -> String {
    if count == 1 {
        "1 thing to confirm from before today".into()
    } else {
        format!("{count} things to confirm from before today")
    }
}

/// One row as the panel draws it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RowView {
    pub key: String,
    pub why: String,
    pub day: String,
    pub project: Option<String>,
    pub billable: bool,
    /// Milliseconds, for the panel's clock.
    pub from: Option<u64>,
    pub to: Option<u64>,
    /// "Log it" works as it stands.
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct View {
    pub rows: Vec<RowView>,
    pub total: usize,
    pub today: String,
}

#[derive(Default)]
struct Inner {
    list: Option<List>,
    read_at: u64,
}

static STATE: Mutex<Inner> = Mutex::new(Inner { list: None, read_at: 0 });

fn asking_here<R: Runtime>(app: &AppHandle<R>) -> Vec<u64> {
    crate::detector::pending_ids(app)
}

/// The panel's section, or `None` with nothing to confirm.
pub(crate) fn view<R: Runtime>(app: &AppHandle<R>) -> Option<View> {
    let list = STATE.lock().ok()?.list.clone()?;
    let rows = shown(&list, &asking_here(app));
    if rows.is_empty() {
        return None;
    }
    Some(View {
        total: rows.len(),
        today: list.today.clone(),
        rows: rows
            .iter()
            .take(SHOWN)
            .map(|item| RowView {
                key: item.key.clone(),
                why: item.why.clone(),
                day: item.day.clone(),
                project: item.project_label.clone(),
                billable: item.billable,
                from: item.span.map(|(from, _)| from * 1000),
                to: item.span.map(|(_, to)| to * 1000),
                ready: item.project_id.is_some() && item.span.is_some(),
            })
            .collect(),
    })
}

/// Forget everything (signed out, or no longer billing by the hour).
pub(crate) fn clear() {
    if let Ok(mut state) = STATE.lock() {
        *state = Inner::default();
    }
}

/// Read the list now (blocking: off the main thread), then remind if due.
pub(crate) fn read<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = crate::walkie::tracker_window(app) else { return };
    let _ = window.eval(READ_JS);
    let Some(raw) = crate::walkie::collect(&window, TAKE_JS, 30) else { return };
    let Some(list) = parse(&raw) else { return };
    let changed = STATE
        .lock()
        .map(|mut state| {
            state.read_at = crate::clock::unix_now();
            let changed = state.list.as_ref() != Some(&list);
            state.list = Some(list);
            changed
        })
        .unwrap_or(false);
    if changed {
        crate::mini::push(app);
    }
    maybe_remind(app);
}

/// Each timer poll (hourly.rs): read again when it's been a few minutes.
pub(crate) fn poll<R: Runtime>(app: &AppHandle<R>) {
    let now = crate::clock::unix_now();
    let stale = STATE.lock().map(|state| now.saturating_sub(state.read_at) >= READ_EVERY_SECS).unwrap_or(false);
    if stale {
        read(app);
    }
}

fn maybe_remind<R: Runtime>(app: &AppHandle<R>) {
    let Some(list) = STATE.lock().ok().and_then(|state| state.list.clone()) else { return };
    let rows = shown(&list, &asking_here(app));
    let hour = ((crate::clock::unix_now() + 2 * 3600) % 86_400) / 3600;
    let reminded = crate::close::strings(app, REMINDED).into_iter().next();
    let count = should_remind(&rows, &list.today, hour, reminded.as_deref());
    // Held back while quiet, not used up.
    if count == 0 || !crate::quiet::allowed(app, crate::quiet::Kind::Tracker) {
        return;
    }
    crate::close::set_strings(app, REMINDED, &[list.today.clone()]);
    crate::banner::show(
        app,
        &reminder_text(count),
        "Log them or put them away in the quick panel, so no work goes unbilled.",
        crate::banner::Target::Panel,
    );
}

/// "Log it" (as it stands) or "Not this" on a row in the panel.
#[tauri::command]
pub(crate) fn mini_to_confirm<R: Runtime>(app: AppHandle<R>, key: String, choice: String) -> Result<(), String> {
    let item = STATE
        .lock()
        .ok()
        .and_then(|state| state.list.as_ref().and_then(|list| list.items.iter().find(|item| item.key == key).cloned()))
        .ok_or("That's already been answered.")?;
    let body = match choice.as_str() {
        "log" => {
            let (Some(project_id), Some((from, to))) = (item.project_id.as_deref(), item.span) else {
                return Err("Fill this one in on My Hours.".into());
            };
            json!({ "confirm": {
                "key": item.key,
                "projectId": project_id,
                "startedAt": crate::hourly::iso_utc(from),
                "endedAt": crate::hourly::iso_utc(to),
            } })
        }
        "skip" => json!({ "skip": [item.key] }),
        _ => return Err("Unknown answer.".into()),
    };
    let app = app.clone();
    std::thread::spawn(move || {
        match crate::hourly::post_suggestions(&app, body) {
            Ok(_) => {
                if let Ok(mut state) = STATE.lock() {
                    if let Some(list) = state.list.as_mut() {
                        list.items.retain(|row| row.key != key);
                    }
                }
                // A desktop stretch answered here is answered for the helper too.
                if let Some(id) = crate::detector::id_of(&key) {
                    crate::detector::answered(&app, id);
                }
                crate::mini::push(&app);
                if choice == "log" {
                    crate::hourly::read_now(&app);
                }
                read(&app);
            }
            Err(error) => crate::hourly::show_error(&app, &error),
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: &str = "11111111-1111-4111-8111-111111111111";

    fn answer(body: serde_json::Value) -> String {
        json!({ "s": 200, "b": body.to_string() }).to_string()
    }

    #[test]
    fn reads_only_a_real_answer_and_checks_each_row() {
        let raw = answer(json!({ "today": "2026-10-14", "suggestions": [
            { "key": "booking:b1", "why": "You were booked on it", "day": "2026-10-12", "projectId": P1,
              "projectLabel": "Vimeo · Logo refresh", "billable": true,
              "startedAt": "2026-10-12T06:00:00.000Z", "endedAt": "2026-10-12T15:00:00.000Z" },
            { "key": format!("work:{P1}:2026-10-13"), "why": "You worked on its tasks that day", "day": "2026-10-13",
              "projectId": P1, "projectLabel": "Vimeo · Logo refresh", "billable": true, "startedAt": null, "endedAt": null },
            { "key": "away:x", "day": "2026-10-13", "projectId": "bad id';", "projectLabel": "Sneaky", "billable": true,
              "startedAt": "2026-10-13T09:00:00.000Z", "endedAt": "2026-10-13T09:30:00.000Z" },
            { "key": "other:1", "day": "2026-10-13" },
            { "key": "booking:b2", "day": "yesterday" },
        ] }));
        let list = parse(&raw).unwrap();
        assert_eq!(list.today, "2026-10-14");
        assert_eq!(list.items.len(), 3);
        let at = |iso: &str| crate::clock::parse_iso(iso).unwrap();
        assert_eq!(list.items[0].span, Some((at("2026-10-12T06:00:00.000Z"), at("2026-10-12T15:00:00.000Z"))));
        assert!(list.items[1].span.is_none());
        // A project id that doesn't read: no project, no label, not billable.
        assert_eq!((list.items[2].project_id.as_deref(), list.items[2].project_label.as_deref(), list.items[2].billable), (None, None, false));
        assert!(parse(&json!({ "s": 404, "b": "" }).to_string()).is_none());
        assert!(parse("garbage").is_none());
    }

    fn item(key: &str, day: &str) -> Item {
        Item { key: key.into(), why: String::new(), day: day.into(), project_id: None, project_label: None, billable: false, span: None }
    }

    #[test]
    fn what_the_helper_here_still_asks_about_isnt_listed_twice() {
        let list = List { today: "2026-10-14".into(), items: vec![item("desktop:100", "2026-10-13"), item("desktop:200", "2026-10-13"), item("booking:b", "2026-10-13")] };
        let keys: Vec<String> = shown(&list, &[100]).into_iter().map(|i| i.key).collect();
        assert_eq!(keys, vec!["desktop:200".to_string(), "booking:b".to_string()]);
    }

    #[test]
    fn the_morning_reminder_comes_once_from_eight_for_what_is_from_before_today() {
        let items = vec![item("booking:a", "2026-10-13"), item("booking:b", "2026-10-12"), item("away:c", "2026-10-14")];
        assert_eq!(should_remind(&items, "2026-10-14", 7, None), 0);
        assert_eq!(should_remind(&items, "2026-10-14", 8, None), 2);
        assert_eq!(should_remind(&items, "2026-10-14", 9, Some("2026-10-13")), 2);
        assert_eq!(should_remind(&items, "2026-10-14", 9, Some("2026-10-14")), 0);
        assert_eq!(should_remind(&[item("away:c", "2026-10-14")], "2026-10-14", 9, None), 0);
        assert_eq!(reminder_text(1), "1 thing to confirm from before today");
        assert_eq!(reminder_text(3), "3 things to confirm from before today");
    }

    #[test]
    fn keys_are_only_the_trackers_own() {
        assert!(valid_key("desktop:1760000000"));
        assert!(valid_key(&format!("work:{P1}:2026-10-12")));
        assert!(!valid_key("desktop:"));
        assert!(!valid_key("other:1"));
        assert!(!valid_key("booking:a'b"));
    }
}
