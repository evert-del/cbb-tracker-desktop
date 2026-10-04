//! Time away from the computer, for the tracker's time sheet.
//!
//! The tracker's time sheet asks a person who was called in and comes back
//! after a while away: was that work, a break, or when did you wrap? It
//! never decides for them. A page cannot see time away (an editor works in
//! another app all day), so the shell measures one thing, how long the
//! computer has had no keyboard or mouse input, and when the person is back
//! it tells the page "away from X to Y", nothing else. No keys, screens,
//! apps or sites are read, and nothing is stored or sent anywhere: the page
//! decides whether to ask (only when its owner is called in, and only past
//! the company's own threshold).
//!
//! As with notify.rs the remote window gets no IPC: the shell `eval`s a DOM
//! event into the page (`cbb:away`) and leaves the same value on
//! `window.cbbLastAway` for a page that was mid-navigation.
//!
//! Idle time comes from the OS: GetLastInputInfo on Windows,
//! CGEventSourceSecondsSinceLastEventType on macOS, and GNOME's idle monitor
//! over D-Bus on Linux. Everywhere, a gap between polls (the computer slept)
//! counts as time away too.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager, Runtime};

const POLL_EVERY: Duration = Duration::from_secs(15);

/// Shorter than this is not "away". The page applies the company's own
/// threshold (5 minutes at least) on top.
pub(crate) const MIN_AWAY_SECS: u64 = 5 * 60;

/// Input within this long of a poll means the person is back.
const BACK_WITHIN_SECS: u64 = 30;

#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) struct Away {
    /// Unix seconds.
    pub from: u64,
    pub to: u64,
}

/// Turns polls of "seconds since the last input" into periods away.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    away_since: Option<u64>,
    last_poll: Option<u64>,
}

impl Tracker {
    /// One poll at `now` (unix seconds). `idle` is the OS's seconds since the
    /// last input, or `None` where it cannot be read (then only sleep counts).
    /// Returns a finished period away when the person is back.
    pub(crate) fn poll(&mut self, now: u64, idle: Option<u64>) -> Option<Away> {
        // The computer slept, or the app was suspended: from the last poll.
        if let Some(last) = self.last_poll {
            let gap = now.saturating_sub(last);
            if gap >= MIN_AWAY_SECS && gap > POLL_EVERY.as_secs() * 2 {
                self.away_since = Some(self.away_since.map_or(last, |since| since.min(last)));
            }
        }
        self.last_poll = Some(now);

        let idle = idle.unwrap_or(0);
        if idle >= MIN_AWAY_SECS {
            let since = now.saturating_sub(idle);
            self.away_since = Some(self.away_since.map_or(since, |known| known.min(since)));
            return None;
        }
        if idle <= BACK_WITHIN_SECS {
            if let Some(from) = self.away_since.take() {
                let to = now.saturating_sub(idle);
                if to.saturating_sub(from) >= MIN_AWAY_SECS {
                    return Some(Away { from, to });
                }
            }
        }
        None
    }
}

/// The script that hands one period away to the page.
pub(crate) fn away_script(away: Away) -> String {
    let (from, to) = (away.from * 1000, away.to * 1000);
    format!(
        "(function(){{var d={{from:{from},to:{to}}};window.cbbLastAway=d;\
         window.dispatchEvent(new CustomEvent('cbb:away',{{detail:d}}));}})()"
    )
}

/// Seconds since the last keyboard or mouse input, where the OS says.
#[cfg(target_os = "windows")]
pub(crate) fn idle_seconds() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: a correctly sized LASTINPUTINFO the call fills in.
    let ok = unsafe { GetLastInputInfo(&mut info) };
    if ok == 0 {
        return None;
    }
    // SAFETY: no arguments; both counters wrap at the same 49.7 days.
    let now = unsafe { GetTickCount() };
    Some(u64::from(now.wrapping_sub(info.dwTime)) / 1000)
}

