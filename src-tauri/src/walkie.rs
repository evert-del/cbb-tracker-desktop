//! Walkie in the quick panel: unread conversations, a banner when someone
//! says something, and a quick chat to read and reply without opening the
//! tracker. Fed by the web app's own walkie routes (the header dock's):
//! `GET /api/walkie/channels`, `POST /api/walkie/open`,
//! `GET /api/walkie/{id}/messages`, `POST /api/walkie/{id}/send` and
//! `POST /api/walkie/{id}/read`.
//!
//! As in notify.rs, the shell holds no credentials: every call is made by the
//! tracker page with its own session, started with `eval` and collected
//! through a window variable. Membership and length rules stay the web app's.
//!
//! What people say is never logged, counted in analytics or written to disk
//! here. It passes through memory on its way to the panel and nothing more.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;

const POLL_EVERY: Duration = Duration::from_secs(15);

/// More fresh conversations than this collapse into one summary banner.
const MAX_BANNERS: usize = 3;

/// Unread conversations the panel lists.
const MAX_CHATS: usize = 3;

/// The longest message the tracker takes (its send route's limit).
const BODY_MAX: usize = 2000;

/// Starts the fetch; the result lands in `window.__cbbWalkie` ("null" on any
/// failure, e.g. signed out or not on the crew).
const START_JS: &str = r#"(function () {
  fetch('/api/walkie/channels', { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.ok ? r.text() : 'null'; })
    .then(function (t) { window.__cbbWalkie = t; })
    .catch(function () { window.__cbbWalkie = 'null'; });
})()"#;

