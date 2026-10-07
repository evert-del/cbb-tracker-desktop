//! The quick panel: the menu-bar icon's own window (left-click), drawn in
//! the tracker's style: the time sheet with a live timer and its clock
//! taps, the newest notifications that need you, shortcuts and settings.
//! The native tray menu stays on right-click (and is all there is on Linux,
//! where tray clicks are not reported). Local UI only (`mini.html`): Rust
//! pushes state with `eval`, clicks come back through the narrow `mini_*`
//! commands (see `permissions/mini.toml`). No credentials, no page
//! scraping: it reads the same managed state the tray already keeps.

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};

/// What the panel shows. Pushed to the page as JSON.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct View {
    /// "in", "break", "out", or "none" when there is no time sheet.
    pub state: String,
    /// e.g. "Called in since 08:02" (the tray's status, without its prefix).
    pub status: String,
    /// The clock taps that make sense now, in menu order.
    pub actions: Vec<ClockAction>,
    /// Raw ISO timestamp the live timer counts from ("" when off clock).
    pub since_iso: String,
    /// Today's worked and break minutes so far (from the tracker's segments).
    pub worked_min: u64,
    pub break_min: u64,
    pub unread: u32,
    /// Newest actionable unread items (each links to its exact page).
    pub needs: Vec<NeedRow>,
    /// Unread notifications by kind, most first ("3 Approval · 2 Phase").
    pub summary: Vec<crate::notify::LabelCount>,
    pub version: String,
    pub autostart: bool,
    /// Closing the main window quits the app (close.rs) instead of hiding it.
    pub close_quits: bool,
    pub pinned: bool,
    /// Shrunk to the mini timer (only while pinned).
    pub compact: bool,
    /// "macos", "windows" or "linux": the panel follows each system's own
    /// look (corner radii, typeface, wording).
    pub platform: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ClockAction {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct NeedRow {
    pub label: String,
    pub body: String,
    pub href: String,
}

/// Derive the panel view from the tray clock and the notification snapshot.
/// The taps are the tray's own (clock::plan); a pending notice is handled
/// by clock::act, which opens the tracker so it is read, never skipped.
pub(crate) fn plan(
    clock: Option<&crate::clock::Clock>,
    unread: u32,
    needs: Vec<crate::notify::InboxEntry>,
    now: u64,
) -> View {
    let planned = crate::clock::plan(clock);
    let available = clock.filter(|c| c.available);
    let mut actions = Vec::new();
    for (on, id, label) in [
        (planned.call_in, "in", "Call in"),
        (planned.back, "back", "Back from break"),
        (planned.take_break, "break", "Break"),
        (planned.wrap, "wrap", "Wrap"),
    ] {
        if on {
            actions.push(ClockAction { id: id.into(), label: label.into() });
        }
    }
    let (worked_min, break_min) = available
        .map(|c| crate::clock::day_totals(&c.today, now))
        .unwrap_or((0, 0));
    View {
        state: available.map(|c| c.state.clone()).unwrap_or_else(|| "none".into()),
        status: planned
            .status
            .strip_prefix("Time sheet: ")
            .unwrap_or(&planned.status)
            .to_string(),
        actions,
        since_iso: available.map(|c| c.since_iso.clone()).unwrap_or_default(),
        worked_min,
        break_min,
        unread,
        needs: needs
            .into_iter()
            .filter_map(|entry| {
                entry.href.map(|href| NeedRow { label: entry.title, body: entry.body, href })
            })
            .collect(),
        summary: Vec::new(),
        version: env!("CARGO_PKG_VERSION").into(),
        autostart: false,
        close_quits: false,
        pinned: false,
        compact: false,
        platform: std::env::consts::OS.into(),
    }
}

/// Push the current view to the panel. Cheap and idempotent; called at the
/// end of the clock and notification polls so the panel trails the tray by
/// at most one tick. Silent when the panel was never opened.
pub(crate) fn push<R: Runtime>(app: &AppHandle<R>) {
    let clock = app
        .try_state::<crate::clock::Last>()
        .and_then(|last| last.0.lock().ok().and_then(|slot| slot.clone()));
    let (unread, needs, summary) = app
        .try_state::<crate::notify::Snapshot>()
        .map(|snap| {
            let unread = snap.unread.lock().ok().map(|slot| *slot).unwrap_or(0);
            let needs = snap.needs.lock().ok().map(|rows| rows.clone()).unwrap_or_default();
            let summary = snap.summary.lock().ok().map(|rows| rows.clone()).unwrap_or_default();
            (unread, needs, summary)
        })
        .unwrap_or((0, Vec::new(), Vec::new()));
    let mut view = plan(clock.as_ref(), unread, needs, crate::clock::unix_now());
    view.autostart = crate::autostart_enabled(app);
    view.close_quits = crate::close::quits(app);
    view.pinned = is_pinned();
    view.compact = is_compact();
    view.summary = summary;
    let Ok(payload) = serde_json::to_string(&view) else { return };
    if let Some(mini) = app.get_webview_window("mini") {
        let script = format!("window.__cbbMiniShow && window.__cbbMiniShow({payload})");
        let _ = mini.eval(script.as_str());
    }
}

/// Pinned: the panel floats on top wherever it was dragged and stays open
/// when it loses focus (a floating timer). Unpinned it is a drop-down under
/// the tray icon. For this run of the app only.
static PINNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn is_pinned() -> bool {
    PINNED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Pinned and shrunk to the mini timer: status, timer, the main clock tap
/// and the notification summary in one small strip.
static COMPACT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn is_compact() -> bool {
    COMPACT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Window sizes (logical px, including the margin the card's shadow needs).
const FULL_SIZE: (f64, f64) = (372.0, 576.0);
const COMPACT_SIZE: (f64, f64) = (372.0, 140.0);

fn set_compact<R: Runtime>(app: &AppHandle<R>, compact: bool) {
    COMPACT.store(compact, std::sync::atomic::Ordering::Relaxed);
    let (w, h) = if compact { COMPACT_SIZE } else { FULL_SIZE };
    if let Some(mini) = app.get_webview_window("mini") {
        let _ = mini.set_size(tauri::LogicalSize::new(w, h));
    }
}

/// When the panel last hid itself. Clicking the tray icon while the panel
/// is open first blurs it (it hides), then delivers the click: without this
/// the click would open it straight back up.
static LAST_HIDDEN: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Show the panel with fresh state, focused so its buttons work on the
/// first click: under the tray icon, or where it was dragged when pinned.
/// Unpinned, it hides again when it loses focus.
/// The main window is left as it is: the panel is a drop-down, not a swap.
pub(crate) fn show<R: Runtime>(app: &AppHandle<R>) {
    if let Some(mini) = app.get_webview_window("mini") {
        // A pinned panel reopens where it was dragged to.
        if !is_pinned() {
            place_under_tray(&mini);
        }
        let _ = mini.show();
        let _ = mini.set_focus();
        push(app);
    }
}

/// Drop the panel down from the tray icon.
fn place_under_tray<R: Runtime>(mini: &tauri::WebviewWindow<R>) {
    use tauri_plugin_positioner::{Position, WindowExt};
    // macOS: the menu bar is at the top, so the panel drops down from the
    // icon. Windows: the taskbar is usually at the bottom, so it rises above
    // the icon. Linux reports no tray position (or clicks: there the panel
    // opens from the tray menu or the shortcut), so it sits in the top-right
    // corner, where most panels keep the tray. Tray positions also fail until
    // the icon has reported where it is; the corner covers that too.
    let at = if cfg!(target_os = "macos") {
        Position::TrayBottomCenter
    } else {
        Position::TrayCenter
    };
    if mini.move_window(at).is_err() {
        let fallback = if cfg!(target_os = "windows") { Position::BottomRight } else { Position::TopRight };
        let _ = mini.move_window(fallback);
    }
}

/// Toggle the panel (tray click, tray menu, Cmd+Shift+M / Ctrl+Alt+M).
pub(crate) fn toggle<R: Runtime>(app: &AppHandle<R>) {
    let visible = app
        .get_webview_window("mini")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    let just_hidden = LAST_HIDDEN
        .lock()
        .ok()
        .and_then(|at| *at)
        .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(300));
    if visible {
        hide(app);
    } else if !just_hidden {
        show(app);
    }
}

/// Hide the panel. Quit stays in the tray.
pub(crate) fn hide<R: Runtime>(app: &AppHandle<R>) {
    if let Some(mini) = app.get_webview_window("mini") {
        if mini.is_visible().unwrap_or(false) {
            if let Ok(mut at) = LAST_HIDDEN.lock() {
                *at = Some(std::time::Instant::now());
            }
        }
        let _ = mini.hide();
    }
}

fn is_allowed(action: &str) -> bool {
    matches!(action, "in" | "break" | "back" | "wrap")
}

#[tauri::command]
pub(crate) fn mini_action<R: Runtime>(app: AppHandle<R>, action: String) -> Result<(), String> {
    if !is_allowed(&action) {
        return Err("Unknown action.".into());
    }
    crate::clock::act(&app, &action);
    Ok(())
}

/// Open the full tracker and hide the panel.
fn expand_to_main<R: Runtime>(app: &AppHandle<R>) {
    hide(app);
    crate::show_main(app);
}

#[tauri::command]
pub(crate) fn mini_expand<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    expand_to_main(&app);
    Ok(())
}

#[tauri::command]
pub(crate) fn mini_expand_notifications<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    if let (Some(window), Ok(url)) = (
        app.get_webview_window("main"),
        format!("{}/notifications", crate::APP_ORIGIN).parse::<url::Url>(),
    ) {
        let _ = window.navigate(url);
    }
    expand_to_main(&app);
    Ok(())
}

/// Open one row's page in the main window (tracker host only) and bring
/// the window forward.
fn open_href<R: Runtime>(app: &AppHandle<R>, href: &str) {
    if !href.starts_with('/') {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        if let Ok(url) = format!("{}{}", crate::APP_ORIGIN, href).parse::<url::Url>() {
            if crate::webview_may_load(&url) {
                let _ = window.navigate(url);
            }
        }
    }
    expand_to_main(app);
}

#[tauri::command]
pub(crate) fn mini_open<R: Runtime>(app: AppHandle<R>, href: String) -> Result<(), String> {
    if !href.starts_with('/') {
        return Err("Unknown destination.".into());
    }
    open_href(&app, &href);
    Ok(())
}

#[tauri::command]
pub(crate) fn mini_hide<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    hide(&app);
    Ok(())
}

