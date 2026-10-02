//! Explicit opt-in offline storage ("Save for offline").
//!
//! Online-only is the default: this module only keeps what the user
//! explicitly saves — call-sheet/quote/invoice PDFs downloaded through the
//! main WebView's own cookie jar, so authenticated routes work with no token
//! handling in Rust. Saves are read-only snapshots labelled with their
//! saved-at time; on reconnect the user re-saves to refresh. No sync, no
//! outbox, no database involvement.
//!
//! Flow: the Library window calls `offline_save_pdf`, which validates the URL
//! against the same allow-list as navigation, arms a pending capture and
//! navigates the main window at the PDF URL. The `on_download` hook (wired
//! in `lib.rs`) redirects that one download into the app-data offline dir;
//! completion or failure is reported back to the Library window as an event.

use std::{
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

use crate::webview_may_load;

/// Largest single PDF kept (call sheets are kilobytes; reels are excluded
/// from offline by the URL rule below, not by size alone).
pub(crate) const MAX_SINGLE_FILE_BYTES: u64 = 25 * 1024 * 1024;
/// Total offline budget across all saved PDFs.
pub(crate) const MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;

const PDF_DIR: &str = "offline-pdfs";
const INDEX_FILE: &str = "offline-index.json";

/// One saved snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OfflineItem {
    pub id: String,
    pub label: String,
    pub source_url: String,
    pub saved_at_millis: u64,
    pub bytes: u64,
}

/// Armed while a requested download is in flight.
#[derive(Debug, Clone)]
pub(crate) struct PendingCapture {
    pub id: String,
    pub label: String,
    pub source_url: String,
    pub dest_path: PathBuf,
}

pub(crate) type PendingState = Mutex<Option<PendingCapture>>;

/// A URL is savable when it loads in-app (allow-listed host) and addresses
/// a PDF download route: last path segment `pdf`. The free template
/// generator (`POST /api/templates/call-sheet/pdf`) is excluded by shape —
/// a GET there fails cleanly with an error, never a silent wrong file.
pub(crate) fn is_savable_pdf_url(url: &url::Url) -> bool {
    webview_may_load(url)
        && url
            .path_segments()
            .is_some_and(|mut segments| segments.next_back() == Some("pdf"))
}

/// Filename-safe slug: lowercase alphanumerics and dashes, capped length.
pub(crate) fn sanitize_label(label: &str) -> String {
    let mut slug: String = label
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-').to_string();
    let shorted: String = slug.chars().take(60).collect();
    if shorted.is_empty() {
        "offline-pdf".to_string()
    } else {
        shorted
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn pdf_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir unavailable: {e}"))?
        .join(PDF_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create offline dir: {e}"))?;
    Ok(dir)
}

fn index_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir unavailable: {e}"))?
        .join(INDEX_FILE))
}

