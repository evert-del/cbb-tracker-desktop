//! Your next call in the quick panel, and your call sheets saved for offline
//! before a set day, fed by the tracker's `GET /api/me/schedule?days=2`
//! (My Schedule's own read: your bookings, their place and call sheet).
//!
//! - The panel shows the next booking not yet over: when, what, where (with
//!   a map link that opens in the browser) and its call sheet.
//! - Each call sheet of the next two business days that isn't saved yet is
//!   saved for offline through the same flow as the Library's Save
//!   (offline.rs), one at a time. On by default; Settings turns it off.
//!
//! While the tracker doesn't have the endpoint yet (404), or the person has
//! no schedule (403), both stay hidden and quiet. As elsewhere, the page asks
//! with its own session (`eval`) and the answer is collected through a
//! window variable; the shell holds no credentials.

use std::{sync::Mutex, time::Duration};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};

const POLL_EVERY: Duration = Duration::from_secs(15 * 60);
/// The first read, once the tracker has had time to load after launch.
const FIRST_AFTER: Duration = Duration::from_secs(45);

/// Saving call sheets ahead is off when set (`settings.json`).
const NO_AUTO_SAVE: &str = "no_auto_offline";

const START_JS: &str = r#"(function () {
  fetch('/api/me/schedule?days=2', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.ok ? r.text() : 'null'; })
    .then(function (t) { window.__cbbSchedule = t; })
    .catch(function () { window.__cbbSchedule = 'null'; });
})()"#;

