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
    /// Unread walkie messages, and the conversations they are in (walkie.rs).
    pub walkie_unread: u32,
    pub chats: Vec<crate::walkie::ChatRow>,
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
        walkie_unread: 0,
        chats: Vec::new(),
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
    view.walkie_unread = crate::walkie::unread(app);
    view.chats = crate::walkie::chats(app);
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

/// Tracker windows the drop-down panel put away when it opened: it's either
/// the panel or the tracker on screen, not both. `bring_tracker_back` (via
/// `show_main`: Open tracker, a notification, the tray's Show Tracker, a
/// deep link) shows them again; pinning the panel does too.
static PUT_AWAY: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Hide every visible tracker window (main and extra tabs / windows) and
/// remember which, for `bring_tracker_back`.
fn put_tracker_away<R: Runtime>(app: &AppHandle<R>) {
    let Ok(mut away) = PUT_AWAY.lock() else { return };
    for (label, window) in app.webview_windows() {
        let tracker = label == "main" || crate::is_extra_tracker_window(&label);
        if tracker && window.is_visible().unwrap_or(false) {
            let _ = window.hide();
            if !away.contains(&label) {
                away.push(label);
            }
        }
    }
}

/// Show the tracker windows the panel put away, as they were.
pub(crate) fn bring_tracker_back<R: Runtime>(app: &AppHandle<R>) {
    let labels = PUT_AWAY.lock().map(|mut away| std::mem::take(&mut *away)).unwrap_or_default();
    if labels.is_empty() {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    for label in labels {
        if let Some(window) = app.get_webview_window(&label) {
            let _ = window.show();
            let _ = window.unminimize();
        }
    }
}

/// Show the panel with fresh state, focused so its buttons work on the
/// first click: under the tray icon, or where it was dragged when pinned.
/// Unpinned, it hides again when it loses focus, and it takes the tracker
/// windows' place (put_tracker_away): either the panel or the tracker.
pub(crate) fn show<R: Runtime>(app: &AppHandle<R>) {
    // Signed out there's no panel: the tray icon opens sign in (session.rs).
    if !crate::session::signed_in() {
        crate::show_main(app);
        return;
    }
    if let Some(mini) = app.get_webview_window("mini") {
        // A pinned panel reopens where it was dragged to, over the tracker.
        if !is_pinned() {
            put_tracker_away(app);
            place_under_tray(&mini);
        }
        let _ = mini.show();
        let _ = mini.set_focus();
        push(app);
    }
}

/// A tracker window was minimised: instead of sitting in the Dock / taskbar
/// it is put away (as the drop-down puts it away) and the panel floats as the
/// mini timer, so the timer and clock buttons stay in reach. Open tracker
/// brings it back (bring_tracker_back). An already pinned panel keeps its
/// size and place.
pub(crate) fn minimised_to_panel<R: Runtime>(app: &AppHandle<R>, label: &str) {
    if !crate::session::signed_in() {
        return;
    }
    let Some(window) = app.get_webview_window(label) else { return };
    {
        let Ok(mut away) = PUT_AWAY.lock() else { return };
        if away.iter().any(|l| l == label) {
            return;
        }
        away.push(label.to_string());
    }
    let _ = window.hide();
    let newly_pinned = !is_pinned();
    if newly_pinned {
        PINNED.store(true, std::sync::atomic::Ordering::Relaxed);
        set_compact(app, true);
    }
    if let Some(mini) = app.get_webview_window("mini") {
        if newly_pinned {
            place_under_tray(&mini);
        }
        let _ = mini.show();
    }
    push(app);
    if newly_pinned {
        crate::analytics::panel_mode(app, true, true);
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
/// `via` says which, for analytics.rs: tray_icon, tray_menu or shortcut.
pub(crate) fn toggle<R: Runtime>(app: &AppHandle<R>, via: &'static str) {
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
        crate::analytics::panel_opened(app, via);
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

/// Signed out: the panel closes, unpinned and full size (session.rs). The
/// tracker windows it put away come back, showing sign in.
pub(crate) fn sign_out<R: Runtime>(app: &AppHandle<R>) {
    PINNED.store(false, std::sync::atomic::Ordering::Relaxed);
    if is_compact() {
        set_compact(app, false);
    }
    let was_open = app
        .get_webview_window("mini")
        .and_then(|mini| mini.is_visible().ok())
        .unwrap_or(false);
    hide(app);
    if was_open {
        crate::show_main(app);
    }
}

/// The one-time "there's a quick panel" tip has been shown (settings.json).
const INTRODUCED: &str = "panel_introduced";

/// Scheduled for this run already (the tip waits for a signed-in page).
static TIP_SCHEDULED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The tip's words, in each system's terms for where the panel lives.
pub(crate) fn tip_text(os: &str) -> (&'static str, &'static str) {
    match os {
        "macos" => (
            "Your time sheet is in the menu bar",
            "Click the CoolerBox icon at the top of your screen for your timer, clock buttons and notifications. Pin it to keep a small timer on screen.",
        ),
        "windows" => (
            "Your time sheet is in the system tray",
            "Click the CoolerBox icon by the clock (it may be under the ^ arrow) for your timer, clock buttons and notifications. Pin it to keep a small timer on screen.",
        ),
        _ => (
            "Your time sheet has a quick panel",
            "Choose Quick panel from the CoolerBox tray icon, or press Ctrl+Alt+M, for your timer, clock buttons and notifications. Pin it to keep a small timer on screen.",
        ),
    }
}

/// Once per install: a few seconds after the tracker's first signed-in page
/// has loaded in the main window, a small in-app message (nav_bar.js toast,
/// bottom of the window, dismissible, gone after 15 s) says where the quick
/// panel lives, with "Show me". Nothing permanent is added to the page.
pub(crate) fn maybe_introduce<R: Runtime>(app: &AppHandle<R>) {
    if crate::close::flag(app, INTRODUCED)
        || TIP_SCHEDULED.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(10));
        let visible = app
            .get_webview_window("main")
            .and_then(|main| main.is_visible().ok())
            .unwrap_or(false);
        if !visible || !crate::session::signed_in() {
            // Try again on a later page load, when someone is looking.
            TIP_SCHEDULED.store(false, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        crate::close::set_flag(&app, INTRODUCED, true);
        let (title, detail) = tip_text(std::env::consts::OS);
        crate::download::toast(
            &app,
            serde_json::json!({
                "kind": "ok",
                "title": title,
                "detail": detail,
                "action": { "label": "Show me", "panel": true },
            }),
        );
    });
}

// ── the mood check-in, asked in the panel after a tray / panel / shortcut ──
// call in or wrap (the tracker's src/lib/mood-checkin.ts is the source of
// truth; these rules mirror it so a bad answer never leaves the app).
//
// It is someone's state of mind: no score, word, cause or note is ever put in
// analytics, logs, crash reports or storage here. The answer goes straight
// from the panel to the tracker page's own session and is not kept.

/// The words offered for each pair of scores (MOOD_BANDS on the tracker).
const MOOD_BANDS: [(u8, u8, [&str; 3]); 5] = [
    (1, 2, ["Drained", "Overwhelmed", "Low"]),
    (3, 4, ["Tired", "Stressed", "Flat"]),
    (5, 6, ["Okay", "Steady", "Meh"]),
    (7, 8, ["Good", "Focused", "Motivated"]),
    (9, 10, ["Great", "Energised", "On fire"]),
];
const MOOD_CAUSES: [&str; 4] = ["WORK", "PERSONAL", "BOTH", "UNSAID"];
const MOOD_NOTE_MAX: usize = 280;

/// The check-in grew a mini timer to the full panel: shrink it back after.
static MOOD_SHRINK_AFTER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether `word` is one of the words offered for `score`.
fn mood_word_fits(score: u8, word: &str) -> bool {
    MOOD_BANDS
        .iter()
        .any(|(from, to, words)| (*from..=*to).contains(&score) && words.contains(&word))
}

/// The request body for POST /api/time-sheet/mood, built only from the
/// fields the tracker accepts, or why the panel's request isn't one.
pub(crate) fn mood_body(input: &serde_json::Value) -> Result<serde_json::Value, &'static str> {
    use serde_json::{json, Value};
    let moment = || match input.get("moment").and_then(Value::as_str) {
        Some(moment @ ("IN" | "WRAP")) => Ok(moment),
        _ => Err("Call in or wrap?"),
    };
    match input.get("intent").and_then(Value::as_str) {
        Some("answer") => {
            let moment = moment()?;
            let score = input
                .get("score")
                .and_then(Value::as_u64)
                .filter(|score| (1..=10).contains(score))
                .ok_or("Pick a number from 1 to 10.")? as u8;
            let keyword = match input.get("keyword") {
                None | Some(Value::Null) => None,
                Some(Value::String(word)) if mood_word_fits(score, word.trim()) => Some(word.trim().to_string()),
                Some(_) => return Err("Pick one of the words, or say it in your own."),
            };
            let cause = match input.get("cause") {
                None | Some(Value::Null) => None,
                Some(Value::String(cause)) if MOOD_CAUSES.contains(&cause.as_str()) => Some(cause.clone()),
                Some(_) => return Err("Unknown cause."),
            };
            let note = match input.get("note") {
                None | Some(Value::Null) => None,
                Some(Value::String(note)) if note.trim().is_empty() => None,
                Some(Value::String(note)) if note.trim().chars().count() <= MOOD_NOTE_MAX => Some(note.trim().to_string()),
                Some(_) => return Err("Keep it to 280 characters."),
            };
            Ok(json!({ "intent": "answer", "moment": moment, "score": score, "keyword": keyword, "cause": cause, "note": note }))
        }
        Some("skip") => Ok(json!({ "intent": "skip", "moment": moment()? })),
        Some("talk") => match input.get("yes").and_then(Value::as_bool) {
            Some(yes) => Ok(json!({ "intent": "talk", "yes": yes })),
            None => Err("Yes or no?"),
        },
        _ => Err("Unknown request."),
    }
}

/// Sends a mood request through the tracker page's own session and leaves
/// only what the panel needs on `window.__cbbMoodResult` (whether to offer a
/// chat, who was told, or the tracker's error), never the answer itself.
/// Then `cbb:clock-changed`, so the web clock doesn't ask again.
pub(crate) fn mood_script(body: &serde_json::Value) -> String {
    format!(
        r#"(function (body) {{
  window.__cbbMoodResult = '';
  fetch('/api/time-sheet/mood', {{ method: 'POST', credentials: 'same-origin', headers: {{ 'Content-Type': 'application/json' }}, body: JSON.stringify(body) }})
    .then(function (r) {{ return r.json().catch(function () {{ return {{}}; }}).then(function (b) {{ return {{ ok: r.ok, b: b || {{}} }}; }}); }})
    .then(function (x) {{
      window.__cbbMoodResult = JSON.stringify(x.ok
        ? {{ ok: true, offerTalk: !!x.b.offerTalk, told: Array.isArray(x.b.told) ? x.b.told : [] }}
        : {{ ok: false, error: x.b.error || "That didn't save. Try again." }});
      if (x.ok) window.dispatchEvent(new CustomEvent('cbb:clock-changed'));
    }})
    .catch(function () {{ window.__cbbMoodResult = JSON.stringify({{ ok: false, error: 'You seem to be offline. Try again in a moment.' }}); }});
}})({body})"#
    )
}

