mod audio;
mod importers;
mod model;
mod persistence;
mod ui;
mod util;

fn main() -> eframe::Result {
    env_logger::init();

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
