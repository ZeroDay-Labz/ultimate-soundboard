use egui::{Ui, Vec2};

/// Curated emoji set, ported from `emoji_picker.py`'s grouped list -- kept
/// as one flat searchable list here since egui menus don't need the
/// section headers to stay usable.
pub const EMOJIS: &[&str] = &[
    // Reactions / emotions
    "😂", "🤣", "😎", "😈", "😡", "🤬", "🤡", "🥴", "🤔", "🙃", "😵", "🫠", "👀", "💀", "👻", "🗿",
    "😏", "🤨", "😬", "😭", "🥲", "😤", "😮", "😴", "😱", "😐", "😑", "😒", "🤯", "🥶", "🥵", "😍",
    "😅", "😆", "😩", "😫", "😌", "😇", "🤝",
    // Audio / soundboard
    "🔊", "🔇", "🔉", "🔈", "🎤", "🎧", "🎶", "🎵", "📢", "📣", "🎙️", "🎚️", "🎛️", "🎼", "🥁", "🎸",
    "🎹", "🎺", "🎻",
    // Phone / calling / radio / comms
    "📞", "☎️", "📟", "📡", "📻", "📲", "📳", "📴", "📠", "🛰️", "📶", "🔌",
    // Alerts / emphasis / danger
    "🔥", "💥", "⚠️", "🚨", "❗", "❓", "‼️", "💣", "☠️", "⛔", "🚫", "🛑", "🔔", "🔕", "✅", "☑️",
    "❌", "🟢", "🟡", "🔴",
    // Playback / control
    "⏯️", "⏹️", "⏩", "⏪", "⏮️", "⏭️", "🔁", "🔂", "▶️", "⏸️", "⏺️", "⏫", "⏬", "⏱️", "⏲️",
    // Speech / interaction
    "💬", "🗣️", "👂", "🤐", "🧏", "🧠", "🧩", "🗯️", "✋", "🤫", "🫡",
    // Prank / chaos
    "🖕", "👌", "👎", "👍", "💩", "🤢", "🤮", "🐸", "🐍", "🦍", "🐐", "🎭", "🧨", "🔪", "🪓", "🧯",
    // Utility / labeling
    "📁", "📂", "🗂️", "📝", "✏️", "🖊️", "⭐", "🌟", "💡", "🏷️", "📌", "📍", "🔖", "🔒", "🔓", "🔑",
    "🧹", "🧰", "🛠️", "⚙️",
];

/// Renders a scrollable grid of emoji buttons. Returns the clicked emoji,
/// if any. `search` filters by substring match against each emoji glyph
/// (matches the Python picker's "type to filter" behavior).
pub fn grid(ui: &mut Ui, search: &str) -> Option<&'static str> {
    let mut picked = None;
    let cols = 8;
    let btn_size = Vec2::splat(26.0);

    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
        egui::Grid::new("emoji_grid").num_columns(cols).spacing(Vec2::splat(2.0)).show(ui, |ui| {
            let mut in_row = 0;
            for emoji in EMOJIS.iter() {
                if !search.is_empty() && !emoji.contains(search) {
                    continue;
                }
                if ui.add_sized(btn_size, egui::Button::new(*emoji)).clicked() {
                    picked = Some(*emoji);
                }
                in_row += 1;
                if in_row >= cols {
                    in_row = 0;
                    ui.end_row();
                }
            }
        });
    });

    picked
}
