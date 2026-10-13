//! "Suggest time from my work": the project timer's helper (hourly.rs) for
//! the time someone forgot to start the timer for.
//!
//! Off until the person switches it on themselves (quick panel › Settings);
//! never on by default, and the company can't switch it on. While it's on,
//! and only while no project timer is running, it notices two things:
//! - files saved in folders they linked to a project;
//! - the front window's name in apps they picked (Illustrator, Photoshop,
//!   InDesign, Figma, …), matched to their projects' client and project
//!   names and numbers. Time on an "Untitled" document goes to the project
//!   the same app's next document matches.
//!
//! From that it builds stretches ("Vimeo · Logo refresh 09:10–10:40") and,
//! when one ends (a switch, a pause, a wrap, starting the timer), asks in
//! the quick panel: "Worked on Vimeo · Logo refresh 09:10–10:40, log it?"
//! Only a stretch the person confirms is sent, and only its project, start
//! and end (`POST /api/me/project-time`, through hourly.rs).
//!
//! Window names and file paths never leave the computer and are never
//! stored: they're matched as they're seen and dropped. What's kept until
//! answered (or 7 days) is the project and the times. The linked folders and
//! picked apps are the person's own settings (`settings.json`). No analytics
//! either: whether it's on, and how it's used, isn't reported. The panel's
//! "What it sees, and what it doesn't" says all this, and is shown before it
//! can be switched on; keep the two in step.
//!
//! Window names: macOS (needs the Screen Recording permission to read other
//! apps' window titles; it's asked for when an app is picked) and Windows.
//! Linux: linked folders only.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime},
};

use serde::Serialize;
use tauri::{AppHandle, Runtime};

const ON: &str = "detector_on";
const APPS: &str = "detector_apps";
const FOLDERS: &str = "detector_folders";
const PENDING: &str = "detector_pending";

/// How often the front window is looked at.
const SAMPLE_EVERY: Duration = Duration::from_secs(15);
/// Linked folders are looked at this often.
const SCAN_EVERY_SECS: u64 = 60;
/// Away from a project longer than this ends its stretch.
pub(crate) const GAP_SECS: u64 = 5 * 60;
/// Saves in a linked folder this close together count as one stretch.
pub(crate) const SAVE_GAP_SECS: u64 = 20 * 60;
/// Stretches shorter than this aren't worth asking about.
pub(crate) const MIN_SECS: u64 = 10 * 60;
/// Unanswered suggestions go after a week.
pub(crate) const KEEP_SECS: u64 = 7 * 24 * 3600;
/// Not sampling past this much idle time (the idle rules cover a timer).
const IDLE_SECS: u64 = 2 * 60;
/// The most files looked at per linked folder, and how deep.
const SCAN_MAX_ENTRIES: usize = 5_000;
const SCAN_MAX_DEPTH: usize = 5;

/// The apps that can be picked: id, name, and how each system names it
/// (macOS: the app's name starts with; Windows: the program file).
pub(crate) const CATALOG: [(&str, &str, &str, &str); 8] = [
    ("illustrator", "Illustrator", "Adobe Illustrator", "illustrator.exe"),
    ("photoshop", "Photoshop", "Adobe Photoshop", "photoshop.exe"),
    ("indesign", "InDesign", "Adobe InDesign", "indesign.exe"),
    ("aftereffects", "After Effects", "Adobe After Effects", "afterfx.exe"),
    ("premiere", "Premiere Pro", "Adobe Premiere Pro", "adobe premiere pro.exe"),
    ("figma", "Figma", "Figma", "figma.exe"),
    ("resolve", "DaVinci Resolve", "DaVinci Resolve", "resolve.exe"),
    ("blender", "Blender", "Blender", "blender.exe"),
];

/// The front window: the app's name (macOS) or program file (Windows), and
/// its title ("" when it can't be read).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Front {
    pub app: String,
    pub title: String,
}

/// Which picked app `front` is, if any.
pub(crate) fn app_id(front: &Front, picked: &[String]) -> Option<&'static str> {
    let app = front.app.to_lowercase();
    CATALOG
        .iter()
        .filter(|(id, ..)| picked.iter().any(|p| p == id))
        .find(|(_, _, mac, win)| app.starts_with(&mac.to_lowercase()) || app == *win)
        .map(|(id, ..)| *id)
}

