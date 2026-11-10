//! Host-owned map color/flicker. Visibility and renderer quality stay local.
use crate::{
    engine::{G, V, boolean, is_struct, real},
    gm::BuiltinFn,
};
use bitcode::{Decode, Encode};
use std::{
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
static STEP: AtomicUsize = AtomicUsize::new(0);
static DISTANCE: AtomicUsize = AtomicUsize::new(0);
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static LATEST: Mutex<Option<Snapshot>> = Mutex::new(None);
static LIGHT_RNG: Mutex<Option<super::rng::State>> = Mutex::new(None);
static HISTORY: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());
static HOST: Mutex<Option<Snapshot>> = Mutex::new(None);
#[derive(Clone, Debug, Encode, Decode)]
pub struct Color {
    blend: u32,
    alpha: f32,
    image: f32,
}
#[derive(Clone, Debug, Encode, Decode)]
pub struct Snapshot {
    room: String,
    level: u32,
    sub: u32,
    pub frame: u64,
    lights: Vec<Color>,
    overlays: Vec<Color>,
}
impl Snapshot {
    pub fn sane(&self) -> bool {
        self.room.starts_with("Room_")
            && self.room.len() < 128
            && self.lights.len() <= 1024
            && self.overlays.len() <= 64
            && self.lights.iter().chain(&self.overlays).all(|v| {
                v.blend <= 0xffffff
                    && v.alpha.is_finite()
                    && v.image.is_finite()
                    && v.alpha.abs() < 100.0
                    && v.image.abs() < 1.0e7
            })
    }
}
pub fn reset() {
    *LIGHT_RNG.lock().unwrap() = None;
    HISTORY.lock().unwrap().clear();
    *LATEST.lock().unwrap() = None;
    *HOST.lock().unwrap() = None;
}
pub fn capture(g: G) -> Option<Snapshot> {
    HOST.lock()
        .unwrap()
        .as_ref()
        .filter(|v| v.room == g.room_name() && (v.level, v.sub) == super::rooms::level_sub(g))
        .cloned()
}
pub fn receive(g: G, s: Snapshot) {
    if s.room == g.room_name() && (s.level, s.sub) == super::rooms::level_sub(g) {
        *LATEST.lock().unwrap() = Some(s);
    }
}
pub fn install(g: G) {
    for (name, f, slot) in [
        ("gml_Object_objTileBuilder_Step_0", step as *const (), &STEP),
        ("point_distance", distance as *const (), &DISTANCE),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, f) {
                Ok(p) => slot.store(p as usize, Ordering::Relaxed),
                Err(e) => crate::log(&e),
            }
        }
    }
}
fn colors(g: G, array: V) -> Vec<Color> {
    (0..g.arr_len(array))
        .map(|i| {
            let light = g.arr_get(array, i);
            Color {
                blend: g.num(light, "blend") as u32,
                alpha: g.num(light, "alpha") as f32,
                image: g.num(light, "image") as f32,
            }
        })
        .collect()
}
fn apply(g: G, array: V, colors: &[Color]) {
    if g.arr_len(array) != colors.len() {
        return;
    }
    for (i, c) in colors.iter().enumerate() {
        let light = g.arr_get(array, i);
        if is_struct(&light) {
            for (k, v) in [
                ("blend", c.blend as f64),
                ("alpha", c.alpha as f64),
                ("image", c.image as f64),
            ] {
                g.set(light, k, real(v));
            }
        }
    }
}
unsafe extern "C-unwind" fn step(s: *mut c_void, o: *mut c_void) {
    let f: unsafe extern "C-unwind" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(STEP.load(Ordering::Relaxed)) };
    let Some(g) = G::new().filter(|_| super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0) else {
        unsafe { f(s, o) };
        return;
    };
    let Some(owner) = (unsafe { g.rt.builtin_var_of("id", s) }) else {
        unsafe { f(s, o) };
        return;
    };
    let host = !super::enemies::PUPPET_MODE.load(Ordering::Relaxed);
    // The native event skips off-camera lights. Evaluate all host light clocks,
    // then restore its native 1000px visibility cull for rendering only.
    let state = if host {
        *LIGHT_RNG.lock().unwrap()
    } else {
        None
    };
    let _random = state
        .map(super::rng::Scope::replay)
        .unwrap_or_else(super::rng::Scope::preserve);
    if host {
        ACTIVE.store(s as usize, Ordering::Relaxed);
    }
    unsafe { f(s, o) };
    ACTIVE.store(0, Ordering::Relaxed);
    let lights = g.get(owner, "array_tile_lights");
    let overlays = g.get(owner, "surf_overlays");
    if host {
        *LIGHT_RNG.lock().unwrap() = super::rng::capture();
        if let Some(p) = g.first("objPlayer") {
            for i in 0..g.arr_len(lights) {
                let l = g.arr_get(lights, i);
                let distance = (g.num(l, "x") - g.num(p, "x")).hypot(g.num(l, "y") - g.num(p, "y"));
                g.set(l, "visible", boolean(distance <= 1000.0));
            }
        }
        let (level, sub) = super::rooms::level_sub(g);
        let mut state = HOST.lock().unwrap();
        let frame = state.as_ref().map_or(1, |v| v.frame.wrapping_add(1));
        let snapshot = Snapshot {
            room: g.room_name(),
            level,
            sub,
            frame,
            lights: colors(g, lights),
            overlays: colors(g, overlays),
        };
        if snapshot.sane() {
            let mut history = HISTORY.lock().unwrap();
            history.push((frame, digest(&snapshot.lights, &snapshot.overlays)));
            if history.len() > 180 {
                history.remove(0);
            }
            *state = Some(snapshot);
        }
    } else if let Some(state) = LATEST.lock().unwrap().as_ref() {
        if state.room == g.room_name() && (state.level, state.sub) == super::rooms::level_sub(g) {
            apply(g, lights, &state.lights);
            apply(g, overlays, &state.overlays);
            g.set(owner, "focmp_light_frame", real(state.frame as f64));
        }
    }
}
unsafe extern "C-unwind" fn distance(r: *mut V, s: *mut c_void, o: *mut c_void, n: i32, a: *mut V) {
    if !s.is_null() && s as usize == ACTIVE.load(Ordering::Relaxed) {
        unsafe { *r = real(0.0) };
        return;
    }
    let f: BuiltinFn = unsafe { std::mem::transmute(DISTANCE.load(Ordering::Relaxed)) };
    unsafe { f(r, s, o, n, a) };
}

fn digest(a: &[Color], b: &[Color]) -> u64 {
    a.iter().chain(b).fold(0xcbf29ce484222325, |h, v| {
        [v.blend, v.alpha.to_bits(), v.image.to_bits()]
            .iter()
            .fold(h, |h, n| super::rng::mix(h ^ *n as u64))
    })
}
pub fn describe(g: G) -> String {
    let Some(b) = g.first("objTileBuilder") else {
        return "{}".into();
    };
    let lights = colors(g, g.get(b, "array_tile_lights"));
    let overlays = colors(g, g.get(b, "surf_overlays"));
    let frame = if super::enemies::PUPPET_MODE.load(Ordering::Relaxed) {
        g.num(b, "focmp_light_frame") as u64
    } else {
        HOST.lock().unwrap().as_ref().map_or(0, |s| s.frame)
    };
    let history = HISTORY
        .lock()
        .unwrap()
        .iter()
        .map(|(f, h)| format!("[{f},\"{h:016x}\"]"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"frame\":{frame},\"hash\":\"{:016x}\",\"lights\":{},\"overlays\":{},\"history\":[{history}]}}",
        digest(&lights, &overlays),
        lights.len(),
        overlays.len()
    )
}
