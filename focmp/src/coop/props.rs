//! Native world props: host-owned damage, shared hit effects and pose/state.
//! Personal shop/lab rooms are intentionally outside this replication scope.
use crate::{
    engine::{G, V, asset_id, boolean, is_struct, real, truthy},
    net::{Msg, Net, PeerId, Role},
};
use bitcode::{Decode, Encode};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
const FIELDS: &[&str] = &[
    "x",
    "y",
    "image_angle",
    "image_index",
    "image_xscale",
    "image_yscale",
    "visible",
    "hp",
    "hp_start",
    "dead",
    "triggered",
    "life",
    "angular_vel",
    "hits_to_break",
    "onfire",
    "poisoned",
    "frozen",
    "depth",
    "active",
];
#[derive(Clone, Debug, PartialEq, Encode, Decode)]
pub enum Scalar {
    Undefined,
    Number(f64),
    Bool(bool),
}
impl Scalar {
    fn capture(v: V) -> Self {
        if v.kind() == crate::gm::KIND_BOOL {
            Self::Bool(truthy(&v))
        } else {
            v.as_f64()
                .filter(|x| x.is_finite())
                .map_or(Self::Undefined, Self::Number)
        }
    }
    fn value(&self) -> V {
        match self {
            Self::Undefined => V::undefined(),
            Self::Number(x) => real(*x),
            Self::Bool(b) => boolean(*b),
        }
    }
    fn sane(&self) -> bool {
        match self {
            Self::Number(x) => x.is_finite() && x.abs() < 1e9,
            _ => true,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Encode, Decode)]
pub struct State {
    id: u32,
    obj: i16,
    ox: f64,
    oy: f64,
    sprite: String,
    values: Vec<Scalar>,
    fly: Option<(f64, f64)>,
}
#[derive(Clone, Debug, Encode, Decode)]
pub struct Snapshot {
    run: u64,
    room: String,
    level: u32,
    sub: u32,
    states: Vec<State>,
    gone: Vec<u32>,
}
impl Snapshot {
    pub fn sane(&self) -> bool {
        self.run != 0
            && self.room.len() < 64
            && self.states.len() <= 4096
            && self.gone.len() <= 4096
            && self.states.iter().all(|s| {
                s.id != 0
                    && s.obj >= 0
                    && s.ox.is_finite()
                    && s.oy.is_finite()
                    && s.ox.abs() < 1e7
                    && s.oy.abs() < 1e7
                    && s.sprite.len() < 128
                    && s.values.len() == FIELDS.len()
                    && s.values.iter().all(Scalar::sane)
                    && s.fly.is_none_or(|(x, y)| {
                        x.is_finite() && y.is_finite() && x.abs() < 1e6 && y.abs() < 1e6
                    })
            })
    }
}
#[derive(Clone, Debug, Encode, Decode)]
pub struct Hit {
    pub run: u64,
    pub room: String,
    pub level: u32,
    pub sub: u32,
    pub id: u32,
    pub args: Vec<Scalar>,
    pub kx: f64,
    pub ky: f64,
    pub rng: super::rng::State,
}
impl Hit {
    pub fn sane(&self) -> bool {
        self.run != 0
            && self.id != 0
            && self.room.len() < 64
            && self.args.len() == 9
            && self.args.iter().all(Scalar::sane)
            && self.kx.is_finite()
            && self.ky.is_finite()
            && self.kx.abs() < 1e6
            && self.ky.abs() < 1e6
            && self.rng.sane()
    }
}
#[derive(Clone)]
struct Entry {
    inst: V,
    obj: i16,
    ox: f64,
    oy: f64,
}
#[derive(Default)]
struct World {
    room: String,
    level: (u32, u32),
    run: u64,
    entries: HashMap<u32, Entry>,
    last: HashMap<u32, State>,
    gone: HashSet<u32>,
    counts: HashMap<u32, u32>,
    ticks: u32,
    frames: u32,
}
static WORLD: LazyLock<Mutex<World>> = LazyLock::new(|| Mutex::new(World::default()));
static OUT: Mutex<Vec<Hit>> = Mutex::new(Vec::new());
static IN: Mutex<Vec<(bool, Hit)>> = Mutex::new(Vec::new());
static PRESENT: AtomicBool = AtomicBool::new(false);
static FLYING_STEP: AtomicUsize = AtomicUsize::new(0);
pub fn install(g: G) {
    if FLYING_STEP.load(Ordering::Relaxed) == 0 {
        match crate::hooks::install(
            g.rt,
            "gml_Object_objFlyingTankMum_Step_0",
            flying_step as *const (),
        ) {
            Ok(p) => FLYING_STEP.store(p as usize, Ordering::SeqCst),
            Err(e) => crate::log(&e),
        }
    }
}
// Drum.Step already has a projectile-event detour; that detour calls this for
// untagged world barrels, preserving the RNG handling of projectile-born drums.
pub fn client_step(g: G, e: V, flying: bool) -> bool {
    if !client() || !gameplay(g) || !managed(g, e) {
        return false;
    }
    // Native collision/lifetime simulation would fight the authoritative pose
    // and apply collision damage or detonate a tank a second time.
    let _cosmetic = super::projectiles::presentation_only();
    if flying {
        let v = g.get(e, "v2_fly");
        if g.num(v, "x").hypot(g.num(v, "y")) > 0.0 {
            method(g, e, "create_particles", &[]);
        }
        method(g, e, "propane_update_geometry", &[]);
        if is_struct(&g.get(e, "light_overlay")) {
            method(g, e, "update_light_overlay", &[]);
        }
    }
    method(g, e, "update_sticky_thing", &[]);
    true
}
unsafe extern "C-unwind" fn flying_step(s: *mut std::ffi::c_void, o: *mut std::ffi::c_void) {
    if let Some(g) = G::new() {
        if unsafe { g.rt.builtin_var_of("id", s) }.is_some_and(|e| client_step(g, e, true)) {
            return;
        }
    }
    let f: unsafe extern "C-unwind" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) =
        unsafe { std::mem::transmute(FLYING_STEP.load(Ordering::Relaxed)) };
    unsafe { f(s, o) };
}
fn run() -> u64 {
    super::rooms::RUN_SEED.load(Ordering::Relaxed)
}
fn client() -> bool {
    super::enemies::PUPPET_MODE.load(Ordering::Relaxed)
}
fn gameplay(g: G) -> bool {
    let r = g.room_name();
    run() != 0 && (r.starts_with("Room_fight") || r.starts_with("Room_hub"))
}
pub fn reset() {
    *WORLD.lock().unwrap() = World::default();
    OUT.lock().unwrap().clear();
    IN.lock().unwrap().clear();
    BUTTONS.lock().unwrap().clear();
}
fn kind(g: G, obj: V) -> bool {
    let base = g.asset("objColAll");
    let entities = g.asset("objColEntities");
    (crate::engine::key(&obj) == crate::engine::key(&base)
        || truthy(&g.call("object_is_ancestor", &[obj, base])))
        && crate::engine::key(&obj) != crate::engine::key(&entities)
        && !truthy(&g.call("object_is_ancestor", &[obj, entities]))
}
pub fn managed(g: G, e: V) -> bool {
    g.exists(e) && g.num(e, "focmp_shot_id") == 0.0 && kind(g, g.get(e, "object_index"))
}
fn sync_room(g: G) {
    let room = g.room_name();
    let level = super::rooms::level_sub(g);
    let mut w = WORLD.lock().unwrap();
    if w.run != run() || w.room != room || w.level != level {
        *w = World {
            room,
            level,
            run: run(),
            ..Default::default()
        };
    }
}
fn tag(g: G, e: V) -> u32 {
    if let Some(id) = g.get(e, "focmp_prop_id").as_f64().filter(|id| *id > 0.0) {
        WORLD
            .lock()
            .unwrap()
            .entries
            .entry(id as u32)
            .or_insert_with(|| Entry {
                inst: e,
                obj: asset_id(g.get(e, "object_index")) as i16,
                ox: g.num(e, "xstart"),
                oy: g.num(e, "ystart"),
            });
        return id as u32;
    }
    let obj = asset_id(g.get(e, "object_index")) as i16;
    let ox = g.num(e, "xstart");
    let oy = g.num(e, "ystart");
    let mut hash = 2166136261u32;
    for b in obj
        .to_le_bytes()
        .into_iter()
        .chain(ox.to_bits().to_le_bytes())
        .chain(oy.to_bits().to_le_bytes())
    {
        hash = (hash ^ b as u32).wrapping_mul(16777619);
    }
    let mut w = WORLD.lock().unwrap();
    let id = loop {
        let ordinal = w.counts.entry(hash).or_default();
        let id = (hash.wrapping_add(ordinal.wrapping_mul(0x9e3779b9))).max(1);
        *ordinal += 1;
        // Snapshots may have registered a host-created prop before its native
        // replacement is created here. Never reuse that prop's identity.
        if !w.entries.contains_key(&id) && !w.gone.contains(&id) {
            break id;
        }
    };
    w.entries.insert(
        id,
        Entry {
            inst: e,
            obj,
            ox,
            oy,
        },
    );
    drop(w);
    g.set(e, "focmp_prop_id", real(id as f64));
    super::visual_damage::install(g);
    id
}
fn method(g: G, e: V, name: &str, args: &[V]) -> V {
    let m = g.get(e, name);
    if !truthy(&g.call("is_method", &[m])) {
        return V::undefined();
    }
    let arr = g.call("array_create", &[real(args.len() as f64)]);
    // The instance owns the temporary argument array across native allocation.
    g.set(e, "focmp_prop_args", arr);
    for (i, v) in args.iter().enumerate() {
        g.call("array_set", &[arr, real(i as f64), *v]);
    }
    let bound = g.call("method", &[e, m]);
    let result = g.call("method_call", &[bound, arr]);
    if g.exists(e) {
        g.set(e, "focmp_prop_args", V::undefined());
    }
    result
}
// Called by existing native set_damage detours. Return the hit to publish after
// the host's native callback; clients queue a request and skip local mutation.
pub unsafe fn damage(g: G, e: V, n: i32, a: *mut *mut V) -> Option<Hit> {
    if !gameplay(g) || !managed(g, e) || PRESENT.load(Ordering::Relaxed) || n < 5 || a.is_null() {
        return None;
    }
    let mut args = vec![Scalar::Undefined; 9];
    let mut kx = 0.0;
    let mut ky = 0.0;
    for i in 0..(n as usize).min(9) {
        let ptr = unsafe { *a.add(i) };
        if ptr.is_null() {
            return None;
        }
        let v = unsafe { *ptr };
        if i == 3 {
            kx = g.num(v, "x");
            ky = g.num(v, "y");
        } else if i != 7 {
            args[i] = Scalar::capture(v);
        }
    }
    let (level, sub) = super::rooms::level_sub(g);
    let h = Hit {
        run: run(),
        room: g.room_name(),
        level,
        sub,
        id: tag(g, e),
        args,
        kx,
        ky,
        rng: super::rng::capture()?,
    };
    h.sane().then_some(h)
}
pub fn publish(hit: Hit) {
    OUT.lock().unwrap().push(hit);
}
pub fn receive(g: G, from: PeerId, committed: bool, hit: Hit) {
    if !hit.sane()
        || hit.run != run()
        || hit.room != g.room_name()
        || (hit.level, hit.sub) != super::rooms::level_sub(g)
    {
        return;
    }
    if client() && from == 0 && committed {
        apply_hit(g, true, hit);
    } else if !client() && from != 0 && !committed {
        IN.lock().unwrap().push((false, hit));
    }
}
fn apply_hit(g: G, committed: bool, h: Hit) {
    let e = WORLD.lock().unwrap().entries.get(&h.id).cloned();
    let Some(e) = e.filter(|e| g.exists(e.inst)) else {
        return;
    };
    let vector = g.get(e.inst, "v2_move");
    let k = if is_struct(&vector) {
        g.call("variable_clone", &[vector])
    } else {
        g.call("json_parse", &[g.s("{\"x\":0,\"y\":0}")])
    };
    g.set(e.inst, "focmp_prop_knock", k);
    g.set(k, "x", real(h.kx));
    g.set(k, "y", real(h.ky));
    let mut args: Vec<V> = h.args.iter().map(Scalar::value).collect();
    args[3] = k;
    args[7] = real(-4.0);
    let _rng = super::rng::Scope::replay(h.rng);
    // Confirmed prop callbacks provide native dents/audio/destruction, while
    // only the host emits their payload projectiles through the shot protocol.
    PRESENT.store(committed, Ordering::Relaxed);
    let _cosmetic = committed.then(super::projectiles::presentation_only);
    method(g, e.inst, "set_damage", &args);
    PRESENT.store(false, Ordering::Relaxed);
}
pub fn tick(g: G, net: &Net) {
    if !gameplay(g) {
        return;
    }
    sync_room(g);
    WORLD.lock().unwrap().frames += 1;
    for e in g.instances("objColAll") {
        if managed(g, e) {
            tag(g, e);
        }
    }
    buttons(g, net);
    for (committed, h) in std::mem::take(&mut *IN.lock().unwrap()) {
        apply_hit(g, committed, h);
    }
    for hit in std::mem::take(&mut *OUT.lock().unwrap()) {
        net.broadcast(Msg::PropHit {
            committed: net.role == Role::Host,
            hit,
        });
    }
}
pub fn capture(g: G) -> Option<Snapshot> {
    if !gameplay(g) {
        return None;
    }
    let mut w = WORLD.lock().unwrap();
    w.ticks = w.ticks.wrapping_add(1);
    // Full state once per second; deltas between, including rolling tombstones.
    let full = w.ticks % 60 == 1;
    let mut states = Vec::new();
    for (id, e) in w.entries.clone() {
        if !g.exists(e.inst) {
            w.gone.insert(id);
            continue;
        }
        let state = State {
            id,
            obj: e.obj,
            ox: e.ox,
            oy: e.oy,
            sprite: g.sprite_name(g.get(e.inst, "sprite_index")),
            values: FIELDS
                .iter()
                .map(|f| Scalar::capture(g.get(e.inst, f)))
                .collect(),
            fly: is_struct(&g.get(e.inst, "v2_fly")).then(|| {
                let v = g.get(e.inst, "v2_fly");
                (g.num(v, "x"), g.num(v, "y"))
            }),
        };
        if full || w.last.get(&id) != Some(&state) {
            w.last.insert(id, state.clone());
            states.push(state);
        }
    }
    if states.is_empty() && w.gone.is_empty() {
        return None;
    }
    Some(Snapshot {
        run: run(),
        room: w.room.clone(),
        level: w.level.0,
        sub: w.level.1,
        states,
        gone: w.gone.iter().copied().collect(),
    })
}
pub fn apply(g: G, s: Snapshot) {
    if !s.sane()
        || s.run != run()
        || s.room != g.room_name()
        || (s.level, s.sub) != super::rooms::level_sub(g)
        || !client()
    {
        return;
    }
    sync_room(g);
    for state in s.states {
        let entry = WORLD.lock().unwrap().entries.get(&state.id).cloned();
        let e = if let Some(e) = entry.filter(|e| g.exists(e.inst)) {
            e.inst
        } else {
            if WORLD.lock().unwrap().frames < 30 {
                continue;
            }
            let obj = real(state.obj as f64);
            if !truthy(&g.call("object_exists", &[obj])) || !kind(g, obj) {
                continue;
            }
            let _cosmetic = super::projectiles::presentation_only();
            let e = g.call(
                "instance_create_depth",
                &[real(state.ox), real(state.oy), real(400.0), obj],
            );
            if !g.exists(e) {
                continue;
            }
            g.set(e, "focmp_prop_id", real(state.id as f64));
            WORLD.lock().unwrap().entries.insert(
                state.id,
                Entry {
                    inst: e,
                    obj: state.obj,
                    ox: state.ox,
                    oy: state.oy,
                },
            );
            super::visual_damage::install(g);
            e
        };
        for (f, v) in FIELDS.iter().zip(&state.values) {
            if !matches!(v, Scalar::Undefined) {
                g.set(e, f, v.value());
            }
        }
        if !state.sprite.is_empty() {
            g.set(e, "sprite_index", g.asset(&state.sprite));
        }
        if let Some((x, y)) = state.fly {
            let v = g.get(e, "v2_fly");
            if is_struct(&v) {
                g.set(v, "x", real(x));
                g.set(v, "y", real(y));
            }
        }
        method(g, e, "propane_update_geometry", &[]);
        if is_struct(&g.get(e, "light_overlay")) {
            method(g, e, "update_light_overlay", &[]);
        }
    }
    for id in s.gone {
        WORLD.lock().unwrap().gone.insert(id);
        let e = WORLD.lock().unwrap().entries.get(&id).cloned();
        if let Some(e) = e.filter(|e| g.exists(e.inst)) {
            let _cosmetic = super::projectiles::presentation_only();
            // Attached gameplay triggers are emitted only by the host.
            if is_struct(&g.get(e.inst, "array_sticky"))
                || g.get(e.inst, "array_sticky").kind() == crate::gm::KIND_ARRAY
            {
                g.set(e.inst, "array_sticky", g.call("array_create", &[real(0.0)]));
            }
            g.call("instance_destroy", &[e.inst]);
        }
    }
}

