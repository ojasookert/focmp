//! The installer's banner and icon. The game's hero art and logo, bundled in `assets/` (they belong to Brainburn
//! Studio and are used with permission), with the mod's MULTIPLAYER pixel lettering under the logo; the blood drips
//! on the lettering are animated by `gui.rs`. Everything is local: no Steam cache or CDN is needed.
//! All of `assets/` is made by `scripts/make_art.py` (the game art with `--game-art`).

use eframe::egui;
use image::imageops::{self, FilterType};
use image::{DynamicImage, GrayImage, Luma, Rgba, RgbaImage};
use serde::Deserialize;

const WORD: &[u8] = include_bytes!("../assets/multiplayer.png");
const DRIPS: &str = include_str!("../assets/drips.json");
const HERO: &[u8] = include_bytes!("../assets/hero.jpg");
const LOGO: &[u8] = include_bytes!("../assets/logo.png");
const ICON: &[u8] = include_bytes!("../assets/icon.png");
pub const W: u32 = 1440;
pub const H: u32 = 400;
pub const PINK: [u8; 3] = [255, 45, 205];
pub const BLOOD: [u8; 3] = [200, 0, 70];
pub const DARK: [u8; 3] = [120, 0, 40];
const INK: [u8; 3] = [10, 4, 12];

#[derive(Deserialize, Clone)]
pub struct Drip {
    pub x: u32,
    pub y0: u32,
    pub lmax: u32,
    pub period: f32,
    pub phase: f32,
}

#[derive(Deserialize)]
struct Spec {
    text_px: u32,
    drips: Vec<Drip>,
}

/// The static banner (W x H) and where the lettering's pixel grid lies on it, for the animated drips.
pub struct Banner {
    pub image: egui::ColorImage,
    pub word_x: f32,
    pub word_y: f32,
    /// Banner pixels per lettering pixel.
    pub px: f32,
    pub word_h: u32,
    pub drips: Vec<Drip>,
}

#[derive(Default)]
pub struct Art {
    pub banner: Option<Banner>,
    pub icon: Option<egui::IconData>,
}

pub fn load() -> Art {
    let hero = image::load_from_memory(HERO).expect("bundled hero art");
    let logo = image::load_from_memory(LOGO).ok();
    Art {
        banner: Some(banner(&hero, logo.as_ref())),
        icon: Some(icon()),
    }
}

/// The helmeted figure from the hero art, square, 256 px.
fn icon() -> egui::IconData {
    let face = image::load_from_memory(ICON).expect("bundled icon").to_rgba8();
    egui::IconData { width: face.width(), height: face.height(), rgba: face.into_raw() }
}

