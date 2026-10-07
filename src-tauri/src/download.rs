//! Opening and saving first-party files (attachments, scripts, call sheets,
//! Cooler Box items).
//!
//! These links are `target="_blank"` anchors to `/api/media/…`,
//! `/api/attachments/…` and the like. Left alone, the WebView loads the file
//! as a bare page (images, PDFs) with no way back and no way to save it.
//! Instead the shell fetches the file itself with the window's own session
//! cookies — nothing is stored in Rust — and tells the user at every step
//! with an in-app message (`window.__cbbToast`, nav_bar.js).
//!
//! An "Open …" link opens: the file is saved into Downloads (no dialog) and
//! handed to the user's own app for it (Word, the PDF reader), as a browser
//! does. Until 0.2.12 every click asked where to save instead and opened
//! nothing; on Windows that Save dialog could sit behind the window, so the
//! click seemed to do nothing at all (found live, 2026-10-06). Only a link
//! the page marks `download` asks where to save, with the dialog parented to
//! the window. A document that is really a link (Google Drive, Dropbox: the
//! tracker redirects to it) answers with a web page, and opens in the
//! browser instead of being saved as a "file".
//!
//! Entry points: a click handler in `nav_bar.js` (hands the URL over through
//! a `cbb-download://` navigation, which `on_navigation` intercepts) and
//! `on_new_window` for `window.open`. "Show in folder" comes back the same
//! way as `cbb-reveal://`, limited to files saved here.

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE, COOKIE};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::{offline, system_open, APP_HOST};

/// Files saved here, so "Show in folder" can only ever reveal those.
static SAVED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
const MAX_SAVED: usize = 50;

/// How often the progress message is refreshed.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// First path segments of the app's file routes: every `/api/` route that
/// serves bytes (attachments, call-sheet PDFs, brand docs, gear files,
/// media, casting self-tapes). Page and action routes (`/projects/…`,
/// `/api/walkie/…/send`, …) are deliberately absent: those keep loading in
/// the window.
const FILE_PREFIXES: &[&str] = &[
    "/api/media/",
    "/api/coolerbox/",
    "/api/attachments/",
    "/api/call-sheets/",
    "/api/clients/",
    "/api/gear/",
    "/api/gazebo/",
    "/api/limbo/item/",
    "/api/cb/asset/",
    "/api/agency/media/",
    "/api/casting/media/",
    "/api/casting/selftape/",
];

/// True for a first-party file URL the shell should save rather than show.
pub(crate) fn is_file_url(url: &url::Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(APP_HOST)
        && FILE_PREFIXES.iter().any(|prefix| url.path().starts_with(prefix))
}

/// Show a message in the main window (see `__cbbToast` in nav_bar.js).
pub(crate) fn toast<R: Runtime>(app: &AppHandle<R>, message: serde_json::Value) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval(format!(
            "window.__cbbToast&&window.__cbbToast({message})"
        ));
    }
}

/// What a fetch ended in.
enum Fetched {
    /// Written to this path.
    Saved(PathBuf),
    /// The user closed the Save dialog.
    Cancelled,
    /// Not a file but a web page (a document that is a Drive/Dropbox link):
    /// open this, the address it ended at, in the browser.
    Page(url::Url),
}

/// True when a response is a web page rather than a file: such a page is
/// opened in the browser, never saved as the file.
fn is_web_page(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|value| {
        value
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/html")
    })
}

/// `name` inside `dir`, or `name (1)`, `name (2)`, … when taken, the way a
/// browser numbers a second download of the same file.
fn unique_path(dir: &Path, name: &str, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(name);
    if !taken(&first) {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|candidate| !taken(candidate))
        .expect("an unused name exists")
}

fn remember(path: &Path) {
    if let Ok(mut saved) = SAVED.lock() {
        saved.retain(|p| p != path);
        saved.push(path.to_path_buf());
        if saved.len() > MAX_SAVED {
            saved.remove(0);
        }
    }
}

