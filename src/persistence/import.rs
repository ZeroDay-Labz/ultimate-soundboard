use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use zip::ZipArchive;

use super::export::ExportPack;
use crate::model::{ButtonModel, TabModel};
use crate::util;

/// Where extracted zip contents (sounds/images) get written to. Kept
/// outside the OS "data dir" JSON file itself, but alongside it, since
/// buttons reference these by absolute path afterwards (files are never
/// "moved" once referenced -- this is just where the *copies pulled out of
/// the zip* land).
fn imports_root() -> PathBuf {
    super::store::data_dir().join("imported_sounds")
}

fn unique_dir(base: &Path) -> PathBuf {
    if !base.exists() {
        return base.to_path_buf();
    }
    let mut i = 1;
    loop {
        let cand = PathBuf::from(format!("{}-{i}", base.display()));
        if !cand.exists() {
            return cand;
        }
        i += 1;
    }
}

fn unique_name(target_dir: &Path, filename: &str) -> String {
    let stem = Path::new(filename).file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = Path::new(filename).extension().and_then(|s| s.to_str());
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

/// Extracts one zip member to `out_path`. Uses `enclosed_name()`, which the
/// `zip` crate refuses to return for any entry that would escape the
/// extraction root (zip-slip protection) -- entries that fail this check
/// are simply skipped.
fn safe_extract_member(
    archive: &mut ZipArchive<File>,
    index: usize,
    out_path: &Path,
) -> Result<bool> {
    let mut entry = archive.by_index(index)?;
    if entry.enclosed_name().is_none() || entry.is_dir() {
        return Ok(false);
    }
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = File::create(out_path)?;
    std::io::copy(&mut entry, &mut out)?;
    Ok(true)
}

fn find_index_by_name(archive: &mut ZipArchive<File>, name: &str) -> Option<usize> {
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            if entry.name() == name {
                return Some(i);
            }
        }
    }
    None
}

/// Imports a structured soundboard export (`soundboard.json` + `sounds/` +
/// `images/`), returning the reconstructed tabs. Direct port of
/// `import_soundboard` in `importer.py`.
pub fn import_soundboard(zip_path: &Path) -> Result<Vec<TabModel>> {
    let root = imports_root();
    fs::create_dir_all(&root)?;

    let stem = zip_path.file_stem().and_then(|s| s.to_str()).unwrap_or("import");
    let import_root = unique_dir(&root.join(stem));
    let sounds_out = import_root.join("sounds");
    let images_out = import_root.join("images");
    fs::create_dir_all(&sounds_out)?;
    fs::create_dir_all(&images_out)?;

    let file = File::open(zip_path).context("opening zip")?;
    let mut archive = ZipArchive::new(file).context("reading zip")?;

    let json_index = find_index_by_name(&mut archive, "soundboard.json")
        .ok_or_else(|| anyhow!("missing soundboard.json"))?;

    let raw = {
        let mut entry = archive.by_index(json_index)?;
        let mut s = String::new();
        entry.read_to_string(&mut s)?;
        s
    };
    let pack: ExportPack = serde_json::from_str(&raw).context("invalid soundboard.json")?;

    let mut tabs = Vec::with_capacity(pack.tabs.len());

    for export_tab in pack.tabs {
        let mut tab = TabModel::new(export_tab.name);
        tab.emoji = export_tab.emoji;
        tab.color = export_tab.color;
        tab.volume = export_tab.volume;
        tab.pitch = export_tab.pitch;
        if export_tab.button_size > 0 {
            tab.button_size = export_tab.button_size;
        }

        for eb in export_tab.buttons {
            let Some(member) = eb.file.as_deref() else { continue };
            let name = Path::new(member).file_name().and_then(|s| s.to_str()).unwrap_or("sound");
            let unique = unique_name(&sounds_out, name);
            let extracted = sounds_out.join(&unique);

            let Some(idx) = find_index_by_name(&mut archive, member) else { continue };
            if !safe_extract_member(&mut archive, idx, &extracted).unwrap_or(false) {
                continue;
            }

            let mut btn = ButtonModel::new(eb.label, extracted.to_string_lossy().to_string());
            btn.volume = eb.volume;
            btn.pitch = eb.pitch;
            btn.hotkey = eb.hotkey;
            btn.x = eb.x;
            btn.y = eb.y;
            btn.width = eb.width;
            btn.height = eb.height;
            btn.color = eb.color;
            btn.emoji = eb.emoji;

            if let Some(img_member) = eb.image.as_deref() {
                let img_name = Path::new(img_member).file_name().and_then(|s| s.to_str()).unwrap_or("image");
                let unique_img = unique_name(&images_out, img_name);
                let img_out = images_out.join(&unique_img);
                if let Some(img_idx) = find_index_by_name(&mut archive, img_member) {
                    if safe_extract_member(&mut archive, img_idx, &img_out).unwrap_or(false) {
                        btn.image = Some(img_out.to_string_lossy().to_string());
                    }
                }
            }

            btn.normalize();
            tab.buttons.push(btn);
        }

        tabs.push(tab);
    }

    Ok(tabs)
}

/// Fallback for a zip that's just a pile of audio files (no
/// `soundboard.json`): extracts every recognized audio extension into one
/// new tab. Port of `import_audio_zip` in `importer.py`.
pub fn import_audio_zip(zip_path: &Path, tab_name: &str) -> Result<TabModel> {
    let root = imports_root();
    fs::create_dir_all(&root)?;

    let stem = zip_path.file_stem().and_then(|s| s.to_str()).unwrap_or("import");
    let import_root = unique_dir(&root.join(stem));
    let sounds_out = import_root.join("sounds");
    fs::create_dir_all(&sounds_out)?;

    let file = File::open(zip_path).context("opening zip")?;
    let mut archive = ZipArchive::new(file).context("reading zip")?;

    let mut tab = TabModel::new(tab_name);

    for i in 0..archive.len() {
        let (name, is_dir) = {
            let entry = archive.by_index(i)?;
            (entry.name().to_string(), entry.is_dir())
        };
        if is_dir {
            continue;
        }
        let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or("");
        if !util::is_audio_ext(ext) {
            continue;
        }

        let src_name = Path::new(&name).file_name().and_then(|s| s.to_str()).unwrap_or("sound");
        let unique = unique_name(&sounds_out, src_name);
        let extracted = sounds_out.join(&unique);

        if !safe_extract_member(&mut archive, i, &extracted).unwrap_or(false) {
            continue;
        }

        let btn = ButtonModel::new(
            util::sanitize_label(src_name),
            extracted.to_string_lossy().to_string(),
        );
        tab.buttons.push(btn);
    }

    Ok(tab)
}

/// Try importing as a structured soundboard export first; if the zip has
/// no `soundboard.json`, fall back to treating it as a raw audio zip.
/// Port of `import_zip_auto` in `importer.py`.
pub fn import_zip_auto(zip_path: &Path) -> Result<Vec<TabModel>> {
    match import_soundboard(zip_path) {
        Ok(tabs) => Ok(tabs),
        Err(_) => {
            let tab_name = zip_path.file_stem().and_then(|s| s.to_str()).unwrap_or("Imported");
            let tab = import_audio_zip(zip_path, tab_name)?;
            Ok(vec![tab])
        }
    }
}

/// Writes an in-memory buffer to `out_path` -- small helper the
/// clone-from-URL importers use to save downloaded audio.
pub fn write_downloaded_file(out_path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = File::create(out_path)?;
    f.write_all(bytes)?;
    Ok(())
}
