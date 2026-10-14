//! The project timer, for the few people an admin set to bill by the hour.
//! Only they get it, and only for their own time. They can time any project
//! they're on; the project decides the billing (an hourly project is
//! "Billable", a quoted one "Tracked only"), so nobody picks it per entry.
//! Time can also start with no project: "Not yet filed", not billable until
//! it is filed.
//!
//! - Quick panel: the running timer (project, label, elapsed, Stop), or
//!   one-tap starts for the projects timed most recently, "Just start, file
//!   it later" and "Other…". Starting another project while one runs
//!   switches.
//! - Tray menu and (macOS) Dock menu: "Logo refresh · 1h 05m", Stop, Switch
//!   to the recent projects, Just start, Open My Hours (dock.rs). The menu
//!   bar title shows the elapsed time.
//! - An unfiled timer that ends here (stopped, switched, or stopped for
//!   idle time) asks right away "Which project was it for?" and files it
//!   (`POST /api/me/project-time/file`). "New project" opens Hours on the
//!   website, which can also add a client.
//! - Idle: after the person's chosen minutes without keyboard or mouse
//!   (10 by default) with a timer running, the panel asks on their return:
//!   keep it, drop the idle time and keep going, or drop it and stop. With
//!   no answer by 30 minutes into the idle time, the timer stops at the
//!   moment idling began, and they're told when they're back. The app only
//!   ever ends time at that moment; it never adds time. That stop says
//!   `away: true`, so My Hours asks about the time cut off ("Still to
//!   confirm"); nothing is logged unless they say so there.
//! - Called in on the time sheet with no timer for half an hour: one gentle
//!   "Working on something?" banner that opens the panel.
//! - A client's monthly retainer: the panel shows what's left ("Retainer ·
//!   2h 30m left", or "Retainer used · 1h over"), and once a month per
//!   project, when the running project's retainer drops under an hour, a
//!   banner says so.
//!
//! - Cmd+Shift+Y (Ctrl+Alt+Y) starts the last project or stops the timer,
//!   from anywhere, for those with a timer (it isn't taken from anyone else).
//! - "Add time…" logs a stretch after the fact (`POST /api/me/project-time`),
//!   and favourite projects are pinned ahead of the recent ones.
//!
//! - Smarter than a plain timer: the project the person is booked on today
//!   (My Schedule, schedule.rs) comes first in the one-tap starts, marked
//!   "Booked today"; and wrapping on the time sheet with a timer running
//!   asks "Stop the timer too?". It asks; it never decides.
//!
//! Everything is named "Client · Project" ("Vimeo · Logo refresh"), never
//! by the project code.
//!
//! The timer also starts and stops on the website (the board's ▶, the
//! header), so it is read every 90 s, on opening the panel and when the
//! tracker window comes forward.
//!
//! All of it reads the tracker's `GET /api/me/hourly`; while that answers
//! 404 (not shipped) or 403 (not hourly), everything here stays hidden and
//! quiet. As elsewhere the page asks with its own session (`eval`), the
//! answer is collected through a window variable, and the shell holds no
//! credentials. Nothing about the time is stored on the computer; only the
//! idle minutes setting is (`settings.json`).

use std::{sync::Mutex, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    AppHandle, Manager, Runtime,
};

/// Evert's brief: every 60–120 s.
const POLL_EVERY: Duration = Duration::from_secs(90);
/// The first read, once the tracker has had time to load after launch.
const FIRST_AFTER: Duration = Duration::from_secs(30);
/// Opening the panel reads again when the last read is older than this.
const FRESH_FOR_SECS: u64 = 5;

/// Minutes without input before the timer's idle time counts (Settings).
const IDLE_MINUTES: &str = "hourly_idle_minutes";
pub(crate) const IDLE_CHOICES: [u64; 4] = [5, 10, 15, 20];
const IDLE_DEFAULT: u64 = 10;
/// Unanswered this long into the idle time, the timer stops at its start.
pub(crate) const AUTO_STOP_SECS: u64 = 30 * 60;
/// Called in this long without a timer: one "Working on something?".
pub(crate) const NUDGE_AFTER_SECS: u64 = 30 * 60;

const NOTE_MAX: usize = 500;

/// Leaves `{ s: status, b: body }` on `window.__cbbHourly` (`s: 0` when the
/// tracker couldn't be reached), so a failed read isn't taken for "not
/// hourly" (see `read_answer`).
const START_JS: &str = r#"(function () {
  fetch('/api/me/hourly', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.text().catch(function () { return ''; }).then(function (t) { return { s: r.status, b: t }; }); })
    .then(function (x) { window.__cbbHourly = JSON.stringify(x); })
    .catch(function () { window.__cbbHourly = JSON.stringify({ s: 0, b: '' }); });
})()"#;

const TAKE_JS: &str =
    "(function () { var v = window.__cbbHourly || ''; window.__cbbHourly = ''; return v; })()";

/// Today's entries: leaves `{ s, b }` on `window.__cbbHourlyToday`.
const TODAY_JS: &str = r#"(function () {
  fetch('/api/me/project-time', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.text().catch(function () { return ''; }).then(function (t) { return { s: r.status, b: t }; }); })
    .then(function (x) { window.__cbbHourlyToday = JSON.stringify(x); })
    .catch(function () { window.__cbbHourlyToday = JSON.stringify({ s: 0, b: '' }); });
})()"#;

const TAKE_TODAY_JS: &str =
    "(function () { var v = window.__cbbHourlyToday || ''; window.__cbbHourlyToday = ''; return v; })()";

const TAKE_RESULT_JS: &str =
    "(function () { var v = window.__cbbHourlyResult || ''; window.__cbbHourlyResult = ''; return v; })()";

const TIMER_URL: &str = "/api/me/project-timer";
const FILE_URL: &str = "/api/me/project-time/file";
const TIME_URL: &str = "/api/me/project-time";
const SUGGESTIONS_URL: &str = "/api/me/project-time-suggestions";
/// In a step's URL: the id of the timer the step before started.
const RUNNING_ID: &str = "{running}";

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Project {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub project_number: String,
    #[serde(default)]
    pub client_name: Option<String>,
    /// An hourly project (billed); otherwise quoted (tracked only).
    #[serde(default)]
    pub billable: bool,
    /// Minutes left on the client's retainer this month (negative: over),
    /// `None` when the client has no retainer.
    #[serde(default)]
    pub retainer_minutes_left: Option<i64>,
    /// On a project billed partly by the hour (mixed): its deliverables, so
    /// the person can say which one their time was for. Empty elsewhere.
    #[serde(default)]
    pub deliverables: Vec<Deliverable>,
}

/// One of a mixed project's deliverables.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub(crate) struct Deliverable {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// Puts the deliverable the person picked on a request's body, when it is
/// one of that project's (the tracker checks again). `None`: none picked.
/// They pick the deliverable only; what is billable is never theirs.
pub(crate) fn with_deliverable(body: &mut Value, info: &Info, project_id: &str, deliverable: Option<&str>) -> Result<(), &'static str> {
    let Some(deliverable) = deliverable.filter(|id| !id.is_empty()) else { return Ok(()) };
    let ok = info.projects.iter().any(|project| project.id == project_id && project.deliverables.iter().any(|d| d.id == deliverable));
    if !ok {
        return Err("Choose one of this project's deliverables.");
    }
    body["deliverableId"] = Value::String(deliverable.to_string());
    Ok(())
}

/// "Vimeo · Logo refresh", or the project's name alone without a client.
pub(crate) fn project_label(project: &Project) -> String {
    let name = if project.name.is_empty() { &project.project_number } else { &project.name };
    match project.client_name.as_deref().map(str::trim).filter(|client| !client.is_empty()) {
        Some(client) => format!("{client} · {name}"),
        None => name.clone(),
    }
}

/// The running timer's label, from its own client and project ("Vimeo ·
/// Logo refresh"; the tracker fills them in even for a project no longer in
/// the list), or "Not yet filed".
pub(crate) fn running_label(info: Option<&Info>, running: &Running) -> String {
    let Some(project_id) = &running.project_id else { return "Not yet filed".into() };
    if let Some(name) = running.project_name.as_deref().map(str::trim).filter(|name| !name.is_empty()) {
        return match running.client_name.as_deref().map(str::trim).filter(|client| !client.is_empty()) {
            Some(client) => format!("{client} · {name}"),
            None => name.to_string(),
        };
    }
    info.and_then(|info| info.projects.iter().find(|project| &project.id == project_id))
        .map(project_label)
        .unwrap_or_else(|| "Not yet filed".into())
}

/// Minutes the running timer has run by `now`.
pub(crate) fn elapsed_minutes(running: &Running, now: u64) -> i64 {
    crate::clock::parse_iso(&running.started_at).map_or(0, |since| (now.saturating_sub(since) / 60) as i64)
}

/// The running project's retainer as it stands now: the tracker's figure
/// (finished entries only) less the running time when it's billable.
pub(crate) fn live_retainer(running: &Running, now: u64) -> Option<i64> {
    let left = running.retainer_minutes_left?;
    Some(if running.billable { left - elapsed_minutes(running, now) } else { left })
}

/// "2h 30m", "1h", "45m".
pub(crate) fn fmt_minutes(minutes: u64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// "Retainer · 2h 30m left", or "Retainer used · 1h over".
pub(crate) fn retainer_text(minutes_left: i64) -> String {
    if minutes_left >= 0 {
        format!("Retainer · {} left", fmt_minutes(minutes_left.unsigned_abs()))
    } else {
        format!("Retainer used · {} over", fmt_minutes(minutes_left.unsigned_abs()))
    }
}

/// The once-a-month "retainer is running out" banner for the running
/// project, when its live retainer is under an hour: the project's id, the
/// client's name and the minutes left.
pub(crate) fn retainer_low(info: &Info, now: u64) -> Option<(String, String, i64)> {
    let running = info.running.as_ref()?;
    let project_id = running.project_id.clone()?;
    let left = live_retainer(running, now)?;
    let client = running
        .client_name
        .as_deref()
        .or(running.project_name.as_deref())
        .map(str::trim)
        .filter(|client| !client.is_empty())?
        .to_string();
    (left < 60).then_some((project_id, client, left))
}

pub(crate) fn retainer_banner(client: &str, left: i64) -> String {
    if left > 0 {
        format!("{client}'s retainer has {} left this month.", fmt_minutes(left.unsigned_abs()))
    } else if left == 0 {
        format!("{client}'s retainer is used up this month. More time is billed extra.")
    } else {
        format!("{client}'s retainer is used up this month ({} over). More time is billed extra.", fmt_minutes(left.unsigned_abs()))
    }
}

/// The month in Johannesburg (UTC+2) as yyyymm, for "once a month".
pub(crate) fn sast_month(now: u64) -> u64 {
    let iso = iso_utc(now + 2 * 3600);
    iso[0..4].parse::<u64>().unwrap_or(0) * 100 + iso[5..7].parse::<u64>().unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Running {
    pub id: String,
    /// `None` for a timer not yet filed under a project.
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub billable: bool,
    /// The client's retainer this month from finished entries only (the
    /// running time isn't in it: see `live_retainer`).
    #[serde(default)]
    pub retainer_minutes_left: Option<i64>,
    /// The project's estimate, and the whole project's finished minutes
    /// against it (hours only), when it has one.
    #[serde(default)]
    pub estimate_minutes: Option<i64>,
    #[serde(default)]
    pub estimate_used_minutes: Option<i64>,
    pub started_at: String,
    #[serde(default)]
    pub note: Option<String>,
}

impl Running {
    pub(crate) fn unfiled(&self) -> bool {
        self.project_id.is_none()
    }

}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Info {
    /// Most recently timed first.
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub running: Option<Running>,
    /// The company's clients, for starting a project from the app. Empty
    /// for freelancers, who never start projects.
    #[serde(default)]
    pub clients: Vec<Client>,
}

/// A client a new project can be for.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub(crate) struct Client {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// What `GET /api/me/hourly` says, or `None` for anything else (404 while it
/// isn't shipped, 403 for someone who doesn't bill by the hour, signed out).
pub(crate) fn parse(raw: &str) -> Option<Info> {
    let value: Value = serde_json::from_str(raw).ok()?;
    // Only a real answer: it always has the projects list.
    value.get("projects")?.as_array()?;
    let mut info: Info = serde_json::from_value(value).ok()?;
    info.projects.retain(|project| valid_id(&project.id));
    for project in &mut info.projects {
        project.deliverables.retain(|d| valid_id(&d.id) && !d.name.trim().is_empty());
    }
    info.clients.retain(|client| valid_id(&client.id) && !client.name.trim().is_empty());
    info.running = info.running.filter(valid_running).map(unfiled_not_billable);
    Some(info)
}

fn valid_running(running: &Running) -> bool {
    valid_id(&running.id) && running.project_id.as_deref().is_none_or(valid_id)
}

/// Not yet filed is never billable, whatever an answer says.
fn unfiled_not_billable(mut running: Running) -> Running {
    if running.project_id.is_none() {
        running.billable = false;
        running.project_name = None;
        running.client_name = None;
        running.retainer_minutes_left = None;
        running.estimate_minutes = None;
        running.estimate_used_minutes = None;
    }
    running
}

/// What a read of `GET /api/me/hourly` means.
#[derive(Debug, PartialEq)]
pub(crate) enum Read {
    /// The person's timer.
    Hourly(Box<Info>),
    /// Not hourly (404 / 403), or signed out (401): hide everything.
    Off,
    /// Offline, a server error, or an answer that doesn't read: keep what
    /// is known (a pending question must not be lost to a blip).
    Keep,
}

pub(crate) fn read_answer(raw: &str) -> Read {
    #[derive(Deserialize)]
    struct Answer {
        s: u16,
        #[serde(default)]
        b: String,
    }
    let Ok(answer) = serde_json::from_str::<Answer>(raw) else { return Read::Keep };
    match answer.s {
        200 => parse(&answer.b).map_or(Read::Keep, |info| Read::Hourly(Box::new(info))),
        401 | 403 | 404 => Read::Off,
        _ => Read::Keep,
    }
}

/// One of today's entries (`GET /api/me/project-time`).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Entry {
    pub id: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub billable: bool,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// On an invoice: it can't be changed.
    #[serde(default)]
    pub billed: bool,
    /// The note shows on the client's Hours page and invoice.
    #[serde(default)]
    pub note_for_client: bool,
    /// The tracker sends a decimal (12.53): rounded to whole minutes.
    #[serde(default, deserialize_with = "whole_minutes")]
    pub minutes: u64,
}

/// Today's entries and totals, as the panel shows them. The tracker sends
/// `{ from, to, entries }`; the totals are added up here.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Today {
    #[serde(default, alias = "from")]
    pub day: String,
    #[serde(default)]
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub total_minutes: u64,
    #[serde(default)]
    pub billable_minutes: u64,
}

