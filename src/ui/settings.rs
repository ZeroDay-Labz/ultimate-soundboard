use egui::{Context, Ui};

use crate::audio::devices::{self, OutputDeviceInfo};
use crate::model::AppState;

#[derive(Default)]
pub struct SettingsResult {
    pub changed: bool,
    /// `Some(name)` when the user picked a device (`None` inside = system default).
    pub device_selected: Option<Option<String>>,
    pub wallpaper_changed: bool,
    pub always_on_top_changed: bool,
}

/// Settings window: audio output device (curated/show-all like the Python
/// dialog), default button size, snap/grid defaults, wallpaper, always on
/// top. Mirrors `settings_dialog.py`.
pub fn show(ctx: &Context, open: &mut bool, state: &mut AppState, current_device_name: Option<&str>) -> SettingsResult {
    let mut result = SettingsResult::default();

    egui::Window::new("Settings").open(open).resizable(false).show(ctx, |ui| {
        ui.horizontal(|ui| {
            if ui.checkbox(&mut state.always_on_top, "Always on top").changed() {
                result.always_on_top_changed = true;
                result.changed = true;
            }
        });

        ui.separator();
        ui.label(egui::RichText::new("Audio Output").strong());

        if ui.checkbox(&mut state.audio_show_all_devices, "Show all devices").changed() {
            result.changed = true;
        }

        if let Some(current) = current_device_name {
            ui.label(format!("Current: {current}"));
        }

        let devices_list = devices::list_output_devices(state.audio_show_all_devices);
        egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
            let is_default_selected = state.audio_output_device.is_none();
            if ui.selectable_label(is_default_selected, "System Default").clicked() {
                state.audio_output_device = None;
                result.device_selected = Some(None);
                result.changed = true;
            }
            for d in &devices_list {
                device_row(ui, d, state, &mut result);
            }
        });

        ui.separator();
        ui.label(egui::RichText::new("Board Defaults").strong());

        ui.horizontal(|ui| {
            ui.label("Default button size");
            if ui.add(egui::Slider::new(&mut state.default_button_size, 48..=200)).changed() {
                result.changed = true;
            }
        });

        ui.horizontal(|ui| {
            if ui.checkbox(&mut state.snap_enabled, "Snap to grid").changed() {
                result.changed = true;
            }
            ui.label("step");
            if ui.add(egui::Slider::new(&mut state.snap_step, 0..=64)).changed() {
                result.changed = true;
            }
        });

        ui.horizontal(|ui| {
            ui.label("Grid spacing");
            if ui.add(egui::Slider::new(&mut state.grid_spacing, 0..=40)).changed() {
                result.changed = true;
            }
        });

        ui.separator();
        ui.label(egui::RichText::new("Background").strong());

        ui.horizontal(|ui| {
            if ui.button("Choose…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp"])
                    .pick_file()
                {
                    state.wallpaper = Some(crate::model::button::normalize_path_string(&path.to_string_lossy()));
                    result.wallpaper_changed = true;
                    result.changed = true;
                }
            }
            if state.wallpaper.is_some() && ui.button("Clear").clicked() {
                state.wallpaper = None;
                result.wallpaper_changed = true;
                result.changed = true;
            }
        });
        if let Some(w) = &state.wallpaper {
            ui.small(w.clone());
        } else {
            ui.small("(none)");
        }
    });

    result
}

fn device_row(ui: &mut Ui, d: &OutputDeviceInfo, state: &mut AppState, result: &mut SettingsResult) {
    let selected = state.audio_output_device.as_deref() == Some(d.name.as_str());
    let label = if d.is_default { format!("{} (system default)", d.name) } else { d.name.clone() };
    if ui.selectable_label(selected, label).clicked() {
        state.audio_output_device = Some(d.name.clone());
        result.device_selected = Some(Some(d.name.clone()));
        result.changed = true;
    }
}
