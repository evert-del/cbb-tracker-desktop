//! The time sheet's clock in the tray: Call in / Break / Back from break /
//! Wrap from the menu, under a first line saying where you are ("Time sheet:
//! Called in since 08:02"). The tooltip stays notify.rs's (the unread count).
//!
//! Like notify.rs the shell holds no credentials and the remote window gets
//! no IPC: Rust asks the *page* to fetch the tracker's own
//! `/api/time-sheet/clock` with its session cookie (`eval`), and collects the
//! answer on the next tick through a window variable. A clock from the tray
//! posts to the same route the page's clock uses, then tells the page
//! (`cbb:clock-changed`) so its own clock catches up. Someone who has not
//! read the time sheet notice yet is shown the tracker's clock instead: the
//! notice is read there, never skipped from the tray.

use std::{sync::Mutex, time::Duration};

use serde::Deserialize;
use tauri::{menu::MenuItem, AppHandle, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;

const POLL_EVERY: Duration = Duration::from_secs(30);

/// Starts a fetch of the clock; the result lands in `window.__cbbClock`
/// ("null" on any failure, e.g. signed out). The label is made in the page,
/// in Johannesburg time, so the shell never formats a time zone itself.
/// `sinceIso` (raw ISO timestamp, "" when off the clock) lets the mini panel
/// tick the elapsed time live without polling.
const START_JS: &str = r#"(function () {
  fetch('/api/time-sheet/clock', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.ok ? r.json() : null; })
    .then(function (c) {
      if (!c) { window.__cbbClock = 'null'; return; }
      var t = c.since ? new Date(c.since).toLocaleTimeString('en-GB', { timeZone: 'Africa/Johannesburg', hour: '2-digit', minute: '2-digit' }) : '';
      window.__cbbClock = JSON.stringify({ available: !!c.available, state: c.state || 'out', since: t, sinceIso: c.since || '', today: c.today || [], notice: !!c.notice });
    })
    .catch(function () { window.__cbbClock = 'null'; });
})()"#;

/// Returns and clears the last result ("" when none is waiting).
const TAKE_JS: &str =
    "(function () { var v = window.__cbbClock || ''; window.__cbbClock = ''; return v; })()";

/// Returns and clears the last tray clock's error ("" when none).
const TAKE_ERROR_JS: &str =
    "(function () { var v = window.__cbbClockError || ''; window.__cbbClockError = ''; return v; })()";

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Clock {
    pub available: bool,
    pub state: String,
    /// "08:02", or "" when off the clock.
    pub since: String,
    /// Raw ISO timestamp of the same moment, or "" when off the clock.
    #[serde(default)]
    pub since_iso: String,
    /// Today's segments; the shell totals them for the wrap-up note.
    #[serde(default)]
    pub today: Vec<DaySegment>,
    /// The person has not read the current notice.
    pub notice: bool,
}

/// One worked/break stretch of today. `ended_at` is null while open.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DaySegment {
    pub kind: String,
    pub started_at: String,
    pub ended_at: Option<String>,
}

/// What the tray shows for a clock.
#[derive(Debug, PartialEq)]
pub(crate) struct Plan {
    pub status: String,
    pub call_in: bool,
    pub take_break: bool,
    pub back: bool,
    pub wrap: bool,
}

pub(crate) fn parse(raw: &str) -> Option<Clock> {
    serde_json::from_str(raw).ok()
}

pub(crate) fn plan(clock: Option<&Clock>) -> Plan {
    let Some(clock) = clock.filter(|clock| clock.available) else {
        return Plan {
            status: "Time sheet: not in use".into(),
            call_in: false,
            take_break: false,
            back: false,
            wrap: false,
        };
    };
    let since = if clock.since.is_empty() { String::new() } else { format!(" since {}", clock.since) };
    let (status, call_in, take_break, back, wrap) = match clock.state.as_str() {
        "in" => (format!("Called in{since}"), false, true, false, true),
        "break" => (format!("On a break{since}"), false, false, true, true),
        _ => ("Off the clock".to_string(), true, false, false, false),
    };
    Plan {
        status: format!("Time sheet: {status}"),
        call_in,
        take_break,
        back,
        wrap,
    }
}

