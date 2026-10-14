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
//! Only a stretch the person confirms is logged (`POST /api/me/project-time`,
//! through hourly.rs). A stretch still waiting for an answer is also handed
//! to the tracker (`POST /api/me/project-time-suggestions`), with only its
//! project (none when no project was recognised, never the app) and its
//! times, so it can be answered on My Hours too ("Still to confirm"); one
//! answered there, or on another computer, is forgotten here (`sync`).
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
use serde_json::{json, Value};
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

/// The apps that can be picked: id, name, kind, and how each system names
/// it: macOS, what the app's name starts with; Windows, what its program
/// file starts with ("" when it isn't on that system; "a|b" for either).
/// Anything else can be added from the apps that are open ("Add another
/// app…", `CUSTOM`). In a browser only pages that name a project count: the
/// rest of the web isn't asked about.
pub(crate) const CATALOG: &[(&str, &str, &str, &str, &str)] = &[
    ("illustrator", "Illustrator", "Design", "Adobe Illustrator", "illustrator"),
    ("photoshop", "Photoshop", "Design", "Adobe Photoshop", "photoshop"),
    ("indesign", "InDesign", "Design", "Adobe InDesign", "indesign"),
    ("figma", "Figma", "Design", "Figma", "figma"),
    ("sketch", "Sketch", "Design", "Sketch", ""),
    ("affinity-designer", "Affinity Designer", "Design", "Affinity Designer", ""),
    ("affinity-photo", "Affinity Photo", "Design", "Affinity Photo", ""),
    ("affinity-publisher", "Affinity Publisher", "Design", "Affinity Publisher", ""),
    ("canva", "Canva", "Design", "Canva", "canva"),
    ("lightroom", "Lightroom", "Design", "Adobe Lightroom", ""),
    ("lightroom-classic", "Lightroom Classic", "Design", "Adobe Lightroom Classic", "lightroom"),
    ("captureone", "Capture One", "Design", "Capture One", "captureone"),
    ("premiere", "Premiere Pro", "Video", "Adobe Premiere Pro", "adobe premiere pro"),
    ("aftereffects", "After Effects", "Video", "Adobe After Effects", "afterfx"),
    ("mediaencoder", "Media Encoder", "Video", "Adobe Media Encoder", "adobe media encoder"),
    ("resolve", "DaVinci Resolve", "Video", "DaVinci Resolve", "resolve"),
    ("finalcut", "Final Cut Pro", "Video", "Final Cut Pro", ""),
    ("mediacomposer", "Media Composer", "Video", "Avid Media Composer", "avidmediacomposer"),
    ("audition", "Audition", "Audio", "Adobe Audition", "adobe audition"),
    ("protools", "Pro Tools", "Audio", "Pro Tools", "protools"),
    ("logic", "Logic Pro", "Audio", "Logic Pro", ""),
    ("ableton", "Ableton Live", "Audio", "Ableton Live", "ableton live"),
    ("reaper", "Reaper", "Audio", "REAPER", "reaper"),
    ("cubase", "Cubase", "Audio", "Cubase", "cubase"),
    ("blender", "Blender", "3D", "Blender", "blender"),
    ("cinema4d", "Cinema 4D", "3D", "Cinema 4D", "cinema 4d"),
    ("maya", "Maya", "3D", "Maya", "maya"),
    ("houdini", "Houdini", "3D", "Houdini", "houdini"),
    ("vscode", "VS Code", "Code & AI", "Code|Visual Studio Code", "code"),
    ("cursor", "Cursor", "Code & AI", "Cursor", "cursor"),
    ("windsurf", "Windsurf", "Code & AI", "Windsurf", "windsurf"),
    ("claude", "Claude", "Code & AI", "Claude", "claude"),
    ("chatgpt", "ChatGPT", "Code & AI", "ChatGPT", "chatgpt"),
    ("terminal", "Terminal", "Code & AI", "Terminal|iTerm2|Warp|Ghostty", "windowsterminal|warp"),
    ("chrome", "Chrome", "Browser", "Google Chrome", "chrome"),
    ("safari", "Safari", "Browser", "Safari", ""),
    ("firefox", "Firefox", "Browser", "Firefox", "firefox"),
    ("edge", "Edge", "Browser", "Microsoft Edge", "msedge"),
    ("arc", "Arc", "Browser", "Arc", "arc"),
    ("brave", "Brave", "Browser", "Brave Browser", "brave"),
    ("word", "Word", "Documents", "Microsoft Word", "winword"),
    ("excel", "Excel", "Documents", "Microsoft Excel", "excel"),
    ("powerpoint", "PowerPoint", "Documents", "Microsoft PowerPoint", "powerpnt"),
    ("keynote", "Keynote", "Documents", "Keynote", ""),
    ("pages", "Pages", "Documents", "Pages", ""),
];

/// A picked app of the person's own, by the name their system gives it
/// ("custom:Pro Tools", "custom:studio one.exe").
pub(crate) const CUSTOM: &str = "custom:";

/// The front window: the app's name (macOS) or program file (Windows), and
/// its title ("" when it can't be read). `own` is CoolerBox Tracker itself
/// (the quick panel open to look at Right now): not work, and not a reason
/// to forget what was in front before it.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Front {
    pub app: String,
    pub title: String,
    pub own: bool,
}

/// The catalogue entry an app name or program file is, if any: the longest
/// match, so "Adobe Lightroom Classic" isn't taken for Lightroom.
pub(crate) fn catalog_id(app: &str) -> Option<&'static str> {
    let app = app.trim().to_lowercase();
    if app.is_empty() {
        return None;
    }
    CATALOG
        .iter()
        .filter_map(|(id, _, _, mac, win)| {
            let len = [mac, win]
                .into_iter()
                .flat_map(|names| names.split('|'))
                .filter(|name| !name.is_empty() && starts_with_name(&app, &name.to_lowercase()))
                .map(str::len)
                .max()?;
            Some((len, *id))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, id)| id)
}

/// `app` is `name`, or `name` and then more that isn't a word going on
/// ("adobe photoshop 2026", "photoshop.exe"; not "archicad" for "arc").
fn starts_with_name(app: &str, name: &str) -> bool {
    app.strip_prefix(name).is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
}

/// Which picked app `front` is, if any.
pub(crate) fn app_id(front: &Front, picked: &[String]) -> Option<String> {
    let app = front.app.trim().to_lowercase();
    if app.is_empty() {
        return None;
    }
    // Their own apps, by the exact name.
    if let Some(id) = picked.iter().find(|p| p.strip_prefix(CUSTOM).is_some_and(|name| name.to_lowercase() == app)) {
        return Some(id.clone());
    }
    let id = catalog_id(&app)?;
    picked.iter().any(|p| p == id).then(|| id.to_string())
}

/// A picked app's id is one the panel may send: from the catalogue, or
/// their own app's name (no separators, not too long).
pub(crate) fn valid_app_id(id: &str) -> bool {
    match id.strip_prefix(CUSTOM) {
        Some(name) => {
            let name = name.trim();
            !name.is_empty() && name.chars().count() <= 80 && !name.chars().any(|c| c.is_control() || c == '|')
        }
        None => CATALOG.iter().any(|(known, ..)| *known == id),
    }
}

/// An app whose window is named after the open document (design, video,
/// audio, 3D and documents): a blank name there means macOS is hiding it.
/// Others (Claude, Terminal, browsers, the person's own) may leave theirs
/// blank all the same.
pub(crate) fn names_documents(id: &str) -> bool {
    CATALOG.iter().any(|(known, _, kind, ..)| *known == id && matches!(*kind, "Design" | "Video" | "Audio" | "3D" | "Documents"))
}

/// A browser: only pages that name a project count.
pub(crate) fn is_browser(id: &str) -> bool {
    CATALOG.iter().any(|(known, _, kind, ..)| *known == id && *kind == "Browser")
}

/// What the panel calls a picked app.
pub(crate) fn app_label(id: &str) -> String {
    match id.strip_prefix(CUSTOM) {
        Some(name) => {
            let name = name.trim();
            name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name).to_string()
        }
        None => CATALOG.iter().find(|(known, ..)| *known == id).map(|(_, label, ..)| label.to_string()).unwrap_or_else(|| id.to_string()),
    }
}