/// Hero art filling W x H, the logo on the left (at most 2/5 of the width) and the lettering under it, with glow.
fn banner(hero: &DynamicImage, logo: Option<&DynamicImage>) -> Banner {
    let spec: Spec = serde_json::from_str(DRIPS).expect("bundled drips.json");
    let word = image::load_from_memory(WORD).expect("bundled lettering").to_rgba8();
    let (ww, wh) = word.dimensions();

    // hero: scale to cover, crop a little above centre
    let scale = (W as f32 / hero.width() as f32).max(H as f32 / hero.height() as f32);
    let (rw, rh) = ((hero.width() as f32 * scale).ceil() as u32, (hero.height() as f32 * scale).ceil() as u32);
    let mut out = hero.resize_exact(rw, rh, FilterType::Triangle).to_rgba8();
    out = imageops::crop(&mut out, (rw - W) / 2, ((rh - H) as f32 * 0.35) as u32, W, H).to_image();

    // logo trimmed to its visible pixels; sizes as in scripts/make_art.py
    let logo = logo.map(|l| trim(&l.to_rgba8()));
    let (lw, lh) = logo.as_ref().map_or((W as f32 * 0.32, 0.0), |l| (l.width() as f32, l.height() as f32));
    let s = (W as f32 * 2.0 / 5.0 / lw).min(H as f32 * 0.92 / (lh + wh as f32 * (lw * 0.92 / ww as f32) * 0.40));
    let (logo_w, logo_h) = ((lw * s) as u32, (lh * s) as u32);
    let px = logo_w as f32 * 0.92 / ww as f32;
    let (word_w, word_h) = ((ww as f32 * px) as u32, (wh as f32 * px) as u32);
    let x0 = W / 24;
    let y0 = ((H as f32 - (logo_h as f32 + spec.text_px as f32 * px * 0.9)) / 2.0).max(4.0) as u32;
    let word_x = x0 + logo_w.saturating_sub(word_w) / 2;
    let word_y = (y0 + logo_h).saturating_sub((word_h as f32 * (spec.text_px + 2) as f32 / wh as f32 * 0.25) as u32);

    // pink glow behind the letters (not the outline): two blurs, combined
    let mut mask = GrayImage::new(ww, wh);
    for (x, y, p) in word.enumerate_pixels() {
        let letter = p[3] > 0 && !(p[0] == INK[0] && p[1] == INK[1] && p[2] == INK[2]);
        mask.put_pixel(x, y, Luma([if letter { 255 } else { 0 }]));
    }
    let mask = imageops::resize(&mask, word_w, word_h, FilterType::Nearest);
    let (wide, tight) = (imageops::blur(&mask, px * 2.6), imageops::blur(&mask, px * 0.9));
    let glow = RgbaImage::from_fn(word_w, word_h, |x, y| {
        let a = wide.get_pixel(x, y)[0] as f32 / 255.0;
        let b = tight.get_pixel(x, y)[0] as f32 / 255.0;
        let alpha = 1.0 - (1.0 - a) * (1.0 - b);
        Rgba([PINK[0], PINK[1], PINK[2], (alpha * 255.0) as u8])
    });
    imageops::overlay(&mut out, &glow, word_x as i64, word_y as i64);
    if let Some(l) = logo {
        let l = imageops::resize(&l, logo_w.max(1), logo_h.max(1), FilterType::Lanczos3);
        imageops::overlay(&mut out, &l, x0 as i64, y0 as i64);
    }
    let letters = imageops::resize(&word, word_w, word_h, FilterType::Nearest);
    imageops::overlay(&mut out, &letters, word_x as i64, word_y as i64);

    Banner {
        image: egui::ColorImage::from_rgba_unmultiplied([W as usize, H as usize], out.as_raw()),
        word_x: word_x as f32,
        word_y: word_y as f32,
        px,
        word_h: wh,
        drips: spec.drips,
    }
}

fn trim(img: &RgbaImage) -> RgbaImage {
    let (mut x0, mut y0, mut x1, mut y1) = (img.width(), img.height(), 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 40 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return img.clone();
    }
    imageops::crop_imm(img, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
}

/// The drip and drop pixels at time `t` (seconds), in lettering pixel coordinates: grow, swell, let go, fall, regrow.
/// Same as `drip_pixels` in scripts/make_art.py.
pub fn drip_pixels(drips: &[Drip], word_h: u32, t: f32, mut put: impl FnMut(i32, i32, [u8; 3])) {
    for d in drips {
        let u = ((t / d.period) + d.phase).rem_euclid(1.0);
        let (x, y0, lmax) = (d.x as i32, d.y0 as i32, d.lmax as i32);
        let (ln, swell, drop_y) = if u < 0.62 {
            let k = u / 0.62;
            (1 + ((lmax - 1) as f32 * (1.0 - (1.0 - k).powi(2))).round() as i32, k > 0.75, None)
        } else {
            let k = (u - 0.62) / 0.38;
            ((lmax - 2).max(1), false, Some(y0 + lmax + (210.0 * (k * 0.38 * d.period).powi(2)).round() as i32))
        };
        for i in 0..ln {
            let tt = i as f32 / (lmax - 1).max(1) as f32;
            put(x, y0 + i, if tt < 0.3 { PINK } else if tt < 0.75 { BLOOD } else { DARK });
        }
        if swell {
            for dx in 0..2 {
                put(x + dx, y0 + ln, BLOOD);
                put(x + dx, y0 + ln + 1, DARK);
            }
        }
        if let Some(dy) = drop_y.filter(|&y| y < word_h as i32 - 1) {
            let c = if dy > y0 + lmax + 14 { DARK } else { BLOOD };
            for dx in 0..2 {
                put(x + dx, dy, c);
                put(x + dx, dy + 1, DARK);
            }
        }
    }
}
