//! The mini bar: a tiny always-on-top window with the clock status, one
//! smart clock button and the unread count. Local UI only (`mini.html`):
//! Rust pushes state with `eval`, clicks come back through the four narrow
//! `mini_*` commands (see `permissions/mini.toml`). No credentials, no page
//! scraping — it reads the same managed state the tray already keeps.

use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};

/// What the mini panel shows. Pushed to the page as JSON.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct View {
    /// e.g. "Called in since 08:02" (the tray's status text, verbatim).
    pub status: String,
    /// State dot colour, mirroring the site's own palette.
    pub dot: String,
    pub action_label: String,
    /// Clock action for the button, or `None` when it must stay disabled.
    pub action: Option<String>,
    /// Raw ISO timestamp the live timer counts from ("" when off clock).
    pub since_iso: String,
    pub unread: u32,
    /// Newest actionable unread items (each links to its exact page).
    pub needs: Vec<NeedRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct NeedRow {
    pub label: String,
    pub body: String,
    pub href: String,
}

/// Derive the mini view from the tray clock, the unread count and the
/// actionable snapshot. Single-button toggle: out → Call in, in → Wrap,
/// break → Back from break. Anything unavailable (or a pending notice)
/// disables the button; the notice itself is read in the full app, never
/// skipped from here.
pub(crate) fn plan(
    clock: Option<&crate::clock::Clock>,
    unread: u32,
    needs: Vec<crate::notify::InboxEntry>,
) -> View {
    let planned = crate::clock::plan(clock);
    // Priority: Call in (out) → Back from break (break) → Wrap (in).
    let action = if planned.call_in {
        Some("in".to_string())
    } else if planned.back {
        Some("back".to_string())
    } else if planned.wrap {
        Some("wrap".to_string())
    } else {
        None
    };
    let (action_label, dot) = match action.as_deref() {
        Some("in") => ("Call in".to_string(), dot_for("out")),
        Some("back") => ("Back from break".to_string(), dot_for("break")),
        Some("wrap") => ("Wrap".to_string(), dot_for("in")),
        _ => ("Call in".to_string(), dot_for("out")),
    };
    // When the button is disabled the dot follows the real state, not grey.
    let dot = if action.is_none() {
        match clock {
            Some(c) => dot_for(c.state.as_str()),
            None => dot_for("out"),
        }
    } else {
        dot
    };
    View {
        status: planned.status,
        dot,
        action_label,
        action,
        since_iso: clock.map(|c| c.since_iso.clone()).unwrap_or_default(),
        unread,
        needs: needs
            .into_iter()
            .filter_map(|entry| {
                entry.href.map(|href| NeedRow { label: entry.title, body: entry.body, href })
            })
            .collect(),
    }
}

fn dot_for(state: &str) -> String {
    match state {
        "in" => "#2E8B57".to_string(),
        "break" => "#D98E04".to_string(),
        _ => "#A3A3AD".to_string(),
    }
}

/// Push the current view to the mini window. Cheap and idempotent; called
/// at the end of the clock and notification polls so the panel trails the
/// tray by at most one tick. Silent when the mini was never opened.
pub(crate) fn push<R: Runtime>(app: &AppHandle<R>) {
    let clock = app
        .try_state::<crate::clock::Last>()
        .and_then(|last| last.0.lock().ok().and_then(|slot| slot.clone()));
    let (unread, needs) = app
        .try_state::<crate::notify::Snapshot>()
        .map(|snap| {
            let unread = snap.unread.lock().ok().map(|slot| *slot).unwrap_or(0);
            let needs = snap.needs.lock().ok().map(|rows| rows.clone()).unwrap_or_default();
            (unread, needs)
        })
        .unwrap_or((0, Vec::new()));
    let view = plan(clock.as_ref(), unread, needs);
    let Ok(payload) = serde_json::to_string(&view) else { return };
    if let Some(mini) = app.get_webview_window("mini") {
        let script = format!("window.__cbbMiniShow && window.__cbbMiniShow({payload})");
        let _ = mini.eval(script.as_str());
    }
}

/// Show the mini panel anchored to the tray (menu-bar-app feel), and push
/// fresh state into it. The main window hides at the same time: mini and
/// main swap, exactly one is ever visible. Showing also focuses the panel
/// so its buttons work first click; it hides again on blur.
pub(crate) fn show<R: Runtime>(app: &AppHandle<R>) {
    use tauri_plugin_positioner::{Position, WindowExt};
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.hide();
    }
    // Panel-only means menu-bar-app: no dock icon until show_main.
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    if let Some(mini) = app.get_webview_window("mini") {
        // Tray positions only resolve once the tray icon has reported its
        // position; before that they error and the window would sit wherever
        // it was created. Fall back to the screen corner instead of leaving
        // it stranded mid-screen.
        if mini.move_window(Position::TrayCenter).is_err() {
            let _ = mini.move_window(Position::TopRight);
        }
        let _ = mini.show();
        let _ = mini.set_focus();
        push(app);
    }
}