/// Reads `GET /api/me/project-time`'s answer (`{ s, b }` as START_JS
/// leaves it): the day, or `None` (not shipped yet, or unreadable: the
/// list just doesn't show).
/// A number of minutes, whole or not, as whole minutes (never negative).
fn whole_minutes<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = Option::<f64>::deserialize(deserializer)?.unwrap_or(0.0);
    Ok(if value.is_finite() && value > 0.0 { value.round() as u64 } else { 0 })
}

pub(crate) fn read_today(raw: &str) -> Option<Today> {
    #[derive(Deserialize)]
    struct Answer {
        s: u16,
        #[serde(default)]
        b: String,
    }
    let answer: Answer = serde_json::from_str(raw).ok()?;
    if answer.s != 200 {
        return None;
    }
    let value: Value = serde_json::from_str(&answer.b).ok()?;
    value.get("entries")?.as_array()?;
    let mut today: Today = serde_json::from_value(value).ok()?;
    today.entries.retain(|entry| valid_id(&entry.id) && entry.project_id.as_deref().is_none_or(valid_id));
    for entry in &mut today.entries {
        if entry.project_id.is_none() {
            entry.billable = false;
        }
    }
    today.total_minutes = today.entries.iter().map(|e| e.minutes).sum();
    today.billable_minutes = today.entries.iter().filter(|e| e.billable).map(|e| e.minutes).sum();
    Some(today)
}

/// An edit of one of today's entries: `patch` is `PATCH
/// /api/me/project-time/:id`'s body (times and note, only what changed);
/// `file_to` moves it to another project (`POST /api/me/project-time/file`).
#[derive(Debug, PartialEq)]
pub(crate) struct Change {
    pub patch: Option<Value>,
    pub file_to: Option<String>,
}

/// The panel's edit view's changes, checked as far as the app can. One
/// argument per field of the form.
#[allow(clippy::too_many_arguments)]
pub(crate) fn entry_change(
    entry: &Entry,
    started: Option<u64>,
    ended: Option<u64>,
    note: Option<&str>,
    project_id: Option<&str>,
    info: &Info,
    now: u64,
    for_client: Option<bool>,
) -> Result<Change, &'static str> {
    if entry.billed {
        return Err("These hours are on an invoice already.");
    }
    let mut body = json!({});
    let start_was = crate::clock::parse_iso(&entry.started_at);
    let end_was = entry.ended_at.as_deref().and_then(crate::clock::parse_iso);
    if let Some(started) = started.filter(|s| Some(*s) != start_was) {
        body["startedAt"] = Value::String(iso_utc(started));
    }
    if let Some(ended) = ended.filter(|e| Some(*e) != end_was) {
        if entry.ended_at.is_none() {
            return Err("Stop the timer to set when it ended.");
        }
        body["endedAt"] = Value::String(iso_utc(ended));
    }
    let start = started.or(start_was).unwrap_or(0);
    let end = ended.or(end_was);
    if end.is_some_and(|end| end <= start) {
        return Err("The end has to be after the start.");
    }
    if end.unwrap_or(start) > now + 60 {
        return Err("That's in the future.");
    }
    if end.is_some_and(|end| end - start > 24 * 3600) {
        return Err("Keep it to 24 hours or less.");
    }
    if let Some(note) = note {
        let note = clean_note(Some(note));
        if note != entry.note {
            body["note"] = note.map_or(Value::Null, Value::String);
        }
    }
    if let Some(for_client) = for_client.filter(|tick| *tick != entry.note_for_client) {
        body["noteForClient"] = Value::Bool(for_client);
    }
    let mut file_to = None;
    if let Some(project_id) = project_id.filter(|id| Some(*id) != entry.project_id.as_deref()) {
        if !info.projects.iter().any(|project| project.id == project_id) {
            return Err("That project isn't one of yours.");
        }
        file_to = Some(project_id.to_string());
    }
    let patch = body.as_object().is_some_and(|fields| !fields.is_empty()).then_some(body);
    if patch.is_none() && file_to.is_none() {
        return Err("Nothing changed.");
    }
    Ok(Change { patch, file_to })
}

/// The tracker's ids (uuid or cuid): nothing that could reach a script.
pub(crate) fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

/// Hours on the website, open on an unfiled stretch to file it under a new
/// project (it can add the client too).
pub(crate) fn new_project_href(entry_id: &str) -> Option<String> {
    valid_id(entry_id).then(|| format!("/hours?entry={entry_id}&panel=new-project"))
}

/// The one-tap starts: the person's pinned projects first, then the ones
/// timed most recently, leaving out the one running now. Four, or as many as
/// are pinned (up to six).
pub(crate) fn quick(info: &Info, pinned: &[String]) -> Vec<Project> {
    let running = info.running.as_ref().and_then(|running| running.project_id.as_deref());
    let open = |project: &&Project| Some(project.id.as_str()) != running;
    let pins: Vec<Project> = pinned
        .iter()
        .filter_map(|id| info.projects.iter().find(|project| &project.id == id))
        .filter(open)
        .cloned()
        .collect();
    let room = pins.len().clamp(4, PINS_MAX);
    let recent = info.projects.iter().filter(open).filter(|project| !pinned.contains(&project.id)).cloned();
    pins.clone().into_iter().chain(recent).take(room.max(pins.len())).collect()
}

/// Pinned projects (`settings.json`), at most six.
const PINNED: &str = "hourly_pinned";

/// The pins, read from settings once and kept (the menus ask every second).
static PINS: Mutex<Option<Vec<String>>> = Mutex::new(None);

fn pins<R: Runtime>(app: &AppHandle<R>) -> Vec<String> {
    let Ok(mut cached) = PINS.lock() else { return crate::close::strings(app, PINNED) };
    cached.get_or_insert_with(|| crate::close::strings(app, PINNED)).clone()
}
const PINS_MAX: usize = 6;

/// Pin or unpin `project_id`; returns the new list.
pub(crate) fn toggle_pin(pinned: &[String], project_id: &str, pin: bool) -> Vec<String> {
    let mut list: Vec<String> = pinned.iter().filter(|id| *id != project_id).cloned().collect();
    if pin {
        list.push(project_id.to_string());
    }
    list.truncate(PINS_MAX);
    list
}

/// A stretch logged after the fact (`POST /api/me/project-time`): one of
/// the person's projects, ended after it started, not in the future (a
/// minute of clock drift allowed) and at most 24 hours.
pub(crate) fn add_body(project_id: &str, started: u64, ended: u64, note: Option<&str>, info: &Info, now: u64) -> Result<Value, &'static str> {
    if !info.projects.iter().any(|project| project.id == project_id) {
        return Err("That project isn't one of yours.");
    }
    if ended <= started {
        return Err("The end has to be after the start.");
    }
    if ended > now + 60 {
        return Err("That's in the future.");
    }
    if ended - started > 24 * 3600 {
        return Err("Keep it to 24 hours or less.");
    }
    let mut body = json!({ "projectId": project_id, "startedAt": iso_utc(started), "endedAt": iso_utc(ended) });
    if let Some(note) = clean_note(note) {
        body["note"] = Value::String(note);
    }
    Ok(body)
}

/// What the panel shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PanelView {
    pub running: Option<Running>,
    pub quick: Vec<Project>,
    /// Every project they can time, for "Other…" and filing.
    pub projects: Vec<Project>,
    /// A question about idle time is waiting for an answer.
    pub asking: bool,
    /// An unfiled stretch is waiting to be filed.
    pub filing: bool,
    /// Settings: minutes without input before idle time counts.
    pub idle_minutes: u64,
    /// Pinned project ids, in order.
    pub pinned: Vec<String>,
    /// The global shortcut, as this system writes it ("⌘⇧Y", "Ctrl+Alt+Y").
    pub shortcut: String,
    /// The project they're booked on today (My Schedule), if they can time it.
    pub booked: Option<String>,
    /// "Suggest time from my work" (detector.rs): its settings and the
    /// suggestion waiting for an answer.
    pub detector: crate::detector::View,
    /// Today's entries and totals; `None` while the tracker doesn't have
    /// the list yet.
    pub today: Option<Today>,
    /// The company's clients, for "New project…" (empty: they can't start
    /// projects).
    pub clients: Vec<Client>,
    /// "Still to confirm" (to_confirm.rs), or `None` with nothing waiting.
    pub to_confirm: Option<crate::to_confirm::View>,
    /// "Not logged today": the blanks in the working day (day_gaps.rs).
    pub day_gaps: Option<crate::day_gaps::View>,
}

/// The project timer's project for today's booking: the booking is today
/// in Johannesburg, and its project number is one of theirs.
pub(crate) fn booked_project(info: &Info, booking_day: &str, booking_number: &str, now: u64) -> Option<String> {
    let today = &iso_utc(now + 2 * 3600)[..10];
    if booking_day != today || booking_number.is_empty() {
        return None;
    }
    info.projects.iter().find(|project| project.project_number == booking_number).map(|project| project.id.clone())
}

/// The one-tap starts with today's booked project first.
pub(crate) fn with_booked(mut quick: Vec<Project>, info: &Info, booked: Option<&str>) -> Vec<Project> {
    let Some(booked) = booked else { return quick };
    if info.running.as_ref().and_then(|running| running.project_id.as_deref()) == Some(booked) {
        return quick;
    }
    let Some(project) = info.projects.iter().find(|project| project.id == booked) else { return quick };
    let len = quick.len().max(1);
    quick.retain(|p| p.id != booked);
    quick.insert(0, project.clone());
    quick.truncate(len.clamp(4, PINS_MAX));
    quick
}

/// A note as it is sent: trimmed, one line, at most 500 characters (the
/// tracker's own limit).
pub(crate) fn clean_note(note: Option<&str>) -> Option<String> {
    let note: String = note?
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .trim()
        .chars()
        .take(NOTE_MAX)
        .collect();
    (!note.is_empty()).then_some(note)
}

/// The body for a start, built only from what the tracker takes: one of the
/// person's own projects, or none ("Just start, file it later").
pub(crate) fn start_body(project_id: Option<&str>, note: Option<&str>, info: &Info) -> Result<Value, &'static str> {
    let mut body = json!({ "action": "start" });
    if let Some(project_id) = project_id {
        if !info.projects.iter().any(|project| project.id == project_id) {
            return Err("That project isn't one of yours.");
        }
        body["projectId"] = Value::String(project_id.to_string());
    }
    if let Some(note) = clean_note(note) {
        body["note"] = Value::String(note);
    }
    Ok(body)
}

/// The requests for a start: the start, then marking its note for the
/// client when asked (and there is a note).
pub(crate) fn start_steps(project_id: Option<&str>, note: Option<&str>, for_client: bool, info: &Info) -> Result<Vec<Step>, &'static str> {
    let body = start_body(project_id, note, info)?;
    let has_note = body.get("note").is_some();
    let mut steps = vec![post(TIMER_URL, body)];
    if for_client && has_note {
        steps.push(("PATCH", format!("{TIME_URL}/{RUNNING_ID}"), json!({ "noteForClient": true })));
    }
    Ok(steps)
}

/// A stop now, or at `at` (unix seconds: the moment idling began).
pub(crate) fn stop_body(at: Option<u64>) -> Value {
    match at {
        Some(at) => json!({ "action": "stop", "at": iso_utc(at) }),
        None => json!({ "action": "stop" }),
    }
}

/// The stop when nobody answered while the person was away: at `at`, and
/// marked so the time cut off is asked about on My Hours.
pub(crate) fn away_stop_body(at: u64) -> Value {
    let mut body = stop_body(Some(at));
    body["away"] = Value::Bool(true);
    body
}

/// The body that files `entry_id` under one of the person's projects.
pub(crate) fn file_body(entry_id: &str, project_id: &str, info: &Info) -> Result<Value, &'static str> {
    if !valid_id(entry_id) {
        return Err("Unknown time.");
    }
    if !info.projects.iter().any(|project| project.id == project_id) {
        return Err("That project isn't one of yours.");
    }
    Ok(json!({ "entryId": entry_id, "projectId": project_id }))
}

/// One request: method, tracker path and JSON body (null: none).
pub(crate) type Step = (&'static str, String, Value);

pub(crate) fn post(url: &str, body: Value) -> Step {
    ("POST", url.to_string(), body)
}

/// Sends each step in turn through the page's own session, stopping at the
/// first refusal, and leaves `{ ok, running }` (the last answer's) or
/// `{ ok: false, error }` on `window.__cbbHourlyResult`. `done` names what
/// already happened when a later step is refused ("Stopped at 09:40, but…").
pub(crate) fn post_script(steps: &[Step], done: &str) -> String {
    let steps = Value::Array(
        steps.iter().map(|(method, url, body)| json!({ "method": method, "url": url, "body": body })).collect(),
    );
    let done = Value::String(done.to_string());
    let running = Value::String(RUNNING_ID.to_string());
    format!(
        r#"(function (steps, done) {{
  window.__cbbHourlyResult = '';
  function post(i, last, body) {{
    if (i >= steps.length) {{ window.__cbbHourlyResult = JSON.stringify({{ ok: true, running: last, body: body }}); return; }}
    // A step on the timer just started: its id, and it keeps that timer.
    var onRunning = steps[i].url.indexOf({running}) >= 0;
    var url = onRunning ? steps[i].url.replace({running}, last && last.id ? last.id : '') : steps[i].url;
    var init = {{ method: steps[i].method, credentials: 'same-origin' }};
    if (steps[i].body !== null) {{ init.headers = {{ 'Content-Type': 'application/json' }}; init.body = JSON.stringify(steps[i].body); }}
    fetch(url, init)
      .then(function (r) {{ return r.json().catch(function () {{ return {{}}; }}).then(function (b) {{ return {{ ok: r.ok, b: b || {{}} }}; }}); }})
      .then(function (x) {{
        if (!x.ok) {{
          var error = typeof x.b.error === 'string' && x.b.error ? x.b.error.slice(0, 300) : "That didn't save. Try again.";
          window.__cbbHourlyResult = JSON.stringify({{ ok: false, error: i > 0 && done ? done + ' ' + error : error }});
          return;
        }}
        post(i + 1, onRunning ? last : x.b.running || null, x.b);
      }})
      .catch(function () {{ window.__cbbHourlyResult = JSON.stringify({{ ok: false, error: 'You seem to be offline. Try again in a moment.' }}); }});
  }}
  post(0, null, null);
}})({steps}, {done})"#
    )
}

