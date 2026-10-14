//! "Not logged today": the blanks in someone's working day, for people who
//! bill by the hour and start on something, then forget the timer for the
//! rest of the day.
//!
//! The day is their own time sheet for today (clock.rs: the worked stretches
//! from calling in to wrapping, breaks left out), as the tray already reads
//! it. Where the company doesn't use the time sheet, it's when this computer
//! was in use today: the first and last moment someone was at it, less the
//! times they were away (idle.rs). Take away what's on the timer today
//! (hourly.rs), and what's left, 15 minutes or more, is a blank.
//!
//! - Worked out on this computer, from what the panel already has. Nothing
//!   new is sent or stored: the in-use times and "Not work" answers live in
//!   memory for the day. Only a blank they choose to log reaches the
//!   tracker, through Add time, the same as adding time by hand.
//! - It asks; it never logs or decides. "Not work" puts a blank away for
//!   the day.

use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};

/// Blanks shorter than this aren't worth asking about.
pub(crate) const MIN_GAP_SECS: u64 = 15 * 60;
/// Under this much idle time, someone is at the computer.
const ACTIVE_IDLE_SECS: u64 = 2 * 60;

/// `spans` less every one of `taken`, as the stretches of at least `min`
/// seconds that are left (unix seconds, in order).
pub(crate) fn gaps(spans: &[(u64, u64)], taken: &[(u64, u64)], min: u64) -> Vec<(u64, u64)> {
    let mut taken: Vec<(u64, u64)> = taken.iter().copied().filter(|(a, b)| b > a).collect();
    taken.sort();
    let mut out = Vec::new();
    for &(start, end) in spans {
        let mut at = start;
        for &(a, b) in &taken {
            if b <= at || a >= end {
                continue;
            }
            if a > at {
                out.push((at, a.min(end)));
            }
            at = at.max(b);
            if at >= end {
                break;
            }
        }
        if at < end {
            out.push((at, end));
        }
    }
    out.into_iter().filter(|(a, b)| b.saturating_sub(*a) >= min).collect()
}

/// The time sheet's worked stretches today, the open one up to `now`.
pub(crate) fn worked(segments: &[crate::clock::DaySegment], now: u64) -> Vec<(u64, u64)> {
    segments
        .iter()
        .filter(|segment| segment.kind == "WORK")
        .filter_map(|segment| {
            let start = crate::clock::parse_iso(&segment.started_at)?;
            let end = segment.ended_at.as_deref().map_or(Some(now), crate::clock::parse_iso)?.min(now);
            (end > start).then_some((start, end))
        })
        .collect()
}

/// The day's in-use time at this computer, as far as the app has seen it.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct InUse {
    /// The Johannesburg day it's for (yyyy-mm-dd).
    pub day: String,
    pub first: Option<u64>,
    pub last: Option<u64>,
    /// Times away from the computer (idle.rs), unix seconds.
    pub away: Vec<(u64, u64)>,
}

