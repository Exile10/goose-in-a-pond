#!/usr/bin/env python3
"""Generate the desktop app's icons from the GIAP brand mark.

Run from pond-desktop/:   python3 scripts/make-icons.py

Outputs (all committed, so nobody has to run this):
    build/icon.png                       1024x1024 master, for the packager
    build/icon.icns                      multi-resolution macOS icon
    electron/assets/trayTemplate.png     menu bar icon, @1x
    electron/assets/trayTemplate@2x.png  menu bar icon, @2x
    electron/assets/about.png            icon for the About panel

Note where the tray icons go, because it matters. electron-builder treats
`build/` as buildResources: it is read at PACKAGING time and never copied into
the app. An icon the main process loads at RUNTIME has to ship inside the
asar, so it lives under electron/ and is listed in electron-builder.yml's
`files`. Putting it in build/ works in dev — app.getAppPath() is the project
directory there — and silently produces a tray with no icon once packaged.

Everything derives from src/assets/goose-logo.png, the brand mark itself. Per
DESIGN.md section 8 the logo is never stretched, skewed, rotated, recoloured
outside the palette, or given effects — so it is only ever scaled
proportionally and placed on a ground.

Two things this fixes, both of which shipped:

  * The app icon was goose-logo-DARK — the white-on-transparent variant meant
    for dark grounds. Its ink measured #E0E0E0, so on a light Dock or in
    Finder it was very nearly invisible.

  * The tray icon was a solid opaque purple square with no mark in it at all.
    Since the main process calls setTemplateImage(true), macOS discarded the
    colour and drew a solid black block in the menu bar.

The tray icon is a TEMPLATE image, which is why it is pure black plus alpha:
macOS ignores the colour entirely and re-tints the alpha to match the menu
bar, so it stays legible in light mode, dark mode, and when the menu is
highlighted. Giving it colour is what produces the black block.
"""

import subprocess
import sys
from pathlib import Path

try:
    from PIL import Image
except ImportError:
    sys.exit("This script needs Pillow: python3 -m pip install Pillow")

HERE = Path(__file__).resolve().parent
DESKTOP = HERE.parent
SOURCE = DESKTOP / "src/assets/goose-logo.png"
BUILD = DESKTOP / "build"
# Runtime assets, shipped inside the asar — see the note above.
ASSETS = DESKTOP / "electron/assets"

# --color-content from src/styles/design-tokens.css: the warm near-white the
# product actually renders on. A neutral ground, which is what DESIGN.md
# permits for the black mark.
GROUND = (250, 250, 248, 255)

# Apple's icon grid: the artwork sits in a rounded rectangle whose corner
# radius is ~22.37% of the canvas, inset from the edges.
CORNER_RATIO = 0.2237
INSET_RATIO = 0.0977
# How much of the rounded rect's width the mark occupies. The mark is wide
# (about 1.7:1), so fitting it by width and letting it sit short is correct;
# forcing it taller would crop or distort it.
MARK_WIDTH_RATIO = 0.74

MASTER = 1024
ICNS_SIZES = [16, 32, 64, 128, 256, 512, 1024]
# Menu bar icons are budgeted about 16pt of height on macOS.
TRAY_HEIGHT = 16


def trimmed_mark() -> Image.Image:
    """The brand mark cropped to its own ink, so placement is predictable.

    The source is a 500x500 canvas with the mark occupying roughly the middle
    third vertically. Cropping to the alpha bounding box removes that padding
    without touching the artwork.
    """
    im = Image.open(SOURCE).convert("RGBA")
    box = im.getchannel("A").getbbox()
    if box is None:
        sys.exit(f"{SOURCE} has no visible pixels")
    return im.crop(box)


def rounded_rect_mask(size: int, radius: int) -> Image.Image:
    from PIL import ImageDraw

    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, size - 1, size - 1), radius, fill=255)
    return mask


def build_app_icon() -> Path:
    mark = trimmed_mark()
    canvas = Image.new("RGBA", (MASTER, MASTER), (0, 0, 0, 0))

    inset = round(MASTER * INSET_RATIO)
    plate_size = MASTER - inset * 2
    plate = Image.new("RGBA", (plate_size, plate_size), GROUND)
    plate.putalpha(rounded_rect_mask(plate_size, round(plate_size * CORNER_RATIO)))
    canvas.paste(plate, (inset, inset), plate)

    target_w = round(plate_size * MARK_WIDTH_RATIO)
    target_h = round(mark.height * target_w / mark.width)
    mark = mark.resize((target_w, target_h), Image.LANCZOS)

    # Optically centred rather than measured-centred: the mark's water line
    # sits at its foot, so dead-centring it reads as slightly low.
    x = (MASTER - target_w) // 2
    y = (MASTER - target_h) // 2 - round(MASTER * 0.012)
    canvas.paste(mark, (x, y), mark)

    out = BUILD / "icon.png"
    canvas.save(out)
    return out


def build_icns(master: Path) -> Path:
    """A real multi-resolution .icns, rather than letting the packager guess."""
    iconset = BUILD / "icon.iconset"
    if iconset.exists():
        for f in iconset.iterdir():
            f.unlink()
    iconset.mkdir(exist_ok=True)

    src = Image.open(master)
    for size in ICNS_SIZES:
        src.resize((size, size), Image.LANCZOS).save(iconset / f"icon_{size}x{size}.png")
        # The @2x of each size is the next size up, which is what iconutil wants.
        if size * 2 in ICNS_SIZES or size <= 512:
            src.resize((size * 2, size * 2), Image.LANCZOS).save(
                iconset / f"icon_{size}x{size}@2x.png"
            )

    out = BUILD / "icon.icns"
    subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(out)], check=True)
    for f in iconset.iterdir():
        f.unlink()
    iconset.rmdir()
    return out


def build_about(master: Path) -> Path:
    """The About panel's icon. Square, and shipped inside the asar."""
    out = ASSETS / "about.png"
    Image.open(master).resize((256, 256), Image.LANCZOS).save(out)
    return out


def build_tray() -> list[Path]:
    """Black-plus-alpha silhouettes for the macOS menu bar."""
    mark = trimmed_mark()
    written = []
    for scale, name in ((1, "trayTemplate.png"), (2, "trayTemplate@2x.png")):
        h = TRAY_HEIGHT * scale
        w = round(mark.width * h / mark.height)
        small = mark.resize((w, h), Image.LANCZOS)
        # A template image carries shape only: keep the alpha, discard every
        # colour including the purple accent, which macOS would drop anyway.
        black = Image.new("RGBA", small.size, (0, 0, 0, 255))
        black.putalpha(small.getchannel("A"))
        out = ASSETS / name
        black.save(out)
        written.append(out)
    return written


def main() -> None:
    BUILD.mkdir(exist_ok=True)
    ASSETS.mkdir(parents=True, exist_ok=True)
    master = build_app_icon()
    icns = build_icns(master)
    about = build_about(master)
    trays = build_tray()
    for path in [master, icns, about, *trays]:
        print(f"  {path.relative_to(DESKTOP)}  ({path.stat().st_size:,} bytes)")


if __name__ == "__main__":
    main()