/// What a moment of work points at.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Hit {
    /// A project (by id); `app` when it came from a window, `None` from a
    /// folder save.
    Project { project: String, app: Option<&'static str> },
    /// A picked app's "Untitled" document: goes to that app's next project.
    Untitled(&'static str),
    /// A picked app, but nothing to match.
    Unknown,
}

/// Matches a window title to one of the projects: its number ("CB-0412"),
/// its name ("Logo refresh"), or its client's name when the client has only
/// that project. The longest match wins.
pub(crate) fn match_title(title: &str, projects: &[crate::hourly::Project]) -> Option<String> {
    let title = fold(title);
    if title.is_empty() {
        return None;
    }
    let mut best: Option<(usize, &str)> = None;
    for project in projects {
        let client = project.client_name.as_deref().map(fold).unwrap_or_default();
        let only_project = !client.is_empty()
            && projects.iter().filter(|p| p.client_name.as_deref().map(fold).as_deref() == Some(client.as_str())).count() == 1;
        let candidates = [
            (fold(&project.project_number), 3),
            (fold(&project.name), 4),
            (if only_project { client } else { String::new() }, 3),
        ];
        for (needle, min) in candidates {
            if needle.chars().count() >= min && contains_words(&title, &needle) && best.is_none_or(|(len, _)| needle.len() > len) {
                best = Some((needle.len(), project.id.as_str()));
            }
        }
    }
    best.map(|(_, id)| id.to_string())
}

/// Lowercase, with anything that isn't a letter or digit as one space
/// ("Logo_Refresh-v2.ai" → "logo refresh v2 ai").
fn fold(text: &str) -> String {
    let spaced: String = text.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' }).collect();
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `needle` appears in `hay` on word boundaries ("logo" in "logo v2", not in
/// "logotype").
fn contains_words(hay: &str, needle: &str) -> bool {
    let hay = format!(" {hay} ");
    hay.contains(&format!(" {needle} "))
}

pub(crate) fn is_untitled(title: &str) -> bool {
    let title = title.to_lowercase();
    title.contains("untitled") || title.contains("naamloos")
}

/// A stretch of work on one project (unix seconds).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stretch {
    pub project: String,
    pub start: u64,
    pub end: u64,
}

/// Turns moments of work into stretches.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    current: Option<Stretch>,
    /// The latest stretch of work only seen on an untitled document, by app.
    untitled: Option<(&'static str, u64, u64)>,
    /// The last moment `current` was seen through a folder save.
    by_save: bool,
}

impl Tracker {
    /// One moment at `now`; returns a stretch that just ended, if any.
    pub(crate) fn step(&mut self, now: u64, hit: Option<Hit>) -> Option<Stretch> {
        let mut ended = self.expire(now);
        match hit {
            Some(Hit::Project { project, app }) => {
                // An untitled document's time goes to the project its app (or
                // a save) shows next.
                let untitled_start = self
                    .untitled
                    .take_if(|(u_app, _, last)| (app.is_none() || app == Some(*u_app)) && now.saturating_sub(*last) <= GAP_SECS)
                    .map(|(_, start, _)| start);
                let from_save = app.is_none();
                match &mut self.current {
                    Some(current) if current.project == project => {
                        current.end = now;
                        if let Some(start) = untitled_start {
                            current.start = current.start.min(start);
                        }
                    }
                    _ => {
                        if let Some(done) = self.current.take() {
                            ended = ended.or(Some(done));
                        }
                        self.current = Some(Stretch { project, start: untitled_start.unwrap_or(now).min(now), end: now });
                    }
                }
                self.by_save = from_save;
            }
            Some(Hit::Untitled(app)) => {
                self.untitled = match self.untitled {
                    Some((u_app, start, last)) if u_app == app && now.saturating_sub(last) <= GAP_SECS => Some((app, start, now)),
                    _ => Some((app, now, now)),
                };
            }
            Some(Hit::Unknown) | None => {}
        }
        ended.filter(|s| s.end.saturating_sub(s.start) >= MIN_SECS)
    }

    /// The current stretch, ended if it's been quiet too long.
    fn expire(&mut self, now: u64) -> Option<Stretch> {
        let gap = if self.by_save { SAVE_GAP_SECS } else { GAP_SECS };
        if self.current.as_ref().is_some_and(|c| now.saturating_sub(c.end) > gap) {
            return self.current.take();
        }
        None
    }

    /// End the current stretch now (a wrap, the timer started, switched off).
    pub(crate) fn close(&mut self) -> Option<Stretch> {
        self.untitled = None;
        self.current.take().filter(|s| s.end.saturating_sub(s.start) >= MIN_SECS)
    }
}

/// A suggestion waiting for an answer, as kept in `settings.json`:
/// "start|end|project". No names or paths.
pub(crate) fn encode(s: &Stretch) -> String {
    format!("{}|{}|{}", s.start, s.end, s.project)
}

pub(crate) fn decode(raw: &str) -> Option<Stretch> {
    let mut parts = raw.splitn(3, '|');
    let start = parts.next()?.parse().ok()?;
    let end = parts.next()?.parse().ok()?;
    let project = parts.next()?.to_string();
    (end > start && crate::hourly::valid_id(&project)).then_some(Stretch { project, start, end })
}

/// The pending suggestions still worth asking about at `now`.
pub(crate) fn fresh(pending: &[String], now: u64) -> Vec<Stretch> {
    pending.iter().filter_map(|raw| decode(raw)).filter(|s| now.saturating_sub(s.end) <= KEEP_SECS).collect()
}

/// A linked folder, kept as "project\tpath".
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Linked {
    pub project: String,
    pub path: PathBuf,
}

pub(crate) fn decode_linked(raw: &str) -> Option<Linked> {
    let (project, path) = raw.split_once('\t')?;
    (crate::hourly::valid_id(project) && !path.is_empty()).then(|| Linked { project: project.into(), path: path.into() })
}

/// Files under `root` saved after `since`, looked at up to a limit (hidden
/// folders and the usual build/cache folders skipped).
pub(crate) fn saved_since(root: &Path, since: SystemTime) -> bool {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > SCAN_MAX_ENTRIES {
                return false;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || matches!(name.as_ref(), "node_modules" | "Cache" | "cache" | "Media Cache Files" | "Adobe Premiere Pro Auto-Save") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                if depth < SCAN_MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
            } else if meta.modified().is_ok_and(|at| at > since) {
                return true;
            }
        }
    }
    false
}

