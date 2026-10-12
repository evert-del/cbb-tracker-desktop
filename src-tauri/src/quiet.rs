//! Which notifications raise a banner. Counts, the quick panel and the menu
//! bar always stay current; only the banners are held back.
//!
//! - Settings (quick panel › Settings, `settings.json`): tracker
//!   notifications on/off, walkie messages on/off, and Channel 1 (the
//!   company-wide channel, the noisy one) off unless switched on.
//! - The tracker's own do-not-disturb (`/api/walkie/status`, `me.dndUntil`),
//!   so quiet on the website is quiet here too, and the panel can set it.
//!   As on the website, a call alert still rings through do-not-disturb.
//!
//! As elsewhere, the shell holds no credentials: the tracker page reads and
//! sets do-not-disturb with its own session (`eval`, collected through a
//! window variable).

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Runtime};

/// Tracker notifications (approvals, phases, …) raise no banner.
const MUTE_TRACKER: &str = "mute_tracker";
/// Walkie messages raise no banner.
const MUTE_WALKIE: &str = "mute_walkie";
/// Channel 1 messages raise a banner (off by default).
const CHANNEL1: &str = "banners_channel1";

/// The banner settings and do-not-disturb, as the panel shows them.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct Prefs {
    pub tracker: bool,
    pub walkie: bool,
    pub channel1: bool,
    /// The tracker's do-not-disturb end (ISO), "" when off or past.
    pub dnd_until: String,
}

/// The kind of banner, for `allows`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    /// A tracker notification (the bell).
    Tracker,
    /// A walkie message; `company` when it was said on Channel 1.
    Walkie { company: bool },
    /// A walkie call alert: always rings, as on the website.
    Call,
}

/// The last do-not-disturb end the tracker reported (ISO, "" when none).
static DND_UNTIL: Mutex<String> = Mutex::new(String::new());

pub(crate) fn prefs<R: Runtime>(app: &AppHandle<R>, now: u64) -> Prefs {
    Prefs {
        tracker: !crate::close::flag(app, MUTE_TRACKER),
        walkie: !crate::close::flag(app, MUTE_WALKIE),
        channel1: crate::close::flag(app, CHANNEL1),
        dnd_until: dnd_until(now),
    }
}

/// Whether a banner of `kind` may show, given the settings and the time.
pub(crate) fn allows(prefs: &Prefs, kind: Kind, now: u64) -> bool {
    let quiet = dnd_active(&prefs.dnd_until, now);
    match kind {
        Kind::Call => true,
        Kind::Tracker => prefs.tracker && !quiet,
        Kind::Walkie { company } => prefs.walkie && (!company || prefs.channel1) && !quiet,
    }
}

pub(crate) fn allowed<R: Runtime>(app: &AppHandle<R>, kind: Kind) -> bool {
    let now = crate::clock::unix_now();
    allows(&prefs(app, now), kind, now)
}

fn dnd_active(until: &str, now: u64) -> bool {
    crate::clock::parse_iso(until).is_some_and(|end| end > now)
}

/// The current do-not-disturb end, "" when off or already past.
fn dnd_until(now: u64) -> String {
    DND_UNTIL
        .lock()
        .ok()
        .map(|until| until.clone())
        .filter(|until| dnd_active(until, now))
        .unwrap_or_default()
}

/// Remember what the tracker says about do-not-disturb (`null` = off).
pub(crate) fn set_dnd_until(until: Option<&str>) {
    if let Ok(mut slot) = DND_UNTIL.lock() {
        *slot = until.unwrap_or_default().to_string();
    }
}

/// `me.dndUntil` from the tracker's `/api/walkie/status` answer. `None` when
/// the answer isn't one (signed out, not on the crew, malformed): the last
/// known state is then kept.
pub(crate) fn parse_status(raw: &str) -> Option<Option<String>> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let me = value.get("me")?;
    Some(me.get("dndUntil").and_then(|until| until.as_str()).map(str::to_string))
}

/// Starts reading do-not-disturb; the answer lands in `window.__cbbDnd`.
pub(crate) const STATUS_START_JS: &str = r#"(function () {
  fetch('/api/walkie/status', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.ok ? r.text() : 'null'; })
    .then(function (t) { window.__cbbDnd = t; })
    .catch(function () { window.__cbbDnd = 'null'; });
})()"#;

/// Returns and clears the last do-not-disturb answer ("" while waiting).
pub(crate) const STATUS_TAKE_JS: &str =
    "(function () { var v = window.__cbbDnd || ''; window.__cbbDnd = ''; return v; })()";

/// The do-not-disturb choices the tracker takes (`null` turns it off).
pub(crate) fn dnd_choice(choice: &str) -> Option<serde_json::Value> {
    match choice {
        "1h" | "2h" | "4h" | "morning" => Some(serde_json::Value::String(choice.into())),
        "off" => Some(serde_json::Value::Null),
        _ => None,
    }
}