/// What a moment of work points at.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Hit {
    /// A project (by id); `app` when it came from a window, `None` from a
    /// folder save.
    Project { project: String, app: Option<String> },
    /// A picked app's "Untitled" document: goes to that app's next project.
    Untitled(String),
    /// A picked app, on something no project was recognised in: asked
    /// about as "Worked in Photoshop … which project?".
    Unknown(String),
}

/// Words too common in project and client names to point at one.
const COMMON: &[&str] = &[
    "the", "and", "for", "with", "from", "new", "old", "final", "draft", "copy", "edit", "edits", "video", "videos", "content",
    "project", "projects", "design", "designs", "social", "socials", "media", "campaign", "event", "events", "launch", "music",
    "cover", "album", "film", "films", "photo", "photos", "shoot", "series", "season", "episode", "promo", "brand", "branding",
    "logo", "web", "website", "online", "print", "digital", "pty", "ltd", "inc", "studio", "studios", "group", "agency",
    "company", "intro", "sequence", "programme", "program", "partnership", "editorial", "version", "master", "export",
    "one", "two", "three", "view", "test", "day", "part",
];

/// The words of a name that can point at it: three letters or more, not
/// all digits, not too common.
fn distinctive(name: &str) -> Vec<String> {
    let mut words: Vec<String> = fold(name)
        .split(' ')
        .filter(|w| w.chars().count() >= 3 && !w.chars().all(|c| c.is_ascii_digit()) && !COMMON.contains(w))
        .map(str::to_string)
        .collect();
    words.sort();
    words.dedup();
    words
}

/// Matches a window title to one of the projects. First exactly: its number
/// ("CB-0412"), its name ("Logo refresh"), or its client's name when the
/// client has only that project; the longest match wins. Then loosely, by
/// the project's and client's own words: two of them, or one long word no
/// other project has ("klutch_promo_v3.psd" → "Motus x News24 … The Klutch
/// Programme"). A tie is no match: the person is asked instead. It always
/// asks before logging, so a near miss costs one tap.
pub(crate) fn match_title(title: &str, projects: &[crate::hourly::Project]) -> Option<String> {
    let title = fold(title);
    if title.is_empty() {
        return None;
    }
    // A project number is the surest sign: one in the title decides.
    let mut numbered = projects.iter().filter(|p| {
        let number = fold(&p.project_number);
        number.chars().count() >= 3 && contains_words(&title, &number)
    });
    if let (Some(only), None) = (numbered.next(), numbered.next()) {
        return Some(only.id.clone());
    }
    let mut best: Option<(usize, &str)> = None;
    // Two projects matching equally well (same name for two clients): ask.
    let mut tied = false;
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
            if needle.chars().count() < min || !contains_words(&title, &needle) {
                continue;
            }
            match best {
                Some((len, id)) if needle.len() == len && id != project.id => tied = true,
                Some((len, _)) if needle.len() <= len => {}
                _ => {
                    best = Some((needle.len(), project.id.as_str()));
                    tied = false;
                }
            }
        }
    }
    if let Some((_, id)) = best {
        return (!tied).then(|| id.to_string());
    }
    // Loosely, by words.
    let in_title: Vec<&str> = title.split(' ').collect();
    let words: Vec<Vec<String>> = projects
        .iter()
        .map(|p| {
            let mut words = distinctive(&p.name);
            words.extend(distinctive(p.client_name.as_deref().unwrap_or_default()));
            words.sort();
            words.dedup();
            words
        })
        .collect();
    let mut scores: Vec<(usize, usize)> = Vec::new();
    for (index, own) in words.iter().enumerate() {
        let found: Vec<&String> = own.iter().filter(|w| in_title.contains(&w.as_str())).collect();
        let unique_long = found.iter().any(|w| {
            w.chars().count() >= 5 && words.iter().enumerate().all(|(other, theirs)| other == index || !theirs.contains(w))
        });
        let score = if found.len() >= 2 { found.len() * 2 } else if unique_long { 1 } else { 0 };
        if score > 0 {
            scores.push((score, index));
        }
    }
    let top = scores.iter().map(|(score, _)| *score).max()?;
    let mut at_top = scores.iter().filter(|(score, _)| *score == top);
    let (_, index) = at_top.next()?;
    at_top.next().is_none().then(|| projects[*index].id.clone())
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

/// A stretch of work on one project (unix seconds). `project` is empty for
/// work in a picked app that no project was recognised in; `app` says which
/// (and is only set then).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stretch {
    pub project: String,
    pub app: Option<String>,
    pub start: u64,
    pub end: u64,
}

/// Something else seen for this long takes over the stretch.
pub(crate) const SWITCH_SECS: u64 = 2 * 60;
/// Samples this close together are one unbroken look.
const SEEN_GAP_SECS: u64 = 90;

/// Turns moments of work into stretches.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    current: Option<Stretch>,
    /// The latest stretch of work only seen on an untitled document, by app.
    untitled: Option<(String, u64, u64)>,
    /// The last moment `current` was seen through a folder save.
    by_save: bool,
    /// Something else seen meanwhile, not for long enough yet to take over:
    /// (project, app, first seen, last seen). A glance at another file
    /// doesn't split the stretch.
    other: Option<(String, Option<String>, u64, u64)>,
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
                    .take_if(|(u_app, _, last)| (app.is_none() || app.as_deref() == Some(u_app.as_str())) && now.saturating_sub(*last) <= GAP_SECS)
                    .map(|(_, start, _)| start);
                let from_save = app.is_none();
                // Work nothing was recognised in, and then a save in a folder
                // linked to a project: it was that project.
                if from_save {
                    // Only work in an app that names its window after the
                    // document (not Claude or a terminal before it).
                    if let Some(current) = self.current.as_mut().filter(|c| c.project.is_empty() && c.app.as_deref().is_some_and(names_documents)) {
                        current.project = project.clone();
                        current.app = None;
                    }
                }
                if let Some(done) = self.moment(now, project, None, from_save, untitled_start) {
                    ended = ended.or(Some(done));
                }
            }
            Some(Hit::Unknown(app)) => {
                if let Some(done) = self.moment(now, String::new(), Some(app), false, None) {
                    ended = ended.or(Some(done));
                }
            }
            Some(Hit::Untitled(app)) => {
                self.untitled = match self.untitled.take() {
                    Some((u_app, start, last)) if u_app == app && now.saturating_sub(last) <= GAP_SECS => Some((app, start, now)),
                    _ => Some((app, now, now)),
                };
            }
            None => {}
        }
        ended.filter(|s| s.end.saturating_sub(s.start) >= MIN_SECS)
    }

    /// A moment on `project` (or, when it's empty, unrecognised work in `app`).
    fn moment(&mut self, now: u64, project: String, app: Option<String>, from_save: bool, untitled_start: Option<u64>) -> Option<Stretch> {
        let app = if project.is_empty() { app } else { None };
        let Some(current) = self.current.as_mut() else {
            self.current = Some(Stretch { project, app, start: untitled_start.unwrap_or(now).min(now), end: now });
            self.by_save = from_save;
            self.other = None;
            return None;
        };
        if current.project == project && current.app == app {
            current.end = now;
            if let Some(start) = untitled_start {
                current.start = current.start.min(start);
            }
            self.by_save = from_save;
            self.other = None;
            return None;
        }
        let (start, switch) = if from_save {
            // Saves are minutes apart: one switches straight away, unless
            // the window shows the current project right now.
            let watching = !self.by_save && now.saturating_sub(current.end) <= SEEN_GAP_SECS;
            if watching {
                return None;
            }
            (untitled_start.unwrap_or(now).min(now), true)
        } else {
            let other = match self.other.take() {
                Some((p, a, first, last)) if p == project && a == app && now.saturating_sub(last) <= SEEN_GAP_SECS => (p, a, first, now),
                _ => (project.clone(), app.clone(), untitled_start.unwrap_or(now).min(now), now),
            };
            let long_enough = other.3.saturating_sub(other.2) >= SWITCH_SECS;
            let start = other.2;
            if !long_enough {
                self.other = Some(other);
            }
            (start, long_enough)
        };
        if !switch {
            return None;
        }
        let done = self.current.replace(Stretch { project, app, start, end: now });
        self.by_save = from_save;
        self.other = None;
        done
    }

    /// The current stretch, ended if it's been quiet too long.
    fn expire(&mut self, now: u64) -> Option<Stretch> {
        let gap = if self.by_save { SAVE_GAP_SECS } else { GAP_SECS };
        if self.current.as_ref().is_some_and(|c| now.saturating_sub(c.end) > gap) {
            self.other = None;
            return self.current.take();
        }
        None
    }

    /// End the current stretch now (a wrap, the timer started, switched off).
    pub(crate) fn close(&mut self) -> Option<Stretch> {
        self.untitled = None;
        self.other = None;
        self.current.take().filter(|s| s.end.saturating_sub(s.start) >= MIN_SECS)
    }

    /// The stretch being built, if any.
    pub(crate) fn current(&self) -> Option<&Stretch> {
        self.current.as_ref()
    }

    /// Take the stretch being built, however short ("Log it and start the
    /// timer").
    pub(crate) fn take(&mut self) -> Option<Stretch> {
        self.untitled = None;
        self.other = None;
        self.current.take()
    }
}