/// The script that clocks from the tray, through the page's own session.
/// After a call in or wrap the clock answers with the card; when the company
/// asks how people are (`mood`) and this moment hasn't been asked today, it
/// leaves `{moment}` on `window.__cbbMoodAsk` for `take_mood_ask`,
/// and the quick panel asks (mini.rs). Only these taps ever ask: the
/// person's own, never an idle-prompt answer.
pub(crate) fn action_script(action: &str) -> String {
    format!(
        r#"(function () {{
  fetch('/api/time-sheet/clock', {{ method: 'POST', credentials: 'same-origin', headers: {{ 'Content-Type': 'application/json' }},
    body: JSON.stringify({{ action: {action:?}, source: 'DESKTOP' }}) }})
    .then(function (r) {{ return r.json().then(function (b) {{ return {{ ok: r.ok, b: b }}; }}); }})
    .then(function (x) {{
      if (!x.ok) {{ window.__cbbClockError = (x.b && x.b.error) || 'That did not save.'; return; }}
      var moment = {action:?} === 'in' ? 'IN' : {action:?} === 'wrap' ? 'WRAP' : null;
      var mood = x.b && x.b.mood;
      if (moment && mood && mood.askedKey !== mood.today + '|' + moment) {{
        window.__cbbMoodAsk = JSON.stringify({{ moment: moment }});
      }}
      window.dispatchEvent(new CustomEvent('cbb:clock-changed'));
    }})
    .catch(function () {{ window.__cbbClockError = 'The tracker could not be reached.'; }});
}})()"#
    )
}

/// Returns and clears a pending mood question ("" when none).
const TAKE_MOOD_ASK_JS: &str =
    "(function () { var v = window.__cbbMoodAsk || ''; window.__cbbMoodAsk = ''; return v; })()";

/// A mood question the last clock tap left (see `action_script`).
#[derive(Debug, serde::Deserialize, PartialEq)]
pub(crate) struct MoodAsk {
    pub moment: String,
}

/// Picks up a pending mood question and has the quick panel ask it.
fn take_mood_ask<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = on_tracker(app) else { return };
    let app = app.clone();
    let _ = window.eval_with_callback(TAKE_MOOD_ASK_JS, move |raw| {
        let json: String = serde_json::from_str(&raw).unwrap_or_default();
        if let Ok(ask) = serde_json::from_str::<MoodAsk>(&json) {
            if ask.moment == "IN" || ask.moment == "WRAP" {
                crate::mini::ask_mood(&app, &ask.moment);
            }
        }
    });
}

/// Opens the tracker's own clock (for the notice, which is read there).
const OPEN_CLOCK_JS: &str = "(function () { var t = document.querySelector('.gd-clock-trigger'); if (t) t.click(); })()";

/// The tray's clock items, kept so each poll can update them.
pub(crate) struct Items<R: Runtime> {
    pub status: MenuItem<R>,
    pub call_in: MenuItem<R>,
    pub take_break: MenuItem<R>,
    pub back: MenuItem<R>,
    pub wrap: MenuItem<R>,
}

/// The last clock read, so a tray click knows whether the notice comes first.
#[derive(Default)]
pub(crate) struct Last(pub Mutex<Option<Clock>>);

fn on_tracker<R: Runtime>(app: &AppHandle<R>) -> Option<tauri::WebviewWindow<R>> {
    let window = app.get_webview_window("main")?;
    let on = window.url().map(|url| url.host_str() == Some(crate::APP_HOST)).unwrap_or(false);
    on.then_some(window)
}

fn apply<R: Runtime>(app: &AppHandle<R>, clock: Option<Clock>) {
    let plan = plan(clock.as_ref());
    if let Some(items) = app.try_state::<Items<R>>() {
        let _ = items.status.set_text(&plan.status);
        let _ = items.call_in.set_enabled(plan.call_in);
        let _ = items.take_break.set_enabled(plan.take_break);
        let _ = items.back.set_enabled(plan.back);
        let _ = items.wrap.set_enabled(plan.wrap);
    }
    // Wrap-up note: the moment someone goes from on-shift to out, close the
    // loop with today's totals (computed from the segments the API sends).
    let prev = app
        .try_state::<Last>()
        .and_then(|last| last.0.lock().ok().and_then(|slot| slot.clone()));
    let wrapped = matches!(&prev, Some(p) if p.available && (p.state == "in" || p.state == "break"))
        && matches!(&clock, Some(c) if c.available && c.state == "out");
    if wrapped {
        if let Some(done) = clock.as_ref() {
            let (worked, rest) = day_totals(&done.today, unix_now());
            if worked > 0 {
                let body = if rest > 0 {
                    format!("{} worked, {} break. See you tomorrow.", fmt_span(worked), fmt_span(rest))
                } else {
                    format!("{} worked. See you tomorrow.", fmt_span(worked))
                };
                let _ = app.notification().builder().title("Wrapped").body(body).show();
            }
        }
    }
    if let Some(last) = app.try_state::<Last>() {
        if let Ok(mut slot) = last.0.lock() {
            *slot = clock;
        }
    }
    crate::mini::push(app);
}