/// Returns and clears the last result ("" when none is waiting).
const TAKE_JS: &str =
    "(function () { var v = window.__cbbWalkie || ''; window.__cbbWalkie = ''; return v; })()";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Ringing {
    #[serde(default)]
    from: String,
    #[serde(default)]
    at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Channel {
    key: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    sub: String,
    #[serde(default)]
    unread: u32,
    #[serde(default)]
    last_at: Option<String>,
    #[serde(default)]
    ringing: Option<Ringing>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Rail {
    channels: Vec<Channel>,
    #[serde(default)]
    unread: u32,
}

/// Parse the page's answer. `None` for "", "null" and anything malformed.
pub(crate) fn parse(raw: &str) -> Option<Rail> {
    serde_json::from_str(raw).ok()
}

/// One unread conversation, as the panel lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatRow {
    /// What opens it: "company", a project id, "dm:<id>" or "group:<id>".
    pub key: String,
    /// "COMPANY", "PROJECT", "DIRECT" or "GROUP".
    pub kind: String,
    pub name: String,
    /// The last thing said ("Michelle: on my way").
    pub preview: String,
    pub unread: u32,
    pub last_at: String,
    /// Who is ringing it, when a call alert is waiting.
    pub ringing: Option<String>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Banner {
    pub title: String,
    pub body: String,
}

/// What was last seen of each conversation, to tell what is new.
#[derive(Debug, Clone, Default, PartialEq)]
struct Seen {
    unread: u32,
    last_at: String,
    ringing_at: String,
}

#[derive(Default)]
pub(crate) struct State {
    seeded: bool,
    seen: HashMap<String, Seen>,
    pub unread: u32,
    pub chats: Vec<ChatRow>,
}

/// The walkie state the panel and the menu bar read.
#[derive(Default)]
pub(crate) struct Walkie(pub Mutex<State>);

/// Unread walkie messages, for the menu bar and the dock badge.
pub(crate) fn unread<R: Runtime>(app: &AppHandle<R>) -> u32 {
    app.try_state::<Walkie>()
        .and_then(|walkie| walkie.0.lock().ok().map(|state| state.unread))
        .unwrap_or(0)
}

/// The unread conversations the panel lists.
pub(crate) fn chats<R: Runtime>(app: &AppHandle<R>) -> Vec<ChatRow> {
    app.try_state::<Walkie>()
        .and_then(|walkie| walkie.0.lock().ok().map(|state| state.chats.clone()))
        .unwrap_or_default()
}

/// Decide which banners a poll warrants, remember what was seen and refresh
/// the panel's rows. The first poll after launch only seeds, so opening the
/// app never replays what was already waiting.
pub(crate) fn plan(state: &mut State, rail: &Rail) -> Vec<Banner> {
    let mut fresh: Vec<Banner> = Vec::new();
    let mut seen = HashMap::new();
    for channel in &rail.channels {
        let now = Seen {
            unread: channel.unread,
            last_at: channel.last_at.clone().unwrap_or_default(),
            ringing_at: channel.ringing.as_ref().map(|r| r.at.clone()).unwrap_or_default(),
        };
        let before = state.seen.get(&channel.key).cloned().unwrap_or_default();
        let rang = !now.ringing_at.is_empty() && now.ringing_at != before.ringing_at;
        let said = channel.unread > 0
            && (channel.unread > before.unread || (now.last_at != before.last_at && !now.last_at.is_empty()));
        if let Some(ringing) = channel.ringing.as_ref().filter(|_| rang) {
            let from = if ringing.from.is_empty() { "Someone" } else { ringing.from.as_str() };
            fresh.push(Banner { title: format!("{from} is calling"), body: format!("Call alert on {}", channel.name) });
        } else if said {
            fresh.push(Banner { title: channel.name.clone(), body: channel.sub.clone() });
        }
        seen.insert(channel.key.clone(), now);
    }
    state.seen = seen;

    let mut waiting: Vec<&Channel> =
        rail.channels.iter().filter(|c| c.unread > 0 || c.ringing.is_some()).collect();
    waiting.sort_by(|a, b| b.last_at.cmp(&a.last_at));
    state.chats = waiting
        .into_iter()
        .take(MAX_CHATS)
        .map(|c| ChatRow {
            key: c.key.clone(),
            kind: c.kind.clone(),
            name: c.name.clone(),
            preview: c.sub.clone(),
            unread: c.unread,
            last_at: c.last_at.clone().unwrap_or_default(),
            ringing: c.ringing.as_ref().map(|r| r.from.clone()),
        })
        .collect();
    state.unread = rail.unread;

    if !state.seeded {
        state.seeded = true;
        return Vec::new();
    }
    if fresh.len() > MAX_BANNERS {
        return vec![Banner {
            title: "Walkie".into(),
            body: format!("New messages in {} conversations", fresh.len()),
        }];
    }
    fresh
}

/// Forget everything (signed out): the next sign-in seeds afresh.
pub(crate) fn clear<R: Runtime>(app: &AppHandle<R>) {
    if let Some(walkie) = app.try_state::<Walkie>() {
        if let Ok(mut state) = walkie.0.lock() {
            *state = State::default();
        }
    }
}

fn apply<R: Runtime>(app: &AppHandle<R>, rail: &Rail) {
    let Some(walkie) = app.try_state::<Walkie>() else { return };
    let banners = match walkie.0.lock() {
        Ok(mut state) => plan(&mut state, rail),
        Err(_) => return,
    };
    for banner in banners {
        let _ = app.notification().builder().title(banner.title).body(banner.body).show();
    }
    crate::notify::set_badge(app);
    crate::mini::push(app);
}

/// The tracker window, when it is on the tracker (not mid sign-in).
fn tracker_window<R: Runtime>(app: &AppHandle<R>) -> Option<tauri::WebviewWindow<R>> {
    app.get_webview_window("main")
        .filter(|main| main.url().is_ok_and(|url| url.host_str() == Some(crate::APP_HOST)))
}

/// Evaluate `take` in `window` every 300 ms until it returns something, for
/// up to `tries` attempts. Blocking: call it from a worker thread.
fn collect<R: Runtime>(window: &tauri::WebviewWindow<R>, take: &'static str, tries: u32) -> Option<String> {
    for _ in 0..tries {
        std::thread::sleep(Duration::from_millis(300));
        let (sender, receiver) = std::sync::mpsc::channel();
        let _ = window.eval_with_callback(take, move |raw| {
            let json: String = serde_json::from_str(&raw).unwrap_or_default();
            let _ = sender.send(json);
        });
        let Ok(json) = receiver.recv_timeout(Duration::from_secs(2)) else { continue };
        if !json.is_empty() {
            return Some(json);
        }
    }
    None
}

/// Poll now (after a reply or an open), so the panel's count clears at once.
pub(crate) fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = tracker_window(app) else { return };
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = window.eval(START_JS);
        if let Some(rail) = collect(&window, TAKE_JS, 16).as_deref().and_then(parse) {
            apply(&app, &rail);
        }
    });
}