/// A suggestion waiting for an answer, as kept in `settings.json`:
/// "start|end|project|app" (the app only for unrecognised work). No names
/// of windows or files, and no paths.
pub(crate) fn encode(s: &Stretch) -> String {
    format!("{}|{}|{}|{}", s.start, s.end, s.project, s.app.as_deref().unwrap_or_default())
}

pub(crate) fn decode(raw: &str) -> Option<Stretch> {
    let mut parts = raw.splitn(4, '|');
    let start = parts.next()?.parse().ok()?;
    let end = parts.next()?.parse().ok()?;
    let project = parts.next()?.to_string();
    let app = parts.next().filter(|app| !app.is_empty()).map(str::to_string);
    let known = crate::hourly::valid_id(&project) && app.is_none();
    let unknown = project.is_empty() && app.as_deref().is_some_and(valid_app_id);
    (end > start && (known || unknown)).then_some(Stretch { project, app, start, end })
}

/// A suggestion's key on the tracker: "desktop:<start>".
pub(crate) fn key_of(id: u64) -> String {
    format!("desktop:{id}")
}

pub(crate) fn id_of(key: &str) -> Option<u64> {
    key.strip_prefix("desktop:")?.parse().ok()
}

/// The most suggestions handed to the tracker at once (as it takes).
const SYNC_MAX: usize = 50;

/// What the tracker keeps of a suggestion still waiting for an answer: its
/// key, its project when one was recognised, and its times. Never the app.
pub(crate) fn suggestion_body(s: &Stretch) -> Value {
    let mut body = json!({
        "key": key_of(s.start),
        "startedAt": crate::hourly::iso_utc(s.start),
        "endedAt": crate::hourly::iso_utc(s.end),
    });
    if !s.project.is_empty() && crate::hourly::valid_id(&s.project) {
        body["projectId"] = Value::String(s.project.clone());
    }
    body
}

/// The pending suggestions still worth asking about at `now`.
pub(crate) fn fresh(pending: &[String], now: u64) -> Vec<Stretch> {
    pending.iter().filter_map(|raw| decode(raw)).filter(|s| now.saturating_sub(s.end) <= KEEP_SECS).collect()
}

/// A folder linked to every project: the project is worked out from the
/// names of the folders the saved file is in ("Clients/Vimeo/Logo
/// refresh/…"). For a drive or folder full of client or project folders.
pub(crate) const ANY_PROJECT: &str = "*";

/// A linked folder, kept as "project\tpath" (`ANY_PROJECT` for one that
/// works the project out from its folders' names).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Linked {
    pub project: String,
    pub path: PathBuf,
}

pub(crate) fn decode_linked(raw: &str) -> Option<Linked> {
    let (project, path) = raw.split_once('\t')?;
    ((crate::hourly::valid_id(project) || project == ANY_PROJECT) && !path.is_empty()).then(|| Linked { project: project.into(), path: path.into() })
}

/// Files under `root` saved after `since` (at most `max`), looked at up to a
/// limit (hidden folders and the usual build/cache folders skipped).
pub(crate) fn saved_since(root: &Path, since: SystemTime, max: usize, max_entries: usize) -> Vec<PathBuf> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    let mut found = Vec::new();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        // Folders changed most recently are looked in first, so on a big
        // drive the limit still reaches where the work is.
        let mut folders: Vec<(SystemTime, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            seen += 1;
            if seen > max_entries {
                return found;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.')
                || matches!(name.as_ref(), "node_modules" | "Cache" | "cache" | "Media Cache Files" | "Adobe Premiere Pro Auto-Save" | "$RECYCLE.BIN" | "System Volume Information")
            {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                if depth < SCAN_MAX_DEPTH {
                    folders.push((meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), entry.path()));
                }
            } else if meta.modified().is_ok_and(|at| at > since) {
                found.push(entry.path());
                if found.len() >= max {
                    return found;
                }
            }
        }
        // The stack takes the newest last, so it's looked in next.
        folders.sort_by_key(|(at, _)| *at);
        stack.extend(folders.into_iter().map(|(_, path)| (path, depth + 1)));
    }
    found
}

/// The project a file saved under an "any project" folder belongs to, from
/// the names of the folders it's in and its own name.
pub(crate) fn project_from_path(root: &Path, file: &Path, projects: &[crate::hourly::Project]) -> Option<String> {
    let inside = file.strip_prefix(root).ok()?;
    let parts: Vec<String> = inside.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    // The folders it's in first, the deepest naming a project winning
    // ("Vimeo/Logo refresh" over "Vimeo"); the file's own name only when
    // no folder names one.
    let folders = parts.len().saturating_sub(1);
    (1..=folders).rev().find_map(|n| match_title(&parts[..n].join(" "), projects)).or_else(|| match_title(&parts.join(" "), projects))
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
                Some(Front { app: app_name, title, own: u32::try_from(pid).ok() == Some(std::process::id()) })
            }
        })
    }

    /// The names of the apps open now (ones with a Dock icon), for "Add
    /// another app…".
    pub(super) fn running_apps() -> Vec<String> {
        let Some(workspace_class) = AnyClass::get(c"NSWorkspace") else { return Vec::new() };
        objc2::rc::autoreleasepool(|_| {
            // SAFETY: AppKit queries; every object is checked for nil, the
            // pool releases the rest.
            unsafe {
                let workspace: *mut AnyObject = msg_send![workspace_class, sharedWorkspace];
                if workspace.is_null() {
                    return Vec::new();
                }
                let apps: *mut AnyObject = msg_send![workspace, runningApplications];
                if apps.is_null() {
                    return Vec::new();
                }
                let count: usize = msg_send![apps, count];
                let mut names = Vec::new();
                for index in 0..count {
                    let app: *mut AnyObject = msg_send![apps, objectAtIndex: index];
                    if app.is_null() {
                        continue;
                    }
                    // NSApplicationActivationPolicyRegular: a normal app.
                    let policy: isize = msg_send![app, activationPolicy];
                    if policy != 0 {
                        continue;
                    }
                    let name: *mut AnyObject = msg_send![app, localizedName];
                    let name = rust_string(name);
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
                names
            }
        })
    }

    /// Whether macOS lets the app read other apps' window titles.
    pub(super) fn titles_allowed() -> bool {
        // SAFETY: no arguments.
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub(super) fn open_settings() {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
            .spawn();
    }

    /// Forget this app's Screen Recording answer (`tccutil reset`), so the
    /// next ask is a fresh one for the build that's running.
    pub(super) fn reset_titles(bundle: &str) -> Result<(), String> {
        let done = std::process::Command::new("/usr/bin/tccutil").args(["reset", "ScreenCapture", bundle]).output();
        match done {
            Ok(out) if out.status.success() => Ok(()),
            _ => {
                open_settings();
                Err("macOS didn't let the app reset it. In System Settings, remove CoolerBox Tracker with −, add it again with +, switch it on, then restart.".into())
            }
        }
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
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };

    /// The program file behind a window ("photoshop.exe"), lowercase.
    unsafe fn program(window: HWND) -> String {
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return String::new();
        }
        let mut path = [0u16; 1024];
        let mut size = path.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut size);
        CloseHandle(process);
        let path = if ok != 0 { String::from_utf16_lossy(&path[..size as usize]) } else { String::new() };
        path.rsplit(['\\', '/']).next().unwrap_or_default().to_lowercase()
    }

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
            Some(Front { app: program(window), title, own: pid == std::process::id() })
        }
    }

    /// The programs with a visible, named window now, for "Add another app…".
    pub(super) fn running_apps() -> Vec<String> {
        unsafe extern "system" fn each(window: HWND, list: LPARAM) -> BOOL {
            // SAFETY: `list` is the Vec passed to EnumWindows below, alive
            // for the whole call.
            let list = unsafe { &mut *(list as *mut Vec<String>) };
            if unsafe { IsWindowVisible(window) } != 0 && unsafe { GetWindowTextLengthW(window) } > 0 {
                let name = unsafe { program(window) };
                if !name.is_empty() && !list.contains(&name) {
                    list.push(name);
                }
            }
            1
        }
        let mut list: Vec<String> = Vec::new();
        // SAFETY: the callback only touches `list`, which outlives the call.
        unsafe {
            EnumWindows(Some(each), &mut list as *mut Vec<String> as LPARAM);
        }
        list
    }

    pub(super) fn titles_allowed() -> bool {
        true
    }

    pub(super) fn ask_titles() {}

    pub(super) fn open_settings() {}

    pub(super) fn reset_titles(_bundle: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use super::Front;

    pub(super) fn front() -> Option<Front> {
        None
    }

    pub(super) fn running_apps() -> Vec<String> {
        Vec::new()
    }

    pub(super) fn titles_allowed() -> bool {
        false
    }

    pub(super) fn ask_titles() {}

    pub(super) fn open_settings() {}

    pub(super) fn reset_titles(_bundle: &str) -> Result<(), String> {
        Ok(())
    }
}

