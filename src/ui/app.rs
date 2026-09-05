use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::RichText;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use uuid::Uuid;

use crate::audio::{AudioEngine, PlayJob};
use crate::importers;
use crate::importers::swf_rip::SwfRipResult;
use crate::model::{AppState, ButtonModel, TabModel};
use crate::persistence;
use crate::util;

use super::{board, meter, settings, tabs, toast};

enum CloneMsg {
    Progress(String),
    Done(Result<importers::ImportResult, String>),
}

struct CloneJob {
    rx: crossbeam_channel::Receiver<CloneMsg>,
    running: Arc<AtomicBool>,
    log: Vec<String>,
}

const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// The last destructive action, kept so it can be reversed.
///
/// Deleting a tab used to take every button in it with no confirmation
/// and no way back -- on a scraped board that's a hundred-plus sounds
/// gone, recoverable only by re-scraping. Single-level (only the most
/// recent action) on purpose: it covers the misclick this exists for
/// without turning board state into an undo stack.
enum UndoAction {
    Tab { index: usize, tab: TabModel },
    Button { tab_id: Uuid, index: usize, button: ButtonModel },
}

pub struct SoundboardApp {
    state: AppState,
    engine: Option<AudioEngine>,
    engine_error: Option<String>,
    renaming_tab: Option<tabs::TabRename>,
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
    /// Which tab's sounds were last sent to the decode cache for
    /// background pre-warming (see `AudioEngine::prewarm`). Re-checked
    /// every frame against the current tab so switching tabs (or editing
    /// the current one) queues a fresh warm-up without repeating it
    /// every single frame.
    prewarmed_tab: Option<uuid::Uuid>,
    prewarmed_button_count: usize,
    /// Pitch the current tab was last warmed at. Changing pitch produces
    /// a different rendering, so the warm-up has to be redone or the next
    /// click pays the vocoder pass it was supposed to have avoided.
    prewarmed_pitch: i32,
    level_meter: meter::LevelMeter,
    last_meter_update: Option<Instant>,
    screenshot_frames: u32,
    hotkeys_panel_open: bool,
    /// Live search text for the current board. Not persisted -- a filter
    /// that survived a restart would look like sounds had gone missing.
    search_filter: String,
    /// Set for one frame to move keyboard focus into the search field.
    focus_search: bool,
    undo: Option<UndoAction>,
    /// Index of a tab awaiting delete confirmation.
    pending_tab_delete: Option<usize>,
    /// Whether the persisted `always_on_top` setting has been pushed to
    /// the viewport yet. It can't be sent from `new()` -- there's no
    /// viewport to command until the first frame -- and until this was
    /// tracked, the level was only ever sent when the checkbox changed,
    /// so the setting silently didn't survive a restart.
    applied_startup_window_level: bool,
}