/// walkie_bridge.js saw the tracker's own walkie list arrive in `label`'s
/// window (its dock reloads it the moment something is said): take it now,
/// rather than on the next poll.
pub(crate) fn take_from<R: Runtime>(app: &AppHandle<R>, label: &str) {
    let app = app.clone();
    let label = label.to_string();
    // Off the navigation handler, so the page is never asked from inside it.
    std::thread::spawn(move || {
        if !crate::session::signed_in() {
            return;
        }
        let Some(window) = app
            .get_webview_window(&label)
            .filter(|window| window.url().is_ok_and(|url| url.host_str() == Some(crate::APP_HOST)))
        else {
            return;
        };
        let app_for_cb = app.clone();
        let _ = window.eval_with_callback(TAKE_JS, move |raw| {
            let raw: String = serde_json::from_str(&raw).unwrap_or_default();
            if let Some(rail) = parse(&raw) {
                apply(&app_for_cb, &rail);
            }
        });
    });
}

/// Start the background poller. Does nothing while the main window is off the
/// tracker (e.g. mid sign-in).
pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(POLL_EVERY);
        if !crate::session::signed_in() {
            clear(&app);
            continue;
        }
        let Some(window) = tracker_window(&app) else { continue };
        let app_for_cb = app.clone();
        // Collect last tick's answer, then ask for a fresh one.
        let _ = window.eval_with_callback(TAKE_JS, move |raw| {
            let raw: String = serde_json::from_str(&raw).unwrap_or_default();
            if let Some(rail) = parse(&raw) {
                apply(&app_for_cb, &rail);
            }
        });
        let _ = window.eval(START_JS);
    });
}

// ── the quick chat ──

/// A channel key the tracker's open route takes: "company", a project id,
/// "dm:<user id>" or "group:<channel id>".
pub(crate) fn valid_key(key: &str) -> bool {
    key == "company"
        || is_uuid(key)
        || key.strip_prefix("dm:").is_some_and(is_uuid)
        || key.strip_prefix("group:").is_some_and(is_uuid)
}

pub(crate) fn is_uuid(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(i, ch)| match i {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        })
}

/// The reply as the tracker will take it, or why not.
pub(crate) fn reply_body(body: &str) -> Result<String, &'static str> {
    let body = body.trim();
    if body.is_empty() {
        return Err("Say something first.");
    }
    if body.chars().count() > BODY_MAX {
        return Err("Keep it to 2000 characters.");
    }
    Ok(body.to_string())
}

/// The quick chat's page script. `ARGS` is replaced with
/// `{ op, seq, key | channelId, body }` as JSON. It opens the conversation
/// (open → messages) or sends a reply (send → messages), moves the read mark
/// so the unread count clears, and leaves only what the panel draws on
/// `window.__cbbWalkieResult`.
const CHAT_JS: &str = r#"(function (a) {
  window.__cbbWalkieResult = '';
  function done(x) { x.op = a.op; x.seq = a.seq; window.__cbbWalkieResult = JSON.stringify(x); }
  function json(r) { return r.json().catch(function () { return {}; }).then(function (b) { return { ok: r.ok, b: b || {} }; }); }
  function post(url, body) {
    return fetch(url, { method: 'POST', credentials: 'same-origin', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body || {}) }).then(json);
  }
  function lines(id, name) {
    var base = '/api/walkie/' + encodeURIComponent(id);
    return fetch(base + '/messages', { credentials: 'same-origin', cache: 'no-store' }).then(json).then(function (m) {
      if (!m.ok) return done({ ok: false, error: m.b.error || "Couldn't read that chat." });
      return post(base + '/read').catch(function () {}).then(function () {
        done({ ok: true, channelId: id, name: name, lines: (m.b.messages || []).slice(-20).map(function (l) {
          return { id: String(l.id), body: l.body || '', author: l.author || '', mine: !!l.mine, createdAt: l.createdAt || '', call: l.kind === 'call', file: !!l.file };
        }) });
      });
    });
  }
  var go = a.op === 'send'
    ? post('/api/walkie/' + encodeURIComponent(a.channelId) + '/send', { body: a.body }).then(function (s) {
        if (!s.ok) return done({ ok: false, error: s.b.error || "That didn't send. Try again." });
        return lines(a.channelId, '');
      })
    : post('/api/walkie/open', { key: a.key }).then(function (o) {
        if (!o.ok || !o.b.channelId) return done({ ok: false, error: o.b.error || "Couldn't open that chat." });
        return lines(o.b.channelId, o.b.name || '');
      });
  go.catch(function () { done({ ok: false, error: 'You seem to be offline. Try again in a moment.' }); });
})(ARGS)"#;

