//! User-selected player colors. Duplicates are deliberately allowed.
// Shared by the picker, pointers, avatars and HUD. Lift the original neon
// palette 20% toward white for brighter pastels without changing color IDs.
pub const PALETTE: [(&str, (u8, u8, u8)); 8] = [
    ("Pink", (255, 91, 192)),
    ("Cyan", (91, 239, 255)),
    ("Lime", (179, 255, 99)),
    ("Orange", (255, 169, 83)),
    ("Violet", (198, 133, 255)),
    ("Yellow", (255, 247, 115)),
    ("Blue", (107, 157, 255)),
    ("Coral", (255, 125, 128)),
];
pub fn name(index: u8) -> &'static str { PALETTE[index as usize % PALETTE.len()].0 }
pub fn value(index: u8) -> f64 {
    let (r, g, b) = PALETTE[index as usize % PALETTE.len()].1;
    (r as u32 | (g as u32) << 8 | (b as u32) << 16) as f64
}