// ── state ──

/// What the last look saw, for "Right now" in Settings. Kept in memory
/// until the next look, never stored or sent.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    /// The project timer is running: nothing to suggest.
    Timer,
    /// Away from the keyboard and mouse.
    Away,
    /// The window in front.
    App { label: String, title: String, picked: bool, project: Option<String> },
}

#[derive(Default)]
struct Inner {
    tracker: Tracker,
    /// When each linked folder was last looked at.
    scanned: Option<SystemTime>,
    last_scan: u64,
    seen: Option<Seen>,
    /// Looks in a row at a picked app whose window name came back empty
    /// (macOS: the permission isn't working until the app restarts).
    blank: u32,
    /// "Not this" on what it was counting: that project (or unrecognised
    /// work in that app) isn't counted again until something else is seen.
    ignored: Option<(String, Option<String>)>,
    /// A stretch handed to "Start timer from …", until the timer is seen
    /// running. If it isn't within a couple of minutes (the tracker
    /// refused, or was offline), it comes back as a suggestion: not lost.
    held: Option<(Stretch, u64)>,
}

/// The project (or, for unrecognised work, the app) a moment points at, as
/// "Not this" remembers it. `None` for an untitled document.
fn hit_key(hit: &Hit) -> Option<(String, Option<String>)> {
    match hit {
        Hit::Project { project, .. } => Some((project.clone(), None)),
        Hit::Unknown(app) => Some((String::new(), Some(app.clone()))),
        Hit::Untitled(_) => None,
    }
}

static STATE: Mutex<Option<Inner>> = Mutex::new(None);

/// The permission to read window names was asked for in this run (macOS
/// only uses it after a restart).
static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn with_state<T>(run: impl FnOnce(&mut Inner) -> T) -> Option<T> {
    let mut slot = STATE.lock().ok()?;
    Some(run(slot.get_or_insert_with(Inner::default)))
}

pub(crate) fn is_on<R: Runtime>(app: &AppHandle<R>) -> bool {
    crate::close::flag(app, ON)
}