/// Last known connectivity. Starts online; the first poll corrects it
/// silently (no banner for starting offline).
pub(crate) struct Net {
    pub online: Mutex<bool>,
    announced: Mutex<bool>,
}

impl Default for Net {
    fn default() -> Self {
        Self { online: Mutex::new(true), announced: Mutex::new(false) }
    }
}

/// Returns true on the online → offline edge (announce once per outage).
fn fell_offline(was: bool, now: bool, announced: bool) -> bool {
    was && !now && !announced
}

/// What the page reports for `navigator.onLine` ("1"/"0", JSON-encoded).
const ONLINE_JS: &str = "(function () { return navigator.onLine ? '1' : '0'; })()";

fn watch_connectivity<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = on_tracker(app) else { return };
    let app_for_cb = app.clone();
    let _ = window.eval_with_callback(ONLINE_JS, move |raw| {
        let now_online: String = serde_json::from_str(&raw).unwrap_or_default();
        let now_online = now_online != "0";
        let Some(net) = app_for_cb.try_state::<Net>() else { return };
        let (Ok(mut online), Ok(mut announced)) = (net.online.lock(), net.announced.lock()) else {
            return;
        };
        if fell_offline(*online, now_online, *announced) {
            *announced = true;
            let _ = app_for_cb
                .notification()
                .builder()
                .title("You're offline")
                .body("Saved sheets are in Saved for offline (tray menu).")
                .show();
        }
        if now_online {
            *announced = false;
        }
        *online = now_online;
    });
}

fn refresh<R: Runtime>(app: &AppHandle<R>) {
    watch_connectivity(app);
    let Some(window) = on_tracker(app) else { return };
    let app_for_cb = app.clone();
    let _ = window.eval_with_callback(TAKE_JS, move |raw| {
        let raw: String = serde_json::from_str(&raw).unwrap_or_default();
        if raw.is_empty() {
            return;
        }
        apply(&app_for_cb, parse(&raw));
    });
    let app_for_error = app.clone();
    let _ = window.eval_with_callback(TAKE_ERROR_JS, move |raw| {
        let message: String = serde_json::from_str(&raw).unwrap_or_default();
        if !message.is_empty() {
            let _ = app_for_error.notification().builder().title("Time sheet").body(message).show();
        }
    });
    let _ = window.eval(START_JS);
}

/// A tray click on one of the clock items: "in", "break", "back" or "wrap".
pub(crate) fn act<R: Runtime>(app: &AppHandle<R>, action: &str) {
    // Signed out, a clock tap (tray, shortcut) opens sign in (session.rs).
    if !crate::session::signed_in() {
        crate::show_main(app);
        return;
    }
    let Some(window) = on_tracker(app) else {
        crate::show_main(app);
        return;
    };
    let needs_notice = app
        .try_state::<Last>()
        .and_then(|last| last.0.lock().ok().and_then(|slot| slot.clone()))
        .map(|clock| clock.notice)
        .unwrap_or(false);
    if needs_notice && action == "in" {
        crate::show_main(app);
        let _ = window.eval(OPEN_CLOCK_JS);
        return;
    }
    let _ = window.eval(action_script(action));
    // Read the result back shortly, rather than waiting for the next tick,
    // and ask how they are if the clock said to (a call in or wrap).
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        refresh(&app);
        take_mood_ask(&app);
        std::thread::sleep(Duration::from_millis(1500));
        refresh(&app);
        take_mood_ask(&app);
    });
}

