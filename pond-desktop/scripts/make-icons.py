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

Everything derives from the brand mark itself, which ships in two variants:
goose-logo.png is dark ink for light grounds, goose-logo-dark.png is white ink
for dark ones. Per DESIGN.md section 8 the logo is never stretched, skewed,
rotated, recoloured outside the palette, or given effects — so it is only ever
scaled proportionally and placed on a ground.

The app icon is the white mark on a near-black plate. Which mark goes on which
ground is the whole question here, and it took three attempts:

  * Originally the app icon was goose-logo-DARK with NO plate at all. White
    ink measured #FEFEFE on transparency, so against a light Dock, Finder
    window or Launchpad page nothing showed but the purple beak.

  * The fix for that put the dark mark on an off-white plate, which is legible
    everywhere but renders as a bright card on a dark desktop, matching
    neither the product's own dark UI nor the apps beside it.

  * Inverting it -- white mark, dark plate -- fixed the brightness but picked
    the plate from a product token, landing a shade LIGHTER than the Dock and
    purple where the Dock is grey. Read as a nested square: an icon inside an
    icon. See the note on GROUND, which is the part people get wrong.

The plate supplies the contrast the mark needs, so legibility never depends on
what is behind the icon; its COLOUR is chosen against the operating system's
chrome rather than the app's.

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
# The mark in both variants. The app icon takes the light-ink one because its
# plate is dark; the tray takes either, since a template image keeps only the
# alpha and the two variants share a silhouette.
MARK_ON_DARK = DESKTOP / "src/assets/goose-logo-dark.png"
MARK_ON_LIGHT = DESKTOP / "src/assets/goose-logo.png"
BUILD = DESKTOP / "build"
# Runtime assets, shipped inside the asar — see the note above.
ASSETS = DESKTOP / "electron/assets"

# macOS's own dark chrome value, and NEUTRAL -- deliberately not one of the
# product's dark surface tokens.
#
# The plate has to sit against the Dock, Launchpad and the app switcher, not
# against the app's own UI. Using --color-surface (#1E1B26) put a plate on
# screen that was both slightly LIGHTER than the Dock and purple where the Dock
# is grey, and the eye reads a near-match in value plus a mismatch in hue as two
# stacked rectangles -- a nested square, reported as "double colours between the
# outer and inner logo".
#
# Matching the Dock's own value instead makes the plate disappear on a dark Dock
# (the mark reads as floating, which is the uniform look) while still rendering
# as an ordinary dark rounded square against a light wallpaper, Finder or
# Launchpad. Legibility never depends on the background, because the plate is
# opaque and the mark is white.
#
# Do not reach for a product token here. They are tuned against the app's own
# chrome; this one is tuned against the operating system's.
GROUND = (0x1C, 0x1C, 0x1E, 255)

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


def trimmed_mark(source: Path) -> Image.Image:
    """The brand mark cropped to its own ink, so placement is predictable.

    The source is a 500x500 canvas with the mark occupying roughly the middle
    third vertically. Cropping to the alpha bounding box removes that padding
    without touching the artwork.
    """
    im = Image.open(source).convert("RGBA")
    box = im.getchannel("A").getbbox()
    if box is None:
        sys.exit(f"{source} has no visible pixels")
    return im.crop(box)


def rounded_rect_mask(size: int, radius: int) -> Image.Image:
    from PIL import ImageDraw

    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, size - 1, size - 1), radius, fill=255)
    return mask


def build_app_icon() -> Path:
    mark = trimmed_mark(MARK_ON_DARK)
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
    mark = trimmed_mark(MARK_ON_LIGHT)
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
