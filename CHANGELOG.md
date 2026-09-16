# Changelog

All notable changes to Ultimate Soundboard are recorded here. Versions
follow [semantic versioning](https://semver.org/).

## [0.2.0] - 2026-09-15

### Added

- **Pure-Rust Flash ADPCM decoder** (`src/importers/swf_adpcm.rs`). ADPCM is
  by far the most common sound format in Flash-era soundboards, and the
  ripper previously skipped every one of them, reporting them as an
  unsupported codec. Written from the Adobe SWF19 spec's tables; no new
  dependencies, and nothing shells out to `ffmpeg` or `swfextract`.
  Verified sample-exact against ffmpeg's `adpcm_swf` decoder.
- The ripper now finds sounds nested inside `DefineSprite` movieclips, which
  were previously invisible, and extracts ADPCM and raw-PCM timeline
  (streaming) audio rather than only MP3 streams.

### Changed

- The ripper walks SWF tags itself instead of using `swf::parse_swf`. It
  reads each tag's code and length and only parses the few sound-related
  ones, so a single malformed tag now costs that one sound instead of
  aborting the whole import -- and ActionScript tag bodies are never parsed
  at all.
- Unsupported codecs are named in plain language ("Nellymoser", "Speex")
  rather than as Rust enum spellings, and sounds that failed to *save* are
  reported separately from sounds we can't decode.

### Fixed

- Dropping several `.swf` files at once only ever imported the last one;
  every rip is now tracked and imported.
- A rip that extracted nothing no longer leaves an empty timestamped
  directory behind in the app data directory.
- Decode or write failures during a rip were logged but never counted, so
  they silently vanished from the UI.
- Extracted WAVs always contain a whole number of sample frames; a trailing
  partial frame is trimmed.

## [0.1.3] - 2026-09-04

- Live volume and pitch on already-playing sounds, per-tab accent colours,
  double-click to reset a slider, and `Space` no longer taken as a global
  grab.

## [0.1.2] - 2026-09-04

- Rack-unit visual pass, `Ctrl+F` board search, and undo for tab and button
  deletes via the hotkey panel.

## [0.1.1] - 2026-09-04

- Colour picker, emoji colour, tab-switch play latency and stuck
  size/gap fixes.

## [0.1.0] - 2026-09-04

- First tagged release: tabs, drag-and-drop imports, per-button and per-tab
  volume/pitch, global hotkeys, audio output routing, zip export/import,
  the realmofdarkness.net importer, and the initial `.swf` ripper.
