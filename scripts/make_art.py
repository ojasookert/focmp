#!/usr/bin/env python3
"""Generates the installer's MULTIPLAYER artwork assets, and optionally the README's animated header.

    python3 scripts/make_art.py [--font DejaVuSans-Bold.ttf] [--game-art] [--header]
    python3 scripts/make_art.py --game-art     (only the game art; needs no font)

Writes (committed, so building the installer needs neither Python nor a font):
  installer/assets/multiplayer.png  the word's pixel letters with a 1 px dark outline, at pixel resolution
  installer/assets/drips.json       the blood drips: column, start row, length, period and phase (4 s loop)
With --game-art also the game's art the installer bundles, downloaded from Steam's CDN (it belongs to Brainburn
Studio and is used with permission; see README, License):
  installer/assets/hero.jpg         the hero art cropped to the banner (1440x400)
  installer/assets/logo.png         the game's logo, trimmed to its visible pixels
  installer/assets/icon.png         the helmeted figure from the hero art, 256 px (window icon)
With --header also docs/header.webp: the installer banner animated (1440x400, 30 fps, 4 s loop).
Needs Pillow.
"""
import argparse, io, json, random, urllib.request
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont, ImageFilter, ImageOps

ROOT = Path(__file__).resolve().parent.parent
PINK, HOT, BLOOD, DARK, INK = (255, 45, 205), (255, 120, 235), (200, 0, 70), (120, 0, 40), (10, 4, 12)
TEXT, PX, EXTRA, LOOP = "MULTIPLAYER", 12, 46, 4.0
FONTS = ["/usr/share/fonts/TTF/DejaVuSans-Bold.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
         "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf"]
CDN = "https://shared.steamstatic.com/store_item_assets/steam/apps/3399100"
# The art's current folders on the CDN; its unversioned paths serve the game's old logo. Same as installer/src/art.rs.
HERO = "110eacf56d942048454af71c943c585f3b745116/library_hero_2x.jpg"
LOGO = "78b726cbb14bae6b97bce00faff99aa182c81ee2/logo_2x.png"


def letters(font):
    f = ImageFont.truetype(font, PX)
    w = int(ImageDraw.Draw(Image.new("L", (1, 1))).textlength(TEXT, font=f)) + 4
    h = PX + EXTRA
    m = Image.new("L", (w, h), 0)
    ImageDraw.Draw(m).text((2, 1), TEXT, font=f, fill=255)
    m = m.point(lambda v: 255 if v > 110 else 0)
    a = m.load()
    top = min(y for x in range(w) for y in range(h) if a[x, y])
    col = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    c = col.load()
    for x in range(w):
        for y in range(h):
            if a[x, y]:
                c[x, y] = (*(HOT if y <= top + 1 else PINK), 255)
    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    img.paste((*INK, 255), mask=m.filter(ImageFilter.MaxFilter(3)))
    img.alpha_composite(col)
    bottoms = {x: max(y for y in range(h) if a[x, y]) for x in range(w) if any(a[x, y] for y in range(h))}
    return img, m, bottoms


def drips(bottoms, seed=3):
    rnd = random.Random(seed)
    base = max(bottoms.values())
    cands = [x for x, b in bottoms.items() if b >= base - 1]
    out, used = [], []
    for x in rnd.sample(cands, len(cands)):
        if any(abs(x - u) < 3 for u in used) or len(out) >= 11:
            continue
        used.append(x)
        out.append(dict(x=x, y0=bottoms[x] + 1, lmax=rnd.choice([3, 4, 5, 6, 7, 8, 10]),
                        period=LOOP / rnd.choice([1, 1, 2]), phase=round(rnd.random(), 4)))
    return out


def drip_pixels(img, spec, t):
    """The same drip animation as installer/src/gui.rs (paint_drips): grow, swell, drop, fall, regrow."""
    im = img.copy()
    P = im.load()
    w, h = im.size

    def put(x, y, c):
        if 0 <= x < w and 0 <= y < h:
            P[x, y] = (*c, 255)
    for d in spec:
        u = ((t / d["period"]) + d["phase"]) % 1.0
        if u < 0.62:
            k = u / 0.62
            ln = 1 + int(round((d["lmax"] - 1) * (1 - (1 - k) ** 2)))
            swell, drop_y = k > 0.75, None
        else:
            k = (u - 0.62) / 0.38
            ln, swell = max(1, d["lmax"] - 2), False
            drop_y = d["y0"] + d["lmax"] + int(round(210 * (k * 0.38 * d["period"]) ** 2))
        for i in range(ln):
            tt = i / max(1, d["lmax"] - 1)
            put(d["x"], d["y0"] + i, PINK if tt < 0.3 else BLOOD if tt < 0.75 else DARK)
        if swell:
            ty = d["y0"] + ln
            for dx in (0, 1):
                put(d["x"] + dx, ty, BLOOD)
                put(d["x"] + dx, ty + 1, DARK)
        if drop_y is not None and drop_y < h - 1:
            c = DARK if drop_y > d["y0"] + d["lmax"] + 14 else BLOOD
            for dx in (0, 1):
                put(d["x"] + dx, drop_y, c)
                put(d["x"] + dx, drop_y + 1, DARK)
    return im


