//! User-selected player colors. Duplicates are deliberately allowed.
pub const PALETTE: [(&str, (u8, u8, u8)); 8] = [
    ("Neon pink", (255, 50, 176)),
    ("Neon cyan", (50, 235, 255)),
    ("Neon lime", (160, 255, 60)),
    ("Neon orange", (255, 148, 40)),
    ("Neon violet", (184, 102, 255)),
    ("Neon yellow", (255, 245, 80)),
    ("Neon blue", (70, 132, 255)),
    ("Neon coral", (255, 92, 96)),
];
pub fn name(index: u8) -> &'static str { PALETTE[index as usize % PALETTE.len()].0 }
pub fn value(index: u8) -> f64 {
    let (r, g, b) = PALETTE[index as usize % PALETTE.len()].1;
    (r as u32 | (g as u32) << 8 | (b as u32) << 16) as f64
}
