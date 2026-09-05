use std::ops::RangeInclusive;

use egui::emath::Numeric;
use egui::{Response, Slider, Ui};

/// A slider that snaps back to `default` when double-clicked.
///
/// Every slider in the app goes through this so the gesture is uniform:
/// there's otherwise no way to get a tab back to exactly unity gain or
/// 1.00x pitch by dragging, and hunting for the exact value with the
/// mouse is miserable. Returns true when the value changed, including
/// when it changed by being reset, so callers persist and re-apply it
/// immediately rather than on some later interaction.
pub fn slider_with_reset<N: Numeric>(
    ui: &mut Ui,
    value: &mut N,
    range: RangeInclusive<N>,
    default: N,
    configure: impl FnOnce(Slider<'_>) -> Slider<'_>,
) -> Response {
    let slider = configure(Slider::new(value, range));
    let mut response = ui.add(slider);

    if response.double_clicked() {
        *value = default;
        // egui only sets `changed` for edits it performed itself, so the
        // reset has to be marked explicitly or callers will ignore it.
        response.mark_changed();
    }

    response.on_hover_text(format!("Double-click to reset to {:.2}", default.to_f64()))
}
