use egui::{Color32, Rect, Sense, Ui, Vec2};

use super::theme::palette;

/// Ballistics for the level meters: how fast the bar falls back after a
/// transient, and how long the peak-hold tick lingers. Rough analogue of
/// a hardware meter's decay -- instant fall looks twitchy and unreadable,
/// so the bar drops smoothly while the thin peak marker hangs behind it.
const DECAY_PER_SECOND: f32 = 1.6;
const PEAK_HOLD_SECONDS: f32 = 1.2;
const PEAK_HOLD_DECAY: f32 = 0.7;

#[derive(Default, Clone, Copy)]
struct Channel {
    level: f32,
    peak: f32,
    peak_age: f32,
}

impl Channel {
    fn update(&mut self, incoming: f32, dt: f32) {
        // Rise instantly (never under-report a transient), fall smoothly.
        self.level = if incoming >= self.level {
            incoming
        } else {
            (self.level - DECAY_PER_SECOND * dt).max(incoming).max(0.0)
        };

        if incoming >= self.peak {
            self.peak = incoming;
            self.peak_age = 0.0;
        } else {
            self.peak_age += dt;
            if self.peak_age > PEAK_HOLD_SECONDS {
                self.peak = (self.peak - PEAK_HOLD_DECAY * dt).max(self.level).max(0.0);
            }
        }
    }
}

/// Stereo output level meter. Owns its own smoothing state, so the caller
/// just feeds it raw per-frame peaks straight from the audio engine.
#[derive(Default)]
pub struct LevelMeter {
    left: Channel,
    right: Channel,
}

impl LevelMeter {
    pub fn update(&mut self, peaks: (f32, f32), dt: f32) {
        let dt = dt.clamp(0.0, 0.25);
        self.left.update(peaks.0, dt);
        self.right.update(peaks.1, dt);
    }

    /// True while anything is still visibly moving -- lets the caller stop
    /// forcing repaints once the meters have fully settled at silence.
    pub fn is_active(&self) -> bool {
        self.left.level > 0.001
            || self.right.level > 0.001
            || self.left.peak > 0.001
            || self.right.peak > 0.001
    }

    pub fn show(&self, ui: &mut Ui, size: Vec2) {
        let (rect, _resp) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 3.0, palette::CRUST);

        let bar_gap = 2.0;
        let bar_height = ((rect.height() - bar_gap) / 2.0).max(2.0);

        for (i, ch) in [self.left, self.right].iter().enumerate() {
            let top = rect.top() + i as f32 * (bar_height + bar_gap);
            let bar_rect = Rect::from_min_size(
                egui::pos2(rect.left(), top),
                Vec2::new(rect.width(), bar_height),
            );
            self.paint_channel(&painter, bar_rect, *ch);
        }
    }

    fn paint_channel(&self, painter: &egui::Painter, rect: Rect, ch: Channel) {
        // Segmented gradient, green through the working range, amber as it
        // gets hot, red at the top -- the read-at-a-glance convention every
        // piece of hardware metering uses.
        let segments = 28;
        let seg_w = rect.width() / segments as f32;
        let lit = (ch.level * segments as f32).ceil() as usize;

        for s in 0..segments {
            let frac = s as f32 / segments as f32;
            let color = if s < lit {
                segment_color(frac)
            } else {
                segment_color(frac).gamma_multiply(0.12)
            };
            let seg = Rect::from_min_size(
                egui::pos2(rect.left() + s as f32 * seg_w, rect.top()),
                Vec2::new((seg_w - 1.0).max(1.0), rect.height()),
            );
            painter.rect_filled(seg, 0.0, color);
        }

        if ch.peak > 0.001 {
            let x = rect.left() + (ch.peak.clamp(0.0, 1.0) * rect.width()).min(rect.width() - 1.5);
            let tick = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(1.5, rect.height()));
            painter.rect_filled(tick, 0.0, segment_color(ch.peak).gamma_multiply(1.0));
        }
    }
}

fn segment_color(frac: f32) -> Color32 {
    if frac > 0.9 {
        palette::RED
    } else if frac > 0.75 {
        palette::YELLOW
    } else {
        palette::GREEN
    }
}
