use egui::{Align2, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::model::button::MIN_BUTTON_SIZE;
use crate::model::ButtonModel;

use super::color;
use super::emoji_picker;

const RESIZE_HANDLE: f32 = 16.0;

#[derive(Default)]
pub struct SoundButtonResult {
    /// User single-clicked the button outside edit mode -- play it.
    pub play: bool,
    /// Geometry changed and the drag/resize just ended -- caller should persist.
    pub geometry_changed: bool,
    /// A field other than geometry/hotkey changed (rename/color/emoji/image/volume/pitch) -- persist.
    pub changed: bool,
    /// The hotkey text changed -- caller should re-sync global hotkey registrations.
    pub hotkey_changed: bool,
    /// User chose Delete from the context menu.
    pub delete_requested: bool,
}

/// Draws one sound button at `rect` and handles click-to-play (normal mode),
/// drag-to-move / drag-the-corner-to-resize (edit mode), and the right-click
/// context menu (rename, color, emoji, image, per-button volume/pitch,
/// hotkey, delete) -- mirrors `sound_button.py`'s behavior. `is_playing`
/// draws a pulsing glow ring while the button's file has an active voice.
pub fn show(ui: &mut Ui, rect: Rect, model: &mut ButtonModel, edit_mode: bool, is_playing: bool) -> SoundButtonResult {
    let mut result = SoundButtonResult::default();

    let id = Id::new(("sound_button", model.id));
    let response = ui.interact(rect, id, Sense::click_and_drag());

    let resize_zone = Rect::from_min_size(
        Pos2::new(rect.right() - RESIZE_HANDLE, rect.bottom() - RESIZE_HANDLE),
        Vec2::splat(RESIZE_HANDLE),
    );

    let resizing_id = id.with("resizing");

    if edit_mode && response.drag_started() {
        let starting_in_resize = response
            .interact_pointer_pos()
            .map(|p| resize_zone.contains(p))
            .unwrap_or(false);
        ui.memory_mut(|m| m.data.insert_temp(resizing_id, starting_in_resize));
    }

    if edit_mode && response.dragged() {
        let is_resizing = ui.memory_mut(|m| m.data.get_temp::<bool>(resizing_id).unwrap_or(false));
        let delta = response.drag_delta();
        if is_resizing {
            model.width = ((model.width as f32) + delta.x).max(MIN_BUTTON_SIZE as f32) as u32;
            model.height = ((model.height as f32) + delta.y).max(MIN_BUTTON_SIZE as f32) as u32;
        } else {
            model.x += delta.x as i32;
            model.y += delta.y as i32;
        }
    }

    if edit_mode && response.drag_stopped() {
        result.geometry_changed = true;
    }

    if !edit_mode && response.clicked() {
        result.play = true;
    }

    // ---- painting ----
    let painter = ui.painter_at(rect);

    if is_playing {
        let t = ui.input(|i| i.time) as f32;
        let pulse = 0.55 + 0.45 * (t * 4.0).sin().max(0.0);
        let glow = super::theme::palette::MAUVE.gamma_multiply(pulse);
        painter.rect_stroke(rect.expand(3.0), 9.0, Stroke::new(3.0, glow), StrokeKind::Outside);
        ui.ctx().request_repaint();
    }

    let bg = if let Some(c) = model.color.as_deref().and_then(color::parse_hex) {
        c
    } else {
        super::theme::palette::SURFACE0
    };
    let bg = if model.missing_file {
        tint_toward(bg, Color32::from_rgb(90, 30, 30), 0.55)
    } else if response.hovered() && !edit_mode {
        tint_toward(bg, Color32::WHITE, 0.08)
    } else {
        bg
    };

    let border = if edit_mode {
        Stroke::new(2.0, super::theme::palette::LAVENDER)
    } else if response.hovered() {
        Stroke::new(1.5, super::theme::palette::SURFACE2)
    } else {
        Stroke::new(1.0, super::theme::palette::SURFACE1)
    };

    painter.rect(rect, 8.0, bg, border, StrokeKind::Inside);

    if edit_mode {
        painter.rect(
            resize_zone.shrink(3.0),
            2.0,
            super::theme::palette::LAVENDER,
            Stroke::NONE,
            StrokeKind::Inside,
        );
    }

    let text_color = if model.missing_file {
        super::theme::palette::SUBTEXT.gamma_multiply(0.6)
    } else {
        super::theme::palette::TEXT
    };
    let mut cursor_y = rect.top() + 8.0;

    if let Some(emoji) = model.emoji.as_deref().filter(|e| !e.is_empty()) {
        painter.text(Pos2::new(rect.center().x, cursor_y), Align2::CENTER_TOP, emoji, FontId::proportional(20.0), text_color);
        cursor_y += 24.0;
    }

    let label_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 4.0, cursor_y),
        Pos2::new(rect.right() - 4.0, rect.bottom() - 6.0),
    );
    painter.text(label_rect.center(), Align2::CENTER_CENTER, &model.label, FontId::proportional(13.0), text_color);

    if model.missing_file {
        response.clone().on_hover_text("Audio file missing. Re-link or re-import.");
    } else if edit_mode {
        response.clone().on_hover_text("Drag to move, drag the corner to resize. Right-click for options.");
    } else {
        response.clone().on_hover_text(model.label.clone());
    }

    show_context_menu(&response, id, model, &mut result);

    result
}

