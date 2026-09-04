use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use regex::Regex;
use scraper::{Html, Selector};
use url::Url;

use super::{downloads_root, ImportResult, ImportedButton, Importer, ProgressFn, RunningFn};

const AUDIO_EXTS: &[&str] = &["wav", "mp3", "ogg", "flac", "aiff", "aif"];

/// Generic soundboard cloner: fetches a page, pulls every `<audio>`/
/// `<source>`/`<a href>` link and any raw audio URL embedded in the HTML,
/// and downloads them. Port of `url_cloner.py`.
pub struct UrlCloner;

impl Importer for UrlCloner {
    fn can_handle(&self, source: &str) -> bool {
        source.starts_with("http://") || source.starts_with("https://")
    }

    fn import_from(&self, source: &str, progress: &ProgressFn, running: &RunningFn) -> Result<ImportResult> {
        let client = reqwest::blocking::Client::builder()
            .user_agent("Mozilla/5.0")
            .timeout(Duration::from_secs(15))
            .build()?;

        let html = client.get(source).send()?.error_for_status()?.text()?;
        let audio_urls = extract_audio_urls(&html, source);
        if audio_urls.is_empty() {
            return Err(anyhow!("No audio files found at URL"));
        }

        let tab_name = Url::parse(source).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| "soundboard".to_string());

        let out_dir = downloads_root().join(&tab_name);
        fs::create_dir_all(&out_dir)?;

        let mut buttons = Vec::new();
        for url in audio_urls {
            if !running() {
                break;
            }
            match download_audio(&client, &url, &out_dir) {
                Ok(path) => {
                    let label = Path::new(&path).file_stem().and_then(|s| s.to_str()).unwrap_or("Sound").to_string();
                    progress(&format!("Downloaded: {label}"));
                    buttons.push(ImportedButton { label, file: path });
                }
                Err(e) => log::warn!("failed to download {url}: {e}"),
            }
        }

        if buttons.is_empty() {
            return Err(anyhow!("No audio files were successfully downloaded"));
        }

        Ok(ImportResult { tab_name, buttons })
    }
}

fn extract_audio_urls(html: &str, page_url: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let mut urls: BTreeSet<String> = BTreeSet::new();

    if let Ok(sel) = Selector::parse("audio[src], audio source[src], source[src]") {
        for el in doc.select(&sel) {
            if let Some(src) = el.value().attr("src") {
                if let Some(abs) = resolve_url(page_url, src) {
                    urls.insert(abs);
                }
            }
        }
    }

    if let Ok(sel) = Selector::parse("a[href]") {
        for el in doc.select(&sel) {
            if let Some(href) = el.value().attr("href") {
                if looks_like_audio(href) {
                    if let Some(abs) = resolve_url(page_url, href) {
                        urls.insert(abs);
                    }
                }
            }
        }
    }

    if let Ok(re) = Regex::new(r#"https?://[^\s"']+\.(?:wav|mp3|ogg|flac|aiff|aif)"#) {
        for m in re.find_iter(html) {
            urls.insert(m.as_str().to_string());
        }
    }

    urls.into_iter().collect()
}

fn resolve_url(base: &str, href: &str) -> Option<String> {
    let base = Url::parse(base).ok()?;
    base.join(href).ok().map(|u| u.to_string())
}

fn looks_like_audio(url: &str) -> bool {
    let base = url.split('?').next().unwrap_or(url).to_lowercase();
    AUDIO_EXTS.iter().any(|ext| base.ends_with(&format!(".{ext}")))
}

fn download_audio(client: &reqwest::blocking::Client, url: &str, out_dir: &Path) -> Result<String> {
    let name = Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|s| s.last().map(str::to_string)))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "sound".to_string());

    let out_path = out_dir.join(&name);
    if out_path.exists() {
        return Ok(out_path.to_string_lossy().to_string());
    }

    let bytes = client.get(url).send()?.error_for_status()?.bytes()?;
    let mut f = fs::File::create(&out_path)?;
    f.write_all(&bytes)?;

    Ok(out_path.to_string_lossy().to_string())
}
