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
pub fn show(
    ui: &mut Ui,
    tabs: &mut [TabModel],
    current: usize,
    renaming: &mut Option<(usize, String)>,
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
            let is_renaming = renaming.as_ref().map(|(idx, _)| *idx == i).unwrap_or(false);

            if is_renaming {
                let tab = &mut tabs[i];
                let (_, name) = renaming.as_mut().unwrap();
                let resp = ui.text_edit_singleline(name);
                resp.request_focus();
                if resp.lost_focus() {
                    let (_, name) = renaming.take().unwrap();
                    let trimmed = name.trim();
                    if !trimmed.is_empty() {
                        tab.name = trimmed.chars().take(48).collect();
                    }
                    action.changed = true;
                }
                continue;
            }

            let tab = &tabs[i];
            let accent = tab.color.as_deref().and_then(color::parse_hex);
            let label = match &tab.emoji {
                Some(e) if !e.is_empty() => format!("{e} {}", tab.name),
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
            let button = egui::Button::new(text).selected(is_current).sense(Sense::click_and_drag());
            let resp = ui.push_id(tab_id, |ui| ui.add(button)).inner;
            this_frame_rects.push((tab_id, resp.rect));

            if let Some(c) = accent {
                let underline_rect = egui::Rect::from_min_max(
                    resp.rect.left_bottom() + egui::vec2(2.0, -2.0),
                    resp.rect.right_bottom() + egui::vec2(-2.0, 0.0),
                );
                ui.painter().rect_filled(underline_rect, 1.0, c);
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
                *renaming = Some((i, tab.name.clone()));
            }

            resp.context_menu(|ui| {
                if ui.button("Rename").clicked() {
                    *renaming = Some((i, tab.name.clone()));
                    ui.close();
                }
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
