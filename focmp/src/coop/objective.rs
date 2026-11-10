//! One defend meter and reward outcome, owned by the host.
use crate::{
    engine::{G, V, boolean, key, real, truthy},
    net::PeerId,
};
use bitcode::{Decode, Encode};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicUsize, Ordering},
};
static STEP: AtomicUsize = AtomicUsize::new(0);
static PLACE: AtomicUsize = AtomicUsize::new(0);
static CONTEXT: AtomicUsize = AtomicUsize::new(0);
type Event = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);

#[derive(Clone, Debug, Encode, Decode)]
pub struct Snapshot {
    run: u64,
    room: String,
    level: u32,
    sub: u32,
    meter: f64,
    blend: f64,
    success: bool,
}
impl Snapshot {
    pub fn sane(&self) -> bool {
        self.run != 0
            && self.room.starts_with("Room_fight")
            && self.room.len() <= 64
            && self.meter.is_finite()
            && (0.0..=1.0).contains(&self.meter)
            && self.blend.is_finite()
            && (0.0..=16777215.0).contains(&self.blend)
    }
}
fn identity(g: G, area: V) -> (u32, u32) {
    if g.get(area, "focmp_level").as_f64().is_none() {
        let (l, s) = super::rooms::level_sub(g);
        g.set(area, "focmp_level", real(l as f64));
        g.set(area, "focmp_sub", real(s as f64));
    }
    (
        g.num(area, "focmp_level") as u32,
        g.num(area, "focmp_sub") as u32,
    )
}
pub fn capture(g: G) -> Option<Snapshot> {
    let area = g.first("objDefendArea")?;
    let p = g.first("objPlayer")?;
    let (level, sub) = identity(g, area);
    Some(Snapshot {
        run: super::rooms::RUN_SEED.load(Ordering::Relaxed),
        room: g.room_name(),
        level,
        sub,
        meter: g.num(area, "meter"),
        blend: g.num(area, "image_blend"),
        success: truthy(&g.get(p, "mission_complete")),
    })
}
pub fn apply(g: G, state: Snapshot) {
    if !state.sane()
        || state.run != super::rooms::RUN_SEED.load(Ordering::Relaxed)
        || state.room != g.room_name()
    {
        return;
    }
    let Some(area) = g.first("objDefendArea") else {
        return;
    };
    if identity(g, area) != (state.level, state.sub) {
        return;
    }
    g.set(area, "meter", real(state.meter));
    g.set(area, "image_blend", real(state.blend));
    if let Some(p) = g.first("objPlayer") {
        g.set(p, "mission_complete", boolean(state.success));
    }
}
pub fn install(g: G) {
    for (name, hook, slot) in [
        ("gml_Object_objDefendArea_Step_0", step as *const (), &STEP),
        ("instance_place", place as *const (), &PLACE),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(f) => slot.store(f as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("objective: {e}")),
            }
        }
    }
}
unsafe extern "C-unwind" fn step(s: *mut c_void, o: *mut c_void) {
    let f: Event = unsafe { std::mem::transmute(STEP.load(Ordering::Relaxed)) };
    let Some(g) = G::new().filter(|_| super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0) else {
        unsafe { f(s, o) };
        return;
    };
    let Some(area) = (unsafe { g.rt.builtin_var_of("id", s) }) else {
        unsafe { f(s, o) };
        return;
    };
    identity(g, area);
    if super::MY_ID.load(Ordering::Relaxed) == 0 {
        let old = CONTEXT.swap(s as usize, Ordering::Relaxed);
        unsafe { f(s, o) };
        CONTEXT.store(old, Ordering::Relaxed);
    } else {
        // Keep native presentation updates, but never advance a local meter or
        // decide the mission result. The host snapshot owns those fields.
        let saved = capture(g);
        let speed = g.get(area, "meter_spd");
        g.set(area, "meter_spd", real(0.0));
        unsafe { f(s, o) };
        if g.exists(area) {
            g.set(area, "meter_spd", speed);
            if let Some(state) = saved {
                apply(g, state);
            }
        }
    }
}
unsafe extern "C-unwind" fn place(r: *mut V, s: *mut c_void, o: *mut c_void, n: i32, a: *mut V) {
    let f: crate::gm::BuiltinFn = unsafe { std::mem::transmute(PLACE.load(Ordering::Relaxed)) };
    if CONTEXT.load(Ordering::Relaxed) != s as usize || s.is_null() || n != 3 || a.is_null() {
        unsafe { f(r, s, o, n, a) };
        return;
    }
    let Some(g) = G::new() else {
        unsafe { f(r, s, o, n, a) };
        return;
    };
    if key(&unsafe { *a.add(2) }) != key(&g.asset("objPlayer")) {
        unsafe { f(r, s, o, n, a) };
        return;
    }
    let Ok(c) = super::COOP.try_lock() else {
        unsafe { *r = real(-4.0) };
        return;
    };
    let mut candidates = Vec::new();
    if let Some(p) = g.first("objPlayer").filter(|p| !truthy(&g.get(*p, "dead"))) {
        candidates.push(p);
    }
    for peer in 1..crate::net::MAX_PLAYERS as PeerId {
        if c.remotes
            .get(&peer)
            .is_some_and(|p| p.room == g.room_name() && p.seen.elapsed().as_secs_f32() < 1.0)
        {
            if let Some(ghost) = c.ghosts.get(peer).filter(|p| !p.dead && g.exists(p.body)) {
                candidates.push(ghost.body);
            }
        }
    }
    for p in candidates {
        let mut args = [unsafe { *a }, unsafe { *a.add(1) }, p];
        unsafe { f(r, s, o, 3, args.as_mut_ptr()) };
        if g.exists(unsafe { *r }) {
            return;
        }
    }
    unsafe { *r = real(-4.0) };
}
