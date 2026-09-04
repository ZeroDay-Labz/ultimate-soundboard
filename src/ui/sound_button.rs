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
/// hotkey, delete) -- mirrors `sound_button.py`'s behavior. `progress` is
/// `Some(0.0..=1.0)` while the button's file has an active voice, driving
/// both the glow and the playback sweep along the bottom edge.
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    model: &mut ButtonModel,
    edit_mode: bool,
    progress: Option<f32>,
) -> SoundButtonResult {
    let is_playing = progress.is_some();
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

    // Bevel: a light top edge and dark bottom edge, the cheap trick that
    // makes a flat rectangle read as a physical, pressable key rather
    // than a coloured box.
    let pressed = response.is_pointer_button_down_on() && !edit_mode;
    let (top_edge, bottom_edge) = if pressed {
        (Color32::from_black_alpha(60), Color32::from_white_alpha(18))
    } else {
        (Color32::from_white_alpha(26), Color32::from_black_alpha(70))
    };
    painter.line_segment(
        [rect.left_top() + Vec2::new(7.0, 1.5), rect.right_top() + Vec2::new(-7.0, 1.5)],
        Stroke::new(1.5, top_edge),
    );
    painter.line_segment(
        [rect.left_bottom() + Vec2::new(7.0, -1.5), rect.right_bottom() + Vec2::new(-7.0, -1.5)],
        Stroke::new(1.5, bottom_edge),
    );

    if edit_mode {
        painter.rect(
            resize_zone.shrink(3.0),
            2.0,
            super::theme::palette::LAVENDER,
            Stroke::NONE,
            StrokeKind::Inside,
        );
    }

    // Contrast-aware rather than always-light: the color picker lets a
    // user choose any fill including near-white ones, and light-on-light
    // would make the label unreadable on their own tile.
    let text_color = if model.missing_file {
        super::theme::palette::SUBTEXT.gamma_multiply(0.6)
    } else if is_light(bg) {
        super::theme::palette::CRUST
    } else {
        super::theme::palette::TEXT
    };

    // Artwork, if the button has any. Drawn behind the label, inset and
    // dimmed enough that text stays readable on top of it. (Setting an
    // image was already possible via the context menu but never actually
    // rendered -- the Python build drew it, so this closes that gap.)
    if let Some(texture) = model.image.as_deref().and_then(|p| super::image_cache::texture_for(ui.ctx(), p)) {
        let art_rect = rect.shrink(6.0);
        let tint = if model.missing_file {
            Color32::from_white_alpha(90)
        } else {
            Color32::from_white_alpha(180)
        };
        painter.image(
            texture.id(),
            art_rect,
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            tint,
        );
    }

    let mut cursor_y = rect.top() + 8.0;

    if let Some(emoji) = model.emoji.as_deref().filter(|e| !e.is_empty()) {
        // Try the color sprite first, fall back to the monochrome font
        // glyph for anything outside the curated atlas set.
        const EMOJI_PX: f32 = 22.0;
        let emoji_rect = Rect::from_center_size(
            Pos2::new(rect.center().x, cursor_y + EMOJI_PX / 2.0),
            Vec2::splat(EMOJI_PX),
        );
        if !super::emoji::paint(ui.ctx(), &painter, emoji_rect, emoji) {
            painter.text(
                Pos2::new(rect.center().x, cursor_y),
                Align2::CENTER_TOP,
                emoji,
                FontId::proportional(20.0),
                text_color,
            );
        }
        cursor_y += 26.0;
    }

    // Labels have to be laid out (wrapped + ellipsized) rather than
    // painted raw: sound names are frequently longer than a ~96px button,
    // and painting them unwrapped just slices words in half at the button
    // edge, which looks broken.
    let label_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 4.0, cursor_y),
        Pos2::new(rect.right() - 4.0, rect.bottom() - 10.0),
    );
    let galley = {
        let mut job = egui::text::LayoutJob::single_section(
            model.label.clone(),
            egui::TextFormat {
                font_id: FontId::proportional(if rect.width() < 80.0 { 11.0 } else { 13.0 }),
                color: text_color,
                ..Default::default()
            },
        );
        job.halign = Align2::CENTER_CENTER.x();
        job.wrap = egui::text::TextWrapping {
            max_width: label_rect.width().max(8.0),
            max_rows: ((label_rect.height() / 15.0).floor() as usize).max(1),
            break_anywhere: false,
            overflow_character: Some('…'),
        };
        painter.layout_job(job)
    };
    let text_pos = label_rect.center() - galley.size() / 2.0;
    painter.galley(text_pos, galley, text_color);

    // Hotkey badge, so an assigned trigger is visible on the face of the
    // button instead of buried in the context menu.
    if let Some(hotkey) = model.hotkey.as_deref().filter(|h| !h.trim().is_empty()) {
        let badge_pos = Pos2::new(rect.left() + 5.0, rect.top() + 4.0);
        painter.text(
            badge_pos,
            Align2::LEFT_TOP,
            hotkey,
            FontId::monospace(9.0),
            super::theme::palette::LAVENDER.gamma_multiply(0.85),
        );
    }

    // Playback sweep along the bottom edge -- shows how far through the
    // clip you are, which matters for the longer drops and bits.
    if let Some(p) = progress {
        let track = Rect::from_min_max(
            Pos2::new(rect.left() + 4.0, rect.bottom() - 6.0),
            Pos2::new(rect.right() - 4.0, rect.bottom() - 3.5),
        );
        painter.rect_filled(track, 1.25, super::theme::palette::CRUST.gamma_multiply(0.8));
        let filled = Rect::from_min_max(
            track.min,
            Pos2::new(track.left() + track.width() * p.clamp(0.0, 1.0), track.max.y),
        );
        painter.rect_filled(filled, 1.25, super::theme::palette::MAUVE);
    }

    if model.missing_file {
        response.clone().on_hover_text("Audio file missing. Re-link or re-import.");
    } else if edit_mode {
        response.clone().on_hover_text("Drag to move, drag the corner to resize. Right-click for options.");
    } else {
        response.clone().on_hover_text(model.label.clone());
    }

    show_context_menu(&response, edit_mode, id, model, &mut result);

    result
}