/// The result of a request, as the page left it.
#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct Outcome {
    pub ok: bool,
    #[serde(default)]
    pub running: Option<Running>,
    #[serde(default)]
    pub error: Option<String>,
    /// The last answer's body (a new project).
    #[serde(default)]
    pub body: Option<Value>,
}

/// `2026-10-12T09:40:00.000Z` for unix seconds (UTC).
pub(crate) fn iso_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

// ── idle time ──

/// What idle time means for the running timer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IdleCall {
    /// Nothing: short, no timer, or a timer started after idling began
    /// (from a phone, say: not this computer's to cut).
    Nothing,
    /// Ask on their return.
    Ask,
    /// 30 minutes in without an answer: stop it when idling began.
    Stop,
}

/// Idle since `since` (unix seconds) until `now`, with `running`.
pub(crate) fn idle_call(running: Option<&Running>, since: u64, now: u64, ask_after_secs: u64) -> IdleCall {
    let Some(started) = running.and_then(|running| crate::clock::parse_iso(&running.started_at)) else {
        return IdleCall::Nothing;
    };
    let idle = now.saturating_sub(since);
    if started > since {
        IdleCall::Nothing
    } else if idle >= AUTO_STOP_SECS {
        IdleCall::Stop
    } else if idle >= ask_after_secs {
        IdleCall::Ask
    } else {
        IdleCall::Nothing
    }
}

/// A question waiting in the panel: the timer that was running, and when
/// idling began and ended (unix seconds).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IdleAsk {
    pub running: Running,
    pub from: u64,
    pub to: u64,
}

/// The requests for an answer: "keep" sends nothing; "stop" stops the timer
/// when idling began; "resume" is the tracker's `restart`: it ends the timer
/// then and starts the same project (or an unfiled timer) with the same note
/// from now, in one transaction, so the idle time is the only gap. Time is
/// only ever ended at the idle moment, never added.
pub(crate) fn idle_steps(choice: &str, ask: &IdleAsk) -> Result<Vec<Value>, &'static str> {
    match choice {
        "keep" => Ok(Vec::new()),
        "stop" => Ok(vec![stop_body(Some(ask.from))]),
        "resume" => Ok(vec![json!({ "action": "restart", "at": iso_utc(ask.from) })]),
        _ => Err("Unknown choice."),
    }
}

/// The unfiled stretch that ending `running` at `at` leaves to be filed.
/// Ended at or before its start, the tracker drops it: nothing to file.
pub(crate) fn left_to_file(running: &Running, at: u64) -> Option<String> {
    let started = crate::clock::parse_iso(&running.started_at)?;
    (running.unfiled() && started < at).then(|| running.id.clone())
}

// ── "Working on something?" ──

/// Called in (not on a break) since `in_since`, the last time a timer was
/// seen running at `last_timer_at`, and the stretch already nudged
/// (`nudged_for`, the call in's start): nudge once a stretch, after half an
/// hour without a timer.
pub(crate) fn should_nudge(in_since: Option<u64>, last_timer_at: u64, nudged_for: u64, now: u64) -> bool {
    let Some(since) = in_since else { return false };
    nudged_for != since && now.saturating_sub(since.max(last_timer_at)) >= NUDGE_AFTER_SECS
}

// ── the tray and Dock menus ──

/// What the timer's menus show (the tray's "Project timer" submenu and the
/// macOS Dock menu).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct MenuModel {
    /// "Logo refresh · 1h 05m" while running, else "Project timer".
    pub title: String,
    /// The running project's retainer ("Retainer · 2h 30m left").
    pub retainer: Option<String>,
    pub running: bool,
    /// `(menu id, label)` for the recent projects: "Switch to" or "Start".
    pub projects: Vec<(String, String)>,
}

pub(crate) const MENU_STOP: &str = "hourly-stop";
pub(crate) const MENU_LATER: &str = "hourly-later";
pub(crate) const MENU_HOURS: &str = "hourly-hours";
pub(crate) const MENU_ADD: &str = "hourly-add";
const MENU_START: &str = "hourly-start-";

pub(crate) fn menu_model(info: &Info, pinned: &[String], today_minutes: Option<u64>, now: u64) -> MenuModel {
    let title = match &info.running {
        Some(running) => {
            let since = crate::clock::parse_iso(&running.started_at).unwrap_or(now);
            format!("{} · {}", running_label(Some(info), running), crate::clock::fmt_span(now.saturating_sub(since) / 60))
        }
        None => match today_minutes.filter(|minutes| *minutes > 0) {
            Some(minutes) => format!("Project timer · Today {}", fmt_minutes(minutes)),
            None => "Project timer".into(),
        },
    };
    let projects = quick(info, pinned)
        .into_iter()
        .map(|project| (format!("{MENU_START}{}", project.id), menu_label(&project)))
        .collect();
    let retainer = info.running.as_ref().and_then(|running| live_retainer(running, now)).map(retainer_text);
    MenuModel { title, retainer, running: info.running.is_some(), projects }
}

fn menu_label(project: &Project) -> String {
    let name: String = project_label(project).chars().take(60).collect();
    format!("{name} ({})", if project.billable { "Billable" } else { "Tracked only" })
}

/// The project a menu id starts, if it's one of those menu ids.
pub(crate) fn menu_project(id: &str) -> Option<&str> {
    id.strip_prefix(MENU_START).filter(|project| valid_id(project))
}

pub(crate) fn is_menu_id(id: &str) -> bool {
    matches!(id, MENU_STOP | MENU_LATER | MENU_HOURS | MENU_ADD) || menu_project(id).is_some()
}

/// The Dock menu's latest model (dock.rs builds the menu when asked).
static DOCK: Mutex<Option<MenuModel>> = Mutex::new(None);

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn dock_model() -> Option<MenuModel> {
    DOCK.lock().ok().and_then(|model| model.clone())
}

/// The tray menu's "Project timer" submenu, shown at the top of the tray
/// menu only for someone with a timer.
pub(crate) struct TrayMenu<R: Runtime> {
    menu: Menu<R>,
    sub: Submenu<R>,
    separator: PredefinedMenuItem<R>,
    shown: Mutex<Option<MenuModel>>,
}

impl<R: Runtime> TrayMenu<R> {
    pub(crate) fn new(app: &AppHandle<R>, menu: &Menu<R>) -> tauri::Result<Self> {
        Ok(Self {
            menu: menu.clone(),
            sub: Submenu::with_id(app, "hourly", "Project timer", true)?,
            separator: PredefinedMenuItem::separator(app)?,
            shown: Mutex::new(None),
        })
    }
}

/// Bring the tray and Dock menus up to date (cheap when nothing changed;
/// called every second by the menu-bar ticker in clock.rs).
pub(crate) fn refresh_menus<R: Runtime>(app: &AppHandle<R>) {
    let now = crate::clock::unix_now();
    let pinned = pins(app);
    let model = with(app, |inner| {
        let today = inner.today.as_ref().map(|today| today.total_minutes);
        inner.info.as_ref().map(|info| menu_model(info, &pinned, today, now))
    })
    .flatten();
    if let Ok(mut dock) = DOCK.lock() {
        dock.clone_from(&model);
    }
    let Some(tray) = app.try_state::<TrayMenu<R>>() else { return };
    // Never wait for it: the menu calls below wait for the main thread, and
    // a command on the main thread may be here too. A skipped refresh is
    // done by the next tick.
    let Ok(mut shown) = tray.shown.try_lock() else { return };
    if *shown == model {
        return;
    }
    match (&*shown, &model) {
        (Some(_), None) => {
            let _ = tray.menu.remove(&tray.sub);
            let _ = tray.menu.remove(&tray.separator);
        }
        (None, Some(_)) => {
            let _ = tray.menu.insert(&tray.sub, 0);
            let _ = tray.menu.insert(&tray.separator, 1);
        }
        _ => {}
    }
    if let Some(model) = &model {
        let _ = tray.sub.set_text(&model.title);
        // The items change only when the projects or the running state do,
        // not every minute with the title.
        let items_changed = shown
            .as_ref()
            .is_none_or(|old| old.running != model.running || old.projects != model.projects || old.retainer != model.retainer);
        if items_changed {
            let _ = fill_submenu(app, &tray.sub, model);
        }
    }
    *shown = model;
}

fn fill_submenu<R: Runtime>(app: &AppHandle<R>, sub: &Submenu<R>, model: &MenuModel) -> tauri::Result<()> {
    for item in sub.items()? {
        sub.remove(&item)?;
    }
    if let Some(retainer) = &model.retainer {
        sub.append(&MenuItem::with_id(app, "hourly-retainer", retainer, false, None::<&str>)?)?;
    }
    if model.running {
        sub.append(&MenuItem::with_id(app, MENU_STOP, "Stop", true, None::<&str>)?)?;
    }
    if !model.projects.is_empty() {
        let label = if model.running { "Switch to" } else { "Start" };
        let projects = Submenu::with_id(app, "hourly-projects", label, true)?;
        for (id, text) in &model.projects {
            projects.append(&MenuItem::with_id(app, id, text, true, None::<&str>)?)?;
        }
        sub.append(&projects)?;
    }
    sub.append(&MenuItem::with_id(app, MENU_LATER, "Just start, file it later", true, None::<&str>)?)?;
    sub.append(&MenuItem::with_id(app, MENU_ADD, "Add time…", true, None::<&str>)?)?;
    sub.append(&PredefinedMenuItem::separator(app)?)?;
    sub.append(&MenuItem::with_id(app, MENU_HOURS, "Open My Hours", true, None::<&str>)?)?;
    Ok(())
}

/// A click on one of the timer's tray or Dock items.
pub(crate) fn menu_action<R: Runtime>(app: &AppHandle<R>, id: &str) {
    let result = match id {
        MENU_STOP => stop(app, "menu", true),
        MENU_LATER => start(app, None, None, "menu", true),
        MENU_ADD => {
            crate::mini::ask_hourly(app, "__cbbMiniHourlyAdd", &json!({}));
            Ok(())
        }
        MENU_HOURS => {
            crate::mini::open_href(app, "/hours");
            crate::analytics::action(app, "hourly_open_hours", "menu");
            Ok(())
        }
        id => match menu_project(id) {
            Some(project) => start(app, Some(project), None, "menu", true),
            None => Ok(()),
        },
    };
    if let Err(error) = result {
        use tauri_plugin_notification::NotificationExt;
        let _ = app.notification().builder().title("Project timer").body(error).show();
    }
}

// ── state ──

#[derive(Default)]
struct Inner {
    info: Option<Info>,
    read_at: u64,
    ask: Option<IdleAsk>,
    /// An unfiled stretch that ended here, waiting for its project.
    to_file: Option<String>,
    /// Said when they're back: the timer stopped while they were idle.
    notice: Option<String>,
    /// The timer already stopped for idle time (its id), so it's done once.
    auto_stopped: Option<String>,
    last_timer_at: u64,
    nudged_for: u64,
    /// Today's entries (`GET /api/me/project-time`).
    today: Option<Today>,
    /// Taps made while offline, waiting to be sent (offline_timer.rs).
    offline: crate::offline_timer::Queue,
}

#[derive(Default)]
pub(crate) struct Hourly(Mutex<Inner>);

fn with<R: Runtime, T>(app: &AppHandle<R>, run: impl FnOnce(&mut Inner) -> T) -> Option<T> {
    let state = app.try_state::<Hourly>()?;
    let mut inner = state.0.lock().ok()?;
    Some(run(&mut inner))
}

pub(crate) fn idle_minutes<R: Runtime>(app: &AppHandle<R>) -> u64 {
    let minutes = crate::close::number(app, IDLE_MINUTES, IDLE_DEFAULT);
    if IDLE_CHOICES.contains(&minutes) {
        minutes
    } else {
        IDLE_DEFAULT
    }
}

/// Settings › "Ask about idle time after": 5, 10, 15 or 20 minutes.
pub(crate) fn set_idle_minutes<R: Runtime>(app: &AppHandle<R>, minutes: u64) -> bool {
    if !IDLE_CHOICES.contains(&minutes) {
        return false;
    }
    crate::close::set_number(app, IDLE_MINUTES, minutes);
    crate::mini::push(app);
    true
}

/// The panel's view, or `None` (hidden) for someone without a timer.
pub(crate) fn panel<R: Runtime>(app: &AppHandle<R>) -> Option<PanelView> {
    let idle_minutes = idle_minutes(app);
    let pinned = pins(app);
    let next = crate::schedule::next(app);
    let now = crate::clock::unix_now();
    with(app, |inner| {
        let info = inner.info.as_ref()?;
        let booked = next.as_ref().and_then(|next| booked_project(info, &next.day, &next.project_number, now));
        let detector = crate::detector::view(app, info);
        let to_confirm = crate::to_confirm::view(app);
        Some(PanelView {
            running: info.running.clone(),
            quick: with_booked(quick(info, &pinned), info, booked.as_deref()),
            booked,
            detector,
            today: inner.today.clone(),
            projects: info.projects.clone(),
            clients: info.clients.clone(),
            to_confirm,
            day_gaps: crate::day_gaps::view(app, inner.today.as_ref(), info.running.as_ref()),
            asking: inner.ask.is_some(),
            filing: inner.to_file.is_some(),
            idle_minutes,
            pinned: pinned.clone(),
            shortcut: shortcut_label(std::env::consts::OS).into(),
        })
    })
    .flatten()
}

/// When the running timer started (unix seconds), for the menu bar.
pub(crate) fn running_since<R: Runtime>(app: &AppHandle<R>) -> Option<u64> {
    with(app, |inner| {
        inner.info.as_ref()?.running.as_ref().and_then(|running| crate::clock::parse_iso(&running.started_at))
    })
    .flatten()
}

fn set_info<R: Runtime>(app: &AppHandle<R>, info: Option<Info>) {
    let now = crate::clock::unix_now();
    with(app, |inner| {
        if info.as_ref().is_some_and(|info| info.running.is_some()) {
            inner.last_timer_at = now;
        }
        // A question about a timer that has since stopped or changed is moot.
        let still = |ask: &IdleAsk| info.as_ref().and_then(|info| info.running.as_ref()).is_some_and(|r| r.id == ask.running.id);
        if inner.ask.as_ref().is_some_and(|ask| !still(ask)) {
            inner.ask = None;
        }
        if info.is_none() {
            inner.to_file = None;
            crate::to_confirm::clear();
        }
        inner.info = info;
        inner.read_at = now;
    });
    refresh_menus(app);
    crate::mini::push(app);
    maybe_warn_retainer(app);
    let on = with(app, |inner| inner.info.is_some()).unwrap_or(false);
    sync_shortcut(app, on);
    // Newly billing by the hour here: walk them through it (setup.rs).
    if on {
        crate::setup::maybe_offer(app);
    }
}