/// Returns and clears the last mood request's result ("" while waiting).
const TAKE_MOOD_RESULT_JS: &str =
    "(function () { var v = window.__cbbMoodResult || ''; window.__cbbMoodResult = ''; return v; })()";

/// Shows the tracker's greeting after a clock tap ("Good morning, Sam.
/// Have a good day."): a line in the panel when it is showing, otherwise
/// a desktop notification (a tray or shortcut tap).
pub(crate) fn greet<R: Runtime>(app: &AppHandle<R>, greeting: &str) {
    let mini = app
        .get_webview_window("mini")
        .filter(|mini| mini.is_visible().unwrap_or(false));
    if let Some(mini) = mini {
        let text = serde_json::Value::from(greeting);
        let _ = mini.eval(format!("window.__cbbMiniGreet && window.__cbbMiniGreet({text})"));
        return;
    }
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title("CoolerBox Tracker").body(greeting).show();
}

/// Ask how they are, in the panel: `moment` is "IN" or "WRAP". Opens the
/// panel (full size) when it isn't showing. Support stays inside the
/// company: the only route is the "someone to check in" request to HR.
/// The tap's greeting, if any, sits above the question.
pub(crate) fn ask_mood<R: Runtime>(app: &AppHandle<R>, moment: &str, greeting: Option<&str>) {
    if is_compact() {
        MOOD_SHRINK_AFTER.store(true, std::sync::atomic::Ordering::Relaxed);
        set_compact(app, false);
        push(app);
    }
    let visible = app
        .get_webview_window("mini")
        .and_then(|mini| mini.is_visible().ok())
        .unwrap_or(false);
    if !visible {
        show(app);
    }
    if let Some(mini) = app.get_webview_window("mini") {
        let ask = serde_json::json!({ "moment": moment, "greeting": greeting });
        let _ = mini.eval(format!("window.__cbbMiniMood && window.__cbbMiniMood({ask})"));
        let _ = mini.set_focus();
    }
}

