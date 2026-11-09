//! Saved servers and settings, stored as TOML in the user's config directory.
//! Secrets never go here: passwords and key passphrases live in the OS credential store.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Auth {
    /// Password, asked for on connect unless saved in the credential store.
    Password,
    /// Private key file (OpenSSH, PEM or PuTTY .ppk).
    Key { path: PathBuf },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Server {
    pub id: Uuid,
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    /// Free-form group shown as a folder in the sidebar. Empty = ungrouped.
    #[serde(default)]
    pub group: String,
}

fn default_port() -> u16 {
    22
}

impl Server {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            host: String::new(),
            port: 22,
            user: "root".into(),
            auth: Auth::Password,
            group: String::new(),
        }
    }

    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() { &self.host } else { &self.name }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    Block,
    Beam,
    Underline,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub font_size: f32,
    pub line_height: f32,
    pub scrollback: usize,
    pub cursor: CursorStyle,
    pub cursor_blink: bool,
    /// Inner padding around the terminal grid, in points.
    pub padding: f32,
    /// Gap between the window edge / sidebar and the terminal pane (Hyprland-style gaps).
    pub gaps: f32,
    pub rounding: f32,
    pub border_width: f32,
    /// Copy the selection to the clipboard as soon as the mouse button is released.
    pub copy_on_select: bool,
    /// Right click pastes (PuTTY behaviour).
    pub right_click_paste: bool,
    pub bold_is_bright: bool,
    pub keepalive_secs: u64,
    pub sidebar_width: f32,
    /// Opacity of the terminal and panel backgrounds (lets the blur/aurora through).
    pub opacity: f32,
    /// Windows acrylic blur behind the window.
    pub blur: bool,
    /// Animated GLSL aurora behind the panels.
    pub animated_background: bool,
    /// Rotate the gradient on the focused pane border (Hyprland `borderangle`).
    pub border_animation: bool,
    /// Smooth cursor movement with a trail, like kitty's `cursor_trail`.
    pub cursor_trail: bool,
    /// Master switch for UI animations.
    pub animations: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "Graphite Mono".into(),
            font_size: 13.5,
            line_height: 1.2,
            scrollback: 10_000,
            cursor: CursorStyle::Block,
            cursor_blink: true,
            padding: 18.0,
            gaps: 10.0,
            rounding: 12.0,
            border_width: 2.0,
            copy_on_select: false,
            right_click_paste: true,
            bold_is_bright: true,
            keepalive_secs: 30,
            sidebar_width: 250.0,
            opacity: 0.86,
            blur: true,
            animated_background: true,
            border_animation: true,
            cursor_trail: true,
            animations: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    #[serde(rename = "server")]
    pub servers: Vec<Server>,
}

/// `%APPDATA%\haunt\config`, or `HAUNT_CONFIG_DIR` if set (portable installs, testing).
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("HAUNT_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    directories::ProjectDirs::from("", "", "haunt").map(|d| d.config_dir().to_path_buf())
}

pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

pub fn known_hosts_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("known_hosts"))
}

impl Config {
    /// Loads the config. A missing file gives defaults; a broken file is kept aside
    /// (renamed to `config.toml.broken`) instead of being silently overwritten.
    pub fn load() -> (Self, Option<String>) {
        let Some(path) = config_path() else {
            return (Self::default(), Some("no config directory available".into()));
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => (cfg, None),
                Err(e) => {
                    let backup = path.with_extension("toml.broken");
                    let _ = std::fs::rename(&path, &backup);
                    (
                        Self::default(),
                        Some(format!("config.toml was invalid and was moved to {}: {e}", backup.display())),
                    )
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (Self::default(), Some(format!("cannot read {}: {e}", path.display()))),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path().context("no config directory available")?;
        let text = toml::to_string_pretty(self)?;
        write_atomic(&path, text.as_bytes())
    }
}

/// Write to a temp file next to the target, then rename over it, so a crash
/// mid-write never leaves a truncated config behind.
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, data).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}