/// Cmd+Shift+Y on macOS, Ctrl+Alt+Y elsewhere (the app's other shortcuts'
/// keys, and free in the browsers and Adobe apps people use here).
pub(crate) fn shortcut() -> tauri_plugin_global_shortcut::Shortcut {
    tauri_plugin_global_shortcut::Shortcut::new(Some(crate::shortcut_modifiers()), tauri_plugin_global_shortcut::Code::KeyY)
}

pub(crate) fn shortcut_label(os: &str) -> &'static str {
    if os == "macos" {
        "⌘⇧Y"
    } else {
        "Ctrl+Alt+Y"
    }
}

static SHORTCUT_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The shortcut is registered only while the person has a timer, so it
/// isn't taken from other apps for anyone else.
fn sync_shortcut<R: Runtime>(app: &AppHandle<R>, on: bool) {
    use std::sync::atomic::Ordering;
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    if SHORTCUT_ON.load(Ordering::Relaxed) == on {
        return;
    }
    let done = if on {
        app.global_shortcut().register(shortcut()).is_ok()
    } else {
        app.global_shortcut().unregister(shortcut()).is_ok()
    };
    if done {
        SHORTCUT_ON.store(on, Ordering::Relaxed);
    }
}

/// The shortcut: stop the running timer, or start the last project timed
/// (an unfiled timer when there's none). A notification says which.
pub(crate) fn shortcut_pressed<R: Runtime>(app: &AppHandle<R>) {
    let Ok(info) = current(app) else { return };
    let result = match (&info.running, info.projects.first()) {
        (Some(_), _) => stop(app, "shortcut", true),
        (None, Some(project)) => start(app, Some(&project.id), None, "shortcut", true),
        (None, None) => start(app, None, None, "shortcut", true),
    };
    if let Err(error) = result {
        say(app, &error);
    }
}

/// A native notification about the timer (for the shortcut and menus,
/// where the panel isn't showing).
fn say<R: Runtime>(app: &AppHandle<R>, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title("Project timer").body(body).show();
}

fn panel_showing<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window("mini").is_some_and(|mini| mini.is_visible().unwrap_or(false))
}

/// Once a month per project: the running project's retainer is under an
/// hour (or used up).
fn maybe_warn_retainer<R: Runtime>(app: &AppHandle<R>) {
    let now = crate::clock::unix_now();
    let Some((project_id, body)) = with(app, |inner| {
        let (project_id, client, left) = retainer_low(inner.info.as_ref()?, now)?;
        Some((project_id, retainer_banner(&client, left)))
    })
    .flatten() else {
        return;
    };
    let key = format!("retainer_warned_{project_id}");
    let month = sast_month(crate::clock::unix_now());
    // Held back while quiet (do not disturb, muted): it's shown later, not
    // used up unseen.
    if crate::close::number(app, &key, 0) == month || !crate::quiet::allowed(app, crate::quiet::Kind::Tracker) {
        return;
    }
    crate::close::set_number(app, &key, month);
    {
        crate::banner::show(
            app,
            "Retainer",
            &body,
            crate::banner::Target::Page { path: "/hours".into(), id: None },
        );
        crate::analytics::action(app, "hourly_retainer_low", "banner");
    }
}

fn set_running<R: Runtime>(app: &AppHandle<R>, running: Option<Running>) {
    let now = crate::clock::unix_now();
    with(app, |inner| {
        // Started, switched or stopped just now: no nudge for a while.
        inner.last_timer_at = now;
        if let Some(info) = inner.info.as_mut() {
            if let Some(running) = &running {
                // The project moves to the front, as the tracker will say.
                if let Some(at) = info.projects.iter().position(|project| Some(&project.id) == running.project_id.as_ref()) {
                    let project = info.projects.remove(at);
                    info.projects.insert(0, project);
                }
            }
            info.running = running;
        }
        // A question about a timer that has just ended (or restarted) is
        // answered.
        let still = inner.info.as_ref().and_then(|info| info.running.as_ref()).map(|running| running.id.clone());
        if inner.ask.as_ref().is_some_and(|ask| Some(&ask.running.id) != still.as_ref()) {
            inner.ask = None;
        }
    });
    refresh_menus(app);
    crate::mini::push(app);
}

/// Read the timer from the tracker (blocking: call it off the main thread).
fn read<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = crate::walkie::tracker_window(app) else { return };
    let _ = window.eval(START_JS);
    let Some(raw) = crate::walkie::collect(&window, TAKE_JS, 30) else { return };
    match read_answer(&raw) {
        Read::Hourly(info) => {
            let queue = with(app, |inner| inner.offline.clone()).unwrap_or_default();
            if queue.is_empty() {
                set_info(app, Some(*info));
                read_entries(app);
            } else {
                send_offline(app, *info, &queue);
            }
        }
        Read::Off => set_info(app, None),
        Read::Keep => {}
    }
}

/// Online again with taps kept offline: send them in order (offline_timer.rs),
/// showing the offline timer meanwhile rather than the tracker's old one.
fn send_offline<R: Runtime>(app: &AppHandle<R>, mut info: Info, queue: &crate::offline_timer::Queue) {
    let server_started = info.running.as_ref().and_then(|running| crate::clock::parse_iso(&running.started_at));
    let steps = queue.steps(server_started, crate::clock::unix_now());
    info.running = offline_running(&info, queue);
    set_info(app, Some(info));
    if steps.is_empty() {
        with(app, |inner| inner.offline = crate::offline_timer::Queue::default());
        read(app);
        return;
    }
    let then = Then::Timer {
        file: None,
        notice: Some("Back online: the time you tracked offline is in the tracker.".into()),
        say: false,
        auto: None,
        replay: true,
    };
    if send(app, steps, "", then).is_err() {
        // No tracker window: tried on the next read.
    }
}

/// The offline timer as the panel and menus show it.
fn offline_running(info: &Info, queue: &crate::offline_timer::Queue) -> Option<Running> {
    let open = queue.running()?;
    let project = info.projects.iter().find(|project| project.id == open.project);
    Some(Running {
        id: crate::offline_timer::OFFLINE_ID.into(),
        project_id: Some(open.project.clone()),
        project_name: project.map(|project| project.name.clone()),
        client_name: project.and_then(|project| project.client_name.clone()),
        billable: project.is_some_and(|project| project.billable),
        retainer_minutes_left: None,
        estimate_minutes: None,
        estimate_used_minutes: None,
        started_at: iso_utc(open.start),
        note: Some("Offline: not sent yet".into()),
    })
}

/// A timer tap that couldn't reach the tracker: kept to send later, and the
/// panel goes on as if it had.
fn keep_offline<R: Runtime>(app: &AppHandle<R>, sent: &[Step]) -> Result<(), String> {
    use crate::offline_timer::{Tap, OFFLINE_ID};
    let now = crate::clock::unix_now();
    let Some(tap) = sent.iter().filter(|(_, url, _)| url == TIMER_URL).find_map(|(_, _, body)| crate::offline_timer::tap_of(body)) else {
        return Err("You seem to be offline. Try again in a moment.".into());
    };
    let running = with(app, |inner| {
        let info = inner.info.as_ref()?;
        let server_running = info.running.as_ref().is_some_and(|running| running.id != OFFLINE_ID);
        // The project and note going on after "Drop the idle time and keep going".
        let current = inner.offline.running().map(|open| (Some(open.project.clone()), open.note.clone(), open.deliverable.clone()))
            .or_else(|| info.running.as_ref().map(|running| (running.project_id.clone(), running.note.clone(), None)));
        let queue = &mut inner.offline;
        match tap {
            Tap::Start { project: Some(project), note, deliverable } => queue.start(server_running, &project, note, deliverable, now),
            Tap::Start { project: None, .. } => return Some(Err(())),
            Tap::Stop { at } => queue.stop(server_running, at.unwrap_or(now).min(now)),
            Tap::Restart { at } => match current {
                Some((Some(project), note, deliverable)) => {
                    queue.stop(server_running, at.min(now));
                    queue.start(false, &project, note, deliverable, now);
                }
                _ => return Some(Err(())),
            },
        }
        let info = inner.info.as_ref()?;
        Some(Ok(offline_running(info, &inner.offline)))
    })
    .flatten();
    match running {
        Some(Ok(running)) => {
            set_running(app, running);
            crate::mini::ask_hourly(app, "__cbbMiniHourlyAfter", &json!({
                "file": false,
                "notice": "You're offline. The timer goes on here and reaches the tracker when you're back: keep the app open until then.",
            }));
            Ok(())
        }
        Some(Err(())) => Err("You're offline. Pick a project to time it offline: time not yet filed needs the tracker.".into()),
        None => Err("You seem to be offline. Try again in a moment.".into()),
    }
}

/// Today's entries, read through the page (blocking).
fn read_entries<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = crate::walkie::tracker_window(app) else { return };
    let _ = window.eval(TODAY_JS);
    let Some(raw) = crate::walkie::collect(&window, TAKE_TODAY_JS, 30) else { return };
    let today = read_today(&raw);
    let changed = with(app, |inner| {
        let changed = inner.today != today;
        inner.today = today;
        changed
    })
    .unwrap_or(false);
    if changed {
        refresh_menus(app);
        crate::mini::push(app);
    }
}

/// The timer the tracker says is running now, read afresh (blocking). After
/// sleep or time away the cached one can be stale, and a stop names no
/// timer: so check before ending anything.
fn running_now<R: Runtime>(app: &AppHandle<R>) -> Option<Running> {
    read(app);
    with(app, |inner| inner.info.as_ref().and_then(|info| info.running.clone())).flatten()
}

/// The tracker has answered whether this person bills by the hour.
pub(crate) fn has_read<R: Runtime>(app: &AppHandle<R>) -> bool {
    with(app, |inner| inner.read_at > 0).unwrap_or(false)
}

/// Read now (the tracker page just loaded), not after the first poll's wait.
pub(crate) fn read_now<R: Runtime>(app: &AppHandle<R>) {
    if crate::session::signed_in() {
        let app = app.clone();
        std::thread::spawn(move || read(&app));
    }
}

/// Read again soon if the last read is stale (the panel opened, or the
/// tracker window came forward).
pub(crate) fn refresh_if_stale<R: Runtime>(app: &AppHandle<R>) {
    let now = crate::clock::unix_now();
    let stale = with(app, |inner| now.saturating_sub(inner.read_at) > FRESH_FOR_SECS).unwrap_or(false);
    if stale && crate::session::signed_in() {
        let app = app.clone();
        std::thread::spawn(move || read(&app));
    }
}

/// One request at a time: they share the page's result variable.
static SENDING: Mutex<()> = Mutex::new(());

/// What to do once a request worked.
enum Then {
    /// Timer requests: the answer is the new running timer.
    Timer {
        /// An unfiled stretch that ended: ask which project it was for.
        file: Option<String>,
        /// Tell them this (the timer stopped while they were idle).
        notice: Option<String>,
        /// Say what happened in a notification (shortcut, menus).
        say: bool,
        /// An idle auto-stop (see `auto_stop`).
        auto: Option<AutoStop>,
        /// Sending what was timed offline (offline_timer.rs): done with once sent.
        replay: bool,
    },
    /// Saved without changing the running timer (filed, or time added):
    /// `filed` clears the question; `notice` is said in the panel.
    /// `logged` is a helper suggestion (detector.rs) to forget once saved.
    Saved { filed: bool, notice: Option<String>, logged: Option<u64> },
}

/// Sends `steps` through the page and hands the outcome on.
fn send<R: Runtime>(app: &AppHandle<R>, steps: Vec<Step>, done: &str, then: Then) -> Result<(), String> {
    let window = crate::walkie::tracker_window(app).ok_or("Open the tracker first.")?;
    let script = post_script(&steps, done);
    let app = app.clone();
    std::thread::spawn(move || {
        let sent = steps;
        let _one = SENDING.lock();
        if window.eval(&script).is_err() {
            return panel_error(&app, "The tracker could not be reached.");
        }
        let outcome = crate::walkie::collect(&window, TAKE_RESULT_JS, 40)
            .and_then(|raw| serde_json::from_str::<Outcome>(&raw).ok());
        match (outcome, then) {
            (Some(Outcome { ok: true, running, .. }), Then::Timer { file, notice, auto: Some(AutoStop { later: true, .. }), .. }) => {
                // Stopped while they're away: tell them, and ask for the
                // project, when they're back.
                set_running(&app, running.filter(valid_running).map(unfiled_not_billable));
                let _ = window.eval("window.dispatchEvent(new Event('cbb:hourly-changed'))");
                with(&app, |inner| {
                    inner.notice = notice;
                    inner.ask = None;
                    if file.is_some() {
                        inner.to_file = file;
                    }
                });
                crate::mini::push(&app);
            }
            (Some(Outcome { ok: true, running, .. }), Then::Timer { file, notice, say: tell, replay, .. }) => {
                if replay {
                    with(&app, |inner| inner.offline = crate::offline_timer::Queue::default());
                }
                let running = running.filter(valid_running).map(unfiled_not_billable);
                if tell && !panel_showing(&app) {
                    let what = match &running {
                        Some(running) => {
                            let label = with(&app, |inner| running_label(inner.info.as_ref(), running)).unwrap_or_default();
                            format!("Started {label}.")
                        }
                        None => "Stopped.".to_string(),
                    };
                    say(&app, &what);
                }
                set_running(&app, running);
                read_entries(&app);
                // The tracker's page shows the same timer: let it catch up.
                let _ = window.eval("window.dispatchEvent(new Event('cbb:hourly-changed'))");
                if file.is_some() || notice.is_some() {
                    if file.is_some() {
                        with(&app, |inner| inner.to_file.clone_from(&file));
                    }
                    crate::mini::push(&app);
                    let after = json!({ "file": file.is_some(), "notice": notice });
                    crate::mini::ask_hourly(&app, "__cbbMiniHourlyAfter", &after);
                }
            }
            (Some(Outcome { ok: true, .. }), Then::Saved { filed, notice, logged }) => {
                if filed {
                    with(&app, |inner| inner.to_file = None);
                }
                if let Some(id) = logged {
                    crate::detector::answered(&app, id);
                }
                read_entries(&app);
                crate::mini::push(&app);
                let _ = window.eval("window.dispatchEvent(new Event('cbb:hourly-changed'))");
                if let Some(notice) = notice {
                    crate::mini::ask_hourly(&app, "__cbbMiniHourlyAfter", &json!({ "file": false, "notice": notice }));
                }
            }
            (other, then) => {
                // A failed auto-stop is tried again on the next idle poll.
                if let Then::Timer { auto: Some(auto), .. } = &then {
                    with(&app, |inner| {
                        if inner.auto_stopped.as_deref() == Some(auto.timer.as_str()) {
                            inner.auto_stopped = None;
                        }
                    });
                }
                let error = other
                    .and_then(|outcome| outcome.error)
                    .unwrap_or_else(|| "The tracker could not be reached.".into());
                let offline = crate::offline_timer::is_offline(&error);
                if let Then::Timer { replay: true, .. } = &then {
                    if offline {
                        // Still offline: kept, and tried on the next read.
                        return;
                    }
                    // Refused: said once, not tried again and again.
                    with(&app, |inner| inner.offline = crate::offline_timer::Queue::default());
                    panel_error(&app, &format!("Your offline time didn't all reach the tracker ({}). Check it on My Hours.", error.trim_end_matches('.')));
                    read(&app);
                    return;
                }
                if offline && matches!(then, Then::Timer { .. }) {
                    match keep_offline(&app, &sent) {
                        Ok(()) => return,
                        Err(why) => return panel_error(&app, &why),
                    }
                }
                panel_error(&app, &error);
                read(&app);
            }
        }
    });
    Ok(())
}