/// Toggle the mini panel (tray click / tray menu).
pub(crate) fn toggle<R: Runtime>(app: &AppHandle<R>) {
    let visible = app
        .get_webview_window("mini")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    if visible {
        hide(app);
    } else {
        show(app);
    }
}

/// Hide the mini panel. Quit stays in the tray.
pub(crate) fn hide<R: Runtime>(app: &AppHandle<R>) {
    if let Some(mini) = app.get_webview_window("mini") {
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

/// Open the full tracker and hide the panel (the reverse swap).
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
    crate::show_main(&app);
    Ok(())
}

/// Open one triage row's page in the main window (tracker host only),
/// mark it read like the bell, and bring the window forward.
fn open_href<R: Runtime>(app: &AppHandle<R>, href: &str) {
    use tauri::Manager;
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

/// Manual drag state. Tauri's own drag region stays as a backup, but this
/// path cannot fail: the page reports mouse movement in CSS pixels, Rust
/// converts by the window's scale factor and moves the physical window.
struct DragState {
    start_x: f64,
    start_y: f64,
    orig_x: i32,
    orig_y: i32,
    scale: f64,
}

static DRAG: std::sync::Mutex<Option<DragState>> = std::sync::Mutex::new(None);

fn drag_state<R: Runtime>(app: &AppHandle<R>) -> Option<DragState> {
    let mini = app.get_webview_window("mini")?;
    let pos = mini.outer_position().ok()?;
    let scale = mini.scale_factor().ok()?;
    Some(DragState { start_x: 0.0, start_y: 0.0, orig_x: pos.x, orig_y: pos.y, scale })
}

#[tauri::command]
pub(crate) fn mini_drag_start<R: Runtime>(app: AppHandle<R>, x: f64, y: f64) -> Result<(), String> {
    if !x.is_finite() || !y.is_finite() {
        return Err("Bad coordinates.".into());
    }
    let Some(mut state) = drag_state(&app) else {
        return Err("Mini unavailable.".into());
    };
    state.start_x = x;
    state.start_y = y;
    *DRAG.lock().map_err(|_| "Busy.".to_string())? = Some(state);
    Ok(())
}

#[tauri::command]
pub(crate) fn mini_drag_move<R: Runtime>(app: AppHandle<R>, x: f64, y: f64) -> Result<(), String> {
    if !x.is_finite() || !y.is_finite() {
        return Err("Bad coordinates.".into());
    }
    let guard = DRAG.lock().map_err(|_| "Busy.".to_string())?;
    let Some(state) = guard.as_ref() else {
        return Err("Not dragging.".into());
    };
    let nx = state.orig_x + ((x - state.start_x) * state.scale).round() as i32;
    let ny = state.orig_y + ((y - state.start_y) * state.scale).round() as i32;
    drop(guard);
    if let Some(mini) = app.get_webview_window("mini") {
        let _ = mini.set_position(tauri::PhysicalPosition::new(nx, ny));
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn mini_drag_end() -> Result<(), String> {
    if let Ok(mut guard) = DRAG.lock() {
        *guard = None;
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

    #[test]
    fn out_offers_call_in() {
        let view = plan(Some(&clock("out", "")), 0, vec![]);
        assert_eq!(view.action.as_deref(), Some("in"));
        assert_eq!(view.action_label, "Call in");
        assert_eq!(view.unread, 0);
        assert!(view.needs.is_empty());
    }

    #[test]
    fn in_offers_wrap_with_green_dot_and_epoch() {
        let view = plan(Some(&clock("in", "08:02")), 3, vec![need("Approval")]);
        assert_eq!(view.action.as_deref(), Some("wrap"));
        assert_eq!(view.action_label, "Wrap");
        assert_eq!(view.dot, "#2E8B57");
        assert_eq!(view.unread, 3);
        assert!(view.status.contains("08:02"));
        assert!(!view.since_iso.is_empty());
        assert_eq!(view.needs.len(), 1);
        assert_eq!(view.needs[0].href, "/approvals");
    }

    #[test]
    fn break_offers_back_with_amber_dot() {
        let view = plan(Some(&clock("break", "13:00")), 1, vec![]);
        assert_eq!(view.action.as_deref(), Some("back"));
        assert_eq!(view.dot, "#D98E04");
    }

    #[test]
    fn rows_without_links_never_reach_the_panel() {
        let mut entry = need("Approval");
        entry.href = None;
        let view = plan(Some(&clock("in", "08:02")), 1, vec![entry]);
        assert!(view.needs.is_empty());
    }

    #[test]
    fn nothing_to_press_without_a_time_sheet() {
        for clock in [None, Some(crate::clock::Clock { available: false, ..clock("in", "08:00") })] {
            let view = plan(clock.as_ref(), 0, vec![]);
            assert_eq!(view.action, None);
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
}
