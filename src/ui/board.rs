use std::collections::HashMap;

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
}

/// Renders one tab's controls row + button board (grid or free-form,
/// depending on `tab.layout_mode`/`tab.edit_mode`) and handles
/// click-to-play plus edit-mode drag/resize. Direct port of the layout
/// rules in `soundboard_tab.py`: grid auto-layout by default, switching
/// permanently to absolute positioning the moment edit mode is turned on
/// (so a later grid relayout never wipes out manually placed buttons).
pub fn show(
    ui: &mut Ui,
    tab: &mut TabModel,
    playing: &HashMap<String, f32>,
    filter: &str,
) -> BoardResult {
    let mut result = BoardResult::default();

    // Recessed strip behind the per-tab controls, so they read as a
    // sub-panel cut into the unit rather than widgets floating on the
    // board. Filled in after layout, since the row's height isn't known
    // until its contents are placed.
    let well = ui.painter().add(egui::Shape::Noop);
    let well_x = ui.max_rect().x_range();

    ui.add_space(2.0);
    ui.horizontal(|ui| {
        micro_label(ui, "VOL");
        if ui.add(Slider::new(&mut tab.volume, 0.0..=2.0).show_value(false).step_by(0.01)).changed() {
            result.changed = true;
        }
        // Every slider gets a readout. Vol and Pitch used to show no
        // number at all while Size/Gap/Snap did, so the row read as
        // half-finished and there was no way to set a tab back to exactly
        // unity gain or unity pitch.
        readout(ui, format!("{:.0}%", tab.volume * 100.0));

        ui.add_space(8.0);
        micro_label(ui, "PITCH");
        if ui.add(Slider::new(&mut tab.pitch, 0.25..=4.0).show_value(false).step_by(0.01)).changed() {
            result.changed = true;
        }
        readout(ui, format!("{:.2}×", tab.pitch));

        ui.add_space(8.0);
        micro_label(ui, "SIZE");
        if ui.add(Slider::new(&mut tab.button_size, 48..=200)).changed() {
            // Grid layout derives every button's size from `tab.button_size`
            // directly, but free-form layout draws each button from its own
            // `width`/`height`. So without this the Size slider silently
            // stopped doing anything the moment a tab left grid layout --
            // which is exactly what "once size is adjusted it's stuck at
            // whatever it's set to" was. Size is a tab-wide control, so it
            // overrides any per-button resizing done by hand in Edit mode.
            let size = tab.button_size;
            for btn in tab.buttons.iter_mut() {
                btn.width = size;
                btn.height = size;
            }
            result.changed = true;
        }

        // Gap only affects the grid layout's cell padding. Turning on Edit
        // mode converts a tab to free-form (absolute) positioning, which
        // is deliberate -- it's what stops a later grid relayout from
        // wiping out hand-placed buttons, same as `soundboard_tab.py`.
        // What made that feel broken was that the conversion used to be
        // one-way and permanent: Gap went dead forever with no way back
        // and no way to tell why. It's still a conversion, but now it's
        // reversible via the Re-flow button below.
        let grid_applies = tab.layout_mode == LayoutMode::Grid;
        ui.add_space(8.0);
        micro_label(ui, "GAP");
        let gap_resp = ui.add_enabled(grid_applies, Slider::new(&mut tab.grid_spacing, 0..=40));
        if gap_resp.changed() {
            result.changed = true;
        }
        if !grid_applies {
            gap_resp.on_disabled_hover_text(
                "Only applies to grid layout. This tab switched to free-form positioning when Edit mode was turned on -- use Re-flow to go back.",
            );
            if ui
                .button("Re-flow")
                .on_hover_text(
                    "Put every button back into an auto-arranged grid and re-enable Gap. \
                     Hand-placed positions are kept, so turning Edit mode on again restores them.",
                )
                .clicked()
            {
                tab.layout_mode = LayoutMode::Grid;
                tab.edit_mode = false;
                result.changed = true;
            }
        } else if grid_applies && tab.edit_mode {
            gap_resp.on_hover_text("Applies once Edit mode is turned off.");
        }

        ui.add_space(8.0);
        micro_label(ui, "SNAP").on_hover_text("Snaps button position while dragging in Edit mode.");
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

    });
    ui.add_space(3.0);

    let well_rect = egui::Rect::from_x_y_ranges(well_x, ui.min_rect().y_range());
    ui.painter().set(
        well,
        egui::Shape::Vec(vec![
            egui::Shape::rect_filled(well_rect, 0.0, super::theme::palette::RECESS),
            egui::Shape::line_segment(
                [well_rect.left_bottom(), well_rect.right_bottom()],
                egui::Stroke::new(1.0, super::theme::palette::PANEL_HILIGHT.gamma_multiply(0.5)),
            ),
        ]),
    );

    let show_grid = tab.layout_mode == LayoutMode::Grid && !tab.edit_mode;
    let filtering = !filter.trim().is_empty();

    if filtering {
        let matches = tab.buttons.iter().filter(|b| matches_filter(b, filter)).count();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{matches} of {} sounds", tab.buttons.len()))
                    .small()
                    .color(super::theme::palette::SUBTEXT),
            );
            if matches == 0 {
                ui.label(
                    RichText::new("— nothing matches that")
                        .small()
                        .color(super::theme::palette::YELLOW),
                );
            }
        });
    }

    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if tab.buttons.is_empty() {
            show_empty_state(ui);
        } else if show_grid {
            show_grid_layout(ui, tab, playing, filter, &mut result);
        } else {
            show_absolute_layout(ui, tab, playing, filter, &mut result);
        }
    });

    // The removal itself is left to the caller: it needs the button that
    // was removed (and its position) to offer an undo, which is lost if
    // the board drops it here.

    result
}

