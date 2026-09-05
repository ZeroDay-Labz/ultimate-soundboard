use egui::{Align2, Color32, FontId, Rect, Sense, Ui, Vec2};

use super::theme::palette;

/// Ballistics for the level meters: how fast the bar falls back after a
/// transient, and how long the peak-hold tick lingers. Rough analogue of
/// a hardware meter's decay -- instant fall looks twitchy and unreadable,
/// so the bar drops smoothly while the thin peak marker hangs behind it.
const DECAY_PER_SECOND: f32 = 1.6;
const PEAK_HOLD_SECONDS: f32 = 1.2;
const PEAK_HOLD_DECAY: f32 = 0.7;

/// Bottom of the meter's dB scale. Everything quieter than this pins to
/// the left end.
const FLOOR_DB: f32 = -48.0;

/// How long the clip indicator stays lit after the signal hits full
/// scale. Long enough that a brief overload can't flash by unnoticed,
/// which is the entire reason hardware latches this light.
const CLIP_HOLD_SECONDS: f32 = 2.0;

/// Amplitude to meter position (0..1), scaled in dB rather than linearly.
///
/// A linear meter is close to useless for audio: normal program material
/// sits crushed against the top of the scale and quiet detail is
/// invisible in the bottom pixel. Mapping to dB spreads the useful range
/// out the way real metering does.
fn db_position(amplitude: f32) -> f32 {
    if amplitude <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * amplitude.log10();
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

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
    /// Seconds since the last full-scale sample, if there's been one
    /// recently. Latches the clip LED.
    clip_age: Option<f32>,
}

impl LevelMeter {
    pub fn update(&mut self, peaks: (f32, f32), dt: f32) {
        let dt = dt.clamp(0.0, 0.25);
        self.left.update(peaks.0, dt);
        self.right.update(peaks.1, dt);

        if peaks.0 >= 0.999 || peaks.1 >= 0.999 {
            self.clip_age = Some(0.0);
        } else if let Some(age) = self.clip_age {
            let age = age + dt;
            self.clip_age = (age < CLIP_HOLD_SECONDS).then_some(age);
        }
    }

    /// True while anything is still visibly moving -- lets the caller stop
    /// forcing repaints once the meters have fully settled at silence.
    pub fn is_active(&self) -> bool {
        self.left.level > 0.001
            || self.right.level > 0.001
            || self.left.peak > 0.001
            || self.right.peak > 0.001
            || self.clip_age.is_some()
    }

    /// The meter well: two LED ladders in a recessed housing, with a clip
    /// indicator at the hot end.
    pub fn show(&self, ui: &mut Ui, size: Vec2) {
        let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(rect);

        // Recessed housing, so the ladder reads as sunk into the panel.
        painter.rect_filled(rect, 2.0, palette::RECESS);
        painter.rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(1.0, palette::PANEL_SHADOW),
            egui::StrokeKind::Inside,
        );

        let clip_w = 8.0;
        let ladder = Rect::from_min_max(
            rect.min + Vec2::new(2.0, 2.0),
            rect.max - Vec2::new(clip_w + 3.0, 2.0),
        );

        let bar_gap = 2.0;
        let bar_height = ((ladder.height() - bar_gap) / 2.0).max(2.0);

        for (i, ch) in [self.left, self.right].iter().enumerate() {
            let top = ladder.top() + i as f32 * (bar_height + bar_gap);
            let bar_rect = Rect::from_min_size(
                egui::pos2(ladder.left(), top),
                Vec2::new(ladder.width(), bar_height),
            );
            paint_channel(&painter, bar_rect, *ch);
        }

        // Clip LED.
        let clip_rect = Rect::from_min_max(
            egui::pos2(rect.right() - clip_w - 2.0, rect.top() + 2.0),
            egui::pos2(rect.right() - 2.0, rect.bottom() - 2.0),
        );
        let clip_lit = self.clip_age.is_some();
        painter.rect_filled(
            clip_rect,
            1.5,
            if clip_lit { palette::LED_RED } else { palette::LED_OFF },
        );

