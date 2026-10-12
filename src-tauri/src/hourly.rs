//! The project timer, for the few people an admin set to bill by the hour.
//! Only they get it, and only for their own time. They can time any project
//! they're on; the project decides the billing (an hourly project is
//! billable, a quoted one is tracked only), so nobody picks it per entry.
//!
//! - The quick panel shows the running timer (project, Billable / Tracked,
//!   elapsed, Stop) or one-tap starts for the projects timed most recently,
//!   plus "Other…". Starting another project while one runs switches.
//! - The menu bar shows the timer's elapsed time while it runs.
//! - Back from time away with a timer running, the panel asks: keep it, cut
//!   the time away, or stop when you left? It never decides.
//! - Called in on the time sheet with no timer for half an hour: one gentle
//!   "Working on something?" banner that opens the panel.
//! - "Just start, file it later" starts a timer with no project ("Not yet
//!   filed", never billable). When it ends (stopped, or switched to a
//!   project) the tracker's Hours page opens on it to file it: filing
//!   happens on the website.
//!
//! The timer also starts and stops on the website (the board's ▶, the
//! header), so it is read every 90 s, on opening the panel and when the
//! tracker window comes forward.
//!
//! All of it reads the tracker's `GET /api/me/hourly`; while that answers
//! 404 (not shipped) or 403 (not hourly), everything here stays hidden and
//! quiet. As elsewhere the page asks with its own session (`eval`), the
//! answer is collected through a window variable, and the shell holds no
//! credentials. Nothing here is stored on the computer.

use std::{sync::Mutex, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

/// Evert's brief: every 60–120 s.
const POLL_EVERY: Duration = Duration::from_secs(90);
/// The first read, once the tracker has had time to load after launch.
const FIRST_AFTER: Duration = Duration::from_secs(30);
/// Opening the panel reads again when the last read is older than this.
const FRESH_FOR_SECS: u64 = 5;

/// Time away shorter than this doesn't ask about the timer.
pub(crate) const IDLE_ASK_SECS: u64 = 10 * 60;
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
    info.running = info.running.filter(valid_running).map(|mut running| {
        // Not yet filed is never billable, whatever the answer says.
        if running.project_id.is_none() {
            running.billable = false;
            running.project_name = None;
        }
        running
    });
    Some(info)
}

fn valid_running(running: &Running) -> bool {
    valid_id(&running.id) && running.project_id.as_deref().is_none_or(valid_id)
}

impl Running {
    pub(crate) fn unfiled(&self) -> bool {
        self.project_id.is_none()
    }
}

/// The tracker's Hours page, open on an unfiled stretch to file it.
pub(crate) fn file_href(entry_id: &str) -> Option<String> {
    valid_id(entry_id).then(|| format!("/hours?entry={entry_id}&panel=file"))
}

/// The tracker's ids (uuid or cuid): nothing that could reach a script.
pub(crate) fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

/// The one-tap starts: the four projects timed most recently, leaving out
/// the one running now.
pub(crate) fn quick(info: &Info) -> Vec<Project> {
    let running = info.running.as_ref().and_then(|running| running.project_id.as_deref());
    info.projects.iter().filter(|project| Some(project.id.as_str()) != running).take(4).cloned().collect()
}

/// What the panel shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PanelView {
    pub running: Option<Running>,
    pub quick: Vec<Project>,
    /// Every project they can time, for "Other…".
    pub projects: Vec<Project>,
    /// A question about time away is waiting for an answer.
    pub asking: bool,
}