fn read_index<R: Runtime>(app: &AppHandle<R>) -> Vec<OfflineItem> {
    let Ok(path) = index_path(app) else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn write_index<R: Runtime>(app: &AppHandle<R>, items: &[OfflineItem]) -> Result<(), String> {
    let path = index_path(app)?;
    let bytes = serde_json::to_vec_pretty(items).map_err(|e| format!("index encode: {e}"))?;
    std::fs::write(path, bytes).map_err(|e| format!("index write: {e}"))
}

fn used_bytes(items: &[OfflineItem]) -> u64 {
    items.iter().map(|i| i.bytes).sum()
}

/// Resolve an item id to its file, refusing anything that escapes the
/// offline dir (ids are ours, but open/delete take them from the UI).
fn item_path(dir: &std::path::Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.contains("..") || id.len() > 128
    {
        return Err("invalid id".to_string());
    }
    Ok(dir.join(format!("{id}.pdf")))
}

/// True when another `incoming` bytes fit inside the total budget.
pub(crate) fn fits_within_cap(used: u64, incoming: u64) -> bool {
    used.saturating_add(incoming) <= MAX_TOTAL_BYTES
}

fn emit_to_library<R: Runtime>(app: &AppHandle<R>, event: &str, payload: serde_json::Value) {
    if let Some(library) = app.get_webview_window("library") {
        let _ = library.emit(event, payload);
    }
}

/// Save a PDF for offline use. Arms the download capture and navigates the
/// main window at the URL; completion arrives as `offline-saved` /
/// `offline-failed` events on the Library window (downloads are async).
#[tauri::command]
pub(crate) async fn offline_save_pdf<R: Runtime>(
    app: AppHandle<R>,
    pending: State<'_, PendingState>,
    url: String,
    label: String,
) -> Result<OfflineItem, String> {
    let parsed = url.parse::<url::Url>().map_err(|_| "not a valid URL")?;
    if !is_savable_pdf_url(&parsed) {
        return Err(
            "only PDF download links from the tracker can be saved (e.g. a call-sheet Download link)"
                .to_string(),
        );
    }
    if pending.lock().map_err(|e| e.to_string())?.is_some() {
        return Err("a save is already in progress".to_string());
    }

    let items = read_index(&app);
    // Reserve headroom for the largest file we accept.
    if !fits_within_cap(used_bytes(&items), MAX_SINGLE_FILE_BYTES) {
        return Err("offline storage is full (500 MB) — delete a saved PDF first".to_string());
    }

    let millis = now_millis();
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("give the saved copy a label first".to_string());
    }
    let id = format!("{millis}-{}", sanitize_label(&label));
    let dest_path = pdf_dir(&app)?.join(format!("{id}.pdf"));
    let item = OfflineItem {
        id: id.clone(),
        label: label.clone(),
        source_url: parsed.to_string(),
        saved_at_millis: millis,
        bytes: 0,
    };

    *pending.lock().map_err(|e| e.to_string())? = Some(PendingCapture {
        id,
        label,
        source_url: parsed.to_string(),
        dest_path,
    });

    let main = app
        .get_webview_window("main")
        .ok_or("main window unavailable")?;
    main.navigate(parsed)
        .map_err(|e| format!("navigation failed: {e}"))?;
    Ok(item)
}

/// Called from the `on_download` hook. Returns true to let the download
/// proceed (redirected when it is our armed capture, untouched otherwise).
pub(crate) fn handle_download<R: Runtime>(
    app: &AppHandle<R>,
    url: &url::Url,
    destination: &mut PathBuf,
) -> bool {
    let pending_guard = match app.state::<PendingState>().inner().lock() {
        Ok(guard) => guard,
        Err(_) => return true,
    };
    if let Some(capture) = pending_guard.as_ref() {
        if capture.source_url == url.as_str() {
            *destination = capture.dest_path.clone();
        }
    }
    true
}

/// Called from the `on_download` Finished event. Finalizes a captured save.
pub(crate) fn finish_download<R: Runtime>(app: &AppHandle<R>, url: &url::Url, success: bool) {
    let capture = match app.state::<PendingState>().inner().lock() {
        Ok(mut guard) => guard.take(),
        Err(_) => return,
    };
    let Some(capture) = capture else { return };
    if capture.source_url != url.as_str() {
        // Not ours (a second download slipped in); put it back.
        if let Ok(mut guard) = app.state::<PendingState>().inner().lock() {
            *guard = Some(capture);
        }
        return;
    }

    if !success {
        let _ = std::fs::remove_file(&capture.dest_path);
        emit_to_library(
            app,
            "offline-failed",
            serde_json::json!({ "label": capture.label, "reason": "download failed" }),
        );
        return;
    }
    let bytes = std::fs::metadata(&capture.dest_path).map(|m| m.len());
    match bytes {
        Ok(size) if size > 0 && size <= MAX_SINGLE_FILE_BYTES => {
            let mut items = read_index(app);
            let others_bytes: u64 = items
                .iter()
                .filter(|i| i.id != capture.id)
                .map(|i| i.bytes)
                .sum();
            if others_bytes.saturating_add(size) > MAX_TOTAL_BYTES {
                let _ = std::fs::remove_file(&capture.dest_path);
                emit_to_library(
                    app,
                    "offline-failed",
                    serde_json::json!({ "label": capture.label, "reason": "offline storage is full (500 MB)" }),
                );
                return;
            }
            items.retain(|i| i.id != capture.id);
            let item = OfflineItem {
                id: capture.id.clone(),
                label: capture.label.clone(),
                source_url: capture.source_url.clone(),
                saved_at_millis: now_millis(),
                bytes: size,
            };
            items.push(item.clone());
            match write_index(app, &items) {
                Ok(()) => {
                    emit_to_library(app, "offline-saved", serde_json::json!({ "item": item }))
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&capture.dest_path);
                    emit_to_library(
                        app,
                        "offline-failed",
                        serde_json::json!({ "label": capture.label, "reason": e }),
                    );
                }
            }
        }
        _ => {
            let _ = std::fs::remove_file(&capture.dest_path);
            emit_to_library(
                app,
                "offline-failed",
                serde_json::json!({ "label": capture.label, "reason": "empty or oversized file (25 MB max)" }),
            );
        }
    }
}