        if clip_lit {
            resp.on_hover_text("Clipping -- the mix hit full scale. Lower master or per-tab volume.");
        } else {
            resp.on_hover_text("Output level (dBFS)");
        }
    }

    /// The dB scale printed under the meter. Separate from `show` so the
    /// caller controls the layout between them.
    pub fn show_scale(&self, ui: &mut Ui, width: f32) {
        let (rect, _resp) = ui.allocate_exact_size(Vec2::new(width, 9.0), Sense::hover());
        let painter = ui.painter_at(rect);
        let font = FontId::monospace(7.0);

        // Same inset the ladder uses, so ticks line up with the segments
        // rather than the housing.
        let clip_w = 8.0;
        let left = rect.left() + 2.0;
        let usable = (rect.width() - clip_w - 5.0).max(1.0);

        for (db, label) in [(0.0, "0"), (-6.0, "6"), (-12.0, "12"), (-24.0, "24")] {
            let amp = 10f32.powf(db / 20.0);
            let x = left + db_position(amp) * usable;
            painter.text(
                egui::pos2(x, rect.top()),
                Align2::CENTER_TOP,
                label,
                font.clone(),
                palette::SUBTEXT.gamma_multiply(0.7),
            );
        }

        painter.text(
            egui::pos2(left, rect.top()),
            Align2::LEFT_TOP,
            "-\u{221e}",
            font,
            palette::SUBTEXT.gamma_multiply(0.7),
        );
    }
}

fn paint_channel(painter: &egui::Painter, rect: Rect, ch: Channel) {
    // Segmented ladder, green through the working range, amber as it gets
    // hot, red at the top -- the read-at-a-glance convention every piece
    // of hardware metering uses.
    let segments = 24;
    let seg_w = rect.width() / segments as f32;
    let lit = (db_position(ch.level) * segments as f32).ceil() as usize;

    for s in 0..segments {
        let frac = s as f32 / segments as f32;
        let color = if s < lit {
            segment_color(frac)
        } else {
            palette::LED_OFF
        };
        let seg = Rect::from_min_size(
            egui::pos2(rect.left() + s as f32 * seg_w, rect.top()),
            Vec2::new((seg_w - 1.0).max(1.0), rect.height()),
        );
        painter.rect_filled(seg, 0.0, color);
    }

    if ch.peak > 0.001 {
        let x = rect.left() + (db_position(ch.peak) * rect.width()).min(rect.width() - 1.5);
        let tick = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(1.5, rect.height()));
        painter.rect_filled(tick, 0.0, Color32::WHITE);
    }
}

fn segment_color(frac: f32) -> Color32 {
    if frac > 0.92 {
        palette::LED_RED
    } else if frac > 0.8 {
        palette::LED_AMBER
    } else {
        palette::LED_GREEN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_scale_places_landmarks_where_expected() {
        assert!((db_position(1.0) - 1.0).abs() < 1e-3, "full scale is the top");
        assert_eq!(db_position(0.0), 0.0, "silence pins to the floor");

        // -6 dB is half amplitude, and should land near the top of the
        // scale rather than halfway -- that's the whole point of metering
        // in dB rather than linearly.
        let half = db_position(0.5);
        assert!(half > 0.8 && half < 0.9, "-6dB landed at {half}");
    }

    #[test]
    fn clip_latches_then_releases() {
        let mut m = LevelMeter::default();
        m.update((1.0, 0.2), 0.016);
        assert!(m.clip_age.is_some(), "full scale should latch the clip LED");

        // Still lit shortly after, since a flash nobody sees is useless.
        m.update((0.1, 0.1), 0.2);
        assert!(m.clip_age.is_some());

        // `update` clamps dt (guarding against a huge delta after a
        // stall), so the hold has to be run out in realistic frame-sized
        // steps rather than one big jump.
        for _ in 0..((CLIP_HOLD_SECONDS / 0.1).ceil() as usize + 1) {
            m.update((0.1, 0.1), 0.1);
        }
        assert!(m.clip_age.is_none(), "clip should release after the hold");
    }
}
