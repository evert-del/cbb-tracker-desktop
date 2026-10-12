//! Quick search in the panel: the tracker's own search-as-you-type
//! (`GET /api/search/suggest?q=`, the top bar's), so who may see what is
//! decided by the tracker. Projects, deliverables and ideas in limbo; the
//! tracker doesn't search people or files there.
//!
//! As elsewhere, the tracker page asks with its own session (`eval`) and the
//! answer is collected through a window variable. Only titles, subtitles and
//! tracker paths reach the panel.

use tauri::{AppHandle, Manager, Runtime};

/// The tracker answers from two characters; long queries are cut here.
const MIN_CHARS: usize = 2;
const MAX_CHARS: usize = 80;

/// The query as it is sent, or `None` when there is nothing to search yet.
pub(crate) fn query(q: &str) -> Option<String> {
    let q: String = q.trim().chars().filter(|ch| !ch.is_control()).take(MAX_CHARS).collect();
    (q.chars().count() >= MIN_CHARS).then_some(q)
}

/// The page script: asks the tracker and leaves `{ seq, ok, groups | error }`
/// on `window.__cbbSearch`. `ARGS` is `{ q, seq }` as JSON.
const SEARCH_JS: &str = r#"(function (a) {
  function only(list) {
    return (Array.isArray(list) ? list : []).filter(function (s) {
      return s && typeof s.href === 'string' && s.href.charAt(0) === '/' && s.href.charAt(1) !== '/';
    }).slice(0, 6).map(function (s) {
      return { title: String(s.title || '').slice(0, 120), subtitle: String(s.subtitle || '').slice(0, 160), href: s.href };
    });
  }
  window.__cbbSearch = '';
  fetch('/api/search/suggest?q=' + encodeURIComponent(a.q), { credentials: 'same-origin', cache: 'no-store' })
    .then(function (r) { return r.json().catch(function () { return {}; }).then(function (b) { return { ok: r.ok, b: b || {} }; }); })
    .then(function (x) {
      window.__cbbSearch = JSON.stringify(x.ok
        ? { seq: a.seq, ok: true, groups: [
            { label: 'Projects', items: only(x.b.projects) },
            { label: 'Deliverables', items: only(x.b.deliverables) },
            { label: 'Ideas', items: only(x.b.limbo) }] }
        : { seq: a.seq, ok: false, error: x.b.error || 'Could not search just now.' });
    })
    .catch(function () { window.__cbbSearch = JSON.stringify({ seq: a.seq, ok: false, error: 'You seem to be offline.' }); });
})(ARGS)"#;

const TAKE_JS: &str =
    "(function () { var v = window.__cbbSearch || ''; window.__cbbSearch = ''; return v; })()";

pub(crate) fn script(q: &str, seq: u32) -> String {
    SEARCH_JS.replace("ARGS", &serde_json::json!({ "q": q, "seq": seq }).to_string())
}

/// Search from the panel; the answer goes back to it through
/// `window.__cbbMiniSearchResult`.
#[tauri::command]
pub(crate) fn mini_search<R: Runtime>(app: AppHandle<R>, q: String, seq: u32) -> Result<(), String> {
    let q = query(&q).ok_or("Type at least two letters.")?;
    let window = crate::walkie::tracker_window(&app).ok_or("Open the tracker first.")?;
    window.eval(script(&q, seq)).map_err(|_| "The tracker could not be reached.".to_string())?;
    std::thread::spawn(move || {
        let result = crate::walkie::collect(&window, TAKE_JS, 20)
            .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
            .unwrap_or_else(|| serde_json::json!({ "seq": seq, "ok": false, "error": "The tracker could not be reached." }));
        if let Some(mini) = app.get_webview_window("mini") {
            let _ = mini.eval(format!("window.__cbbMiniSearchResult && window.__cbbMiniSearchResult({result})"));
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_need_two_letters_and_are_trimmed_and_cut() {
        assert_eq!(query(" a "), None);
        assert_eq!(query(""), None);
        assert_eq!(query("  tvc  ").as_deref(), Some("tvc"));
        assert_eq!(query("tv\u{0}c\n").as_deref(), Some("tvc"));
        assert_eq!(query(&"x".repeat(200)).map(|q| q.len()), Some(80));
    }

    #[test]
    fn the_script_asks_the_trackers_own_search_with_a_json_query() {
        let js = script("O'Neil </script>", 7);
        assert!(js.contains("fetch('/api/search/suggest?q=' + encodeURIComponent(a.q)"));
        assert!(js.contains(r#"{"q":"O'Neil </script>","seq":7}"#));
        assert!(!js.contains("ARGS"));
        // Only tracker paths, never another site (or a protocol-relative one).
        assert!(js.contains("s.href.charAt(0) === '/' && s.href.charAt(1) !== '/'"));
        assert!(!js.contains("localStorage") && !js.contains("console."));
    }
}