/// Right-click always opens this (rename/color/emoji/image/volume/pitch/
/// hotkey/delete); in Edit mode a plain click (that isn't a drag) also
/// opens it, since a click there can't mean "play" and would otherwise do
/// nothing -- discoverability matters more than a convention here, most
/// people won't think to right-click a tile they're already editing.
fn show_context_menu(response: &egui::Response, edit_mode: bool, id: Id, model: &mut ButtonModel, result: &mut SoundButtonResult) {
    let should_open = response.secondary_clicked() || (edit_mode && response.clicked() && !response.dragged());

    egui::Popup::from_response(response)
        .kind(egui::PopupKind::Menu)
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .at_pointer_fixed()
        .open_memory(should_open.then_some(egui::SetOpenCommand::Bool(true)))
        .show(|ui| {
        ui.set_min_width(220.0);

        ui.label("Rename");
        if ui.text_edit_singleline(&mut model.label).lost_focus() {
            result.changed = true;
        }

        ui.separator();

        // `color_edit_button_srgba` opens its picker in a popup of its own,
        // and that popup is not a child of this menu popup -- so clicking
        // into the picker registered as a click *outside* the menu, egui
        // closed the menu, and the picker died with it before any color
        // could be chosen. That was the "tile color picker does not work"
        // bug: not a broken picker, a picker that closed the instant it
        // was touched. Hosting it in a `menu_button` submenu instead puts
        // it inside egui's own menu nesting, which is built to survive
        // exactly this (the Emoji submenu below already relies on it).
        let current = model.color.as_deref().and_then(color::parse_hex);
        ui.menu_button(format!("Color {}", if current.is_some() { "●" } else { "—" }), |ui| {
            let mut c = current.unwrap_or(Color32::from_gray(32));

            // Theme swatches first: picking a tile color is nearly always
            // "make this one visually distinct from its neighbors", and a
            // one-click palette does that far faster than a colour wheel.
            ui.label("Presets");
            ui.horizontal_wrapped(|ui| {
                for preset in super::theme::palette::TILE_PRESETS {
                    if ui
                        .add(egui::Button::new("").fill(*preset).min_size(Vec2::splat(22.0)))
                        .clicked()
                    {
                        model.color = Some(color::to_hex(*preset));
                        result.changed = true;
                        ui.close();
                    }
                }
            });

            ui.separator();

            if egui::color_picker::color_picker_color32(ui, &mut c, egui::color_picker::Alpha::Opaque) {
                model.color = Some(color::to_hex(c));
                result.changed = true;
            }

            ui.separator();
            if ui.button("Clear color").clicked() {
                model.color = None;
                result.changed = true;
                ui.close();
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

/// Rec. 601 perceived luminance, used only to decide light-vs-dark label
/// text on a user-chosen tile fill.
fn is_light(c: Color32) -> bool {
    let l = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    l > 140.0
}

fn tint_toward(base: Color32, target: Color32, amount: f32) -> Color32 {
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount) as u8;
    Color32::from_rgb(lerp(base.r(), target.r()), lerp(base.g(), target.g()), lerp(base.b(), target.b()))
}