/// The panel's check-in: answer, skip ("Not now") or the chat offer.
#[tauri::command]
pub(crate) fn mini_mood<R: Runtime>(app: AppHandle<R>, request: serde_json::Value) -> Result<(), String> {
    let body = mood_body(&request).map_err(str::to_string)?;
    let window = app
        .get_webview_window("main")
        .filter(|main| main.url().is_ok_and(|url| url.host_str() == Some(crate::APP_HOST)))
        .ok_or("Open the tracker first.")?;
    window.eval(mood_script(&body)).map_err(|_| "That didn't save. Try again.".to_string())?;
    // Collect the outcome and hand it to the panel (up to ~10 s).
    std::thread::spawn(move || {
        for _ in 0..33 {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let (sender, receiver) = std::sync::mpsc::channel();
            let _ = window.eval_with_callback(TAKE_MOOD_RESULT_JS, move |raw| {
                let json: String = serde_json::from_str(&raw).unwrap_or_default();
                let _ = sender.send(json);
            });
            let Ok(json) = receiver.recv_timeout(std::time::Duration::from_secs(2)) else { continue };
            if json.is_empty() {
                continue;
            }
            if let (Some(mini), Ok(result)) = (app.get_webview_window("mini"), serde_json::from_str::<serde_json::Value>(&json)) {
                let _ = mini.eval(format!("window.__cbbMiniMoodResult && window.__cbbMiniMoodResult({result})"));
            }
            return;
        }
        if let Some(mini) = app.get_webview_window("mini") {
            let _ = mini.eval(
                "window.__cbbMiniMoodResult && window.__cbbMiniMoodResult({ok:false,error:'The tracker could not be reached.'})",
            );
        }
    });
    Ok(())
}

