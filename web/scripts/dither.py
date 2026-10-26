"""Generate the website's dithered artwork into public/dither/.

Run from the repository root:  python web/scripts/dither.py
Requires Pillow and NumPy. The outputs are committed; the site build does not run this.
"""
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "web" / "public" / "dither"
OUT.mkdir(parents=True, exist_ok=True)

NIGHT = (11, 14, 16)
DEEP = (13, 58, 72)
TEAL = (23, 161, 204)
YELLOW = (255, 205, 79)


def bayer(n: int) -> np.ndarray:
    """Normalised Bayer threshold matrix of size n (a power of two), values in [0, 1)."""
    m = np.array([[0, 2], [3, 1]])
    while m.shape[0] < n:
        m = np.block([[4 * m, 4 * m + 2], [4 * m + 3, 4 * m + 1]])
    return (m + 0.5) / m.size


def atkinson(gray: np.ndarray, levels: int) -> np.ndarray:
    """Atkinson error diffusion of a [0, 1] image to `levels` evenly spaced steps."""
    img = gray.astype(float).copy()
    h, w = img.shape
    out = np.zeros_like(img, dtype=int)
    for y in range(h):
        for x in range(w):
            old = img[y, x]
            q = int(round(np.clip(old, 0, 1) * (levels - 1)))
            out[y, x] = q
            err = (old - q / (levels - 1)) / 8
            for dx, dy in ((1, 0), (2, 0), (-1, 1), (0, 1), (1, 1), (0, 2)):
                if 0 <= x + dx < w and y + dy < h:
                    img[y + dy, x + dx] += err
    return out


def folder_art(size: int, name: str) -> None:
    """The Foldex folder, Atkinson-dithered into grey steps with a teal tab."""
    src = Image.open(ROOT / "assets" / "brand" / "foldex.png").convert("RGBA")
    src = src.resize((size, size), Image.LANCZOS)
    a = np.asarray(src).astype(float) / 255
    rgb, alpha = a[..., :3], a[..., 3]
    luma = rgb @ np.array([0.299, 0.587, 0.114])
    blue = (rgb[..., 2] - rgb[..., 0]) > 0.12  # the blue tab of the artwork
    greys = [(46, 52, 56), (98, 108, 113), (160, 168, 172), (222, 227, 229)]
    q = atkinson(np.clip((luma - 0.25) / 0.6, 0, 1), len(greys))
    out = np.zeros((size, size, 4), dtype=np.uint8)
    for i, c in enumerate(greys):
        out[q == i, :3] = c
    tab = atkinson(np.clip((luma - 0.3) / 0.5, 0, 1), 2)
    out[blue & (tab == 1), :3] = TEAL
    out[blue & (tab == 0), :3] = DEEP
    out[..., 3] = np.where(alpha > 0.5, 255, 0)
    Image.fromarray(out, "RGBA").save(OUT / name, optimize=True)


def fade_mask(width: int = 8, height: int = 16, cell: int = 4) -> None:
    """Opaque at the top, transparent at the bottom, through an ordered dither.

    Used as a CSS mask so one asset serves every section transition colour. It is
    baked at display size (cell px per dot) because masks are scaled smoothly.
    """
    t = bayer(8)
    ys = (np.arange(height) + 0.5) / height
    level = 1 - ys[:, None]
    mask = level > np.tile(t, (height // 8 + 1, width // 8))[:height, :width]
    img = np.zeros((height, width, 4), dtype=np.uint8)
    img[..., 3] = mask * 255
    img = img.repeat(cell, axis=0).repeat(cell, axis=1)
    Image.fromarray(img, "RGBA").save(OUT / "fade.png", optimize=True)


def value_noise(w: int, h: int, scale: float, seed: int) -> np.ndarray:
    rng = np.random.default_rng(seed)
    gw, gh = int(w / scale) + 2, int(h / scale) + 2
    grid = rng.random((gh, gw))
    ys, xs = np.mgrid[0:h, 0:w] / scale
    x0, y0 = xs.astype(int), ys.astype(int)
    fx, fy = xs - x0, ys - y0
    fx, fy = fx * fx * (3 - 2 * fx), fy * fy * (3 - 2 * fy)
    a, b = grid[y0, x0], grid[y0, x0 + 1]
    c, d = grid[y0 + 1, x0], grid[y0 + 1, x0 + 1]
    return (a * (1 - fx) + b * fx) * (1 - fy) + (c * (1 - fx) + d * fx) * fy


def hero_field(w: int = 400, h: int = 240) -> None:
    """Static stand-in for the hero shader: same palette, field and Bayer 8x8."""
    v = 0.6 * value_noise(w, h, 60, 3) + 0.4 * value_noise(w, h, 24, 9)
    ys, xs = np.mgrid[0:h, 0:w]
    u, vv = xs / w, ys / h
    v *= np.clip(0.15 + 1.1 * u, 0, 1)  # calmer behind the headline on the left
    v *= np.clip(1.3 - 1.4 * np.hypot(u - 0.75, (vv - 0.55) * 1.2), 0, 1)
    t = np.tile(bayer(8), (h // 8 + 1, w // 8 + 1))[:h, :w]
    q = np.floor(np.clip(v, 0, 0.999) * 3.2 + t).astype(int).clip(0, 3)
    pal = np.array([NIGHT, DEEP, TEAL, YELLOW], dtype=np.uint8)
    Image.fromarray(pal[q], "RGB").save(OUT / "hero.png", optimize=True)


folder_art(64, "folder-64.png")
folder_art(160, "folder-160.png")
fade_mask()
hero_field()
print("wrote", sorted(p.name for p in OUT.iterdir()))
