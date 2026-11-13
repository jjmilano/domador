"""Genera los iconos de Domador (estático y frames de la bandeja).

Uso:
    python3 tools/make_icon.py

No usa fuentes ni dependencias fuera de Pillow/numpy. Dibuja a 1024 px y
reduce con LANCZOS para que las versiones de 16/24 px sigan leyéndose.
"""

from __future__ import annotations

import math
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
TRAY = ASSETS / "tray"
S = 1024
CENTER = (512, 545)

BG1 = (37, 24, 84)       # violeta profundo
BG2 = (10, 132, 255)      # azul eléctrico
GOLD = (255, 190, 61)
GOLD_DARK = (214, 126, 22)
CREAM = (255, 232, 189)
INK = (24, 17, 46)
CYAN = (63, 231, 255)


def rounded_mask(size: int, radius: int) -> Image.Image:
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle((18, 18, size - 18, size - 18), radius, fill=255)
    return mask


def gradient(size: int, top=(37, 24, 84), bottom=(10, 132, 255)) -> Image.Image:
    y, x = np.mgrid[0:size, 0:size].astype(np.float32)
    t = (x * 0.35 + y * 0.65) / max(size - 1, 1)
    arr = np.zeros((size, size, 3), dtype=np.uint8)
    for i in range(3):
        arr[:, :, i] = (top[i] + (bottom[i] - top[i]) * t).astype(np.uint8)
    return Image.fromarray(arr, "RGB").convert("RGBA")


def base(glow: float = 1.0) -> Image.Image:
    bg = gradient(S)
    # Halo interior: le da volumen al cuadro sin depender del tema de Windows.
    halo = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    hd = ImageDraw.Draw(halo)
    hd.ellipse((70, -160, S - 70, 600), fill=(255, 255, 255, int(36 * glow)))
    bg = Image.alpha_composite(bg, halo.filter(ImageFilter.GaussianBlur(70)))
    bg.putalpha(rounded_mask(S, 218))
    return bg


def draw_bolt(draw: ImageDraw.ImageDraw, cx: float, cy: float, h: float, color, outline=INK):
    w = h * 0.52
    pts = [
        (cx + w * 0.10, cy - h / 2),
        (cx - w * 0.42, cy + h * 0.06),
        (cx - w * 0.05, cy + h * 0.06),
        (cx - w * 0.23, cy + h / 2),
        (cx + w * 0.44, cy - h * 0.10),
        (cx + w * 0.04, cy - h * 0.10),
    ]
    draw.polygon(pts, fill=color, outline=outline)


def draw_lion_simple(glow: float = 1.0, blink: bool = False) -> Image.Image:
    """Versión de 16/20/24 px: sin puntas finas ni detalles que se pierdan."""
    img = base(glow)
    d = ImageDraw.Draw(img)
    cx, cy = 512, 535
    # Melena redonda, sin picos: a 16 px los picos se convierten en ruido.
    d.ellipse((cx - 370, cy - 370, cx + 370, cy + 370), fill=GOLD, outline=INK, width=24)
    for ex in (-230, 230):
        d.ellipse((cx + ex - 95, cy - 275, cx + ex + 95, cy - 90), fill=GOLD_DARK, outline=INK, width=20)
        d.ellipse((cx + ex - 52, cy - 225, cx + ex + 52, cy - 125), fill=(255, 151, 131))
    d.ellipse((cx - 250, cy - 255, cx + 250, cy + 255), fill=CREAM, outline=INK, width=24)
    for ex in (-105, 105):
        if blink:
            d.line((cx + ex - 52, cy - 20, cx + ex + 52, cy - 20), fill=INK, width=30)
        else:
            d.ellipse((cx + ex - 52, cy - 78, cx + ex + 52, cy + 38), fill=INK)
            d.ellipse((cx + ex - 30, cy - 55, cx + ex + 8, cy - 15), fill=(255, 255, 255))
            d.ellipse((cx + ex + 10, cy + 2, cx + ex + 28, cy + 20), fill=(140, 230, 255))
    d.ellipse((cx - 120, cy + 60, cx + 120, cy + 200), fill=(255, 210, 160))
    d.polygon([(cx - 45, cy + 92), (cx + 45, cy + 92), (cx, cy + 140)], fill=INK)
    d.arc((cx - 108, cy + 105, cx + 108, cy + 230), 25, 155, fill=INK, width=15)
    return img


