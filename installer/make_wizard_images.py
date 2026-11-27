"""Generate modern HiDPI BMP artwork for the Lumen Inno Setup 6 installer.

Outputs 24-bit RGB BMPs (required by Inno Setup):
- installer/assets/wizard-side.bmp  (328x628, 2x of 164x314)
- installer/assets/wizard-small.bmp (110x116, 2x of 55x58)

Usage: python installer/make_wizard_images.py
"""

from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "installer" / "assets"
ICON_PATH = ROOT / "assets" / "lumen.png"


def make_side() -> Image.Image:
    w, h = 328, 628
    # Deep Fluent 2 slate-to-indigo vertical gradient.
    img = Image.new("RGBA", (w, h), (15, 20, 30, 255))
    px = img.load()
    top = (18, 27, 44)
    bot = (10, 15, 24)
    for y in range(h):
        t = y / (h - 1)
        r = round(top[0] + (bot[0] - top[0]) * t)
        g = round(top[1] + (bot[1] - top[1]) * t)
        b = round(top[2] + (bot[2] - top[2]) * t)
        for x in range(w):
            px[x, y] = (r, g, b, 255)

    # Soft Fluent blue radial glow behind the icon.
    glow = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    gd.ellipse([-40, 60, w + 40, 420], fill=(0, 120, 212, 95))
    gd.ellipse([40, 430, w + 90, 670], fill=(56, 152, 255, 45))
    glow = glow.filter(ImageFilter.GaussianBlur(radius=48))
    img = Image.alpha_composite(img, glow)

    # Subtle accent bar at top and fine right edge divider.
    overlay = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    od = ImageDraw.Draw(overlay)
    od.rectangle([0, 0, w, 5], fill=(0, 120, 212, 255))
    od.rectangle([w - 1, 0, w, h], fill=(255, 255, 255, 22))
    # Minimal geometric workbench grid lines in lower half.
    for gx in range(36, w - 20, 44):
        od.line([(gx, 390), (gx, h - 36)], fill=(255, 255, 255, 10), width=1)
    for gy in range(390, h - 20, 44):
        od.line([(36, gy), (w - 36, gy)], fill=(255, 255, 255, 10), width=1)
    img = Image.alpha_composite(img, overlay)

    # Place Lumen app icon with soft drop shadow.
    icon = Image.open(ICON_PATH).convert("RGBA").resize((132, 132), Image.LANCZOS)
    ix = (w - icon.width) // 2
    iy = 156
    shadow = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    sd = ImageDraw.Draw(shadow)
    sd.rounded_rectangle([ix + 2, iy + 10, ix + icon.width + 2, iy + icon.height + 10], radius=28, fill=(0, 0, 0, 110))
    shadow = shadow.filter(ImageFilter.GaussianBlur(radius=16))
    img = Image.alpha_composite(img, shadow)
    img.alpha_composite(icon, (ix, iy))

    return img.convert("RGB")


def make_small() -> Image.Image:
    w, h = 110, 116
    img = Image.new("RGBA", (w, h), (255, 255, 255, 255))
    icon = Image.open(ICON_PATH).convert("RGBA").resize((84, 84), Image.LANCZOS)
    img.alpha_composite(icon, ((w - icon.width) // 2, (h - icon.height) // 2))
    return img.convert("RGB")


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    side_path = OUT_DIR / "wizard-side.bmp"
    small_path = OUT_DIR / "wizard-small.bmp"
    make_side().save(side_path, "BMP")
    make_small().save(small_path, "BMP")
    print(f"wrote {side_path} and {small_path}")


if __name__ == "__main__":
    main()
