//! The project timer while offline (hourly.rs): a start, a switch or a stop
//! tapped with no connection is kept here, with the moment it was tapped,
//! and sent to the tracker in order once it answers again. Crew on set or on
//! the road keep timing; nothing waits on the signal.
//!
//! - Kept in memory only, never written to the computer: quitting the app
//!   while offline loses what wasn't sent yet (the panel says so).
//! - Time not yet filed on a project isn't kept offline: there's no project
//!   to put it on later. The panel asks for a project instead.
//! - On the way back it ends whatever the tracker had running at the first
//!   offline tap, adds each finished offline stretch as time
//!   (`POST /api/me/project-time`), and starts the one still running, after
//!   adding the minutes it ran offline. A timer started elsewhere meanwhile
//!   (a phone) is left alone.
//! - Only ever what was tapped: it never adds time of its own.

use serde_json::{json, Value};

/// A stretch timed offline (unix seconds). `end` is `None` while it runs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stretch {
    pub project: String,
    pub note: Option<String>,
    /// On a mixed project, the deliverable picked at the start.
    pub deliverable: Option<String>,
    pub start: u64,
    pub end: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Queue {
    /// When the timer the tracker had running was stopped offline (by a stop
    /// or a switch), if it was running.
    pub stop_server_at: Option<u64>,
    pub stretches: Vec<Stretch>,
}

/// The running timer's id while it runs offline (it isn't on the tracker yet).
pub(crate) const OFFLINE_ID: &str = "offline";

impl Queue {
    pub(crate) fn is_empty(&self) -> bool {
        self.stop_server_at.is_none() && self.stretches.is_empty()
    }

    /// The stretch running offline, if any.
    pub(crate) fn running(&self) -> Option<&Stretch> {
        self.stretches.last().filter(|s| s.end.is_none())
    }

    /// Ends what's running at `at`: the offline stretch, or else the
    /// tracker's own timer (`server_running`).
    fn end_at(&mut self, server_running: bool, at: u64) {
        if let Some(open) = self.stretches.last_mut().filter(|s| s.end.is_none()) {
            open.end = Some(at.max(open.start));
        } else if server_running && self.stretches.is_empty() && self.stop_server_at.is_none() {
            self.stop_server_at = Some(at);
        }
    }

    /// Started (or switched to) `project` at `at`.
    pub(crate) fn start(&mut self, server_running: bool, project: &str, note: Option<String>, deliverable: Option<String>, at: u64) {
        self.end_at(server_running, at);
        self.stretches.push(Stretch { project: project.to_string(), note, deliverable, start: at, end: None });
    }

    /// Stopped at `at`.
    pub(crate) fn stop(&mut self, server_running: bool, at: u64) {
        self.end_at(server_running, at);
    }

    /// What to send, in order, now that the tracker answers. `server_started`
    /// is when the tracker's running timer started (`None`: nothing runs).
    /// Each step's body is checked as Add time and Start are.
    pub(crate) fn steps(&self, server_started: Option<u64>, now: u64) -> Vec<(&'static str, String, Value)> {
        let mut steps = Vec::new();
        // Only the timer that was running when the first offline tap came:
        // one started later, elsewhere, is left alone.
        if let (Some(at), Some(started)) = (self.stop_server_at, server_started) {
            if started < at {
                steps.push(("POST", TIMER_URL.to_string(), json!({ "action": "stop", "at": crate::hourly::iso_utc(at) })));
            }
        }
        for stretch in &self.stretches {
            let end = stretch.end.unwrap_or(now).min(now);
            // Under a minute isn't worth a line; more than a day can't be one.
            if end >= stretch.start + 60 && end - stretch.start <= 24 * 3600 {
                let mut body = json!({
                    "projectId": stretch.project,
                    "startedAt": crate::hourly::iso_utc(stretch.start),
                    "endedAt": crate::hourly::iso_utc(end),
                });
                if let Some(note) = &stretch.note {
                    body["note"] = Value::String(note.clone());
                }
                if let Some(deliverable) = &stretch.deliverable {
                    body["deliverableId"] = Value::String(deliverable.clone());
                }
                steps.push(("POST", TIME_URL.to_string(), body));
            }
        }
        if let Some(open) = self.running() {
            let mut body = json!({ "action": "start", "projectId": open.project });
            if let Some(note) = &open.note {
                body["note"] = Value::String(note.clone());
            }
            if let Some(deliverable) = &open.deliverable {
                body["deliverableId"] = Value::String(deliverable.clone());
            }
            steps.push(("POST", TIMER_URL.to_string(), body));
        }
        steps
    }
}