/// The last two parts of a path, for the panel ("Vimeo/Logo refresh").
pub(crate) fn short_path(path: &Path) -> String {
    let parts: Vec<String> = path.components().rev().take(2).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

// ── the front window, per system ──

#[cfg(target_os = "macos")]
mod platform {
    use super::Front;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use std::ffi::{c_void, CStr};

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> *const c_void;
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    /// kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements.
    const ON_SCREEN: u32 = 1 | 16;

    unsafe fn ns_string(text: &CStr) -> *mut AnyObject {
        match AnyClass::get(c"NSString") {
            Some(class) => msg_send![class, stringWithUTF8String: text.as_ptr()],
            None => std::ptr::null_mut(),
        }
    }

    unsafe fn rust_string(value: *mut AnyObject) -> String {
        if value.is_null() {
            return String::new();
        }
        let utf8: *const std::ffi::c_char = msg_send![value, UTF8String];
        if utf8.is_null() {
            String::new()
        } else {
            CStr::from_ptr(utf8).to_string_lossy().into_owned()
        }
    }

    pub(super) fn front() -> Option<Front> {
        let workspace_class = AnyClass::get(c"NSWorkspace")?;
        objc2::rc::autoreleasepool(|_| {
            // SAFETY: AppKit/CoreGraphics queries; every object is checked
            // for nil, the window list is released, the pool takes the rest.
            unsafe {
                let workspace: *mut AnyObject = msg_send![workspace_class, sharedWorkspace];
                if workspace.is_null() {
                    return None;
                }
                let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
                if app.is_null() {
                    return None;
                }
                let name: *mut AnyObject = msg_send![app, localizedName];
                let pid: i32 = msg_send![app, processIdentifier];
                let app_name = rust_string(name);
                let mut title = String::new();
                let list = CGWindowListCopyWindowInfo(ON_SCREEN, 0) as *mut AnyObject;
                if !list.is_null() {
                    let (owner_key, layer_key, name_key) =
                        (ns_string(c"kCGWindowOwnerPID"), ns_string(c"kCGWindowLayer"), ns_string(c"kCGWindowName"));
                    let count: usize = msg_send![list, count];
                    for index in 0..count {
                        let window: *mut AnyObject = msg_send![list, objectAtIndex: index];
                        let owner: *mut AnyObject = msg_send![window, objectForKey: owner_key];
                        let layer: *mut AnyObject = msg_send![window, objectForKey: layer_key];
                        if owner.is_null() || layer.is_null() {
                            continue;
                        }
                        let owner: i32 = msg_send![owner, intValue];
                        let layer: i32 = msg_send![layer, intValue];
                        if owner != pid || layer != 0 {
                            continue;
                        }
                        let window_name: *mut AnyObject = msg_send![window, objectForKey: name_key];
                        let text = rust_string(window_name);
                        if !text.trim().is_empty() {
                            title = text;
                            break;
                        }
                    }
                    CFRelease(list as *const c_void);
                }
                Some(Front { app: app_name, title })
            }
        })
    }

    /// Whether macOS lets the app read other apps' window titles.
    pub(super) fn titles_allowed() -> bool {
        // SAFETY: no arguments.
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    /// Ask macOS for it (a system prompt the first time; System Settings
    /// after that).
    pub(super) fn ask_titles() {
        // SAFETY: no arguments.
        unsafe {
            if !CGRequestScreenCaptureAccess() {
                let _ = std::process::Command::new("open")
                    .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
                    .spawn();
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::Front;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId};

    pub(super) fn front() -> Option<Front> {
        // SAFETY: plain Win32 queries with correctly sized buffers; the
        // process handle is closed.
        unsafe {
            let window = GetForegroundWindow();
            if window.is_null() {
                return None;
            }
            let mut text = [0u16; 512];
            let len = GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32);
            let title = String::from_utf16_lossy(&text[..len.max(0) as usize]);
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, &mut pid);
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return Some(Front { app: String::new(), title });
            }
            let mut path = [0u16; 1024];
            let mut size = path.len() as u32;
            let ok = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut size);
            CloseHandle(process);
            let path = if ok != 0 { String::from_utf16_lossy(&path[..size as usize]) } else { String::new() };
            let app = path.rsplit(['\\', '/']).next().unwrap_or_default().to_lowercase();
            Some(Front { app, title })
        }
    }

    pub(super) fn titles_allowed() -> bool {
        true
    }

    pub(super) fn ask_titles() {}
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::Front;

    pub(super) fn front() -> Option<Front> {
        None
    }

    pub(super) fn titles_allowed() -> bool {
        false
    }

    pub(super) fn ask_titles() {}
}

