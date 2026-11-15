// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! Platform integration: file association (`--register`) and receiving
//! "open this file" requests from the OS.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Files the OS asked us to open that the app has not picked up yet.
static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static REPAINT: OnceLock<egui::Context> = OnceLock::new();

/// Queue a file for the app and wake the UI. Safe to call before the app
/// exists (a cold launch from Finder delivers files very early).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn push_opened_file(path: PathBuf) {
    OPENED.lock().unwrap_or_else(|e| e.into_inner()).push(path);
    if let Some(ctx) = REPAINT.get() {
        ctx.request_repaint();
    }
}

/// Take the files queued by the OS since the last call.
pub fn take_opened_files() -> Vec<PathBuf> {
    std::mem::take(&mut *OPENED.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Let OS callbacks wake the UI.
pub fn register_repaint(ctx: egui::Context) {
    let _ = REPAINT.set(ctx);
}

/// A command chosen from the menu bar.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    About,
    Preferences,
    Open,
    Close,
    ZoomExtents,
    ToggleLayers,
    CopyMcpConfig,
}

#[cfg(target_os = "macos")]
pub use macos::NativeMenu;

/// Platforms without a native menu bar use the in-window one instead.
#[cfg(not(target_os = "macos"))]
pub struct NativeMenu;

#[cfg(not(target_os = "macos"))]
impl NativeMenu {
    pub fn install(_ctx: &egui::Context) -> Option<NativeMenu> {
        None
    }
    pub fn take_actions(&self) -> Vec<MenuAction> {
        Vec::new()
    }
    pub fn sync(&self, _has_document: bool, _layers_shown: bool) {}
}

/// Hook OS-level "open file" events. Must run after the winit event loop has
/// been created. A no-op except on macOS, where files opened from Finder
/// arrive as Apple Events rather than through argv.
pub fn install_open_handler() {
    #[cfg(target_os = "macos")]
    if !macos::install_open_handler() {
        log::warn!("could not install the macOS open-file handler; Finder opens will be ignored");
    }
}

/// Register RustyDxf as the handler for `.dxf` files for the current user.
pub fn register() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    return macos::register();
    #[cfg(windows)]
    return windows::register();
    #[cfg(target_os = "linux")]
    return linux::register();
    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    anyhow::bail!("--register is not supported on this platform");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opened_files_queue_drains() {
        // Other tests do not touch the queue, so this is deterministic.
        push_opened_file(PathBuf::from("/tmp/a.dxf"));
        push_opened_file(PathBuf::from("/tmp/b.dxf"));
        assert_eq!(
            take_opened_files(),
            vec![PathBuf::from("/tmp/a.dxf"), PathBuf::from("/tmp/b.dxf")]
        );
        assert!(take_opened_files().is_empty());
    }
}
