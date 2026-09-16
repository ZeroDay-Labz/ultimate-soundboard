//! Works out what each sound in a `.swf` soundboard is *called*, by reading
//! the text drawn on the button that plays it.
//!
//! Flash soundboards don't usually name their sounds. `angel_v1.swf` sets no
//! `ExportAssets` symbol at all, so every tile used to import as "Sound 1",
//! "Sound 2", ... even though the board plainly shows a caption on each
//! button. The caption is in the file; it just takes four tags to reach:
//!
//! ```text
//! DefineButtonSound ---> DefineButton2 (same character id)
//!   |                      \--> ButtonRecord.id ---> DefineText
//!   |                                                  \--> glyph indices
//!   \--> DefineSound id                                      \--> DefineFont code table
//! ```
//!
//! The `swf` crate parses every one of those tags for us, and conveniently
//! exposes `Glyph::code` as the Unicode code point, so this module is join
//! logic rather than bit twiddling.
//!
//! Like `swf_rip`'s walker, this never parses ActionScript and tolerates a
//! malformed tag by skipping it.

use std::collections::HashMap;

use swf::{CharacterId, Text};

/// Longest label we'll carry over. Captions are short; anything past this is
/// a runaway text field, and the label also becomes a filename.
const MAX_LABEL_CHARS: usize = 100;

// The tags this module cares about.
const TAG_END: u16 = 0;
const TAG_DEFINE_BUTTON: u16 = 7;
const TAG_DEFINE_TEXT: u16 = 11;
const TAG_START_SOUND: u16 = 15;
const TAG_DEFINE_BUTTON_SOUND: u16 = 17;
const TAG_PLACE_OBJECT_2: u16 = 26;
const TAG_DEFINE_TEXT_2: u16 = 33;
const TAG_DEFINE_BUTTON_2: u16 = 34;
const TAG_DEFINE_EDIT_TEXT: u16 = 37;
const TAG_DEFINE_SPRITE: u16 = 39;
const TAG_DEFINE_FONT_2: u16 = 48;
const TAG_PLACE_OBJECT_3: u16 = 70;
const TAG_DEFINE_FONT_3: u16 = 75;

const MAX_SPRITE_DEPTH: usize = 16;

/// Everything gathered on the way through the tags. Fonts have to be known
/// before any text can be rendered, and nothing guarantees a font appears
/// before the text using it, so text is kept unrendered until the walk ends.
#[derive(Default)]
struct Harvest {
    /// Font character id -> glyph index -> character.
    fonts: HashMap<CharacterId, Vec<char>>,
    /// Text character id -> the parsed tag, rendered later.
    texts: HashMap<CharacterId, Text>,
    /// `DefineEditText` carries its string directly, so it needs no font.
    edit_texts: HashMap<CharacterId, String>,
    /// Button character id -> the character ids its records place.
    buttons: HashMap<CharacterId, Vec<CharacterId>>,
    /// (button character id, sounds that button triggers).
    button_sounds: Vec<(CharacterId, Vec<CharacterId>)>,
    /// Sounds and text found inside one sprite, for the fallback below.
    sprites: Vec<SpriteContents>,
}

/// What a single `DefineSprite` contained, used only by the strict
/// one-sound-one-caption fallback.
struct SpriteContents {
    sounds: Vec<CharacterId>,
    placed: Vec<CharacterId>,
}

/// Maps `DefineSound` character ids to the caption on the button that plays
/// them. Ids missing from the map had no caption we could resolve, and the
/// caller should fall back to its own naming.
pub fn collect_sound_labels(data: &[u8], version: u8) -> HashMap<CharacterId, String> {
    let mut harvest = Harvest::default();
    walk(data, version, 0, &mut harvest, None);

    // Render every text now that all fonts are known.
    let mut strings: HashMap<CharacterId, String> = HashMap::new();
    for (id, text) in &harvest.texts {
        let rendered = render_text(text, &harvest.fonts);
        if !rendered.is_empty() {
            strings.insert(*id, rendered);
        }
    }
    for (id, s) in &harvest.edit_texts {
        let cleaned = tidy(s);
        if !cleaned.is_empty() {
            strings.entry(*id).or_insert(cleaned);
        }
    }

    let mut labels = HashMap::new();

    // Primary: DefineButtonSound -> DefineButton -> its text character.
    for (button_id, sound_ids) in &harvest.button_sounds {
        let Some(chars) = harvest.buttons.get(button_id) else { continue };
        let Some(label) = chars.iter().find_map(|c| strings.get(c)) else { continue };
        for sound_id in sound_ids {
            labels.entry(*sound_id).or_insert_with(|| label.clone());
        }
    }

    // Fallback for boards built from sprites rather than buttons. Only
    // applied when a sprite is unambiguous -- exactly one sound and exactly
    // one caption -- because a wrong label is worse than a numbered one.
    for sprite in &harvest.sprites {
        if sprite.sounds.len() != 1 {
            continue;
        }
        let mut captions = sprite.placed.iter().filter_map(|c| strings.get(c));
        let (Some(label), None) = (captions.next(), captions.next()) else { continue };
        labels.entry(sprite.sounds[0]).or_insert_with(|| label.clone());
    }

    labels
}

