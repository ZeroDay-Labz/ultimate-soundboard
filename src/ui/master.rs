use egui::{Align2, Color32, FontId, RichText, Sense, Ui, Vec2};

use super::meter::LevelMeter;
use super::theme::palette;

/// Width this strip needs. The caller reserves exactly this much so the
/// strip and the controls beside it can't overlap -- letting egui's
/// right-to-left layout negotiate it is what put "MASTER" on top of
/// "Settings" the first time around.
///
/// Must cover the controls below plus the spacing between them
/// (48 + 38 + 96 + 34 + 150 + 56, with 5 gaps of 6), or the last one
/// drawn -- STOP -- gets clipped off the window edge.
pub const WIDTH: f32 = 460.0;
pub const HEIGHT: f32 = 34.0;

#[derive(Default)]
pub struct MasterResult {
    /// Volume or mute changed -- the caller re-applies gain and persists.
    pub changed: bool,
    pub stop_all: bool,
}

/// The master section: fader, mute switch, output meter with a dB scale,
/// and the global stop.
///
/// `STOP` lives here rather than in the per-tab controls row, where it
/// used to sit. It has always stopped *every* sound in the app, not just
/// the current tab's, so a per-tab row was quietly misleading about its
/// scope; grouped with the other master controls it reads correctly, and
/// it gets to be the big red button it should have been.
pub fn show(ui: &mut Ui, meter: &LevelMeter, volume: &mut f32, muted: &mut bool) -> MasterResult {
    let mut result = MasterResult::default();

    ui.spacing_mut().item_spacing.x = 6.0;

    // ---- MASTER legend ----
    let (label_rect, _) = ui.allocate_exact_size(Vec2::new(48.0, HEIGHT), Sense::hover());
    super::theme::engraved(
        &ui.painter_at(label_rect),
        label_rect.center(),
        Align2::CENTER_CENTER,
        "MASTER",
        FontId::monospace(9.0),
        palette::SUBTEXT,
    );

    // ---- mute ----
    // Lit red when engaged, dark when not: the switch state is carried by
    // the lamp, not by changing the legend.
    let mute_button = egui::Button::new(
        RichText::new("MUTE")
            .monospace()
            .size(9.0)
            .color(if *muted { Color32::WHITE } else { palette::SUBTEXT }),
    )
    .fill(if *muted { palette::LED_RED.gamma_multiply(0.75) } else { palette::LED_OFF })
    .stroke(egui::Stroke::new(
        1.0,
        if *muted { palette::LED_RED } else { palette::PANEL_SHADOW },
    ));
    if ui
        .add_sized(Vec2::new(38.0, 18.0), mute_button)
        .on_hover_text(if *muted { "Unmute output" } else { "Mute output" })
        .clicked()
    {
        *muted = !*muted;
        result.changed = true;
    }

    // ---- fader + readout ----
    let slider = egui::Slider::new(volume, 0.0..=2.0).show_value(false);
    if ui
        .add_sized([96.0, 18.0], slider)
        .on_hover_text("Master volume (100% is unity gain)")
        .changed()
    {
        result.changed = true;
    }
    ui.add_sized(
        Vec2::new(34.0, 18.0),
        egui::Label::new(RichText::new(format!("{:.0}%", *volume * 100.0)).monospace().small()),
    );

    // ---- meter, with its scale directly beneath ----
    ui.allocate_ui_with_layout(
        Vec2::new(150.0, HEIGHT),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.add_space(2.0);
            meter.show(ui, Vec2::new(150.0, 15.0));
            meter.show_scale(ui, 150.0);
        },
    );

    // ---- stop ----
    let stop = ui.add_sized(
        Vec2::new(56.0, 26.0),
        egui::Button::new(RichText::new("STOP").strong().monospace().color(Color32::WHITE))
            .fill(palette::LED_RED.gamma_multiply(0.6))
            .stroke(egui::Stroke::new(1.0, palette::LED_RED)),
    );
    if stop.on_hover_text("Stop every sound now (Space)").clicked() {
        result.stop_all = true;
    }

    result
}
