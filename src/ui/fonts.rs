use egui::{Context, FontData, FontDefinitions, FontFamily};

/// Noto Emoji (monochrome outline build), bundled under the SIL Open Font
/// License 1.1 -- see `assets/fonts/NotoEmoji-OFL.txt`.
///
/// Why bundle rather than use a system font: egui's default font set only
/// covers a small handful of emoji, so most picks from the emoji picker
/// render as tofu boxes. The obvious system fonts don't help either --
/// the color emoji fonts everywhere ship as COLRv1 (Linux) or the Segoe
/// COLR build (Windows), and epaint has no color-glyph support at all, so
/// it can't rasterize them. The monochrome *outline* build is the one
/// thing egui can actually draw, and bundling it is what makes emoji
/// render identically on Windows, Flatpak and every distro rather than
/// depending on whatever happens to be installed.
///
/// This font is the *fallback* path for emoji, not the main one. Emoji in
/// the curated picker set are drawn in full color as sprites instead (see
/// `super::emoji`); this catches everything else -- an emoji typed into a
/// label, or one that arrived with a scraped board -- which would
/// otherwise be a tofu box.
const NOTO_EMOJI: &[u8] = include_bytes!("../../assets/fonts/NotoEmoji-Regular.ttf");

pub fn install(ctx: &Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert("noto_emoji".to_owned(), FontData::from_static(NOTO_EMOJI).into());

    // Appended (not prepended) so normal text keeps using egui's default
    // face and only codepoints it lacks -- i.e. the emoji -- fall through
    // to this one.
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("noto_emoji".to_owned());
    }

    ctx.set_fonts(fonts);
}