/// Open `url` (or, with `save_as`, ask where to save it). Anything that is
/// not a first-party file URL is ignored.
pub(crate) fn start<R: Runtime>(app: &AppHandle<R>, url: url::Url, save_as: bool) {
    if !is_file_url(&url) {
        return;
    }
    let app = app.clone();
    // Own thread: cookie access, the dialog and the transfer must not run on
    // the UI thread.
    std::thread::spawn(move || {
        let title = if save_as { "Preparing your download…" } else { "Opening…" };
        toast(&app, serde_json::json!({ "kind": "busy", "title": title }));
        match tauri::async_runtime::block_on(fetch(&app, &url, save_as)) {
            Ok(Fetched::Saved(path)) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let shown = path.display().to_string();
                remember(&path);
                if save_as {
                    toast(
                        &app,
                        serde_json::json!({
                            "kind": "ok",
                            "title": "Download complete",
                            "detail": format!("{name}\nSaved to {shown}"),
                            "action": { "label": "Show in folder", "path": shown },
                        }),
                    );
                    offline::finish_regular_download(&app, Some(path), true);
                } else if system_open::open_path(&app, &shown).is_ok() {
                    toast(
                        &app,
                        serde_json::json!({
                            "kind": "ok",
                            "title": format!("Opened {name}"),
                            "detail": format!("A copy is in {shown}"),
                            "action": { "label": "Show in folder", "path": shown },
                        }),
                    );
                } else {
                    toast(
                        &app,
                        serde_json::json!({
                            "kind": "ok",
                            "title": "Saved to your Downloads",
                            "detail": format!("{name} could not be opened here. Open it from {shown}"),
                            "action": { "label": "Show in folder", "path": shown },
                        }),
                    );
                }
            }
            Ok(Fetched::Cancelled) => toast(&app, serde_json::json!({ "kind": "hide" })),
            Ok(Fetched::Page(page)) => {
                system_open::open_url(&app, page.as_str());
                toast(
                    &app,
                    serde_json::json!({
                        "kind": "ok",
                        "title": "Opened in your browser",
                        "detail": "This document is a link, so it opens where it lives.",
                    }),
                );
            }
            Err(_) => {
                toast(
                    &app,
                    serde_json::json!({
                        "kind": "error",
                        "title": "Download failed",
                        "detail": "The file could not be downloaded. Check your connection and try again.",
                    }),
                );
                offline::finish_regular_download(&app, None, false);
            }
        }
    });
}

/// Reveal a file this module saved in the system file manager.
pub(crate) fn reveal<R: Runtime>(app: &AppHandle<R>, path: &str) {
    let path = PathBuf::from(path);
    let known = SAVED
        .lock()
        .map(|saved| saved.contains(&path))
        .unwrap_or(false);
    if known {
        let _ = app.opener().reveal_item_in_dir(&path);
    }
}

/// Whole percent done, when the size is known.
fn percent(done: u64, total: Option<u64>) -> Option<u8> {
    let total = total.filter(|t| *t > 0)?;
    Some(((done.min(total) * 100) / total) as u8)
}

async fn fetch<R: Runtime>(
    app: &AppHandle<R>,
    url: &url::Url,
    save_as: bool,
) -> Result<Fetched, String> {
    let window = app.get_webview_window("main").ok_or("main window unavailable")?;
    let cookies = window
        .cookies_for_url(url.clone())
        .map_err(|e| e.to_string())?;
    let cookie_header = cookies
        .iter()
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect::<Vec<_>>()
        .join("; ");

    let mut response = reqwest::Client::new()
        .get(url.clone())
        .header(COOKIE, cookie_header)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    // Redirects are followed (reqwest drops the cookies on leaving the
    // tracker), so a link-type document ends on the other site's page.
    if is_web_page(
        response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
    ) {
        return Ok(Fetched::Page(response.url().clone()));
    }
    let total = response.content_length();

    let name = file_name(
        response
            .headers()
            .get(CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok()),
        // Where the redirects ended (/api/media/<key>), not the request
        // (/api/attachments/<id>/download): the name needs its extension
        // for the right app to open it.
        response.url(),
    );

    let downloads = app.path().download_dir().ok();
    let dest = if save_as {
        // Ask where to put it (native Save dialog, name pre-filled, Downloads
        // first), in front of the window: unparented, Windows could put it
        // behind. The transfer is already started, so this is quick after
        // "Save".
        toast(
            app,
            serde_json::json!({
                "kind": "busy",
                "title": "Choose where to save the file",
                "detail": name,
            }),
        );
        let mut dialog = app
            .dialog()
            .file()
            .set_parent(&window)
            .set_file_name(name.clone());
        if let Some(dir) = downloads {
            dialog = dialog.set_directory(dir);
        }
        let chosen = tauri::async_runtime::spawn_blocking(move || dialog.blocking_save_file())
            .await
            .map_err(|e| e.to_string())?;
        let Some(chosen) = chosen else {
            return Ok(Fetched::Cancelled);
        };
        chosen.into_path().map_err(|e| e.to_string())?
    } else {
        let dir = downloads.ok_or("no Downloads folder")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        unique_path(&dir, &name, |p| p.exists())
    };
    let part = PathBuf::from(format!("{}.cbb-part", dest.display()));

    toast(
        app,
        serde_json::json!({
            "kind": "busy",
            "title": if save_as { "Downloading…" } else { "Opening…" },
            "detail": name,
            "progress": 0,
        }),
    );

    let result = async {
        let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut done: u64 = 0;
        let mut last_update = Instant::now();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            file.write_all(&chunk).map_err(|e| e.to_string())?;
            done += chunk.len() as u64;
            if last_update.elapsed() >= PROGRESS_EVERY {
                last_update = Instant::now();
                toast(
                    app,
                    serde_json::json!({
                        "kind": "busy",
                        "title": if save_as { "Downloading…" } else { "Opening…" },
                        "detail": name,
                        "progress": percent(done, total),
                    }),
                );
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&part, &dest).map_err(|e| e.to_string())
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result.map(|()| Fetched::Saved(dest))
}

fn decode(raw: &str) -> String {
    percent_encoding::percent_decode_str(raw)
        .decode_utf8_lossy()
        .into_owned()
}

/// Pull a file name out of a `Content-Disposition` value, preferring the
/// RFC 5987 `filename*=UTF-8''…` form.
fn from_content_disposition(value: &str) -> Option<String> {
    let parts: Vec<&str> = value.split(';').map(str::trim).collect();
    for part in &parts {
        if part.to_ascii_lowercase().starts_with("filename*=") {
            let rest = &part["filename*=".len()..];
            let encoded = rest.split_once("''").map(|(_, v)| v).unwrap_or(rest);
            return Some(decode(encoded.trim_matches('"')));
        }
    }
    for part in &parts {
        if part.to_ascii_lowercase().starts_with("filename=") {
            return Some(part["filename=".len()..].trim_matches('"').to_string());
        }
    }
    None
}

/// Keep only a safe last path component for use inside Downloads.
fn sanitize(raw: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "download".to_string()
    } else {
        trimmed.chars().take(150).collect()
    }
}

