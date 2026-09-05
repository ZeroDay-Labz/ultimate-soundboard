mod audio;
mod importers;
mod model;
mod persistence;
mod ui;
mod util;

/// winit 0.30's Wayland backend has no drag-and-drop implementation at
/// all (confirmed against its source: `dnd.rs` exists only under its X11
/// backend), and `global-hotkey` is X11-only too -- both hard blockers
/// for this app on a native Wayland session. `WINIT_UNIX_BACKEND` (the
/// old way to force X11) was removed in winit 0.29+ in favor of just
/// checking whether `WAYLAND_DISPLAY` is set, so the only way left to
/// prefer X11 is to clear that variable ourselves before winit reads it
/// -- which then falls through to `DISPLAY`, i.e. XWayland, which almost
/// every mainstream Linux desktop runs by default for exactly this kind
/// of legacy-X11-app compatibility. This buys back both drag-and-drop
/// and global hotkeys at the cost of native-Wayland-only niceties (e.g.
/// fractional scaling edge cases) that don't matter much for this app.
/// Set ULTIMATE_SOUNDBOARD_FORCE_WAYLAND=1 to opt back into native
/// Wayland if you have a specific reason to (no XWayland available,
/// etc.) and don't mind losing drag-and-drop and global hotkeys.
#[cfg(target_os = "linux")]
fn prefer_x11_for_dnd_and_hotkeys() {
    let opted_out = std::env::var("ULTIMATE_SOUNDBOARD_FORCE_WAYLAND")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if opted_out {
        log::info!("ULTIMATE_SOUNDBOARD_FORCE_WAYLAND set: staying on native Wayland (no drag-and-drop or global hotkeys there)");
        return;
    }

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        if std::env::var_os("DISPLAY").is_some() {
            log::info!("running under XWayland (via DISPLAY) instead of native Wayland, for drag-and-drop and global hotkey support");
            // SAFETY: called once, at the very start of `main`, before
            // eframe/winit (or any other thread) has been initialized --
            // nothing else can be reading or writing the environment
            // concurrently at this point.
            unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
        } else {
            log::warn!(
                "no XWayland (DISPLAY unset) alongside Wayland -- drag-and-drop and global hotkeys won't work on this session, see README's Wayland note"
            );
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn prefer_x11_for_dnd_and_hotkeys() {}

/// The same icon the RPM and Flatpak install for the desktop entry,
/// compiled in so the running window and its taskbar entry match the
/// launcher instead of falling back to the toolkit's generic default.
fn window_icon() -> Option<egui::IconData> {
    const ICON_PNG: &[u8] = include_bytes!("../packaging/linux/icon.png");
    match image::load_from_memory(ICON_PNG) {
        Ok(img) => {
            let img = img.to_rgba8();
            let (width, height) = (img.width(), img.height());
            Some(egui::IconData { rgba: img.into_raw(), width, height })
        }
        Err(e) => {
            log::warn!("could not decode bundled window icon: {e}");
            None
        }
    }
}

fn main() -> eframe::Result {
    // Default to info-level so the app's own diagnostic logging (decode
    // failures, unrecognized drops, importer errors) is visible to a user
    // just running the binary normally, without needing to know to set
    // RUST_LOG themselves. RUST_LOG still overrides this if set.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    prefer_x11_for_dnd_and_hotkeys();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Ultimate Soundboard")
        .with_inner_size([1000.0, 680.0])
        .with_min_inner_size([560.0, 360.0]);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        "Ultimate Soundboard",
        options,
        Box::new(|cc| Ok(Box::new(ui::SoundboardApp::new(cc)))),
    )
}