fn picked<R: Runtime>(app: &AppHandle<R>) -> Vec<String> {
    crate::close::strings(app, APPS).into_iter().filter(|id| valid_app_id(id)).collect()
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

fn ask_titles() {
    ASKED.store(true, std::sync::atomic::Ordering::Relaxed);
    platform::ask_titles();
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
    /// macOS: the permission needs the app to restart before it works.
    pub restart: bool,
    /// macOS, what the permission line says: "ask" (not given to this
    /// app), "restart" (asked for in this run: restart once it's on),
    /// "stuck" (given, but window names still come back blank), or none.
    pub permission: Option<&'static str>,
    /// "Right now": what it's seeing, and the stretch it's counting.
    pub now: Option<NowView>,
    /// The oldest suggestion waiting for an answer, and how many more.
    pub suggestion: Option<SuggestionView>,
    pub more: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AppChoice {
    pub id: String,
    pub label: String,
    /// "Design", "Video", "Audio", "3D", "Documents", or "Yours".
    pub kind: &'static str,
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FolderRow {
    /// The stored line, which unlinking names (not a position that can
    /// shift between the panel's render and the click).
    pub key: String,
    pub folder: String,
    pub project: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NowView {
    /// "timer", "away" or "app".
    pub kind: &'static str,
    pub app: String,
    /// The window's name, only on this computer.
    pub title: String,
    pub picked: bool,
    /// The project recognised in it.
    pub project: Option<String>,
    /// The stretch being counted: what (a project, or "something in
    /// Photoshop"), since when (unix ms), and whether it can be logged now.
    pub counting: Option<String>,
    pub since: Option<u64>,
    pub can_log: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SuggestionView {
    /// Its start (unix seconds): the answer names it.
    pub id: u64,
    /// Empty for work no project was recognised in.
    pub project_id: String,
    /// The project, or for unrecognised work the app ("Photoshop").
    pub label: String,
    pub unrecognised: bool,
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
        label: if s.project.is_empty() { s.app.as_deref().map(app_label).unwrap_or_default() } else { label_of(&s.project) },
        unrecognised: s.project.is_empty(),
        billable: info.projects.iter().any(|p| p.id == s.project && p.billable),
        from: s.start * 1000,
        to: s.end * 1000,
    });
    let mut apps: Vec<AppChoice> = CATALOG
        .iter()
        .map(|(id, label, kind, ..)| AppChoice { id: id.to_string(), label: label.to_string(), kind, on: picked.iter().any(|p| p == id) })
        .collect();
    apps.extend(picked.iter().filter(|id| id.starts_with(CUSTOM)).map(|id| AppChoice { id: id.clone(), label: app_label(id), kind: "Yours", on: true }));
    let titles_allowed = platform::titles_allowed();
    let (seen, building, blank) = if on {
        with_state(|state| (state.seen.clone(), state.tracker.current().cloned(), state.blank)).unwrap_or_default()
    } else {
        (None, None, 0)
    };
    let now = seen.map(|seen| {
        let (counting, since, can_log) = match &building {
            Some(b) if b.project.is_empty() => (
                Some(format!("something in {}", b.app.as_deref().map(app_label).unwrap_or_default())),
                Some(b.start * 1000),
                crate::clock::unix_now().saturating_sub(b.start) >= 60,
            ),
            Some(b) => (Some(label_of(&b.project)), Some(b.start * 1000), crate::clock::unix_now().saturating_sub(b.start) >= 60),
            None => (None, None, false),
        };
        match seen {
            Seen::Timer => NowView { kind: "timer", app: String::new(), title: String::new(), picked: false, project: None, counting: None, since: None, can_log: false },
            Seen::Away => NowView { kind: "away", app: String::new(), title: String::new(), picked: false, project: None, counting, since, can_log },
            Seen::App { label, title, picked, project } => {
                NowView { kind: "app", app: label, title, picked, project: project.as_deref().map(label_of), counting, since, can_log }
            }
        }
    });
    View {
        on,
        apps,
        folders: linked(app)
            .iter()
            .map(|l| FolderRow {
                key: format!("{}\t{}", l.project, l.path.display()),
                folder: short_path(&l.path),
                project: if l.project == ANY_PROJECT { "Any project, from its folders' names".into() } else { label_of(&l.project) },
            })
            .collect(),
        titles_allowed,
        titles_supported: cfg!(any(target_os = "macos", windows)),
        restart: cfg!(target_os = "macos") && on && ((ASKED.load(std::sync::atomic::Ordering::Relaxed) && !titles_allowed) || blank >= 4),
        permission: permission_state(
            cfg!(target_os = "macos") && on && picked.iter().any(|id| !is_browser(id)),
            titles_allowed,
            ASKED.load(std::sync::atomic::Ordering::Relaxed),
            blank,
        ),
        now,
        suggestion,
        more: waiting.len().saturating_sub(1),
    }
}

/// What the panel says about macOS's permission to read window names.
/// macOS ties it to the app's signature: a test build, or an update of one,
/// can show it switched on in System Settings while it doesn't apply, and
/// switching it on again changes nothing. "stuck" says how to get past that.
pub(crate) fn permission_state(needed: bool, allowed: bool, asked: bool, blank: u32) -> Option<&'static str> {
    if !needed {
        return None;
    }
    match (allowed, asked) {
        (false, true) => Some("restart"),
        (false, false) => Some("ask"),
        (true, _) if blank >= 4 => Some("stuck"),
        (true, _) => None,
    }
}

/// A stretch ended: keep it to ask about, and say so once.
fn suggest<R: Runtime>(app: &AppHandle<R>, stretch: Stretch) {
    let mut list = pending(app);
    if list.iter().any(|s| s.start == stretch.start) {
        return;
    }
    list.push(stretch.clone());
    set_pending(app, &list);
    crate::mini::push(app);
    let handle = app.clone();
    std::thread::spawn(move || sync(&handle));
    if crate::quiet::allowed(app, crate::quiet::Kind::Tracker) {
        let times = format!("{}–{}", crate::clock::fmt_hhmm_sast(stretch.start), crate::clock::fmt_hhmm_sast(stretch.end));
        let (title, body) = if stretch.project.is_empty() {
            let app_name = stretch.app.as_deref().map(app_label).unwrap_or_default();
            ("Worked on something?", format!("{app_name} {times}. Which project was it?"))
        } else {
            let label = crate::hourly::info_snapshot(app)
                .and_then(|info| info.projects.iter().find(|p| p.id == stretch.project).map(crate::hourly::project_label))
                .unwrap_or_default();
            ("Worked on this?", format!("{label} {times}. Log it?"))
        };
        crate::banner::show(app, title, &body, crate::banner::Target::Panel);
    }
}

/// End the stretch being built now (a wrap on the time sheet).
pub(crate) fn close_now<R: Runtime>(app: &AppHandle<R>) {
    if let Some(stretch) = with_state(|state| state.tracker.close()).flatten() {
        suggest(app, stretch);
    }
}

/// Note what this look saw; the panel hears of a change.
fn set_seen<R: Runtime>(app: &AppHandle<R>, seen: Option<Seen>) {
    let changed = with_state(|state| {
        let before = (state.seen.clone(), state.tracker.current().map(|c| (c.project.clone(), c.app.clone(), c.start)));
        state.seen = seen;
        before != (state.seen.clone(), state.tracker.current().map(|c| (c.project.clone(), c.app.clone(), c.start)))
    });
    if changed == Some(true) {
        crate::mini::push(app);
    }
}

/// The name to show for an app that isn't picked ("Safari", "chrome").
fn plain_name(app: &str) -> String {
    let app = app.trim();
    app.strip_suffix(".exe").unwrap_or(app).to_string()
}

/// One look: the front window and, every minute, the linked folders.
fn tick<R: Runtime>(app: &AppHandle<R>, now: u64) {
    if !is_on(app) || !crate::session::signed_in() {
        with_state(|state| *state = Inner::default());
        return;
    }
    let Some(info) = crate::hourly::info_snapshot(app) else { return };
    // "Start timer from …" worked (the timer runs): the held stretch is
    // logged. It didn't: offer it again.
    let unheld = with_state(|state| {
        let done = info.running.is_some() || state.held.as_ref().is_some_and(|(_, at)| now.saturating_sub(*at) > 120);
        if done { state.held.take().filter(|_| info.running.is_none()).map(|(s, _)| s) } else { None }
    })
    .flatten();
    if let Some(stretch) = unheld {
        suggest(app, stretch);
    }
    // Timing already: nothing to suggest; what was being built ends here.
    if info.running.is_some() {
        close_now(app);
        set_seen(app, Some(Seen::Timer));
        return;
    }
    if crate::idle::idle_seconds().is_some_and(|idle| idle >= IDLE_SECS) {
        if let Some(stretch) = with_state(|state| state.tracker.step(now, None)).flatten() {
            suggest(app, stretch);
        }
        set_seen(app, Some(Seen::Away));
        return;
    }
    let picked = picked(app);
    let mut hits: Vec<Hit> = Vec::new();
    let mut seen = None;
    let front = platform::front();
    // The quick panel itself in front: leave what was seen before as it was.
    let own = front.as_ref().is_some_and(|f| f.own);
    if let Some(front) = front.filter(|f| !f.own) {
        let id = app_id(&front, &picked);
        let mut project = None;
        // A blank name only counts against the permission in an app that
        // names its window after the document.
        if id.as_deref().is_some_and(names_documents) {
            let blank = front.title.trim().is_empty();
            with_state(|state| state.blank = if blank { state.blank.saturating_add(1) } else { 0 });
        }
        if let Some(id) = &id {
            let hit = if is_untitled(&front.title) {
                Some(Hit::Untitled(id.clone()))
            } else {
                match match_title(&front.title, &info.projects) {
                    Some(found) => {
                        project = Some(found.clone());
                        Some(Hit::Project { project: found, app: Some(id.clone()) })
                    }
                    // Not the rest of the web.
                    None if is_browser(id) => None,
                    // Work in a picked app all the same (even when its
                    // window's name can't be read): asked about later.
                    None => Some(Hit::Unknown(id.clone())),
                }
            };
            hits.extend(hit);
        }
        // The window's name is only kept until the next look, for "Right
        // now" on this computer: never stored or sent.
        let mut title = front.title.trim().to_string();
        if title.chars().count() > 120 {
            title = title.chars().take(119).chain(['…']).collect();
        }
        let label = id.as_deref().map(app_label).unwrap_or_else(|| plain_name(&front.app));
        seen = Some(Seen::App { label, title, picked: id.is_some(), project });
    }
    let scan_due = with_state(|state| now.saturating_sub(state.last_scan) >= SCAN_EVERY_SECS).unwrap_or(false);
    if scan_due {
        let since = with_state(|state| state.scanned).flatten().unwrap_or_else(SystemTime::now);
        let scanned_at = SystemTime::now();
        for folder in linked(app) {
            if folder.project == ANY_PROJECT {
                for file in saved_since(&folder.path, since, 20, SCAN_MAX_ENTRIES * 4) {
                    if let Some(project) = project_from_path(&folder.path, &file, &info.projects) {
                        if !hits.iter().any(|h| matches!(h, Hit::Project { project: p, app: None } if *p == project)) {
                            hits.push(Hit::Project { project, app: None });
                        }
                    }
                }
            } else if info.projects.iter().any(|p| p.id == folder.project) && !saved_since(&folder.path, since, 1, SCAN_MAX_ENTRIES).is_empty() {
                hits.push(Hit::Project { project: folder.project.clone(), app: None });
            }
        }
        with_state(|state| {
            state.scanned = Some(scanned_at);
            state.last_scan = now;
        });
    }
    if hits.is_empty() {
        if let Some(stretch) = with_state(|state| state.tracker.step(now, None)).flatten() {
            suggest(app, stretch);
        }
    }
    for hit in hits {
        // "Not this": skip it until something else turns up.
        let skip = with_state(|state| {
            let key = hit_key(&hit);
            match (&state.ignored, key) {
                (Some(ignored), Some(key)) if *ignored == key => true,
                (Some(_), Some(_)) => {
                    state.ignored = None;
                    false
                }
                _ => false,
            }
        })
        .unwrap_or(false);
        if skip {
            continue;
        }
        if let Some(stretch) = with_state(|state| state.tracker.step(now, Some(hit))).flatten() {
            suggest(app, stretch);
        }
    }
    if !own {
        set_seen(app, seen);
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

/// Pick or unpick an app: from the catalogue, or their own ("custom:…").
/// Picking one on macOS asks for the permission to read window names, if it
/// isn't given yet.
#[tauri::command]
pub(crate) fn mini_detector_app<R: Runtime>(app: AppHandle<R>, id: String) -> Result<(), String> {
    if !valid_app_id(&id) {
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
    if on && cfg!(any(target_os = "macos", windows)) && !platform::titles_allowed() {
        ask_titles();
    }
    crate::mini::push(&app);
    Ok(())
}

/// An app that's open now, for "Add another app…".
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OpenApp {
    pub id: String,
    pub label: String,
}

/// The apps open now that aren't picked yet: a catalogue app by its id, any
/// other by its own name.
pub(crate) fn open_apps(names: &[String], picked: &[String]) -> Vec<OpenApp> {
    const SKIP: &[&str] = &["finder", "explorer.exe", "system settings", "systemsettings.exe", "applicationframehost.exe", "textinputhost.exe"];
    let mut apps: Vec<OpenApp> = Vec::new();
    for name in names {
        let lower = name.trim().to_lowercase();
        if lower.is_empty() || lower.contains("coolerbox") || lower.contains("cbb-tracker") || SKIP.contains(&lower.as_str()) {
            continue;
        }
        let id = catalog_id(name).map(str::to_string).unwrap_or_else(|| format!("{CUSTOM}{}", name.trim()));
        if !valid_app_id(&id) || picked.contains(&id) || apps.iter().any(|a| a.id == id) {
            continue;
        }
        apps.push(OpenApp { label: app_label(&id), id });
    }
    apps.sort_by_key(|a| a.label.to_lowercase());
    apps
}

#[tauri::command]
pub(crate) fn mini_detector_open_apps<R: Runtime>(app: AppHandle<R>) -> Result<Vec<OpenApp>, String> {
    Ok(open_apps(&platform::running_apps(), &picked(&app)))
}

/// Ask again for the permission to read window titles (macOS).
#[tauri::command]
pub(crate) fn mini_detector_permission<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    ask_titles();
    crate::mini::push(&app);
    Ok(())
}

/// Open System Settings at Screen & System Audio Recording (macOS).
#[tauri::command]
pub(crate) fn mini_detector_open_settings() -> Result<(), String> {
    platform::open_settings();
    Ok(())
}

/// "Ask macOS again": forget this app's Screen Recording answer, so macOS
/// asks afresh for the build that's running (macOS only, on a tap).
#[tauri::command]
pub(crate) fn mini_detector_reset_permission<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    platform::reset_titles(&app.config().identifier)?;
    ask_titles();
    with_state(|state| state.blank = 0);
    crate::mini::push(&app);
    Ok(())
}

/// macOS uses the permission only after the app restarts.
#[tauri::command]
pub(crate) fn mini_detector_restart<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    app.restart();
}

/// "Start the timer from 10:45": the stretch being counted, up to now, then
/// the timer on its project (or on `project_id`, picked for work no project
/// was recognised in).
#[tauri::command]
pub(crate) fn mini_detector_log_now<R: Runtime>(app: AppHandle<R>, project_id: Option<String>) -> Result<(), String> {
    let stretch = with_state(|state| state.tracker.current().cloned()).flatten().ok_or("Nothing to log yet.")?;
    let project = project_id.filter(|p| !p.is_empty()).unwrap_or_else(|| stretch.project.clone());
    if project.is_empty() {
        return Err("Pick the project it's for.".into());
    }
    let end = crate::clock::unix_now();
    if end < stretch.start + 60 {
        return Err("Give it a minute first.".into());
    }
    crate::hourly::log_and_start(&app, &project, stretch.start, end)?;
    // Held until the timer is seen running (see `tick`).
    let now = crate::clock::unix_now();
    with_state(|state| {
        if let Some(taken) = state.tracker.take() {
            state.held = Some((Stretch { project: project.clone(), app: None, end, ..taken }, now));
        }
    });
    crate::mini::push(&app);
    Ok(())
}

/// "Not this" on what it's counting: forget the stretch, and don't count
/// the same project (or app) again until something else is in front.
#[tauri::command]
pub(crate) fn mini_detector_drop_now<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    with_state(|state| {
        if let Some(stretch) = state.tracker.take() {
            state.ignored = Some((stretch.project.clone(), if stretch.project.is_empty() { stretch.app } else { None }));
        }
    });
    crate::mini::push(&app);
    Ok(())
}

/// The folders being linked, between choosing them and choosing their
/// project.
static LINKING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The folder chooser is open: the panel stays open under it (lib.rs).
static CHOOSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn choosing_folder() -> bool {
    CHOOSING.load(std::sync::atomic::Ordering::Relaxed)
}

/// "Link folders…": choose one or more (on any drive), then the panel asks
/// for their project.
#[tauri::command]
pub(crate) async fn mini_detector_link<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    use tauri::Manager;
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app.dialog().file().set_title("Link folders to a project");
    // On the panel, so it opens in front of it and the panel stays.
    if let Some(mini) = app.get_webview_window("mini") {
        dialog = dialog.set_parent(&mini);
    }
    CHOOSING.store(true, std::sync::atomic::Ordering::Relaxed);
    let chosen = tauri::async_runtime::spawn_blocking(move || dialog.blocking_pick_folders()).await;
    CHOOSING.store(false, std::sync::atomic::Ordering::Relaxed);
    let Some(chosen) = chosen.map_err(|e| e.to_string())? else { return Ok(()) };
    let paths: Vec<PathBuf> = chosen.into_iter().filter_map(|p| p.into_path().ok()).collect();
    if paths.is_empty() {
        return Ok(());
    }
    let folders = match paths.as_slice() {
        [one] => short_path(one),
        many => format!("{} folders: {}", many.len(), many.iter().map(|p| short_path(p)).collect::<Vec<_>>().join(", ")),
    };
    if let Ok(mut slot) = LINKING.lock() {
        *slot = paths;
    }
    crate::mini::ask_hourly(&app, "__cbbMiniDetectorLink", &serde_json::json!({ "folder": folders }));
    Ok(())
}