/// The check-in is over: a mini timer it grew goes back to its small size.
#[tauri::command]
pub(crate) fn mini_mood_done<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    if MOOD_SHRINK_AFTER.swap(false, std::sync::atomic::Ordering::Relaxed) && is_pinned() {
        set_compact(&app, true);
        push(&app);
    }
    Ok(())
}

fn is_allowed(action: &str) -> bool {
    matches!(action, "in" | "break" | "back" | "wrap")
}

/// A clock tap from the panel (`from`: "panel") or the mini timer
/// ("mini_timer").
#[tauri::command]
pub(crate) fn mini_action<R: Runtime>(
    app: AppHandle<R>,
    action: String,
    from: Option<String>,
) -> Result<(), String> {
    let Some(label) = crate::analytics::clock_label(&action).filter(|_| is_allowed(&action)) else {
        return Err("Unknown action.".into());
    };
    crate::clock::act(&app, &action);
    let from = if from.as_deref() == Some("mini_timer") { "mini_timer" } else { "panel" };
    crate::analytics::clock_tapped(&app, label, from);
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
    crate::analytics::action(&app, "open_tracker", "panel");
    Ok(())
}

#[tauri::command]
pub(crate) fn mini_expand_notifications<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    // Handed over before the page navigates away.
    crate::analytics::action(&app, "see_all_notifications", "panel");
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
    // Handed over before the page navigates away.
    crate::analytics::action(&app, "notification", "panel");
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
    if pinned {
        // A floating timer is for working in the tracker: bring it back.
        bring_tracker_back(&app);
    } else {
        // The drop-down is always the full panel.
        set_compact(&app, false);
        if let Some(mini) = app.get_webview_window("mini") {
            place_under_tray(&mini);
        }
    }
    push(&app);
    crate::analytics::panel_mode(&app, is_pinned(), is_compact());
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
    crate::analytics::panel_mode(&app, is_pinned(), is_compact());
    Ok(())
}