#[tauri::command]
pub(crate) fn offline_list<R: Runtime>(app: AppHandle<R>) -> Result<Vec<OfflineItem>, String> {
    let mut items = read_index(&app);
    items.sort_by(|a, b| b.saved_at_millis.cmp(&a.saved_at_millis));
    Ok(items)
}

#[tauri::command]
pub(crate) fn offline_open<R: Runtime>(app: AppHandle<R>, id: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = pdf_dir(&app)?;
    let path = item_path(&dir, &id)?;
    if !path.is_file() {
        return Err("saved file not found — it may have been deleted".to_string());
    }
    app.opener()
        .open_path(path.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| format!("cannot open PDF: {e}"))
}

#[tauri::command]
pub(crate) fn offline_delete<R: Runtime>(app: AppHandle<R>, id: String) -> Result<(), String> {
    let dir = pdf_dir(&app)?;
    let path = item_path(&dir, &id)?;
    let _ = std::fs::remove_file(&path);
    let mut items = read_index(&app);
    items.retain(|i| i.id != id);
    write_index(&app, &items)
}

#[tauri::command]
pub(crate) fn offline_storage_info<R: Runtime>(
    app: AppHandle<R>,
) -> Result<serde_json::Value, String> {
    let items = read_index(&app);
    Ok(serde_json::json!({
        "count": items.len(),
        "bytes": used_bytes(&items),
        "capBytes": MAX_TOTAL_BYTES,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(raw: &str) -> url::Url {
        url::Url::parse(raw).expect("test URL parses")
    }

    #[test]
    fn savable_pdf_urls_are_allow_listed_pdf_routes() {
        assert!(is_savable_pdf_url(&parsed(
            "https://tracker.coolerboxbrothers.com/api/call-sheets/abc/pdf"
        )));
        assert!(is_savable_pdf_url(&parsed(
            "https://tracker.coolerboxbrothers.com/api/quotes/abc/pdf"
        )));
    }

    #[test]
    fn non_pdf_and_foreign_urls_are_not_savable() {
        for raw in [
            "https://tracker.coolerboxbrothers.com/projects",
            "https://tracker.coolerboxbrothers.com/api/call-sheets/abc",
            "https://coolerboxbrothers.com/api/call-sheets/abc/pdf",
            "http://tracker.coolerboxbrothers.com/api/call-sheets/abc/pdf",
        ] {
            assert!(!is_savable_pdf_url(&parsed(raw)), "{raw}");
        }
    }

    #[test]
    fn labels_become_safe_slugs() {
        assert_eq!(
            sanitize_label("Con Spirito S1 - Day 1"),
            "con-spirito-s1-day-1"
        );
        assert_eq!(sanitize_label("  "), "offline-pdf");
        assert_eq!(sanitize_label("Motus/Klutch: 23.09"), "motus-klutch-23-09");
    }

    #[test]
    fn storage_cap_math() {
        assert!(fits_within_cap(0, MAX_TOTAL_BYTES));
        assert!(!fits_within_cap(1, MAX_TOTAL_BYTES));
        assert!(!fits_within_cap(u64::MAX, 1));
    }

    #[test]
    fn item_paths_cannot_escape() {
        let dir = std::path::Path::new("/data/offline-pdfs");
        assert!(item_path(dir, "123-abc").is_ok());
        for bad in ["", "../x", "a/b", "a\\b", &"x".repeat(129)] {
            assert!(item_path(dir, bad).is_err(), "{bad}");
        }
    }
}