/// Pin the panel as a floating timer, or unpin it back into a drop-down
/// under the tray icon (it then closes on the next click elsewhere).
#[tauri::command]
pub(crate) fn mini_pin<R: Runtime>(app: AppHandle<R>, pinned: bool) -> Result<(), String> {
    PINNED.store(pinned, std::sync::atomic::Ordering::Relaxed);
    if !pinned {
        // The drop-down is always the full panel.
        set_compact(&app, false);
        if let Some(mini) = app.get_webview_window("mini") {
            place_under_tray(&mini);
        }
    }
    push(&app);
    Ok(())
}

/// Shrink the pinned panel to the mini timer, or grow it back.
#[tauri::command]
pub(crate) fn mini_compact<R: Runtime>(app: AppHandle<R>, compact: bool) -> Result<(), String> {
    if compact && !is_pinned() {
        return Err("Pin the panel first.".into());
    }
    set_compact(&app, compact);
    push(&app);
    Ok(())
}

fn is_menu_item(item: &str) -> bool {
    matches!(
        item,
        "offline" | "update" | "diagnostics" | "autostart" | "close-keep" | "close-quit" | "quit"
    )
}

/// The panel's shortcuts and settings: the same actions as the tray menu.
#[tauri::command]
pub(crate) fn mini_menu<R: Runtime>(app: AppHandle<R>, item: String) -> Result<(), String> {
    if !is_menu_item(&item) {
        return Err("Unknown item.".into());
    }
    match item.as_str() {
        "offline" => {
            hide(&app);
            crate::show_library(&app);
        }
        "update" => {
            hide(&app);
            crate::updater::check_now(app.clone());
        }
        "diagnostics" => {
            hide(&app);
            crate::diagnostics::show(&app);
        }
        "autostart" => {
            crate::toggle_autostart(&app);
            push(&app);
        }
        "close-keep" | "close-quit" => {
            crate::close::set_quits(&app, item == "close-quit");
            push(&app);
        }
        "quit" => app.exit(0),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(state: &str, since: &str) -> crate::clock::Clock {
        crate::clock::Clock {
            available: true,
            state: state.into(),
            since: since.into(),
            since_iso: "2026-10-05T06:00:00.000Z".into(),
            today: Vec::new(),
            notice: false,
        }
    }

    fn need(label: &str) -> crate::notify::InboxEntry {
        crate::notify::InboxEntry {
            id: "1".into(),
            title: label.into(),
            body: "body".into(),
            href: Some("/approvals".into()),
        }
    }

    fn ids(view: &View) -> Vec<&str> {
        view.actions.iter().map(|a| a.id.as_str()).collect()
    }

    const NOW: u64 = 1_791_360_000;

    #[test]
    fn out_offers_call_in() {
        let view = plan(Some(&clock("out", "")), 0, vec![], NOW);
        assert_eq!(ids(&view), ["in"]);
        assert_eq!(view.actions[0].label, "Call in");
        assert_eq!(view.state, "out");
        assert_eq!(view.status, "Off the clock");
        assert_eq!(view.unread, 0);
        assert!(view.needs.is_empty());
    }

    #[test]
    fn in_offers_break_and_wrap_with_epoch() {
        let view = plan(Some(&clock("in", "08:02")), 3, vec![need("Approval")], NOW);
        assert_eq!(ids(&view), ["break", "wrap"]);
        assert_eq!(view.state, "in");
        assert_eq!(view.status, "Called in since 08:02");
        assert_eq!(view.unread, 3);
        assert!(!view.since_iso.is_empty());
        assert_eq!(view.needs.len(), 1);
        assert_eq!(view.needs[0].href, "/approvals");
    }

    #[test]
    fn break_offers_back_then_wrap() {
        let view = plan(Some(&clock("break", "13:00")), 1, vec![], NOW);
        assert_eq!(ids(&view), ["back", "wrap"]);
        assert_eq!(view.state, "break");
    }

    #[test]
    fn today_totals_come_from_the_segments() {
        let mut c = clock("in", "08:00");
        c.today = vec![
            crate::clock::DaySegment {
                kind: "WORK".into(),
                started_at: "2026-10-07T06:00:00.000Z".into(),
                ended_at: Some("2026-10-07T08:00:00.000Z".into()),
            },
            crate::clock::DaySegment {
                kind: "BREAK".into(),
                started_at: "2026-10-07T08:00:00.000Z".into(),
                ended_at: Some("2026-10-07T08:30:00.000Z".into()),
            },
        ];
        let view = plan(Some(&c), 0, vec![], NOW);
        assert_eq!((view.worked_min, view.break_min), (120, 30));
    }

    #[test]
    fn rows_without_links_never_reach_the_panel() {
        let mut entry = need("Approval");
        entry.href = None;
        let view = plan(Some(&clock("in", "08:02")), 1, vec![entry], NOW);
        assert!(view.needs.is_empty());
    }

    #[test]
    fn nothing_to_press_without_a_time_sheet() {
        for clock in [None, Some(crate::clock::Clock { available: false, ..clock("in", "08:00") })] {
            let view = plan(clock.as_ref(), 0, vec![], NOW);
            assert!(view.actions.is_empty());
            assert_eq!(view.state, "none");
            assert_eq!(view.since_iso, "");
        }
    }

    #[test]
    fn only_known_actions_pass() {
        for action in ["in", "break", "back", "wrap"] {
            assert!(is_allowed(action), "{action}");
        }
        for action in ["", "admin", "in;rm", "../escape"] {
            assert!(!is_allowed(action), "{action}");
        }
    }

    #[test]
    fn only_known_menu_items_pass() {
        for item in ["offline", "update", "diagnostics", "autostart", "close-keep", "close-quit", "quit"] {
            assert!(is_menu_item(item), "{item}");
        }
        for item in ["", "show", "quit;", "eval"] {
            assert!(!is_menu_item(item), "{item}");
        }
    }
}
