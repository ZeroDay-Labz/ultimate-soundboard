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

/// A soft glow color for the now-playing pulse ring, matching the accent.
pub fn playing_glow() -> Rgba {
    Rgba::from(MAUVE)
}
