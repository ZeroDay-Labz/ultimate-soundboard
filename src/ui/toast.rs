use std::time::{Duration, Instant};

use egui::{Align2, Color32, Context, RichText};

const LIFETIME: Duration = Duration::from_secs(5);
/// Undo offers stay up longer than a status message: the whole point is
/// to give you time to notice you deleted the wrong thing.
const UNDO_LIFETIME: Duration = Duration::from_secs(12);
const FADE: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastLevel {
    fn accent(self) -> Color32 {
        match self {
            ToastLevel::Info => Color32::from_rgb(137, 180, 250),
            ToastLevel::Success => Color32::from_rgb(166, 227, 161),
            ToastLevel::Warning => Color32::from_rgb(249, 226, 175),
            ToastLevel::Error => Color32::from_rgb(243, 139, 168),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            ToastLevel::Info => "ℹ",
            ToastLevel::Success => "✓",
            ToastLevel::Warning => "⚠",
            ToastLevel::Error => "✕",
        }
    }
}

struct Toast {
    text: String,
    level: ToastLevel,
    created_at: Instant,
    lifetime: Duration,
    /// Shows an "Undo" button on the toast. Only one undo can be pending
    /// at a time (see `app.rs`'s `undo`), so pushing a new one supersedes
    /// any older offer.
    undoable: bool,
}

/// Small auto-dismissing notification queue, rendered bottom-right.
/// Replaces sticky banner text for transient results (import/export/
/// clone/SWF-rip done) -- the user shouldn't have to manually dismiss
/// something that was just a status update, only things that need
/// action stay as a persistent banner (see `app.rs`'s `engine_error`).
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, level: ToastLevel, text: impl Into<String>) {
        self.items.push(Toast {
            text: text.into(),
            level,
            created_at: Instant::now(),
            lifetime: LIFETIME,
            undoable: false,
        });
    }

    /// Pushes a toast with an Undo button. Any previous undo offer is
    /// dropped, since only the most recent action can be undone.
    pub fn undoable(&mut self, text: impl Into<String>) {
        self.items.retain(|t| !t.undoable);
        self.items.push(Toast {
            text: text.into(),
            level: ToastLevel::Info,
            created_at: Instant::now(),
            lifetime: UNDO_LIFETIME,
            undoable: true,
        });
    }

    /// Drops any pending undo offer -- called once the undo is used or
    /// otherwise no longer valid.
    pub fn clear_undo(&mut self) {
        self.items.retain(|t| !t.undoable);
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push(ToastLevel::Info, text);
    }
    pub fn success(&mut self, text: impl Into<String>) {
        self.push(ToastLevel::Success, text);
    }
    pub fn warning(&mut self, text: impl Into<String>) {
        self.push(ToastLevel::Warning, text);
    }
    pub fn error(&mut self, text: impl Into<String>) {
        self.push(ToastLevel::Error, text);
    }

    /// Returns true if the user clicked Undo on the pending undo toast.
    pub fn show(&mut self, ctx: &Context) -> bool {
        self.items.retain(|t| t.created_at.elapsed() < t.lifetime);
        if self.items.is_empty() {
            return false;
        }

        let mut undo_clicked = false;
        // The stack is click-through unless something on it is actually
        // clickable, so ordinary status toasts never swallow a click aimed
        // at a button underneath them.
        let interactable = self.items.iter().any(|t| t.undoable);

        egui::Area::new(egui::Id::new("toast_stack"))
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .interactable(interactable)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    for toast in self.items.iter().rev() {
                        let age = toast.created_at.elapsed();
                        let alpha = if age > toast.lifetime.saturating_sub(FADE) {
                            let fade_progress = (toast.lifetime - age).as_secs_f32() / FADE.as_secs_f32();
                            fade_progress.clamp(0.0, 1.0)
                        } else {
                            1.0
                        };

                        egui::Frame::new()
                            .fill(Color32::from_rgba_unmultiplied(30, 30, 46, (235.0 * alpha) as u8))
                            .stroke(egui::Stroke::new(1.0, toast.level.accent().gamma_multiply(alpha)))
                            .corner_radius(8.0)
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(toast.level.icon()).color(toast.level.accent().gamma_multiply(alpha)));
                                    ui.label(
                                        RichText::new(&toast.text)
                                            .color(Color32::from_rgba_unmultiplied(205, 214, 244, (255.0 * alpha) as u8)),
                                    );
                                    if toast.undoable {
                                        ui.add_space(4.0);
                                        if ui.button("Undo").clicked() {
                                            undo_clicked = true;
                                        }
                                    }
                                });
                            });
                        ui.add_space(6.0);
                    }
                });
            });

        ctx.request_repaint_after(Duration::from_millis(100));

        if undo_clicked {
            self.clear_undo();
        }
        undo_clicked
    }
}
