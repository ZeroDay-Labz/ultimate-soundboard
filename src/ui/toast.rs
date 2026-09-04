use std::time::{Duration, Instant};

use egui::{Align2, Color32, Context, RichText};

const LIFETIME: Duration = Duration::from_secs(5);
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
        self.items.push(Toast { text: text.into(), level, created_at: Instant::now() });
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

    pub fn show(&mut self, ctx: &Context) {
        self.items.retain(|t| t.created_at.elapsed() < LIFETIME);
        if self.items.is_empty() {
            return;
        }

        egui::Area::new(egui::Id::new("toast_stack"))
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    for toast in self.items.iter().rev() {
                        let age = toast.created_at.elapsed();
                        let alpha = if age > LIFETIME.saturating_sub(FADE) {
                            let fade_progress = (LIFETIME - age).as_secs_f32() / FADE.as_secs_f32();
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
                                });
                            });
                        ui.add_space(6.0);
                    }
                });
            });

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}
