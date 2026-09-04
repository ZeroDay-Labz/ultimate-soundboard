use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use zip::write::{FileOptions, SimpleFileOptions};
use zip::ZipWriter;

use crate::model::AppState;

pub const EXPORT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportButton {
    pub label: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default = "one")]
    pub volume: f32,
    #[serde(default = "one")]
    pub pitch: f32,
    #[serde(default)]
    pub hotkey: Option<String>,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub missing_file: bool,
}

fn one() -> f32 { 1.0 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportTab {
    pub name: String,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default = "one")]
    pub volume: f32,
    #[serde(default = "one")]
    pub pitch: f32,
    #[serde(default)]
    pub button_size: u32,
    #[serde(default)]
    pub buttons: Vec<ExportButton>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPack {
    pub format_version: u32,
    pub exported_at_unix: u64,
    pub tabs: Vec<ExportTab>,
}

fn unique_name(target_dir: &Path, name: &str) -> String {
    let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = Path::new(name).extension().and_then(|s| s.to_str());
    let make = |i: u32| match (ext, i) {
        (Some(e), 0) => format!("{stem}.{e}"),
        (Some(e), n) => format!("{stem}-{n}.{e}"),
        (None, 0) => stem.to_string(),
        (None, n) => format!("{stem}-{n}"),
    };
    let mut i = 0;
    loop {
        let candidate = make(i);
        if !target_dir.join(&candidate).exists() {
            return candidate;
        }
        i += 1;
    }
}

/// Exports `state` (tabs + buttons + referenced sound/image files) to a
/// shareable `.zip`. Mirrors `exporter.py`: bundles every distinct
/// referenced file exactly once under `sounds/` / `images/` inside the
/// archive, and still exports buttons whose source file is missing
/// (flagged `missing_file: true`) rather than dropping them silently.
pub fn export_soundboard(state: &AppState, zip_path: &Path) -> Result<()> {
    if let Some(parent) = zip_path.parent() {
        fs::create_dir_all(parent).ok();
    }

    let tmp_dir = std::env::temp_dir().join(format!(
        "ultimate-soundboard-export-{}",
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
    ));
    let sounds_dir = tmp_dir.join("sounds");
    let images_dir = tmp_dir.join("images");
    fs::create_dir_all(&sounds_dir).context("creating export sounds dir")?;
    fs::create_dir_all(&images_dir).context("creating export images dir")?;

    let mut sound_map: HashMap<PathBuf, String> = HashMap::new();
    let mut image_map: HashMap<PathBuf, String> = HashMap::new();

    let mut export_tabs = Vec::with_capacity(state.tabs.len());

    for tab in &state.tabs {
        let mut export_buttons = Vec::with_capacity(tab.buttons.len());

        for btn in &tab.buttons {
            let mut export_btn = ExportButton {
                label: btn.label.clone(),
                file: None,
                volume: btn.volume,
                pitch: btn.pitch,
                hotkey: btn.hotkey.clone(),
                x: btn.x,
                y: btn.y,
                width: btn.width,
                height: btn.height,
                color: btn.color.clone(),
                emoji: btn.emoji.clone(),
                image: None,
                missing_file: false,
            };

            if !btn.file.is_empty() {
                let src = Path::new(&btn.file);
                if src.exists() {
                    let abs = src.canonicalize().unwrap_or_else(|_| src.to_path_buf());
                    let rel = sound_map.entry(abs.clone()).or_insert_with(|| {
                        let name = src.file_name().and_then(|s| s.to_str()).unwrap_or("sound");
                        let unique = unique_name(&sounds_dir, name);
                        let dst = sounds_dir.join(&unique);
                        let _ = fs::copy(src, &dst);
                        format!("sounds/{unique}")
                    });
                    export_btn.file = Some(rel.clone());
                } else {
                    export_btn.missing_file = true;
                }
            } else {
                export_btn.missing_file = true;
            }

            if let Some(img) = &btn.image {
                let img_src = Path::new(img);
                if img_src.exists() {
                    let abs = img_src.canonicalize().unwrap_or_else(|_| img_src.to_path_buf());
                    let rel = image_map.entry(abs).or_insert_with(|| {
                        let name = img_src.file_name().and_then(|s| s.to_str()).unwrap_or("image");
                        let unique = unique_name(&images_dir, name);
                        let dst = images_dir.join(&unique);
                        let _ = fs::copy(img_src, &dst);
                        format!("images/{unique}")
                    });
                    export_btn.image = Some(rel.clone());
                }
            }

            export_buttons.push(export_btn);
        }

        export_tabs.push(ExportTab {
            name: tab.name.clone(),
            emoji: tab.emoji.clone(),
            color: tab.color.clone(),
            volume: tab.volume,
            pitch: tab.pitch,
            button_size: tab.button_size,
            buttons: export_buttons,
        });
    }

    let pack = ExportPack {
        format_version: EXPORT_FORMAT_VERSION,
        exported_at_unix: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        tabs: export_tabs,
    };
    let json = serde_json::to_string_pretty(&pack)?;

    let file = File::create(zip_path).context("creating zip file")?;
    let mut zip = ZipWriter::new(file);
    let options: SimpleFileOptions = FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    zip.start_file("soundboard.json", options)?;
    zip.write_all(json.as_bytes())?;

    for (path, rel) in walk_files(&sounds_dir, "sounds") {
        zip.start_file(&rel, options)?;
        let bytes = fs::read(&path)?;
        zip.write_all(&bytes)?;
    }
    for (path, rel) in walk_files(&images_dir, "images") {
        zip.start_file(&rel, options)?;
        let bytes = fs::read(&path)?;
        zip.write_all(&bytes)?;
    }

    zip.finish()?;
    let _ = fs::remove_dir_all(&tmp_dir);
    Ok(())
}

fn walk_files(dir: &Path, prefix: &str) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                    out.push((path.clone(), format!("{prefix}/{name}")));
                }
            }
        }
    }
    out
}
