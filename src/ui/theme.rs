use egui::{Color32, Context, CornerRadius, Rgba, Shadow, Stroke, Visuals};

/// Hand-rolled dark palette, Catppuccin-Mocha-inspired (no dependency --
/// the off-the-shelf `catppuccin-egui`/`egui-phosphor` crates don't have a
/// release targeting egui 0.36 yet). Slate/lavender base with soft
/// rounded corners; matches the accent colors already used for toast
/// levels (`toast.rs`) and the now-playing glow (`sound_button.rs`) so the
/// whole app reads as one coherent palette.
pub mod palette {
    use egui::Color32;

    pub const BASE: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x2e);
    pub const MANTLE: Color32 = Color32::from_rgb(0x18, 0x18, 0x25);
    pub const CRUST: Color32 = Color32::from_rgb(0x11, 0x11, 0x1b);
    pub const SURFACE0: Color32 = Color32::from_rgb(0x31, 0x32, 0x44);
    pub const SURFACE1: Color32 = Color32::from_rgb(0x45, 0x47, 0x5a);
    pub const SURFACE2: Color32 = Color32::from_rgb(0x58, 0x5b, 0x70);
    pub const OVERLAY: Color32 = Color32::from_rgb(0x6c, 0x70, 0x86);
    pub const TEXT: Color32 = Color32::from_rgb(0xcd, 0xd6, 0xf4);
    pub const SUBTEXT: Color32 = Color32::from_rgb(0xa6, 0xad, 0xc8);
    pub const LAVENDER: Color32 = Color32::from_rgb(0xb4, 0xbe, 0xfe);
    pub const BLUE: Color32 = Color32::from_rgb(0x89, 0xb4, 0xfa);
    pub const MAUVE: Color32 = Color32::from_rgb(0xcb, 0xa6, 0xf7);
    pub const GREEN: Color32 = Color32::from_rgb(0xa6, 0xe3, 0xa1);
    pub const YELLOW: Color32 = Color32::from_rgb(0xf9, 0xe2, 0xaf);
    pub const RED: Color32 = Color32::from_rgb(0xf3, 0x8b, 0xa8);

    // ---- rack-unit tokens ----
    // The chrome is painted to read as a piece of rack-mounted hardware:
    // metal faces with a vertical gradient, etched labels, and LED-style
    // indicators. These are the raw colors; `super::rack_panel` and
    // `super::engraved` do the actual drawing.

    /// Top and bottom of the panel-face gradient. The spread is
    /// deliberately narrow -- a strong gradient reads as a cheap glossy
    /// web button, a subtle one reads as brushed metal.
    pub const PANEL_TOP: Color32 = Color32::from_rgb(0x3a, 0x3c, 0x52);
    pub const PANEL_BOTTOM: Color32 = Color32::from_rgb(0x1f, 0x20, 0x2f);
    /// Hairlines along a panel's top and bottom edges, which is what sells
    /// the illusion of a physical bevelled face.
    pub const PANEL_HILIGHT: Color32 = Color32::from_rgb(0x50, 0x52, 0x6b);
    pub const PANEL_SHADOW: Color32 = Color32::from_rgb(0x14, 0x14, 0x1e);
    /// Recessed area (meter wells, control sub-panels) -- darker than the
    /// face so insets read as cut into it.
    pub const RECESS: Color32 = Color32::from_rgb(0x15, 0x15, 0x20);

    pub const LED_GREEN: Color32 = Color32::from_rgb(0x53, 0xe0, 0x8a);
    pub const LED_AMBER: Color32 = Color32::from_rgb(0xf5, 0xc2, 0x4b);
    pub const LED_RED: Color32 = Color32::from_rgb(0xff, 0x5c, 0x5c);
    /// Unlit segment/indicator -- visible as a dark well so the meter
    /// still reads as a row of LEDs when nothing is playing.
    pub const LED_OFF: Color32 = Color32::from_rgb(0x25, 0x27, 0x35);

    /// One-click fills for the tile color picker. Deliberately dark and
    /// muted rather than the bright accent hues above: these are used as
    /// full-tile backgrounds behind light label text, so they need to sit
    /// well below the text in luminance to stay readable at a glance on a
    /// wall of buttons.
    pub const TILE_PRESETS: &[Color32] = &[
        SURFACE0,
        Color32::from_rgb(0x45, 0x47, 0x5a), // slate
        Color32::from_rgb(0x5c, 0x2b, 0x35), // red
        Color32::from_rgb(0x5e, 0x3a, 0x22), // peach
        Color32::from_rgb(0x5c, 0x4d, 0x22), // amber
        Color32::from_rgb(0x2c, 0x4a, 0x2e), // green
        Color32::from_rgb(0x1f, 0x4a, 0x47), // teal
        Color32::from_rgb(0x24, 0x3c, 0x5e), // blue
        Color32::from_rgb(0x44, 0x2f, 0x5e), // violet
        Color32::from_rgb(0x5c, 0x2f, 0x4c), // magenta
    ];
}

