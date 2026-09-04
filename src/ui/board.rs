use std::collections::HashSet;

use egui::{FontId, Pos2, Rect, RichText, ScrollArea, Sense, Slider, Ui, Vec2};

use crate::model::{ButtonModel, LayoutMode, TabModel};

use super::sound_button;

pub struct PlayRequest {
    pub file: String,
    pub volume: f32,
    pub pitch_semitones: f32,
}

#[derive(Default)]
pub struct BoardResult {
    pub play: Option<PlayRequest>,
    pub changed: bool,
    pub hotkeys_changed: bool,
    pub delete: Option<uuid::Uuid>,
    pub stop_all: bool,
}

/// Renders one tab's controls row + button board (grid or free-form,
/// depending on `tab.layout_mode`/`tab.edit_mode`) and handles
/// click-to-play plus edit-mode drag/resize. Direct port of the layout
/// rules in `soundboard_tab.py`: grid auto-layout by default, switching
/// permanently to absolute positioning the moment edit mode is turned on
/// (so a later grid relayout never wipes out manually placed buttons).
pub fn show(ui: &mut Ui, tab: &mut TabModel, playing: &HashSet<String>) -> BoardResult {
    let mut result = BoardResult::default();

    ui.horizontal(|ui| {
        ui.label("Vol");
        if ui.add(Slider::new(&mut tab.volume, 0.0..=2.0).show_value(false).step_by(0.01)).changed() {
            result.changed = true;
        }

        ui.add_space(8.0);
        ui.label("Pitch");
        if ui.add(Slider::new(&mut tab.pitch, 0.25..=4.0).show_value(false).step_by(0.01)).changed() {
            result.changed = true;
        }

        ui.add_space(8.0);
        ui.label("Size");
        if ui.add(Slider::new(&mut tab.button_size, 48..=200)).changed() {
            result.changed = true;
        }

        ui.add_space(8.0);
        ui.label("Gap");
        if ui.add(Slider::new(&mut tab.grid_spacing, 0..=40)).changed() {
            result.changed = true;
        }

        ui.add_space(8.0);
        ui.label("Snap");
        if ui.add(Slider::new(&mut tab.snap_size, 0..=64)).changed() {
            result.changed = true;
        }

        ui.add_space(12.0);
        let was_edit = tab.edit_mode;
        ui.toggle_value(&mut tab.edit_mode, "Edit");
        if tab.edit_mode && !was_edit && tab.layout_mode == LayoutMode::Grid {
            capture_grid_positions(tab);
            tab.layout_mode = LayoutMode::Absolute;
            result.changed = true;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("STOP").clicked() {
                result.stop_all = true;
            }
        });
    });

    ui.separator();

    let show_grid = tab.layout_mode == LayoutMode::Grid && !tab.edit_mode;

    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if tab.buttons.is_empty() {
            show_empty_state(ui);
        } else if show_grid {
            show_grid_layout(ui, tab, playing, &mut result);
        } else {
            show_absolute_layout(ui, tab, playing, &mut result);
        }
    });

    if let Some(id) = result.delete {
        tab.buttons.retain(|b| b.id != id);
        result.changed = true;
    }

    result
}

fn show_empty_state(ui: &mut Ui) {
    let avail = ui.available_size().max(Vec2::new(200.0, 160.0));
    ui.allocate_ui(avail, |ui| {
        ui.with_layout(egui::Layout::centered_and_justified(egui::Direction::TopDown), |ui| {
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("🎧").font(FontId::proportional(40.0)));
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Drag audio files, a folder, a .zip pack, or a .swf soundboard here to get started")
                        .color(super::theme::palette::SUBTEXT),
                );
            });
        });
    });
}