/// The project for the folders just chosen, or `ANY_PROJECT` to work it out
/// from the names of the folders inside.
#[tauri::command]
pub(crate) fn mini_detector_link_project<R: Runtime>(app: AppHandle<R>, project_id: String) -> Result<(), String> {
    let info = crate::hourly::info_snapshot(&app).ok_or("The project timer isn't on for you.")?;
    if project_id != ANY_PROJECT && !info.projects.iter().any(|p| p.id == project_id) {
        return Err("That project isn't one of yours.".into());
    }
    let paths = LINKING.lock().ok().map(|mut slot| std::mem::take(&mut *slot)).unwrap_or_default();
    if paths.is_empty() {
        return Err("Choose the folders again.".into());
    }
    let mut list = crate::close::strings(&app, FOLDERS);
    for path in paths {
        let line = format!("{project_id}\t{}", path.display());
        if !list.contains(&line) {
            list.push(line);
        }
    }
    crate::close::set_strings(&app, FOLDERS, &list);
    crate::mini::push(&app);
    Ok(())
}

#[tauri::command]
pub(crate) fn mini_detector_unlink<R: Runtime>(app: AppHandle<R>, key: String) -> Result<(), String> {
    let mut list = crate::close::strings(&app, FOLDERS);
    list.retain(|line| *line != key);
    crate::close::set_strings(&app, FOLDERS, &list);
    crate::mini::push(&app);
    Ok(())
}