/// Quietly hands the tracker the helper's suggestions (detector.rs `sync`,
/// or a "Not this"), through the page like everything else, and returns the
/// keys the tracker says are answered already. Nothing is shown: a failure
/// is simply tried again on the next poll. Blocking.
pub(crate) fn sync_suggestions<R: Runtime>(app: &AppHandle<R>, body: Value) -> Vec<String> {
    post_suggestions(app, body).map(|body| answered_keys(&body)).unwrap_or_default()
}

/// One POST to `/api/me/project-time-suggestions` through the page, and its
/// answer's body, or the tracker's refusal. Blocking.
pub(crate) fn post_suggestions<R: Runtime>(app: &AppHandle<R>, body: Value) -> Result<Value, String> {
    let window = crate::walkie::tracker_window(app).ok_or("Open the tracker first.")?;
    let script = post_script(&[post(SUGGESTIONS_URL, body)], "");
    let _one = SENDING.lock();
    window.eval(&script).map_err(|_| "The tracker could not be reached.".to_string())?;
    let outcome = crate::walkie::collect(&window, TAKE_RESULT_JS, 40)
        .and_then(|raw| serde_json::from_str::<Outcome>(&raw).ok())
        .ok_or("The tracker could not be reached.")?;
    if outcome.ok {
        Ok(outcome.body.unwrap_or(Value::Null))
    } else {
        Err(outcome.error.unwrap_or_else(|| "That didn't save. Try again.".into()))
    }
}

/// The keys in the tracker's `{answered: [...]}`, only ones the helper makes.
pub(crate) fn answered_keys(body: &Value) -> Vec<String> {
    body.get("answered")
        .and_then(Value::as_array)
        .map(|keys| keys.iter().filter_map(Value::as_str).filter(|key| crate::detector::id_of(key).is_some()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// A refusal from elsewhere in the panel (to_confirm.rs), said the same way.
pub(crate) fn show_error<R: Runtime>(app: &AppHandle<R>, error: &str) {
    panel_error(app, error);
}

/// A refusal: in the panel when it's showing, else as a notification (the
/// shortcut, the menus).
fn panel_error<R: Runtime>(app: &AppHandle<R>, error: &str) {
    match app.get_webview_window("mini").filter(|mini| mini.is_visible().unwrap_or(false)) {
        Some(mini) => {
            let text = Value::String(error.to_string());
            let _ = mini.eval(format!("window.__cbbMiniHourlyError && window.__cbbMiniHourlyError({text})"));
        }
        None => say(app, error),
    }
}

fn current<R: Runtime>(app: &AppHandle<R>) -> Result<Info, String> {
    with(app, |inner| inner.info.clone()).flatten().ok_or_else(|| "The project timer isn't on for you.".to_string())
}

/// The unfiled timer that a start or stop now ends, to file next.
fn ends_unfiled(info: &Info) -> Option<String> {
    info.running.as_ref().filter(|running| running.unfiled()).map(|running| running.id.clone())
}

/// Start (or switch to) a project, or an unfiled timer with `None`.
fn start<R: Runtime>(app: &AppHandle<R>, project_id: Option<&str>, note: Option<&str>, from: &'static str, tell: bool) -> Result<(), String> {
    start_with(app, project_id, note, false, None, from, tell)
}

/// `start`, also showing the client the note (the tracker's start takes no
/// such flag, so the new entry is marked right after).
fn start_with<R: Runtime>(
    app: &AppHandle<R>,
    project_id: Option<&str>,
    note: Option<&str>,
    for_client: bool,
    deliverable: Option<&str>,
    from: &'static str,
    tell: bool,
) -> Result<(), String> {
    let info = current(app)?;
    let mut steps = start_steps(project_id, note, for_client, &info).map_err(str::to_string)?;
    if let (Some(project_id), Some(first)) = (project_id, steps.first_mut()) {
        with_deliverable(&mut first.2, &info, project_id, deliverable).map_err(str::to_string)?;
    }
    let done = if steps.len() > 1 { "Started, but the note isn't shown to the client:" } else { "" };
    send(app, steps, done, Then::Timer { file: ends_unfiled(&info), notice: None, say: tell, auto: None, replay: false })?;
    let what = match (info.running.is_some(), project_id.is_some()) {
        (true, _) => "hourly_switch",
        (false, false) => "hourly_start_unfiled",
        (false, true) => "hourly_start",
    };
    crate::analytics::action(app, what, from);
    Ok(())
}

fn stop<R: Runtime>(app: &AppHandle<R>, from: &'static str, tell: bool) -> Result<(), String> {
    let info = current(app)?;
    if info.running.is_none() {
        return Err("No timer is running.".into());
    }
    send(app, vec![post(TIMER_URL, stop_body(None))], "", Then::Timer { file: ends_unfiled(&info), notice: None, say: tell, auto: None, replay: false })?;
    crate::analytics::action(app, "hourly_stop", from);
    Ok(())
}

/// Start (or switch to) a project, or stop, from the panel.
#[tauri::command]
pub(crate) fn mini_hourly<R: Runtime>(
    app: AppHandle<R>,
    action: String,
    project_id: Option<String>,
    note: Option<String>,
    deliverable_id: Option<String>,
) -> Result<(), String> {
    match action.as_str() {
        "start" => start_with(&app, project_id.as_deref(), note.as_deref(), false, deliverable_id.as_deref(), "panel", false),
        "stop" => stop(&app, "panel", false),
        _ => Err("Unknown action.".into()),
    }
}

/// The answer to "Keep it / Drop the idle time and keep going / Drop it
/// and stop" (`keep`, `resume`, `stop`).
#[tauri::command]
pub(crate) fn mini_hourly_idle<R: Runtime>(app: AppHandle<R>, choice: String) -> Result<(), String> {
    // The question stays until the answer has worked (a failed request can
    // be answered again); it goes when the timer it's about stops.
    let ask = with(&app, |inner| inner.ask.clone()).flatten().ok_or("That timer has already stopped.")?;
    let steps = idle_steps(&choice, &ask).map_err(str::to_string)?;
    if steps.is_empty() {
        with(&app, |inner| inner.ask = None);
        crate::mini::push(&app);
    } else {
        crate::walkie::tracker_window(&app).ok_or("Open the tracker first.")?;
        let app = app.clone();
        std::thread::spawn(move || {
            // Only the timer the question is about: it may have been
            // stopped or switched elsewhere meanwhile.
            if running_now(&app).as_ref().map(|running| &running.id) != Some(&ask.running.id) {
                with(&app, |inner| inner.ask = None);
                crate::mini::push(&app);
                return panel_error(&app, "That timer was stopped or switched elsewhere, so nothing changed.");
            }
            // The unfiled stretch up to the idle moment is filed next,
            // whether the timer stays stopped or starts again.
            let file = left_to_file(&ask.running, ask.from);
            let steps = steps.into_iter().map(|body| post(TIMER_URL, body)).collect();
            let then = Then::Timer { file, notice: None, say: false, auto: None, replay: false };
            if let Err(error) = send(&app, steps, "", then) {
                panel_error(&app, &error);
            }
        });
    }
    let what = match choice.as_str() {
        "resume" => "hourly_idle_resume",
        "stop" => "hourly_idle_stop",
        _ => "hourly_idle_keep",
    };
    crate::analytics::action(&app, what, "panel");
    Ok(())
}

/// "Which project was it for?": `file` with a project, `new` (Hours on the
/// website, to make a new project for it) or `later`.
#[tauri::command]
pub(crate) fn mini_hourly_file<R: Runtime>(app: AppHandle<R>, choice: String, project_id: Option<String>) -> Result<(), String> {
    let entry = with(&app, |inner| inner.to_file.clone()).flatten().ok_or("That time is already filed.")?;
    match choice.as_str() {
        "file" => {
            let info = current(&app)?;
            let body = file_body(&entry, project_id.as_deref().unwrap_or_default(), &info).map_err(str::to_string)?;
            send(&app, vec![post(FILE_URL, body)], "", Then::Saved { filed: true, notice: None, logged: None })?;
            crate::analytics::action(&app, "hourly_file", "panel");
        }
        "new" => {
            with(&app, |inner| inner.to_file = None);
            if let Some(href) = new_project_href(&entry) {
                crate::mini::open_href(&app, &href);
            }
            crate::analytics::action(&app, "hourly_file_new_project", "panel");
        }
        "later" => {
            with(&app, |inner| inner.to_file = None);
            crate::mini::push(&app);
            crate::analytics::action(&app, "hourly_file_later", "panel");
        }
        _ => return Err("Unknown choice.".into()),
    }
    Ok(())
}

/// The timer's state as last read, for the helper (detector.rs).
pub(crate) fn info_snapshot<R: Runtime>(app: &AppHandle<R>) -> Option<Info> {
    with(app, |inner| inner.info.clone()).flatten()
}

/// Logs a stretch the helper suggested and the person confirmed
/// (detector.rs); `id` is the suggestion, forgotten once it's logged.
pub(crate) fn log_stretch<R: Runtime>(app: &AppHandle<R>, project_id: &str, start: u64, end: u64, id: u64, deliverable: Option<&str>) -> Result<(), String> {
    let info = current(app)?;
    let mut body = add_body(project_id, start, end, None, &info, crate::clock::unix_now()).map_err(str::to_string)?;
    with_deliverable(&mut body, &info, project_id, deliverable).map_err(str::to_string)?;
    // So My Hours stops asking about it too.
    body["suggestionKey"] = Value::String(crate::detector::key_of(id));
    let label = info.projects.iter().find(|project| project.id == project_id).map(project_label).unwrap_or_default();
    let notice = format!("Logged {} on {label}.", fmt_minutes((end - start) / 60));
    send(app, vec![post(TIME_URL, body)], "", Then::Saved { filed: false, notice: Some(notice), logged: Some(id) })
}

const PROJECTS_URL: &str = "/api/me/projects";

/// `POST /api/me/projects`'s body: a name, and an existing client or a new
/// one's name (reused when the tracker has it already).
pub(crate) fn new_project_body(name: &str, client_id: Option<&str>, new_client: Option<&str>, billable: bool, info: &Info) -> Result<Value, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the project a name.");
    }
    if name.chars().count() > 200 {
        return Err("Keep the name shorter.");
    }
    let mut body = json!({ "name": name, "billable": billable });
    match (client_id.filter(|id| !id.is_empty()), new_client.map(str::trim).filter(|n| !n.is_empty())) {
        (Some(id), _) => {
            if !info.clients.iter().any(|client| client.id == id) {
                return Err("Pick the client again.");
            }
            body["clientId"] = Value::String(id.to_string());
        }
        (None, Some(client)) => {
            if !(2..=120).contains(&client.chars().count()) {
                return Err("A client's name is 2 to 120 characters.");
            }
            body["newClientName"] = Value::String(client.to_string());
        }
        (None, None) => return Err("Pick the client, or type a new one."),
    }
    Ok(body)
}

/// "New project…": made on the tracker, then added to the list straight
/// away and handed back, so the panel can start it, file to it or link to
/// it. Making it doesn't start the timer.
#[tauri::command]
pub(crate) async fn mini_hourly_new_project<R: Runtime>(
    app: AppHandle<R>,
    name: String,
    client_id: Option<String>,
    new_client_name: Option<String>,
    billable: bool,
) -> Result<Project, String> {
    let info = current(&app)?;
    let body = new_project_body(&name, client_id.as_deref(), new_client_name.as_deref(), billable, &info).map_err(str::to_string)?;
    let window = crate::walkie::tracker_window(&app).ok_or("Open the tracker first.")?;
    let script = post_script(&[post(PROJECTS_URL, body)], "");
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let _one = SENDING.lock();
        window.eval(&script).ok()?;
        let outcome = crate::walkie::collect(&window, TAKE_RESULT_JS, 40).and_then(|raw| serde_json::from_str::<Outcome>(&raw).ok());
        if outcome.as_ref().is_some_and(|o| o.ok) {
            let _ = window.eval("window.dispatchEvent(new Event('cbb:hourly-changed'))");
        }
        outcome
    })
    .await
    .map_err(|e| e.to_string())?
    .ok_or("The tracker didn't answer. Try again.")?;
    if !outcome.ok {
        return Err(outcome.error.unwrap_or_else(|| "That didn't save. Try again.".into()));
    }
    let project = outcome
        .body
        .and_then(|body| body.get("project").cloned())
        .and_then(|project| serde_json::from_value::<Project>(project).ok())
        .filter(|project| valid_id(&project.id))
        .ok_or("Made, but the answer didn't read: refresh the panel.")?;
    with(&app, |inner| {
        if let Some(info) = inner.info.as_mut() {
            info.projects.retain(|p| p.id != project.id);
            info.projects.insert(0, project.clone());
        }
    });
    refresh_menus(&app);
    crate::mini::push(&app);
    // A new client's id comes with the next read.
    let reader = app.clone();
    std::thread::spawn(move || read(&reader));
    Ok(project)
}