fn show_grid_layout(ui: &mut Ui, tab: &mut TabModel, playing: &HashSet<String>, result: &mut BoardResult) {
    let size = tab.button_size as f32;
    let spacing = tab.grid_spacing as f32;
    let avail_width = ui.available_width().max(size);
    let columns = (((avail_width + spacing) / (size + spacing)).floor() as usize).max(1);
    tab.last_grid_columns = columns;

    let rows = tab.buttons.len().div_ceil(columns).max(1);
    let total_height = rows as f32 * (size + spacing) + spacing;

    let (rect, _resp) = ui.allocate_exact_size(Vec2::new(avail_width, total_height), Sense::hover());
    let origin = rect.min;

    for (i, btn) in tab.buttons.iter_mut().enumerate() {
        let row = i / columns;
        let col = i % columns;
        let x = origin.x + spacing + col as f32 * (size + spacing);
        let y = origin.y + spacing + row as f32 * (size + spacing);
        let btn_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(size));

        apply_button(ui, btn_rect, btn, false, tab.volume, tab.pitch, playing, result);
    }
}

fn show_absolute_layout(ui: &mut Ui, tab: &mut TabModel, playing: &HashSet<String>, result: &mut BoardResult) {
    let avail = ui.available_size();
    let max_x = tab
        .buttons
        .iter()
        .map(|b| (b.x + b.width as i32) as f32)
        .fold(avail.x, f32::max);
    let max_y = tab
        .buttons
        .iter()
        .map(|b| (b.y + b.height as i32) as f32)
        .fold(avail.y, f32::max);

    let (rect, _resp) = ui.allocate_exact_size(Vec2::new(max_x, max_y), Sense::hover());
    let origin = rect.min;
    let edit_mode = tab.edit_mode;
    let snap = tab.snap_size;

    for btn in tab.buttons.iter_mut() {
        if edit_mode {
            snap_button(btn, snap);
        }
        let btn_rect = Rect::from_min_size(
            origin + Vec2::new(btn.x as f32, btn.y as f32),
            Vec2::new(btn.width as f32, btn.height as f32),
        );
        apply_button(ui, btn_rect, btn, edit_mode, tab.volume, tab.pitch, playing, result);
    }
}

fn apply_button(
    ui: &mut Ui,
    rect: Rect,
    btn: &mut ButtonModel,
    edit_mode: bool,
    tab_volume: f32,
    tab_pitch: f32,
    playing: &HashSet<String>,
    result: &mut BoardResult,
) {
    let is_playing = !btn.file.is_empty() && playing.contains(&btn.file);
    let r = sound_button::show(ui, rect, btn, edit_mode, is_playing);
    if r.play {
        let volume = (btn.volume * tab_volume).clamp(0.0, 2.0);
        let semitones = crate::audio::pitch::ratio_to_semitones(btn.pitch)
            + crate::audio::pitch::ratio_to_semitones(tab_pitch);
        result.play = Some(PlayRequest { file: btn.file.clone(), volume, pitch_semitones: semitones });
    }
    if r.geometry_changed || r.changed {
        result.changed = true;
    }
    if r.hotkey_changed {
        result.hotkeys_changed = true;
    }
    if r.delete_requested {
        result.delete = Some(btn.id);
    }
}

fn snap_button(btn: &mut ButtonModel, snap: u32) {
    if snap == 0 {
        return;
    }
    let snap = snap as i32;
    btn.x = ((btn.x as f32 / snap as f32).round() as i32) * snap;
    btn.y = ((btn.y as f32 / snap as f32).round() as i32) * snap;
}

/// Freezes the current grid cell positions into `x`/`y` before switching a
/// tab from grid to absolute layout, so buttons don't jump when the user
/// first enables Edit mode.
fn capture_grid_positions(tab: &mut TabModel) {
    let size = tab.button_size as f32;
    let spacing = tab.grid_spacing as f32;
    let columns = tab.last_grid_columns.max(1);
    let button_size = tab.button_size;
    let buttons = &mut tab.buttons;
    for (i, btn) in buttons.iter_mut().enumerate() {
        if btn.x != 20 || btn.y != 20 {
            continue; // already has a real position from a previous edit session
        }
        let row = i / columns;
        let col = i % columns;
        btn.x = (spacing + col as f32 * (size + spacing)) as i32;
        btn.y = (spacing + row as f32 * (size + spacing)) as i32;
        btn.width = button_size;
        btn.height = button_size;
    }
}