/// The answer to "Worked on X 09:10–10:40, log it?" (or "Worked in
/// Photoshop … which project?"): `log` (on its own project, or `project_id`
/// when they chose one) or `skip`.
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
            let project = project_id.filter(|p| !p.is_empty()).unwrap_or_else(|| stretch.project.clone());
            if project.is_empty() {
                return Err("Pick the project it was for.".into());
            }
            crate::hourly::log_stretch(&app, &project, stretch.start, stretch.end, id)?;
        }
        "skip" => {
            answered(&app, id);
            // "Not this" on My Hours too.
            let handle = app.clone();
            std::thread::spawn(move || {
                crate::hourly::sync_suggestions(&handle, json!({ "skip": [key_of(id)] }));
            });
        }
        _ => return Err("Unknown answer.".into()),
    }
    Ok(())
}

/// Hands the tracker the suggestions still waiting here, so they can be
/// answered on My Hours too, and forgets any already answered there (or on
/// another computer). Each poll (hourly.rs) and once one is kept; blocking,
/// so off the main thread. Sending the same ones again changes nothing.
pub(crate) fn sync<R: Runtime>(app: &AppHandle<R>) {
    if !is_on(app) {
        return;
    }
    let list = pending(app);
    if list.is_empty() {
        return;
    }
    let body = json!({ "suggestions": list.iter().take(SYNC_MAX).map(suggestion_body).collect::<Vec<_>>() });
    let done: Vec<u64> = crate::hourly::sync_suggestions(app, body).iter().filter_map(|key| id_of(key)).collect();
    if done.is_empty() {
        return;
    }
    let keep: Vec<Stretch> = pending(app).into_iter().filter(|s| !done.contains(&s.start)).collect();
    set_pending(app, &keep);
    crate::mini::push(app);
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
        let mac = Front { app: "Adobe Photoshop 2026".into(), title: String::new(), own: false };
        let win = Front { app: "photoshop.exe".into(), title: String::new(), own: false };
        let other = Front { app: "Adobe Illustrator 2026".into(), title: String::new(), own: false };
        assert_eq!(app_id(&mac, &picked).as_deref(), Some("photoshop"));
        assert_eq!(app_id(&win, &picked).as_deref(), Some("photoshop"));
        assert_eq!(app_id(&other, &picked), None, "not picked");
        assert_eq!(app_id(&Front { app: "Safari".into(), title: String::new(), own: false }, &picked), None);
        // The closest catalogue name: Lightroom Classic isn't Lightroom.
        assert_eq!(catalog_id("Adobe Lightroom Classic"), Some("lightroom-classic"));
        assert_eq!(catalog_id("Adobe Lightroom"), Some("lightroom"));
        assert_eq!(catalog_id("Pro Tools"), Some("protools"));
        assert_eq!(catalog_id("WINWORD.EXE"), Some("word"));
        assert_eq!(catalog_id("Code"), Some("vscode"));
        assert_eq!(catalog_id("iTerm2"), Some("terminal"));
        assert_eq!(catalog_id("msedge.exe"), Some("edge"));
        assert!(is_browser("chrome") && !is_browser("cursor"));
        let lr = vec!["lightroom".to_string()];
        assert_eq!(app_id(&Front { app: "Adobe Lightroom Classic".into(), title: String::new(), own: false }, &lr), None);
    }

    #[test]
    fn any_app_can_be_picked_by_its_own_name() {
        let picked = vec!["custom:Studio One".to_string(), "custom:bitwig studio.exe".to_string()];
        assert_eq!(app_id(&Front { app: "Studio One".into(), title: String::new(), own: false }, &picked).as_deref(), Some("custom:Studio One"));
        assert_eq!(
            app_id(&Front { app: "bitwig studio.exe".into(), title: String::new(), own: false }, &picked).as_deref(),
            Some("custom:bitwig studio.exe")
        );
        assert_eq!(app_id(&Front { app: "Studio One Pro".into(), title: String::new(), own: false }, &picked), None, "exact names only");
        assert!(valid_app_id("custom:Studio One"));
        assert!(valid_app_id("photoshop"));
        assert!(!valid_app_id("custom:"));
        assert!(!valid_app_id("custom:a|b"));
        assert!(!valid_app_id("notepad"));
        assert_eq!(app_label("custom:bitwig studio.exe"), "bitwig studio");
        assert_eq!(app_label("protools"), "Pro Tools");
        let mut ids: Vec<&str> = CATALOG.iter().map(|(id, ..)| *id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len(), "no repeated ids");
        // "Add another app…": open apps not picked yet, catalogue ones by id.
        let open = open_apps(
            &["Finder".into(), "Pro Tools".into(), "Studio One".into(), "Bitwig Studio".into(), "CoolerBox Tracker".into(), "Pro Tools".into()],
            &["custom:Studio One".to_string()],
        );
        assert_eq!(
            open,
            vec![
                OpenApp { id: "custom:Bitwig Studio".into(), label: "Bitwig Studio".into() },
                OpenApp { id: "protools".into(), label: "Pro Tools".into() },
            ]
        );
    }

    #[test]
    fn the_permission_line_says_what_to_do() {
        assert_eq!(permission_state(false, false, false, 9), None, "nothing picked that needs it");
        assert_eq!(permission_state(true, false, false, 0), Some("ask"));
        assert_eq!(permission_state(true, false, true, 0), Some("restart"));
        assert_eq!(permission_state(true, true, false, 3), None);
        assert_eq!(permission_state(true, true, false, 4), Some("stuck"), "on in System Settings, still blank");
        assert!(names_documents("photoshop") && names_documents("word"));
        assert!(!names_documents("claude") && !names_documents("chrome") && !names_documents("custom:Studio One"));
    }

    #[test]
    fn titles_match_loosely_by_the_projects_own_words() {
        let ps = vec![
            project("k", "CB-1", "Motus x News24 Editorial Partnership - The Klutch Programme", Some("POP24")),
            project("se", "CB-2", "SE October Content", Some("Stiebel Eltron")),
            project("se2", "CB-3", "SE November Content", Some("Stiebel Eltron")),
            project("cv", "CB-4", "Cooking with a view Intro sequence", Some("One 22")),
        ];
        assert_eq!(match_title("klutch_promo_v3.psd @ 50% (RGB/8)", &ps).as_deref(), Some("k"));
        assert_eq!(match_title("stiebel_eltron_october.psd", &ps).as_deref(), Some("se"));
        assert_eq!(match_title("cooking_titles.aep", &ps).as_deref(), Some("cv"));
        // Two projects share the client's words: a tie asks instead.
        assert_eq!(match_title("stiebel_eltron_banner.ai", &ps), None);
        assert_eq!(match_title("SE_Oct_banner.ai", &ps), None);
        // Common words point at nothing.
        assert_eq!(match_title("final content video.mp4", &ps), None);
        // The same name for two clients: ask rather than guess.
        let twins = vec![project("a", "CB-7", "Logo refresh", Some("Vimeo")), project("b", "CB-8", "Logo refresh", Some("Spotify"))];
        assert_eq!(match_title("Logo refresh v2.psd", &twins), None);
        assert_eq!(match_title("CB-8 Logo refresh v2.psd", &twins).as_deref(), Some("b"));
        // Short catalogue names don't claim other apps.
        assert_eq!(catalog_id("ArchiCAD 27"), None);
        assert_eq!(catalog_id("archicad.exe"), None);
        assert_eq!(catalog_id("codeblocks.exe"), None);
        assert_eq!(catalog_id("Arc"), Some("arc"));
        assert_eq!(catalog_id("code.exe"), Some("vscode"));
        // The tester's real file.
        let pp = vec![project("pp", "CB-5", "Pieter Pruis Cover - Mooi maar gevaarlik", Some("CoolerBox Records"))];
        assert_eq!(match_title("PIETER PRUIS COVER - MOOI MAAR GEVAARLIK.psd @ 25% (RGB/8)", &pp).as_deref(), Some("pp"));
    }

    const T0: u64 = 1_791_788_400;

    fn on(project: &str) -> Option<Hit> {
        Some(Hit::Project { project: project.into(), app: Some("photoshop".into()) })
    }

    fn known(project: &str, start: u64, end: u64) -> Stretch {
        Stretch { project: project.into(), app: None, start, end }
    }

    #[test]
    fn stretches_end_on_a_switch_or_a_pause_and_short_ones_are_dropped() {
        let mut t = Tracker::default();
        for minute in 0..=40 {
            assert_eq!(t.step(T0 + minute * 60, on("p1")), None);
        }
        // Two minutes on p2 ends p1's 40 minutes; p2 began when it was first seen.
        assert_eq!(t.step(T0 + 41 * 60, on("p2")), None);
        assert_eq!(t.step(T0 + 42 * 60, on("p2")), None);
        assert_eq!(t.step(T0 + 43 * 60, on("p2")), Some(known("p1", T0, T0 + 40 * 60)));
        // Five minutes on p2, then a long pause: too short to ask about.
        for minute in 44..=46 {
            t.step(T0 + minute * 60, on("p2"));
        }
        assert_eq!(t.step(T0 + 60 * 60, None), None);
        // A short look elsewhere doesn't break a stretch.
        let mut t = Tracker::default();
        for minute in 0..=10 {
            t.step(T0 + minute * 60, on("p1"));
        }
        t.step(T0 + 13 * 60, None);
        t.step(T0 + 14 * 60, on("p1"));
        assert_eq!(t.close(), Some(known("p1", T0, T0 + 14 * 60)));
        // Nor does a glance at another file.
        let mut t = Tracker::default();
        for minute in 0..=10 {
            t.step(T0 + minute * 60, on("p1"));
        }
        t.step(T0 + 11 * 60, Some(Hit::Unknown("photoshop".into())));
        t.step(T0 + 12 * 60, on("p1"));
        assert_eq!(t.close(), Some(known("p1", T0, T0 + 12 * 60)));
    }

    #[test]
    fn work_nothing_was_recognised_in_is_asked_about_too() {
        let unknown = || Some(Hit::Unknown("photoshop".into()));
        let mut t = Tracker::default();
        for minute in 0..=30 {
            assert_eq!(t.step(T0 + minute * 60, unknown()), None);
        }
        assert_eq!(
            t.step(T0 + 40 * 60, None),
            Some(Stretch { project: String::new(), app: Some("photoshop".into()), start: T0, end: T0 + 30 * 60 })
        );
        // Then a save in a folder linked to a project: it was that project.
        let mut t = Tracker::default();
        for minute in 0..=15 {
            t.step(T0 + minute * 60, unknown());
        }
        t.step(T0 + 16 * 60, Some(Hit::Project { project: "p4".into(), app: None }));
        assert_eq!(t.close(), Some(known("p4", T0, T0 + 16 * 60)));
        // Not work in an app that doesn't name documents (Claude, a terminal).
        let mut t = Tracker::default();
        for minute in 0..=15 {
            t.step(T0 + minute * 60, Some(Hit::Unknown("claude".into())));
        }
        t.step(T0 + 16 * 60, Some(Hit::Project { project: "p4".into(), app: None }));
        let done = t.close().expect("the Claude stretch");
        assert_eq!((done.project.as_str(), done.app.as_deref()), ("", Some("claude")));
        // Another picked app is another stretch.
        let mut t = Tracker::default();
        for minute in 0..=12 {
            t.step(T0 + minute * 60, unknown());
        }
        let audition = || Some(Hit::Unknown("audition".into()));
        assert_eq!(t.step(T0 + 13 * 60, audition()), None);
        assert_eq!(t.step(T0 + 14 * 60, audition()), None);
        let done = t.step(T0 + 15 * 60, audition()).expect("photoshop's stretch");
        assert_eq!((done.app.as_deref(), done.end), (Some("photoshop"), T0 + 12 * 60));
    }

    #[test]
    fn untitled_time_goes_to_the_project_its_app_shows_next() {
        let mut t = Tracker::default();
        for minute in 0..20 {
            t.step(T0 + minute * 60, Some(Hit::Untitled("photoshop".into())));
        }
        // Saved as "Logo refresh.psd".
        for minute in 20..=30 {
            t.step(T0 + minute * 60, on("p1"));
        }
        assert_eq!(t.close(), Some(known("p1", T0, T0 + 30 * 60)));
        // Another app's next project doesn't take it.
        let mut t = Tracker::default();
        t.step(T0, Some(Hit::Untitled("illustrator".into())));
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
        assert_eq!(t.step(T0 + 60 * 60, None), Some(known("p4", T0, T0 + 30 * 60)));
    }

    #[test]
    fn the_tracker_gets_only_a_suggestions_key_project_and_times() {
        let known = Stretch { project: "p1".into(), app: None, start: 1_760_000_000, end: 1_760_005_400 };
        let body = suggestion_body(&known);
        assert_eq!(body, json!({
            "key": "desktop:1760000000", "projectId": "p1",
            "startedAt": "2025-10-09T08:53:20.000Z", "endedAt": "2025-10-09T10:23:20.000Z",
        }));
        // Work no project was recognised in: no project, and never the app.
        let unknown = Stretch { project: String::new(), app: Some("photoshop".into()), start: 1_760_000_000, end: 1_760_005_400 };
        let body = suggestion_body(&unknown);
        assert!(body.get("projectId").is_none());
        assert!(!body.to_string().to_lowercase().contains("photoshop"));
        assert_eq!(id_of(&key_of(42)), Some(42));
        assert_eq!(id_of("work:42"), None);
        assert_eq!(id_of("desktop:x"), None);
    }

    #[test]
    fn suggestions_keep_only_the_project_and_times_for_a_week() {
        let s = known("p1", T0, T0 + 3600);
        let raw = encode(&s);
        assert_eq!(raw, format!("{T0}|{}|p1|", T0 + 3600));
        assert_eq!(decode(&raw), Some(s.clone()));
        // Kept by an earlier version: still read.
        assert_eq!(decode(&format!("{T0}|{}|p1", T0 + 3600)), Some(s.clone()));
        // Unrecognised work keeps the app, never the window's name.
        let u = Stretch { project: String::new(), app: Some("custom:Studio One".into()), start: T0, end: T0 + 900 };
        assert_eq!(encode(&u), format!("{T0}|{}||custom:Studio One", T0 + 900));
        assert_eq!(decode(&encode(&u)), Some(u));
        assert_eq!(decode(&format!("{T0}|{}||", T0 + 900)), None, "neither a project nor an app");
        assert_eq!(decode(&format!("{T0}|{}||notepad", T0 + 900)), None);
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
    fn a_folder_of_projects_works_out_the_project_from_its_folders() {
        let ps = projects();
        let root = Path::new("/Volumes/Work 2/Clients");
        assert_eq!(project_from_path(root, &root.join("Vimeo/Logo refresh/Exports/v3.psd"), &ps).as_deref(), Some("p1"));
        assert_eq!(project_from_path(root, &root.join("First Bank/report_layout.indd"), &ps).as_deref(), Some("p4"));
        assert_eq!(project_from_path(root, &root.join("Spirit Films/Brand film cutdowns/15s.prproj"), &ps).as_deref(), Some("p3"));
        assert_eq!(project_from_path(root, &root.join("Misc/notes.txt"), &ps), None);
        // The folder it's saved in beats a file named after another project.
        assert_eq!(project_from_path(root, &root.join("Spirit Films/Brand film/Brand film cutdowns ref.mp4"), &ps).as_deref(), Some("p2"));
        // A file's own name only when no folder names a project.
        assert_eq!(project_from_path(root, &root.join("Inbox/CB-0412 notes.txt"), &ps).as_deref(), Some("p1"));
        assert_eq!(project_from_path(root, Path::new("/elsewhere/Vimeo/x.ai"), &ps), None);
        assert!(decode_linked("*\t/Volumes/Work 2/Clients").is_some());
    }

    #[test]
    fn a_save_in_a_linked_folder_is_noticed() {
        let dir = std::env::temp_dir().join(format!("cbb-detector-{}", std::process::id()));
        let nested = dir.join("Exports/v2");
        std::fs::create_dir_all(&nested).unwrap();
        let before = SystemTime::now() - Duration::from_secs(5);
        assert!(saved_since(&dir, SystemTime::now() + Duration::from_secs(60), 1, SCAN_MAX_ENTRIES).is_empty());
        std::fs::write(nested.join("logo.ai"), b"x").unwrap();
        assert_eq!(saved_since(&dir, before, 5, SCAN_MAX_ENTRIES), vec![nested.join("logo.ai")]);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