/// "Log it and start the timer" (detector.rs): the time worked so far on a
/// project, then the timer on it from now.
pub(crate) fn log_and_start<R: Runtime>(app: &AppHandle<R>, project_id: &str, start: u64, end: u64) -> Result<(), String> {
    let info = current(app)?;
    if info.running.is_some() {
        return Err("The timer's already running.".into());
    }
    let add = add_body(project_id, start, end, None, &info, crate::clock::unix_now()).map_err(str::to_string)?;
    let begin = start_body(Some(project_id), None, &info).map_err(str::to_string)?;
    let label = info.projects.iter().find(|project| project.id == project_id).map(project_label).unwrap_or_default();
    let notice = format!("Logged {} on {label}, and the timer's running.", fmt_minutes((end - start) / 60));
    let then = Then::Timer { file: None, notice: Some(notice), say: false, auto: None, replay: false };
    send(app, vec![post(TIME_URL, add), post(TIMER_URL, begin)], "Logged the time, but the timer didn't start:", then)
}

/// Wrapped on the time sheet (clock.rs) with a project timer still running:
/// the panel asks "Stop the timer too?". Stopping is the person's tap.
pub(crate) fn on_wrap<R: Runtime>(app: &AppHandle<R>) {
    // What the helper was building ends at a wrap too.
    crate::detector::close_now(app);
    day_summary(app);
    let Some(label) = with(app, |inner| {
        let info = inner.info.as_ref()?;
        info.running.as_ref().map(|running| running_label(Some(info), running))
    })
    .flatten() else {
        return;
    };
    crate::mini::ask_hourly(app, "__cbbMiniHourlyWrap", &json!({ "project": label }));
    crate::analytics::action(app, "hourly_wrap_asked", "time_sheet");
}

/// "Today: 6h 20m on 3 projects. 2 still to confirm." `None` with nothing
/// logged and nothing to confirm.
pub(crate) fn summary_text(today: Option<&Today>, to_confirm: usize, unlogged_minutes: u64) -> Option<String> {
    let minutes = today.map_or(0, |today| today.total_minutes);
    let projects = today.map_or(0, |today| {
        let mut ids: Vec<Option<&str>> = today.entries.iter().map(|entry| entry.project_id.as_deref()).collect();
        ids.sort();
        ids.dedup();
        ids.len()
    });
    let logged = match (minutes, projects) {
        (0, _) => "Nothing logged on the timer today.".to_string(),
        (m, 1) => format!("Today: {} on 1 project.", fmt_minutes(m)),
        (m, n) => format!("Today: {} on {n} projects.", fmt_minutes(m)),
    };
    let blanks = if unlogged_minutes > 0 { format!(" {} of your day isn't logged.", fmt_minutes(unlogged_minutes)) } else { String::new() };
    match (minutes, to_confirm, unlogged_minutes) {
        (0, 0, 0) => None,
        (_, 0, _) => Some(format!("{logged}{blanks}")),
        (_, 1, _) => Some(format!("{logged}{blanks} 1 thing still to confirm.")),
        (_, n, _) => Some(format!("{logged}{blanks} {n} things still to confirm.")),
    }
}

/// A wrap on the time sheet: what today came to, so anything missing is put
/// right before they leave. In the panel, and as a banner (unless quiet).
fn day_summary<R: Runtime>(app: &AppHandle<R>) {
    if with(app, |inner| inner.info.is_none()).unwrap_or(true) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        read_entries(&app);
        let to_confirm = crate::to_confirm::view(&app).map_or(0, |view| view.total);
        let today = with(&app, |inner| inner.today.clone()).flatten();
        let running = with(&app, |inner| inner.info.as_ref().and_then(|info| info.running.clone())).flatten();
        let unlogged = crate::day_gaps::view(&app, today.as_ref(), running.as_ref()).map_or(0, |gaps| gaps.minutes);
        let Some(text) = summary_text(today.as_ref(), to_confirm, unlogged) else { return };
        crate::mini::ask_hourly(&app, "__cbbMiniHourlyAfter", &json!({ "file": false, "notice": text }));
        if !panel_showing(&app) && crate::quiet::allowed(&app, crate::quiet::Kind::Tracker) {
            crate::banner::show(&app, "Your day on the timer", &text, crate::banner::Target::Panel);
        }
    });
}

/// "Add time…": a stretch the person confirmed, logged after the fact.
/// `started_at` and `ended_at` are ISO moments from the panel's inputs.
#[tauri::command]
pub(crate) fn mini_hourly_add<R: Runtime>(
    app: AppHandle<R>,
    project_id: String,
    started_at: String,
    ended_at: String,
    note: Option<String>,
    deliverable_id: Option<String>,
) -> Result<(), String> {
    let info = current(&app)?;
    let (Some(started), Some(ended)) = (crate::clock::parse_iso(&started_at), crate::clock::parse_iso(&ended_at)) else {
        return Err("Those times don't read right.".into());
    };
    let mut body = add_body(&project_id, started, ended, note.as_deref(), &info, crate::clock::unix_now()).map_err(str::to_string)?;
    with_deliverable(&mut body, &info, &project_id, deliverable_id.as_deref()).map_err(str::to_string)?;
    let label = info.projects.iter().find(|project| project.id == project_id).map(project_label).unwrap_or_default();
    let notice = format!("Added {} to {label}.", fmt_minutes((ended - started) / 60));
    send(&app, vec![post(TIME_URL, body)], "", Then::Saved { filed: false, notice: Some(notice), logged: None })?;
    crate::analytics::action(&app, "hourly_add_time", "panel");
    Ok(())
}

fn today_entry<R: Runtime>(app: &AppHandle<R>, entry_id: &str) -> Result<Entry, String> {
    with(app, |inner| inner.today.as_ref().and_then(|today| today.entries.iter().find(|e| e.id == entry_id).cloned()))
        .flatten()
        .ok_or_else(|| "That entry isn't in today's list any more.".to_string())
}

/// ▶ on one of today's entries: carry on with its project and note (an
/// unfiled one carries on unfiled).
#[tauri::command]
pub(crate) fn mini_hourly_continue<R: Runtime>(app: AppHandle<R>, entry_id: String) -> Result<(), String> {
    let entry = today_entry(&app, &entry_id)?;
    let project = entry.project_id.as_deref();
    // Carrying on a project they're no longer on: the tracker refuses a start
    // there, so say so plainly.
    if let Some(project) = project {
        if !current(&app)?.projects.iter().any(|p| p.id == project) {
            return Err("You can't time that project any more.".into());
        }
    }
    start_with(&app, project, entry.note.as_deref(), entry.note_for_client, None, "panel", false)?;
    crate::analytics::action(&app, "hourly_continue", "panel");
    Ok(())
}

/// Save the panel's edit of one of today's entries. Times are ISO moments.
#[tauri::command]
pub(crate) fn mini_hourly_edit<R: Runtime>(
    app: AppHandle<R>,
    entry_id: String,
    started_at: Option<String>,
    ended_at: Option<String>,
    note: Option<String>,
    project_id: Option<String>,
    note_for_client: Option<bool>,
) -> Result<(), String> {
    let entry = today_entry(&app, &entry_id)?;
    let info = current(&app)?;
    let read = |raw: Option<String>| -> Result<Option<u64>, String> {
        match raw {
            None => Ok(None),
            Some(raw) => crate::clock::parse_iso(&raw).map(Some).ok_or_else(|| "Those times don't read right.".to_string()),
        }
    };
    let change = entry_change(
        &entry,
        read(started_at)?,
        read(ended_at)?,
        note.as_deref(),
        project_id.as_deref(),
        &info,
        crate::clock::unix_now(),
        note_for_client,
    )
    .map_err(str::to_string)?;
    // The project first (the tracker's "file"), then the times and note.
    let mut steps = Vec::new();
    if let Some(project) = &change.file_to {
        steps.push(post(FILE_URL, json!({ "entryId": entry.id, "projectId": project })));
    }
    if let Some(patch) = change.patch {
        steps.push(("PATCH", format!("{TIME_URL}/{}", entry.id), patch));
    }
    let done = if change.file_to.is_some() { "Moved to the project, but the times didn't save:" } else { "" };
    send(&app, steps, done, Then::Saved { filed: false, notice: Some("Saved.".into()), logged: None })?;
    crate::analytics::action(&app, "hourly_edit", "panel");
    Ok(())
}

/// Delete one of today's entries (not the running one: stop it first).
#[tauri::command]
pub(crate) fn mini_hourly_delete<R: Runtime>(app: AppHandle<R>, entry_id: String) -> Result<(), String> {
    let entry = today_entry(&app, &entry_id)?;
    if entry.billed {
        return Err("These hours are on an invoice already.".into());
    }
    if entry.ended_at.is_none() {
        return Err("Stop the timer first.".into());
    }
    let url = format!("{TIME_URL}/{}", entry.id);
    send(&app, vec![("DELETE", url, Value::Null)], "", Then::Saved { filed: false, notice: Some("Deleted.".into()), logged: None })?;
    crate::analytics::action(&app, "hourly_delete", "panel");
    Ok(())
}

/// Pin a project to the front of the one-tap starts, or unpin it.
#[tauri::command]
pub(crate) fn mini_hourly_pin<R: Runtime>(app: AppHandle<R>, project_id: String, pinned: bool) -> Result<(), String> {
    let info = current(&app)?;
    if !info.projects.iter().any(|project| project.id == project_id) {
        return Err("That project isn't one of yours.".into());
    }
    let list = toggle_pin(&pins(&app), &project_id, pinned);
    crate::close::set_strings(&app, PINNED, &list);
    if let Ok(mut cached) = PINS.lock() {
        *cached = Some(list);
    }
    refresh_menus(&app);
    crate::mini::push(&app);
    crate::analytics::setting_changed(&app, "hourly_pin", pinned);
    Ok(())
}

/// The first time a timer is stopped for idle time (once per timer).
fn first_auto_stop<R: Runtime>(app: &AppHandle<R>, running: &Running) -> bool {
    with(app, |inner| {
        let first = inner.auto_stopped.as_deref() != Some(running.id.as_str());
        inner.auto_stopped = Some(running.id.clone());
        first
    })
    .unwrap_or(false)
}

/// Every idle poll (idle.rs): `idle` is the OS's seconds without input.
/// Thirty minutes into idle time with the timer running since before it
/// began, nobody has answered: stop it when idling began.
pub(crate) fn on_idle<R: Runtime>(app: &AppHandle<R>, now: u64, idle: Option<u64>) {
    let Some(idle) = idle else { return };
    crate::day_gaps::note_idle(now, idle);
    let since = now.saturating_sub(idle);
    let running = with(app, |inner| inner.info.as_ref().and_then(|info| info.running.clone())).flatten();
    let Some(running) = running else { return };
    if idle_call(Some(&running), since, now, idle_minutes(app) * 60) == IdleCall::Stop && first_auto_stop(app, &running) {
        auto_stop(app, &running, since, false);
    }
}

/// Stops `running` at `at` (when idling began) and tells the person: now if
/// they're `back`, else when they come back (on_away).
fn auto_stop<R: Runtime>(app: &AppHandle<R>, running: &Running, at: u64, back: bool) {
    let app = app.clone();
    let running = running.clone();
    std::thread::spawn(move || {
        // Only the timer that was running when idling began, as the tracker
        // has it now: after sleep the cached one can be stale, and a stop
        // names no timer.
        if running_now(&app).as_ref().map(|now| &now.id) != Some(&running.id) {
            return;
        }
        let label = with(&app, |inner| running_label(inner.info.as_ref(), &running)).unwrap_or_else(|| "Not yet filed".into());
        let notice = format!(
            "You were away, so your timer on {label} stopped at {}, when you went idle.",
            crate::clock::fmt_hhmm_sast(at)
        );
        let then = Then::Timer {
            file: left_to_file(&running, at),
            notice: Some(notice),
            say: false,
            auto: Some(AutoStop { timer: running.id.clone(), later: !back }),
            replay: false,
        };
        if send(&app, vec![post(TIMER_URL, away_stop_body(at))], "", then).is_err() {
            with(&app, |inner| inner.auto_stopped = None);
        }
        crate::analytics::action(&app, "hourly_idle_auto_stop", "idle");
    });
}

/// An idle auto-stop in flight: `later` keeps what to tell for when the
/// person is back (on_away); a failure lets it be tried again.
struct AutoStop {
    timer: String,
    later: bool,
}

/// Back from idle time (idle.rs).
pub(crate) fn on_away<R: Runtime>(app: &AppHandle<R>, away: crate::idle::Away) {
    crate::day_gaps::note_away(away.from, away.to);
    // Stopped while they were away: say so, and ask for its project if it
    // wasn't filed.
    let (notice, filing) = with(app, |inner| (inner.notice.take(), inner.to_file.is_some())).unwrap_or((None, false));
    if let Some(notice) = notice {
        crate::mini::ask_hourly(app, "__cbbMiniHourlyAfter", &json!({ "file": filing, "notice": notice }));
        return;
    }
    // Afresh: after sleep the cached timer can be stale (on idle.rs's
    // thread, so a blocking read is fine here).
    let Some(running) = running_now(app) else { return };
    match idle_call(Some(&running), away.from, away.to, idle_minutes(app) * 60) {
        IdleCall::Nothing => {}
        // The computer slept through it: the same rule, now they're back.
        IdleCall::Stop => {
            if first_auto_stop(app, &running) {
                auto_stop(app, &running, away.from, true);
            }
        }
        IdleCall::Ask => {
            let ask = IdleAsk { running: running.clone(), from: away.from, to: away.to };
            with(app, |inner| inner.ask = Some(ask));
            let label = with(app, |inner| running_label(inner.info.as_ref(), &running)).unwrap_or_default();
            let question = json!({
                "project": label,
                "billable": running.billable,
                "unfiled": running.unfiled(),
                "from": away.from * 1000,
                "to": away.to * 1000,
            });
            crate::mini::push(app);
            crate::mini::ask_hourly(app, "__cbbMiniHourlyIdle", &question);
        }
    }
}

fn maybe_nudge<R: Runtime>(app: &AppHandle<R>) {
    let now = crate::clock::unix_now();
    let in_since = app
        .try_state::<crate::clock::Last>()
        .and_then(|last| last.0.lock().ok().and_then(|slot| slot.clone()))
        .filter(|clock| clock.available && clock.state == "in")
        .and_then(|clock| crate::clock::parse_iso(&clock.since_iso));
    // Held back while quiet, not used up.
    if !crate::quiet::allowed(app, crate::quiet::Kind::Tracker) {
        return;
    }
    let nudge = with(app, |inner| {
        let eligible = inner.info.as_ref().is_some_and(|info| info.running.is_none() && !info.projects.is_empty());
        if eligible && should_nudge(in_since, inner.last_timer_at, inner.nudged_for, now) {
            inner.nudged_for = in_since.unwrap_or_default();
            true
        } else {
            false
        }
    })
    .unwrap_or(false);
    if nudge {
        crate::banner::show(
            app,
            "Working on something?",
            "Start a project timer from the quick panel.",
            crate::banner::Target::Panel,
        );
        crate::analytics::action(app, "hourly_nudge", "banner");
    }
}