impl SoundboardApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        super::theme::apply(&cc.egui_ctx);
        super::fonts::install(&cc.egui_ctx);

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
            prewarmed_tab: None,
            prewarmed_button_count: 0,
            prewarmed_pitch: 0,
            level_meter: meter::LevelMeter::default(),
            last_meter_update: None,
            screenshot_frames: 0,
            hotkeys_panel_open: false,
            search_filter: String::new(),
            focus_search: false,
            undo: None,
            pending_tab_delete: None,
            applied_startup_window_level: false,
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

        let (tx, rx) = crossbeam_channel::unbounded();
        let running = Arc::new(AtomicBool::new(true));
        let running_bg = running.clone();

        std::thread::spawn(move || {
            let tx_progress = tx.clone();
            // Sync (unlike std::sync::mpsc::Sender) so importers can call
            // this concurrently from a worker-thread pool -- see
            // `realm_of_darkness.rs`'s parallel downloads.
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

    /// Sends the current tab's button files to the decode cache in the
    /// background whenever the visible tab changes (or gains/loses
    /// buttons), so clicking a button is a cache hit instead of a cold
    /// decode. Cheap to call every frame -- it's a no-op unless something
    /// actually changed.
    fn prewarm_current_tab(&mut self) {
        let Some(tab) = self.state.tabs.get(self.state.current_tab) else { return };
        let pitch_key = (tab.pitch * 100.0).round() as i32;
        if self.prewarmed_tab == Some(tab.id)
            && self.prewarmed_button_count == tab.buttons.len()
            && self.prewarmed_pitch == pitch_key
        {
            return;
        }
        self.prewarmed_tab = Some(tab.id);
        self.prewarmed_button_count = tab.buttons.len();
        self.prewarmed_pitch = pitch_key;

        if let Some(engine) = &self.engine {
            let tab_semitones = crate::audio::pitch::ratio_to_semitones(tab.pitch);
            let jobs = tab
                .buttons
                .iter()
                .filter(|b| !b.missing_file && !b.file.is_empty())
                .map(|b| {
                    let semitones =
                        crate::audio::pitch::ratio_to_semitones(b.pitch) + tab_semitones;
                    (PathBuf::from(&b.file), semitones)
                });
            engine.prewarm(jobs);
        }
    }

    /// Visual "drop here" feedback while something is being dragged over
    /// the window. Doubles as a diagnostic: if a file being dragged in
    /// from elsewhere never makes this overlay appear, the OS/toolkit
    /// never told egui about the drag at all (nothing this app's code can
    /// fix -- most likely the source app not offering a real file via
    /// native drag-and-drop, e.g. some Electron apps on Linux). If the
    /// overlay does appear but nothing happens on release, that's a
    /// narrower, fixable bug in `handle_drops`.
    fn show_drag_hover_overlay(&self, ctx: &egui::Context) {
        let count = ctx.input(|i| i.raw.hovered_files.len());
        if count == 0 {
            return;
        }
        egui::Area::new(egui::Id::new("drag_hover_overlay"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(super::theme::palette::LAVENDER.gamma_multiply(0.9))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.colored_label(
                            super::theme::palette::CRUST,
                            format!("Drop {count} file{} to add", if count == 1 { "" } else { "s" }),
                        );
                    });
            });
    }

    /// Dev/QA affordance: with `ULTIMATE_SOUNDBOARD_SCREENSHOT=<path>`
    /// set, the app captures its own framebuffer a few frames in and
    /// writes it out, then exits. Worth having as a real feature rather
    /// than a throwaway: egui screenshotting itself works on any
    /// compositor, whereas external X11 grabbers capture pure black for
    /// anything composited by Wayland, which makes "does this actually
    /// look right" otherwise unanswerable on a modern Linux desktop.
    fn handle_screenshot_hook(&mut self, ctx: &egui::Context) {
        let Ok(path) = std::env::var("ULTIMATE_SOUNDBOARD_SCREENSHOT") else { return };

        self.screenshot_frames += 1;
        ctx.request_repaint();

        // Give layout, fonts and the first prewarm a moment to settle.
        if self.screenshot_frames == 30 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }

        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });

        if let Some(image) = shot {
            let [w, h] = image.size;
            let pixels: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, pixels) {
                Some(buf) => match buf.save(&path) {
                    Ok(()) => log::info!("wrote screenshot to {path} ({w}x{h})"),
                    Err(e) => log::error!("could not write screenshot to {path}: {e}"),
                },
                None => log::error!("screenshot buffer had unexpected size"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Confirmation for deleting a tab, naming what's about to go.
    fn tab_delete_dialog(&mut self, ctx: &egui::Context) {
        let Some(index) = self.pending_tab_delete else { return };
        let Some(tab) = self.state.tabs.get(index) else {
            self.pending_tab_delete = None;
            return;
        };

        let name = tab.name.clone();
        let count = tab.buttons.len();
        let mut confirmed = false;
        let mut cancelled = false;

        egui::Modal::new(egui::Id::new("confirm_tab_delete")).show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.heading("Delete tab?");
            ui.add_space(6.0);
            ui.label(match count {
                0 => format!("\"{name}\" is empty."),
                1 => format!("\"{name}\" holds 1 sound."),
                n => format!("\"{name}\" holds {n} sounds."),
            });
            ui.label(
                RichText::new(
                    "The sound files on disk are left alone -- only this tab's buttons are removed.",
                )
                .small()
                .color(super::theme::palette::SUBTEXT),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancelled = true;
                }
                if ui
                    .button(RichText::new("Delete tab").color(super::theme::palette::RED))
                    .clicked()
                {
                    confirmed = true;
                }
            });
        });

        if cancelled {
            self.pending_tab_delete = None;
        }
        if confirmed {
            self.pending_tab_delete = None;
            self.delete_tab(index);
        }
    }

    fn delete_tab(&mut self, index: usize) {
        if self.state.tabs.len() <= 1 || index >= self.state.tabs.len() {
            return;
        }
        let tab = self.state.tabs.remove(index);
        let name = tab.name.clone();
        if self.state.current_tab >= self.state.tabs.len() {
            self.state.current_tab = self.state.tabs.len() - 1;
        }
        self.undo = Some(UndoAction::Tab { index, tab });
        self.toasts.undoable(format!("Deleted tab \"{name}\""));
        self.resync_button_hotkeys();
        self.mark_dirty();
    }

    /// Puts back whatever the last destructive action removed.
    fn apply_undo(&mut self) {
        match self.undo.take() {
            Some(UndoAction::Tab { index, tab }) => {
                let name = tab.name.clone();
                let at = index.min(self.state.tabs.len());
                self.state.tabs.insert(at, tab);
                self.state.current_tab = at;
                self.resync_button_hotkeys();
                self.mark_dirty();
                self.toasts.success(format!("Restored tab \"{name}\""));
            }
            Some(UndoAction::Button { tab_id, index, button }) => {
                let label = button.label.clone();
                // Found by id rather than index: tabs can be reordered
                // between the delete and the undo.
                if let Some(tab) = self.state.tabs.iter_mut().find(|t| t.id == tab_id) {
                    let at = index.min(tab.buttons.len());
                    tab.buttons.insert(at, button);
                    self.resync_button_hotkeys();
                    self.mark_dirty();
                    self.toasts.success(format!("Restored \"{label}\""));
                } else {
                    self.toasts.warning(format!("Can't restore \"{label}\" -- its tab is gone"));
                }
            }
            None => {}
        }
    }

    fn apply_window_level(&self, ctx: &egui::Context) {
        let level = if self.state.always_on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
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
                self.play(board::PlayRequest {
                    file: btn.file.clone(),
                    volume,
                    pitch_semitones: semitones,
                    button_id: btn.id,
                });
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
                button_id: req.button_id,
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
        // Deleting a tab throws away every sound in it, so it asks first
        // rather than acting on the click.
        if let Some(i) = action.delete {
            if self.state.tabs.len() > 1 && i < self.state.tabs.len() {
                self.pending_tab_delete = Some(i);
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

    /// File-picker alternative to drag-and-drop, added because drag-out
    /// from some apps (Electron-based chat clients in particular) doesn't
    /// reliably hand over a real OS file path to drop targets -- this
    /// path goes through a native "Open File" dialog instead, which is
    /// unaffected by that. Adds to the current tab, same as dropping.
    fn add_sound_files_dialog(&mut self) {
        let files = rfd::FileDialog::new().add_filter("Audio", util::AUDIO_EXTENSIONS).pick_files();
        if let Some(files) = files {
            let count = files.len();
            self.add_files_to_current_tab(files);
            if count > 0 {
                self.toasts.success(format!("Added {count} sound{}", if count == 1 { "" } else { "s" }));
            }
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
        let mut unrecognized: Vec<String> = Vec::new();

        for f in dropped {
            let path = f.path().to_path_buf();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("(unnamed)").to_string();
            let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());

            if !path.exists() {
                log::warn!("dropped item has no on-disk path (or app can't see it): {path:?}");
                unrecognized.push(format!("{name} (no readable file path -- if this came straight from Discord's window, try Save As to a folder first, then drag that file)"));
                continue;
            }

            if path.is_dir() {
                folders.push(path);
            } else if ext.as_deref() == Some("zip") {
                zips.push(path);
            } else if ext.as_deref() == Some("swf") {
                swfs.push(path);
            } else if util::is_audio_file(&path) {
                files.push(path);
            } else {
                log::warn!("dropped file not recognized as audio/zip/swf: {path:?} (ext: {ext:?})");
                unrecognized.push(name);
            }
        }

        let folders_added: usize = folders.len();
        for folder in folders {
            self.add_tab_from_folder(&folder);
        }
        for zip in &zips {
            self.import_zip_path(zip);
        }
        for swf in swfs {
            self.start_swf_rip(swf);
        }
        let files_count = files.len();
        self.add_files_to_current_tab(files);

        if files_count == 0 && zips.is_empty() && folders_added == 0 && !unrecognized.is_empty() {
            for name in unrecognized.iter().take(3) {
                self.toasts.warning(format!("Not added: {name}"));
            }
        }
    }

    /// The rack header: brand plate and grouped controls on the left,
    /// master section on the right, tab strip below.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("menu_bar").show(ui, |ui| {
            // Reserve a slot for the panel face now and fill it in once the
            // rows below have been laid out -- the header's real height
            // isn't known until then.
            let face = ui.painter().add(egui::Shape::Noop);
            let full_width = ui.max_rect().x_range();

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                self.brand_plate(ui);
                ui.add_space(10.0);

                // Grouped by what the actions do to your board, rather
                // than one undifferentiated run of buttons.
                if ui.button("➕ New Tab").clicked() {
                    self.new_tab(None);
                }
                if ui.button("🎵 Add Sound").on_hover_text("Add audio file(s) to the current tab (works when drag-and-drop doesn't -- e.g. dragging out of Discord's own window isn't reliable)").clicked() {
                    self.add_sound_files_dialog();
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
                if ui.button("⌨ Hotkeys").on_hover_text("See every button hotkey and spot conflicts").clicked() {
                    self.hotkeys_panel_open = true;
                }
                if ui.button("⚙ Settings").clicked() {
                    self.settings_open = true;
                }

                ui.separator();
                self.search_field(ui);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Keep status compact: a long sentence here competes
                    // with the master strip for width and the two end up
                    // drawn on top of each other on narrower windows.
                    if let Some(err) = &self.engine_error {
                        ui.label(RichText::new("⚠ audio").color(super::theme::palette::RED))
                            .on_hover_text(format!("Audio unavailable: {err}"));
                    } else if !self.space_hotkey_is_global
                        && ui
                            .button(RichText::new("⚠ hotkeys").color(super::theme::palette::YELLOW).small())
                            .on_hover_text(
                                "System-wide hotkeys aren't available on this session, so stop-all and per-button hotkeys only fire while this window is focused. Click to review your hotkeys.",
                            )
                            .clicked()
                    {
                        self.hotkeys_panel_open = true;
                    }
                });
            });

            ui.add_space(4.0);

            // Tabs and the master strip share a row, with each side given
            // an explicit width. Letting egui negotiate it is what made
            // the master section collide with the toolbar above.
            ui.horizontal(|ui| {
                let avail = ui.available_width();
                let master_w = super::master::WIDTH.min(avail * 0.6);
                let tabs_w = (avail - master_w).max(120.0);

                ui.allocate_ui_with_layout(
                    egui::vec2(tabs_w, super::master::HEIGHT),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        let action = tabs::show(
                            ui,
                            &mut self.state.tabs,
                            self.state.current_tab,
                            &mut self.renaming_tab,
                        );
                        self.apply_tabs_action(action);
                    },
                );

                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), super::master::HEIGHT),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        // Built right-to-left so the strip stays pinned to
                        // the window edge; `master::show` lays its own
                        // controls out in reading order within that.
                        ui.allocate_ui_with_layout(
                            egui::vec2(master_w, super::master::HEIGHT),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                let result = super::master::show(
                                    ui,
                                    &self.level_meter,
                                    &mut self.state.master_volume,
                                    &mut self.state.muted,
                                );
                                if result.changed {
                                    self.apply_master_gain();
                                    self.mark_dirty();
                                }
                                if result.stop_all {
                                    self.stop_all();
                                }
                            },
                        );
                    },
                );
            });

            ui.add_space(3.0);

            let face_rect = egui::Rect::from_x_y_ranges(full_width, ui.min_rect().y_range());
            ui.painter().set(face, super::theme::rack_panel_shape(face_rect));
        });
    }

    /// Search box for the current board. A 157-button scraped tab is not
    /// something you can scan by eye, and there was no way to find one
    /// sound short of scrolling.
    fn search_field(&mut self, ui: &mut egui::Ui) {
        let resp = ui.add_sized(
            egui::vec2(150.0, 20.0),
            egui::TextEdit::singleline(&mut self.search_filter)
                .hint_text("🔍 Find a sound")
                .desired_width(150.0),
        );

        if self.focus_search {
            self.focus_search = false;
            resp.request_focus();
        }

        // Escape clears rather than just unfocusing: a filter left applied
        // with the field unfocused looks like sounds have gone missing.
        if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.search_filter.clear();
        }

        if !self.search_filter.is_empty()
            && ui
                .small_button("✖")
                .on_hover_text("Clear the search (Esc)")
                .clicked()
        {
            self.search_filter.clear();
        }
    }

    /// Etched name plate, the way a rack unit carries its model name.
    fn brand_plate(&self, ui: &mut egui::Ui) {
        let (rect, _resp) = ui.allocate_exact_size(egui::vec2(148.0, 24.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 3.0, super::theme::palette::RECESS);
        painter.rect_stroke(
            rect,
            3.0,
            egui::Stroke::new(1.0, super::theme::palette::PANEL_SHADOW),
            egui::StrokeKind::Inside,
        );
        super::theme::engraved(
            &painter,
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "ULTIMATE SOUNDBOARD",
            egui::FontId::monospace(9.5),
            super::theme::palette::LAVENDER,
        );
    }
}

