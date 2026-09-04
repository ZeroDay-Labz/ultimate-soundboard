use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::RichText;
use global_hotkey::hotkey::{Code, HotKey};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use uuid::Uuid;

use crate::audio::{AudioEngine, PlayJob};
use crate::importers;
use crate::importers::swf_rip::SwfRipResult;
use crate::model::{AppState, ButtonModel, TabModel};
use crate::persistence;
use crate::util;

use super::{board, settings, tabs, toast};

enum CloneMsg {
    Progress(String),
    Done(Result<importers::ImportResult, String>),
}

struct CloneJob {
    rx: mpsc::Receiver<CloneMsg>,
    running: Arc<AtomicBool>,
    log: Vec<String>,
}

const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

pub struct SoundboardApp {
    state: AppState,
    engine: Option<AudioEngine>,
    engine_error: Option<String>,
    renaming_tab: Option<(usize, String)>,
    dirty: bool,
    dirty_since: Option<Instant>,
    toasts: toast::Toasts,
    hotkey_manager: Option<GlobalHotKeyManager>,
    space_hotkey_id: Option<u32>,
    space_hotkey_is_global: bool,
    /// hotkey id -> button id, for per-button global hotkeys. Rebuilt
    /// wholesale via `resync_button_hotkeys` whenever a hotkey field
    /// changes (cheap: at most a few dozen entries).
    button_hotkeys: HashMap<u32, Uuid>,
    registered_button_hotkeys: Vec<HotKey>,
    settings_open: bool,
    wallpaper_texture: Option<(String, egui::TextureHandle)>,
    clone_dialog_open: bool,
    clone_url: String,
    clone_job: Option<CloneJob>,
    swf_job: Option<mpsc::Receiver<Result<SwfRipResult, String>>>,
}

