# Ultimate Soundboard

[![CI](https://github.com/ZeroDay-Labz/ultimate-soundboard/actions/workflows/ci.yml/badge.svg)](https://github.com/ZeroDay-Labz/ultimate-soundboard/actions/workflows/ci.yml)

A cross-platform soundboard workspace: tabs, drag-and-drop imports, per-tab
and per-button volume/pitch, global Space-to-stop-all, routable audio
output (Voicemeeter on Windows, PipeWire/PulseAudio on Linux), zip
export/import for sharing setups with friends, and a one-click importer for
[realmofdarkness.net](https://www.realmofdarkness.net/sb/) soundboards.

Built with [`egui`](https://github.com/emilk/egui)/`eframe` in Rust. This
is a ground-up rewrite of an earlier Python/PySide6 prototype ("DeadAir"),
keeping its proven design (mixer architecture, decode caching, the
Realm of Darkness scraper's multi-stage URL resolution) while targeting a
proper cross-platform packaged app instead of a dev script.

## Features

- **Tabs** -- rename, set an emoji + accent color per tab so boards are
  distinguishable at a glance.
- **Drag-and-drop** -- drop audio files onto a tab to add buttons, drop a
  folder to create a new tab pre-populated from it, drop a `.zip` to
  import a soundboard pack. Files are referenced in place, never moved or
  copied.
- **Formats** -- wav/pcm, mp3, flac, aac, and ogg-vorbis decode natively;
  Opus and Discord-style `.ogg`/`.opus` files fall back to `ffmpeg` if it's
  available (same approach the Python prototype used).
- **True pitch-shift** -- duration-preserving per-button and per-tab pitch,
  not just a playback-rate change.
- **Global Stop** -- Space instantly stops every sound, system-wide on
  Windows/macOS/X11 (Wayland has no API for this -- see below).
- **Per-button global hotkeys** -- trigger a specific sound while another
  app (Discord, a softphone) has focus.
- **Audio output routing** -- pick a specific output device so the board
  plays into a virtual cable/Voicemeeter input on Windows, or shows up as a
  routable node in `pavucontrol`/`qpwgraph`/`helvum` on Linux (PipeWire).
- **Export/Import** -- bundle a tab's sounds + images + layout into a
  shareable `.zip`; import one back, or import a plain zip of audio files.
- **Clone from URL** -- realmofdarkness.net boards get the full scraper
  (candidate-filename guessing, `sndpath`/linked-JS extraction, category
  folder heuristics); other pages fall back to grabbing any audio links
  found on them.
- **`.swf` ripper** -- drop an old Flash soundboard file on the app and it
  pulls every embedded sound (MP3 and raw PCM) out into a new tab. This
  only parses the SWF's static tag structure to copy out audio bytes --
  there's no ActionScript interpreter anywhere in this app, so a
  malicious old soundboard's script has no way to run.
- **Now-playing indicator, master volume + mute, drag-to-reorder tabs** --
  see it running rather than read about it.

### Not yet built

- "Chained" soundboards (idea still being scoped).
- "Chained" soundboards (idea still being scoped).

## Building

```sh
cargo build --release
cargo run              # dev build
```

Linux build dependencies (Fedora package names shown; Debian/Ubuntu
equivalents are `libasound2-dev`/`libgtk-3-dev`):

```sh
sudo dnf install alsa-lib-devel gtk3-devel
```

Windows and macOS need no extra system packages beyond a working Rust
toolchain.

## Wayland note

Global (system-wide) hotkeys use the `global-hotkey` crate, which only
supports Windows, macOS, and X11 -- Wayland's security model doesn't let
any app observe key events while it's unfocused, portal or not. Under
Wayland, Space and per-button hotkeys still work while the app window
itself has focus; the app tells you this in the top bar when it detects
global registration isn't available.

## Packaging

| Target  | How                                                                               | Status                                                                                                                                                     |
|---------|-----------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------|
| RPM     | `cargo install cargo-generate-rpm && cargo build --release && cargo generate-rpm` | Verified -- produces a correctly-tagged `.rpm` with auto-detected `.so` deps in `target/generate-rpm/`                                                     |
| Flatpak | `packaging/flatpak/` -- see [its README](packaging/flatpak/README.md)             | Verified in CI on every push (`.github/workflows/ci.yml`'s `flatpak` job builds the manifest for real via `flathub-infra`'s container); `flatpak-builder` isn't installed locally in this dev environment, so it's not been run outside CI |
| Windows | `cargo build --release` on a Windows host or CI runner                            | `packaging/windows/README.txt` covers device routing + the ffmpeg fallback for end users                                                                   |

`.github/workflows/ci.yml` builds+tests on every push/PR, and on a
`vX.Y.Z` tag push builds the RPM and a portable Windows zip and attaches
both to a GitHub Release.

## Data location

State lives in the OS's standard app-data directory (via the `dirs`
crate) -- `~/.local/share/ultimate-soundboard/` on Linux (redirected
inside `~/.var/app/<id>/data/` automatically under Flatpak),
`%APPDATA%\ultimate-soundboard\` on Windows. Sound/image files you add are
referenced by their original path and never copied there; only
Clone-from-URL downloads and zip-import extractions land under
`<data dir>/downloaded-sounds/` and `<data dir>/imported_sounds/`.

## Testing

```sh
cargo test                                          # offline unit tests
cargo test -- --ignored --nocapture realm_of_darkness  # live network test
                                                        # against a real board
```

The live-network test isn't run in CI (it hits the real
realmofdarkness.net); run it manually if you touch the scraper.

## Emoji rendering

egui's text rasterizer has no color-glyph support, so no font can give us
colored emoji: the COLRv1 fonts shipped by Linux and Windows can't be
rasterized by it at all. Emoji are therefore drawn as **images**, not
text.

`assets/emoji/atlas.png` is a sprite atlas of the curated picker set,
pre-rendered offline by `tools/gen_emoji_atlas.py` (Pango/cairo, which do
understand COLRv1). Regenerate it whenever `EMOJIS` in
`src/ui/emoji_picker.rs` changes:

```sh
python3 tools/gen_emoji_atlas.py   # needs python3-gobject, python3-cairo,
                                   # and Noto Color Emoji installed
```

Emoji outside that set -- typed into a label, or arriving with a scraped
board -- fall back to `assets/fonts/NotoEmoji-Regular.ttf`, a monochrome
outline build bundled under the SIL Open Font License 1.1 (full text in
`assets/fonts/NotoEmoji-OFL.txt`). Without that fallback they'd render as
empty tofu boxes, since egui's default fonts cover only a handful of
emoji. Both assets are compiled into the binary, so every platform
renders identically with no system font dependency.
