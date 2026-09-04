use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use url::Url;

use super::{downloads_root, ImportResult, ImportedButton, Importer, ProgressFn, RunningFn};


const DOMAIN: &str = "realmofdarkness.net/sb/";
const SITE_ROOT: &str = "https://www.realmofdarkness.net";
const USER_AGENT: &str = "Mozilla/5.0";

/// realmofdarkness.net soundboard cloner. This is a faithful port of the
/// scraper the user built and asked to carry forward as-is: it locates a
/// board's audio base URL through five fallback stages (an inline
/// `sndpath` JS variable, a direct `/audio/....mp3` reference in the page,
/// any `/audio/.../` directory mentioned in the page, the same two
/// searches over linked same-site JS files, and finally slug/category
/// folder-naming heuristics), then guesses each button's real filename
/// (`id`, `id-a`, `id-b`, numeric variants, thumbnail-id matches) and
/// downloads whichever guess a ranged GET confirms exists.
pub struct RealmOfDarknessImporter;

impl Importer for RealmOfDarknessImporter {
    fn can_handle(&self, source: &str) -> bool {
        source.contains(DOMAIN)
    }

    fn import_from(&self, source: &str, progress: &ProgressFn, running: &RunningFn) -> Result<ImportResult> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(25))
            .build()?;

        let html_text = get_text(&client, source, source)?;
        let doc = Html::parse_document(&html_text);

        let slug = source.trim_end_matches('/').rsplit('/').next().unwrap_or(source).to_string();
        let tab_name = slug.clone();

        let button_items = extract_button_items(&doc);
        if button_items.is_empty() {
            return Err(anyhow!("No sound buttons found on page"));
        }

        let thumb_ids = extract_thumb_ids(&doc);
        let thumb_set: HashSet<String> = thumb_ids.iter().cloned().collect();
        let prefers_dash_a = thumb_ids.iter().any(|t| t.ends_with("-a") || t.contains("-a"));

        let mut probe_ids = thumb_ids.clone();
        for (sid, _) in &button_items {
            if !probe_ids.contains(sid) {
                probe_ids.push(sid.clone());
            }
        }

        let audio_base = resolve_audio_base(&client, &doc, &html_text, source, &slug, &probe_ids, running)?
            .ok_or_else(|| anyhow!("Could not determine audio base path"))?;

        let base_dir = downloads_root().join(&slug);
        fs::create_dir_all(&base_dir)?;

        let buttons = download_all_buttons(
            &client,
            &button_items,
            &thumb_ids,
            &thumb_set,
            prefers_dash_a,
            &audio_base,
            &base_dir,
            progress,
            running,
        );

        if buttons.is_empty() {
            return Err(anyhow!("No audio files were successfully imported"));
        }

        Ok(ImportResult { tab_name, buttons })
    }
}

/// How many buttons to resolve+download at once. Bounded concurrency --
/// effectively a semaphore with this many permits, implemented as a fixed
/// pool of worker threads pulling from a shared job queue (`crossbeam_channel`
/// gives that for free: N consumers racing `recv()` on the same receiver
/// is exactly N concurrent permits, no separate semaphore type needed).
/// High enough to meaningfully parallelize a 100+ button board, low
/// enough to stay polite to the site instead of hammering it.
const DOWNLOAD_CONCURRENCY: usize = 8;

/// Resolves and downloads every button's audio file in parallel across a
/// small worker pool, instead of one full request-probe-download cycle at
/// a time. This is the dominant cost for a big board (confirmed against
/// the real site: ~150 buttons took several minutes fully sequential),
/// since each button can involve several candidate-filename probes before
/// landing on the real one.
#[allow(clippy::too_many_arguments)]
fn download_all_buttons(
    client: &reqwest::blocking::Client,
    button_items: &[(String, String)],
    thumb_ids: &[String],
    thumb_set: &HashSet<String>,
    prefers_dash_a: bool,
    audio_base: &str,
    base_dir: &Path,
    progress: &ProgressFn,
    running: &RunningFn,
) -> Vec<ImportedButton> {
    let (job_tx, job_rx) = crossbeam_channel::unbounded::<(String, String)>();
    for item in button_items {
        let _ = job_tx.send(item.clone());
    }
    drop(job_tx);

    let (result_tx, result_rx) = crossbeam_channel::unbounded::<ImportedButton>();
    let worker_count = DOWNLOAD_CONCURRENCY.min(button_items.len().max(1));

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            scope.spawn(move || {
                while let Ok((sid, label)) = job_rx.recv() {
                    if !running() {
                        continue;
                    }

                    let candidates = build_candidates(&sid, thumb_ids, thumb_set, prefers_dash_a);
                    let mut saved: Option<String> = None;

                    for fname in candidates {
                        if !running() {
                            break;
                        }
                        let mp3_url = format!("{audio_base}{fname}.mp3");
                        progress(&format!("Downloading: {label} ({fname}.mp3)"));
                        match download(client, &mp3_url, base_dir, running) {
                            Ok(path) => {
                                saved = Some(path);
                                break;
                            }
                            Err(_) => continue,
                        }
                    }

                    if let Some(path) = saved {
                        progress(&format!("Downloaded: {label}"));
                        let _ = result_tx.send(ImportedButton { label: label.clone(), file: path });
                    }
                }
            });
        }
        drop(result_tx);
    });

    result_rx.into_iter().collect()
}