const TIMER_URL: &str = "/api/me/project-timer";
const TIME_URL: &str = "/api/me/project-time";

/// The fetch itself failed: no connection. (A refusal, or a slow answer, is
/// never taken for offline: the tap may have reached the tracker.)
pub(crate) fn is_offline(error: &str) -> bool {
    error.starts_with("You seem to be offline")
}

/// What a timer step asked for, read back from its body.
#[derive(Debug, PartialEq)]
pub(crate) enum Tap {
    Start { project: Option<String>, note: Option<String>, deliverable: Option<String> },
    Stop { at: Option<u64> },
    /// Drop the idle time and keep going: stop at `at`, start the same again now.
    Restart { at: u64 },
}

pub(crate) fn tap_of(body: &Value) -> Option<Tap> {
    let text = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_string);
    let at = text("at").and_then(|at| crate::clock::parse_iso(&at));
    match body.get("action")?.as_str()? {
        "start" => Some(Tap::Start { project: text("projectId"), note: text("note"), deliverable: text("deliverableId") }),
        "stop" => Some(Tap::Stop { at }),
        "restart" => Some(Tap::Restart { at: at? }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iso(secs: u64) -> String {
        crate::hourly::iso_utc(secs)
    }

    const T: u64 = 1_760_000_000;

    #[test]
    fn a_switch_and_a_stop_offline_end_the_trackers_timer_and_add_the_stretch() {
        let mut q = Queue::default();
        q.start(true, "p2", Some("Grade".into()), Some("d1".into()), T);
        q.stop(true, T + 3600);
        assert_eq!(q.running(), None);
        let steps = q.steps(Some(T - 600), T + 4000);
        assert_eq!(steps, vec![
            ("POST", TIMER_URL.to_string(), json!({ "action": "stop", "at": iso(T) })),
            ("POST", TIME_URL.to_string(), json!({ "projectId": "p2", "startedAt": iso(T), "endedAt": iso(T + 3600), "note": "Grade", "deliverableId": "d1" })),
        ]);
    }

    #[test]
    fn a_timer_still_running_is_added_up_to_now_then_started() {
        let mut q = Queue::default();
        q.start(false, "p1", None, None, T);
        q.start(false, "p2", None, None, T + 1800);
        assert_eq!(q.running().map(|s| s.project.as_str()), Some("p2"));
        let steps = q.steps(None, T + 2400);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].2["endedAt"], json!(iso(T + 1800)));
        assert_eq!(steps[1].2, json!({ "projectId": "p2", "startedAt": iso(T + 1800), "endedAt": iso(T + 2400) }));
        assert_eq!(steps[2].2, json!({ "action": "start", "projectId": "p2" }));
    }

    #[test]
    fn a_timer_started_elsewhere_later_is_left_alone_and_a_blip_adds_nothing() {
        let mut q = Queue::default();
        q.stop(true, T);
        assert!(q.steps(Some(T + 60), T + 120).is_empty());
        let mut q = Queue::default();
        q.start(false, "p1", None, None, T);
        q.stop(false, T + 20);
        assert!(q.steps(None, T + 30).is_empty());
        assert!(Queue::default().is_empty());
    }

    #[test]
    fn only_a_failed_fetch_counts_as_offline() {
        assert!(is_offline("You seem to be offline. Try again in a moment."));
        assert!(!is_offline("The tracker could not be reached."));
        assert!(!is_offline("You can only time a project you work on."));
    }

    #[test]
    fn taps_are_read_back_from_the_bodies_the_app_sends() {
        assert_eq!(tap_of(&json!({ "action": "start", "projectId": "p1", "note": "x" })),
            Some(Tap::Start { project: Some("p1".into()), note: Some("x".into()), deliverable: None }));
        assert_eq!(tap_of(&json!({ "action": "stop" })), Some(Tap::Stop { at: None }));
        assert_eq!(tap_of(&json!({ "action": "stop", "at": iso(T) })), Some(Tap::Stop { at: Some(T) }));
        assert_eq!(tap_of(&json!({ "action": "restart", "at": iso(T) })), Some(Tap::Restart { at: T }));
        assert_eq!(tap_of(&json!({ "action": "restart" })), None);
        assert_eq!(tap_of(&json!({ "projectId": "p1" })), None);
    }
}