#[derive(Clone, Debug, Encode, Decode)]
pub struct Interact {
    pub run: u64,
    pub room: String,
    pub level: u32,
    pub sub: u32,
    pub id: u32,
    pub rng: super::rng::State,
}
impl Interact {
    pub fn sane(&self) -> bool {
        self.run != 0 && self.id != 0 && self.room.len() < 64 && self.rng.sane()
    }
}
static BUTTONS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
fn button(g: G, e: V) -> bool {
    let obj = g.get(e, "object_index");
    [
        "objIngameButton",
        "objIngameButton_press_only",
        "objIngameButton_shoot",
    ]
    .iter()
    .any(|n| asset_id(obj) == asset_id(g.asset(n)))
}
pub unsafe fn filter_input(s: *mut std::ffi::c_void, n: i32, a: *mut *mut V, r: *mut V) {
    if r.is_null()
        || !truthy(&unsafe { *r })
        || n < 1
        || a.is_null()
        || unsafe { (*a).is_null() || (**a).as_f64() != Some(8.0) }
    {
        return;
    }
    let Some(g) = G::new().filter(|g| gameplay(*g)) else {
        return;
    };
    let Some(e) = (unsafe { g.rt.builtin_var_of("id", s) }).filter(|e| button(g, *e)) else {
        return;
    };
    // InputPressed runs before the native button's 32-pixel range check.
    // Preserve that local range; the host's looser check tolerates transit time.
    if !g.first("objPlayer").is_some_and(|p| {
        !truthy(&g.get(p, "dead"))
            && (g.num(e, "x") - g.num(p, "x")).hypot(g.num(e, "y") - g.num(p, "y")) < 32.0
    }) {
        unsafe { *r = boolean(false) };
        return;
    }
    let id = tag(g, e);
    let mut buttons = BUTTONS.lock().unwrap();
    if !buttons.contains(&id) {
        buttons.push(id);
    }
    unsafe { *r = boolean(false) };
}
fn press(g: G, e: V) {
    if !g.exists(e) || !button(g, e) || !truthy(&g.get(e, "connected")) {
        return;
    }
    if asset_id(g.get(e, "object_index")) == asset_id(g.asset("objIngameButton_press_only")) {
        let target = g.get(e, "connect_id");
        if !g.exists(target) {
            return;
        }
        // Native toggle returns 0/1 for the resulting state, 2 for rejection.
        match method(g, target, "toggle", &[]).as_f64() {
            Some(state @ (0.0 | 1.0)) => {
                g.set(e, "image_index", real(state));
                method(g, e, "succes", &[]);
            }
            Some(2.0) => {
                method(g, e, "error", &[]);
            }
            _ => {}
        }
    } else {
        method(g, e, "button_pressed", &[]);
    }
}
fn do_interact(
    g: G,
    net: &Net,
    from: PeerId,
    committed: bool,
    action: Interact,
    position: Option<(f64, f64)>,
) {
    if !action.sane()
        || action.run != run()
        || action.room != g.room_name()
        || (action.level, action.sub) != super::rooms::level_sub(g)
    {
        return;
    }
    let entry = WORLD.lock().unwrap().entries.get(&action.id).cloned();
    let Some(e) = entry.filter(|e| g.exists(e.inst) && button(g, e.inst)) else {
        return;
    };
    if net.role == Role::Host && !committed {
        let Some((x, y)) = position else {
            return;
        };
        if (g.num(e.inst, "x") - x).hypot(g.num(e.inst, "y") - y) > 64.0 {
            return;
        }
        let action = Interact {
            rng: super::rng::capture().unwrap_or(action.rng),
            ..action
        };
        press(g, e.inst);
        net.broadcast(Msg::PropInteract {
            committed: true,
            action,
        });
    } else if net.role == Role::Client && from == 0 && committed {
        let _rng = super::rng::Scope::replay(action.rng);
        let _cosmetic = super::projectiles::presentation_only();
        press(g, e.inst);
    }
}
pub fn receive_interact(g: G, c: &super::Coop, from: PeerId, committed: bool, action: Interact) {
    let Some(net) = &c.net else {
        return;
    };
    let pos = c
        .remotes
        .get(&from)
        .filter(|r| r.seen.elapsed().as_secs_f32() < 1.0 && !r.busy)
        .and_then(|r| r.state.as_ref())
        .filter(|s| s.room == g.room_name() && !s.dead)
        .map(|s| (s.x as f64, s.y as f64));
    do_interact(g, net, from, committed, action, pos);
}
fn buttons(g: G, net: &Net) {
    let (level, sub) = super::rooms::level_sub(g);
    for id in std::mem::take(&mut *BUTTONS.lock().unwrap()) {
        let Some(rng) = super::rng::capture() else {
            continue;
        };
        let action = Interact {
            run: run(),
            room: g.room_name(),
            level,
            sub,
            id,
            rng,
        };
        if net.role == Role::Host {
            let pos = g
                .first("objPlayer")
                .filter(|p| !truthy(&g.get(*p, "dead")))
                .map(|p| (g.num(p, "x"), g.num(p, "y")));
            do_interact(g, net, net.my_id, false, action, pos);
        } else {
            net.broadcast(Msg::PropInteract {
                committed: false,
                action,
            });
        }
    }
}