// -------------------------
// Page parsing
// -------------------------

fn extract_button_items(doc: &Html) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let Ok(sel) = Selector::parse("button[id]") else { return out };

    for el in doc.select(&sel) {
        let sid = el.value().attr("id").unwrap_or("").trim().to_string();
        if sid.is_empty() {
            continue;
        }
        let class = el.value().classes().collect::<Vec<_>>().join(" ").to_lowercase();
        if class.contains("stop") || sid.ends_with("-st") || sid == "1-st" || sid == "stop" {
            continue;
        }
        if !seen.insert(sid.clone()) {
            continue;
        }
        let label = element_text(el);
        let label = if label.trim().is_empty() { sid.clone() } else { label };
        out.push((sid, label));
    }
    out
}

fn element_text(el: ElementRef) -> String {
    el.text().collect::<Vec<_>>().join("").trim().to_string()
}

fn extract_thumb_ids(doc: &Html) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let Ok(sel) = Selector::parse("img.sbth[id]") else { return out };
    for el in doc.select(&sel) {
        if let Some(id) = el.value().attr("id") {
            let id = id.trim().to_string();
            if !id.is_empty() && seen.insert(id.clone()) {
                out.push(id);
            }
        }
    }
    out
}

// -------------------------
// Candidate filenames
// -------------------------

fn build_candidates(sid: &str, thumb_ids: &[String], thumb_set: &HashSet<String>, prefers_dash_a: bool) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let add = |c: String, candidates: &mut Vec<String>| {
        if !c.is_empty() && !candidates.contains(&c) {
            candidates.push(c);
        }
    };

    if prefers_dash_a {
        add(format!("{sid}-a"), &mut candidates);
        add(format!("{sid}a"), &mut candidates);
        add(format!("{sid}_a"), &mut candidates);
    }

    add(sid.to_string(), &mut candidates);

    for variant in [
        format!("{sid}-a"),
        format!("{sid}a"),
        format!("{sid}_a"),
        format!("{sid}-b"),
        format!("{sid}_b"),
        format!("{sid}-1"),
        format!("{sid}_1"),
    ] {
        if thumb_set.contains(&variant) {
            candidates.retain(|c| c != &variant);
            candidates.insert(0, variant);
        }
    }

    if sid.chars().all(|c| c.is_ascii_digit()) {
        candidates.truncate(5);
        return candidates;
    }

    let mut scan_limit = 8;
    for t in thumb_ids {
        if t.starts_with(sid) {
            add(t.clone(), &mut candidates);
            scan_limit -= 1;
            if scan_limit <= 0 {
                break;
            }
        }
    }

    candidates.truncate(12);
    candidates
}

// -------------------------
// Base resolution
// -------------------------