pub(crate) fn panel_view(info: &Info, asking: bool) -> PanelView {
    PanelView { running: info.running.clone(), quick: quick(info), projects: info.projects.clone(), asking }
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

/// The body for `POST /api/me/project-timer`, built only from what the
/// tracker takes. A start names one of the person's own projects, or none
/// ("Just start, file it later").
pub(crate) fn timer_body(action: &str, project_id: Option<&str>, note: Option<&str>, info: &Info) -> Result<Value, &'static str> {
    match action {
        "start" => {
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
        "stop" => Ok(json!({ "action": "stop" })),
        _ => Err("Unknown action."),
    }
}

/// Posts each body in turn through the page's own session, stopping at the
/// first refusal, and leaves `{ ok, running }` or `{ ok: false, error }` on
/// `window.__cbbHourlyResult`. `done` names what already happened when a
/// later step is refused ("Stopped at 09:40, but…").
pub(crate) fn timer_script(steps: &[Value], done: &str) -> String {
    let steps = Value::Array(steps.to_vec());
    let done = Value::String(done.to_string());
    format!(
        r#"(function (steps, done) {{
  window.__cbbHourlyResult = '';
  function post(i, last) {{
    if (i >= steps.length) {{ window.__cbbHourlyResult = JSON.stringify({{ ok: true, running: last }}); return; }}
    fetch('/api/me/project-timer', {{ method: 'POST', credentials: 'same-origin', headers: {{ 'Content-Type': 'application/json' }}, body: JSON.stringify(steps[i]) }})
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

/// The result of a timer request, as the page left it.
#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct Outcome {
    pub ok: bool,
    #[serde(default)]
    pub running: Option<Running>,
    #[serde(default)]
    pub error: Option<String>,
}

// ── coming back from time away with a timer running ──

/// A question waiting in the panel: the timer that was running, and the
/// time away (unix seconds, `from` no earlier than the timer's start).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IdleAsk {
    pub running: Running,
    pub from: u64,
    pub to: u64,
}

/// Whether time away should ask about `running`: long enough, and the timer
/// was already running when they left (one started meanwhile, say from a
/// phone, isn't theirs to cut).
pub(crate) fn idle_ask(running: Option<&Running>, away: crate::idle::Away) -> Option<IdleAsk> {
    let running = running?;
    let started = crate::clock::parse_iso(&running.started_at)?;
    if away.to.saturating_sub(away.from) < IDLE_ASK_SECS || started > away.from {
        return None;
    }
    Some(IdleAsk { running: running.clone(), from: away.from, to: away.to })
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

/// The requests for an answer: "keep" sends nothing; "stop" stops the timer
/// when they left; "cut" does that and starts the same project again now,
/// with the same note, so the time away is the only gap.
pub(crate) fn idle_steps(choice: &str, ask: &IdleAsk) -> Result<Vec<Value>, &'static str> {
    let stop = json!({ "action": "stop", "endedAt": iso_utc(ask.from) });
    match choice {
        "keep" => Ok(Vec::new()),
        "stop" => Ok(vec![stop]),
        "cut" => {
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

// ── state ──

#[derive(Default)]
struct Inner {
    info: Option<Info>,
    read_at: u64,
    ask: Option<IdleAsk>,
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

/// The panel's view, or `None` (hidden) for someone without a timer.
pub(crate) fn panel<R: Runtime>(app: &AppHandle<R>) -> Option<PanelView> {
    with(app, |inner| inner.info.as_ref().map(|info| panel_view(info, inner.ask.is_some()))).flatten()
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
        inner.info = info;
        inner.read_at = now;
    });
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
    crate::mini::push(app);
}

fn read<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = crate::walkie::tracker_window(app) else { return };
    let _ = window.eval(START_JS);
    if let Some(raw) = crate::walkie::collect(&window, TAKE_JS, 30) {
        set_info(app, parse(&raw));
    }
}

/// Read again soon if the last read is stale (the panel was opened).
pub(crate) fn refresh_if_stale<R: Runtime>(app: &AppHandle<R>) {
    let now = crate::clock::unix_now();
    let stale = with(app, |inner| now.saturating_sub(inner.read_at) > FRESH_FOR_SECS).unwrap_or(false);
    if stale && crate::session::signed_in() {
        let app = app.clone();
        std::thread::spawn(move || read(&app));
    }
}

/// Sends `steps` and hands the outcome to the state and the panel. When it
/// worked and `file` names an unfiled stretch that just ended, the Hours
/// page opens on it to be filed.
fn send<R: Runtime>(app: &AppHandle<R>, steps: Vec<Value>, done: &str, file: Option<String>) -> Result<(), String> {
    let window = crate::walkie::tracker_window(app).ok_or("Open the tracker first.")?;
    window
        .eval(timer_script(&steps, done))
        .map_err(|_| "The tracker could not be reached.".to_string())?;
    let app = app.clone();
    std::thread::spawn(move || {
        let outcome = crate::walkie::collect(&window, TAKE_RESULT_JS, 40)
            .and_then(|raw| serde_json::from_str::<Outcome>(&raw).ok());
        match outcome {
            Some(Outcome { ok: true, running, .. }) => {
                let running = running.filter(valid_running);
                set_running(&app, running);
                if let Some(href) = file.as_deref().and_then(file_href) {
                    let app_for_main = app.clone();
                    let _ = app.run_on_main_thread(move || crate::mini::open_href(&app_for_main, &href));
                    crate::analytics::action(&app, "hourly_file", "panel");
                }
                // The tracker's page shows the same timer: let it catch up.
                if let Some(window) = crate::walkie::tracker_window(&app) {
                    let _ = window.eval("window.dispatchEvent(new CustomEvent('cbb:hourly-changed'))");
                }
            }
            other => {
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

/// Start (or switch to) a project, or stop, from the panel.
#[tauri::command]
pub(crate) fn mini_hourly<R: Runtime>(
    app: AppHandle<R>,
    action: String,
    project_id: Option<String>,
    note: Option<String>,
) -> Result<(), String> {
    let info = with(&app, |inner| inner.info.clone()).flatten().ok_or("The project timer isn't on for you.")?;
    let body = timer_body(&action, project_id.as_deref(), note.as_deref(), &info).map_err(str::to_string)?;
    // An unfiled timer that ends here (stopped, or switched to a project)
    // is filed next, on the Hours page.
    let file = info.running.as_ref().filter(|running| running.unfiled()).map(|running| running.id.clone());
    send(&app, vec![body], "", file)?;
    let what = match (action.as_str(), info.running.is_some(), project_id.is_some()) {
        ("start", true, _) => "hourly_switch",
        ("start", false, false) => "hourly_start_unfiled",
        ("start", false, true) => "hourly_start",
        _ => "hourly_stop",
    };
    crate::analytics::action(&app, what, "panel");
    Ok(())
}

/// The answer to "Keep, cut the time away, or stop when you left?".
#[tauri::command]
pub(crate) fn mini_hourly_idle<R: Runtime>(app: AppHandle<R>, choice: String) -> Result<(), String> {
    let ask = with(&app, |inner| inner.ask.take()).flatten().ok_or("That timer has already stopped.")?;
    let steps = idle_steps(&choice, &ask).map_err(str::to_string)?;
    if !steps.is_empty() {
        let left = crate::clock::fmt_hhmm_sast(ask.from);
        // Stopped for good, an unfiled stretch is filed next; cut, the
        // person is back at work and files it later from Hours.
        let file = (choice == "stop" && ask.running.unfiled()).then(|| ask.running.id.clone());
        send(&app, steps, &format!("Stopped at {left}, but it didn't start again:"), file)?;
    }
    let what = match choice.as_str() {
        "cut" => "hourly_idle_cut",
        "stop" => "hourly_idle_stop",
        _ => "hourly_idle_keep",
    };
    crate::analytics::action(&app, what, "panel");
    Ok(())
}

/// Time away ended (idle.rs): with a timer running since before they left,
/// the panel asks what to do with it.
pub(crate) fn on_away<R: Runtime>(app: &AppHandle<R>, away: crate::idle::Away) {
    let running = with(app, |inner| inner.info.as_ref().and_then(|info| info.running.clone())).flatten();
    let Some(ask) = idle_ask(running.as_ref(), away) else { return };
    with(app, |inner| inner.ask = Some(ask.clone()));
    let question = json!({
        "project": ask.running.project_name.as_deref().unwrap_or("Not yet filed"),
        "unfiled": ask.running.unfiled(),
        "billable": ask.running.billable,
        "from": ask.from * 1000,
        "to": ask.to * 1000,
    });
    crate::mini::ask_hourly_idle(app, &question);
}

fn maybe_nudge<R: Runtime>(app: &AppHandle<R>) {
    let now = crate::clock::unix_now();
    let in_since = app
        .try_state::<crate::clock::Last>()
        .and_then(|last| last.0.lock().ok().and_then(|slot| slot.clone()))
        .filter(|clock| clock.available && clock.state == "in")
        .and_then(|clock| crate::clock::parse_iso(&clock.since_iso));
    let nudge = with(app, |inner| {
        let timing = inner.info.as_ref().is_some_and(|info| info.running.is_some());
        let eligible = inner.info.is_some() && !timing;
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
pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_AFTER);
        loop {
            if crate::session::signed_in() {
                read(&app);
                maybe_nudge(&app);
            } else {
                with(&app, |inner| *inner = Inner::default());
            }
            std::thread::sleep(POLL_EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idle::Away;

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
    fn starts_name_one_of_your_projects_and_carry_only_what_the_tracker_takes() {
        let info = info();
        assert_eq!(
            timer_body("start", Some("p1"), Some("  Edit\nv2  "), &info),
            Ok(json!({ "action": "start", "projectId": "p1", "note": "Edit v2" }))
        );
        assert_eq!(timer_body("start", Some("p1"), Some("   "), &info), Ok(json!({ "action": "start", "projectId": "p1" })));
        assert_eq!(timer_body("stop", Some("p1"), Some("x"), &info), Ok(json!({ "action": "stop" })));
        assert!(timer_body("start", Some("someone-elses"), None, &info).is_err());
        // Just start, file it later.
        assert_eq!(timer_body("start", None, Some("Call"), &info), Ok(json!({ "action": "start", "note": "Call" })));
        assert!(timer_body("delete", Some("p1"), None, &info).is_err());
        let long = "x".repeat(500);
        let body = timer_body("start", Some("p1"), Some(&long), &info).unwrap();
        assert_eq!(body["note"].as_str().map(str::len), Some(200));
    }

    #[test]
    fn the_script_posts_each_step_to_the_tracker_and_stops_at_a_refusal() {
        let js = timer_script(&[json!({ "action": "stop" }), json!({ "action": "start", "projectId": "p1" })], "Stopped.");
        assert!(js.contains("fetch('/api/me/project-timer', { method: 'POST'"));
        assert!(js.ends_with(r#"})([{"action":"stop"},{"action":"start","projectId":"p1"}], "Stopped.")"#));
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

    // 2026-10-12T07:00:00Z, when the running timer started.
    const STARTED: u64 = 1_791_788_400;

    #[test]
    fn time_away_asks_only_about_a_timer_running_before_you_left() {
        let info = info();
        let running = info.running.as_ref();
        let long = Away { from: STARTED + 3600, to: STARTED + 3600 + 25 * 60 };
        let ask = idle_ask(running, long).expect("asks");
        assert_eq!((ask.from, ask.to), (long.from, long.to));
        // Too short.
        assert_eq!(idle_ask(running, Away { from: STARTED + 3600, to: STARTED + 3600 + 9 * 60 }), None);
        // The timer started while they were away (from a phone, say).
        assert_eq!(idle_ask(running, Away { from: STARTED - 600, to: STARTED + 1200 }), None);
        // No timer.
        assert_eq!(idle_ask(None, long), None);
    }

    #[test]
    fn answers_become_the_trackers_requests() {
        let ask = IdleAsk { running: info().running.unwrap(), from: STARTED + 9600, to: STARTED + 11_400 };
        assert_eq!(idle_steps("keep", &ask), Ok(vec![]));
        assert_eq!(idle_steps("stop", &ask), Ok(vec![json!({ "action": "stop", "endedAt": "2026-10-12T09:40:00.000Z" })]));
        assert_eq!(
            idle_steps("cut", &ask),
            Ok(vec![
                json!({ "action": "stop", "endedAt": "2026-10-12T09:40:00.000Z" }),
                json!({ "action": "start", "projectId": "p2", "note": "Grade" }),
            ])
        );
        assert!(idle_steps("delete", &ask).is_err());
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
        // Not called in.
        assert!(!should_nudge(None, 0, 0, since + 7200));
        // 29 minutes in.
        assert!(!should_nudge(Some(since), 0, 0, since + 29 * 60));
        assert!(should_nudge(Some(since), 0, 0, since + 30 * 60));
        // Already nudged this stretch.
        assert!(!should_nudge(Some(since), 0, since, since + 7200));
        // A timer stopped 10 minutes ago.
        assert!(!should_nudge(Some(since), since + 7200 - 600, 0, since + 7200));
        assert!(should_nudge(Some(since), since + 3600, 0, since + 3600 + 1800));
    }

    #[test]
    fn an_unfiled_timer_is_never_billable_and_files_on_hours() {
        let raw = r#"{"projects":[],"running":{"id":"t9","projectId":null,"projectName":null,"billable":true,"startedAt":"2026-10-12T07:00:00.000Z","note":null}}"#;
        let running = parse(raw).and_then(|info| info.running).expect("an unfiled timer");
        assert!(running.unfiled());
        assert!(!running.billable);
        assert_eq!(file_href(&running.id).as_deref(), Some("/hours?entry=t9&panel=file"));
        assert_eq!(file_href("x&panel=evil"), None);
        // A bad project id drops the timer rather than showing it as unfiled.
        let bad = r#"{"projects":[],"running":{"id":"t9","projectId":"a b","startedAt":"2026-10-12T07:00:00.000Z"}}"#;
        assert_eq!(parse(bad).and_then(|info| info.running), None);
        // Cut restarts it unfiled too.
        let ask = IdleAsk { running, from: STARTED + 600, to: STARTED + 1800 };
        assert_eq!(
            idle_steps("cut", &ask).unwrap()[1],
            json!({ "action": "start" })
        );
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
