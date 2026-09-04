use std::path::Path;

/// Extensions the app treats as droppable/importable audio. Kept broad on
/// purpose (this is the exact list the user asked to support): wav/pcm
/// containers, the common lossy formats, and Discord-style opus/ogg.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "wave", "pcm", "mp3", "ogg", "opus", "flac", "aiff", "aif", "m4a", "aac", "wma",
];

pub fn is_audio_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => AUDIO_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(ext)),
        None => false,
    }
}

pub fn is_audio_ext(ext: &str) -> bool {
    AUDIO_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(ext))
}

/// Recursively collects audio files under `root` (or just `root` itself if
/// it's a single file). Mirrors `_iter_audio_files` in the Python UI code.
pub fn iter_audio_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if root.is_file() {
        if is_audio_file(root) {
            out.push(root.to_path_buf());
        }
        return out;
    }
    if root.is_dir() {
        for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
            let p = entry.path();
            if is_audio_file(p) {
                out.push(p.to_path_buf());
            }
        }
    }
    out
}

/// Friendly label from a filename: strips extension, replaces `_`/`-` runs
/// with spaces, collapses whitespace, truncates to 48 chars.
pub fn sanitize_label(name: &str) -> String {
    let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let mut out = String::new();
    let mut last_was_space = false;
    for c in stem.chars() {
        let c = if c == '_' || c == '-' { ' ' } else { c };
        if c == ' ' {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        "Sound".to_string()
    } else {
        trimmed.chars().take(48).collect()
    }
}