const TAKE_JS: &str =
    "(function () { var v = window.__cbbSchedule || ''; window.__cbbSchedule = ''; return v; })()";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Project {
    #[serde(default)]
    number: String,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Sheet {
    version_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    revision_label: String,
    pdf: String,
    #[serde(default)]
    href: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Booking {
    #[serde(default)]
    day: String,
    starts_at: String,
    #[serde(default)]
    ends_at: String,
    #[serde(default)]
    title: String,
    project: Project,
    #[serde(default)]
    place: Option<String>,
    #[serde(default)]
    map_url: Option<String>,
    #[serde(default)]
    pencilled: bool,
    #[serde(default)]
    sheet: Option<Sheet>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Ahead {
    #[serde(default)]
    bookings: Vec<Booking>,
}

pub(crate) fn parse(raw: &str) -> Option<Ahead> {
    serde_json::from_str(raw).ok()
}

/// The next call, as the panel shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct NextCall {
    pub starts_at: String,
    pub ends_at: String,
    /// The business day (yyyy-mm-dd), so "Today" / "Tomorrow" is the
    /// tracker's, not this computer's clock.
    pub day: String,
    pub title: String,
    pub project: String,
    pub place: Option<String>,
    /// Google Maps only (checked), opened in the browser.
    pub map_url: Option<String>,
    pub pencilled: bool,
    /// The call sheet's tracker page.
    pub sheet_href: Option<String>,
}

/// The first booking (the tracker leaves out those already over).
pub(crate) fn next_call(ahead: &Ahead) -> Option<NextCall> {
    let booking = ahead.bookings.first()?;
    Some(NextCall {
        starts_at: booking.starts_at.clone(),
        ends_at: booking.ends_at.clone(),
        day: booking.day.clone(),
        title: booking.title.clone(),
        project: format!("{} {}", booking.project.number, booking.project.name).trim().to_string(),
        place: booking.place.clone(),
        map_url: booking.map_url.clone().filter(|url| is_map_url(url)),
        pencilled: booking.pencilled,
        sheet_href: booking.sheet.as_ref().map(|sheet| sheet.href.clone()).filter(|href| href.starts_with('/')),
    })
}

/// A link the panel may open in the browser: Google Maps, nothing else.
pub(crate) fn is_map_url(url: &str) -> bool {
    url.starts_with("https://www.google.com/maps/")
}

/// A call sheet to save: its absolute PDF address and the Library's label.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToSave {
    pub url: String,
    pub label: String,
}

/// The call sheets of the days ahead not saved yet, once each. Only the
/// tracker's own call-sheet PDF route; a pencilled day's sheet too (it may
/// still be the day).
pub(crate) fn to_save(ahead: &Ahead, saved: &[String]) -> Vec<ToSave> {
    let mut out: Vec<ToSave> = Vec::new();
    for booking in &ahead.bookings {
        let Some(sheet) = &booking.sheet else { continue };
        let expected = format!("/api/call-sheets/{}/pdf", sheet.version_id);
        if sheet.pdf != expected || !crate::walkie::is_uuid(&sheet.version_id) {
            continue;
        }
        let url = format!("{}{}", crate::APP_ORIGIN, sheet.pdf);
        if saved.iter().any(|s| s == &url) || out.iter().any(|s| s.url == url) {
            continue;
        }
        let what = [sheet.title.as_str(), sheet.revision_label.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty() && *part != "Original")
            .collect::<Vec<_>>()
            .join(" ");
        let label = format!("{} {} — {} ({})", booking.project.number, booking.project.name, what, booking.day)
            .trim()
            .to_string();
        out.push(ToSave { url, label });
    }
    out
}

/// The latest next call, for the panel.
#[derive(Default)]
pub(crate) struct Schedule(pub Mutex<Option<NextCall>>);

pub(crate) fn next<R: Runtime>(app: &AppHandle<R>) -> Option<NextCall> {
    app.try_state::<Schedule>().and_then(|state| state.0.lock().ok().and_then(|slot| slot.clone()))
}

pub(crate) fn auto_save<R: Runtime>(app: &AppHandle<R>) -> bool {
    !crate::close::flag(app, NO_AUTO_SAVE)
}

/// Settings › "Save my call sheets for offline". Returns the new value.
pub(crate) fn toggle_auto_save<R: Runtime>(app: &AppHandle<R>) -> bool {
    let on = !auto_save(app);
    crate::close::set_flag(app, NO_AUTO_SAVE, !on);
    if on {
        refresh(app);
    }
    on
}

fn apply<R: Runtime>(app: &AppHandle<R>, ahead: Option<Ahead>) {
    let next = ahead.as_ref().and_then(next_call);
    if let Some(state) = app.try_state::<Schedule>() {
        if let Ok(mut slot) = state.0.lock() {
            *slot = next;
        }
    }
    crate::mini::push(app);
    if let Some(ahead) = ahead.filter(|_| auto_save(app)) {
        save_ahead(app, &ahead);
    }
}

/// Save the call sheets not saved yet, one after the other (a save is a
/// download through the main window, one at a time).
fn save_ahead<R: Runtime>(app: &AppHandle<R>, ahead: &Ahead) {
    let wanted = to_save(ahead, &crate::offline::saved_sources(app));
    if wanted.is_empty() {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        for sheet in wanted {
            // Wait for any save in flight (the Library's, or the last one).
            for _ in 0..60 {
                if !crate::offline::busy(&app) {
                    break;
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            if crate::offline::busy(&app) || !crate::session::signed_in() {
                return;
            }
            let Some(pending) = app.try_state::<crate::offline::PendingState>() else { return };
            if crate::offline::start_save(&app, pending.inner(), &sheet.url, &sheet.label).is_ok() {
                crate::analytics::action(&app, "call_sheet_saved_ahead", "schedule");
            }
        }
    });
}

/// Read the schedule now (Settings turned saving on).
pub(crate) fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = crate::walkie::tracker_window(app) else { return };
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = window.eval(START_JS);
        if let Some(raw) = crate::walkie::collect(&window, TAKE_JS, 30) {
            apply(&app, parse(&raw));
        }
    });
}

