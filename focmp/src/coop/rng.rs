//! Full native WELL RNG state, scoped on the game's main thread. A seed is not
//! a checkpoint. Verify the supported runtime before accessing its state block.
use crate::engine::G;
use bitcode::{Decode, Encode};
use std::sync::OnceLock;
#[derive(Clone, Copy)]
struct Layout { words: usize, index: usize, seed: usize }
static LAYOUT: OnceLock<Option<Layout>> = OnceLock::new();
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
fn resolve(g: G) -> Option<Layout> {
    let image = crate::pe::Image::parse(crate::win::exe_base()).ok()?;
    let seed_getter = *g.rt.builtins.get("random_get_seed")?;
    let getter = image.text.slice(seed_getter, 26)?;
    // mov result.kind, REAL; xorps; mov eax,[seed]; convert to real; return.
    if getter[..12] != [0xc7, 0x41, 0x0c, 0, 0, 0, 0, 0x0f, 0x57, 0xc0, 0x8b, 0x05]
        || getter[16..] != [0xf2, 0x48, 0x0f, 0x2a, 0xc0, 0xf2, 0x0f, 0x11, 0x01, 0xc3] {
        return None;
    }
    // WELL512a core: capture the state pointer and cursor from RIP-relative
    // operands. Only those displacements may differ between supported builds.
    let core = image.unique_text(&[
        0x48, 0x89, 0x5c, 0x24, 0x10, 0x44, 0x8b, 0x15, 0, 0, 0, 0,
        0x48, 0x8d, 0x1d, 0, 0, 0, 0, 0x42, 0x8b, 0x14, 0x93, 0x4e,
        0x8d, 0x04, 0x93, 0x41, 0x8d, 0x42, 0xfd, 0x83, 0xe0, 0x0f,
        0x44, 0x8d, 0x1c, 0x12,
    ], &[8..12, 15..19])?;
    let layout = Layout {
        seed: image.rip_target(seed_getter, 12)?,
        index: image.rip_target(core, 8)?,
        words: image.rip_target(core, 15)?,
    };
    // Both the read and write at the end of the core must use the same cursor.
    if image.text.slice(core + 0x73, 2)? != [0x89, 0x0d]
        || image.text.slice(core + 0x95, 2)? != [0x8b, 0x0d]
        || image.rip_target(core, 0x75)? != layout.index
        || image.rip_target(core, 0x97)? != layout.index
        || layout.words % 4 != 0 || layout.index % 4 != 0 || layout.seed % 4 != 0 {
        return None;
    }
    image.data.slice(layout.words, 64)?;
    image.data.slice(layout.index, 4)?;
    image.data.slice(layout.seed, 4)?;
    Some(layout)
}
pub fn install(g: G) -> bool {
    LAYOUT.get_or_init(|| {
        let layout = resolve(g);
        crate::log(if layout.is_some() {
            "RNG: verified full 16-word state, cursor and seed"
        } else {
            "RNG: unsupported runtime state layout"
        });
        layout
    }).is_some()
}
pub fn capture() -> Option<State> {
    let layout = (*LAYOUT.get()?)?;
    Some(unsafe {
        State {
            words: std::ptr::read(layout.words as *const [u32; 16]),
            index: std::ptr::read(layout.index as *const u32),
            seed: std::ptr::read(layout.seed as *const u32),
        }
    })
}
fn restore(s: State) {
    if !s.sane() { return; }
    if let Some(Some(layout)) = LAYOUT.get() {
        unsafe {
            std::ptr::write(layout.words as *mut [u32; 16], s.words);
            std::ptr::write(layout.index as *mut u32, s.index);
            std::ptr::write(layout.seed as *mut u32, s.seed);
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
