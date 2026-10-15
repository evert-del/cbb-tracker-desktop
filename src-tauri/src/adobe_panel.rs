//! The tracker's review panel for Adobe apps (github.com/evert-del/cbb-tracker-adobe),
//! installed and kept up to date by this app.
//!
//! The tracker publishes the panel's packages at
//! `/download/adobe-panel/current.json` (one `.ccx` per UXP app: Premiere
//! Pro, Photoshop, InDesign; one `.zxp` for Illustrator and After Effects).
//! Installing one is Adobe's own job: its Unified Plugin Installer Agent,
//! which Creative Cloud Desktop puts on every Mac and Windows computer with
//! Adobe apps. This app only:
//!
//! - asks first, once ("Install the Review panel in your Adobe apps?");
//! - downloads each package into its own app-data folder and checks its
//!   size and SHA-256 against the manifest before anything runs it;
//! - hands it to Adobe's installer (`--install` / `/install`), nothing else;
//! - remembers the version, and from then on checks with the shell's own
//!   updates (30 s after launch, then every 6 h) and installs a newer panel
//!   by itself, saying so in a notification.
//!
//! Nothing about the person or their projects is read or sent; the panel
//! signs itself in by code (`tracker://link-device` opens that page here,
//! in the window that is already signed in).

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use serde::Deserialize;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_store::StoreExt;

const MANIFEST_PATH: &str = "/download/adobe-panel/current.json";
const STORE: &str = "adobe-panel.json";
const INSTALLED_KEY: &str = "installedVersion";
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Larger than any panel package will be; a bigger download is refused.
const MAX_PACKAGE_BYTES: u64 = 50 * 1024 * 1024;

static BUSY: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct PanelFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct PanelManifest {
    pub version: String,
    pub minimum: String,
    pub files: BTreeMap<String, PanelFile>,
}

