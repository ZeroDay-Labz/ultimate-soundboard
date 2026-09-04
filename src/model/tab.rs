use serde::{Deserialize, Serialize};

use super::button::{is_valid_hex_color, ButtonModel, DEFAULT_BUTTON_SIZE};

const SCHEMA_VERSION: u32 = 1;

fn clampf(v: f32, lo: f32, hi: f32) -> f32 {
    if v.is_nan() { lo } else { v.max(lo).min(hi) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    Grid,
    Absolute,
}

impl Default for LayoutMode {
    fn default() -> Self { LayoutMode::Grid }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TabModel {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Transient: stable per-tab identity for UI widget IDs (e.g.
    /// drag-to-reorder in `tabs.rs`), so egui's ID-keyed interaction state
    /// (drag/hover) survives a tab moving to a different array index.
    /// Not persisted -- a fresh id each load is fine, nothing compares it
    /// across sessions.
    #[serde(skip, default = "uuid::Uuid::new_v4")]
    pub id: uuid::Uuid,

    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub buttons: Vec<ButtonModel>,

    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default = "default_pitch")]
    pub pitch: f32,

    #[serde(default = "default_button_size")]
    pub button_size: u32,
    #[serde(default)]
    pub layout_mode: LayoutMode,
    #[serde(default = "default_snap_size")]
    pub snap_size: u32,
    #[serde(default = "default_grid_spacing")]
    pub grid_spacing: u32,

    /// Transient UI state: whether this tab is currently in layout-edit
    /// mode. Not persisted -- always reopens in normal (play) mode, same
    /// as the Python app's per-widget `layout_edit_mode` flag.
    #[serde(skip)]
    pub edit_mode: bool,

    /// Transient: the column count the grid layout last rendered with,
    /// cached so switching into Edit mode can freeze buttons into their
    /// exact on-screen grid positions instead of guessing.
    #[serde(skip, default = "default_last_grid_columns")]
    pub last_grid_columns: usize,
}

fn default_last_grid_columns() -> usize { 1 }

fn default_schema_version() -> u32 { SCHEMA_VERSION }
fn default_name() -> String { "Untitled".to_string() }
fn default_volume() -> f32 { 1.0 }
fn default_pitch() -> f32 { 1.0 }
fn default_button_size() -> u32 { DEFAULT_BUTTON_SIZE }
fn default_snap_size() -> u32 { 10 }
fn default_grid_spacing() -> u32 { 10 }

impl TabModel {
    pub fn new(name: impl Into<String>) -> Self {
        let mut t = Self {
            schema_version: SCHEMA_VERSION,
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            emoji: None,
            color: None,
            buttons: Vec::new(),
            volume: 1.0,
            pitch: 1.0,
            button_size: DEFAULT_BUTTON_SIZE,
            layout_mode: LayoutMode::Grid,
            snap_size: 10,
            grid_spacing: 10,
            edit_mode: false,
            last_grid_columns: 1,
        };
        t.normalize();
        t
    }

    pub fn normalize(&mut self) {
        let name = self.name.trim();
        self.name = if name.is_empty() {
            "Untitled".to_string()
        } else {
            name.chars().take(48).collect()
        };

        self.emoji = self.emoji.take().filter(|s| !s.trim().is_empty());
        self.color = self.color.take().and_then(|c| {
            let c = c.trim().to_string();
            if is_valid_hex_color(&c) { Some(c.to_lowercase()) } else { None }
        });

        self.volume = clampf(self.volume, 0.0, 2.0);
        self.pitch = clampf(self.pitch, 0.25, 4.0);
        self.button_size = self.button_size.clamp(48, 256);
        self.snap_size = self.snap_size.min(128);
        self.grid_spacing = self.grid_spacing.min(80);

        for b in &mut self.buttons {
            b.normalize();
        }
    }

    pub fn add_button(&mut self, button: ButtonModel) {
        self.buttons.push(button);
    }

    pub fn remove_button(&mut self, id: uuid::Uuid) {
        self.buttons.retain(|b| b.id != id);
    }
}