def draw_lion(glow: float = 1.0, blink: bool = False, bolt_alpha: int = 255) -> Image.Image:
    img = base(glow)
    d = ImageDraw.Draw(img)
    cx, cy = CENTER

    # Melena: puntas redondeadas alrededor de la cara.
    mane = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    md = ImageDraw.Draw(mane)
    for i in range(18):
        a = (i / 18) * math.tau - math.pi / 2
        a2 = a + math.tau / 36
        p1 = (cx + math.cos(a) * 270, cy + math.sin(a) * 270)
        p2 = (cx + math.cos(a - 0.08) * 425, cy + math.sin(a - 0.08) * 425)
        p3 = (cx + math.cos(a2) * 405, cy + math.sin(a2) * 405)
        color = GOLD if i % 2 == 0 else GOLD_DARK
        md.polygon([p1, p2, p3], fill=color)
    mane = mane.filter(ImageFilter.GaussianBlur(2))
    img = Image.alpha_composite(img, mane)
    d = ImageDraw.Draw(img)
    d.ellipse((cx - 330, cy - 330, cx + 330, cy + 330), fill=GOLD, outline=INK, width=18)
    d.ellipse((cx - 300, cy - 300, cx + 300, cy + 300), fill=GOLD)

    # Orejas y cara.
    for ex in (-235, 235):
        d.ellipse((cx + ex - 82, cy - 265, cx + ex + 82, cy - 110), fill=GOLD_DARK, outline=INK, width=16)
        d.ellipse((cx + ex - 45, cy - 225, cx + ex + 45, cy - 145), fill=(255, 151, 131))
    d.ellipse((cx - 245, cy - 250, cx + 245, cy + 250), fill=CREAM, outline=INK, width=18)

    # Ojos grandes de mascota.
    for ex in (-95, 95):
        if blink:
            d.line((cx + ex - 42, cy - 18, cx + ex + 42, cy - 18), fill=INK, width=24)
        else:
            d.ellipse((cx + ex - 40, cy - 62, cx + ex + 40, cy + 30), fill=INK)
            d.ellipse((cx + ex - 24, cy - 45, cx + ex + 5, cy - 15), fill=(255, 255, 255))
            d.ellipse((cx + ex + 8, cy - 2, cx + ex + 22, cy + 12), fill=(140, 230, 255))

    # Hocico, nariz y sonrisa.
    d.ellipse((cx - 118, cy + 55, cx + 118, cy + 190), fill=(255, 210, 160))
    d.polygon([(cx - 43, cy + 88), (cx + 43, cy + 88), (cx, cy + 135)], fill=INK)
    d.arc((cx - 105, cy + 105, cx + 105, cy + 225), 25, 155, fill=INK, width=13)
    d.line((cx, cy + 135, cx, cy + 168), fill=INK, width=10)

    # Rayo en la frente: el detalle "Domador".
    if bolt_alpha > 0:
        layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
        ld = ImageDraw.Draw(layer)
        draw_bolt(ld, cx, cy - 175, 105, CYAN + (bolt_alpha,))
        img = Image.alpha_composite(img, layer)

    # Brillo superior.
    shine = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    sd = ImageDraw.Draw(shine)
    sd.ellipse((130, 70, 650, 320), fill=(255, 255, 255, 30))
    img = Image.alpha_composite(img, shine.filter(ImageFilter.GaussianBlur(45)))
    return img