/// Seconds since the Unix epoch, via the system clock.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Days since 1970-01-01 (Howard Hinnant's civil-days algorithm).
fn days_since_epoch(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse the clock's ISO moment (`2026-10-05T06:00:00.000Z`, `Z` or
/// `±hh:mm`) to epoch seconds. `None` for anything unparseable — the tray
/// title then simply stays off. No chrono dependency for one timestamp.
pub(crate) fn parse_iso(s: &str) -> Option<u64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if d.next().is_some() {
        return None;
    }
    // Split the zone off the clock reading.
    let (clock, zone) = if let Some(pos) = time.find(['Z', '+', '-']) {
        time.split_at(pos)
    } else {
        return None;
    };
    let mut c = clock.split(':');
    let hh: i64 = c.next()?.parse().ok()?;
    let mm: i64 = c.next()?.parse().ok()?;
    let sec_part = c.next()?;
    if c.next().is_some() {
        return None;
    }
    let secs: i64 = sec_part.split('.').next()?.parse().ok()?;
    if !(1..=12).contains(&m) || day < 1 || day > 31 || hh > 23 || mm > 59 || secs > 60 {
        return None;
    }
    let offset: i64 = if zone == "Z" {
        0
    } else {
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let mut z = zone[1..].split(':');
        let zh: i64 = z.next()?.parse().ok()?;
        let zm: i64 = z.next()?.parse().ok()?;
        sign * (zh * 3600 + zm * 60)
    };
    let days = days_since_epoch(y, m, day);
    let epoch = days * 86_400 + hh * 3600 + mm * 60 + secs - offset;
    u64::try_from(epoch).ok()
}

/// Elapsed seconds between the clock's ISO moment and now.
pub(crate) fn elapsed_since(since_iso: &str, now: u64) -> Option<u64> {
    parse_iso(since_iso).and_then(|t| now.checked_sub(t))
}

/// Toggl-style menu-bar reading: `7:03` for 7 h 3 min. Hours unpadded,
/// minutes zero-padded, like the reference.
pub(crate) fn tray_title(elapsed_secs: u64) -> String {
    format!("{}:{:02}", elapsed_secs / 3600, (elapsed_secs % 3600) / 60)
}

/// Worked/break minutes from today's segments (an open stretch ends now).
pub(crate) fn day_totals(today: &[DaySegment], now: u64) -> (u64, u64) {
    let mut worked = 0;
    let mut rest = 0;
    for seg in today {
        let Some(start) = parse_iso(&seg.started_at) else { continue };
        let end = seg.ended_at.as_deref().and_then(parse_iso).unwrap_or(now);
        let mins = end.saturating_sub(start) / 60;
        if seg.kind == "BREAK" {
            rest += mins;
        } else {
            worked += mins;
        }
    }
    (worked, rest)
}

/// `8h 12m`, or `45m` under the hour — the site's own minute language.
pub(crate) fn fmt_span(mins: u64) -> String {
    if mins >= 60 {
        format!("{}h {:02}m", mins / 60, mins % 60)
    } else {
        format!("{mins}m")
    }
}

/// SAST is UTC+2 year-round (no daylight saving): weekday (Mon = 0) and
/// HHMM in Johannesburg from an epoch.
fn sast_weekday_hhmm(now: u64) -> (u64, u64) {
    let sast = now + 2 * 3600;
    let days = sast / 86_400;
    let tod = sast % 86_400;
    ((days + 3) % 7, (tod / 3600) * 100 + ((tod % 3600) / 60))
}

/// True inside the 08:30–08:34 SAST Mon–Fri window, once per day, when the
/// person is available but still off the clock.
fn should_nudge(weekday: u64, hhmm: u64, day: u64, nudged_day: u64, clock: Option<&Clock>) -> bool {
    weekday < 5
        && (830..=834).contains(&hhmm)
        && nudged_day != day
        && matches!(clock, Some(c) if c.available && c.state == "out")
}

static LAST_NUDGE_DAY: Mutex<u64> = Mutex::new(0);

/// One native reminder per weekday morning for anyone not called in yet.
/// The banner can't open the app on every platform, so it names the three
/// one-tap paths (tray, mini panel, hotkey) instead of a dead button.
fn maybe_nudge<R: Runtime>(app: &AppHandle<R>) {
    let now = unix_now();
    let (weekday, hhmm) = sast_weekday_hhmm(now);
    let day = now / 86_400;
    let clock = app
        .try_state::<Last>()
        .and_then(|last| last.0.lock().ok().and_then(|slot| slot.clone()));
    let nudged = LAST_NUDGE_DAY.lock().ok().map(|slot| *slot).unwrap_or(0);
    if !should_nudge(weekday, hhmm, day, nudged, clock.as_ref()) {
        return;
    }
    if let Ok(mut slot) = LAST_NUDGE_DAY.lock() {
        *slot = day;
    }
    let _ = app
        .notification()
        .builder()
        .title("Not called in yet")
        .body(if cfg!(target_os = "macos") {
            "Call in from the menu bar, the quick panel (Cmd+Shift+M), or Cmd+Shift+I."
        } else {
            "Call in from the tray, the quick panel (Ctrl+Alt+M), or Ctrl+Alt+I."
        })
        .show();
}