use palette::*;

pub fn apply(ctx: &Context) {
    let mut visuals = Visuals::dark();

    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = LAVENDER;
    visuals.faint_bg_color = SURFACE0;
    visuals.extreme_bg_color = CRUST;
    visuals.code_bg_color = MANTLE;
    visuals.warn_fg_color = YELLOW;
    visuals.error_fg_color = RED;

    visuals.window_fill = BASE;
    visuals.window_stroke = Stroke::new(1.0, SURFACE1);
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.window_shadow = Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(120) };
    visuals.panel_fill = BASE;
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.popup_shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(100) };

    visuals.selection.bg_fill = LAVENDER.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, LAVENDER);

    let w = &mut visuals.widgets;
    w.noninteractive.bg_fill = MANTLE;
    w.noninteractive.weak_bg_fill = MANTLE;
    w.noninteractive.bg_stroke = Stroke::new(1.0, SURFACE0);
    w.noninteractive.fg_stroke = Stroke::new(1.0, SUBTEXT);
    w.noninteractive.corner_radius = CornerRadius::same(8);

    w.inactive.bg_fill = SURFACE0;
    w.inactive.weak_bg_fill = SURFACE0;
    w.inactive.bg_stroke = Stroke::new(1.0, SURFACE1);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = CornerRadius::same(8);

    w.hovered.bg_fill = SURFACE1;
    w.hovered.weak_bg_fill = SURFACE1;
    w.hovered.bg_stroke = Stroke::new(1.0, LAVENDER);
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.corner_radius = CornerRadius::same(8);
    w.hovered.expansion = 1.0;

    w.active.bg_fill = LAVENDER.gamma_multiply(0.28);
    w.active.weak_bg_fill = LAVENDER.gamma_multiply(0.28);
    w.active.bg_stroke = Stroke::new(1.5, LAVENDER);
    w.active.fg_stroke = Stroke::new(1.0, TEXT);
    w.active.corner_radius = CornerRadius::same(8);

    w.open.bg_fill = SURFACE1;
    w.open.weak_bg_fill = SURFACE1;
    w.open.bg_stroke = Stroke::new(1.0, SURFACE2);
    w.open.fg_stroke = Stroke::new(1.0, TEXT);
    w.open.corner_radius = CornerRadius::same(8);

    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style.spacing.window_margin = egui::Margin::same(12);
    });
}

/// Builds a metal panel face: a vertical gradient with a light hairline
/// along the top edge and a dark one along the bottom.
///
/// Returns a `Shape` rather than painting directly because a panel's real
/// height isn't known until its contents have been laid out -- inside a
/// top panel, `ui.max_rect()` is the entire remaining window, so painting
/// eagerly smears the gradient across the whole app. Callers reserve an
/// index with `painter.add(Shape::Noop)` before adding content and
/// `painter.set(...)` this shape into it afterwards, which also puts it
/// behind the widgets.
///
/// egui has no gradient primitive, so the gradient is a stack of
/// horizontal slices. `STEPS` is low on purpose -- across a ~30px toolbar
/// the banding isn't visible, and this runs every frame.
pub fn rack_panel_shape(rect: egui::Rect) -> egui::Shape {
    const STEPS: usize = 12;
    if rect.height() <= 0.0 || rect.width() <= 0.0 {
        return egui::Shape::Noop;
    }

    let top = Rgba::from(PANEL_TOP);
    let bottom = Rgba::from(PANEL_BOTTOM);
    let step_h = rect.height() / STEPS as f32;
    let mut shapes = Vec::with_capacity(STEPS + 2);

    for i in 0..STEPS {
        let t = i as f32 / (STEPS - 1).max(1) as f32;
        let color = Color32::from(top * (1.0 - t) + bottom * t);
        let slice = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + i as f32 * step_h),
            // Overdraw by half a pixel so slices can't leave seams when
            // the panel height doesn't divide evenly.
            egui::vec2(rect.width(), step_h + 0.5),
        );
        shapes.push(egui::Shape::rect_filled(slice, 0.0, color));
    }

    shapes.push(egui::Shape::line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, PANEL_HILIGHT),
    ));
    shapes.push(egui::Shape::line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, PANEL_SHADOW),
    ));

    egui::Shape::Vec(shapes)
}

/// Draws etched/engraved text: a dark copy offset one pixel down, with
/// the real text over it. Reads as stamped into the panel rather than
/// printed on top of it.
pub fn engraved(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: egui::Align2,
    text: &str,
    font: egui::FontId,
    color: Color32,
) {
    painter.text(pos + egui::vec2(0.0, 1.0), align, text, font.clone(), PANEL_SHADOW);
    painter.text(pos, align, text, font, color);
}
