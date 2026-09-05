use egui::{Id, RichText, Sense, Ui};

use crate::model::TabModel;

use super::color;
use super::theme::palette;

#[derive(Default)]
pub struct TabsAction {
    pub switch_to: Option<usize>,
    pub new_tab: bool,
    pub delete: Option<usize>,
    pub changed: bool,
    /// A drag-to-reorder swap happened between these two indices this
    /// frame -- caller should keep `current_tab` pointed at the same
    /// logical tab (see `app.rs::apply_tabs_action`).
    pub reorder: Option<(usize, usize)>,
}

const DRAGGING_TAB: &str = "tabs_dragging_id";
const LAST_FRAME_RECTS: &str = "tabs_last_frame_rects";

type RectsById = Vec<(uuid::Uuid, egui::Rect)>;

/// Tab strip: click to switch, double-click to rename, drag to reorder,
/// "+" to add a tab, right-click for delete. Each tab shows its
/// emoji/color accent so boards are distinguishable at a glance.
///
/// Reorder detection uses *last frame's* tab rects (cached in egui memory)
/// rather than trying to hit-test against rects still being computed in
/// this same pass -- a tab being dragged rightward can't know where tabs
/// further right will land until they've been laid out, so "one frame
/// stale" is the standard trick immediate-mode drag-reorder relies on. At
/// 60fps this is imperceptible.
/// In-progress tab rename. `focus_requested` exists because focus has to
/// be grabbed exactly once, when the field appears: calling
/// `request_focus()` every frame (as this used to) re-grabs focus the
/// instant it's lost, so `lost_focus()` never fires and there is
/// literally no way to commit or dismiss the rename.
pub struct TabRename {
    pub index: usize,
    pub name: String,
    pub focus_requested: bool,
}

impl TabRename {
    pub fn new(index: usize, name: String) -> Self {
        Self { index, name, focus_requested: false }
    }
}