/// Start the background poller for the app's life, plus the once-a-second
/// menu-bar title ticker (Toggl-style live worked time).
pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn({
        let app = app.clone();
        move || loop {
            refresh(&app);
            std::thread::sleep(POLL_EVERY);
        }
    });
    std::thread::spawn(move || {
        let mut last = String::new();
        loop {
            tick_title(&app, &mut last);
            maybe_nudge(&app);
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

/// Refresh the menu-bar title from the last known clock: the live worked
/// time while called in or on a break, nothing otherwise. Called once a
/// second; only touches the tray when the text actually changed.
fn tick_title<R: Runtime>(app: &AppHandle<R>, last: &mut String) {
    let text = app
        .try_state::<Last>()
        .and_then(|state| state.0.lock().ok().and_then(|slot| slot.clone()))
        .filter(|c| c.available && (c.state == "in" || c.state == "break"))
        .and_then(|c| elapsed_since(&c.since_iso, unix_now()))
        .map(tray_title);
    let next = with_walkie(text.unwrap_or_default(), crate::walkie::unread(app));
    if next != *last {
        *last = next.clone();
        if let Some(tray) = app.tray_by_id(crate::TRAY_ID) {
            let _ = tray.set_title(if next.is_empty() { None } else { Some(next) });
        }
    }
}

/// The menu-bar title with unread walkie messages beside the timer
/// ("7:03 · 2 new"), so a message waiting is seen without opening anything.
pub(crate) fn with_walkie(timer: String, walkie_unread: u32) -> String {
    match (timer.is_empty(), walkie_unread) {
        (_, 0) => timer,
        (true, n) => format!("{n} new"),
        (false, n) => format!("{timer} · {n} new"),
    }
}

#[cfg(test)]
mod mood_ask_tests {
    use super::*;

    #[test]
    fn a_call_in_or_wrap_leaves_the_mood_question() {
        let js = action_script("in");
        assert!(js.contains("source: 'DESKTOP'"));
        assert!(js.contains("window.__cbbMoodAsk"));
        assert!(js.contains("mood.askedKey !== mood.today + '|' + moment"));
        let ask: MoodAsk = serde_json::from_str(r#"{"moment":"WRAP"}"#).unwrap();
        assert_eq!(ask, MoodAsk { moment: "WRAP".into() });
        assert!(!js.contains("support"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(state: &str, since: &str) -> Clock {
        Clock { available: true, state: state.into(), since: since.into(), since_iso: String::new(), today: Vec::new(), notice: false }
    }

    #[test]
    fn reads_the_pages_answer() {
        assert_eq!(
            parse(r#"{"available":true,"state":"in","since":"08:02","sinceIso":"2026-10-05T06:00:00.000Z","notice":false}"#),
            Some(Clock {
                available: true,
                state: "in".into(),
                since: "08:02".into(),
                since_iso: "2026-10-05T06:00:00.000Z".into(),
                today: Vec::new(),
                notice: false,
            })
        );
        for raw in ["", "null", "<html>", "{\"state\":\"in\"}"] {
            assert!(parse(raw).is_none(), "{raw}");
        }
    }

    #[test]
    fn offers_only_what_makes_sense_now() {
        let out = plan(Some(&clock("out", "")));
        assert_eq!((out.call_in, out.take_break, out.back, out.wrap), (true, false, false, false));
        assert_eq!(out.status, "Time sheet: Off the clock");

        let working = plan(Some(&clock("in", "08:02")));
        assert_eq!((working.call_in, working.take_break, working.back, working.wrap), (false, true, false, true));
        assert_eq!(working.status, "Time sheet: Called in since 08:02");

        let resting = plan(Some(&clock("break", "13:00")));
        assert_eq!((resting.call_in, resting.take_break, resting.back, resting.wrap), (false, false, true, true));
    }

    #[test]
    fn nothing_to_press_without_a_time_sheet() {
        for clock in [None, Some(Clock { available: false, ..clock("in", "08:00") })] {
            let p = plan(clock.as_ref());
            assert!(!(p.call_in || p.take_break || p.back || p.wrap));
            assert_eq!(p.status, "Time sheet: not in use");
        }
    }

    #[test]
    fn clocks_through_the_trackers_own_route_as_the_desktop() {        let script = action_script("wrap");
        assert!(script.contains("fetch('/api/time-sheet/clock'"));
        assert!(script.contains(r#"action: "wrap""#));
        assert!(script.contains("source: 'DESKTOP'"));
        assert!(script.contains("cbb:clock-changed"));
    }

    #[test]
    fn iso_moments_parse_to_epoch() {
        assert_eq!(parse_iso("2026-10-05T06:00:00.000Z"), Some(1791180000));
        assert_eq!(parse_iso("2026-10-05T08:00:00+02:00"), Some(1791180000));
        assert_eq!(parse_iso("2026-10-05T06:00:00Z"), Some(1791180000));
        for raw in ["", "null", "08:02", "2026-10-05", "2026-13-05T06:00:00Z", "2026-10-05T25:00:00Z"] {
            assert_eq!(parse_iso(raw), None, "{raw}");
        }
    }

    #[test]
    fn elapsed_counts_from_the_iso_moment() {
        assert_eq!(elapsed_since("2026-10-05T06:00:00.000Z", 1791180000 + 7 * 3600 + 3 * 60), Some(7 * 3600 + 3 * 60));
        assert_eq!(elapsed_since("", 1791180000), None);
        // A moment in the future never shows a negative reading.
        assert_eq!(elapsed_since("2026-10-05T06:00:00.000Z", 1791180000 - 1), None);
    }

    #[test]
    fn tray_title_reads_like_the_reference() {
        assert_eq!(tray_title(7 * 3600 + 3 * 60), "7:03");
        assert_eq!(tray_title(24 * 60), "0:24");
        assert_eq!(tray_title(0), "0:00");
    }

    #[test]
    fn walkie_messages_sit_beside_the_timer() {
        assert_eq!(with_walkie("7:03".into(), 0), "7:03");
        assert_eq!(with_walkie("7:03".into(), 2), "7:03 · 2 new");
        assert_eq!(with_walkie(String::new(), 1), "1 new");
        assert_eq!(with_walkie(String::new(), 0), "");
    }

    #[test]
    fn day_totals_split_work_and_break() {
        let seg = |kind: &str, start: &str, end: Option<&str>| DaySegment {
            kind: kind.into(),
            started_at: start.into(),
            ended_at: end.map(|s| s.into()),
        };
        // 2026-10-05: 06:00–09:00 work, 09:00–09:30 break, 09:30–open work.
        let today = vec![
            seg("WORK", "2026-10-05T06:00:00Z", Some("2026-10-05T09:00:00Z")),
            seg("BREAK", "2026-10-05T09:00:00Z", Some("2026-10-05T09:30:00Z")),
            seg("WORK", "2026-10-05T09:30:00Z", None),
        ];
        // "now" = 10:00Z: 210 worked, 30 break.
        assert_eq!(day_totals(&today, 1791180000 + 4 * 3600), (210, 30));
        assert_eq!(fmt_span(210), "3h 30m");
        assert_eq!(fmt_span(45), "45m");
    }

    #[test]
    fn nudge_fires_once_on_weekday_mornings() {
        // 2026-10-05 is a Monday. 08:30 SAST = 06:30Z.
        let monday_0830 = 1791180000 + 1800;
        let (weekday, hhmm) = sast_weekday_hhmm(monday_0830);
        assert_eq!((weekday, hhmm), (0, 830));
        let out = clock("out", "");
        assert!(should_nudge(0, 830, monday_0830 / 86_400, 0, Some(&out)));
        // Same day again: silent.
        assert!(!should_nudge(0, 831, monday_0830 / 86_400, monday_0830 / 86_400, Some(&out)));
        // Saturday: silent. Called in: silent.
        assert!(!should_nudge(5, 830, 0, 0, Some(&out)));
        assert!(!should_nudge(0, 830, 0, 0, Some(&clock("in", "08:02"))));
    }

    #[test]
    fn offline_edge_announces_once_per_outage() {
        assert!(fell_offline(true, false, false));
        assert!(!fell_offline(true, false, true));
        assert!(!fell_offline(false, false, false));
        assert!(!fell_offline(true, true, false));
    }
}
