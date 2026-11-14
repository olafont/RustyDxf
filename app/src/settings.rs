//! User preferences, stored as JSON in the per-user config directory.
//!
//! The viewer edits them (File → Preferences…); the MCP server process reads
//! them on demand, so a change applies without restarting either side.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the operating system.
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderBackground {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpSettings {
    /// MCP clients may drive the viewer window (`show_in_viewer`,
    /// `get_viewer_state`).
    pub allow_viewer_control: bool,
    /// `show_in_viewer` may start the viewer when it is not running.
    pub launch_viewer: bool,
    /// Background used by `render_png` when the caller does not choose one.
    pub render_background: RenderBackground,
    /// Port used by `rustydxf --mcp --http` when no address is given.
    pub http_port: u16,
}

pub const DEFAULT_HTTP_PORT: u16 = 8765;

impl Default for McpSettings {
    fn default() -> Self {
        McpSettings {
            allow_viewer_control: true,
            launch_viewer: true,
            render_background: RenderBackground::Dark,
            http_port: DEFAULT_HTTP_PORT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub mcp: McpSettings,
}

/// Where the settings live. `RUSTYDXF_CONFIG_DIR` overrides the directory
/// (the tests use it to stay away from the user's real preferences).
pub fn path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RUSTYDXF_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join(FILE_NAME));
    }
    directories::ProjectDirs::from("dev", "art-tech", "rustydxf")
        .map(|d| d.config_dir().join(FILE_NAME))
}

impl Settings {
    /// Load the settings; a missing or unreadable file gives the defaults.
    pub fn load() -> Settings {
        let Some(path) = path() else {
            return Settings::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Settings::parse(&text).unwrap_or_else(|e| {
                log::warn!("ignoring {}: {e}", path.display());
                Settings::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => {
                log::warn!("cannot read {}: {e}", path.display());
                Settings::default()
            }
        }
    }

    fn parse(text: &str) -> Result<Settings, serde_json::Error> {
        let mut settings: Settings = serde_json::from_str(text)?;
        if settings.mcp.http_port < 1024 {
            settings.mcp.http_port = DEFAULT_HTTP_PORT;
        }
        Ok(settings)
    }

    pub fn save(&self) -> Result<()> {
        let path = path().context("no config directory on this system")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serialising the settings")?;
        // Write then rename, so a reader never sees a half-written file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_partial_files() {
        let d = Settings::default();
        assert_eq!(d.theme, Theme::System);
        assert!(d.mcp.allow_viewer_control && d.mcp.launch_viewer);
        assert_eq!(d.mcp.http_port, DEFAULT_HTTP_PORT);

        // Missing fields take their defaults; unknown ones are ignored.
        let s =
            Settings::parse(r#"{"theme":"light","mcp":{"launch_viewer":false},"x":1}"#).unwrap();
        assert_eq!(s.theme, Theme::Light);
        assert!(!s.mcp.launch_viewer);
        assert!(s.mcp.allow_viewer_control);
        assert_eq!(s.mcp.render_background, RenderBackground::Dark);

        assert_eq!(Settings::parse("{}").unwrap(), d);
        assert!(Settings::parse("not json").is_err());
        assert!(Settings::parse(r#"{"theme":"purple"}"#).is_err());
        // Privileged ports are not accepted.
        let s = Settings::parse(r#"{"mcp":{"http_port":80}}"#).unwrap();
        assert_eq!(s.mcp.http_port, DEFAULT_HTTP_PORT);
    }

    #[test]
    fn round_trip() {
        let s = Settings {
            theme: Theme::Dark,
            mcp: McpSettings {
                render_background: RenderBackground::Light,
                http_port: 9123,
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(Settings::parse(&json).unwrap(), s);
    }
}