pub fn show(
    ui: &mut Ui,
    tabs: &mut [TabModel],
    current: usize,
    renaming: &mut Option<TabRename>,
) -> TabsAction {
    let mut action = TabsAction::default();

    let drag_id = Id::new(DRAGGING_TAB);
    let rects_id = Id::new(LAST_FRAME_RECTS);

    let dragging_uuid: Option<uuid::Uuid> = ui.memory(|m| m.data.get_temp(drag_id)).flatten();
    let last_rects: RectsById = ui.memory(|m| m.data.get_temp(rects_id)).unwrap_or_default();
    let pointer_pos = ui.input(|i| i.pointer.interact_pos());

    let mut next_dragging_uuid = dragging_uuid;
    let mut this_frame_rects: RectsById = Vec::with_capacity(tabs.len());

    ui.horizontal_wrapped(|ui| {
        for i in 0..tabs.len() {
            let tab_id = tabs[i].id;
            let is_current = i == current;
            let is_renaming = renaming.as_ref().map(|r| r.index == i).unwrap_or(false);

            if is_renaming {
                let tab = &mut tabs[i];
                let state = renaming.as_mut().unwrap();
                let resp = ui.add(egui::TextEdit::singleline(&mut state.name).desired_width(110.0));

                // Once only -- see `TabRename`.
                if !state.focus_requested {
                    state.focus_requested = true;
                    resp.request_focus();
                }

                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));

                if escape {
                    renaming.take();
                } else if enter || resp.lost_focus() {
                    let state = renaming.take().unwrap();
                    let trimmed = state.name.trim();
                    if !trimmed.is_empty() {
                        tab.name = trimmed.chars().take(48).collect();
                    }
                    action.changed = true;
                }
                continue;
            }

            let tab = &tabs[i];
            let accent = tab.color.as_deref().and_then(color::parse_hex);

            // The emoji becomes a real image on the button rather than a
            // prefix in the label string when we have a color sprite for
            // it; only emoji outside the atlas stay inline text.
            let tab_emoji = tab.emoji.as_deref().filter(|e| !e.is_empty());
            let emoji_image = tab_emoji.and_then(|e| super::emoji::image(ui.ctx(), e, 14.0));
            let label = match (tab_emoji, emoji_image.is_some()) {
                (Some(e), false) => format!("{e} {}", tab.name),
                _ => tab.name.clone(),
            };

            let mut text = RichText::new(label);
            if let Some(c) = accent {
                text = text.color(c);
            }
            if is_current {
                text = text.strong();
            }

            let being_dragged = dragging_uuid == Some(tab_id);
            let button = match emoji_image {
                Some(img) => egui::Button::image_and_text(img, text),
                None => egui::Button::new(text),
            }
            .selected(is_current)
            .sense(Sense::click_and_drag());
            let resp = ui.push_id(tab_id, |ui| ui.add(button)).inner;
            this_frame_rects.push((tab_id, resp.rect));

            // Indicator lamp along the bottom edge: lit for the active tab,
            // dimmed to a hint for the others. A tab with no accent colour
            // still lights up, so "which board am I on" is readable at a
            // glance rather than resting on egui's subtle selection fill.
            let lamp = accent.unwrap_or(palette::LAVENDER);
            let (thickness, color) = if is_current {
                (3.0, lamp)
            } else if accent.is_some() {
                (2.0, lamp.gamma_multiply(0.45))
            } else {
                (0.0, lamp)
            };
            if thickness > 0.0 {
                let underline_rect = egui::Rect::from_min_max(
                    resp.rect.left_bottom() + egui::vec2(2.0, -thickness),
                    resp.rect.right_bottom() + egui::vec2(-2.0, 0.0),
                );
                ui.painter().rect_filled(underline_rect, 1.0, color);
            }
            if being_dragged {
                ui.painter().rect_stroke(resp.rect, 6.0, egui::Stroke::new(2.0, palette::LAVENDER), egui::StrokeKind::Outside);
            }

            if resp.drag_started() {
                next_dragging_uuid = Some(tab_id);
            }
            if resp.drag_stopped() {
                next_dragging_uuid = None;
            }

            if resp.clicked() && !resp.dragged() {
                action.switch_to = Some(i);
            }
            if resp.double_clicked() {
                *renaming = Some(TabRename::new(i, tab.name.clone()));
            }

            let tab_name = tabs[i].name.clone();
            resp.context_menu(|ui| {
                ui.set_min_width(190.0);

                if ui.button("Rename").clicked() {
                    *renaming = Some(TabRename::new(i, tab_name.clone()));
                    ui.close();
                }

                // Colour and emoji are set here rather than in a dialog,
                // matching how a tile is customized. Both were rendered
                // by the tab strip already but there was no way to
                // actually set either one.
                let tab = &mut tabs[i];
                let current_color = tab.color.as_deref().and_then(color::parse_hex);

                ui.menu_button(
                    format!("Color {}", if current_color.is_some() { "●" } else { "—" }),
                    |ui| {
                        let mut c = current_color.unwrap_or(palette::LAVENDER);

                        ui.label("Presets");
                        ui.horizontal_wrapped(|ui| {
                            for preset in palette::TAB_PRESETS {
                                if ui
                                    .add(egui::Button::new("").fill(*preset).min_size(egui::vec2(22.0, 22.0)))
                                    .clicked()
                                {
                                    tab.color = Some(color::to_hex(*preset));
                                    action.changed = true;
                                    ui.close();
                                }
                            }
                        });

                        ui.separator();
                        if egui::color_picker::color_picker_color32(
                            ui,
                            &mut c,
                            egui::color_picker::Alpha::Opaque,
                        ) {
                            tab.color = Some(color::to_hex(c));
                            action.changed = true;
                        }

                        ui.separator();
                        if ui.button("Clear color").clicked() {
                            tab.color = None;
                            action.changed = true;
                            ui.close();
                        }
                    },
                );

                ui.menu_button(
                    format!("Emoji {}", tab.emoji.as_deref().unwrap_or("")),
                    |ui| {
                        let search_id = egui::Id::new(("tab_emoji_search", tab_id));
                        let mut search = ui
                            .memory_mut(|m| m.data.get_temp::<String>(search_id).unwrap_or_default());
                        if ui.text_edit_singleline(&mut search).changed() {
                            ui.memory_mut(|m| m.data.insert_temp(search_id, search.clone()));
                        }
                        if let Some(chosen) = super::emoji_picker::grid(ui, &search) {
                            tab.emoji = Some(chosen.to_string());
                            action.changed = true;
                            ui.close();
                        }
                        if ui.small_button("Clear emoji").clicked() {
                            tab.emoji = None;
                            action.changed = true;
                            ui.close();
                        }
                    },
                );

                ui.separator();
                if ui.button(RichText::new("Delete tab").color(palette::RED)).clicked() {
                    action.delete = Some(i);
                    ui.close();
                }
            });
        }

        if ui.button("+").on_hover_text("New tab").clicked() {
            action.new_tab = true;
        }
    });

    // Reorder: if a tab is being dragged and the pointer has moved off of
    // it into where a different tab was last frame, swap them.
    if action.reorder.is_none() {
        if let (Some(dragged_id), Some(pos)) = (dragging_uuid, pointer_pos) {
            if let Some(from) = tabs.iter().position(|t| t.id == dragged_id) {
                let dragged_rect = this_frame_rects.iter().find(|(id, _)| *id == dragged_id).map(|(_, r)| *r);
                let pointer_left_current = dragged_rect.map(|r| !r.contains(pos)).unwrap_or(true);
                if pointer_left_current {
                    for (id, rect) in &last_rects {
                        if *id != dragged_id && rect.contains(pos) {
                            if let Some(to) = tabs.iter().position(|t| t.id == *id) {
                                action.reorder = Some((from, to));
                            }
                            break;
                        }
                    }
                }
            }
        }
    }

    ui.memory_mut(|m| {
        m.data.insert_temp(drag_id, next_dragging_uuid);
        m.data.insert_temp(rects_id, this_frame_rects);
    });

    if let Some((from, to)) = action.reorder {
        tabs.swap(from, to);
    }

    action
}