def fetch(name):
    req = urllib.request.Request(f"{CDN}/{name}", headers={"User-Agent": "focmp-make-art"})
    return Image.open(io.BytesIO(urllib.request.urlopen(req, timeout=30).read())).convert("RGBA")


def game_art(assets):
    """The game's art at the sizes the installer uses (installer/src/art.rs composes the banner from it)."""
    hero, logo = fetch(HERO), fetch(LOGO)
    ImageOps.fit(hero, (1440, 400), Image.Resampling.LANCZOS, centering=(0.5, 0.35)).convert("RGB").save(
        assets / "hero.jpg", quality=90, optimize=True)
    lgc = logo.crop(logo.getchannel("A").point(lambda v: 255 if v > 40 else 0).getbbox())
    lgc.thumbnail((640, 640), Image.LANCZOS)
    lgc.save(assets / "logo.png", optimize=True)
    s = hero.width / 3840
    face = hero.crop((int(2470 * s), int(150 * s), int(2950 * s), int(630 * s)))
    face.resize((256, 256), Image.LANCZOS).convert("RGB").save(assets / "icon.png", optimize=True)


def header(img, mask, spec, out):
    """The installer banner (same layout as installer/src/art.rs), animated."""
    W, H = 1440, 400
    hero, logo = fetch(HERO), fetch(LOGO)
    bg = ImageOps.fit(hero, (W, H), Image.Resampling.BILINEAR, centering=(0.5, 0.35))
    lgc = logo.crop(logo.getchannel("A").point(lambda v: 255 if v > 40 else 0).getbbox())
    w, h = img.size
    s = min(W * 2 / 5 / lgc.width, H * 0.92 / (lgc.height + h * (lgc.width * 0.92 / w) * 0.40))
    logo_s = lgc.resize((int(lgc.width * s), int(lgc.height * s)), Image.LANCZOS)
    pxs = logo_s.width * 0.92 / w
    size = (int(w * pxs), int(h * pxs))
    x0, y0 = W // 24, max(4, (H - int(logo_s.height + PX * pxs * 0.9)) // 2)
    wx, wy = x0 + (logo_s.width - size[0]) // 2, y0 + logo_s.height - int(size[1] * (PX + 2) / h * 0.25)
    glow = Image.new("RGBA", size, (0, 0, 0, 0))
    glow.paste((*PINK, 255), mask=mask.resize(size, Image.NEAREST))
    glow = Image.alpha_composite(glow.filter(ImageFilter.GaussianBlur(pxs * 2.6)), glow.filter(ImageFilter.GaussianBlur(pxs * 0.9)))
    base = bg.copy()
    base.alpha_composite(glow, (wx, wy))
    base.alpha_composite(logo_s, (x0, y0))
    frames = []
    for i in range(int(LOOP * 30)):
        fr = base.copy()
        fr.alpha_composite(drip_pixels(img, spec, i / 30).resize(size, Image.NEAREST), (wx, wy))
        frames.append(fr.convert("RGB"))
    frames[0].save(out, save_all=True, append_images=frames[1:], duration=33, loop=0, quality=85, method=6)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--font", default=next((f for f in FONTS if Path(f).is_file()), None), help="a bold TTF (DejaVu Sans Bold)")
    p.add_argument("--game-art", action="store_true", help="also write the game art the installer bundles (downloads it)")
    p.add_argument("--header", action="store_true", help="also render docs/header.webp (downloads the game's art)")
    a = p.parse_args()
    assets = ROOT / "installer/assets"
    if a.game_art and not a.header:
        # the game art doesn't depend on the lettering, so this needs no font
        game_art(assets)
        print("hero.jpg, logo.png, icon.png")
        return
    if not a.font:
        p.error("no DejaVu Sans Bold found; pass --font")
    img, mask, bottoms = letters(a.font)
    spec = drips(bottoms)
    img.save(assets / "multiplayer.png", optimize=True)
    (assets / "drips.json").write_text(json.dumps({"text_px": PX, "loop": LOOP, "drips": spec}, indent=1) + "\n")
    print(f"multiplayer.png {img.size}, {len(spec)} drips")
    if a.game_art:
        game_art(assets)
        print("hero.jpg, logo.png, icon.png")
    if a.header:
        header(img, mask, spec, ROOT / "docs/header.webp")
        print("docs/header.webp")


if __name__ == "__main__":
    main()
