use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MIN_BUTTON_SIZE: u32 = 60;
pub const DEFAULT_BUTTON_SIZE: u32 = 96;

const SCHEMA_VERSION: u32 = 1;

fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    if v.is_nan() { lo } else { v.max(lo).min(hi) }
}

pub(crate) fn is_valid_hex_color(s: &str) -> bool {
    let s = s.strip_prefix('#').unwrap_or(s);
    (s.len() == 6 || s.len() == 8) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Normalize a user-provided path: expand `~`, use forward slashes, but do
/// NOT resolve/canonicalize it (that would fail for nonexistent files and
/// would turn portable relative paths into machine-specific absolute ones).
pub fn normalize_path_string(raw: &str) -> String {
    let s = raw.trim().trim_matches('"').trim_matches('\'');
    if s.is_empty() {
        return String::new();
    }

    let expanded = if let Some(rest) = s.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| s.to_string())
    } else {
        s.to_string()
    };

    expanded.replace('\\', "/")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ButtonModel {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    #[serde(default = "default_label")]
    pub label: String,
    #[serde(default)]
    pub file: String,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default = "default_pitch")]
    pub pitch: f32,
    #[serde(default)]
    pub hotkey: Option<String>,
    #[serde(default = "default_xy")]
    pub x: i32,
    #[serde(default = "default_xy")]
    pub y: i32,
    #[serde(default = "default_size")]
    pub width: u32,
    #[serde(default = "default_size")]
    pub height: u32,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub missing_file: bool,
}

fn default_schema_version() -> u32 { SCHEMA_VERSION }
fn default_label() -> String { "Sound".to_string() }
fn default_volume() -> f32 { 1.0 }
fn default_pitch() -> f32 { 1.0 }
fn default_xy() -> i32 { 20 }
fn default_size() -> u32 { DEFAULT_BUTTON_SIZE }

impl ButtonModel {
    pub fn new(label: impl Into<String>, file: impl Into<String>) -> Self {
        let mut b = Self {
            schema_version: SCHEMA_VERSION,
            id: Uuid::new_v4(),
            label: label.into(),
            file: file.into(),
            volume: 1.0,
            pitch: 1.0,
            hotkey: None,
            x: 20,
            y: 20,
            width: DEFAULT_BUTTON_SIZE,
            height: DEFAULT_BUTTON_SIZE,
            image: None,
            color: None,
            emoji: None,
            missing_file: false,
        };
        b.normalize();
        b
    }

    /// Re-applies the same clamping/validation rules the Python
    /// `ButtonModel.__post_init__` enforced, and recomputes `missing_file`.
    /// Call this after loading from disk or editing any field programmatically.
    pub fn normalize(&mut self) {
        self.file = normalize_path_string(&self.file);
        self.image = self.image.as_deref().map(normalize_path_string).filter(|s| !s.is_empty());

        let label = self.label.trim();
        self.label = if label.is_empty() {
            "Sound".to_string()
        } else {
            label.chars().take(64).collect()
        };

        self.volume = clampf(self.volume, 0.0, 2.0);
        self.pitch = clampf(self.pitch, 0.25, 4.0);

        self.width = self.width.max(MIN_BUTTON_SIZE);
        self.height = self.height.max(MIN_BUTTON_SIZE);

        if let Some(hk) = &self.hotkey {
            if hk.trim().is_empty() {
                self.hotkey = None;
            }
        }

        self.color = self.color.take().and_then(|c| {
            let c = c.trim().to_string();
            if is_valid_hex_color(&c) { Some(c.to_lowercase()) } else { None }
        });

        self.missing_file = self.file.is_empty() || !self.file_exists();
    }

    pub fn file_exists(&self) -> bool {
        if self.file.is_empty() {
            return false;
        }
        Path::new(&self.file).exists()
    }

    pub fn image_exists(&self) -> bool {
        match &self.image {
            Some(p) if !p.is_empty() => Path::new(p).exists(),
            _ => false,
        }
    }
}
