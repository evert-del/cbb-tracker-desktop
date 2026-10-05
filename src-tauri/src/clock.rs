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
      window.__cbbClock = JSON.stringify({ available: !!c.available, state: c.state || 'out', since: t, sinceIso: c.since || '', notice: !!c.notice });
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
pub(crate) struct Clock {
    pub available: bool,
    pub state: String,
    /// "08:02", or "" when off the clock.
    pub since: String,
    /// Raw ISO timestamp of the same moment, or "" when off the clock.
    #[serde(default)]
    pub since_iso: String,
    /// The person has not read the current notice.
    pub notice: bool,
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
pub(crate) fn action_script(action: &str) -> String {
    format!(
        r#"(function () {{
  fetch('/api/time-sheet/clock', {{ method: 'POST', credentials: 'same-origin', headers: {{ 'Content-Type': 'application/json' }},
    body: JSON.stringify({{ action: {action:?}, source: 'DESKTOP' }}) }})
    .then(function (r) {{ return r.json().then(function (b) {{ return {{ ok: r.ok, b: b }}; }}); }})
    .then(function (x) {{
      if (!x.ok) {{ window.__cbbClockError = (x.b && x.b.error) || 'That did not save.'; return; }}
      window.dispatchEvent(new CustomEvent('cbb:clock-changed'));
    }})
    .catch(function () {{ window.__cbbClockError = 'The tracker could not be reached.'; }});
}})()"#
    )
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
    if let Some(last) = app.try_state::<Last>() {
        if let Ok(mut slot) = last.0.lock() {
            *slot = clock;
        }
    }
    crate::mini::push(app);
}

fn refresh<R: Runtime>(app: &AppHandle<R>) {
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
    // Read the result back shortly, rather than waiting for the next tick.
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        refresh(&app);
        std::thread::sleep(Duration::from_millis(1500));
        refresh(&app);
    });
}

/// Start the background poller for the app's life.
pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || loop {
        refresh(&app);
        std::thread::sleep(POLL_EVERY);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(state: &str, since: &str) -> Clock {
        Clock { available: true, state: state.into(), since: since.into(), since_iso: String::new(), notice: false }
    }

    #[test]
    fn reads_the_pages_answer() {
        assert_eq!(
            parse(r#"{"available":true,"state":"in","since":"08:02","notice":false}"#),
            Some(clock("in", "08:02"))
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
    fn clocks_through_the_trackers_own_route_as_the_desktop() {
        let script = action_script("wrap");
        assert!(script.contains("fetch('/api/time-sheet/clock'"));
        assert!(script.contains(r#"action: "wrap""#));
        assert!(script.contains("source: 'DESKTOP'"));
        assert!(script.contains("cbb:clock-changed"));
    }
}