/// Same resilient walk as `swf_rip::walk_tags`: read each tag's code and
/// length, parse only the handful we understand, and always advance by the
/// declared length so one bad tag costs one tag.
fn walk(
    data: &[u8],
    version: u8,
    depth: usize,
    harvest: &mut Harvest,
    mut sprite: Option<&mut SpriteContents>,
) {
    let mut reader = swf::read::Reader::new(data, version);

    loop {
        let Ok((code, length)) = reader.read_tag_code_and_length() else { break };
        if code == TAG_END {
            break;
        }
        let remaining = reader.get_ref();
        if length > remaining.len() {
            break;
        }
        let body = &remaining[..length];
        *reader.get_mut() = &remaining[length..];

        let body_reader = || swf::read::Reader::new(body, version);

        match code {
            TAG_DEFINE_FONT_2 | TAG_DEFINE_FONT_3 => {
                let font_version = if code == TAG_DEFINE_FONT_2 { 2 } else { 3 };
                if let Ok(font) = body_reader().read_define_font_2(font_version) {
                    let table =
                        font.glyphs.iter().map(|g| char::from_u32(g.code as u32).unwrap_or('?'));
                    harvest.fonts.insert(font.id, table.collect());
                }
            }
            TAG_DEFINE_TEXT | TAG_DEFINE_TEXT_2 => {
                let text_version = if code == TAG_DEFINE_TEXT { 1 } else { 2 };
                if let Ok(text) = body_reader().read_define_text(text_version) {
                    harvest.texts.insert(text.id, text);
                }
            }
            TAG_DEFINE_EDIT_TEXT => {
                if let Ok(edit) = body_reader().read_define_edit_text()
                    && let Some(text) = edit.initial_text()
                {
                    let encoding = swf::SwfStr::encoding_for_version(version);
                    harvest.edit_texts.insert(edit.id(), text.to_str_lossy(encoding).into_owned());
                }
            }
            TAG_DEFINE_BUTTON | TAG_DEFINE_BUTTON_2 => {
                let parsed = if code == TAG_DEFINE_BUTTON {
                    body_reader().read_define_button_1()
                } else {
                    body_reader().read_define_button_2()
                };
                if let Ok(button) = parsed {
                    harvest
                        .buttons
                        .insert(button.id, button.records.iter().map(|r| r.id).collect());
                }
            }
            TAG_DEFINE_BUTTON_SOUND => {
                if let Ok(sounds) = body_reader().read_define_button_sound() {
                    let ids: Vec<CharacterId> = [
                        // Click first: on a soundboard that's the one that
                        // actually plays the clip.
                        &sounds.over_to_down_sound,
                        &sounds.up_to_over_sound,
                        &sounds.over_to_up_sound,
                        &sounds.down_to_over_sound,
                    ]
                    .iter()
                    .filter_map(|s| s.as_ref().map(|(id, _)| *id))
                    .filter(|id| *id != 0)
                    .collect();
                    if !ids.is_empty() {
                        harvest.button_sounds.push((sounds.id, ids));
                    }
                }
            }
            TAG_START_SOUND => {
                if let Some(sprite) = sprite.as_deref_mut()
                    && let Ok(start) = body_reader().read_start_sound_1()
                {
                    sprite.sounds.push(start.id);
                }
            }
            TAG_PLACE_OBJECT_2 | TAG_PLACE_OBJECT_3 => {
                if let Some(sprite) = sprite.as_deref_mut() {
                    let place_version = if code == TAG_PLACE_OBJECT_2 { 2 } else { 3 };
                    if let Ok(place) = body_reader().read_place_object_2_or_3(place_version) {
                        match place.action {
                            swf::PlaceObjectAction::Place(id)
                            | swf::PlaceObjectAction::Replace(id) => sprite.placed.push(id),
                            swf::PlaceObjectAction::Modify => {}
                        }
                    }
                }
            }
            TAG_DEFINE_SPRITE => {
                // Body is id(u16) + frame_count(u16) + a nested tag list.
                if depth < MAX_SPRITE_DEPTH && body.len() > 4 {
                    let mut nested = SpriteContents { sounds: Vec::new(), placed: Vec::new() };
                    walk(&body[4..], version, depth + 1, harvest, Some(&mut nested));
                    if !nested.sounds.is_empty() {
                        harvest.sprites.push(nested);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Turns a `DefineText` back into a string by mapping each glyph index
/// through its font's code table.
///
/// A record that moves down the page is a new line, and the two halves of a
/// wrapped caption have to be separated or "HI THIS IS ANGEL" + "FROM SOC
/// POLICE" runs together into one word. A record that only shifts
/// horizontally is same-line kerning and gets no separator.
fn render_text(text: &Text, fonts: &HashMap<CharacterId, Vec<char>>) -> String {
    let mut out = String::new();
    let mut font: Option<&Vec<char>> = None;
    let mut line: Option<i32> = None;

    for record in &text.records {
        if let Some(id) = record.font_id {
            // A record without a font id keeps using the previous one.
            font = fonts.get(&id);
        }
        let Some(table) = font else { continue };

        if let Some(y) = record.y_offset {
            let y = y.get();
            if line.is_some_and(|prev| prev != y) && !out.is_empty() {
                out.push(' ');
            }
            line = Some(y);
        }

        for glyph in &record.glyphs {
            if let Some(c) = table.get(glyph.index as usize) {
                out.push(*c);
            }
        }
    }

    tidy(&out)
}

/// Collapses whitespace and caps the length -- these strings become both a
/// tile caption and a filename.
fn tidy(s: &str) -> String {
    let mut out = String::new();
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if out.chars().count() > MAX_LABEL_CHARS {
        out = out.chars().take(MAX_LABEL_CHARS).collect::<String>().trim_end().to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use swf::{GlyphEntry, Matrix, Rectangle, TextRecord, Twips};

    fn font_table(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// A text record whose glyph indices point into `table`'s ordering.
    fn record(font_id: Option<u16>, y: Option<i32>, table: &[char], s: &str) -> TextRecord {
        TextRecord {
            font_id,
            color: None,
            x_offset: None,
            y_offset: y.map(Twips::new),
            height: None,
            glyphs: s
                .chars()
                .map(|c| GlyphEntry {
                    index: table.iter().position(|t| *t == c).unwrap() as u32,
                    advance: 0,
                })
                .collect(),
        }
    }

    fn text(records: Vec<TextRecord>) -> Text {
        Text {
            id: 1,
            bounds: Rectangle::default(),
            matrix: Matrix::IDENTITY,
            records,
        }
    }

    #[test]
    fn renders_glyph_indices_through_the_font_table() {
        let table = font_table("ABCDEFGHIJKLMNOPQRSTUVWXYZ ");
        let fonts = HashMap::from([(7u16, table.clone())]);
        let t = text(vec![record(Some(7), Some(0), &table, "HELLO")]);
        assert_eq!(render_text(&t, &fonts), "HELLO");
    }

    /// The bug this rule exists for: a caption wrapped onto two lines used to
    /// come out as "HI THIS IS ANGELFROM SOC POLICE".
    #[test]
    fn a_new_line_becomes_a_space() {
        let table = font_table("ABCDEFGHIJKLMNOPQRSTUVWXYZ ");
        let fonts = HashMap::from([(7u16, table.clone())]);
        let t = text(vec![
            record(Some(7), Some(240), &table, "HI THIS IS ANGEL"),
            record(Some(7), Some(580), &table, "FROM SOC POLICE"),
        ]);
        assert_eq!(render_text(&t, &fonts), "HI THIS IS ANGEL FROM SOC POLICE");
    }

    /// Same line, only kerned along -- no separator, or words get split.
    #[test]
    fn same_line_records_are_not_separated() {
        let table = font_table("ABCDEFGHIJKLMNOPQRSTUVWXYZ ");
        let fonts = HashMap::from([(7u16, table.clone())]);
        let t = text(vec![
            record(Some(7), Some(240), &table, "ANG"),
            record(Some(7), Some(240), &table, "EL"),
        ]);
        assert_eq!(render_text(&t, &fonts), "ANGEL");
    }

    /// Per the spec a record without a font id keeps using the previous one.
    #[test]
    fn font_carries_forward_to_records_that_omit_it() {
        let table = font_table("ABCDEFGHIJKLMNOPQRSTUVWXYZ ");
        let fonts = HashMap::from([(7u16, table.clone())]);
        let t = text(vec![
            record(Some(7), Some(0), &table, "OK"),
            record(None, Some(0), &table, "AY"),
        ]);
        assert_eq!(render_text(&t, &fonts), "OKAY");
    }

    #[test]
    fn a_missing_font_yields_nothing_rather_than_panicking() {
        let table = font_table("ABC");
        let t = text(vec![record(Some(99), Some(0), &table, "AB")]);
        assert_eq!(render_text(&t, &HashMap::new()), "");
    }

    /// A glyph index past the end of the code table is skipped, not indexed.
    #[test]
    fn out_of_range_glyph_indices_are_skipped() {
        let fonts = HashMap::from([(7u16, font_table("AB"))]);
        let t = text(vec![TextRecord {
            font_id: Some(7),
            color: None,
            x_offset: None,
            y_offset: Some(Twips::new(0)),
            height: None,
            glyphs: vec![
                GlyphEntry { index: 0, advance: 0 },
                GlyphEntry { index: 9999, advance: 0 },
                GlyphEntry { index: 1, advance: 0 },
            ],
        }]);
        assert_eq!(render_text(&t, &fonts), "AB");
    }

    #[test]
    fn tidy_collapses_whitespace_and_caps_length() {
        assert_eq!(tidy("  HI   THERE \n YOU "), "HI THERE YOU");
        assert_eq!(tidy(""), "");
        let long = "A".repeat(MAX_LABEL_CHARS + 50);
        assert_eq!(tidy(&long).chars().count(), MAX_LABEL_CHARS);
    }

    // --- tag-level tests for the button -> sound -> caption join ---

    fn tag(code: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if body.len() < 0x3F {
            out.extend_from_slice(&((code << 6) | body.len() as u16).to_le_bytes());
        } else {
            out.extend_from_slice(&((code << 6) | 0x3F).to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        }
        out.extend_from_slice(body);
        out
    }

    /// DefineEditText carries its string inline, so it exercises the join
    /// chain without hand-assembling a font's glyph shape table.
    fn edit_text(id: u16, s: &str) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&id.to_le_bytes());
        body.push(0x00); // empty RECT (nbits = 0)
        body.push(0x80); // HasText
        body.push(0x00);
        body.push(0x00); // empty VariableName
        body.extend_from_slice(s.as_bytes());
        body.push(0x00);
        tag(TAG_DEFINE_EDIT_TEXT, &body)
    }

    /// One button record per referenced character, with identity matrix and
    /// colour transform (a zero byte each).
    fn button2(id: u16, chars: &[u16]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&id.to_le_bytes());
        body.push(0x00); // flags
        body.extend_from_slice(&0u16.to_le_bytes()); // action offset
        for (i, c) in chars.iter().enumerate() {
            body.push(0x07); // up | over | down
            body.extend_from_slice(&c.to_le_bytes());
            body.extend_from_slice(&(i as u16 + 1).to_le_bytes()); // depth
            body.push(0x00); // MATRIX: no scale, no rotate, 0 translate bits
            body.push(0x00); // CXFORMWITHALPHA: no add, no mult, 0 bits
        }
        body.push(0x00); // end of records
        tag(TAG_DEFINE_BUTTON_2, &body)
    }

    /// Puts `sound_id` in the over-to-down (click) slot.
    fn button_sound(button_id: u16, sound_id: u16) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&button_id.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes()); // over_to_up: none
        body.extend_from_slice(&0u16.to_le_bytes()); // up_to_over: none
        body.extend_from_slice(&sound_id.to_le_bytes());
        body.push(0x00); // SOUNDINFO: no flags
        body.extend_from_slice(&0u16.to_le_bytes()); // down_to_over: none
        tag(TAG_DEFINE_BUTTON_SOUND, &body)
    }

    fn labels_of(tags: &[u8]) -> HashMap<CharacterId, String> {
        collect_sound_labels(tags, 8)
    }

    #[test]
    fn joins_button_sound_to_the_buttons_caption() {
        let mut tags = Vec::new();
        tags.extend_from_slice(&edit_text(2, "THIS IS ANGEL"));
        tags.extend_from_slice(&button2(4, &[2]));
        tags.extend_from_slice(&button_sound(4, 3));
        tags.extend_from_slice(&0u16.to_le_bytes()); // End

        assert_eq!(labels_of(&tags).get(&3).map(String::as_str), Some("THIS IS ANGEL"));
    }

    #[test]
    fn a_button_with_no_text_character_yields_no_label() {
        let mut tags = Vec::new();
        tags.extend_from_slice(&button2(4, &[99])); // a shape, not text
        tags.extend_from_slice(&button_sound(4, 3));
        tags.extend_from_slice(&0u16.to_le_bytes());

        assert!(labels_of(&tags).is_empty());
    }

    /// One malformed tag between two good buttons must not lose the second.
    #[test]
    fn a_corrupt_tag_does_not_abort_the_label_walk() {
        let mut tags = Vec::new();
        tags.extend_from_slice(&edit_text(2, "FIRST"));
        tags.extend_from_slice(&button2(4, &[2]));
        tags.extend_from_slice(&button_sound(4, 3));
        tags.extend_from_slice(&tag(TAG_DEFINE_BUTTON_2, &[0x01])); // too short
        tags.extend_from_slice(&edit_text(5, "SECOND"));
        tags.extend_from_slice(&button2(7, &[5]));
        tags.extend_from_slice(&button_sound(7, 6));
        tags.extend_from_slice(&0u16.to_le_bytes());

        let labels = labels_of(&tags);
        assert_eq!(labels.get(&3).map(String::as_str), Some("FIRST"));
        assert_eq!(labels.get(&6).map(String::as_str), Some("SECOND"));
    }

    /// The sprite fallback only fires when the pairing is unambiguous.
    #[test]
    fn a_sprite_with_one_sound_and_one_caption_is_paired() {
        let mut inner = Vec::new();
        inner.extend_from_slice(&edit_text(2, "SPRITE LABEL"));
        // PlaceObject2: flags = HasCharacter, depth, character id
        inner.extend_from_slice(&tag(TAG_PLACE_OBJECT_2, &[0x02, 0x01, 0x00, 0x02, 0x00]));
        inner.extend_from_slice(&tag(TAG_START_SOUND, &[0x03, 0x00, 0x00]));
        inner.extend_from_slice(&0u16.to_le_bytes());

        let mut sprite_body = Vec::new();
        sprite_body.extend_from_slice(&50u16.to_le_bytes());
        sprite_body.extend_from_slice(&1u16.to_le_bytes());
        sprite_body.extend_from_slice(&inner);

        let mut tags = tag(TAG_DEFINE_SPRITE, &sprite_body);
        tags.extend_from_slice(&0u16.to_le_bytes());

        assert_eq!(labels_of(&tags).get(&3).map(String::as_str), Some("SPRITE LABEL"));
    }

    #[test]
    fn an_ambiguous_sprite_is_left_unlabelled() {
        let mut inner = Vec::new();
        inner.extend_from_slice(&edit_text(2, "ONE"));
        inner.extend_from_slice(&tag(TAG_PLACE_OBJECT_2, &[0x02, 0x01, 0x00, 0x02, 0x00]));
        inner.extend_from_slice(&tag(TAG_START_SOUND, &[0x03, 0x00, 0x00]));
        // A second sound in the same sprite makes the pairing a guess.
        inner.extend_from_slice(&tag(TAG_START_SOUND, &[0x04, 0x00, 0x00]));
        inner.extend_from_slice(&0u16.to_le_bytes());

        let mut sprite_body = Vec::new();
        sprite_body.extend_from_slice(&50u16.to_le_bytes());
        sprite_body.extend_from_slice(&1u16.to_le_bytes());
        sprite_body.extend_from_slice(&inner);

        let mut tags = tag(TAG_DEFINE_SPRITE, &sprite_body);
        tags.extend_from_slice(&0u16.to_le_bytes());

        assert!(labels_of(&tags).is_empty(), "two sounds in one sprite must not be guessed at");
    }
}