#[cfg(target_os = "macos")]
pub(crate) fn idle_seconds() -> Option<u64> {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    }
    // kCGEventSourceStateCombinedSessionState, kCGAnyInputEventType.
    // SAFETY: a pure query with constant arguments.
    let seconds = unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) };
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds as u64)
}

#[cfg(target_os = "linux")]
pub(crate) fn idle_seconds() -> Option<u64> {
    let output = std::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.gnome.Mutter.IdleMonitor",
            "--object-path",
            "/org/gnome/Mutter/IdleMonitor/Core",
            "--method",
            "org.gnome.Mutter.IdleMonitor.GetIdletime",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_mutter_idle(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub(crate) fn idle_seconds() -> Option<u64> {
    None
}

/// GNOME answers "(uint64 12345,)" in milliseconds.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_mutter_idle(raw: &str) -> Option<u64> {
    let digits: String = raw
        .trim()
        .trim_start_matches('(')
        .trim_start_matches("uint64")
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse::<u64>().ok().map(|ms| ms / 1000)
}

/// True where the shell can tell time away from input (not only from sleep),
/// so the page knows the prompt is real. Read once at start.
pub(crate) fn supported() -> bool {
    idle_seconds().is_some()
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Start the background poller for the app's life.
pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        let mut tracker = Tracker::default();
        loop {
            std::thread::sleep(POLL_EVERY);
            let Some(away) = tracker.poll(unix_now(), idle_seconds()) else {
                continue;
            };
            let Some(window) = app.get_webview_window("main") else {
                continue;
            };
            let on_tracker = window
                .url()
                .map(|url| url.host_str() == Some(crate::APP_HOST))
                .unwrap_or(false);
            if on_tracker {
                let _ = window.eval(away_script(away));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_800_000_000;

    #[test]
    fn short_pauses_are_not_away() {
        let mut tracker = Tracker::default();
        for (n, idle) in [0, 60, 240, 10].into_iter().enumerate() {
            assert_eq!(tracker.poll(T0 + n as u64 * 15, Some(idle)), None);
        }
    }

    #[test]
    fn coming_back_after_a_while_reports_once() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.poll(T0, Some(0)), None);
        // 20 minutes without input, polled along the way.
        assert_eq!(tracker.poll(T0 + 600, Some(600)), None);
        assert_eq!(tracker.poll(T0 + 1200, Some(1200)), None);
        // Back: the last input was 5 seconds before this poll.
        assert_eq!(tracker.poll(T0 + 1215, Some(5)), Some(Away { from: T0, to: T0 + 1210 }));
        assert_eq!(tracker.poll(T0 + 1230, Some(3)), None);
    }

    #[test]
    fn sleep_counts_as_away_without_idle_data() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.poll(T0, None), None);
        // The next poll comes 40 minutes later: the computer slept.
        assert_eq!(tracker.poll(T0 + 2400, None), Some(Away { from: T0, to: T0 + 2400 }));
    }

    #[test]
    fn sleep_and_idle_together_start_at_the_earlier() {
        let mut tracker = Tracker::default();
        tracker.poll(T0, Some(0));
        tracker.poll(T0 + 400, Some(400));
        // Slept for an hour, woke up idle, then typed.
        assert_eq!(tracker.poll(T0 + 4000, Some(3600)), None);
        assert_eq!(tracker.poll(T0 + 4015, Some(2)), Some(Away { from: T0, to: T0 + 4013 }));
    }

    #[test]
    fn the_page_gets_milliseconds_and_an_event() {
        let script = away_script(Away { from: 10, to: 20 });
        assert!(script.contains("from:10000,to:20000"));
        assert!(script.contains("window.cbbLastAway=d"));
        assert!(script.contains("new CustomEvent('cbb:away',{detail:d})"));
    }

    #[test]
    fn reads_gnomes_answer() {
        assert_eq!(parse_mutter_idle("(uint64 125300,)\n"), Some(125));
        assert_eq!(parse_mutter_idle("(uint64 0,)"), Some(0));
        assert_eq!(parse_mutter_idle("Error: no such service"), None);
    }
}