// ── state ──

#[derive(Default)]
struct Inner {
    tracker: Tracker,
    /// When each linked folder was last looked at.
    scanned: Option<SystemTime>,
    last_scan: u64,
}

static STATE: Mutex<Option<Inner>> = Mutex::new(None);

fn with_state<T>(run: impl FnOnce(&mut Inner) -> T) -> Option<T> {
    let mut slot = STATE.lock().ok()?;
    Some(run(slot.get_or_insert_with(Inner::default)))
}

pub(crate) fn is_on<R: Runtime>(app: &AppHandle<R>) -> bool {
    crate::close::flag(app, ON)
}

fn picked<R: Runtime>(app: &AppHandle<R>) -> Vec<String> {
    crate::close::strings(app, APPS)
}

fn linked<R: Runtime>(app: &AppHandle<R>) -> Vec<Linked> {
    crate::close::strings(app, FOLDERS).iter().filter_map(|raw| decode_linked(raw)).collect()
}

fn pending<R: Runtime>(app: &AppHandle<R>) -> Vec<Stretch> {
    fresh(&crate::close::strings(app, PENDING), crate::clock::unix_now())
}

fn set_pending<R: Runtime>(app: &AppHandle<R>, list: &[Stretch]) {
    let raw: Vec<String> = list.iter().map(encode).collect();
    crate::close::set_strings(app, PENDING, &raw);
}

