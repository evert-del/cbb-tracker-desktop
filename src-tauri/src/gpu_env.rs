//! Which GPU WebKitGTK draws with on Linux. Set in `run()` before GTK starts.
//!
//! WebKitGTK's DMA-BUF renderer hands finished frames to the compositor on
//! the GPU. Without it every frame is copied through main memory, and on
//! 4K screens the whole app lags behind Chrome (Michael's laptop, 2026-10-17).
//!
//! It was switched off everywhere in 0.2.15 because the page flashed black on
//! an Intel + NVIDIA laptop under Wayland. The flashing came from the web
//! process picking NVIDIA's EGL (glvnd lists `10_nvidia.json` before
//! `50_mesa.json`), not from DMA-BUF itself: with EGL pointed at Mesa, the
//! GPU that drives the screen, DMA-BUF stays on, is as quick as Chrome and
//! never flashed (tested live 2026-10-17). So:
//!
//! - NVIDIA beside an Intel/AMD GPU: EGL on Mesa, DMA-BUF on.
//! - NVIDIA alone: DMA-BUF off, as since 0.2.15.
//! - Intel/AMD alone: nothing changes.
//!
//! Anyone who set either variable themselves keeps their own choice.

// Only `apply` (Linux) reaches the decision; elsewhere it is tests only.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::sync::OnceLock;

const DMABUF_VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
const EGL_VENDOR_VAR: &str = "__EGL_VENDOR_LIBRARY_FILENAMES";

/// Where glvnd distributions put Mesa's EGL vendor file.
const MESA_VENDOR_FILES: &[&str] = &[
    "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
    "/etc/glvnd/egl_vendor.d/50_mesa.json",
];

/// Kernel drivers whose GPUs Mesa draws with.
const MESA_DRIVERS: &[&str] = &["i915", "xe", "amdgpu", "radeon"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Choice {
    /// Hybrid laptop: EGL pointed at this Mesa vendor file, DMA-BUF on.
    MesaEgl(String),
    /// NVIDIA's EGL is the only one: DMA-BUF off.
    DmabufOff,
    /// Nothing to change.
    Default,
    /// The user set one of the variables; left alone.
    UserSet,
}

/// Pure decision from the render nodes' kernel drivers and the Mesa vendor
/// file found on disk (if any).
pub(crate) fn choose(drivers: &[String], mesa_vendor_file: Option<&str>) -> Choice {
    let nvidia = drivers.iter().any(|d| d == "nvidia");
    let mesa = drivers.iter().any(|d| MESA_DRIVERS.contains(&d.as_str()));
    match (nvidia, mesa, mesa_vendor_file) {
        (true, true, Some(file)) => Choice::MesaEgl(file.to_string()),
        (true, _, _) => Choice::DmabufOff,
        _ => Choice::Default,
    }
}

static CHOSEN: OnceLock<Choice> = OnceLock::new();

/// What `apply` chose, for Diagnostics.
pub(crate) fn describe() -> String {
    match CHOSEN.get() {
        Some(Choice::MesaEgl(_)) => "GPU fast path on Intel/AMD (NVIDIA skipped)".to_string(),
        Some(Choice::DmabufOff) => "GPU fast path off (NVIDIA only)".to_string(),
        Some(Choice::Default) => "GPU fast path on".to_string(),
        Some(Choice::UserSet) => format!(
            "set by hand ({DMABUF_VAR}={}, {EGL_VENDOR_VAR}={})",
            std::env::var(DMABUF_VAR).unwrap_or_default(),
            std::env::var(EGL_VENDOR_VAR).unwrap_or_default(),
        ),
        None => "not chosen (not Linux)".to_string(),
    }
}

/// Variables `apply` set itself. Children the shell starts in the user's own
/// session (xdg-open) leave them out, so the host's apps choose for themselves.
pub(crate) fn set_by_shell() -> &'static [&'static str] {
    match CHOSEN.get() {
        Some(Choice::MesaEgl(_)) => &[EGL_VENDOR_VAR],
        Some(Choice::DmabufOff) => &[DMABUF_VAR],
        _ => &[],
    }
}

/// The kernel driver behind each DRM render node, e.g. `i915`, `nvidia`.
#[cfg(target_os = "linux")]
fn render_node_drivers() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("renderD"))
        .filter_map(|e| std::fs::read_link(e.path().join("device/driver")).ok())
        .filter_map(|link| link.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// Sets the environment for WebKitGTK. Must run before GTK starts and before
/// any thread is spawned.
#[cfg(target_os = "linux")]
pub(crate) fn apply() {
    let choice = if std::env::var_os(DMABUF_VAR).is_some() || std::env::var_os(EGL_VENDOR_VAR).is_some() {
        Choice::UserSet
    } else {
        let mesa_file = MESA_VENDOR_FILES
            .iter()
            .copied()
            .find(|f| std::path::Path::new(f).is_file());
        choose(&render_node_drivers(), mesa_file)
    };
    match &choice {
        Choice::MesaEgl(file) => std::env::set_var(EGL_VENDOR_VAR, file),
        Choice::DmabufOff => std::env::set_var(DMABUF_VAR, "1"),
        Choice::Default | Choice::UserSet => {}
    }
    let _ = CHOSEN.set(choice);
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESA: &str = "/usr/share/glvnd/egl_vendor.d/50_mesa.json";

    fn drivers(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn hybrid_laptop_draws_with_mesa() {
        assert_eq!(choose(&drivers(&["i915", "nvidia"]), Some(MESA)), Choice::MesaEgl(MESA.into()));
        assert_eq!(choose(&drivers(&["nvidia", "amdgpu"]), Some(MESA)), Choice::MesaEgl(MESA.into()));
        assert_eq!(choose(&drivers(&["xe", "nvidia"]), Some(MESA)), Choice::MesaEgl(MESA.into()));
    }

    #[test]
    fn nvidia_without_mesa_keeps_dmabuf_off() {
        assert_eq!(choose(&drivers(&["nvidia"]), Some(MESA)), Choice::DmabufOff);
        assert_eq!(choose(&drivers(&["i915", "nvidia"]), None), Choice::DmabufOff);
    }

    #[test]
    fn mesa_alone_changes_nothing() {
        assert_eq!(choose(&drivers(&["i915"]), Some(MESA)), Choice::Default);
        assert_eq!(choose(&drivers(&["amdgpu"]), None), Choice::Default);
        assert_eq!(choose(&drivers(&["nouveau"]), Some(MESA)), Choice::Default);
        assert_eq!(choose(&[], None), Choice::Default);
    }
}
