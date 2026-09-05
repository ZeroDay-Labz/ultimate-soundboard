use std::collections::HashMap;

use egui::{Context, RichText};
use global_hotkey::hotkey::HotKey;

use crate::model::TabModel;

use super::theme::palette;

/// One button's hotkey, as the panel understands it.
pub struct Binding {
    pub hotkey: String,
    pub button_label: String,
    pub tab_name: String,
    /// False when the string doesn't parse as a hotkey at all -- the
    /// registrar skips those, so without surfacing them here a typo is
    /// invisible short of reading the log.
    pub parses: bool,
    /// True when another button claims the same combo. Only the first
    /// registration wins, so the rest silently never fire.
    pub conflicts: bool,
}

/// Collects every button hotkey across all tabs.
///
/// Deliberately derived from the same `hotkey` strings and the same
/// `HotKey` parse that `SoundboardApp::resync_button_hotkeys` registers
/// from, so the panel can't tell the user something different from what
/// the app actually bound.
pub fn collect(tabs: &[TabModel]) -> Vec<Binding> {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    let mut rows = Vec::new();

    for tab in tabs {
        for btn in &tab.buttons {
            let Some(hk_str) = btn.hotkey.as_deref().filter(|s| !s.trim().is_empty()) else {
                continue;
            };
            let parsed = hk_str.trim().parse::<HotKey>().ok();
            if let Some(hk) = parsed {
                *counts.entry(hk.id()).or_default() += 1;
            }
            rows.push((parsed.map(|h| h.id()), Binding {
                hotkey: hk_str.trim().to_string(),
                button_label: btn.label.clone(),
                tab_name: tab.name.clone(),
                parses: parsed.is_some(),
                conflicts: false,
            }));
        }
    }

    let mut out: Vec<Binding> = rows
        .into_iter()
        .map(|(id, mut b)| {
            b.conflicts = id.map(|id| counts.get(&id).copied().unwrap_or(0) > 1).unwrap_or(false);
            b
        })
        .collect();

    out.sort_by(|a, b| a.hotkey.to_lowercase().cmp(&b.hotkey.to_lowercase()));
    out
}

/// Read-only overview window. Hotkeys are still edited per-tile through
/// the right-click menu; this exists so you can see the whole set at once
/// and notice conflicts, which is impossible one tile at a time.
pub fn show(ctx: &Context, open: &mut bool, tabs: &[TabModel], global_hotkeys_available: bool) {
    let bindings = collect(tabs);
    let problems = bindings.iter().filter(|b| b.conflicts || !b.parses).count();

    egui::Window::new("Hotkeys")
        .open(open)
        .resizable(true)
        .default_width(460.0)
        .show(ctx, |ui| {
            if !global_hotkeys_available {
                ui.label(
                    RichText::new(
                        "⚠ System-wide hotkeys aren't available on this session -- these only fire while the window is focused.",
                    )
                    .color(palette::YELLOW)
                    .small(),
                );
                ui.add_space(4.0);
            }

            ui.horizontal(|ui| {
                ui.label(RichText::new("Space").monospace().strong());
                ui.label(RichText::new("stop all sounds (always bound)").color(palette::SUBTEXT));
            });
            ui.separator();

            if bindings.is_empty() {
                ui.label(
                    RichText::new("No button hotkeys set yet. Right-click a tile to give it one.")
                        .color(palette::SUBTEXT),
                );
                return;
            }

            if problems > 0 {
                ui.label(
                    RichText::new(format!(
                        "⚠ {problems} hotkey(s) won't work -- duplicates and unrecognized combos are flagged below."
                    ))
                    .color(palette::YELLOW)
                    .small(),
                );
                ui.add_space(4.0);
            }

            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                egui::Grid::new("hotkey_grid").num_columns(4).striped(true).show(ui, |ui| {
                    ui.label(RichText::new("Hotkey").strong().small());
                    ui.label(RichText::new("Button").strong().small());
                    ui.label(RichText::new("Tab").strong().small());
                    ui.label("");
                    ui.end_row();

                    for b in &bindings {
                        let bad = b.conflicts || !b.parses;
                        let key = RichText::new(&b.hotkey).monospace();
                        ui.label(if bad { key.color(palette::YELLOW) } else { key });
                        ui.label(&b.button_label);
                        ui.label(RichText::new(&b.tab_name).color(palette::SUBTEXT));

                        if !b.parses {
                            ui.label(RichText::new("⚠ not a valid combo").color(palette::RED).small())
                                .on_hover_text(
                                    "Use a form like \"Ctrl+Shift+F1\". This one couldn't be parsed, so it was never registered.",
                                );
                        } else if b.conflicts {
                            ui.label(RichText::new("⚠ duplicate").color(palette::YELLOW).small())
                                .on_hover_text(
                                    "More than one button claims this combo. Only the first one registered will fire.",
                                );
                        } else {
                            ui.label("");
                        }
                        ui.end_row();
                    }
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ButtonModel;

    fn tab_with(name: &str, buttons: Vec<(&str, Option<&str>)>) -> TabModel {
        let mut tab = TabModel::new(name.to_string());
        tab.buttons = buttons
            .into_iter()
            .map(|(label, hk)| {
                let mut b = ButtonModel::new(label, "");
                b.hotkey = hk.map(|s| s.to_string());
                b
            })
            .collect();
        tab
    }

    #[test]
    fn flags_duplicates_across_tabs() {
        let tabs = vec![
            tab_with("A", vec![("one", Some("Ctrl+Alt+D")), ("two", Some("Ctrl+Shift+F1"))]),
            tab_with("B", vec![("three", Some("Ctrl+Alt+D"))]),
        ];

        let rows = collect(&tabs);
        assert_eq!(rows.len(), 3);

        let dupes: Vec<_> = rows.iter().filter(|b| b.conflicts).map(|b| &b.button_label).collect();
        assert_eq!(dupes.len(), 2, "both sides of the clash should be flagged");
        assert!(dupes.contains(&&"one".to_string()) && dupes.contains(&&"three".to_string()));

        let unique = rows.iter().find(|b| b.button_label == "two").unwrap();
        assert!(!unique.conflicts);
    }

    #[test]
    fn flags_unparseable_and_skips_empty() {
        let tabs = vec![tab_with(
            "A",
            vec![("bad", Some("nonsense+++")), ("blank", Some("   ")), ("none", None)],
        )];

        let rows = collect(&tabs);
        assert_eq!(rows.len(), 1, "blank and unset hotkeys aren't bindings");
        assert!(!rows[0].parses);
    }
}