fn file_name(content_disposition: Option<&str>, url: &url::Url) -> String {
    let raw = content_disposition
        .and_then(from_content_disposition)
        .or_else(|| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back().map(decode))
        })
        .unwrap_or_default();
    sanitize(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(raw: &str) -> url::Url {
        url::Url::parse(raw).expect("test URL parses")
    }

    #[test]
    fn only_first_party_file_routes_are_saved() {
        for raw in [
            "https://tracker.coolerboxbrothers.com/api/media/k%2Fx.png",
            "https://tracker.coolerboxbrothers.com/api/coolerbox/abc",
            "https://tracker.coolerboxbrothers.com/api/attachments/xyz/download",
            "https://tracker.coolerboxbrothers.com/api/call-sheets/1/pdf",
            "https://tracker.coolerboxbrothers.com/api/clients/1/brand-documents/a/download",
            "https://tracker.coolerboxbrothers.com/api/gear/2/file?kind=photo",
            "https://tracker.coolerboxbrothers.com/api/casting/selftape/9",
        ] {
            assert!(is_file_url(&parsed(raw)), "{raw}");
        }
        for raw in [
            "https://tracker.coolerboxbrothers.com/projects/1",
            "https://tracker.coolerboxbrothers.com/api/walkie/1/send",
            "https://tracker.coolerboxbrothers.com/api/time-sheet/clock",
            "https://tracker.coolerboxbrothers.com/api/notifications/recent",
            "https://evil.example/api/media/x",
            "http://tracker.coolerboxbrothers.com/api/media/x",
        ] {
            assert!(!is_file_url(&parsed(raw)), "{raw}");
        }
    }

    #[test]
    fn name_comes_from_content_disposition() {
        let url = parsed("https://tracker.coolerboxbrothers.com/api/media/ignored");
        assert_eq!(
            file_name(Some("attachment; filename=\"Reddit.png\""), &url),
            "Reddit.png"
        );
        assert_eq!(
            file_name(
                Some("inline; filename=\"fallback.png\"; filename*=UTF-8''Caf%C3%A9%20menu.pdf"),
                &url
            ),
            "Café menu.pdf"
        );
    }

    #[test]
    fn name_falls_back_to_the_url() {
        let url = parsed("https://tracker.coolerboxbrothers.com/api/media/projects%2Fabc%2F1700-Reddit.png");
        assert_eq!(file_name(None, &url), "1700-Reddit.png");
    }

    #[test]
    fn progress_percent() {
        assert_eq!(percent(0, Some(200)), Some(0));
        assert_eq!(percent(50, Some(200)), Some(25));
        assert_eq!(percent(500, Some(200)), Some(100));
        assert_eq!(percent(10, None), None);
        assert_eq!(percent(10, Some(0)), None);
    }

    #[test]
    fn a_second_copy_gets_a_number() {
        let dir = Path::new("/dl");
        let taken = |names: &'static [&'static str]| {
            move |p: &Path| names.iter().any(|n| dir.join(n) == p)
        };
        assert_eq!(unique_path(dir, "a.pdf", taken(&[])), dir.join("a.pdf"));
        assert_eq!(unique_path(dir, "a.pdf", taken(&["a.pdf"])), dir.join("a (1).pdf"));
        assert_eq!(
            unique_path(dir, "a.pdf", taken(&["a.pdf", "a (1).pdf"])),
            dir.join("a (2).pdf")
        );
        assert_eq!(unique_path(dir, "notes", taken(&["notes"])), dir.join("notes (1)"));
        assert_eq!(unique_path(dir, ".env", taken(&[".env"])), dir.join(".env (1)"));
    }

    #[test]
    fn a_web_page_is_not_a_file() {
        assert!(is_web_page(Some("text/html")));
        assert!(is_web_page(Some("text/html; charset=utf-8")));
        assert!(is_web_page(Some("TEXT/HTML")));
        assert!(!is_web_page(Some("application/pdf")));
        assert!(!is_web_page(Some(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        )));
        assert!(!is_web_page(Some("text/plain")));
        assert!(!is_web_page(None));
    }

    #[test]
    fn names_cannot_escape_downloads() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("..\\..\\boot.ini"), "boot.ini");
        assert_eq!(sanitize("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize("  ..  "), "download");
        assert_eq!(sanitize(""), "download");
    }
}
