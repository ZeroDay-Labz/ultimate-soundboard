use serde::{Deserialize, Serialize};

use super::tab::TabModel;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AppState {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    #[serde(default)]
    pub tabs: Vec<TabModel>,

    #[serde(default)]
    pub current_tab: usize,

    // Audio routing
    #[serde(default)]
    pub audio_output_device: Option<String>,
    #[serde(default)]
    pub audio_show_all_devices: bool,

    // Layout defaults
    #[serde(default = "default_button_size")]
    pub default_button_size: u32,
    #[serde(default = "default_true")]
    pub snap_enabled: bool,
    #[serde(default = "default_snap_step")]
    pub snap_step: u32,
    #[serde(default = "default_grid_spacing")]
    pub grid_spacing: u32,

    // Appearance
    #[serde(default)]
    pub wallpaper: Option<String>,
    #[serde(default)]
    pub theme: Option<String>,

    #[serde(default = "default_true")]
    pub always_on_top: bool,

    // Global playback level
    #[serde(default = "default_master_volume")]
    pub master_volume: f32,
    #[serde(default)]
    pub muted: bool,
}

fn default_schema_version() -> u32 { SCHEMA_VERSION }
fn default_button_size() -> u32 { crate::model::button::DEFAULT_BUTTON_SIZE }
fn default_snap_step() -> u32 { 12 }
fn default_grid_spacing() -> u32 { 10 }
fn default_true() -> bool { true }
fn default_master_volume() -> f32 { 1.0 }

impl Default for AppState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            tabs: Vec::new(),
            current_tab: 0,
            audio_output_device: None,
            audio_show_all_devices: false,
            default_button_size: default_button_size(),
            snap_enabled: true,
            snap_step: default_snap_step(),
            grid_spacing: default_grid_spacing(),
            wallpaper: None,
            theme: None,
            always_on_top: false,
            master_volume: 1.0,
            muted: false,
        }
    }
}

impl AppState {
    pub fn normalize(&mut self) {
        for t in &mut self.tabs {
            t.normalize();
        }
        if self.tabs.is_empty() {
            self.tabs.push(TabModel::new("Default"));
        }
        if self.current_tab >= self.tabs.len() {
            self.current_tab = 0;
        }
        self.master_volume = self.master_volume.clamp(0.0, 2.0);
        self.default_button_size = self.default_button_size.clamp(48, 256);
        self.snap_step = self.snap_step.min(128);
        self.grid_spacing = self.grid_spacing.min(80);
    }
}