impl SoundboardApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        super::theme::apply(&cc.egui_ctx);

        let mut state = persistence::load_state();
        state.normalize();

        let (engine, engine_error) = match AudioEngine::new(state.audio_output_device.as_deref()) {
            Ok(e) => (Some(e), None),
            Err(e) => {
                log::error!("failed to start audio engine: {e:#}");
                (None, Some(format!("{e:#}")))
            }
        };

        let (hotkey_manager, space_hotkey_id, space_hotkey_is_global) = setup_global_space_hotkey();

        let mut app = Self {
            state,
            engine,
            engine_error,
            renaming_tab: None,
            dirty: false,
            dirty_since: None,
            toasts: toast::Toasts::default(),
            hotkey_manager,
            space_hotkey_id,
            space_hotkey_is_global,
            button_hotkeys: HashMap::new(),
            registered_button_hotkeys: Vec::new(),
            settings_open: false,
            wallpaper_texture: None,
            clone_dialog_open: false,
            clone_url: String::new(),
            clone_job: None,
            swf_job: None,
        };
        app.resync_button_hotkeys();
        app.apply_master_gain();
        app
    }

    fn start_clone_import(&mut self) {
        let url = self.clone_url.trim().to_string();
        if url.is_empty() {
            return;
        }

        let (tx, rx) = mpsc::channel();
        let running = Arc::new(AtomicBool::new(true));
        let running_bg = running.clone();

        std::thread::spawn(move || {
            let tx_progress = tx.clone();
            let progress = move |msg: &str| {
                let _ = tx_progress.send(CloneMsg::Progress(msg.to_string()));
            };
            let is_running = move || running_bg.load(Ordering::Relaxed);
            let result = importers::import_from_source(&url, &progress, &is_running);
            let msg = match result {
                Ok(r) => CloneMsg::Done(Ok(r)),
                Err(e) => CloneMsg::Done(Err(format!("{e:#}"))),
            };
            let _ = tx.send(msg);
        });

        self.clone_job = Some(CloneJob { rx, running, log: Vec::new() });
    }

    fn poll_clone_job(&mut self, ctx: &egui::Context) {
        let mut finished = None;
        if let Some(job) = &mut self.clone_job {
            while let Ok(msg) = job.rx.try_recv() {
                match msg {
                    CloneMsg::Progress(p) => job.log.push(p),
                    CloneMsg::Done(result) => finished = Some(result),
                }
            }
            ctx.request_repaint_after(Duration::from_millis(150));
        }

        if let Some(result) = finished {
            match result {
                Ok(r) => {
                    let count = r.buttons.len();
                    let mut tab = TabModel::new(r.tab_name);
                    tab.button_size = self.state.default_button_size;
                    for b in r.buttons {
                        let mut btn = ButtonModel::new(b.label, b.file);
                        btn.width = tab.button_size;
                        btn.height = tab.button_size;
                        tab.buttons.push(btn);
                    }
                    self.state.tabs.push(tab);
                    self.state.current_tab = self.state.tabs.len() - 1;
                    self.mark_dirty();
                    self.clone_dialog_open = false;
                    self.clone_url.clear();
                    self.toasts.success(format!("Cloned {count} sound{}", if count == 1 { "" } else { "s" }));
                }
                Err(e) => {
                    self.toasts.error(format!("Clone failed: {e}"));
                }
            }
            self.clone_job = None;
        }
    }

    fn clone_dialog(&mut self, ctx: &egui::Context) {
        if !self.clone_dialog_open {
            return;
        }
        let mut open = self.clone_dialog_open;
        let mut start = false;
        let mut cancel = false;

        egui::Window::new("Clone from URL").open(&mut open).show(ctx, |ui| {
            if let Some(job) = &self.clone_job {
                ui.label("Working…");
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for line in &job.log {
                        ui.label(line);
                    }
                });
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            } else {
                ui.label("Paste a soundboard URL. realmofdarkness.net boards get the full scraper; other pages fall back to grabbing any audio links found.");
                ui.text_edit_singleline(&mut self.clone_url);
                if ui.button("Start").clicked() {
                    start = true;
                }
            }
        });

        self.clone_dialog_open = open;
        if start {
            self.start_clone_import();
        }
        if cancel {
            if let Some(job) = &self.clone_job {
                job.running.store(false, Ordering::Relaxed);
            }
        }
    }

    fn start_swf_rip(&mut self, path: PathBuf) {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("soundboard.swf").to_string();
        self.toasts.info(format!("Ripping audio from {name}…"));

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = importers::swf_rip::extract_sounds(&path).map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
        self.swf_job = Some(rx);
    }

    fn poll_swf_job(&mut self) {
        let Some(rx) = &self.swf_job else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.swf_job = None;

        match result {
            Ok(r) => {
                if r.buttons.is_empty() {
                    self.toasts.warning(format!(
                        "No usable sounds found in {} ({} skipped, unsupported codec)",
                        r.tab_name, r.skipped
                    ));
                    return;
                }
                let extracted = r.buttons.len();
                let mut tab = TabModel::new(r.tab_name);
                tab.button_size = self.state.default_button_size;
                for b in r.buttons {
                    let mut btn = ButtonModel::new(b.label, b.file);
                    btn.width = tab.button_size;
                    btn.height = tab.button_size;
                    tab.buttons.push(btn);
                }
                self.state.tabs.push(tab);
                self.state.current_tab = self.state.tabs.len() - 1;
                self.mark_dirty();

                if r.skipped > 0 {
                    let codecs = r.skipped_codecs.iter().collect::<std::collections::HashSet<_>>();
                    let codec_list = codecs.into_iter().cloned().collect::<Vec<_>>().join(", ");
                    self.toasts.warning(format!("Extracted {extracted} sounds, skipped {} ({codec_list} not supported)", r.skipped));
                } else {
                    self.toasts.success(format!("Extracted {extracted} sound{} from the .swf", if extracted == 1 { "" } else { "s" }));
                }
            }
            Err(e) => {
                self.toasts.error(format!("SWF rip failed: {e}"));
            }
        }
    }

    fn switch_output_device(&mut self, device: Option<String>) {
        match AudioEngine::new(device.as_deref()) {
            Ok(e) => {
                self.engine = Some(e);
                self.engine_error = None;
                self.apply_master_gain();
            }
            Err(e) => {
                log::error!("failed to switch audio device: {e:#}");
                self.toasts.error(format!("Could not switch audio device: {e}"));
            }
        }
    }

    fn apply_master_gain(&self) {
        if let Some(engine) = &self.engine {
            let gain = if self.state.muted { 0.0 } else { self.state.master_volume };
            engine.set_master_gain(gain);
        }
    }

    fn ensure_wallpaper_texture(&mut self, ctx: &egui::Context) {
        let Some(path) = self.state.wallpaper.clone() else {
            self.wallpaper_texture = None;
            return;
        };
        if self.wallpaper_texture.as_ref().map(|(p, _)| p == &path).unwrap_or(false) {
            return;
        }
        match image::open(&path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let color_image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
                let tex = ctx.load_texture("wallpaper", color_image, egui::TextureOptions::LINEAR);
                self.wallpaper_texture = Some((path, tex));
            }
            Err(e) => {
                log::warn!("failed to load wallpaper {path}: {e}");
                self.toasts.warning(format!("Could not load background image: {e}"));
                self.wallpaper_texture = None;
            }
        }
    }

    /// Rebuilds every registered per-button global hotkey from scratch:
    /// unregisters the old set, then parses and (re-)registers each
    /// button's `hotkey` string across all tabs. Called once at startup
    /// and again whenever a hotkey field is edited.
    fn resync_button_hotkeys(&mut self) {
        let Some(manager) = &self.hotkey_manager else { return };

        if !self.registered_button_hotkeys.is_empty() {
            let _ = manager.unregister_all(&self.registered_button_hotkeys);
        }
        self.registered_button_hotkeys.clear();
        self.button_hotkeys.clear();

        for tab in &self.state.tabs {
            for btn in &tab.buttons {
                let Some(hk_str) = btn.hotkey.as_deref().filter(|s| !s.trim().is_empty()) else { continue };
                let Ok(hotkey) = hk_str.trim().parse::<HotKey>() else {
                    log::warn!("button '{}' has an unparseable hotkey '{hk_str}'", btn.label);
                    continue;
                };
                match manager.register(hotkey) {
                    Ok(()) => {
                        self.button_hotkeys.insert(hotkey.id(), btn.id);
                        self.registered_button_hotkeys.push(hotkey);
                    }
                    Err(e) => {
                        log::warn!("could not register hotkey '{hk_str}' for '{}': {e}", btn.label);
                    }
                }
            }
        }
    }

    fn play_button_by_id(&self, id: Uuid) {
        for tab in &self.state.tabs {
            if let Some(btn) = tab.buttons.iter().find(|b| b.id == id) {
                if btn.file.is_empty() || btn.missing_file {
                    return;
                }
                let volume = (btn.volume * tab.volume).clamp(0.0, 2.0);
                let semitones = crate::audio::pitch::ratio_to_semitones(btn.pitch)
                    + crate::audio::pitch::ratio_to_semitones(tab.pitch);
                self.play(board::PlayRequest { file: btn.file.clone(), volume, pitch_semitones: semitones });
                return;
            }
        }
    }

    fn stop_all(&self) {
        if let Some(engine) = &self.engine {
            engine.stop_all();
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.dirty_since = Some(Instant::now());
    }

    fn maybe_save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(since) = self.dirty_since else { return };
        if since.elapsed() < SAVE_DEBOUNCE {
            return;
        }
        if let Err(e) = persistence::save_state(&self.state) {
            log::error!("failed to save state: {e:#}");
            self.toasts.error(format!("Save failed: {e}"));
        }
        self.dirty = false;
        self.dirty_since = None;
    }

    fn play(&self, req: board::PlayRequest) {
        if req.file.is_empty() {
            return;
        }
        if let Some(engine) = &self.engine {
            engine.play(PlayJob {
                path: PathBuf::from(req.file),
                volume: req.volume,
                pitch_semitones: req.pitch_semitones,
            });
        }
    }

    fn new_tab(&mut self, name: Option<String>) {
        let name = name.unwrap_or_else(|| format!("Tab {}", self.state.tabs.len() + 1));
        let mut tab = TabModel::new(name);
        tab.button_size = self.state.default_button_size;
        self.state.tabs.push(tab);
        self.state.current_tab = self.state.tabs.len() - 1;
        self.mark_dirty();
    }

    fn apply_tabs_action(&mut self, action: tabs::TabsAction) {
        if let Some(i) = action.switch_to {
            self.state.current_tab = i;
        }
        if action.new_tab {
            self.new_tab(None);
        }
        if let Some(i) = action.delete {
            if self.state.tabs.len() > 1 && i < self.state.tabs.len() {
                self.state.tabs.remove(i);
                if self.state.current_tab >= self.state.tabs.len() {
                    self.state.current_tab = self.state.tabs.len() - 1;
                }
                self.resync_button_hotkeys();
                self.mark_dirty();
            }
        }
        if let Some((from, to)) = action.reorder {
            if self.state.current_tab == from {
                self.state.current_tab = to;
            } else if self.state.current_tab == to {
                self.state.current_tab = from;
            }
            self.mark_dirty();
        }
        if action.changed {
            self.mark_dirty();
        }
    }

    fn add_files_to_current_tab(&mut self, files: Vec<PathBuf>) {
        if files.is_empty() {
            return;
        }
        let size = self.state.default_button_size;
        if self.state.tabs.is_empty() {
            self.new_tab(None);
        }
        let idx = self.state.current_tab.min(self.state.tabs.len() - 1);
        let tab = &mut self.state.tabs[idx];
        let btn_size = if tab.button_size > 0 { tab.button_size } else { size };

        let mut existing: std::collections::HashSet<String> =
            tab.buttons.iter().map(|b| b.file.clone()).collect();

        let mut changed = false;
        for f in files {
            let normalized = crate::model::button::normalize_path_string(&f.to_string_lossy());
            if existing.contains(&normalized) {
                continue;
            }
            let label = util::sanitize_label(f.file_name().and_then(|s| s.to_str()).unwrap_or("Sound"));
            let mut btn = ButtonModel::new(label, normalized.clone());
            btn.width = btn_size;
            btn.height = btn_size;
            existing.insert(normalized);
            tab.buttons.push(btn);
            changed = true;
        }
        if changed {
            self.mark_dirty();
        }
    }

    fn add_tab_from_folder(&mut self, folder: &Path) {
        let audio_files = util::iter_audio_files(folder);
        if audio_files.is_empty() {
            return;
        }
        let name = folder.file_name().and_then(|s| s.to_str()).unwrap_or("Imported").to_string();
        let mut tab = TabModel::new(name);
        tab.button_size = self.state.default_button_size;
        for f in audio_files {
            let normalized = crate::model::button::normalize_path_string(&f.to_string_lossy());
            let label = util::sanitize_label(f.file_name().and_then(|s| s.to_str()).unwrap_or("Sound"));
            let mut btn = ButtonModel::new(label, normalized);
            btn.width = tab.button_size;
            btn.height = tab.button_size;
            tab.buttons.push(btn);
        }
        self.state.tabs.push(tab);
        self.state.current_tab = self.state.tabs.len() - 1;
        self.mark_dirty();
    }

    fn import_zip_path(&mut self, path: &Path) {
        match persistence::import::import_zip_auto(path) {
            Ok(new_tabs) => {
                if new_tabs.is_empty() {
                    self.toasts.warning("Zip contained no importable sounds");
                    return;
                }
                let count: usize = new_tabs.iter().map(|t| t.buttons.len()).sum();
                for mut t in new_tabs {
                    t.button_size = if t.button_size > 0 { t.button_size } else { self.state.default_button_size };
                    t.normalize();
                    self.state.tabs.push(t);
                }
                self.state.current_tab = self.state.tabs.len() - 1;
                self.resync_button_hotkeys();
                self.mark_dirty();
                self.toasts.success(format!("Imported {count} sound{}", if count == 1 { "" } else { "s" }));
            }
            Err(e) => {
                log::error!("import failed: {e:#}");
                self.toasts.error(format!("Import failed: {e}"));
            }
        }
    }

    fn export_zip(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Soundboard zip", &["zip"])
            .set_file_name("soundboard.zip")
            .save_file()
        {
            match persistence::export::export_soundboard(&self.state, &path) {
                Ok(()) => self.toasts.success("Exported soundboard"),
                Err(e) => {
                    log::error!("export failed: {e:#}");
                    self.toasts.error(format!("Export failed: {e}"));
                }
            }
        }
    }

    fn import_zip_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Soundboard zip", &["zip"]).pick_file() {
            self.import_zip_path(&path);
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }

        let mut folders = Vec::new();
        let mut files = Vec::new();
        let mut zips = Vec::new();
        let mut swfs = Vec::new();

        for f in dropped {
            let path = f.path().to_path_buf();
            let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
            if path.is_dir() {
                folders.push(path);
            } else if ext.as_deref() == Some("zip") {
                zips.push(path);
            } else if ext.as_deref() == Some("swf") {
                swfs.push(path);
            } else if util::is_audio_file(&path) {
                files.push(path);
            }
        }

        for folder in folders {
            self.add_tab_from_folder(&folder);
        }
        for zip in &zips {
            self.import_zip_path(zip);
        }
        for swf in swfs {
            self.start_swf_rip(swf);
        }
        self.add_files_to_current_tab(files);
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui.button("➕ New Tab").clicked() {
                    self.new_tab(None);
                }
                ui.separator();
                if ui.button("📂 Import").on_hover_text("Import a soundboard .zip").clicked() {
                    self.import_zip_dialog();
                }
                if ui.button("💾 Export").on_hover_text("Export this soundboard as a .zip").clicked() {
                    self.export_zip();
                }
                if ui.button("🔗 Clone URL").on_hover_text("Clone a soundboard from a URL").clicked() {
                    self.clone_dialog_open = true;
                }
                ui.separator();
                if ui.button("⚙ Settings").clicked() {
                    self.settings_open = true;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(err) = &self.engine_error {
                        ui.label(RichText::new(format!("Audio unavailable: {err}")).color(super::theme::palette::RED));
                    } else if !self.space_hotkey_is_global {
                        ui.label(
                            RichText::new("Space stops sounds only while this window is focused (no system-wide hotkey support here)")
                                .color(super::theme::palette::SUBTEXT)
                                .small(),
                        );
                    }

                    ui.add_space(8.0);
                    let mute_icon = if self.state.muted { "🔇" } else { "🔊" };
                    if ui.button(mute_icon).on_hover_text("Mute / unmute").clicked() {
                        self.state.muted = !self.state.muted;
                        self.apply_master_gain();
                        self.mark_dirty();
                    }
                    let mut volume = self.state.master_volume;
                    let slider = egui::Slider::new(&mut volume, 0.0..=2.0).show_value(false);
                    if ui.add_sized([90.0, 18.0], slider).changed() {
                        self.state.master_volume = volume;
                        self.apply_master_gain();
                        self.mark_dirty();
                    }
                    ui.label(RichText::new("Master").color(super::theme::palette::SUBTEXT).small());
                });
            });

            ui.add_space(4.0);
            let action = tabs::show(ui, &mut self.state.tabs, self.state.current_tab, &mut self.renaming_tab);
            self.apply_tabs_action(action);
        });
    }
}