/// The manifest, or None for anything that isn't exactly one: a version,
/// and file names that are plain package names (never a path).
pub(crate) fn parse_manifest(raw: &str) -> Option<PanelManifest> {
    let manifest: PanelManifest = serde_json::from_str(raw).ok()?;
    parse_version(&manifest.version)?;
    parse_version(&manifest.minimum)?;
    if manifest.files.is_empty() {
        return None;
    }
    for (package, file) in &manifest.files {
        // A package for another app (DaVinci Resolve's zip, and any added later) is
        // left to the person (installs_with_adobe); only its name must still be plain.
        let known = package.len() <= 40 && package.chars().all(|c| c.is_ascii_lowercase());
        let plain = !file.name.is_empty()
            && file.name.len() <= 160
            && file.name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && !file.name.starts_with('.')
            && (file.name.ends_with(".ccx") || file.name.ends_with(".zxp") || file.name.ends_with(".zip"));
        let sha = file.sha256.len() == 64 && file.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        if !known || !plain || !sha || file.size == 0 || file.size > MAX_PACKAGE_BYTES {
            return None;
        }
    }
    Some(manifest)
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.split('.').map(|p| p.parse::<u32>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// The packages Adobe's installer takes: a .ccx (Premiere Pro, Photoshop,
/// InDesign) or a .zxp (Illustrator, After Effects). DaVinci Resolve's zip is
/// copied into Resolve's plugins folder by hand (the tracker's /download page
/// says where), so this app skips it.
pub(crate) fn installs_with_adobe(package: &str, file: &PanelFile) -> bool {
    matches!(package, "premierepro" | "photoshop" | "indesign" | "cep")
        && (file.name.ends_with(".ccx") || file.name.ends_with(".zxp"))
}

/// True when `published` is newer than what is installed (or nothing is).
pub(crate) fn is_newer(published: &str, installed: Option<&str>) -> bool {
    match (parse_version(published), installed.and_then(parse_version)) {
        (Some(p), Some(i)) => p > i,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Where Creative Cloud keeps Adobe's plugin installer on this computer.
pub(crate) fn installer_candidates() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![PathBuf::from(
            "/Library/Application Support/Adobe/Adobe Desktop Common/RemoteComponents/UPI/UnifiedPluginInstallerAgent/UnifiedPluginInstallerAgent.app/Contents/MacOS/UnifiedPluginInstallerAgent",
        )]
    } else if cfg!(target_os = "windows") {
        let common = std::env::var("CommonProgramFiles").unwrap_or_else(|_| r"C:\Program Files\Common Files".into());
        vec![PathBuf::from(common)
            .join("Adobe")
            .join("Adobe Desktop Common")
            .join("RemoteComponents")
            .join("UPI")
            .join("UnifiedPluginInstallerAgent")
            .join("UnifiedPluginInstallerAgent.exe")]
    } else {
        Vec::new()
    }
}

/// The installer's arguments: `--install <file>` on a Mac, `/install <file>` on Windows.
pub(crate) fn install_args(package: &Path) -> Vec<String> {
    let flag = if cfg!(target_os = "windows") { "/install" } else { "--install" };
    vec![flag.into(), package.to_string_lossy().into_owned()]
}

/// Whether this computer can take the panel at all (Adobe apps exist on Mac and Windows only).
pub(crate) fn supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

fn installer() -> Option<PathBuf> {
    installer_candidates().into_iter().find(|p| p.exists())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The package's bytes, if they are exactly what the manifest says.
pub(crate) fn verified(bytes: &[u8], file: &PanelFile) -> bool {
    bytes.len() as u64 == file.size && sha256_hex(bytes) == file.sha256
}

fn installed_version<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    let store = app.store(STORE).ok()?;
    store.get(INSTALLED_KEY).and_then(|v| v.as_str().map(str::to_owned))
}

fn remember_version<R: Runtime>(app: &AppHandle<R>, version: &str) {
    if let Ok(store) = app.store(STORE) {
        store.set(INSTALLED_KEY, serde_json::Value::String(version.into()));
        let _ = store.save();
    }
}

async fn fetch_manifest(client: &reqwest::Client) -> Result<PanelManifest, String> {
    let url = format!("{}{MANIFEST_PATH}", crate::APP_ORIGIN);
    let response = client.get(url).send().await.map_err(|_| "Could not reach the tracker.".to_string())?;
    if !response.status().is_success() {
        return Err("The Review panel isn't available from the tracker yet.".into());
    }
    let text = response.text().await.map_err(|_| "Could not read the tracker's answer.".to_string())?;
    parse_manifest(&text).ok_or_else(|| "The tracker's panel manifest isn't valid.".into())
}

/// Downloads, checks and installs every package. Returns how many installed.
async fn install_all<R: Runtime>(app: &AppHandle<R>, manifest: &PanelManifest) -> Result<usize, String> {
    let installer = installer().ok_or_else(|| {
        "Adobe's plugin installer wasn't found. Install or update Creative Cloud Desktop, then try again.".to_string()
    })?;
    let folder = app
        .path()
        .app_data_dir()
        .map_err(|_| "No app folder to download into.".to_string())?
        .join("adobe-panel")
        .join(&manifest.version);
    std::fs::create_dir_all(&folder).map_err(|_| "Could not make the download folder.".to_string())?;
    let client = reqwest::Client::new();
    let mut installed = 0;
    let mut failed = Vec::new();
    for (package, file) in manifest.files.iter().filter(|(p, f)| installs_with_adobe(p, f)) {
        let url = format!("{}/download/adobe-panel/{}", crate::APP_ORIGIN, slug(package));
        let bytes = match client.get(url).send().await {
            Ok(r) if r.status().is_success() => r.bytes().await.map_err(|_| ()),
            _ => Err(()),
        };
        let Ok(bytes) = bytes else {
            failed.push(package.clone());
            continue;
        };
        if !verified(&bytes, file) {
            failed.push(package.clone());
            continue;
        }
        let path = folder.join(&file.name);
        if std::fs::write(&path, &bytes).is_err() {
            failed.push(package.clone());
            continue;
        }
        let ok = std::process::Command::new(&installer)
            .args(install_args(&path))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok { installed += 1 } else { failed.push(package.clone()) }
    }
    if installed == 0 {
        return Err("Adobe's installer didn't take the panel. Close your Adobe apps and try again.".into());
    }
    if !failed.is_empty() {
        eprintln!("[adobe-panel] not installed: {failed:?}");
    }
    Ok(installed)
}

fn slug(package: &str) -> &'static str {
    match package {
        "premierepro" => "premiere-pro",
        "cep" => "after-effects-illustrator",
        "photoshop" => "photoshop",
        _ => "indesign",
    }
}

fn tell<R: Runtime>(app: &AppHandle<R>, title: &str, message: String) {
    let _ = app.dialog().message(message).title(title).show(|_| {});
}

/// The tray's "Install the Adobe panel" (and `tracker://adobe-panel`): asks
/// first, then installs or updates, and always says how it went.
pub(crate) fn install_now<R: Runtime>(app: AppHandle<R>) {
    if !supported() {
        tell(&app, "Review panel", "The Review panel is for Adobe apps on a Mac or Windows computer.".into());
        return;
    }
    std::thread::spawn(move || {
        if BUSY.swap(true, Ordering::SeqCst) {
            return;
        }
        tauri::async_runtime::block_on(async {
            let client = reqwest::Client::new();
            match fetch_manifest(&client).await {
                Err(message) => tell(&app, "Review panel", message),
                Ok(manifest) => {
                    let current = installed_version(&app);
                    let ask = app
                        .dialog()
                        .message(format!(
                            "Install the Review panel {} in Premiere Pro, After Effects, Photoshop, Illustrator and InDesign?\n\nClose your Adobe apps first. From then on this app keeps the panel up to date.",
                            manifest.version
                        ))
                        .title(if current.is_some() { "Reinstall the Review panel" } else { "Install the Review panel" })
                        .buttons(MessageDialogButtons::OkCancelCustom("Install".into(), "Not now".into()));
                    let yes = tauri::async_runtime::spawn_blocking(move || ask.blocking_show()).await.unwrap_or(false);
                    if yes {
                        match install_all(&app, &manifest).await {
                            Ok(n) => {
                                remember_version(&app, &manifest.version);
                                tell(&app, "Review panel installed", format!(
                                    "Installed in {n} place{}. Open your Adobe app, then Window › Extensions (or Plugins) › Review panel, and sign in with the tracker.",
                                    if n == 1 { "" } else { "s" }
                                ));
                            }
                            Err(message) => tell(&app, "Review panel", message),
                        }
                    }
                }
            }
        });
        BUSY.store(false, Ordering::SeqCst);
    });
}

/// Keeps an installed panel up to date, quietly. Only once someone installed
/// it through this app: nothing is installed without that first yes.
pub(crate) fn start<R: Runtime>(app: AppHandle<R>) {
    if !supported() {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(FIRST_CHECK_DELAY);
        loop {
            if let Some(installed) = installed_version(&app) {
                if !BUSY.swap(true, Ordering::SeqCst) {
                    tauri::async_runtime::block_on(async {
                        let client = reqwest::Client::new();
                        if let Ok(manifest) = fetch_manifest(&client).await {
                            if is_newer(&manifest.version, Some(&installed)) {
                                if install_all(&app, &manifest).await.is_ok() {
                                    remember_version(&app, &manifest.version);
                                    crate::banner::show(
                                        &app,
                                        "Review panel updated",
                                        &format!("Version {} is in your Adobe apps. Reopen the panel to use it.", manifest.version),
                                        crate::banner::Target::Page { path: "/link-device".into(), id: None },
                                    );
                                }
                            }
                        }
                    });
                    BUSY.store(false, Ordering::SeqCst);
                }
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(name: &str, sha: &str) -> String {
        format!(r#"{{"version":"0.2.0","publishedAt":"2026-10-14T12:00:00Z","minimum":"0.1.0","files":{{"premierepro":{{"name":"{name}","size":3,"sha256":"{sha}"}}}}}}"#)
    }

    #[test]
    fn reads_only_a_real_manifest() {
        let sha = sha256_hex(b"abc");
        assert!(parse_manifest(&manifest("review-premierepro-0.2.0.ccx", &sha)).is_some());
        assert!(parse_manifest(&manifest("../../evil.ccx", &sha)).is_none());
        assert!(parse_manifest(&manifest("setup.exe", &sha)).is_none());
        assert!(parse_manifest(&manifest("review.ccx", "short")).is_none());
        assert!(parse_manifest(r#"{"version":"x","minimum":"0.1.0","files":{}}"#).is_none());
        assert!(parse_manifest("not json").is_none());
    }

    #[test]
    fn checks_the_bytes_before_installing() {
        let sha = sha256_hex(b"abc");
        let file = parse_manifest(&manifest("review-premierepro-0.2.0.ccx", &sha)).unwrap().files["premierepro"].clone();
        assert!(verified(b"abc", &file));
        assert!(!verified(b"abd", &file));
        assert!(!verified(b"abcd", &file));
    }

    #[test]
    fn updates_only_to_a_newer_version() {
        assert!(is_newer("0.2.0", Some("0.1.9")));
        assert!(is_newer("0.10.0", Some("0.9.0")));
        assert!(!is_newer("0.2.0", Some("0.2.0")));
        assert!(!is_newer("0.1.0", Some("0.2.0")));
        assert!(is_newer("0.1.0", None));
    }

    #[test]
    fn asks_adobes_installer_with_its_own_flag() {
        let args = install_args(Path::new("/tmp/review.ccx"));
        assert_eq!(args[1], "/tmp/review.ccx");
        assert_eq!(args[0], if cfg!(target_os = "windows") { "/install" } else { "--install" });
    }

    #[test]
    fn leaves_resolve_and_anything_new_to_the_person() {
        let sha = sha256_hex(b"abc");
        let raw = format!(
            r#"{{"version":"0.2.0","minimum":"0.1.0","files":{{"cep":{{"name":"review-cep-0.2.0.zxp","size":3,"sha256":"{sha}"}},"resolve":{{"name":"review-resolve-0.2.0.zip","size":3,"sha256":"{sha}"}}}}}}"#
        );
        let manifest = parse_manifest(&raw).expect("a manifest with Resolve's zip is still a manifest");
        let adobe: Vec<_> = manifest.files.iter().filter(|(p, f)| installs_with_adobe(p, f)).map(|(p, _)| p.as_str()).collect();
        assert_eq!(adobe, vec!["cep"]);
        assert!(parse_manifest(&raw.replace("\"resolve\"", "\"Evil Name\"")).is_none());
    }

    #[test]
    fn knows_where_the_downloads_are() {
        assert_eq!(slug("premierepro"), "premiere-pro");
        assert_eq!(slug("cep"), "after-effects-illustrator");
    }
}