fn show_context_menu(response: &egui::Response, id: Id, model: &mut ButtonModel, result: &mut SoundButtonResult) {
    response.context_menu(|ui| {
        ui.set_min_width(220.0);

        ui.label("Rename");
        if ui.text_edit_singleline(&mut model.label).lost_focus() {
            result.changed = true;
        }

        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Color");
            let mut c = model.color.as_deref().and_then(color::parse_hex).unwrap_or(Color32::from_gray(32));
            if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::Opaque).changed() {
                model.color = Some(color::to_hex(c));
                result.changed = true;
            }
            if ui.small_button("Clear").clicked() {
                model.color = None;
                result.changed = true;
            }
        });

        ui.menu_button(
            format!("Emoji {}", model.emoji.as_deref().unwrap_or("")),
            |ui| {
                let search_id = id.with("emoji_search");
                let mut search = ui.memory_mut(|m| m.data.get_temp::<String>(search_id).unwrap_or_default());
                if ui.text_edit_singleline(&mut search).changed() {
                    ui.memory_mut(|m| m.data.insert_temp(search_id, search.clone()));
                }
                if let Some(chosen) = emoji_picker::grid(ui, &search) {
                    model.emoji = Some(chosen.to_string());
                    result.changed = true;
                    ui.close();
                }
                if ui.small_button("Clear emoji").clicked() {
                    model.emoji = None;
                    result.changed = true;
                    ui.close();
                }
            },
        );

        ui.horizontal(|ui| {
            ui.label("Image");
            if ui.small_button("Choose…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif", "bmp"])
                    .pick_file()
                {
                    model.image = Some(crate::model::button::normalize_path_string(&path.to_string_lossy()));
                    result.changed = true;
                }
            }
            if model.image.is_some() && ui.small_button("Clear").clicked() {
                model.image = None;
                result.changed = true;
            }
        });

        ui.separator();

        ui.label("Volume");
        if ui.add(egui::Slider::new(&mut model.volume, 0.0..=2.0).show_value(true)).changed() {
            result.changed = true;
        }

        ui.label("Pitch");
        if ui.add(egui::Slider::new(&mut model.pitch, 0.25..=4.0).show_value(true)).changed() {
            result.changed = true;
        }

        ui.separator();

        ui.label("Hotkey (e.g. \"Ctrl+Shift+F1\")");
        let mut hotkey_text = model.hotkey.clone().unwrap_or_default();
        let resp = ui.text_edit_singleline(&mut hotkey_text);
        if resp.lost_focus() {
            let trimmed = hotkey_text.trim();
            let new_value = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
            if new_value != model.hotkey {
                model.hotkey = new_value;
                result.hotkey_changed = true;
                result.changed = true;
            }
        }
        if hotkey_text.trim().is_empty() {
            ui.small(" ");
        } else if hotkey_text.trim().parse::<global_hotkey::hotkey::HotKey>().is_err() {
            ui.colored_label(Color32::LIGHT_RED, "Not recognized -- try \"Ctrl+F1\" or \"Shift+A\"");
        }

        ui.separator();

        if ui.button(egui::RichText::new("Delete").color(Color32::LIGHT_RED)).clicked() {
            result.delete_requested = true;
            ui.close();
        }
    });
}

fn tint_toward(base: Color32, target: Color32, amount: f32) -> Color32 {
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount) as u8;
    Color32::from_rgb(lerp(base.r(), target.r()), lerp(base.g(), target.g()), lerp(base.b(), target.b()))
}
