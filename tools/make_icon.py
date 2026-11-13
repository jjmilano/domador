"""Domador — identidad visual steampunk (león domador con antiparras de latón).

Genera, sin fuentes ni assets externos (sólo Pillow + numpy):

    assets/domador.png           master 1024 px
    assets/domador.ico           .ico multirresolución 16/20/24/32/48/64/128/256
    assets/icon-32/64/256.png    favicon y logo de la UI
    assets/favicon.ico           16/32/48
    assets/tray/anim00..07.ico   8 frames animados (16/20/24/32) + .png 256
    docs/icon-preview.png        hoja de contactos

Motivo: medallón de latón envejecido (engranaje + esfera de manómetro) sobre
carbón, león con antiparras de latón y correas de cuero, rayo de cobre.

Dos maquetas, no sólo dos niveles de detalle:
  * FULL  (>=32 px): medallón, graduaciones, remaches, correas, bigotes.
  * SMALL (16/20/24 px): menos dientes, melena más simple, cara y antiparras
    más grandes — lo que sobrevive a 16 px.

Todo se dibuja a 2048 px (2x) con coordenadas de un espacio de diseño de
1024 px y se reduce con LANCZOS.

Uso:
    python3 tools/make_icon.py
"""

from __future__ import annotations

import io
import math
import struct
from pathlib import Path

import numpy as np
from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
TRAY = ASSETS / "tray"
DOCS = ROOT / "docs"

DESIGN = 1024           # espacio de diseño
SS = 2                  # supersampling
PX = DESIGN * SS

# --------------------------------------------------------------------------
# Paleta: carbón cálido, latón envejecido, cobre y marfil.
# --------------------------------------------------------------------------
COAL_0 = (58, 47, 39)
COAL_1 = (11, 10, 9)
BRASS_HI = (255, 240, 196)
BRASS_LT = (233, 190, 118)
BRASS = (182, 132, 62)
BRASS_DK = (112, 75, 33)
BRASS_SH = (50, 32, 15)
COPPER_HI = (255, 218, 168)
COPPER_LT = (246, 152, 82)
COPPER = (182, 82, 38)
IVORY = (248, 236, 214)
IVORY_DK = (196, 164, 126)
LEATHER = (72, 45, 31)
LEATHER_DK = (28, 18, 13)
AMBER = (238, 158, 74)
INK = (17, 11, 9)

ROUND = 206.0
PAD = 8.0

# --------------------------------------------------------------------------
# Maquetas (coordenadas en el espacio de 1024 px).
# --------------------------------------------------------------------------
FULL = dict(
    badge=(512.0, 486.0), teeth_tip=420.0, teeth_root=376.0,
    ring_out=384.0, ring_in=340.0, dial=340.0, n_teeth=9,
    head=(512.0, 572.0), mane_tip=268.0, mane_alt=246.0, mane_blob=12.0,
    mane_disc=204.0, mane_n=13,
    face=(512.0, 592.0), face_rx=184.0, face_ry=192.0,
    eye_dx=86.0, eye_y=534.0, rim_out=80.0, rim_in=58.0,
    bolt=(518.0, 208.0, 118.0, 96.0),
)

SMALL = dict(
    badge=(512.0, 500.0), teeth_tip=432.0, teeth_root=392.0,
    ring_out=398.0, ring_in=352.0, dial=352.0, n_teeth=9,
    head=(512.0, 586.0), mane_tip=328.0, mane_alt=306.0, mane_blob=0.0,
    mane_disc=248.0, mane_n=10,
    face=(512.0, 610.0), face_rx=224.0, face_ry=230.0,
    eye_dx=108.0, eye_y=546.0, rim_out=98.0, rim_in=74.0,
    bolt=(518.0, 196.0, 90.0, 96.0),
)

# 16 px: una sola forma por idea. Sin correas, sin orejas, sin dientes
# profundos: lo único que sobrevive es el aro, la cara y las antiparras.
NANO = dict(
    badge=(512.0, 500.0), teeth_tip=436.0, teeth_root=406.0,
    ring_out=436.0, ring_in=390.0, dial=390.0, n_teeth=6,
    head=(512.0, 590.0), mane_tip=336.0, mane_alt=336.0, mane_blob=0.0,
    mane_disc=296.0, mane_n=6,
    face=(512.0, 608.0), face_rx=244.0, face_ry=248.0,
    eye_dx=114.0, eye_y=544.0, rim_out=106.0, rim_in=82.0,
    bolt=(518.0, 196.0, 76.0, 112.0),
    straps=False, ears=False,
)

_X = _Y = None


# --------------------------------------------------------------------------
# utilidades de rasterizado
# --------------------------------------------------------------------------
def sc(*v):
    return [x * SS for x in v]


def wd(v):
    return max(1, int(round(v * SS)))


def grids():
    global _X, _Y
    if _X is None:
        y, x = np.mgrid[0:PX, 0:PX].astype(np.float32)
        _X, _Y = x, y
    return _X, _Y


def _rgba(t, c0, c1):
    arr = np.empty((PX, PX, 4), dtype=np.float32)
    for i in range(4):
        a = c0[i] if len(c0) > i else 255
        b = c1[i] if len(c1) > i else 255
        arr[..., i] = a + (b - a) * t
    return Image.fromarray(arr.clip(0, 255).astype(np.uint8), "RGBA")