/// Start the background reader for the app's life.
pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_AFTER);
        loop {
            if crate::session::signed_in() {
                if let Some(window) = crate::walkie::tracker_window(&app) {
                    let _ = window.eval(START_JS);
                    if let Some(raw) = crate::walkie::collect(&window, TAKE_JS, 30) {
                        apply(&app, parse(&raw));
                    }
                }
            } else if let Some(state) = app.try_state::<Schedule>() {
                if let Ok(mut slot) = state.0.lock() {
                    *slot = None;
                }
            }
            std::thread::sleep(POLL_EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = "0b5e0a52-3c4d-4e8f-9a1b-2c3d4e5f6a7b";
    const V2: &str = "1c6f1b63-4d5e-4f90-8b2c-3d4e5f6a7b8c";

    fn ahead(json: &str) -> Ahead {
        parse(json).expect("test payload parses")
    }

    fn booking(day: &str, version: Option<&str>, revision: &str) -> String {
        let sheet = version.map_or("null".to_string(), |v| {
            format!(r#"{{"versionId":"{v}","title":"Shoot Day 1","versionNumber":2,"revisionLabel":"{revision}","stale":false,"pdf":"/api/call-sheets/{v}/pdf","href":"/call-sheets/{v}"}}"#)
        });
        format!(
            r#"{{"id":"b","day":"{day}","startsAt":"{day}T04:00:00.000Z","endsAt":"{day}T16:00:00.000Z","title":"Shoot Day 1","kind":"SHOOT","project":{{"number":"CB-0412","name":"Con Spirito S1"}},"place":"Melrose Arch","address":"1 Melrose Sq","mapUrl":"https://www.google.com/maps/search/?api=1&query=x","designation":"Gaffer","pencilled":false,"sheet":{sheet},"href":"/my-schedule"}}"#
        )
    }

    fn payload(bookings: &[String]) -> String {
        format!(r#"{{"today":"2026-10-12","days":2,"bookings":[{}]}}"#, bookings.join(","))
    }

    #[test]
    fn unusable_answers_are_ignored() {
        for raw in ["", "null", "<html>", r#"{"error":"x"}"#] {
            assert!(parse(raw).is_none() || parse(raw).is_some_and(|a| a.bookings.is_empty()), "{raw}");
        }
        assert!(parse(r#"{"error":"x"}"#).is_some_and(|a| next_call(&a).is_none()));
    }

    #[test]
    fn the_next_call_is_the_first_booking() {
        let a = ahead(&payload(&[booking("2026-10-12", Some(V1), "Rev 2"), booking("2026-10-13", None, "")]));
        let next = next_call(&a).expect("a next call");
        assert_eq!(next.day, "2026-10-12");
        assert_eq!(next.project, "CB-0412 Con Spirito S1");
        assert_eq!(next.place.as_deref(), Some("Melrose Arch"));
        assert_eq!(next.sheet_href, Some(format!("/call-sheets/{V1}")));
        assert!(next.map_url.is_some());
    }

    #[test]
    fn only_google_maps_links_open() {
        assert!(is_map_url("https://www.google.com/maps/search/?api=1&query=x"));
        for bad in ["https://evil.example/maps", "http://www.google.com/maps/", "javascript:alert(1)", "https://www.google.com.evil.example/maps/"] {
            assert!(!is_map_url(bad), "{bad}");
        }
    }

    #[test]
    fn call_sheets_not_saved_yet_are_saved_once_with_a_clear_label() {
        let a = ahead(&payload(&[
            booking("2026-10-12", Some(V1), "Rev 2"),
            booking("2026-10-13", Some(V1), "Rev 2"), // a block shoot: the same sheet
            booking("2026-10-13", Some(V2), "Original"),
            booking("2026-10-13", None, ""),
        ]));
        let saved = vec![format!("{}/api/call-sheets/{V2}/pdf", crate::APP_ORIGIN)];
        let wanted = to_save(&a, &saved);
        assert_eq!(
            wanted,
            vec![ToSave {
                url: format!("{}/api/call-sheets/{V1}/pdf", crate::APP_ORIGIN),
                label: "CB-0412 Con Spirito S1 — Shoot Day 1 Rev 2 (2026-10-12)".into(),
            }]
        );
    }

    #[test]
    fn only_the_trackers_call_sheet_pdfs_are_saved() {
        let odd = r#"{"day":"2026-10-12","startsAt":"x","project":{"number":"1","name":"n"},"sheet":{"versionId":"0b5e0a52-3c4d-4e8f-9a1b-2c3d4e5f6a7b","pdf":"/api/invoices/1/pdf"}}"#;
        let bad_id = r#"{"day":"2026-10-12","startsAt":"x","project":{"number":"1","name":"n"},"sheet":{"versionId":"../x","pdf":"/api/call-sheets/../x/pdf"}}"#;
        let a = ahead(&payload(&[odd.to_string(), bad_id.to_string()]));
        assert!(to_save(&a, &[]).is_empty());
    }
}