pub(crate) fn chat_script(args: &serde_json::Value) -> String {
    CHAT_JS.replace("ARGS", &args.to_string())
}

/// Returns and clears the last quick-chat result ("" while waiting).
const TAKE_RESULT_JS: &str =
    "(function () { var v = window.__cbbWalkieResult || ''; window.__cbbWalkieResult = ''; return v; })()";

/// Run a quick-chat request in the tracker page and hand its outcome to the
/// panel (`window.__cbbMiniWalkieResult`), then refresh the unread count.
fn run<R: Runtime>(app: AppHandle<R>, args: serde_json::Value) -> Result<(), String> {
    let window = tracker_window(&app).ok_or("Open the tracker first.")?;
    let seq = args.get("seq").cloned().unwrap_or_default();
    let op = args.get("op").cloned().unwrap_or_default();
    window.eval(chat_script(&args)).map_err(|_| "The tracker could not be reached.".to_string())?;
    std::thread::spawn(move || {
        let result = collect(&window, TAKE_RESULT_JS, 33)
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            .unwrap_or_else(|| {
                serde_json::json!({ "ok": false, "op": op, "seq": seq, "error": "The tracker could not be reached." })
            });
        if let Some(mini) = app.get_webview_window("mini") {
            let _ = mini.eval(format!("window.__cbbMiniWalkieResult && window.__cbbMiniWalkieResult({result})"));
        }
        refresh(&app);
    });
    Ok(())
}

/// Open a conversation in the panel: its last lines, marked read.
#[tauri::command]
pub(crate) fn mini_walkie_open<R: Runtime>(app: AppHandle<R>, key: String, seq: u32) -> Result<(), String> {
    if !valid_key(&key) {
        return Err("Unknown chat.".into());
    }
    crate::analytics::action(&app, "walkie_open", "panel");
    run(app, serde_json::json!({ "op": "open", "seq": seq, "key": key }))
}