/// What the panel shows about it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct View {
    pub on: bool,
    pub apps: Vec<AppChoice>,
    pub folders: Vec<FolderRow>,
    /// Window titles can be read (macOS asks for Screen Recording).
    pub titles_allowed: bool,
    /// Window titles are read on this system at all (not Linux).
    pub titles_supported: bool,
    /// The oldest suggestion waiting for an answer, and how many more.
    pub suggestion: Option<SuggestionView>,
    pub more: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AppChoice {
    pub id: &'static str,
    pub label: &'static str,
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FolderRow {
    pub index: usize,
    pub folder: String,
    pub project: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SuggestionView {
    /// Its start (unix seconds): the answer names it.
    pub id: u64,
    pub project_id: String,
    pub label: String,
    pub billable: bool,
    /// Unix milliseconds, for the panel's own time formatting.
    pub from: u64,
    pub to: u64,
}

pub(crate) fn view<R: Runtime>(app: &AppHandle<R>, info: &crate::hourly::Info) -> View {
    let on = is_on(app);
    let picked = picked(app);
    let label_of = |id: &str| {
        info.projects.iter().find(|p| p.id == id).map(crate::hourly::project_label).unwrap_or_else(|| "A project you're no longer on".into())
    };
    let waiting = if on { pending(app) } else { Vec::new() };
    let suggestion = waiting.first().map(|s| SuggestionView {
        id: s.start,
        project_id: s.project.clone(),
        label: label_of(&s.project),
        billable: info.projects.iter().any(|p| p.id == s.project && p.billable),
        from: s.start * 1000,
        to: s.end * 1000,
    });
    View {
        on,
        apps: CATALOG.iter().map(|(id, label, ..)| AppChoice { id, label, on: picked.iter().any(|p| p == id) }).collect(),
        folders: linked(app)
            .iter()
            .enumerate()
            .map(|(index, l)| FolderRow { index, folder: short_path(&l.path), project: label_of(&l.project) })
            .collect(),
        titles_allowed: platform::titles_allowed(),
        titles_supported: cfg!(any(target_os = "macos", windows)),
        suggestion,
        more: waiting.len().saturating_sub(1),
    }
}

/// A stretch ended: keep it to ask about, and say so once.
fn suggest<R: Runtime>(app: &AppHandle<R>, stretch: Stretch) {
    let mut list = pending(app);
    if list.iter().any(|s| s.project == stretch.project && s.start == stretch.start) {
        return;
    }
    list.push(stretch.clone());
    set_pending(app, &list);
    crate::mini::push(app);
    if crate::quiet::allowed(app, crate::quiet::Kind::Tracker) {
        let label = crate::hourly::info_snapshot(app)
            .and_then(|info| info.projects.iter().find(|p| p.id == stretch.project).map(crate::hourly::project_label))
            .unwrap_or_default();
        let body = format!(
            "{label} {}–{}. Log it?",
            crate::clock::fmt_hhmm_sast(stretch.start),
            crate::clock::fmt_hhmm_sast(stretch.end)
        );
        crate::banner::show(app, "Worked on this?", &body, crate::banner::Target::Panel);
    }
}

/// End the stretch being built now (a wrap on the time sheet).
pub(crate) fn close_now<R: Runtime>(app: &AppHandle<R>) {
    if let Some(stretch) = with_state(|state| state.tracker.close()).flatten() {
        suggest(app, stretch);
    }
}

/// One look: the front window and, every minute, the linked folders.
fn tick<R: Runtime>(app: &AppHandle<R>, now: u64) {
    if !is_on(app) || !crate::session::signed_in() {
        with_state(|state| *state = Inner::default());
        return;
    }
    let Some(info) = crate::hourly::info_snapshot(app) else { return };
    // Timing already: nothing to suggest; what was being built ends here.
    if info.running.is_some() {
        close_now(app);
        return;
    }
    if crate::idle::idle_seconds().is_some_and(|idle| idle >= IDLE_SECS) {
        if let Some(stretch) = with_state(|state| state.tracker.step(now, None)).flatten() {
            suggest(app, stretch);
        }
        return;
    }
    let picked = picked(app);
    let mut hits: Vec<Hit> = Vec::new();
    if let Some(front) = platform::front() {
        if let Some(app_id) = app_id(&front, &picked) {
            hits.push(if is_untitled(&front.title) {
                Hit::Untitled(app_id)
            } else {
                match match_title(&front.title, &info.projects) {
                    Some(project) => Hit::Project { project, app: Some(app_id) },
                    None => Hit::Unknown,
                }
            });
        }
        // The title is dropped here: never kept or sent.
    }
    let scan_due = with_state(|state| now.saturating_sub(state.last_scan) >= SCAN_EVERY_SECS).unwrap_or(false);
    if scan_due {
        let since = with_state(|state| state.scanned).flatten().unwrap_or_else(SystemTime::now);
        let scanned_at = SystemTime::now();
        for folder in linked(app) {
            if info.projects.iter().any(|p| p.id == folder.project) && saved_since(&folder.path, since) {
                hits.push(Hit::Project { project: folder.project.clone(), app: None });
            }
        }
        with_state(|state| {
            state.scanned = Some(scanned_at);
            state.last_scan = now;
        });
    }
    if hits.is_empty() {
        hits.push(Hit::Unknown);
    }
    for hit in hits {
        if let Some(stretch) = with_state(|state| state.tracker.step(now, Some(hit))).flatten() {
            suggest(app, stretch);
        }
    }
}

/// Start the helper's loop for the app's life (it does nothing while off).
pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(SAMPLE_EVERY);
        tick(&app, crate::clock::unix_now());
    });
}