def radial(cx, cy, r, c0, c1):
    x, y = grids()
    t = np.sqrt((x - cx * SS) ** 2 + (y - cy * SS) ** 2) / max(r * SS, 1.0)
    return _rgba(np.clip(t, 0.0, 1.0), c0, c1)


def linear(p0, p1, c0, c1):
    x, y = grids()
    dx, dy = (p1[0] - p0[0]) * SS, (p1[1] - p0[1]) * SS
    t = ((x - p0[0] * SS) * dx + (y - p0[1] * SS) * dy) / max(dx * dx + dy * dy, 1.0)
    return _rgba(np.clip(t, 0.0, 1.0), c0, c1)


def solid(color):
    return Image.new("RGBA", (PX, PX), color)


def layer():
    return Image.new("RGBA", (PX, PX), (0, 0, 0, 0))


def mask_l():
    return Image.new("L", (PX, PX), 0)


def paint(base, mask, img_or_color):
    base.paste(img_or_color, (0, 0), mask)


def ring_mask(cx, cy, r_in, r_out):
    m = mask_l()
    d = ImageDraw.Draw(m)
    d.ellipse(sc(cx - r_out, cy - r_out, cx + r_out, cy + r_out), fill=255)
    if r_in > 0:
        d.ellipse(sc(cx - r_in, cy - r_in, cx + r_in, cy + r_in), fill=0)
    return m


def disc_mask(cx, cy, rx, ry=None):
    ry = rx if ry is None else ry
    m = mask_l()
    ImageDraw.Draw(m).ellipse(sc(cx - rx, cy - ry, cx + rx, cy + ry), fill=255)
    return m


def add_glow(img, lay, blur, alpha=255):
    g = lay.filter(ImageFilter.GaussianBlur(blur * SS))
    if alpha < 255:
        g.putalpha(g.getchannel("A").point(lambda v: int(v * alpha / 255)))
    return Image.alpha_composite(img, g)


def flat(pts):
    return [c for xy in pts for c in sc(*xy)]


def arc_line(d, cx, cy, r, a0, a1, col, width):
    d.arc(sc(cx - r, cy - r, cx + r, cy + r), a0, a1, fill=col, width=wd(width))


def polar(cx, cy, a, r):
    return cx + math.cos(a) * r, cy + math.sin(a) * r


def bolt_points(cx, cy, height, width, tilt=0.0):
    """Rayo en zigzag, punta hacia arriba, inclinado `tilt` grados."""
    base = [
        (0.12, 1.00),
        (-0.46, 0.08),
        (-0.05, 0.03),
        (-0.24, -1.00),
        (0.46, -0.04),
        (0.05, 0.01),
    ]
    hx = width / 0.92
    hy = height / 2.0
    a = math.radians(tilt)
    ca, sa = math.cos(a), math.sin(a)
    out = []
    for x, y in base:
        x, y = x * hx, y * hy
        out.append((cx + x * ca + y * sa, cy + x * sa - y * ca))
    return out


