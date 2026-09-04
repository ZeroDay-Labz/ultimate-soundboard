use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::model::AppState;

const APP_DIR_NAME: &str = "ultimate-soundboard";
const STATE_FILE_NAME: &str = "soundboard.json";

/// Per-OS data directory for this app. Under Flatpak this resolves inside
/// the sandbox's own `~/.var/app/<id>/data/` automatically since `dirs`
/// reads `$XDG_DATA_HOME`, which the Flatpak runtime already redirects.
pub fn data_dir() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    base.join(APP_DIR_NAME)
}

fn state_file() -> PathBuf {
    data_dir().join(STATE_FILE_NAME)
}

fn backup_file() -> PathBuf {
    data_dir().join(format!("{STATE_FILE_NAME}.bak"))
}

/// Loads state from disk. Never fails the app: missing/corrupt files just
/// produce a fresh default state, same tolerance the Python loader had.
pub fn load_state() -> AppState {
    let path = state_file();
    let mut state = match fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<AppState>(&raw) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("soundboard.json failed to parse ({e}), trying backup");
                load_backup().unwrap_or_default()
            }
        },
        Err(_) => AppState::default(),
    };
    state.normalize();
    state
}

fn load_backup() -> Option<AppState> {
    let raw = fs::read_to_string(backup_file()).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Atomic save: write to a temp file, fsync, rotate the previous good file
/// to `.bak`, then rename into place. Mirrors the Python store's durability
/// approach so a crash mid-write can't corrupt the user's soundboard.
pub fn save_state(state: &AppState) -> Result<()> {
    let dir = data_dir();
    fs::create_dir_all(&dir).context("creating data dir")?;

    let path = state_file();
    let tmp_path = path.with_extension("json.tmp");

    let data = serde_json::to_string_pretty(state).context("serializing state")?;

    {
        let mut f = fs::File::create(&tmp_path).context("creating tmp state file")?;
        f.write_all(data.as_bytes()).context("writing tmp state file")?;
        f.sync_all().ok();
    }

    if path.exists() {
        // Best-effort backup; don't block the save if this fails.
        let _ = fs::rename(&path, backup_file());
    }

    fs::rename(&tmp_path, &path).context("renaming tmp state file into place")?;
    Ok(())
}