def draw_paw(glow: float = 1.0, bolt_alpha: int = 255) -> Image.Image:
    img = base(glow)
    d = ImageDraw.Draw(img)
    cx, cy = 512, 565

    # Almohadilla y dedos.
    d.ellipse((cx - 220, cy - 60, cx + 220, cy + 330), fill=CREAM, outline=INK, width=22)
    for i, (x, y, rx, ry) in enumerate(
        [(-270, -230, 92, 122), (-105, -315, 92, 126), (105, -315, 92, 126), (270, -230, 92, 122)]
    ):
        d.ellipse((cx + x - rx, cy + y - ry, cx + x + rx, cy + y + ry), fill=CREAM, outline=INK, width=22)

    # Rayo recortado sobre la almohadilla.
    layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ld = ImageDraw.Draw(layer)
    draw_bolt(ld, cx, cy + 105, 250, CYAN + (bolt_alpha,))
    img = Image.alpha_composite(img, layer)
    return img


def contact_sheet(img: Image.Image, path: Path) -> None:
    sizes = [256, 128, 64, 32, 24, 16]
    pad = 24
    sheet = Image.new("RGBA", (sum(s + pad for s in sizes) + pad, 256 + pad * 2), (16, 18, 26, 255))
    x = pad
    for size in sizes:
        small = img.resize((size, size), Image.Resampling.LANCZOS)
        sheet.alpha_composite(small, (x, pad + (256 - size) // 2))
        x += size + pad
    sheet.save(path)


def ico_frames(img: Image.Image) -> list[Image.Image]:
    # El tray usa 16/20/24/32; el .ico estático incluye 256 para Explorer.
    return [img.resize((s, s), Image.Resampling.LANCZOS) for s in (16, 20, 24, 32, 40, 48, 64, 128, 256)]


def main() -> None:
    ASSETS.mkdir(exist_ok=True)
    TRAY.mkdir(exist_ok=True)

    lion = draw_lion()
    simple = draw_lion_simple()

    # Icono estático: simplificado en 16/20/24/32 y detallado desde 48 px.
    # Windows elige el tamaño según DPI; así el tray no se ensucia y el
    # Explorador conserva la melena con puntas.
    small_sizes = [16, 20, 24, 32]
    large_sizes = [48, 64, 128, 256]
    images = [simple.resize((s, s), Image.Resampling.LANCZOS) for s in small_sizes]
    images += [lion.resize((s, s), Image.Resampling.LANCZOS) for s in large_sizes]
    sizes = small_sizes + large_sizes
    images[-1].save(
        ASSETS / "domador.ico",
        format="ICO",
        sizes=[(s, s) for s in sizes],
        append_images=images[:-1],
    )
    lion.resize((512, 512), Image.Resampling.LANCZOS).save(ASSETS / "domador.png")
    simple.resize((256, 256), Image.Resampling.LANCZOS).save(ASSETS / "icon-256.png")
    simple.resize((64, 64), Image.Resampling.LANCZOS).save(ASSETS / "icon-64.png")
    simple.resize((32, 32), Image.Resampling.LANCZOS).save(ASSETS / "icon-32.png")
    contact_sheet(lion, ASSETS / "preview-lion.png")
    contact_sheet(simple, ASSETS / "preview-simple.png")

    paw = draw_paw()
    contact_sheet(paw, ASSETS / "preview-paw.png")

    # 8 frames de bandeja: 16/20/24/32 con la cara simple. El pulso de halo
    # se nota sin mover la silueta; el parpadeo cae en el frame 6.
    frames = []
    for i in range(8):
        phase = i / 8 * math.tau
        glow = 0.55 + 0.45 * (0.5 + 0.5 * math.sin(phase))
        blink = i == 6
        frame = draw_lion_simple(glow=glow, blink=blink)
        frame.save(TRAY / f"anim{i:02}.png")
        frame.resize((32, 32), Image.Resampling.LANCZOS).save(
            TRAY / f"anim{i:02}.ico",
            format="ICO",
            sizes=[(16, 16), (20, 20), (24, 24), (32, 32)],
        )
        frames.append(frame)

    contact_sheet(frames[0], ASSETS / "preview-anim.png")
    print("listo:", ASSETS / "domador.ico")


if __name__ == "__main__":
    main()
