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
//!   ever ends time at that moment; it never adds time.
//! - Called in on the time sheet with no timer for half an hour: one gentle
//!   "Working on something?" banner that opens the panel.
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

const NOTE_MAX: usize = 200;

const START_JS: &str = r#"(function () {
  fetch('/api/me/hourly', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.ok ? r.text() : 'null'; })
    .then(function (t) { window.__cbbHourly = t; })
    .catch(function () { window.__cbbHourly = 'null'; });
})()"#;

const TAKE_JS: &str =
    "(function () { var v = window.__cbbHourly || ''; window.__cbbHourly = ''; return v; })()";

const TAKE_RESULT_JS: &str =
    "(function () { var v = window.__cbbHourlyResult || ''; window.__cbbHourlyResult = ''; return v; })()";

const TIMER_URL: &str = "/api/me/project-timer";
const FILE_URL: &str = "/api/me/project-time/file";

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
    pub billable: bool,
    pub started_at: String,
    #[serde(default)]
    pub note: Option<String>,
}

impl Running {
    pub(crate) fn unfiled(&self) -> bool {
        self.project_id.is_none()
    }

    /// The project's name, or "Not yet filed".
    pub(crate) fn label(&self) -> &str {
        self.project_name.as_deref().filter(|name| !name.is_empty()).unwrap_or("Not yet filed")
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
}

/// What `GET /api/me/hourly` says, or `None` for anything else (404 while it
/// isn't shipped, 403 for someone who doesn't bill by the hour, signed out).
pub(crate) fn parse(raw: &str) -> Option<Info> {
    let value: Value = serde_json::from_str(raw).ok()?;
    // Only a real answer: it always has the projects list.
    value.get("projects")?.as_array()?;
    let mut info: Info = serde_json::from_value(value).ok()?;
    info.projects.retain(|project| valid_id(&project.id));
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
    }
    running
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

/// The one-tap starts: the four projects timed most recently, leaving out
/// the one running now.
pub(crate) fn quick(info: &Info) -> Vec<Project> {
    let running = info.running.as_ref().and_then(|running| running.project_id.as_deref());
    info.projects.iter().filter(|project| Some(project.id.as_str()) != running).take(4).cloned().collect()
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
}

/// A note as it is sent: trimmed, one line, at most 200 characters.
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

/// A stop now, or at `at` (unix seconds: the moment idling began).
pub(crate) fn stop_body(at: Option<u64>) -> Value {
    match at {
        Some(at) => json!({ "action": "stop", "at": iso_utc(at) }),
        None => json!({ "action": "stop" }),
    }
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

/// Posts each `(url, body)` in turn through the page's own session,
/// stopping at the first refusal, and leaves `{ ok, running }` (the last
/// answer's) or `{ ok: false, error }` on `window.__cbbHourlyResult`.
/// `done` names what already happened when a later step is refused
/// ("Stopped at 09:40, but…").
pub(crate) fn post_script(steps: &[(&str, Value)], done: &str) -> String {
    let steps = Value::Array(steps.iter().map(|(url, body)| json!({ "url": url, "body": body })).collect());
    let done = Value::String(done.to_string());
    format!(
        r#"(function (steps, done) {{
  window.__cbbHourlyResult = '';
  function post(i, last) {{
    if (i >= steps.length) {{ window.__cbbHourlyResult = JSON.stringify({{ ok: true, running: last }}); return; }}
    fetch(steps[i].url, {{ method: 'POST', credentials: 'same-origin', headers: {{ 'Content-Type': 'application/json' }}, body: JSON.stringify(steps[i].body) }})
      .then(function (r) {{ return r.json().catch(function () {{ return {{}}; }}).then(function (b) {{ return {{ ok: r.ok, b: b || {{}} }}; }}); }})
      .then(function (x) {{
        if (!x.ok) {{
          var error = typeof x.b.error === 'string' && x.b.error ? x.b.error.slice(0, 300) : "That didn't save. Try again.";
          window.__cbbHourlyResult = JSON.stringify({{ ok: false, error: i > 0 && done ? done + ' ' + error : error }});
          return;
        }}
        post(i + 1, x.b.running || null);
      }})
      .catch(function () {{ window.__cbbHourlyResult = JSON.stringify({{ ok: false, error: 'You seem to be offline. Try again in a moment.' }}); }});
  }}
  post(0, null);
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
/// when idling began; "resume" does that and starts the same project (or an
/// unfiled timer) again now, with the same note, so the idle time is the
/// only gap. Time is only ever ended at the idle moment, never added.
pub(crate) fn idle_steps(choice: &str, ask: &IdleAsk) -> Result<Vec<Value>, &'static str> {
    let stop = stop_body(Some(ask.from));
    match choice {
        "keep" => Ok(Vec::new()),
        "stop" => Ok(vec![stop]),
        "resume" => {
            let mut start = json!({ "action": "start" });
            if let Some(project_id) = &ask.running.project_id {
                start["projectId"] = Value::String(project_id.clone());
            }
            if let Some(note) = clean_note(ask.running.note.as_deref()) {
                start["note"] = Value::String(note);
            }
            Ok(vec![stop, start])
        }
        _ => Err("Unknown choice."),
    }
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
    pub running: bool,
    /// `(menu id, label)` for the recent projects: "Switch to" or "Start".
    pub projects: Vec<(String, String)>,
}

pub(crate) const MENU_STOP: &str = "hourly-stop";
pub(crate) const MENU_LATER: &str = "hourly-later";
pub(crate) const MENU_HOURS: &str = "hourly-hours";
const MENU_START: &str = "hourly-start-";

pub(crate) fn menu_model(info: &Info, now: u64) -> MenuModel {
    let title = match &info.running {
        Some(running) => {
            let since = crate::clock::parse_iso(&running.started_at).unwrap_or(now);
            format!("{} · {}", running.label(), crate::clock::fmt_span(now.saturating_sub(since) / 60))
        }
        None => "Project timer".into(),
    };
    let projects = quick(info)
        .into_iter()
        .map(|project| (format!("{MENU_START}{}", project.id), menu_label(&project)))
        .collect();
    MenuModel { title, running: info.running.is_some(), projects }
}

fn menu_label(project: &Project) -> String {
    let name = if project.name.is_empty() { &project.project_number } else { &project.name };
    let name: String = name.chars().take(48).collect();
    format!("{name} ({})", if project.billable { "Billable" } else { "Tracked only" })
}

/// The project a menu id starts, if it's one of those menu ids.
pub(crate) fn menu_project(id: &str) -> Option<&str> {
    id.strip_prefix(MENU_START).filter(|project| valid_id(project))
}

pub(crate) fn is_menu_id(id: &str) -> bool {
    matches!(id, MENU_STOP | MENU_LATER | MENU_HOURS) || menu_project(id).is_some()
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
    let model = with(app, |inner| inner.info.as_ref().map(|info| menu_model(info, now))).flatten();
    if let Ok(mut dock) = DOCK.lock() {
        dock.clone_from(&model);
    }
    let Some(tray) = app.try_state::<TrayMenu<R>>() else { return };
    let Ok(mut shown) = tray.shown.lock() else { return };
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
        let items_changed = shown.as_ref().is_none_or(|old| old.running != model.running || old.projects != model.projects);
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
    sub.append(&PredefinedMenuItem::separator(app)?)?;
    sub.append(&MenuItem::with_id(app, MENU_HOURS, "Open My Hours", true, None::<&str>)?)?;
    Ok(())
}

/// A click on one of the timer's tray or Dock items.
pub(crate) fn menu_action<R: Runtime>(app: &AppHandle<R>, id: &str) {
    let result = match id {
        MENU_STOP => stop(app, "menu"),
        MENU_LATER => start(app, None, None, "menu"),
        MENU_HOURS => {
            crate::mini::open_href(app, "/hours");
            crate::analytics::action(app, "hourly_open_hours", "menu");
            Ok(())
        }
        id => match menu_project(id) {
            Some(project) => start(app, Some(project), None, "menu"),
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
    with(app, |inner| {
        let info = inner.info.as_ref()?;
        Some(PanelView {
            running: info.running.clone(),
            quick: quick(info),
            projects: info.projects.clone(),
            asking: inner.ask.is_some(),
            filing: inner.to_file.is_some(),
            idle_minutes,
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
        }
        inner.info = info;
        inner.read_at = now;
    });
    refresh_menus(app);
    crate::mini::push(app);
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
    });
    refresh_menus(app);
    crate::mini::push(app);
}

fn read<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = crate::walkie::tracker_window(app) else { return };
    let _ = window.eval(START_JS);
    if let Some(raw) = crate::walkie::collect(&window, TAKE_JS, 30) {
        set_info(app, parse(&raw));
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
    },
    /// Filed: nothing more to ask.
    Filed,
}

/// Sends `steps` through the page and hands the outcome on.
fn send<R: Runtime>(app: &AppHandle<R>, steps: Vec<(&'static str, Value)>, done: &str, then: Then) -> Result<(), String> {
    let window = crate::walkie::tracker_window(app).ok_or("Open the tracker first.")?;
    let script = post_script(&steps, done);
    let app = app.clone();
    std::thread::spawn(move || {
        let _one = SENDING.lock();
        if window.eval(&script).is_err() {
            return panel_error(&app, "The tracker could not be reached.");
        }
        let outcome = crate::walkie::collect(&window, TAKE_RESULT_JS, 40)
            .and_then(|raw| serde_json::from_str::<Outcome>(&raw).ok());
        match (outcome, then) {
            (Some(Outcome { ok: true, running, .. }), Then::Timer { file, notice }) => {
                set_running(&app, running.filter(valid_running).map(unfiled_not_billable));
                // The tracker's page shows the same timer: let it catch up.
                let _ = window.eval("window.dispatchEvent(new CustomEvent('cbb:hourly-changed'))");
                if file.is_some() || notice.is_some() {
                    if file.is_some() {
                        with(&app, |inner| inner.to_file.clone_from(&file));
                    }
                    crate::mini::push(&app);
                    let after = json!({ "file": file.is_some(), "notice": notice });
                    crate::mini::ask_hourly(&app, "__cbbMiniHourlyAfter", &after);
                }
            }
            (Some(Outcome { ok: true, .. }), Then::Filed) => {
                with(&app, |inner| inner.to_file = None);
                crate::mini::push(&app);
                let _ = window.eval("window.dispatchEvent(new CustomEvent('cbb:hourly-changed'))");
            }
            (other, _) => {
                let error = other
                    .and_then(|outcome| outcome.error)
                    .unwrap_or_else(|| "The tracker could not be reached.".into());
                panel_error(&app, &error);
                read(&app);
            }
        }
    });
    Ok(())
}

fn panel_error<R: Runtime>(app: &AppHandle<R>, error: &str) {
    if let Some(mini) = app.get_webview_window("mini") {
        let text = Value::String(error.to_string());
        let _ = mini.eval(format!("window.__cbbMiniHourlyError && window.__cbbMiniHourlyError({text})"));
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
fn start<R: Runtime>(app: &AppHandle<R>, project_id: Option<&str>, note: Option<&str>, from: &'static str) -> Result<(), String> {
    let info = current(app)?;
    let body = start_body(project_id, note, &info).map_err(str::to_string)?;
    send(app, vec![(TIMER_URL, body)], "", Then::Timer { file: ends_unfiled(&info), notice: None })?;
    let what = match (info.running.is_some(), project_id.is_some()) {
        (true, _) => "hourly_switch",
        (false, false) => "hourly_start_unfiled",
        (false, true) => "hourly_start",
    };
    crate::analytics::action(app, what, from);
    Ok(())
}

fn stop<R: Runtime>(app: &AppHandle<R>, from: &'static str) -> Result<(), String> {
    let info = current(app)?;
    if info.running.is_none() {
        return Err("No timer is running.".into());
    }
    send(app, vec![(TIMER_URL, stop_body(None))], "", Then::Timer { file: ends_unfiled(&info), notice: None })?;
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
) -> Result<(), String> {
    match action.as_str() {
        "start" => start(&app, project_id.as_deref(), note.as_deref(), "panel"),
        "stop" => stop(&app, "panel"),
        _ => Err("Unknown action.".into()),
    }
}

/// The answer to "Keep it / Drop the idle time and keep going / Drop it
/// and stop" (`keep`, `resume`, `stop`).
#[tauri::command]
pub(crate) fn mini_hourly_idle<R: Runtime>(app: AppHandle<R>, choice: String) -> Result<(), String> {
    let ask = with(&app, |inner| inner.ask.take()).flatten().ok_or("That timer has already stopped.")?;
    let steps = idle_steps(&choice, &ask).map_err(str::to_string)?;
    crate::mini::push(&app);
    if !steps.is_empty() {
        let left = crate::clock::fmt_hhmm_sast(ask.from);
        // The unfiled stretch up to the idle moment is filed next, whether
        // the timer stays stopped or starts again.
        let file = ask.running.unfiled().then(|| ask.running.id.clone());
        let steps = steps.into_iter().map(|body| (TIMER_URL, body)).collect();
        send(&app, steps, &format!("Stopped at {left}, but it didn't start again:"), Then::Timer { file, notice: None })?;
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
            send(&app, vec![(FILE_URL, body)], "", Then::Filed)?;
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
    let notice = format!(
        "You were away, so your timer on {} stopped at {}, when you went idle.",
        running.label(),
        crate::clock::fmt_hhmm_sast(at)
    );
    let file = running.unfiled().then(|| running.id.clone());
    let then = if back {
        Then::Timer { file, notice: Some(notice) }
    } else {
        with(app, |inner| {
            inner.notice = Some(notice);
            inner.ask = None;
            if file.is_some() {
                inner.to_file.clone_from(&file);
            }
        });
        Then::Timer { file: None, notice: None }
    };
    let _ = send(app, vec![(TIMER_URL, stop_body(Some(at)))], "", then);
    crate::analytics::action(app, "hourly_idle_auto_stop", "idle");
}

/// Back from idle time (idle.rs).
pub(crate) fn on_away<R: Runtime>(app: &AppHandle<R>, away: crate::idle::Away) {
    // Stopped while they were away: say so, and ask for its project if it
    // wasn't filed.
    let (notice, filing) = with(app, |inner| (inner.notice.take(), inner.to_file.is_some())).unwrap_or((None, false));
    if let Some(notice) = notice {
        crate::mini::ask_hourly(app, "__cbbMiniHourlyAfter", &json!({ "file": filing, "notice": notice }));
        return;
    }
    let running = with(app, |inner| inner.info.as_ref().and_then(|info| info.running.clone())).flatten();
    let Some(running) = running else { return };
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
            let question = json!({
                "project": running.label(),
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
    if nudge && crate::quiet::allowed(app, crate::quiet::Kind::Tracker) {
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
                read(&app);
                maybe_nudge(&app);
            } else {
                with(&app, |inner| *inner = Inner::default());
                refresh_menus(&app);
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
        let ids: Vec<String> = quick(&info()).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["p1", "p3", "p4", "p5"]);
        let mut stopped = info();
        stopped.running = None;
        let ids: Vec<String> = quick(&stopped).into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["p1", "p2", "p3", "p4"]);
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
        let long = "x".repeat(500);
        let body = start_body(Some("p1"), Some(&long), &info).unwrap();
        assert_eq!(body["note"].as_str().map(str::len), Some(200));
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
        let js = post_script(&[(TIMER_URL, json!({ "action": "stop" })), (FILE_URL, json!({ "entryId": "t", "projectId": "p" }))], "Stopped.");
        assert!(js.contains("fetch(steps[i].url, { method: 'POST', credentials: 'same-origin'"));
        assert!(js.ends_with(
            r#"})([{"body":{"action":"stop"},"url":"/api/me/project-timer"},{"body":{"entryId":"t","projectId":"p"},"url":"/api/me/project-time/file"}], "Stopped.")"#
        ));
        assert!(js.contains("if (!x.ok)"));
        assert!(!js.contains("localStorage") && !js.contains("console."));
    }

    #[test]
    fn outcomes_read_both_ways() {
        let ok: Outcome = serde_json::from_str(r#"{"ok":true,"running":null}"#).unwrap();
        assert_eq!(ok, Outcome { ok: true, running: None, error: None });
        let refused: Outcome = serde_json::from_str(r#"{"ok":false,"error":"That project is closed."}"#).unwrap();
        assert_eq!(refused.error.as_deref(), Some("That project is closed."));
    }

    #[test]
    fn an_unfiled_timer_is_never_billable() {
        let raw = r#"{"projects":[],"running":{"id":"t9","projectId":null,"projectName":"x","billable":true,"startedAt":"2026-10-12T07:00:00.000Z","note":null}}"#;
        let running = parse(raw).and_then(|info| info.running).expect("an unfiled timer");
        assert!(running.unfiled());
        assert!(!running.billable);
        assert_eq!(running.label(), "Not yet filed");
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
    fn answers_only_ever_end_time_at_the_idle_moment() {
        let ask = IdleAsk { running: info().running.unwrap(), from: STARTED + 9600, to: STARTED + 11_400 };
        assert_eq!(idle_steps("keep", &ask), Ok(vec![]));
        assert_eq!(idle_steps("stop", &ask), Ok(vec![json!({ "action": "stop", "at": "2026-10-12T09:40:00.000Z" })]));
        assert_eq!(
            idle_steps("resume", &ask),
            Ok(vec![
                json!({ "action": "stop", "at": "2026-10-12T09:40:00.000Z" }),
                json!({ "action": "start", "projectId": "p2", "note": "Grade" }),
            ])
        );
        assert!(idle_steps("delete", &ask).is_err());
        // An unfiled timer resumes unfiled.
        let mut unfiled = ask.clone();
        unfiled.running.project_id = None;
        unfiled.running.note = None;
        assert_eq!(idle_steps("resume", &unfiled).unwrap()[1], json!({ "action": "start" }));
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
        let model = menu_model(&info(), STARTED + 65 * 60);
        assert_eq!(model.title, "Brand film · 1h 05m");
        assert!(model.running);
        assert_eq!(model.projects[0], ("hourly-start-p1".to_string(), "Con Spirito S1 (Billable)".to_string()));
        assert_eq!(model.projects.len(), 4);
        let mut stopped = info();
        stopped.running = None;
        assert_eq!(menu_model(&stopped, STARTED).title, "Project timer");
        let mut unfiled = info();
        unfiled.running.as_mut().unwrap().project_id = None;
        unfiled.running.as_mut().unwrap().project_name = None;
        assert_eq!(menu_model(&unfiled, STARTED + 600).title, "Not yet filed · 10m");
        for id in [MENU_STOP, MENU_LATER, MENU_HOURS, "hourly-start-p1"] {
            assert!(is_menu_id(id), "{id}");
        }
        assert_eq!(menu_project("hourly-start-p1"), Some("p1"));
        for id in ["hourly-start-", "hourly-start-a b", "hourly-delete", "tray-quit"] {
            assert!(!is_menu_id(id), "{id}");
        }
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