impl InUse {
    /// At the computer at `now` (on `day`): a new day starts afresh.
    pub(crate) fn active(&mut self, day: &str, now: u64) {
        if self.day != day {
            *self = InUse { day: day.to_string(), ..InUse::default() };
        }
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    pub(crate) fn away(&mut self, from: u64, to: u64) {
        if to > from {
            self.away.push((from, to));
        }
    }

    /// The in-use stretches of the day: from the first moment to the last,
    /// less the times away.
    pub(crate) fn spans(&self) -> Vec<(u64, u64)> {
        match (self.first, self.last) {
            (Some(first), Some(last)) if last > first => gaps(&[(first, last)], &self.away, 1),
            _ => Vec::new(),
        }
    }
}

/// One blank as the panel shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GapView {
    /// Milliseconds, for the panel's clock and Add time.
    pub from: u64,
    pub to: u64,
    pub minutes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct View {
    pub gaps: Vec<GapView>,
    pub minutes: u64,
    /// What the day was read from: "time sheet" or "computer".
    pub from: &'static str,
}

struct State {
    in_use: InUse,
    /// "Not work" today: the blanks' starts and ends.
    dismissed: (String, Vec<(u64, u64)>),
}

static STATE: Mutex<State> = Mutex::new(State {
    in_use: InUse { day: String::new(), first: None, last: None, away: Vec::new() },
    dismissed: (String::new(), Vec::new()),
});

fn today(now: u64) -> String {
    crate::hourly::iso_utc(now + 2 * 3600)[..10].to_string()
}

/// Each idle poll (hourly.rs `on_idle`): at the computer if it was used lately.
pub(crate) fn note_idle(now: u64, idle: u64) {
    if idle < ACTIVE_IDLE_SECS {
        if let Ok(mut state) = STATE.lock() {
            state.in_use.active(&today(now), now);
        }
    }
}

/// Back after time away (hourly.rs `on_away`).
pub(crate) fn note_away(from: u64, to: u64) {
    if let Ok(mut state) = STATE.lock() {
        state.in_use.away(from, to);
    }
}

/// The day's blanks, or `None` with none (or without a timer).
pub(crate) fn view<R: Runtime>(app: &AppHandle<R>, today_entries: Option<&crate::hourly::Today>, running: Option<&crate::hourly::Running>) -> Option<View> {
    let now = crate::clock::unix_now();
    let day = today(now);
    let clock = app.try_state::<crate::clock::Last>().and_then(|last| last.0.lock().ok().and_then(|slot| slot.clone()));
    let sheet = clock.filter(|clock| clock.available).map(|clock| worked(&clock.today, now)).filter(|spans| !spans.is_empty());
    let state = STATE.lock().ok()?;
    let (spans, from) = match sheet {
        Some(spans) => (spans, "time sheet"),
        None if state.in_use.day == day => (state.in_use.spans(), "computer"),
        None => return None,
    };
    let mut taken: Vec<(u64, u64)> = today_entries
        .map(|t| {
            t.entries
                .iter()
                .filter_map(|e| {
                    let start = crate::clock::parse_iso(&e.started_at)?;
                    let end = e.ended_at.as_deref().map_or(Some(now), crate::clock::parse_iso)?;
                    Some((start, end))
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(start) = running.and_then(|r| crate::clock::parse_iso(&r.started_at)) {
        taken.push((start, now));
    }
    if state.dismissed.0 == day {
        taken.extend(state.dismissed.1.iter().copied());
    }
    let blanks = gaps(&spans, &taken, MIN_GAP_SECS);
    if blanks.is_empty() {
        return None;
    }
    let minutes = blanks.iter().map(|(a, b)| (b - a) / 60).sum();
    Some(View {
        gaps: blanks.iter().map(|&(a, b)| GapView { from: a * 1000, to: b * 1000, minutes: (b - a) / 60 }).collect(),
        minutes,
        from,
    })
}

/// "Not work": put a blank away for the rest of the day.
#[tauri::command]
pub(crate) fn mini_day_gap_dismiss<R: Runtime>(app: AppHandle<R>, from: u64, to: u64) -> Result<(), String> {
    let (from, to) = (from / 1000, to / 1000);
    if to <= from {
        return Err("Unknown time.".into());
    }
    let day = today(crate::clock::unix_now());
    if let Ok(mut state) = STATE.lock() {
        if state.dismissed.0 != day {
            state.dismissed = (day, Vec::new());
        }
        state.dismissed.1.push((from, to));
    }
    crate::mini::push(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600;
    const T: u64 = 1_760_000_000;

    #[test]
    fn the_blanks_are_the_day_less_what_is_logged() {
        // Worked 8 to 12 and 13 to 17; logged 8:00–9:30 and 15:00–15:10.
        let day = [(T, T + 4 * H), (T + 5 * H, T + 9 * H)];
        let logged = [(T, T + 3 * H / 2), (T + 7 * H, T + 7 * H + 600)];
        assert_eq!(gaps(&day, &logged, MIN_GAP_SECS), vec![
            (T + 3 * H / 2, T + 4 * H),
            (T + 5 * H, T + 7 * H),
            (T + 7 * H + 600, T + 9 * H),
        ]);
        // Fully logged, or blanks under 15 minutes: nothing.
        assert!(gaps(&day, &[(T - H, T + 10 * H)], MIN_GAP_SECS).is_empty());
        assert!(gaps(&[(T, T + 600)], &[], MIN_GAP_SECS).is_empty());
        // Overlapping entries in any order.
        assert_eq!(gaps(&[(T, T + 4 * H)], &[(T + 2 * H, T + 3 * H), (T + H, T + 5 * H / 2)], MIN_GAP_SECS),
            vec![(T, T + H), (T + 3 * H, T + 4 * H)]);
    }

    fn seg(kind: &str, start: u64, end: Option<u64>) -> crate::clock::DaySegment {
        crate::clock::DaySegment {
            kind: kind.into(),
            started_at: crate::hourly::iso_utc(start),
            ended_at: end.map(crate::hourly::iso_utc),
        }
    }

    #[test]
    fn the_time_sheet_day_leaves_out_breaks_and_runs_to_now() {
        let segments = [seg("WORK", T, Some(T + 2 * H)), seg("BREAK", T + 2 * H, Some(T + 5 * H / 2)), seg("WORK", T + 5 * H / 2, None)];
        assert_eq!(worked(&segments, T + 4 * H), vec![(T, T + 2 * H), (T + 5 * H / 2, T + 4 * H)]);
    }

    #[test]
    fn in_use_time_is_first_to_last_less_time_away_and_starts_afresh_each_day() {
        let mut in_use = InUse::default();
        in_use.active("2026-10-14", T);
        in_use.active("2026-10-14", T + 3 * H);
        in_use.away(T + H, T + 2 * H);
        assert_eq!(in_use.spans(), vec![(T, T + H), (T + 2 * H, T + 3 * H)]);
        in_use.active("2026-10-15", T + 24 * H);
        assert_eq!((in_use.first, in_use.away.len()), (Some(T + 24 * H), 0));
    }
}
