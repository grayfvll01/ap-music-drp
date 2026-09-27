"""Regenerates the app icon: assets/icon.ico (exe, tray, installer) and
assets/logo.png (README). Requires Pillow.

    python scripts/make_icon.py

Design: a rounded square fading from music pink-red to Discord-style
blurple, a white beamed note, and a green "online" presence dot cut into the
corner, the way Discord shows a status on an avatar.
"""
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

S = 1024  # supersampled canvas
ASSETS = Path(__file__).resolve().parent.parent / "assets"

PINK = (255, 60, 110)
BLURPLE = (88, 101, 242)
GREEN = (35, 196, 94)


def gradient() -> Image.Image:
    """Diagonal pink (top-left) to blurple (bottom-right)."""
    small = Image.new("RGB", (64, 64))
    px = small.load()
    for y in range(64):
        for x in range(64):
            t = (x + y) / 126
            px[x, y] = tuple(round(a + (b - a) * t) for a, b in zip(PINK, BLURPLE))
    return small.resize((S, S), Image.BICUBIC).convert("RGBA")


def note(d: ImageDraw.ImageDraw, ox: float, oy: float, k: float) -> None:
    """Two beamed eighth notes; (ox, oy) offset and k scale in canvas units."""
    w = (255, 255, 255, 255)
    stem = 0.058 * k
    l_x, r_x = ox + 0.37 * k, ox + 0.69 * k
    l_top, r_top, beam = oy + 0.22 * k, oy + 0.16 * k, 0.13 * k
    d.polygon([(l_x - stem, l_top), (r_x, r_top), (r_x, r_top + beam), (l_x - stem, l_top + beam)], fill=w)
    d.rectangle((l_x - stem, l_top, l_x, oy + 0.69 * k), fill=w)
    d.rectangle((r_x - stem, r_top, r_x, oy + 0.63 * k), fill=w)
    for cx, cy in ((l_x - 0.105 * k, oy + 0.70 * k), (r_x - 0.105 * k, oy + 0.64 * k)):
        rx, ry = 0.125 * k, 0.095 * k
        d.ellipse((cx - rx, cy - ry, cx + rx, cy + ry), fill=w)


def render() -> Image.Image:
    shape = Image.new("L", (S, S), 0)
    ImageDraw.Draw(shape).rounded_rectangle((0, 0, S - 1, S - 1), radius=S * 0.24, fill=255)

    # Presence dot in the bottom-right corner, with a transparent ring cut
    # out of the tile around it.
    r_dot, gap = S * 0.17, S * 0.055
    cx = cy = S - r_dot - S * 0.035
    cut = Image.new("L", (S, S), 0)
    ImageDraw.Draw(cut).ellipse((cx - r_dot - gap, cy - r_dot - gap, cx + r_dot + gap, cy + r_dot + gap), fill=255)
    shape = ImageChops.subtract(shape, cut)

    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    img.paste(gradient(), (0, 0), shape)
    d = ImageDraw.Draw(img)
    note(d, S * 0.02, S * 0.02, S * 0.88)
    d.ellipse((cx - r_dot, cy - r_dot, cx + r_dot, cy + r_dot), fill=GREEN + (255,))
    return img


def main() -> None:
    big = render()
    sizes = [16, 20, 24, 32, 40, 48, 64]
    frames = [big.resize((n, n), Image.LANCZOS) for n in sizes]
    ASSETS.mkdir(parents=True, exist_ok=True)
    ico = ASSETS / "icon.ico"
    frames[-1].save(ico, format="ICO", sizes=[(n, n) for n in sizes], append_images=frames[:-1])
    big.resize((256, 256), Image.LANCZOS).save(ASSETS / "logo.png", optimize=True)
    print(ico, ico.stat().st_size, "bytes")


if __name__ == "__main__":
    main()