impl eframe::App for SoundboardApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        self.handle_screenshot_hook(&ctx);

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

        // Gated on nothing having keyboard focus. Without the guard, typing
        // a space into any text field -- rename, clone URL, and now the
        // search box -- stops every playing sound.
        let typing = ctx.egui_wants_keyboard_input();

        if !typing && ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.stop_all();
        }

        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            self.focus_search = true;
        }

        // Ctrl+Z as well as the toast button: the toast can be missed or
        // can expire, and this is the shortcut anyone will reach for.
        if !typing
            && self.undo.is_some()
            && ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z))
        {
            self.toasts.clear_undo();
            self.apply_undo();
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
                self.apply_window_level(&ctx);
            }
            if result.changed {
                self.mark_dirty();
            }
        }

        if self.hotkeys_panel_open {
            let mut open = self.hotkeys_panel_open;
            super::hotkeys_panel::show(&ctx, &mut open, &self.state.tabs, self.space_hotkey_is_global);
            self.hotkeys_panel_open = open;
        }

        // Deferred to the first frame rather than done in `new()`: there
        // is no viewport to send a command to until the app is running.
        if !self.applied_startup_window_level {
            self.applied_startup_window_level = true;
            if self.state.always_on_top {
                self.apply_window_level(&ctx);
            }
        }

        self.ensure_wallpaper_texture(&ctx);
        self.poll_clone_job(&ctx);
        self.clone_dialog(&ctx);

        let mut hotkeys_changed = false;

        let playing = self.engine.as_ref().map(|e| e.playing_progress()).unwrap_or_default();

        // Feed the meters with real elapsed time so their fall-off is
        // frame-rate independent, and keep repainting while either a sound
        // is playing or the meters are still settling back to silence.
        let now = Instant::now();
        let dt = self.last_meter_update.map(|t| now.duration_since(t).as_secs_f32()).unwrap_or(1.0 / 60.0);
        self.last_meter_update = Some(now);
        let peaks = self.engine.as_ref().map(|e| e.peak_levels()).unwrap_or((0.0, 0.0));
        self.level_meter.update(peaks, dt);

        if !playing.is_empty() || self.level_meter.is_active() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        self.prewarm_current_tab();
        self.show_drag_hover_overlay(&ctx);

        // Declared out here so the undo offer can be raised after the
        // panel closure has given back its borrow of `self`.
        let mut deleted_button = None;

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
            let filter = self.search_filter.clone();
            let default_button_size = self.state.default_button_size;
            // Everything the board asks for is collected while `tab` is
            // borrowed and acted on after, so the handlers below are free
            // to touch `self` again.
            let mut play_request = None;
            let mut mix_update = None;
            let mut board_changed = false;

            if let Some(tab) = self.state.tabs.get_mut(idx) {
                let result = board::show(ui, tab, &playing, &filter, default_button_size);
                play_request = result.play;
                mix_update = result.mix_update;
                board_changed = result.changed;
                hotkeys_changed = result.hotkeys_changed;

                // Removal happens here rather than in the board so the
                // deleted button and its position survive long enough to
                // be offered back as an undo.
                if let Some(id) = result.delete {
                    if let Some(pos) = tab.buttons.iter().position(|b| b.id == id) {
                        let button = tab.buttons.remove(pos);
                        deleted_button = Some((tab.id, pos, button));
                    }
                }

                if let Some(id) = result.duplicate {
                    if let Some(pos) = tab.buttons.iter().position(|b| b.id == id) {
                        let mut copy = tab.buttons[pos].clone();
                        // A fresh identity, and no hotkey: the original
                        // keeps the binding, since two buttons claiming
                        // one combo silently leaves the second dead.
                        copy.id = Uuid::new_v4();
                        copy.hotkey = None;
                        copy.x += 16;
                        copy.y += 16;
                        tab.buttons.insert(pos + 1, copy);
                        board_changed = true;
                    }
                }
            }

            if let Some(play) = play_request {
                self.play(play);
            }
            // Push slider changes onto sound that is already playing, so
            // the tile's volume and pitch act on it live instead of only
            // applying the next time it's triggered.
            if let Some(update) = mix_update {
                if let Some(engine) = &self.engine {
                    engine.set_button_gain(update.button_id, update.volume);
                    if !update.file.is_empty() {
                        engine.retune_button(
                            update.button_id,
                            PathBuf::from(update.file),
                            update.semitones,
                        );
                    }
                }
            }
            if board_changed {
                self.mark_dirty();
            }
        });

        if let Some((tab_id, index, button)) = deleted_button {
            let label = button.label.clone();
            self.undo = Some(UndoAction::Button { tab_id, index, button });
            self.toasts.undoable(format!("Deleted \"{label}\""));
            self.resync_button_hotkeys();
            self.mark_dirty();
        }

        if hotkeys_changed {
            self.resync_button_hotkeys();
        }

        self.handle_drops(&ctx);
        self.poll_swf_job();
        self.tab_delete_dialog(&ctx);

        if self.toasts.show(&ctx) {
            self.apply_undo();
        }
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
/// System-wide stop-all combo.
///
/// Deliberately *not* a bare Space. Registering an unmodified key as a
/// global hotkey takes a display-wide grab on it (an `XGrabKey` on X11),
/// which means the key stops being delivered to anyone else -- including
/// this app's own text fields, and including every other program running
/// on the desktop. That's what made it impossible to type a space when
/// renaming a tab or a tile, and it was quietly eating the spacebar
/// system-wide the whole time the app was open.
///
/// Space alone still stops everything while the window is focused; that
/// path goes through egui's normal keyboard input and grabs nothing.
pub const GLOBAL_STOP_LABEL: &str = "Ctrl+Shift+Space";

fn setup_global_space_hotkey() -> (Option<GlobalHotKeyManager>, Option<u32>, bool) {
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            log::warn!("global hotkey manager unavailable ({e}); stop-all will only work while focused");
            return (None, None, false);
        }
    };

    let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space);
    let id = hotkey.id();
    match manager.register(hotkey) {
        Ok(()) => (Some(manager), Some(id), true),
        Err(e) => {
            log::warn!(
                "could not register global {GLOBAL_STOP_LABEL} hotkey ({e}); stop-all will only work while focused"
            );
            (None, None, false)
        }
    }
}