impl eframe::App for SoundboardApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            if Some(event.id) == self.space_hotkey_id {
                self.stop_all();
            } else if let Some(button_id) = self.button_hotkeys.get(&event.id).copied() {
                self.play_button_by_id(button_id);
            }
        }

        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.stop_all();
        }

        self.top_bar(ui);

        if self.settings_open {
            let current_device_name = self.engine.as_ref().and_then(|e| e.device_name.clone());
            let mut open = self.settings_open;
            let result = settings::show(&ctx, &mut open, &mut self.state, current_device_name.as_deref());
            self.settings_open = open;

            if let Some(device) = result.device_selected {
                self.switch_output_device(device);
            }
            if result.wallpaper_changed {
                self.wallpaper_texture = None;
            }
            if result.always_on_top_changed {
                let level = if self.state.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
                ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
            }
            if result.changed {
                self.mark_dirty();
            }
        }

        self.ensure_wallpaper_texture(&ctx);
        self.poll_clone_job(&ctx);
        self.clone_dialog(&ctx);

        let mut hotkeys_changed = false;

        let playing = self.engine.as_ref().map(|e| e.playing_paths()).unwrap_or_default();
        if !playing.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        egui::CentralPanel::default().show(ui, |ui| {
            if let Some((_, tex)) = &self.wallpaper_texture {
                let rect = ui.max_rect();
                ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::from_white_alpha(50),
                );
            }

            if self.state.tabs.is_empty() {
                self.new_tab(None);
            }
            let idx = self.state.current_tab.min(self.state.tabs.len().saturating_sub(1));
            if let Some(tab) = self.state.tabs.get_mut(idx) {
                let result = board::show(ui, tab, &playing);
                if let Some(play) = result.play {
                    self.play(play);
                }
                if result.stop_all {
                    self.stop_all();
                }
                if result.changed {
                    self.mark_dirty();
                }
                hotkeys_changed = result.hotkeys_changed;
            }
        });

        if hotkeys_changed {
            self.resync_button_hotkeys();
        }

        self.handle_drops(&ctx);
        self.poll_swf_job();
        self.toasts.show(&ctx);
        self.maybe_save();

        if self.dirty {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty {
            let _ = persistence::save_state(&self.state);
        }
    }
}

/// Registers a global (system-wide) Space hotkey so Stop-All works even
/// while another app (Discord, a softphone) has focus. This only works on
/// Windows, macOS, and X11 -- the `global-hotkey` crate has no Wayland
/// backend, since Wayland's security model doesn't let apps observe key
/// events while unfocused. When it's unavailable we fall back to the
/// in-window binding above and tell the user why in the top bar.
fn setup_global_space_hotkey() -> (Option<GlobalHotKeyManager>, Option<u32>, bool) {
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            log::warn!("global hotkey manager unavailable ({e}); Space will only work while focused");
            return (None, None, false);
        }
    };

    let hotkey = HotKey::new(None, Code::Space);
    let id = hotkey.id();
    match manager.register(hotkey) {
        Ok(()) => (Some(manager), Some(id), true),
        Err(e) => {
            log::warn!("could not register global Space hotkey ({e}); Space will only work while focused");
            (None, None, false)
        }
    }
}
