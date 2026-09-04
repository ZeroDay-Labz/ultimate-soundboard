use cpal::traits::HostTrait;

/// Keywords for devices that are almost certainly what a "route this into
/// Discord/OBS/a softphone" user wants: Voicemeeter/VB-Cable on Windows,
/// PipeWire's own routable nodes on Linux, and plain headphone/speaker
/// outputs. Mirrors `_CURATED_KEYWORDS` in the Python app's engine.py,
/// extended with the Linux/PipeWire equivalents.
const CURATED_KEYWORDS: &[&str] = &[
    "voicemeeter",
    "vb-audio",
    "cable",
    "pipewire",
    "pulse",
    "headphones",
    "headset",
    "speakers",
    "earphone",
    "earbuds",
    "digital audio",
];

const HIDE_KEYWORDS: &[&str] = &["microsoft sound mapper", "primary sound driver"];

#[derive(Debug, Clone)]
pub struct OutputDeviceInfo {
    pub name: String,
    pub is_default: bool,
}

fn list_all() -> Vec<OutputDeviceInfo> {
    let host = cpal::default_host();
    let default_name = host.default_output_device().map(|d| d.to_string());

    let devices = match host.output_devices() {
        Ok(d) => d,
        Err(e) => {
            log::warn!("could not enumerate output devices: {e}");
            return Vec::new();
        }
    };

    devices
        .map(|d| {
            let name = d.to_string();
            let is_default = default_name.as_deref() == Some(name.as_str());
            OutputDeviceInfo { name, is_default }
        })
        .collect()
}

/// Lists output devices. When `show_all` is false, applies the same
/// curated shortlist behavior as the Python settings dialog: devices
/// matching a routing-relevant keyword are preferred, and if none match,
/// the full (minus hidden) list is returned rather than an empty one.
pub fn list_output_devices(show_all: bool) -> Vec<OutputDeviceInfo> {
    let all = list_all();

    let visible: Vec<OutputDeviceInfo> = all
        .into_iter()
        .filter(|d| {
            let low = d.name.to_lowercase();
            !HIDE_KEYWORDS.iter().any(|k| low.contains(k))
        })
        .collect();

    if show_all {
        return visible;
    }

    let curated: Vec<OutputDeviceInfo> = visible
        .iter()
        .filter(|d| {
            let low = d.name.to_lowercase();
            CURATED_KEYWORDS.iter().any(|k| low.contains(k))
        })
        .cloned()
        .collect();

    if curated.is_empty() {
        visible
    } else {
        curated
    }
}

/// Resolves a saved device name back to a live `cpal::Device`, falling
/// back to the system default if the saved device is gone (unplugged,
/// renamed, etc.) -- same graceful-degrade the Python app does.
pub fn resolve_device(name: Option<&str>) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if let Some(name) = name {
        if let Ok(mut devices) = host.output_devices() {
            if let Some(d) = devices.find(|d| d.to_string() == name) {
                return Some(d);
            }
        }
        log::warn!("saved output device '{name}' not found, falling back to default");
    }
    host.default_output_device()
}