fn resolve_audio_base(
    client: &reqwest::blocking::Client,
    doc: &Html,
    html_text: &str,
    source: &str,
    slug: &str,
    probe_ids: &[String],
    running: &RunningFn,
) -> Result<Option<String>> {
    // 1) sndpath from inline scripts
    if let Some(js_base) = extract_sndpath_from_soup(doc) {
        let base = normalize_base(&js_base);
        if probe_any(client, &base, probe_ids) {
            return Ok(Some(base));
        }
    }

    // 2) direct /audio/...mp3 in HTML
    if let Ok(re) = Regex::new(r#"(["'])(/audio/[^"']+?\.(?:mp3|wav|ogg))\1"#) {
        if let Some(caps) = re.captures(html_text) {
            let full = format!("{SITE_ROOT}{}", &caps[2]);
            if let Some(idx) = full.rfind('/') {
                return Ok(Some(format!("{}/", &full[..idx])));
            }
        }
    }

    // 3) any /audio/.../ dirs mentioned in the page
    for base in extract_audio_dirs_from_text(html_text) {
        if !running() {
            return Ok(None);
        }
        if probe_any(client, &base, probe_ids) {
            return Ok(Some(base));
        }
    }

    // 4) same-site linked JS files
    for surl in extract_script_srcs(doc, source) {
        if !running() {
            return Ok(None);
        }
        let Some(js_text) = fetch_text_limited(client, &surl, 1_000_000) else { continue };

        if let Some(js_base) = extract_sndpath_from_text(&js_text) {
            let base = normalize_base(&js_base);
            if probe_any(client, &base, probe_ids) {
                return Ok(Some(base));
            }
        }

        for base in extract_audio_dirs_from_text(&js_text) {
            if probe_any(client, &base, probe_ids) {
                return Ok(Some(base));
            }
        }
    }

    // 5) heuristic fallbacks
    for base in heuristic_bases(doc, slug) {
        if !running() {
            return Ok(None);
        }
        if probe_any(client, &base, probe_ids) {
            return Ok(Some(base));
        }
    }

    Ok(None)
}

fn probe_any(client: &reqwest::blocking::Client, base: &str, probe_ids: &[String]) -> bool {
    for pid in probe_ids.iter().take(12) {
        if probe(client, base, pid) {
            return true;
        }
        if probe(client, base, &format!("{pid}-a")) {
            return true;
        }
    }
    false
}

