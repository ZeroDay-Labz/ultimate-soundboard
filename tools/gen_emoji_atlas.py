#!/usr/bin/env python3
"""Pre-renders the curated emoji set into a single color sprite atlas.

Why this exists: egui's text rasterizer has no color-glyph support, so it
physically cannot draw the COLRv1 emoji fonts that ship on Linux and
Windows -- the best a font can give us is monochrome outlines. The way
around that is to stop treating emoji as text and draw them as images
instead, which means having the pixels ready ahead of time.

This runs offline against whatever color emoji font the *build machine*
has (via Pango/cairo, which do understand COLRv1) and bakes the result
into `assets/emoji/`. Users never need the font, and every platform gets
identical color emoji.

Run it only when `EMOJIS` in src/ui/emoji_picker.rs changes:

    python3 tools/gen_emoji_atlas.py

Requires: python3-gobject, python3-cairo, Noto Color Emoji installed.
"""

import os
import re
import sys

import gi

gi.require_version("Pango", "1.0")
gi.require_version("PangoCairo", "1.0")
import cairo  # noqa: E402
from gi.repository import Pango, PangoCairo  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PICKER = os.path.join(ROOT, "src", "ui", "emoji_picker.rs")
OUT_DIR = os.path.join(ROOT, "assets", "emoji")

# 72px cells: emoji draw at ~20px on a button and ~14px in the tab strip,
# so this leaves plenty of headroom for a large tile or a HiDPI screen
# while keeping the whole atlas comfortably small.
CELL = 72
COLS = 16
FONT = "Noto Color Emoji"


def read_emojis() -> list[str]:
    """Pulls the EMOJIS list straight out of the Rust source.

    Parsing the real list keeps the atlas and the picker from drifting
    apart -- a hand-maintained copy here would silently stop covering
    emoji as soon as someone edited only the Rust side.
    """
    src = open(PICKER, encoding="utf-8").read()
    m = re.search(r"pub const EMOJIS: &\[&str\] = &\[(.*?)\n\];", src, re.S)
    if not m:
        sys.exit("could not find EMOJIS list in emoji_picker.rs")
    body = m.group(1)
    # Strip // comments before pulling out the quoted glyphs.
    body = re.sub(r"//[^\n]*", "", body)
    out, seen = [], set()
    for glyph in re.findall(r'"([^"]+)"', body):
        if glyph not in seen:
            seen.add(glyph)
            out.append(glyph)
    return out


def render(emojis: list[str]) -> cairo.ImageSurface:
    rows = (len(emojis) + COLS - 1) // COLS
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, COLS * CELL, rows * CELL)
    ctx = cairo.Context(surface)

    layout = PangoCairo.create_layout(ctx)
    # Point size is chosen so the glyph's ink fits the cell; it gets
    # centered per-cell below rather than trusting the font's metrics.
    layout.set_font_description(Pango.FontDescription(f"{FONT} 52"))

    missing = []
    for i, glyph in enumerate(emojis):
        layout.set_text(glyph, -1)
        ink, _logical = layout.get_pixel_extents()
        if ink.width == 0 or ink.height == 0:
            missing.append(glyph)
            continue

        col, row = i % COLS, i // COLS
        x = col * CELL + (CELL - ink.width) / 2 - ink.x
        y = row * CELL + (CELL - ink.height) / 2 - ink.y

        ctx.save()
        ctx.move_to(x, y)
        PangoCairo.show_layout(ctx, layout)
        ctx.restore()

    if missing:
        print(f"warning: {len(missing)} glyph(s) rendered empty: {' '.join(missing)}")
    return surface


def main() -> None:
    emojis = read_emojis()
    os.makedirs(OUT_DIR, exist_ok=True)

    surface = render(emojis)
    surface.write_to_png(os.path.join(OUT_DIR, "atlas.png"))

    # The index is written explicitly rather than assuming Rust can
    # re-derive positions from the picker order: that coupling would
    # break the moment the two lists diverge, and silently show the
    # wrong emoji rather than fail loudly.
    with open(os.path.join(OUT_DIR, "atlas.txt"), "w", encoding="utf-8") as f:
        f.write(f"cell={CELL}\ncols={COLS}\n")
        for glyph in emojis:
            f.write(glyph + "\n")

    rows = (len(emojis) + COLS - 1) // COLS
    size = os.path.getsize(os.path.join(OUT_DIR, "atlas.png"))
    print(f"wrote {len(emojis)} emoji -> {COLS*CELL}x{rows*CELL} atlas, {size//1024} KB")


if __name__ == "__main__":
    main()
