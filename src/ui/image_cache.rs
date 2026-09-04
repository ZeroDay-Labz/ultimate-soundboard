use egui::{Context, Id, TextureHandle, TextureOptions};

/// Longest edge we keep for button artwork. Buttons are ~100px, so
/// anything beyond this is GPU memory spent on detail nobody can see --
/// and a board can easily have a hundred of them.
const MAX_EDGE: u32 = 512;

/// Loads (and then caches) a button's artwork as a GPU texture, keyed by
/// path in egui's own memory so it survives across frames and is shared
/// by every button pointing at the same file.
///
/// A failed load is cached as a miss too, so a broken or unreadable path
/// is attempted once rather than re-read from disk every single frame.
pub fn texture_for(ctx: &Context, path: &str) -> Option<TextureHandle> {
    if path.is_empty() {
        return None;
    }

    let id = Id::new(("button_image", path));

    if let Some(cached) = ctx.data(|d| d.get_temp::<Option<TextureHandle>>(id)) {
        return cached;
    }

    let loaded = load(ctx, path);
    if loaded.is_none() {
        log::warn!("could not load button image: {path}");
    }
    ctx.data_mut(|d| d.insert_temp(id, loaded.clone()));
    loaded
}

fn load(ctx: &Context, path: &str) -> Option<TextureHandle> {
    let img = image::open(path).ok()?;

    let (w, h) = (img.width(), img.height());
    let img = if w.max(h) > MAX_EDGE {
        img.thumbnail(MAX_EDGE, MAX_EDGE)
    } else {
        img
    };

    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let color_image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());

    Some(ctx.load_texture(format!("btn_img:{path}"), color_image, TextureOptions::LINEAR))
}
