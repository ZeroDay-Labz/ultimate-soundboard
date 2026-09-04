mod audio;
mod importers;
mod model;
mod persistence;
mod ui;
mod util;

fn main() -> eframe::Result {
    // Default to info-level so the app's own diagnostic logging (decode
    // failures, unrecognized drops, importer errors) is visible to a user
    // just running the binary normally, without needing to know to set
    // RUST_LOG themselves. RUST_LOG still overrides this if set.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ultimate Soundboard")
            .with_inner_size([1000.0, 680.0])
            .with_min_inner_size([560.0, 360.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Ultimate Soundboard",
        options,
        Box::new(|cc| Ok(Box::new(ui::SoundboardApp::new(cc)))),
    )
}