/// Start the background reader for the app's life.
pub(crate) fn start_polling<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_AFTER);
        loop {
            if crate::session::signed_in() {
                // read() checks the retainer too (set_info).
                read(&app);
                maybe_nudge(&app);
                if with(&app, |inner| inner.info.is_some()).unwrap_or(false) {
                    crate::detector::sync(&app);
                    crate::to_confirm::poll(&app);
                }
            } else if with(&app, |inner| inner.info.is_some()).unwrap_or(false) {
                // Signed out: nothing of the timer stays, and the shortcut
                // is given back to other apps.
                with(&app, |inner| *inner = Inner::default());
                crate::to_confirm::clear();
                sync_shortcut(&app, false);
                refresh_menus(&app);
                crate::mini::push(&app);
            }
            std::thread::sleep(POLL_EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = r#"{"rateCents":65000,"currency":"ZAR","projects":[
        {"id":"p1","name":"Con Spirito S1","projectNumber":"CB-0412","clientName":"Spirit Films","billable":true},
        {"id":"p2","name":"Brand film","projectNumber":"CB-0399","clientName":null,"billable":false},
        {"id":"p3","name":"Three","projectNumber":"CB-0300","billable":true},
        {"id":"p4","name":"Four","projectNumber":"CB-0301","billable":false},
        {"id":"p5","name":"Five","projectNumber":"CB-0302","billable":false},
        {"id":"bad id';alert(1)//","name":"x","billable":true}],
      "running":{"id":"t1","projectId":"p2","projectName":"Brand film","billable":false,"startedAt":"2026-10-12T07:00:00.000Z","note":"Grade"}}"#;

    fn info() -> Info {
        parse(ANSWER).expect("a real answer")
    }

    // 2026-10-12T07:00:00Z, when the running timer started.
    const STARTED: u64 = 1_791_788_400;

    #[test]
    fn only_a_real_answer_shows_the_timer() {
        for raw in ["", "null", "<html>", r#"{"error":"Not found"}"#, r#"{"projects":"x"}"#] {
            assert_eq!(parse(raw), None, "{raw}");
        }
        let info = info();
        assert_eq!(info.projects.len(), 5, "an id that could reach a script is dropped");
        assert_eq!(info.running.as_ref().and_then(|r| r.project_id.as_deref()), Some("p2"));
        assert_eq!(parse(r#"{"projects":[],"running":null}"#).map(|i| i.projects.len()), Some(0));
    }

    #[test]
    fn one_tap_starts_are_the_four_latest_but_the_running_one() {
        let ids: Vec<String> = quick(&info(), &[]).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["p1", "p3", "p4", "p5"]);
        let mut stopped = info();
        stopped.running = None;
        let ids: Vec<String> = quick(&stopped, &[]).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["p1", "p2", "p3", "p4"]);
        // Pinned first; a pin that isn't one of theirs any more is skipped.
        let pins = vec!["p5".to_string(), "gone".to_string()];
        let ids: Vec<String> = quick(&stopped, &pins).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["p5", "p1", "p2", "p3"]);
        // Many pins: all of them, up to six.
        let pins: Vec<String> = ["p5", "p4", "p3", "p2", "p1"].iter().map(|s| s.to_string()).collect();
        let ids: Vec<String> = quick(&stopped, &pins).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["p5", "p4", "p3", "p2", "p1"]);
        // The running project is never a one-tap start, pinned or not.
        let ids: Vec<String> = quick(&info(), &["p2".to_string()]).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["p1", "p3", "p4", "p5"]);
    }

    #[test]
    fn starts_name_one_of_your_projects_or_none_and_carry_only_what_the_tracker_takes() {
        let info = info();
        assert_eq!(
            start_body(Some("p1"), Some("  Edit\nv2  "), &info),
            Ok(json!({ "action": "start", "projectId": "p1", "note": "Edit v2" }))
        );
        assert_eq!(start_body(Some("p1"), Some("   "), &info), Ok(json!({ "action": "start", "projectId": "p1" })));
        // Just start, file it later.
        assert_eq!(start_body(None, Some("Call"), &info), Ok(json!({ "action": "start", "note": "Call" })));
        assert!(start_body(Some("someone-elses"), None, &info).is_err());
        // Carrying on a note the client sees: the new entry is marked too.
        let steps = start_steps(Some("p1"), Some("Call"), true, &info).unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!((steps[1].0, steps[1].1.as_str()), ("PATCH", "/api/me/project-time/{running}"));
        assert_eq!(steps[1].2, json!({ "noteForClient": true }));
        assert_eq!(start_steps(Some("p1"), None, true, &info).unwrap().len(), 1);
        assert_eq!(start_steps(Some("p1"), Some("Call"), false, &info).unwrap().len(), 1);
        assert!(post_script(&steps, "").contains("indexOf(\"{running}\")"));
        let long = "x".repeat(900);
        let body = start_body(Some("p1"), Some(&long), &info).unwrap();
        assert_eq!(body["note"].as_str().map(str::len), Some(500));
        assert_eq!(stop_body(None), json!({ "action": "stop" }));
        assert_eq!(stop_body(Some(STARTED)), json!({ "action": "stop", "at": "2026-10-12T07:00:00.000Z" }));
    }

    #[test]
    fn filing_names_the_entry_and_one_of_your_projects() {
        let info = info();
        assert_eq!(file_body("t9", "p3", &info), Ok(json!({ "entryId": "t9", "projectId": "p3" })));
        assert!(file_body("t9", "someone-elses", &info).is_err());
        assert!(file_body("t9'", "p3", &info).is_err());
        assert_eq!(new_project_href("t9").as_deref(), Some("/hours?entry=t9&panel=new-project"));
        assert_eq!(new_project_href("x&panel=evil"), None);
    }

    #[test]
    fn the_script_posts_each_step_and_stops_at_a_refusal() {
        let js = post_script(&[post(TIMER_URL, json!({ "action": "stop" })), post(FILE_URL, json!({ "entryId": "t", "projectId": "p" }))], "Stopped.");
        assert!(js.contains("var init = { method: steps[i].method, credentials: 'same-origin' };"));
        assert!(js.contains("fetch(url, init)"));
        assert!(js.ends_with(
            r#"})([{"body":{"action":"stop"},"method":"POST","url":"/api/me/project-timer"},{"body":{"entryId":"t","projectId":"p"},"method":"POST","url":"/api/me/project-time/file"}], "Stopped.")"#
        ));
        assert!(js.contains("if (!x.ok)"));
        assert!(!js.contains("localStorage") && !js.contains("console."));
    }

    #[test]
    fn outcomes_read_both_ways() {
        let ok: Outcome = serde_json::from_str(r#"{"ok":true,"running":null}"#).unwrap();
        assert_eq!(ok, Outcome { ok: true, running: None, error: None, body: None });
        let refused: Outcome = serde_json::from_str(r#"{"ok":false,"error":"That project is closed."}"#).unwrap();
        assert_eq!(refused.error.as_deref(), Some("That project is closed."));
    }

    #[test]
    fn an_unfiled_timer_is_never_billable() {
        let raw = r#"{"projects":[],"running":{"id":"t9","projectId":null,"projectName":"x","clientName":"y","retainerMinutesLeft":30,"billable":true,"startedAt":"2026-10-12T07:00:00.000Z","note":null}}"#;
        let running = parse(raw).and_then(|info| info.running).expect("an unfiled timer");
        assert!(running.unfiled());
        assert!(!running.billable);
        assert_eq!((running.client_name.as_deref(), running.retainer_minutes_left), (None, None));
        assert_eq!(running_label(None, &running), "Not yet filed");
        // A bad project id drops the timer rather than showing it as unfiled.
        let bad = r#"{"projects":[],"running":{"id":"t9","projectId":"a b","startedAt":"2026-10-12T07:00:00.000Z"}}"#;
        assert_eq!(parse(bad).and_then(|info| info.running), None);
    }

    #[test]
    fn idle_time_asks_after_the_chosen_minutes_and_stops_at_thirty() {
        let info = info();
        let running = info.running.as_ref();
        let since = STARTED + 3600;
        let ten = 10 * 60;
        assert_eq!(idle_call(running, since, since + 9 * 60, ten), IdleCall::Nothing);
        assert_eq!(idle_call(running, since, since + ten, ten), IdleCall::Ask);
        assert_eq!(idle_call(running, since, since + 5 * 60, 5 * 60), IdleCall::Ask);
        assert_eq!(idle_call(running, since, since + 29 * 60, ten), IdleCall::Ask);
        assert_eq!(idle_call(running, since, since + 30 * 60, ten), IdleCall::Stop);
        // The timer started after idling began (from a phone, say).
        assert_eq!(idle_call(running, STARTED - 600, STARTED + 3600, ten), IdleCall::Nothing);
        assert_eq!(idle_call(None, since, since + 7200, ten), IdleCall::Nothing);
    }

    #[test]
    fn a_stop_while_away_is_marked_and_answers_read_only_the_helpers_keys() {
        assert_eq!(away_stop_body(1_760_000_000), json!({ "action": "stop", "at": "2025-10-09T08:53:20.000Z", "away": true }));
        assert!(stop_body(Some(1_760_000_000)).get("away").is_none());
        let body = json!({ "answered": ["desktop:1760000000", "booking:x", 7, "desktop:nope"] });
        assert_eq!(answered_keys(&body), vec!["desktop:1760000000".to_string()]);
        assert!(answered_keys(&json!({})).is_empty());
    }

    #[test]
    fn a_deliverable_is_sent_only_when_it_is_one_of_that_projects() {
        let raw = r#"{"projects":[{"id":"p1","name":"Mixed job","billable":true,"deliverables":[{"id":"d1","name":"The edit"},{"id":"bad id'","name":"x"},{"id":"d2","name":""}]},
            {"id":"p2","name":"Quoted","billable":false}]}"#;
        let info = parse(raw).unwrap();
        assert_eq!(info.projects[0].deliverables, vec![Deliverable { id: "d1".into(), name: "The edit".into() }]);
        assert!(info.projects[1].deliverables.is_empty());
        let mut body = json!({ "action": "start", "projectId": "p1" });
        with_deliverable(&mut body, &info, "p1", Some("d1")).unwrap();
        assert_eq!(body["deliverableId"], json!("d1"));
        let mut body = json!({});
        with_deliverable(&mut body, &info, "p1", None).unwrap();
        with_deliverable(&mut body, &info, "p1", Some("")).unwrap();
        assert!(body.get("deliverableId").is_none());
        assert!(with_deliverable(&mut body, &info, "p2", Some("d1")).is_err());
        assert!(with_deliverable(&mut body, &info, "p1", Some("d9")).is_err());
    }

    #[test]
    fn the_wrap_says_what_today_came_to_and_what_is_left_to_confirm() {
        let entry = |project: Option<&str>| Entry {
            id: "e".into(), project_id: project.map(str::to_string), project_name: None, client_name: None, billable: true,
            started_at: "2026-10-14T07:00:00.000Z".into(), ended_at: None, minutes: 0, note: None, billed: false, note_for_client: false,
        };
        let today = Today { day: "2026-10-14".into(), entries: vec![entry(Some("p1")), entry(Some("p2")), entry(Some("p1"))], total_minutes: 380, billable_minutes: 380 };
        assert_eq!(summary_text(Some(&today), 0, 0).as_deref(), Some("Today: 6h 20m on 2 projects."));
        assert_eq!(summary_text(Some(&today), 2, 0).as_deref(), Some("Today: 6h 20m on 2 projects. 2 things still to confirm."));
        let one = Today { entries: vec![entry(Some("p1"))], total_minutes: 45, ..today.clone() };
        assert_eq!(summary_text(Some(&one), 1, 0).as_deref(), Some("Today: 45m on 1 project. 1 thing still to confirm."));
        assert_eq!(summary_text(None, 1, 0).as_deref(), Some("Nothing logged on the timer today. 1 thing still to confirm."));
        assert_eq!(summary_text(None, 0, 0), None);
        assert_eq!(summary_text(Some(&today), 1, 95).as_deref(), Some("Today: 6h 20m on 2 projects. 1h 35m of your day isn't logged. 1 thing still to confirm."));
    }

    #[test]
    fn answers_only_ever_end_time_at_the_idle_moment() {
        let ask = IdleAsk { running: info().running.unwrap(), from: STARTED + 9600, to: STARTED + 11_400 };
        assert_eq!(idle_steps("keep", &ask), Ok(vec![]));
        assert_eq!(idle_steps("stop", &ask), Ok(vec![json!({ "action": "stop", "at": "2026-10-12T09:40:00.000Z" })]));
        // One call, one transaction on the tracker.
        assert_eq!(idle_steps("resume", &ask), Ok(vec![json!({ "action": "restart", "at": "2026-10-12T09:40:00.000Z" })]));
        assert!(idle_steps("delete", &ask).is_err());
        // Only an unfiled stretch that is still there is left to file.
        assert_eq!(left_to_file(&ask.running, ask.from), None, "filed under a project");
        let mut unfiled = ask.running.clone();
        unfiled.project_id = None;
        assert_eq!(left_to_file(&unfiled, ask.from).as_deref(), Some("t1"));
        assert_eq!(left_to_file(&unfiled, STARTED), None, "ended at its start: dropped");
    }

    #[test]
    fn iso_round_trips_with_the_clocks_reader() {
        for secs in [0, STARTED, 951_782_400 /* 2000-02-29 */, 4_102_444_799] {
            assert_eq!(crate::clock::parse_iso(&iso_utc(secs)), Some(secs), "{secs}");
        }
        assert_eq!(iso_utc(STARTED), "2026-10-12T07:00:00.000Z");
    }

    #[test]
    fn a_nudge_comes_once_per_call_in_after_half_an_hour_without_a_timer() {
        let since = STARTED;
        assert!(!should_nudge(None, 0, 0, since + 7200));
        assert!(!should_nudge(Some(since), 0, 0, since + 29 * 60));
        assert!(should_nudge(Some(since), 0, 0, since + 30 * 60));
        assert!(!should_nudge(Some(since), 0, since, since + 7200));
        assert!(!should_nudge(Some(since), since + 7200 - 600, 0, since + 7200));
        assert!(should_nudge(Some(since), since + 3600, 0, since + 3600 + 1800));
    }

    #[test]
    fn the_menus_show_the_timer_and_the_recent_projects() {
        let model = menu_model(&info(), &[], None, STARTED + 65 * 60);
        assert_eq!(model.title, "Brand film · 1h 05m");
        assert!(model.running);
        assert_eq!(model.projects[0], ("hourly-start-p1".to_string(), "Spirit Films · Con Spirito S1 (Billable)".to_string()));
        assert_eq!(model.projects.len(), 4);
        let mut stopped = info();
        stopped.running = None;
        assert_eq!(menu_model(&stopped, &[], None, STARTED).title, "Project timer");
        assert_eq!(menu_model(&stopped, &[], Some(320), STARTED).title, "Project timer · Today 5h 20m");
        let mut unfiled = info();
        unfiled.running.as_mut().unwrap().project_id = None;
        unfiled.running.as_mut().unwrap().project_name = None;
        assert_eq!(menu_model(&unfiled, &[], Some(320), STARTED + 600).title, "Not yet filed · 10m");
        for id in [MENU_STOP, MENU_LATER, MENU_HOURS, MENU_ADD, "hourly-start-p1"] {
            assert!(is_menu_id(id), "{id}");
        }
        assert_eq!(menu_project("hourly-start-p1"), Some("p1"));
        for id in ["hourly-start-", "hourly-start-a b", "hourly-delete", "tray-quit"] {
            assert!(!is_menu_id(id), "{id}");
        }
    }

    #[test]
    fn everything_is_named_client_then_project() {
        let info = info();
        assert_eq!(project_label(&info.projects[0]), "Spirit Films · Con Spirito S1");
        // No client: the project's name alone, never the code.
        assert_eq!(project_label(&info.projects[1]), "Brand film");
        let running = info.running.clone().unwrap();
        assert_eq!(running_label(Some(&info), &running), "Brand film");
        // The running timer's own client and project, even for a project
        // no longer in the list.
        let mut elsewhere = running.clone();
        elsewhere.project_id = Some("p99".into());
        elsewhere.project_name = Some("Logo refresh".into());
        elsewhere.client_name = Some("Vimeo".into());
        assert_eq!(running_label(Some(&info), &elsewhere), "Vimeo · Logo refresh");
        // Without names on it, the list's.
        let mut bare = running;
        bare.project_id = Some("p1".into());
        bare.project_name = None;
        assert_eq!(running_label(Some(&info), &bare), "Spirit Films · Con Spirito S1");
    }

    #[test]
    fn retainers_read_as_time_left_or_over() {
        assert_eq!(retainer_text(150), "Retainer · 2h 30m left");
        assert_eq!(retainer_text(60), "Retainer · 1h left");
        assert_eq!(retainer_text(0), "Retainer · 0m left");
        assert_eq!(retainer_text(-60), "Retainer used · 1h over");
        assert_eq!(retainer_text(-45), "Retainer used · 45m over");
        // 90 minutes left from finished entries; the running timer has run
        // 40 of them (started 07:00, now 07:40), so 50 are left: under an hour.
        let raw = r#"{"projects":[],
            "running":{"id":"t1","projectId":"p1","projectName":"Logo refresh","clientName":"Vimeo","billable":true,"retainerMinutesLeft":90,"startedAt":"2026-10-12T07:00:00.000Z"}}"#;
        let info = parse(raw).unwrap();
        let now = STARTED + 40 * 60;
        let running = info.running.as_ref().unwrap();
        assert_eq!(live_retainer(running, now), Some(50));
        assert_eq!(retainer_low(&info, now), Some(("p1".into(), "Vimeo".into(), 50)));
        assert_eq!(retainer_banner("Vimeo", 50), "Vimeo's retainer has 50m left this month.");
        assert_eq!(retainer_banner("Vimeo", -90), "Vimeo's retainer is used up this month (1h 30m over). More time is billed extra.");
        // Ten minutes in, 80 are left: no banner yet.
        assert_eq!(retainer_low(&info, STARTED + 10 * 60), None);
        // A tracked-only timer doesn't use the retainer.
        let mut tracked = info.clone();
        tracked.running.as_mut().unwrap().billable = false;
        assert_eq!(live_retainer(tracked.running.as_ref().unwrap(), now), Some(90));
        let mut none = info.clone();
        none.running.as_mut().unwrap().retainer_minutes_left = None;
        assert_eq!(retainer_low(&none, now), None);
        let mut stopped = info;
        stopped.running = None;
        assert_eq!(retainer_low(&stopped, now), None);
        // 2026-10-31 23:30 UTC is already November in Johannesburg.
        assert_eq!(sast_month(1_793_489_400), 202611);
        assert_eq!(sast_month(STARTED), 202610);
    }

    #[test]
    fn todays_booking_comes_first_when_its_one_of_theirs() {
        let mut info = info();
        info.running = None;
        // 2026-10-12 07:00 UTC: 09:00 in Johannesburg, the same day.
        let now = STARTED;
        assert_eq!(booked_project(&info, "2026-10-12", "CB-0301", now).as_deref(), Some("p4"));
        assert_eq!(booked_project(&info, "2026-10-13", "CB-0301", now), None, "tomorrow");
        assert_eq!(booked_project(&info, "2026-10-12", "CB-9999", now), None, "not one of theirs");
        assert_eq!(booked_project(&info, "2026-10-12", "", now), None);
        // 23:30 UTC is already the next day in Johannesburg.
        assert_eq!(booked_project(&info, "2026-10-13", "CB-0301", STARTED + 16 * 3600 + 1800).as_deref(), Some("p4"));
        let ids: Vec<String> = with_booked(quick(&info, &[]), &info, Some("p4")).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["p4", "p1", "p2", "p3"]);
        // Already timing it: nothing to suggest.
        let running = self::info();
        let ids: Vec<String> = with_booked(quick(&running, &[]), &running, Some("p2")).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["p1", "p3", "p4", "p5"]);
    }

    #[test]
    fn pins_toggle_and_stay_at_six() {
        let pins = toggle_pin(&[], "p1", true);
        assert_eq!(pins, ["p1"]);
        let pins = toggle_pin(&pins, "p2", true);
        assert_eq!(toggle_pin(&pins, "p1", true), ["p2", "p1"], "pinning again moves it last, once");
        assert_eq!(toggle_pin(&pins, "p1", false), ["p2"]);
        let many: Vec<String> = (0..9).map(|n| format!("p{n}")).collect();
        assert_eq!(toggle_pin(&many, "x", true).len(), 6);
    }

    #[test]
    fn added_time_is_checked_before_it_is_sent() {
        let info = info();
        let now = STARTED + 8 * 3600;
        assert_eq!(
            add_body("p1", STARTED, STARTED + 5400, Some(" Rough cut "), &info, now),
            Ok(json!({ "projectId": "p1", "startedAt": "2026-10-12T07:00:00.000Z", "endedAt": "2026-10-12T08:30:00.000Z", "note": "Rough cut" }))
        );
        assert!(add_body("nope", STARTED, STARTED + 60, None, &info, now).is_err());
        assert!(add_body("p1", STARTED, STARTED, None, &info, now).is_err(), "ends at its start");
        assert!(add_body("p1", now, now + 600, None, &info, now).is_err(), "in the future");
        assert!(add_body("p1", now - 600, now + 30, None, &info, now).is_ok(), "a little clock drift");
        assert!(add_body("p1", STARTED - 25 * 3600, STARTED, None, &info, now).is_err(), "over 24 hours");
    }

    #[test]
    fn the_shortcut_is_written_each_systems_way() {
        assert_eq!(shortcut_label("macos"), "⌘⇧Y");
        assert_eq!(shortcut_label("windows"), "Ctrl+Alt+Y");
    }

    #[test]
    fn only_404_403_401_hide_the_timer_and_blips_keep_it() {
        let ok = serde_json::to_string(&json!({ "s": 200, "b": ANSWER })).unwrap();
        assert!(matches!(read_answer(&ok), Read::Hourly(_)));
        for s in [401, 403, 404] {
            assert_eq!(read_answer(&json!({ "s": s, "b": "{\"error\":\"Not found.\"}" }).to_string()), Read::Off, "{s}");
        }
        for s in [0, 500, 502, 503] {
            assert_eq!(read_answer(&json!({ "s": s, "b": "" }).to_string()), Read::Keep, "{s}");
        }
        // A 200 that doesn't read, or no answer at all: keep what's known.
        assert_eq!(read_answer(&json!({ "s": 200, "b": "<html>" }).to_string()), Read::Keep);
        assert_eq!(read_answer("null"), Read::Keep);
        assert_eq!(read_answer(""), Read::Keep);
    }

    fn entry(ended: Option<&str>) -> Entry {
        Entry {
            id: "e1".into(),
            project_id: Some("p1".into()),
            project_name: Some("Con Spirito S1".into()),
            client_name: Some("Spirit Films".into()),
            billable: true,
            started_at: "2026-10-12T07:00:00.000Z".into(),
            ended_at: ended.map(str::to_string),
            note: Some("Grade".into()),
            billed: false,
            minutes: 90,
            note_for_client: false,
        }
    }

    #[test]
    fn a_note_can_be_shown_to_the_client() {
        let info = info();
        let done = entry(Some("2026-10-12T08:30:00.000Z"));
        let now = crate::clock::parse_iso("2026-10-12T12:00:00.000Z").unwrap();
        assert_eq!(
            entry_change(&done, None, None, None, None, &info, now, Some(true)),
            Ok(Change { patch: Some(json!({ "noteForClient": true })), file_to: None })
        );
        assert_eq!(entry_change(&done, None, None, None, None, &info, now, Some(false)), Err("Nothing changed."));
    }

    #[test]
    fn a_new_project_needs_a_name_and_a_client() {
        let mut info = info();
        info.clients = vec![Client { id: "c1".into(), name: "Vimeo".into() }];
        assert_eq!(
            new_project_body(" Logo refresh ", Some("c1"), Some("ignored"), true, &info),
            Ok(json!({ "name": "Logo refresh", "billable": true, "clientId": "c1" }))
        );
        assert_eq!(
            new_project_body("Album", None, Some(" CoolerBox Records "), false, &info),
            Ok(json!({ "name": "Album", "billable": false, "newClientName": "CoolerBox Records" }))
        );
        assert!(new_project_body("  ", Some("c1"), None, true, &info).is_err());
        assert!(new_project_body("Album", None, None, true, &info).is_err());
        assert!(new_project_body("Album", None, Some("X"), true, &info).is_err(), "too short a client name");
        assert!(new_project_body("Album", Some("someone-elses"), None, true, &info).is_err());
        // Clients come with the timer's read; odd ones are dropped.
        let read = parse(r#"{"projects":[],"running":null,"clients":[{"id":"c1","name":"Vimeo"},{"id":"bad id","name":"X"},{"id":"c2","name":" "}]}"#).unwrap();
        assert_eq!(read.clients, vec![Client { id: "c1".into(), name: "Vimeo".into() }]);
    }

    #[test]
    fn todays_list_reads_only_a_real_answer() {
        let body = r#"{"from":"2026-10-12","to":"2026-10-12","entries":[
            {"id":"e1","projectId":null,"projectName":null,"clientName":null,"billable":true,"billed":false,"startedAt":"2026-10-12T09:00:00.000Z","endedAt":null,"minutes":5,"note":null,"source":"DESKTOP"},
            {"id":"e2","projectId":"p1","projectName":"Logo refresh","clientName":"Vimeo","billable":true,"billed":false,"startedAt":"2026-10-12T07:00:00.000Z","endedAt":"2026-10-12T08:30:00.000Z","minutes":89.6,"note":"Grade","source":"WEB"},
            {"id":"bad id","startedAt":"x"}]}"#;
        let today = read_today(&json!({ "s": 200, "b": body }).to_string()).expect("a day");
        assert_eq!(today.day, "2026-10-12");
        assert_eq!(today.entries.len(), 2, "an id that could reach a script is dropped");
        assert!(!today.entries[0].billable, "unfiled is never billable");
        assert_eq!((today.total_minutes, today.billable_minutes), (95, 90));
        assert_eq!(read_today(&json!({ "s": 404, "b": "{}" }).to_string()), None, "not shipped yet");
        assert_eq!(read_today(&json!({ "s": 200, "b": "<html>" }).to_string()), None);
        assert_eq!(read_today(""), None);
    }

    #[test]
    fn edits_send_only_what_changed_and_never_end_the_running_one() {
        let info = info();
        let now = STARTED + 8 * 3600;
        let done = entry(Some("2026-10-12T08:30:00.000Z"));
        assert_eq!(
            entry_change(&done, None, Some(STARTED + 7200), Some("Grade"), Some("p1"), &info, now, None),
            Ok(Change { patch: Some(json!({ "endedAt": "2026-10-12T09:00:00.000Z" })), file_to: None })
        );
        assert_eq!(
            entry_change(&done, None, None, Some("  "), Some("p3"), &info, now, None),
            Ok(Change { patch: Some(json!({ "note": null })), file_to: Some("p3".into()) })
        );
        assert_eq!(entry_change(&done, None, None, None, Some("p3"), &info, now, None), Ok(Change { patch: None, file_to: Some("p3".into()) }));
        assert_eq!(entry_change(&done, Some(STARTED), None, Some("Grade"), None, &info, now, None), Err("Nothing changed."));
        assert!(entry_change(&done, Some(STARTED + 7200), None, None, None, &info, now, None).is_err(), "start after its end");
        assert!(entry_change(&done, None, Some(now + 600), None, None, &info, now, None).is_err(), "in the future");
        assert!(entry_change(&done, None, None, None, Some("someone-elses"), &info, now, None).is_err());
        let running = entry(None);
        assert!(entry_change(&running, None, Some(STARTED + 600), None, None, &info, now, None).is_err());
        assert_eq!(
            entry_change(&running, Some(STARTED - 600), None, None, None, &info, now, None),
            Ok(Change { patch: Some(json!({ "startedAt": "2026-10-12T06:50:00.000Z" })), file_to: None })
        );
        let mut billed = done;
        billed.billed = true;
        assert!(entry_change(&billed, None, None, Some("x"), None, &info, now, None).is_err());
    }

    #[test]
    fn requests_carry_their_method_and_a_delete_sends_no_body() {
        let js = post_script(&[("DELETE", "/api/me/project-time/e1".into(), Value::Null)], "");
        assert!(js.contains(r#""method":"DELETE""#));
        assert!(js.contains("if (steps[i].body !== null)"));
    }

    #[test]
    fn ids_are_plain() {
        for ok in ["p1", "cl9x0abc_12", "0b5e0a52-3c4d-4e8f-9a1b-2c3d4e5f6a7b"] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in ["", "a b", "x'y", "../x", &"x".repeat(65)] {
            assert!(!valid_id(bad), "{bad}");
        }
    }
}