// ── the panel's commands ──

/// Settings › "Suggest time from my work", on or off. Off forgets what it
/// was building and any unanswered suggestions.
#[tauri::command]
pub(crate) fn mini_detector_toggle<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let on = !is_on(&app);
    crate::close::set_flag(&app, ON, on);
    if !on {
        with_state(|state| *state = Inner::default());
        set_pending(&app, &[]);
    }
    crate::mini::push(&app);
    Ok(())
}

/// Pick or unpick an app. Picking one on macOS asks for the permission to
/// read window titles, if it isn't given yet.
#[tauri::command]
pub(crate) fn mini_detector_app<R: Runtime>(app: AppHandle<R>, id: String) -> Result<(), String> {
    if !CATALOG.iter().any(|(known, ..)| *known == id) {
        return Err("Unknown app.".into());
    }
    let mut list = picked(&app);
    let on = !list.contains(&id);
    if on {
        list.push(id);
    } else {
        list.retain(|p| p != &id);
    }
    crate::close::set_strings(&app, APPS, &list);
    if on && !platform::titles_allowed() {
        platform::ask_titles();
    }
    crate::mini::push(&app);
    Ok(())
}

/// Ask again for the permission to read window titles (macOS).
#[tauri::command]
pub(crate) fn mini_detector_permission<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    platform::ask_titles();
    crate::mini::push(&app);
    Ok(())
}

/// The folder being linked, between choosing it and choosing its project.
static LINKING: Mutex<Option<PathBuf>> = Mutex::new(None);

/// "Link a folder…": choose it, then the panel asks for its project.
#[tauri::command]
pub(crate) async fn mini_detector_link<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    use tauri_plugin_dialog::DialogExt;
    let dialog = app.dialog().file().set_title("Link a folder to a project");
    let chosen = tauri::async_runtime::spawn_blocking(move || dialog.blocking_pick_folder())
        .await
        .map_err(|e| e.to_string())?;
    let Some(chosen) = chosen else { return Ok(()) };
    let path = chosen.into_path().map_err(|e| e.to_string())?;
    let folder = short_path(&path);
    if let Ok(mut slot) = LINKING.lock() {
        *slot = Some(path);
    }
    crate::mini::ask_hourly(&app, "__cbbMiniDetectorLink", &serde_json::json!({ "folder": folder }));
    Ok(())
}

/// The project for the folder just chosen.
#[tauri::command]
pub(crate) fn mini_detector_link_project<R: Runtime>(app: AppHandle<R>, project_id: String) -> Result<(), String> {
    let info = crate::hourly::info_snapshot(&app).ok_or("The project timer isn't on for you.")?;
    if !info.projects.iter().any(|p| p.id == project_id) {
        return Err("That project isn't one of yours.".into());
    }
    let path = LINKING.lock().ok().and_then(|mut slot| slot.take()).ok_or("Choose the folder again.")?;
    let mut list = crate::close::strings(&app, FOLDERS);
    let line = format!("{project_id}\t{}", path.display());
    if !list.contains(&line) {
        list.push(line);
    }
    crate::close::set_strings(&app, FOLDERS, &list);
    crate::mini::push(&app);
    Ok(())
}