/// Small uppercase control legend, the way controls are labelled on a
/// hardware panel.
fn micro_label(ui: &mut Ui, text: &str) -> egui::Response {
    ui.label(
        RichText::new(text)
            .monospace()
            .size(9.0)
            .color(super::theme::palette::SUBTEXT),
    )
}

/// Fixed-width numeric readout beside a slider, so the row's controls
/// don't shift horizontally as their values change width.
fn readout(ui: &mut Ui, text: String) {
    ui.add_sized(
        Vec2::new(38.0, 16.0),
        egui::Label::new(RichText::new(text).monospace().small()),
    );
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

fn show_grid_layout(
    ui: &mut Ui,
    tab: &mut TabModel,
    playing: &HashMap<String, f32>,
    filter: &str,
    result: &mut BoardResult,
) {
    let size = tab.button_size as f32;
    let spacing = tab.grid_spacing as f32;
    let avail_width = ui.available_width().max(size);
    let columns = (((avail_width + spacing) / (size + spacing)).floor() as usize).max(1);
    tab.last_grid_columns = columns;

    // Grid layout is auto-arranged anyway, so a filter can compact it --
    // non-matches are skipped entirely and the survivors close ranks.
    let visible: Vec<usize> = tab
        .buttons
        .iter()
        .enumerate()
        .filter(|(_, b)| matches_filter(b, filter))
        .map(|(i, _)| i)
        .collect();

    let rows = visible.len().div_ceil(columns).max(1);
    let total_height = rows as f32 * (size + spacing) + spacing;

    let (rect, _resp) = ui.allocate_exact_size(Vec2::new(avail_width, total_height), Sense::hover());
    let origin = rect.min;

    for (slot, &i) in visible.iter().enumerate() {
        let row = slot / columns;
        let col = slot % columns;
        let x = origin.x + spacing + col as f32 * (size + spacing);
        let y = origin.y + spacing + row as f32 * (size + spacing);
        let btn_rect = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(size));

        let (volume, pitch) = (tab.volume, tab.pitch);
        let btn = &mut tab.buttons[i];
        apply_button(ui, btn_rect, btn, false, 0, volume, pitch, playing, true, result);
    }
}

/// Case-insensitive match on the button's label and its file name. An
/// empty filter matches everything.
fn matches_filter(btn: &ButtonModel, filter: &str) -> bool {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    if btn.label.to_lowercase().contains(&needle) {
        return true;
    }
    // File name, not the whole path -- matching the path would hit every
    // button in a folder-imported tab and look like the filter is broken.
    std::path::Path::new(&btn.file)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase().contains(&needle))
        .unwrap_or(false)
}

fn show_absolute_layout(
    ui: &mut Ui,
    tab: &mut TabModel,
    playing: &HashMap<String, f32>,
    filter: &str,
    result: &mut BoardResult,
) {
    let avail = ui.available_size();
    let edit_mode = tab.edit_mode;
    let snap = tab.snap_size;

    // Canvas size must be computed from *already-snapped* positions, or a
    // snap that rounds a button outward can land it beyond the area this
    // frame allocated for the board -- which is what "snapping pushes
    // tiles off the screen" actually was: the allocation used stale,
    // pre-snap bounds. Non-negative too, so nothing can snap to a
    // negative coordinate and become unreachable off the top/left edge.
    for btn in tab.buttons.iter_mut() {
        clamp_button_position(btn);
    }

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

    let (volume, pitch) = (tab.volume, tab.pitch);
    for btn in tab.buttons.iter_mut() {
        let btn_rect = Rect::from_min_size(
            origin + Vec2::new(btn.x as f32, btn.y as f32),
            Vec2::new(btn.width as f32, btn.height as f32),
        );
        // Free-form tiles are where the user put them, so a filter dims
        // non-matches in place rather than hiding them. Compacting here
        // would rearrange a hand-built layout and leave it rearranged
        // once the filter cleared.
        let matched = matches_filter(btn, filter);
        apply_button(ui, btn_rect, btn, edit_mode, snap, volume, pitch, playing, matched, result);
    }
}

fn apply_button(
    ui: &mut Ui,
    rect: Rect,
    btn: &mut ButtonModel,
    edit_mode: bool,
    snap: u32,
    tab_volume: f32,
    tab_pitch: f32,
    playing: &HashMap<String, f32>,
    matches_filter: bool,
    result: &mut BoardResult,
) {
    let progress = if btn.file.is_empty() { None } else { playing.get(&btn.file).copied() };
    let r = sound_button::show(ui, rect, btn, edit_mode, progress, matches_filter);
    if r.play {
        let volume = (btn.volume * tab_volume).clamp(0.0, 2.0);
        let semitones = crate::audio::pitch::ratio_to_semitones(btn.pitch)
            + crate::audio::pitch::ratio_to_semitones(tab_pitch);
        result.play = Some(PlayRequest { file: btn.file.clone(), volume, pitch_semitones: semitones });
    }
    if r.geometry_changed {
        // Snap on release, not every frame -- snapping continuously while
        // dragging (the old behavior) fought the drag delta each frame
        // and the button never actually landed on a grid line, which is
        // what "snap doesn't actually snap" was.
        snap_button(btn, snap);
        clamp_button_position(btn);
        result.changed = true;
    }
    if r.changed {
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

/// Keeps a button's stored position non-negative so it can never end up
/// unreachable off the top/left edge of the board (defensive -- rounding
/// during snap should never produce this, but cheap to guarantee).
fn clamp_button_position(btn: &mut ButtonModel) {
    btn.x = btn.x.max(0);
    btn.y = btn.y.max(0);
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