# --------------------------------------------------------------------------
# fondo
# --------------------------------------------------------------------------
def draw_bg(P, detail: bool) -> Image.Image:
    img = linear((100, 20), (1010, 1015), COAL_0, COAL_1)
    img = Image.alpha_composite(img, radial(450, 360, 820, (176, 120, 58, 84), (0, 0, 0, 0)))
    img = Image.alpha_composite(img, radial(512, 480, 800, (0, 0, 0, 0), (0, 0, 0, 170)))

    if detail:
        # grano de metal: textura de latón/carbón sin ensuciar los bordes
        rng = np.random.default_rng(7)
        noise = rng.normal(0.0, 1.0, (PX // 2, PX // 2)).astype(np.float32)
        n_img = Image.fromarray(((noise * 26) + 128).clip(0, 255).astype(np.uint8), "L")
        n_img = n_img.resize((PX, PX), Image.Resampling.BILINEAR)
        arr = np.asarray(img, dtype=np.int16)
        n = (np.asarray(n_img, dtype=np.int16) - 128) // 8
        arr[..., :3] = np.clip(arr[..., :3] + n[..., None], 0, 255)
        img = Image.fromarray(arr.astype(np.uint8), "RGBA")

        rim = layer()
        rd = ImageDraw.Draw(rim)
        rd.rounded_rectangle(sc(PAD + 13, PAD + 13, DESIGN - PAD - 13, DESIGN - PAD - 13),
                             radius=wd(ROUND - 13), outline=BRASS_DK + (140,), width=wd(9))
        rd.rounded_rectangle(sc(PAD + 21, PAD + 21, DESIGN - PAD - 21, DESIGN - PAD - 21),
                             radius=wd(ROUND - 22), outline=BRASS_LT + (60,), width=wd(3))
        img = Image.alpha_composite(img, rim)
    return img


# --------------------------------------------------------------------------
# medallón: engranaje + esfera de manómetro
# --------------------------------------------------------------------------
def draw_gear(img: Image.Image, phase: float, P, detail: bool) -> Image.Image:
    cx, cy = P["badge"]
    n = P["n_teeth"]
    tip = P["teeth_tip"]
    root = P["teeth_root"]

    def tooth(a, r_root, r_tip, half_root, half_tip):
        return [
            (cx + math.cos(a - half_root) * r_root, cy + math.sin(a - half_root) * r_root),
            (cx + math.cos(a - half_tip) * r_tip, cy + math.sin(a - half_tip) * r_tip),
            (cx + math.cos(a + half_tip) * r_tip, cy + math.sin(a + half_tip) * r_tip),
            (cx + math.cos(a + half_root) * r_root, cy + math.sin(a + half_root) * r_root),
        ]

    half_root = math.radians(180.0 / n * 0.92)
    half_tip = math.radians(180.0 / n * 0.50)

    dark = layer()
    dd = ImageDraw.Draw(dark)
    body = layer()
    bd = ImageDraw.Draw(body)
    edge = layer()
    ed = ImageDraw.Draw(edge)
    for i in range(n):
        a = math.radians(-90.0 + phase + i * 360.0 / n)
        dd.polygon(flat(tooth(a, root, tip + 8, half_root + math.radians(1.2),
                              half_tip + math.radians(1.6))), fill=INK + (255,))
        bd.polygon(flat(tooth(a, root - 2, tip, half_root, half_tip)), fill=BRASS + (255,))
        p = tooth(a, root, tip - 6, half_root * 0.9, half_tip * 0.84)
        ed.line([sc(*p[1]), sc(*p[2])], fill=BRASS_HI + (180,), width=wd(8))
    img = Image.alpha_composite(img, dark)
    img = Image.alpha_composite(img, body)
    img = Image.alpha_composite(img, edge)

    # esfera del manómetro: oscura, pero no negra (si no, el contorno del
    # león desaparece contra el fondo)
    img.paste(radial(cx - 80, cy - 120, P["dial"] * 1.35, (70, 57, 46, 255), (28, 22, 19, 255)),
              (0, 0), disc_mask(cx, cy, P["dial"]))

    # aro de latón
    img.paste(radial(cx - 70, cy - 110, P["ring_out"] * 1.5, BRASS_HI + (255,), BRASS + (255,)),
              (0, 0), ring_mask(cx, cy, P["ring_in"], P["ring_out"]))

    ring = layer()
    rd = ImageDraw.Draw(ring)
    rd.ellipse(sc(cx - P["ring_out"], cy - P["ring_out"],
                  cx + P["ring_out"], cy + P["ring_out"]),
               outline=INK + (255,), width=wd(7))
    rd.ellipse(sc(cx - P["ring_in"], cy - P["ring_in"],
                  cx + P["ring_in"], cy + P["ring_in"]),
               outline=BRASS_SH + (255,), width=wd(9))
    arc_line(rd, cx, cy, P["ring_out"] - 7, 155, 305, BRASS_HI + (220,), 8)
    arc_line(rd, cx, cy, P["ring_in"] + 7, 165, 300, BRASS_SH + (150,), 5)
    img = Image.alpha_composite(img, ring)

    if detail:
        # remaches del aro
        rv = layer()
        rdv = ImageDraw.Draw(rv)
        mid = (P["ring_in"] + P["ring_out"]) / 2
        for i in range(n):
            a = math.radians(-90.0 + phase + i * 360.0 / n + 180.0 / n)
            x, y = cx + math.cos(a) * mid, cy + math.sin(a) * mid
            rdv.ellipse(sc(x - 11, y - 11, x + 11, y + 11), fill=BRASS_SH + (255,))
            rdv.ellipse(sc(x - 7.5, y - 7.5, x + 7.5, y + 7.5), fill=BRASS_HI + (255,))
            rdv.ellipse(sc(x - 3.5, y - 5, x + 1.5, y - 0.5), fill=(255, 252, 240, 240))
        img = Image.alpha_composite(img, rv)

        # graduaciones (fijas: el engranaje gira por detrás)
        ticks = layer()
        tk = ImageDraw.Draw(ticks)
        count = 12
        for i in range(count):
            a = math.radians(-90.0 + i * 360.0 / count)
            r0, r1 = P["dial"] - 50, P["dial"] - 10
            p0 = (cx + math.cos(a) * r0, cy + math.sin(a) * r0)
            p1 = (cx + math.cos(a) * r1, cy + math.sin(a) * r1)
            tk.line(sc(*p0, *p1), fill=BRASS_SH + (255,), width=wd(16))
            tk.line(sc(*p0, *p1), fill=BRASS_LT, width=wd(10))
        img = Image.alpha_composite(img, ticks)

        # reflejo de vidrio sobre la esfera
        gl = layer()
        gd = ImageDraw.Draw(gl)
        gd.polygon(sc(cx - 325, cy - 60, cx - 70, cy - 330, cx + 5, cy - 330, cx - 225, cy - 40),
                   fill=(255, 255, 255, 24))
        gl = gl.filter(ImageFilter.GaussianBlur(14 * SS))
        gl.putalpha(ImageChops.multiply(gl.getchannel("A"), disc_mask(cx, cy, P["dial"] - 8)))
        img = Image.alpha_composite(img, gl)
    else:
        # bisel simple en la esfera: una sola luz, sin graduaciones
        bz = layer()
        ImageDraw.Draw(bz).arc(sc(cx - P["dial"] + 12, cy - P["dial"] + 12,
                                 cx + P["dial"] - 12, cy + P["dial"] - 12),
                               200, 340, fill=BRASS_DK + (120,), width=wd(16))
        img = Image.alpha_composite(img, bz.filter(ImageFilter.GaussianBlur(6 * SS)))
    return img


def draw_bolt(img: Image.Image, P, detail: bool) -> Image.Image:
    cx, cy, height, width = P["bolt"]
    poly = flat(bolt_points(cx, cy, height, width, 10.0))

    l = layer()
    d = ImageDraw.Draw(l)
    d.polygon(poly, fill=COPPER + (255,))
    d.polygon(poly, outline=COPPER_LT + (255,), width=wd(9 if detail else 11))
    inner = bolt_points(cx - 5, cy + 3, height * 0.56, width * 0.44, 10.0)
    d.polygon(flat(inner), fill=COPPER_HI + (215,))

    edge = layer()
    ImageDraw.Draw(edge).polygon(poly, outline=INK + (255,), width=wd(11 if detail else 13))

    img = add_glow(img, l, 20 if detail else 24, alpha=100)
    img = Image.alpha_composite(img, edge)
    return Image.alpha_composite(img, l)


# --------------------------------------------------------------------------
# león
# --------------------------------------------------------------------------
def draw_mane(img: Image.Image, P, detail: bool) -> Image.Image:
    cx, cy = P["head"]
    n = P["mane_n"]
    tip_hi, tip_lo = P["mane_tip"], P["mane_alt"]
    blob = P["mane_blob"]
    disc = P["mane_disc"]

    def petal_masks(count, r_hi, r_lo, blob_r, base_r, grow, phase=0.0):
        even = layer()
        odd = layer()
        de = ImageDraw.Draw(even)
        do = ImageDraw.Draw(odd)
        for i in range(count):
            d = de if i % 2 == 0 else do
            a = math.radians(-90.0 + phase + i * 360.0 / count)
            half = math.radians(180.0 / count * 1.30)
            r = (r_hi if i % 2 == 0 else r_lo) + grow
            base = base_r - 6
            mid = base + (r - base) * 0.52
            p = [
                polar(cx, cy, a - half, base),
                polar(cx, cy, a - half * 0.42, mid),
                polar(cx, cy, a, r),
                polar(cx, cy, a + half * 0.42, mid),
                polar(cx, cy, a + half, base),
            ]
            d.polygon(flat(p), fill=(255, 255, 255, 255))
            if blob_r > 0:
                tx, ty = polar(cx, cy, a, r)
                rr = blob_r + grow * 0.5
                d.ellipse(sc(tx - rr, ty - rr, tx + rr, ty + rr), fill=(255, 255, 255, 255))
        return even.getchannel("A"), odd.getchannel("A")

    # contorno oscuro
    rim_e, rim_o = petal_masks(n, tip_hi, tip_lo, blob, disc - 20, 13.0)
    dark = layer()
    dark.paste(solid(INK), (0, 0), ImageChops.lighter(rim_e, rim_o))
    img = Image.alpha_composite(img, dark)

    if detail:
        mid_e, mid_o = petal_masks(n, tip_hi - 22, tip_lo - 22, max(blob - 4, 0),
                                   disc - 26, 0.0, phase=180.0 / n)
        mid = layer()
        mid.paste(linear((430, 330), (700, 900), BRASS, BRASS_DK), (0, 0),
                  ImageChops.lighter(mid_e, mid_o))
        img = Image.alpha_composite(img, mid)

    e, o = petal_masks(n, tip_hi, tip_lo, blob, disc - 20, 0.0)
    body = layer()
    body.paste(linear((410, 300), (690, 900), BRASS_HI, BRASS_DK), (0, 0),
               ImageChops.lighter(e, o))
    body.paste(solid((44, 28, 13, 36)), (0, 0), o)
    img = Image.alpha_composite(img, body)

    # disco de la melena
    img.paste(radial(cx - 74, cy - 112, disc * 1.8, BRASS_HI + (255,), BRASS_DK + (255,)),
              (0, 0), disc_mask(cx, cy, disc))

    gr = layer()
    gd = ImageDraw.Draw(gr)
    if detail:
        for i in range(n):
            a = math.radians(-90.0 + i * 360.0 / n + 180.0 / n)
            gd.line([sc(*polar(cx, cy, a, disc - 84)), sc(*polar(cx, cy, a, disc - 10))],
                    fill=BRASS_SH + (70,), width=wd(6))
    arc_line(gd, cx, cy, disc - 12, 205, 335, BRASS_HI + (85,), 8)
    img = Image.alpha_composite(img, gr.filter(ImageFilter.GaussianBlur(2.5 * SS)))

    # orejas triangulares (felinas), más oscuras que la melena para leerse
    ears = layer()
    ed = ImageDraw.Draw(ears)
    for sx in ((-1, 1) if P.get("ears", True) else ()):
        a = math.radians(-90 + sx * 44)
        x, y = polar(cx, cy, a, disc + 22)
        size = 66 if detail else 78
        for grow, col in ((15, INK + (255,)), (0, BRASS + (255,))):
            s = size + grow
            tip = polar(x, y, a, s * 1.12)
            l = polar(x, y, a - math.radians(62), s * 0.82)
            r = polar(x, y, a + math.radians(62), s * 0.82)
            ed.polygon(flat([l, tip, r]), fill=col)
            for p in (l, tip, r):
                rr = s * 0.19
                ed.ellipse(sc(p[0] - rr, p[1] - rr, p[0] + rr, p[1] + rr), fill=col)
        inner = polar(x, y, a, size * 0.52)
        il = polar(x, y, a - math.radians(50), size * 0.40)
        ir = polar(x, y, a + math.radians(50), size * 0.40)
        ed.polygon(flat([il, inner, ir]), fill=LEATHER_DK + (255,))
        ed.arc(sc(x - size * 0.72, y - size * 0.72, x + size * 0.72, y + size * 0.72),
               a * 57.3 - 52, a * 57.3 + 16, fill=BRASS_HI + (200,), width=wd(7))
    img = Image.alpha_composite(img, ears)
    return img


def draw_face(img: Image.Image, P, detail: bool) -> Image.Image:
    cx, cy = P["face"]
    rx, ry = P["face_rx"], P["face_ry"]

    dark = layer()
    dd = ImageDraw.Draw(dark)
    dd.ellipse(sc(cx - rx - 12, cy - ry - 12, cx + rx + 12, cy + ry + 12), fill=INK + (255,))
    dd.ellipse(sc(cx - rx * 0.72 - 12, cy + ry * 0.34 - 12,
                  cx + rx * 0.72 + 12, cy + ry * 0.98 + 12), fill=INK + (255,))
    img = Image.alpha_composite(img, dark)

    m = ImageChops.lighter(disc_mask(cx, cy, rx, ry),
                           disc_mask(cx, cy + ry * 0.44, rx * 0.72, ry * 0.52))
    paint(img, m, radial(cx - 66, cy - 108, (rx + ry) * 0.95, IVORY + (255,), IVORY_DK + (255,)))

    sh = layer()
    ImageDraw.Draw(sh).arc(sc(cx - rx + 16, cy - ry + 16, cx + rx - 16, cy + ry - 16),
                           26, 154, fill=(112, 76, 44, 175), width=wd(28))
    sh.putalpha(ImageChops.multiply(sh.getchannel("A"), m))
    img = Image.alpha_composite(img, sh.filter(ImageFilter.GaussianBlur(13 * SS)))

    # hocico
    mz = ImageChops.lighter(disc_mask(cx - rx * 0.30, cy + ry * 0.54, rx * 0.40, ry * 0.32),
                            disc_mask(cx + rx * 0.30, cy + ry * 0.54, rx * 0.40, ry * 0.32))
    paint(img, mz, radial(cx - 14, cy + ry * 0.24, rx * 1.05,
                          (253, 245, 228, 255), (206, 172, 132, 255)))

    # nariz
    ny = cy + ry * 0.25
    nose = layer()
    nd = ImageDraw.Draw(nose)
    nd.polygon(sc(cx - rx * 0.23, ny - 12, cx + rx * 0.23, ny - 12,
                  cx + rx * 0.10, ny + 27, cx - rx * 0.10, ny + 27),
               fill=LEATHER_DK + (255,))
    nd.arc(sc(cx - rx * 0.23, ny - 28, cx + rx * 0.23, ny + 10), 196, 344,
           fill=BRASS_LT + (150,), width=wd(6))
    img = Image.alpha_composite(img, nose)

    # boca en W
    mouth = layer()
    md = ImageDraw.Draw(mouth)
    md.line(sc(cx, ny + 27, cx, ny + 48), fill=LEATHER_DK + (255,), width=wd(7))
    dd2 = rx * 0.40
    md.arc(sc(cx - dd2, ny + 22, cx + 2, ny + 22 + dd2 * 1.6), 0, 180,
           fill=LEATHER_DK + (255,), width=wd(7))
    md.arc(sc(cx - 2, ny + 22, cx + dd2, ny + 22 + dd2 * 1.6), 0, 180,
           fill=LEATHER_DK + (255,), width=wd(7))
    img = Image.alpha_composite(img, mouth)

    # bigotes: invisibles a 16 px, felinos a 256
    if detail:
        wh = layer()
        wd_ = ImageDraw.Draw(wh)
        for sx in (-1, 1):
            for dy, y2 in ((0, -10), (14, 16)):
                wd_.line(sc(cx + sx * rx * 0.37, cy + ry * 0.52 + dy,
                            cx + sx * rx * 0.72, cy + ry * 0.53 + y2),
                         fill=(94, 62, 36, 170), width=wd(4))
        img = Image.alpha_composite(img, wh)
    return img


def draw_goggles(img: Image.Image, P, blink: bool, lamp: float, detail: bool) -> Image.Image:
    cx = P["head"][0]
    cy = P["eye_y"]
    rim = P["rim_out"]
    lens = P["rim_in"]
    face_rx = P["face_rx"]

    # correas de cuero: cortas, caen hacia atrás y mueren en la melena
    straps = layer()
    sd = ImageDraw.Draw(straps)
    for sx in ((-1, 1) if P.get("straps", True) else ()):
        x0 = cx + sx * (rim - 14)
        x1 = cx + sx * (face_rx - 6)
        h = 25 if detail else 29
        drop = 26.0
        pts = [(x0, cy - h), (x1, cy - h + drop), (x1, cy + h + drop), (x0, cy + h)]
        sd.polygon(flat(pts), fill=LEATHER + (255,))
        sd.polygon(flat(pts), outline=INK + (220,), width=wd(5))
        inner = [(x0, cy - h + 9), (x1, cy - h + drop + 9),
                 (x1, cy + h + drop - 8), (x0, cy + h - 8)]
        sd.polygon(flat(inner), fill=(94, 59, 39, 255))
        if detail:
            for i in range(5):
                t = (i + 0.55) / 5.0
                xa = x0 + (x1 - x0) * t
                sd.line(sc(xa, cy - h + drop * t + 8, xa, cy + h + drop * t - 8),
                        fill=BRASS_DK + (130,), width=wd(3))
        bx = cx + sx * (face_rx - 28)
        by = cy + drop * 0.70
        sd.rounded_rectangle(sc(bx - 16, by - 28, bx + 16, by + 28), radius=wd(7),
                             fill=BRASS + (255,), outline=BRASS_SH + (255,), width=wd(4))
        sd.rounded_rectangle(sc(bx - 9, by - 16, bx + 9, by + 16), radius=wd(4),
                             fill=LEATHER_DK + (255,))
    img = Image.alpha_composite(img, straps)

    for sx in (-1, 1):
        ex = cx + sx * P["eye_dx"]
        img.paste(radial(ex - 26, cy - 38, rim * 1.6, BRASS_HI + (255,), BRASS_DK + (255,)),
                  (0, 0), ring_mask(ex, cy, lens, rim))

        ring = layer()
        rd = ImageDraw.Draw(ring)
        rd.ellipse(sc(ex - rim, cy - rim, ex + rim, cy + rim), outline=INK + (255,), width=wd(5))
        rd.ellipse(sc(ex - lens, cy - lens, ex + lens, cy + lens), outline=BRASS_SH + (215,),
                   width=wd(4))
        rd.arc(sc(ex - rim + 6, cy - rim + 6, ex + rim - 6, cy + rim - 6), 145, 300,
               fill=BRASS_HI + (230,), width=wd(9))
        rd.arc(sc(ex - rim + 2, cy - rim + 2, ex + rim - 2, cy + rim - 2), 20, 120,
               fill=BRASS_SH + (170,), width=wd(5))
        img = Image.alpha_composite(img, ring)

        if detail:
            rv = layer()
            rdv = ImageDraw.Draw(rv)
            mid = (lens + rim) / 2
            for i in range(6):
                a = math.radians(-90 + i * 60 + 30)
                x, y = ex + math.cos(a) * mid, cy + math.sin(a) * mid
                rdv.ellipse(sc(x - 6.5, y - 6.5, x + 6.5, y + 6.5), fill=BRASS_SH + (255,))
                rdv.ellipse(sc(x - 3.8, y - 3.8, x + 3.8, y + 3.8), fill=(255, 246, 224, 240))
            img = Image.alpha_composite(img, rv)

        # vidrio ahumado
        img.paste(radial(ex - lens * 0.30, cy - lens * 0.36, lens * 1.35,
                         (112, 72, 42, 255), (13, 9, 8, 255)), (0, 0),
                  disc_mask(ex, cy, lens - 3))
        lens_mask = disc_mask(ex, cy, lens - 4)

        if blink:
            bl = layer()
            bd = ImageDraw.Draw(bl)
            bd.ellipse(sc(ex - lens + 2, cy - lens + 2, ex + lens - 2, cy + lens - 2),
                       fill=(15, 10, 8, 255))
            bd.line(sc(ex - lens + 10, cy + 4, ex + lens - 10, cy - 6),
                    fill=COPPER + (215,), width=wd(7))
            img = Image.alpha_composite(img, bl)
        else:
            g = lamp
            ir = layer()
            idr = ImageDraw.Draw(ir)
            idr.ellipse(sc(ex - lens * 0.58, cy - lens * 0.58, ex + lens * 0.58, cy + lens * 0.58),
                        outline=COPPER + (int(175 * g),), width=wd(6))
            idr.ellipse(sc(ex - lens * 0.38, cy - lens * 0.38, ex + lens * 0.38, cy + lens * 0.38),
                        fill=(11, 8, 6, 255))
            idr.ellipse(sc(ex - lens * 0.26, cy - lens * 0.27, ex + lens * 0.15, cy + lens * 0.13),
                        fill=AMBER + (int(235 * g),))
            idr.ellipse(sc(ex - lens * 0.13, cy - lens * 0.17, ex + lens * 0.05, cy + 0),
                        fill=COPPER_HI + (int(250 * g),))
            ir.putalpha(ImageChops.multiply(ir.getchannel("A"), lens_mask))
            img = Image.alpha_composite(img, ir)
            img = add_glow(img, ir, 9, alpha=int(80 * g))

            sp = layer()
            sd2 = ImageDraw.Draw(sp)
            sd2.ellipse(sc(ex - lens * 0.72, cy - lens * 0.78, ex - lens * 0.10, cy - lens * 0.20),
                        fill=(255, 250, 240, 185))
            sd2.ellipse(sc(ex + lens * 0.28, cy + lens * 0.34, ex + lens * 0.56, cy + lens * 0.62),
                        fill=(255, 240, 220, 95))
            sp.putalpha(ImageChops.multiply(sp.getchannel("A"), lens_mask))
            img = Image.alpha_composite(img, sp.filter(ImageFilter.GaussianBlur(4 * SS)))

    # puente de latón entre antiparras
    br = layer()
    bd = ImageDraw.Draw(br)
    gap = max(P["eye_dx"] - rim, 12)
    bd.rounded_rectangle(sc(cx - gap - 10, cy - 15, cx + gap + 10, cy + 15),
                         radius=wd(9), fill=BRASS + (255,), outline=BRASS_SH + (255,),
                         width=wd(4))
    bd.ellipse(sc(cx - 6, cy - 6, cx + 6, cy + 6), fill=BRASS_HI + (255,))
    img = Image.alpha_composite(img, br)
    return img


# --------------------------------------------------------------------------
LAYOUTS = {"full": FULL, "small": SMALL, "nano": NANO}


def draw_icon(gear: float = 0.0, blink: bool = False, lamp: float = 1.0,
              layout: str = "full") -> Image.Image:
    P = LAYOUTS[layout]
    detail = layout == "full"
    img = draw_bg(P, detail)
    img = draw_gear(img, gear, P, detail)

    # rayo de cobre sobre la esfera, arriba del león
    img = draw_bolt(img, P, detail)

    # sombra del león sobre la esfera: separa la cabeza del fondo
    hx, hy = P["head"]
    sh = layer()
    ImageDraw.Draw(sh).ellipse(sc(hx - 250, hy - 160, hx + 250, hy + 250),
                               fill=(0, 0, 0, 165))
    img = Image.alpha_composite(img, sh.filter(ImageFilter.GaussianBlur(20 * SS)))

    img = draw_mane(img, P, detail)
    img = draw_face(img, P, detail)
    img = draw_goggles(img, P, blink, lamp, detail)

    if detail:
        img = add_glow(img, radial(430, 300, 430, (255, 224, 176, 52), (0, 0, 0, 0)), 55)

    m = mask_l()
    ImageDraw.Draw(m).rounded_rectangle(sc(PAD, PAD, DESIGN - PAD, DESIGN - PAD),
                                       radius=wd(ROUND), fill=255)
    out = Image.new("RGBA", (PX, PX), (0, 0, 0, 0))
    out.paste(img, (0, 0), ImageChops.multiply(m, img.getchannel("A")))
    edge = layer()
    ImageDraw.Draw(edge).rounded_rectangle(
        sc(PAD + 2, PAD + 2, DESIGN - PAD - 2, DESIGN - PAD - 2),
        radius=wd(ROUND - 2), outline=(0, 0, 0, 150), width=wd(5))
    return Image.alpha_composite(out, edge)


# --------------------------------------------------------------------------
# escritura de .ico (BMP para <=64 px, PNG para 128/256)
# --------------------------------------------------------------------------
def _bmp_entry(img: Image.Image) -> bytes:
    img = img.convert("RGBA")
    w, h = img.size
    arr = np.asarray(img)
    bgra = arr[..., [2, 1, 0, 3]].copy()

    row_bytes = ((w + 31) // 32) * 4
    mask = np.zeros((h, row_bytes), dtype=np.uint8)
    for y in range(h):
        bits = arr[y, :, 3] < 128
        for x in range(w):
            if bits[x]:
                mask[y, x // 8] |= 0x80 >> (x % 8)

    xor = bgra[::-1].tobytes()
    andm = mask[::-1].tobytes()
    header = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, len(xor) + len(andm), 0, 0, 0, 0)
    return header + xor + andm


def _png_entry(img: Image.Image) -> bytes:
    buf = io.BytesIO()
    img.convert("RGBA").save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def write_ico(path: Path, entries) -> None:
    """entries: [(tamaño, imagen)] -> .ico con BMP chico y PNG grande."""
    blobs = []
    for size, im in entries:
        data = _png_entry(im) if size >= 128 else _bmp_entry(im)
        blobs.append((size, data))

    out = struct.pack("<HHH", 0, 1, len(blobs))
    offset = 6 + 16 * len(blobs)
    directory = b""
    body = b""
    for size, data in blobs:
        b = 0 if size >= 256 else size
        directory += struct.pack("<BBBBHHII", b, b, 0, 0, 1, 32, len(data), offset)
        body += data
        offset += len(data)
    path.write_bytes(out + directory + body)


def resized(img: Image.Image, size: int) -> Image.Image:
    return img.resize((size, size), Image.Resampling.LANCZOS)


# --------------------------------------------------------------------------
# preview
# --------------------------------------------------------------------------
def _font(size=14):
    try:
        return ImageFont.load_default(size=size)
    except TypeError:
        return ImageFont.load_default()


def contact_sheet(master, simple, nano, frames, path: Path) -> None:
    W, H = 1180, 660
    sheet = Image.new("RGBA", (W, H), (17, 15, 14, 255))
    d = ImageDraw.Draw(sheet)
    f = _font(14)
    fs = _font(12)

    def label(txt, x, y, col=(158, 138, 110), font=f):
        d.text((x, y), txt, fill=col, font=font)

    label("DOMADOR · identidad steampunk · león domador con antiparras", 28, 12, (214, 178, 124), fs)

    # master
    sheet.alpha_composite(resized(master, 360), (28, 52))
    label("master 1024 px → assets/domador.png", 28, 418, (196, 160, 112), fs)

    # notas
    x0 = 410
    for i, line in enumerate((
        "medallón: engranaje de 9 dientes + esfera de manómetro",
        "león con antiparras de latón, correas de cuero y remaches",
        "rayo de cobre sobre la esfera, fondo carbón",
        "32..256 px: maqueta completa con graduaciones",
        "20/24 px: maqueta chica (menos piezas, cara más grande)",
        "16 px: maqueta mínima (aro liso, antiparras gruesas)",
    )):
        label("· " + line, x0, 54 + i * 21, (150, 132, 108), fs)

    # fila a escala real (maqueta completa)
    y = 200
    label("escala real 1:1", x0, y - 20, (196, 160, 112), fs)
    x = x0
    for size in (256, 128, 64, 48, 32):
        sheet.alpha_composite(resized(master, size), (x, y))
        label(f"{size}", x, y + size + 6, (150, 132, 108), fs)
        x += size + 18

    # 16/20/24/32: píxeles ampliados + copia 1:1 debajo
    y2 = 486
    label("píxeles reales ampliados (NEAREST) y copia 1:1", 28, y2 - 22,
          (196, 160, 112), fs)
    x = 28
    for lay, size in (("nano", 16), ("small", 20), ("small", 24), ("small", 32)):
        src = nano if lay == "nano" else simple
        tile = resized(src, size).resize((110, 110), Image.Resampling.NEAREST)
        sheet.alpha_composite(tile, (x, y2))
        strip = Image.new("RGBA", (110, 34), (26, 23, 20, 255))
        one = resized(src, size)
        strip.alpha_composite(one, (one.width + (110 - one.width) // 2, (34 - size) // 2))
        sheet.alpha_composite(strip, (x, y2 + 116))
        label(f"{size} px", x, y2 + 154, (150, 132, 108), fs)
        x += 110 + 20

    # frames de bandeja
    x = 530
    label("frames de bandeja anim00..07 (48 px)", x, y2 - 22, (196, 160, 112), fs)
    for i, fr in enumerate(frames):
        sheet.alpha_composite(resized(fr, 48), (x, y2 + 2))
        label(str(i), x + 20, y2 + 56, (140, 122, 98), fs)
        x += 60

    label("giro de engranaje 15°/frame · pulso de la lámpara · parpadeo en el 6",
          530, y2 + 100, (140, 122, 98), fs)

    sheet.convert("RGB").save(path, optimize=True)


# --------------------------------------------------------------------------
def main() -> None:
    ASSETS.mkdir(exist_ok=True)
    TRAY.mkdir(exist_ok=True)
    DOCS.mkdir(exist_ok=True)

    master = draw_icon(layout="full")
    small = draw_icon(layout="small")
    nano = draw_icon(layout="nano")

    master.resize((DESIGN, DESIGN), Image.Resampling.LANCZOS).save(ASSETS / "domador.png")
    resized(master, 512).save(DOCS / "icon-master-512.png")

    # .ico: 16 px maqueta mínima, 20/24 chica, 32+ completa
    entries = [(16, resized(nano, 16)), (20, resized(small, 20)), (24, resized(small, 24))]
    entries += [(s, resized(master, s)) for s in (32, 48, 64, 128, 256)]
    write_ico(ASSETS / "domador.ico", entries)

    # UI: favicon 32, logo 64, icono de ventana 256
    resized(small, 32).save(ASSETS / "icon-32.png", optimize=True)
    resized(master, 64).save(ASSETS / "icon-64.png", optimize=True)
    resized(master, 256).save(ASSETS / "icon-256.png", optimize=True)
    write_ico(ASSETS / "favicon.ico", [(16, resized(nano, 16)),
                                       (32, resized(small, 32)),
                                       (48, resized(master, 48))])

    # 8 frames: el engranaje gira 15°/frame (9 dientes: 120° = 3 dientes,
    # el loop cierra exacto), la lámpara del vidrio pulsa y el frame 6 parpadea.
    frames = []
    for i in range(8):
        ph = i / 8.0 * math.tau
        lamp = 0.55 + 0.45 * (0.5 + 0.5 * math.cos(ph))
        f_small = draw_icon(gear=i * 15.0, blink=(i == 6), lamp=lamp, layout="small")
        f_nano = draw_icon(gear=i * 15.0, blink=(i == 6), lamp=lamp, layout="nano")
        resized(f_small, 256).save(TRAY / f"anim{i:02}.png", optimize=True)
        write_ico(TRAY / f"anim{i:02}.ico",
                  [(16, resized(f_nano, 16))] +
                  [(s, resized(f_small, s)) for s in (20, 24, 32)])
        frames.append(f_small)

    contact_sheet(master, small, nano, frames, DOCS / "icon-preview.png")
    print("listo:", ASSETS / "domador.ico")


if __name__ == "__main__":
    main()