fn probe(client: &reqwest::blocking::Client, base: &str, sound_id: &str) -> bool {
    let url = format!("{base}{sound_id}.mp3");
    let resp = client
        .get(&url)
        .header("Accept-Encoding", "identity")
        .header("Range", "bytes=0-0")
        .header("Connection", "close")
        .header("Referer", format!("{SITE_ROOT}/"))
        .header("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(12))
        .send();

    matches!(resp, Ok(r) if r.status().as_u16() == 200 || r.status().as_u16() == 206)
}

// -------------------------
// Extractors
// -------------------------

fn extract_sndpath_from_soup(doc: &Html) -> Option<String> {
    let sel = Selector::parse("script").ok()?;
    for el in doc.select(&sel) {
        let text = element_text(el);
        if text.trim().is_empty() {
            continue;
        }
        if let Some(v) = extract_sndpath_from_text(&text) {
            return Some(v);
        }
    }
    None
}

fn extract_sndpath_from_text(text: &str) -> Option<String> {
    let re = Regex::new(r#"\bsndpath\s*=\s*['"]([^'"]+)['"]"#).ok()?;
    re.captures(text).map(|c| c[1].to_string())
}

fn extract_audio_dirs_from_text(text: &str) -> Vec<String> {
    let mut dirs = Vec::new();

    if let Ok(re) = Regex::new(r"(/audio/[a-zA-Z0-9/_-]+/)") {
        for m in re.find_iter(text) {
            if m.as_str().len() >= 8 {
                dirs.push(format!("{SITE_ROOT}{}", m.as_str()));
            }
        }
    }

    if let Ok(re) = Regex::new(r"(?i)(/audio/[a-zA-Z0-9/_-]+?\.(?:mp3|wav|ogg))") {
        for m in re.find_iter(text) {
            let full = format!("{SITE_ROOT}{}", m.as_str());
            if let Some(idx) = full.rfind('/') {
                dirs.push(format!("{}/", &full[..idx]));
            }
        }
    }

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for d in dirs {
        let d = normalize_base(&d);
        if seen.insert(d.clone()) {
            out.push(d);
        }
    }
    out
}

fn extract_script_srcs(doc: &Html, page_url: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(sel) = Selector::parse("script[src]") else { return out };
    let Ok(base) = Url::parse(page_url) else { return out };

    for el in doc.select(&sel) {
        let Some(src) = el.value().attr("src") else { continue };
        let Ok(abs) = base.join(src) else { continue };
        if !abs.host_str().map(|h| h.contains("realmofdarkness.net")).unwrap_or(false) {
            continue;
        }
        let s = abs.to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn fetch_text_limited(client: &reqwest::blocking::Client, url: &str, limit_bytes: usize) -> Option<String> {
    let resp = client
        .get(url)
        .header("Accept-Encoding", "identity")
        .header("Connection", "close")
        .header("Referer", format!("{SITE_ROOT}/"))
        .header("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(15))
        .send()
        .ok()?
        .error_for_status()
        .ok()?;

    let mut data = Vec::new();
    let mut reader = resp.take(limit_bytes as u64);
    reader.read_to_end(&mut data).ok()?;

    String::from_utf8(data).ok()
}

// -------------------------
// Heuristics
// -------------------------

fn heuristic_bases(doc: &Html, slug: &str) -> Vec<String> {
    let (base_name, page) = split_slug(slug);
    let slug_norm = slug_to_folder(slug);
    let base_norm = slug_to_folder(&base_name);

    let cat_slugs = extract_category_slugs(doc);
    let cat_folders = map_categories_to_folders(&cat_slugs);

    let mut candidates: Vec<String> = Vec::new();

    if let Some(page) = &page {
        candidates.push(format!("{SITE_ROOT}/audio/{base_norm}/{page}/"));
        candidates.push(format!("{SITE_ROOT}/audio/{base_name}/{page}/"));
    }

    candidates.push(format!("{SITE_ROOT}/audio/{slug_norm}/"));
    candidates.push(format!("{SITE_ROOT}/audio/{base_norm}/"));

    for cf in &cat_folders {
        if cf.is_empty() {
            continue;
        }
        candidates.push(format!("{SITE_ROOT}/audio/{cf}/{slug_norm}/"));
        candidates.push(format!("{SITE_ROOT}/audio/{cf}/{base_norm}/"));
        candidates.push(format!("{SITE_ROOT}/audio/{cf}/{slug}/"));
        candidates.push(format!("{SITE_ROOT}/audio/{cf}/{base_name}/"));
    }

    candidates.push(format!("{SITE_ROOT}/audio/sfx/{slug_norm}/"));
    candidates.push(format!("{SITE_ROOT}/audio/sfx/{base_norm}/"));
    candidates.push(format!("{SITE_ROOT}/audio/sfx/gun/{slug_norm}/"));
    candidates.push(format!("{SITE_ROOT}/audio/sfx/gun/{base_norm}/"));

    let parts: Vec<&str> = slug.split('-').collect();
    if parts.len() >= 2 {
        let first = slug_to_folder(parts[0]);
        let last = slug_to_folder(parts[parts.len() - 1]);
        let middle: Vec<String> = parts[1..parts.len() - 1].iter().filter(|p| !p.trim().is_empty()).map(|p| slug_to_folder(p)).collect();

        candidates.push(format!("{SITE_ROOT}/audio/vg/{first}/{last}/"));
        candidates.push(format!("{SITE_ROOT}/audio/vg/{first}/{first}/{last}/"));

        if !middle.is_empty() {
            let mid_join = middle.join("/");
            candidates.push(format!("{SITE_ROOT}/audio/vg/{first}/{mid_join}/{last}/"));
            candidates.push(format!("{SITE_ROOT}/audio/vg/{first}/{first}/{mid_join}/{last}/"));
        }

        if first.starts_with("mk") && first != "mk" {
            let rem = &first[2..];
            if !rem.is_empty() {
                candidates.push(format!("{SITE_ROOT}/audio/vg/mk/{rem}/{last}/"));
                candidates.push(format!("{SITE_ROOT}/audio/vg/mk/{first}/{last}/"));
                candidates.push(format!("{SITE_ROOT}/audio/vg/mk/mk/{rem}/{last}/"));
            }
        }
    }

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for c in candidates {
        let c = normalize_base(&c);
        if seen.insert(c.clone()) {
            out.push(c);
        }
    }
    out
}

fn normalize_base(base: &str) -> String {
    let mut base = if let Some(rest) = base.strip_prefix('/') { format!("{SITE_ROOT}{rest}", rest = format!("/{rest}")) } else { base.to_string() };
    if !base.ends_with('/') {
        base.push('/');
    }
    base
}

fn slug_to_folder(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase()
}

fn split_slug(slug: &str) -> (String, Option<String>) {
    let parts: Vec<&str> = slug.split('-').collect();
    if parts.len() >= 2 && parts[parts.len() - 1].chars().all(|c| c.is_ascii_digit()) && !parts[parts.len() - 1].is_empty() {
        (parts[..parts.len() - 1].join("-"), Some(parts[parts.len() - 1].to_string()))
    } else {
        (slug.to_string(), None)
    }
}

fn extract_category_slugs(doc: &Html) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(sel) = Selector::parse(r#"a[rel="category tag"], a[rel="tag"]"#) else { return out };
    for el in doc.select(&sel) {
        let Some(href) = el.value().attr("href") else { continue };
        if let Ok(u) = Url::parse(href).or_else(|_| Url::parse(&format!("{SITE_ROOT}{href}"))) {
            if let Some(mut segs) = u.path_segments() {
                if let Some(last) = segs.next_back() {
                    if !last.is_empty() {
                        out.push(last.to_lowercase());
                    }
                }
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|s| seen.insert(s.clone()));
    out
}

fn map_categories_to_folders(cat_slugs: &[String]) -> Vec<String> {
    let category_map: &[(&str, &str)] = &[
        ("politics", "pol"),
        ("political", "pol"),
        ("sfx", "sfx"),
        ("sound-effects", "sfx"),
        ("video-games", "vg"),
        ("videogames", "vg"),
        ("star-wars", "sw"),
        ("cartoons", "toons"),
        ("toons", "toons"),
        ("popular", "pop"),
        ("trump", "trump"),
        ("arnold", "arnold"),
        ("dr-phil", "drphil"),
        ("drphil", "drphil"),
    ];

    let mut out = Vec::new();
    for s in cat_slugs {
        if let Some((_, v)) = category_map.iter().find(|(k, _)| k == s) {
            out.push(v.to_string());
        }
        let s2: String = s.chars().filter(|c| *c != '-').collect();
        if s2.len() <= 12 && s2.chars().all(|c| c.is_ascii_alphanumeric()) {
            out.push(s2);
        }
    }
    let mut seen = HashSet::new();
    out.retain(|s| seen.insert(s.clone()));
    out
}

// -------------------------
// Download
// -------------------------

fn get_text(client: &reqwest::blocking::Client, url: &str, referer: &str) -> Result<String> {
    let resp = client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Referer", referer)
        .header("Accept-Encoding", "identity")
        .timeout(Duration::from_secs(25))
        .send()?
        .error_for_status()?;
    Ok(resp.text()?)
}

fn download(client: &reqwest::blocking::Client, url: &str, out_dir: &Path, running: &RunningFn) -> Result<String> {
    let name = Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|s| s.last().map(str::to_string)))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "sound.mp3".to_string());

    let path = out_dir.join(&name);
    if path.exists() && path.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(path.to_string_lossy().to_string());
    }

    let mut resp = client
        .get(url)
        .header("Accept-Encoding", "identity")
        .header("Connection", "close")
        .header("Referer", format!("{SITE_ROOT}/"))
        .header("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(35))
        .send()?
        .error_for_status()?;

    let tmp_path = path.with_extension("part");
    let mut f = fs::File::create(&tmp_path)?;

    let mut buf = [0u8; 64 * 1024];
    loop {
        if !running() {
            drop(f);
            let _ = fs::remove_file(&tmp_path);
            return Err(anyhow!("Import cancelled"));
        }
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n])?;
    }
    drop(f);
    fs::rename(&tmp_path, &path)?;

    Ok(path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live network test against a real board -- run explicitly with
    /// `cargo test --release -- --ignored --nocapture realm_of_darkness`.
    /// Not run by default so `cargo test` stays offline/fast.
    #[test]
    #[ignore]
    fn imports_a_real_board() {
        let importer = RealmOfDarknessImporter;
        let progress = |msg: &str| println!("[progress] {msg}");
        let running = || true;

        let result = importer
            .import_from("https://www.realmofdarkness.net/sb/trump/", &progress, &running)
            .expect("import should succeed");

        assert_eq!(result.tab_name, "trump");
        assert!(!result.buttons.is_empty(), "expected at least one downloaded button");
        for b in &result.buttons {
            let path = std::path::Path::new(&b.file);
            assert!(path.exists(), "downloaded file should exist: {}", b.file);
            assert!(path.metadata().unwrap().len() > 0, "downloaded file should be non-empty: {}", b.file);
        }
        println!("Imported {} buttons into tab '{}'", result.buttons.len(), result.tab_name);
    }
}