/// Reply in the open conversation.
#[tauri::command]
pub(crate) fn mini_walkie_send<R: Runtime>(
    app: AppHandle<R>,
    channel_id: String,
    body: String,
    seq: u32,
) -> Result<(), String> {
    if !is_uuid(&channel_id) {
        return Err("Unknown chat.".into());
    }
    let body = reply_body(&body).map_err(str::to_string)?;
    crate::analytics::action(&app, "walkie_reply", "panel");
    run(app, serde_json::json!({ "op": "send", "seq": seq, "channelId": channel_id, "body": body }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0b5e0a52-3c4d-4e8f-9a1b-2c3d4e5f6a7b";

    fn rail(json: &str) -> Rail {
        parse(json).expect("test payload parses")
    }

    fn channel(key: &str, unread: u32, last_at: &str) -> String {
        format!(
            r#"{{"key":"{key}","kind":"DIRECT","name":"Name {key}","sub":"said {key}","unread":{unread},"lastAt":"{last_at}","ringing":null}}"#
        )
    }

    fn payload(channels: &[String], unread: u32) -> String {
        format!(r#"{{"channels":[{}],"unread":{unread}}}"#, channels.join(","))
    }

    #[test]
    fn unusable_answers_are_ignored() {
        for raw in ["", "null", "{\"error\":\"Walkie is for the crew.\"}", "<html>"] {
            assert!(parse(raw).is_none(), "{raw}");
        }
    }

    #[test]
    fn first_poll_seeds_without_banners_but_lists_chats() {
        let mut state = State::default();
        let r = rail(&payload(&[channel("a", 2, "2026-10-09T08:00:00.000Z")], 2));
        assert!(plan(&mut state, &r).is_empty());
        assert_eq!(state.unread, 2);
        assert_eq!(state.chats.len(), 1);
        assert_eq!(state.chats[0].preview, "said a");
        assert!(plan(&mut state, &r).is_empty());
    }

    #[test]
    fn a_new_message_gets_one_banner() {
        let mut state = State::default();
        plan(&mut state, &rail(&payload(&[channel("a", 0, "2026-10-09T08:00:00.000Z")], 0)));
        let banners = plan(&mut state, &rail(&payload(&[channel("a", 1, "2026-10-09T08:05:00.000Z")], 1)));
        assert_eq!(banners, vec![Banner { title: "Name a".into(), body: "said a".into() }]);
    }

    #[test]
    fn reading_elsewhere_raises_nothing() {
        let mut state = State::default();
        plan(&mut state, &rail(&payload(&[channel("a", 2, "2026-10-09T08:00:00.000Z")], 2)));
        let banners = plan(&mut state, &rail(&payload(&[channel("a", 0, "2026-10-09T08:00:00.000Z")], 0)));
        assert!(banners.is_empty());
        assert!(state.chats.is_empty());
        assert_eq!(state.unread, 0);
    }

    #[test]
    fn a_call_alert_says_who_is_calling() {
        let mut state = State::default();
        plan(&mut state, &rail(&payload(&[], 0)));
        let ringing = r#"{"key":"company","kind":"COMPANY","name":"Channel 1","sub":"Call alert","unread":1,"lastAt":"2026-10-09T08:00:00.000Z","ringing":{"from":"Michelle","at":"2026-10-09T08:00:00.000Z"}}"#;
        let banners = plan(&mut state, &rail(&payload(&[ringing.to_string()], 1)));
        assert_eq!(banners, vec![Banner { title: "Michelle is calling".into(), body: "Call alert on Channel 1".into() }]);
        assert_eq!(state.chats[0].ringing.as_deref(), Some("Michelle"));
    }

    #[test]
    fn bursts_collapse_into_a_summary() {
        let mut state = State::default();
        plan(&mut state, &rail(&payload(&[], 0)));
        let channels: Vec<String> = ["a", "b", "c", "d"].iter().map(|k| channel(k, 1, "2026-10-09T08:00:00.000Z")).collect();
        let banners = plan(&mut state, &rail(&payload(&channels, 4)));
        assert_eq!(banners.len(), 1);
        assert_eq!(banners[0].body, "New messages in 4 conversations");
    }

    #[test]
    fn chats_are_the_newest_three_waiting() {
        let mut state = State::default();
        let channels = vec![
            channel("old", 1, "2026-10-09T07:00:00.000Z"),
            channel("read", 0, "2026-10-09T09:30:00.000Z"),
            channel("new", 1, "2026-10-09T09:00:00.000Z"),
            channel("mid", 3, "2026-10-09T08:00:00.000Z"),
            channel("older", 1, "2026-10-09T06:00:00.000Z"),
        ];
        plan(&mut state, &rail(&payload(&channels, 6)));
        let keys: Vec<&str> = state.chats.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["new", "mid", "old"]);
    }

    #[test]
    fn only_tracker_channel_keys_pass() {
        for key in ["company".to_string(), ID.to_string(), format!("dm:{ID}"), format!("group:{ID}")] {
            assert!(valid_key(&key), "{key}");
        }
        for key in ["", "Company", "dm:", "dm:x", "group:../x", "0b5e0a52-3c4d-4e8f-9a1b-2c3d4e5f6a7", "'); alert(1); ('"] {
            assert!(!valid_key(key), "{key}");
        }
    }

    #[test]
    fn replies_are_trimmed_and_bounded() {
        assert_eq!(reply_body("  on my way \n").unwrap(), "on my way");
        assert!(reply_body("   ").is_err());
        assert!(reply_body(&"x".repeat(2000)).is_ok());
        assert!(reply_body(&"x".repeat(2001)).is_err());
    }

    #[test]
    fn the_bridge_only_watches_the_walkie_list_and_sends_nothing() {
        let js = include_str!("walkie_bridge.js");
        assert!(js.contains("url.pathname === '/api/walkie/channels'"));
        assert!(js.contains("url.origin === location.origin"));
        assert!(js.contains("r.clone()"), "the page still gets its own answer");
        assert!(js.contains("location.href = 'cbb-walkie://rail'"));
        assert_eq!(js.matches("pageFetch.apply").count(), 1, "no fetch of its own");
        assert!(!js.contains("localStorage") && !js.contains("console."));
    }

    #[test]
    fn the_chat_script_only_talks_to_the_walkie_and_keeps_nothing() {
        let js = chat_script(&serde_json::json!({ "op": "send", "seq": 1, "channelId": ID, "body": "it's \"fine\"</script>" }));
        assert!(js.contains("'/api/walkie/open'") && js.contains("/send'") && js.contains("/read'"));
        assert!(js.contains(r#""body":"it's \"fine\"</script>""#), "args are JSON");
        assert!(!js.contains("ARGS"));
        assert!(!js.contains("localStorage") && !js.contains("console."));
        let fetches = js.matches("fetch(").count();
        assert_eq!(fetches, 2, "one GET (messages) and one POST helper");
    }
}