/// Sets do-not-disturb through the tracker page's own session and leaves its
/// answer (`{ dndUntil }`) on `window.__cbbDnd`, as a status read would.
pub(crate) fn set_dnd_script(dnd: &serde_json::Value) -> String {
    format!(
        r#"(function (dnd) {{
  window.__cbbDnd = '';
  fetch('/api/walkie/status', {{ method: 'POST', credentials: 'same-origin', headers: {{ 'Content-Type': 'application/json' }}, body: JSON.stringify({{ dnd: dnd }}) }})
    .then(function (r) {{ return r.ok ? r.json() : null; }})
    .then(function (b) {{ window.__cbbDnd = b ? JSON.stringify({{ me: {{ dndUntil: b.dndUntil || null }} }}) : 'null'; }})
    .catch(function () {{ window.__cbbDnd = 'null'; }});
}})({dnd})"#
    )
}

/// A settings toggle from the panel. Returns the setting's name and new
/// value for analytics, or `None` for an unknown item.
pub(crate) fn toggle<R: Runtime>(app: &AppHandle<R>, item: &str) -> Option<(&'static str, bool)> {
    let (key, name) = match item {
        "banners-tracker" => (MUTE_TRACKER, "banners_tracker"),
        "banners-walkie" => (MUTE_WALKIE, "banners_walkie"),
        "banners-channel1" => (CHANNEL1, "banners_channel1"),
        _ => return None,
    };
    let stored = !crate::close::flag(app, key);
    crate::close::set_flag(app, key, stored);
    // The two mute keys are stored inverted (on = muted).
    let on = if key == CHANNEL1 { stored } else { !stored };
    Some((name, on))
}

pub(crate) fn is_toggle(item: &str) -> bool {
    matches!(item, "banners-tracker" | "banners-walkie" | "banners-channel1")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_791_352_800; // 2026-10-07T06:00:00Z

    fn prefs(tracker: bool, walkie: bool, channel1: bool, dnd_until: &str) -> Prefs {
        Prefs { tracker, walkie, channel1, dnd_until: dnd_until.into() }
    }

    #[test]
    fn defaults_mute_only_channel_1() {
        let p = prefs(true, true, false, "");
        assert!(allows(&p, Kind::Tracker, NOW));
        assert!(allows(&p, Kind::Walkie { company: false }, NOW));
        assert!(!allows(&p, Kind::Walkie { company: true }, NOW));
        assert!(allows(&p, Kind::Call, NOW));
    }

    #[test]
    fn muting_holds_back_each_kind() {
        assert!(!allows(&prefs(false, true, true, ""), Kind::Tracker, NOW));
        let no_walkie = prefs(true, false, true, "");
        assert!(!allows(&no_walkie, Kind::Walkie { company: false }, NOW));
        assert!(!allows(&no_walkie, Kind::Walkie { company: true }, NOW));
        assert!(allows(&prefs(true, true, true, ""), Kind::Walkie { company: true }, NOW));
    }

    #[test]
    fn do_not_disturb_quiets_all_but_calls_until_it_ends() {
        let quiet = prefs(true, true, true, "2026-10-07T08:00:00.000Z");
        assert!(!allows(&quiet, Kind::Tracker, NOW));
        assert!(!allows(&quiet, Kind::Walkie { company: false }, NOW));
        assert!(allows(&quiet, Kind::Call, NOW));
        let over = prefs(true, true, true, "2026-10-07T05:59:00.000Z");
        assert!(allows(&over, Kind::Tracker, NOW));
        let garbage = prefs(true, true, true, "soon");
        assert!(allows(&garbage, Kind::Tracker, NOW));
    }

    #[test]
    fn status_answers_are_read_for_me_only() {
        assert_eq!(
            parse_status(r#"{"me":{"id":"u","dndUntil":"2026-10-07T08:00:00.000Z"},"people":[]}"#),
            Some(Some("2026-10-07T08:00:00.000Z".into()))
        );
        assert_eq!(parse_status(r#"{"me":{"id":"u","dndUntil":null}}"#), Some(None));
        for raw in ["", "null", r#"{"error":"Walkie is for the crew."}"#, "<html>"] {
            assert_eq!(parse_status(raw), None, "{raw}");
        }
    }

    #[test]
    fn only_the_trackers_choices_pass() {
        for choice in ["1h", "2h", "4h", "morning"] {
            assert_eq!(dnd_choice(choice), Some(serde_json::Value::String(choice.into())));
        }
        assert_eq!(dnd_choice("off"), Some(serde_json::Value::Null));
        for bad in ["", "8h", "forever", "1h;"] {
            assert_eq!(dnd_choice(bad), None, "{bad}");
        }
        let js = set_dnd_script(&dnd_choice("2h").unwrap());
        assert!(js.contains("fetch('/api/walkie/status', { method: 'POST'"));
        assert!(js.ends_with("})(\"2h\")"));
        assert!(set_dnd_script(&serde_json::Value::Null).ends_with("})(null)"));
    }

    #[test]
    fn only_known_toggles_pass() {
        for item in ["banners-tracker", "banners-walkie", "banners-channel1"] {
            assert!(is_toggle(item));
        }
        assert!(!is_toggle("banners-everything"));
    }
}
