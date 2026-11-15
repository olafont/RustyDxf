// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 art-tech

//! Parsed-drawing cache for the MCP tools, keyed by path and validated by
//! modification time and size, so repeated calls on a large file are fast.

use rustydxf_core::Drawing;
use rustydxf_core::tessellate::{Mesh, TessellateOptions, tessellate};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// Most drawings kept in memory at once.
const CAPACITY: usize = 8;

pub struct Entry {
    pub drawing: Drawing,
    mesh: OnceLock<Mesh>,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("path", &self.drawing.path)
            .finish_non_exhaustive()
    }
}

impl Entry {
    /// The render mesh (with text as outlines), built on first use.
    pub fn mesh(&self) -> &Mesh {
        self.mesh.get_or_init(|| {
            tessellate(
                &self.drawing,
                TessellateOptions {
                    text_outlines: true,
                },
            )
        })
    }
}

struct Slot {
    modified: Option<SystemTime>,
    len: u64,
    last_used: u64,
    entry: Arc<Entry>,
}

#[derive(Default)]
pub struct Cache {
    slots: Mutex<HashMap<PathBuf, Slot>>,
    clock: Mutex<u64>,
}

impl Cache {
    fn tick(&self) -> u64 {
        let mut c = self.clock.lock().unwrap_or_else(|e| e.into_inner());
        *c += 1;
        *c
    }

    /// Load `path`, reusing the cached drawing when the file is unchanged.
    /// Errors are user-facing messages.
    pub fn get(&self, path: &Path) -> Result<Arc<Entry>, String> {
        let meta = std::fs::metadata(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("file not found: {}", path.display()),
            std::io::ErrorKind::PermissionDenied => {
                format!("permission denied reading {}", path.display())
            }
            _ => format!("cannot read {}: {e}", path.display()),
        })?;
        if meta.is_dir() {
            return Err(format!("{} is a directory, not a DXF file", path.display()));
        }
        let (modified, len) = (meta.modified().ok(), meta.len());
        let now = self.tick();

        {
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(slot) = slots.get_mut(path)
                && slot.modified == modified
                && slot.len == len
            {
                slot.last_used = now;
                return Ok(slot.entry.clone());
            }
        }

        // Parse outside the lock: big files take a while.
        let drawing = rustydxf_core::load_file(path).map_err(|e| e.to_string())?;
        let entry = Arc::new(Entry {
            drawing,
            mesh: OnceLock::new(),
        });

        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if slots.len() >= CAPACITY
            && !slots.contains_key(path)
            && let Some(oldest) = slots
                .iter()
                .min_by_key(|(_, s)| s.last_used)
                .map(|(p, _)| p.clone())
        {
            slots.remove(&oldest);
        }
        slots.insert(
            path.to_path_buf(),
            Slot {
                modified,
                len,
                last_used: now,
                entry: entry.clone(),
            },
        );
        Ok(entry)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.lock().unwrap().len()
    }
}

/// Resolve a user-supplied path: expand a leading `~` and require the result
/// to be absolute.
pub fn resolve_path(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("path is empty".into());
    }
    let expanded = if raw == "~" || raw.starts_with("~/") || raw.starts_with("~\\") {
        let home = directories::BaseDirs::new()
            .map(|b| b.home_dir().to_path_buf())
            .ok_or("cannot expand ~: no home directory")?;
        home.join(raw[1..].trim_start_matches(['/', '\\']))
    } else {
        PathBuf::from(raw)
    };
    if !expanded.is_absolute() {
        return Err(format!(
            "path must be absolute (or start with ~), got: {raw}"
        ));
    }
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_must_be_absolute_and_tilde_expands() {
        assert!(resolve_path("relative/file.dxf").is_err());
        assert!(resolve_path("file.dxf").is_err());
        assert!(resolve_path("").is_err());
        assert!(resolve_path("  ").is_err());
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        assert_eq!(resolve_path("~").unwrap(), home);
        assert_eq!(resolve_path("~/a/b.dxf").unwrap(), home.join("a/b.dxf"));
        // "~user" is not expanded and is not absolute.
        assert!(resolve_path("~other/file.dxf").is_err());
        #[cfg(unix)]
        assert_eq!(
            resolve_path("/tmp/x.dxf").unwrap(),
            PathBuf::from("/tmp/x.dxf")
        );
    }

    #[test]
    fn cache_reuses_until_the_file_changes() {
        let dir = std::env::temp_dir().join(format!("rustydxf-cache-test-{}", std::process::id()));
        rustydxf_core::fixtures::write_all(&dir).unwrap();
        let path = dir.join("lines.dxf");
        let cache = Cache::default();

        let a = cache.get(&path).unwrap();
        let b = cache.get(&path).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "second call hits the cache");
        assert_eq!(cache.len(), 1);

        // Replace the file with different content (different size).
        std::fs::copy(dir.join("arcs.dxf"), &path).unwrap();
        let c = cache.get(&path).unwrap();
        assert!(!Arc::ptr_eq(&a, &c), "changed file is re-parsed");
        assert_eq!(c.drawing.entities.len(), 5);

        assert!(
            cache
                .get(&dir.join("missing.dxf"))
                .unwrap_err()
                .contains("not found")
        );
        assert!(cache.get(&dir).unwrap_err().contains("directory"));
        assert!(
            cache
                .get(&dir.join("corrupt.dxf"))
                .unwrap_err()
                .contains("not a valid DXF")
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
