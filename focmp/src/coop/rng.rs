//! Full native WELL RNG state, scoped on the game's main thread. A seed is not
//! a checkpoint. Verify the supported runtime before accessing its state block.
use crate::engine::G;
use bitcode::{Decode, Encode};
use std::sync::OnceLock;
static BASE: OnceLock<Option<usize>> = OnceLock::new();
#[derive(Clone, Copy, Debug, PartialEq, Encode, Decode)]
pub struct State {
    words: [u32; 16],
    index: u32,
    seed: u32,
}
impl State {
    pub fn sane(&self) -> bool {
        self.index < 16
    }
    pub fn salt(&self) -> u32 {
        self.words
            .iter()
            .fold(mix(self.seed as u64 ^ self.index as u64), |h, w| {
                mix(h ^ *w as u64)
            }) as u32
    }
}
pub fn install(g: G) -> bool {
    BASE.get_or_init(|| {
        let base = crate::win::exe_base();
        let signature = [
            0xc7, 0x41, 0x0c, 0, 0, 0, 0, 0x0f, 0x57, 0xc0, 0x8b, 0x05, 0x40, 0xc6, 0xea, 0, 0xf2,
            0x48, 0x0f, 0x2a, 0xc0, 0xf2, 0x0f, 0x11, 0x01, 0xc3,
        ];
        let core = [
            0x48, 0x89, 0x5c, 0x24, 0x10, 0x44, 0x8b, 0x15, 0x1c, 0x4d, 0xd6, 0, 0x48, 0x8d, 0x1d,
            0x1d, 0x4d, 0xd6, 0,
        ];
        if g.rt.builtins.get("random_get_seed").copied() != Some(base + 0xe36e60)
            || unsafe {
                std::slice::from_raw_parts((base + 0xe36e60) as *const u8, signature.len())
            } != signature
            || unsafe { std::slice::from_raw_parts((base + 0xf7e340) as *const u8, core.len()) }
                != core
        {
            crate::log("RNG: unsupported runtime state layout");
            return None;
        }
        crate::log("RNG: verified full 16-word state, cursor and seed");
        Some(base)
    })
    .is_some()
}
pub fn capture() -> Option<State> {
    let base = (*BASE.get()?)?;
    Some(unsafe {
        State {
            words: std::ptr::read((base + 0x1ce3070) as *const [u32; 16]),
            index: std::ptr::read((base + 0x1ce3068) as *const u32),
            seed: std::ptr::read((base + 0x1ce34b0) as *const u32),
        }
    })
}
fn restore(s: State) {
    if !s.sane() {
        return;
    }
    if let Some(Some(base)) = BASE.get() {
        unsafe {
            std::ptr::write((base + 0x1ce3070) as *mut [u32; 16], s.words);
            std::ptr::write((base + 0x1ce3068) as *mut u32, s.index);
            std::ptr::write((base + 0x1ce34b0) as *mut u32, s.seed);
        }
    }
}
pub struct Scope(Option<State>);
impl Scope {
    pub fn replay(state: State) -> Self {
        let old = capture();
        if old.is_some() {
            restore(state);
        }
        Self(old)
    }
    pub fn seeded(g: G, seed: u32) -> Self {
        let old = capture();
        if old.is_some() {
            g.call("random_set_seed", &[crate::engine::real(seed as f64)]);
        }
        Self(old)
    }
    pub fn preserve() -> Self {
        Self(capture())
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        if let Some(s) = self.0 {
            restore(s);
        }
    }
}
pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Same-frame native check: both integer and real draws, plus exact restoration.
pub fn check(g: G) -> String {
    if !install(g) {
        return "error: unsupported RNG layout".into();
    }
    let before = capture().unwrap();
    let (same, advanced, values) = {
        let _restore = Scope::preserve();
        g.call("random_set_seed", &[crate::engine::real(814971.0)]);
        for _ in 0..19 {
            g.call("random", &[crate::engine::real(100.0)]);
        }
        let checkpoint = capture().unwrap();
        let draw = || {
            [
                g.call("random", &[crate::engine::real(100.0)]).v,
                g.call("irandom", &[crate::engine::real(1000.0)]).v,
                g.call(
                    "random_range",
                    &[crate::engine::real(-9.0), crate::engine::real(7.0)],
                )
                .v,
            ]
        };
        let first = draw();
        let advanced = capture().unwrap() != checkpoint;
        let second = {
            let _replay = Scope::replay(checkpoint);
            draw()
        };
        (first == second, advanced, first.map(f64::from_bits))
    };
    format!(
        "{{\"checkpoint_matches\":{same},\"state_advances\":{advanced},\"outer_restored\":{},\"draws\":{:?}}}",
        capture() == Some(before),
        values
    )
}