#[tauri::command]
pub(crate) fn mini_detector_unlink<R: Runtime>(app: AppHandle<R>, index: usize) -> Result<(), String> {
    let mut list = crate::close::strings(&app, FOLDERS);
    if index < list.len() {
        list.remove(index);
    }
    crate::close::set_strings(&app, FOLDERS, &list);
    crate::mini::push(&app);
    Ok(())
}

/// The answer to "Worked on X 09:10–10:40, log it?": `log` (on its own
/// project, or `project_id` when they chose another) or `skip`.
#[tauri::command]
pub(crate) fn mini_detector_answer<R: Runtime>(
    app: AppHandle<R>,
    id: u64,
    choice: String,
    project_id: Option<String>,
) -> Result<(), String> {
    let list = pending(&app);
    let stretch = list.iter().find(|s| s.start == id).cloned().ok_or("That's already been answered.")?;
    match choice.as_str() {
        "log" => {
            let project = project_id.unwrap_or_else(|| stretch.project.clone());
            crate::hourly::log_stretch(&app, &project, stretch.start, stretch.end, id)?;
        }
        "skip" => {
            answered(&app, id);
        }
        _ => return Err("Unknown answer.".into()),
    }
    Ok(())
}

/// A suggestion is done with (logged, or skipped): forget it.
pub(crate) fn answered<R: Runtime>(app: &AppHandle<R>, id: u64) {
    let list: Vec<Stretch> = pending(app).into_iter().filter(|s| s.start != id).collect();
    set_pending(app, &list);
    crate::mini::push(app);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hourly::Project;

    fn project(id: &str, number: &str, name: &str, client: Option<&str>) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            project_number: number.into(),
            client_name: client.map(str::to_string),
            billable: true,
            retainer_minutes_left: None,
        }
    }

    fn projects() -> Vec<Project> {
        vec![
            project("p1", "CB-0412", "Logo refresh", Some("Vimeo")),
            project("p2", "CB-0399", "Brand film", Some("Spirit Films")),
            project("p3", "CB-0400", "Brand film cutdowns", Some("Spirit Films")),
            project("p4", "CB-0300", "Annual report", Some("First Bank")),
        ]
    }

    #[test]
    fn titles_match_by_number_name_or_a_one_project_client() {
        let ps = projects();
        assert_eq!(match_title("CB-0412_logo_v3.ai @ 66% (RGB/Preview)", &ps).as_deref(), Some("p1"));
        assert_eq!(match_title("Logo Refresh - Board 2.psd", &ps).as_deref(), Some("p1"));
        // The longer name wins over a shorter one inside it.
        assert_eq!(match_title("brand film cutdowns 15s.prproj", &ps).as_deref(), Some("p3"));
        assert_eq!(match_title("Brand film master.prproj", &ps).as_deref(), Some("p2"));
        // A client with one project names it; with two, it doesn't.
        assert_eq!(match_title("Vimeo socials.fig", &ps).as_deref(), Some("p1"));
        assert_eq!(match_title("Spirit Films pitch.indd", &ps), None);
        // Whole words only.
        assert_eq!(match_title("logotype study", &ps), None);
        assert_eq!(match_title("", &ps), None);
        assert!(is_untitled("Untitled-1 @ 100% (RGB/8)"));
        assert!(!is_untitled("Logo refresh.ai"));
    }

    #[test]
    fn only_picked_apps_count() {
        let picked = vec!["photoshop".to_string(), "figma".to_string()];
        let mac = Front { app: "Adobe Photoshop 2026".into(), title: String::new() };
        let win = Front { app: "photoshop.exe".into(), title: String::new() };
        let other = Front { app: "Adobe Illustrator 2026".into(), title: String::new() };
        assert_eq!(app_id(&mac, &picked), Some("photoshop"));
        assert_eq!(app_id(&win, &picked), Some("photoshop"));
        assert_eq!(app_id(&other, &picked), None, "not picked");
        assert_eq!(app_id(&Front { app: "Safari".into(), title: String::new() }, &picked), None);
    }

    const T0: u64 = 1_791_788_400;

    fn on(project: &str) -> Option<Hit> {
        Some(Hit::Project { project: project.into(), app: Some("photoshop") })
    }

    #[test]
    fn stretches_end_on_a_switch_or_a_pause_and_short_ones_are_dropped() {
        let mut t = Tracker::default();
        for minute in 0..=40 {
            assert_eq!(t.step(T0 + minute * 60, on("p1")), None);
        }
        // Switching ends p1's 40 minutes.
        assert_eq!(t.step(T0 + 41 * 60, on("p2")), Some(Stretch { project: "p1".into(), start: T0, end: T0 + 40 * 60 }));
        // Five minutes on p2, then a long pause: too short to ask about.
        for minute in 42..=46 {
            t.step(T0 + minute * 60, on("p2"));
        }
        assert_eq!(t.step(T0 + 60 * 60, None), None);
        // A short look elsewhere doesn't break a stretch.
        let mut t = Tracker::default();
        for minute in 0..=10 {
            t.step(T0 + minute * 60, on("p1"));
        }
        t.step(T0 + 13 * 60, Some(Hit::Unknown));
        t.step(T0 + 14 * 60, on("p1"));
        assert_eq!(t.close(), Some(Stretch { project: "p1".into(), start: T0, end: T0 + 14 * 60 }));
    }

    #[test]
    fn untitled_time_goes_to_the_project_its_app_shows_next() {
        let mut t = Tracker::default();
        for minute in 0..20 {
            t.step(T0 + minute * 60, Some(Hit::Untitled("photoshop")));
        }
        // Saved as "Logo refresh.psd".
        for minute in 20..=30 {
            t.step(T0 + minute * 60, on("p1"));
        }
        assert_eq!(t.close(), Some(Stretch { project: "p1".into(), start: T0, end: T0 + 30 * 60 }));
        // Another app's next project doesn't take it.
        let mut t = Tracker::default();
        t.step(T0, Some(Hit::Untitled("illustrator")));
        for minute in 1..=12 {
            t.step(T0 + minute * 60, on("p1"));
        }
        assert_eq!(t.close().map(|s| s.start), Some(T0 + 60));
    }

    #[test]
    fn folder_saves_hold_a_stretch_for_longer() {
        let mut t = Tracker::default();
        let save = |project: &str| Some(Hit::Project { project: project.into(), app: None });
        t.step(T0, save("p4"));
        assert_eq!(t.step(T0 + 15 * 60, save("p4")), None, "15 minutes between saves: one stretch");
        assert_eq!(t.step(T0 + 30 * 60, save("p4")), None);
        assert_eq!(t.step(T0 + 60 * 60, None), Some(Stretch { project: "p4".into(), start: T0, end: T0 + 30 * 60 }));
    }

    #[test]
    fn suggestions_keep_only_the_project_and_times_for_a_week() {
        let s = Stretch { project: "p1".into(), start: T0, end: T0 + 3600 };
        let raw = encode(&s);
        assert_eq!(raw, format!("{T0}|{}|p1", T0 + 3600));
        assert_eq!(decode(&raw), Some(s.clone()));
        assert_eq!(decode("x|y|p1"), None);
        assert_eq!(decode(&format!("{T0}|{T0}|p1")), None);
        assert_eq!(decode(&format!("{T0}|{}|a b", T0 + 1)), None);
        assert_eq!(fresh(std::slice::from_ref(&raw), T0 + 3600 + KEEP_SECS), vec![s]);
        assert!(fresh(&[raw], T0 + 3601 + KEEP_SECS).is_empty());
        assert_eq!(
            decode_linked("p1\t/Users/sam/Work/Vimeo/Logo refresh"),
            Some(Linked { project: "p1".into(), path: "/Users/sam/Work/Vimeo/Logo refresh".into() })
        );
        assert_eq!(short_path(Path::new("/Users/sam/Work/Vimeo/Logo refresh")), "Vimeo/Logo refresh");
    }

    #[test]
    fn a_save_in_a_linked_folder_is_noticed() {
        let dir = std::env::temp_dir().join(format!("cbb-detector-{}", std::process::id()));
        let nested = dir.join("Exports/v2");
        std::fs::create_dir_all(&nested).unwrap();
        let before = SystemTime::now() - Duration::from_secs(5);
        assert!(!saved_since(&dir, SystemTime::now() + Duration::from_secs(60)));
        std::fs::write(nested.join("logo.ai"), b"x").unwrap();
        assert!(saved_since(&dir, before));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