fn is_menu_item(item: &str) -> bool {
    matches!(
        item,
        "offline" | "new-window" | "update" | "diagnostics" | "autostart" | "close-keep" | "close-quit" | "quit"
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
            crate::analytics::action(&app, "saved_offline", "panel");
        }
        "new-window" => {
            hide(&app);
            crate::tabs::open_from_front(&app, false);
        }
        "update" => {
            hide(&app);
            crate::updater::check_now(app.clone());
            crate::analytics::action(&app, "check_updates", "panel");
        }
        "diagnostics" => {
            hide(&app);
            crate::diagnostics::show(&app);
            crate::analytics::action(&app, "diagnostics", "panel");
        }
        "autostart" => {
            let on = crate::toggle_autostart(&app);
            push(&app);
            crate::analytics::setting_changed(&app, "launch_at_login", on);
        }
        "close-keep" | "close-quit" => {
            let quits = item == "close-quit";
            crate::close::set_quits(&app, quits);
            push(&app);
            crate::analytics::setting_changed(&app, "close_quits", quits);
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
    fn tip_names_where_the_panel_lives() {
        assert!(tip_text("macos").1.contains("top of your screen"));
        assert!(tip_text("windows").1.contains("^ arrow"));
        assert!(tip_text("linux").1.contains("Ctrl+Alt+M"));
    }

    #[test]
    fn mood_answers_keep_only_what_the_tracker_accepts() {
        use serde_json::json;
        let body = mood_body(&json!({
            "intent": "answer", "moment": "IN", "score": 7, "keyword": "Focused",
            "cause": "WORK", "note": "  ", "extra": "dropped"
        }))
        .expect("valid");
        assert_eq!(
            body,
            json!({ "intent": "answer", "moment": "IN", "score": 7, "keyword": "Focused", "cause": "WORK", "note": null })
        );
        let own = mood_body(&json!({ "intent": "answer", "moment": "WRAP", "score": 2, "note": " rough day " })).expect("valid");
        assert_eq!(own["note"], "rough day");
        assert_eq!(own["keyword"], serde_json::Value::Null);
    }

    #[test]
    fn mood_answers_that_dont_fit_are_refused() {
        use serde_json::json;
        for bad in [
            json!({ "intent": "answer", "moment": "IN", "score": 0 }),
            json!({ "intent": "answer", "moment": "IN", "score": 11 }),
            json!({ "intent": "answer", "moment": "LUNCH", "score": 5 }),
            // A word from another band.
            json!({ "intent": "answer", "moment": "IN", "score": 9, "keyword": "Drained" }),
            json!({ "intent": "answer", "moment": "IN", "score": 5, "keyword": "Happy" }),
            json!({ "intent": "answer", "moment": "IN", "score": 5, "cause": "MONEY" }),
            json!({ "intent": "answer", "moment": "IN", "score": 5, "note": "x".repeat(281) }),
            json!({ "intent": "skip" }),
            json!({ "intent": "talk", "yes": "yes" }),
            json!({ "intent": "delete" }),
        ] {
            assert!(mood_body(&bad).is_err(), "{bad}");
        }
        assert_eq!(mood_body(&json!({ "intent": "skip", "moment": "WRAP" })).unwrap(), json!({ "intent": "skip", "moment": "WRAP" }));
        assert_eq!(mood_body(&json!({ "intent": "talk", "yes": true })).unwrap(), json!({ "intent": "talk", "yes": true }));
    }

    #[test]
    fn mood_script_posts_to_the_tracker_and_keeps_no_answer() {
        let body = mood_body(&serde_json::json!({ "intent": "skip", "moment": "IN" })).unwrap();
        let js = mood_script(&body);
        assert!(js.contains("fetch('/api/time-sheet/mood'"));
        assert!(js.contains("cbb:clock-changed"));
        // Only the outcome is left on the page, never the body.
        assert!(js.contains("offerTalk") && js.contains("told"));
        assert!(!js.contains("localStorage") && !js.contains("console."));
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
        for item in ["offline", "new-window", "update", "diagnostics", "autostart", "close-keep", "close-quit", "quit"] {
            assert!(is_menu_item(item), "{item}");
        }
        for item in ["", "show", "quit;", "eval"] {
            assert!(!is_menu_item(item), "{item}");
        }
    }
}
