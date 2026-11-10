//! Native shot replication. Capture the actual compiled shot at the shared
//! factory, including modifier callbacks, and replay it once on each peer.
use super::shot_graph::Graph;
use crate::engine::{G, V, boolean, key, real, truthy};
use crate::net::{Msg, Net, PeerId, ShotBatch, ShotFire};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
static FACTORY: AtomicUsize = AtomicUsize::new(0);
static DEPTH: AtomicUsize = AtomicUsize::new(0);
static PRESENT_ONLY: AtomicBool = AtomicBool::new(false);
static REPLAY: AtomicBool = AtomicBool::new(false);
static COMPILE: AtomicUsize = AtomicUsize::new(0);
static OUT: Mutex<VecDeque<ShotFire>> = Mutex::new(VecDeque::new());
// Immutable wire snapshots survive native structs being edited or collected.
static DEFINITIONS: LazyLock<Mutex<HashMap<u64, Graph>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static DIRTY: Mutex<VecDeque<u64>> = Mutex::new(VecDeque::new());
static ANNOUNCED: LazyLock<Mutex<HashSet<u64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static REMOTE_GRAPHS: LazyLock<Mutex<HashMap<(PeerId, u64), Graph>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
struct Pending {
    peer: PeerId,
    room: String,
    level: u32,
    sub: u32,
    received: Instant,
    fire: ShotFire,
}
static PENDING: Mutex<Vec<Pending>> = Mutex::new(Vec::new());
static REQUESTED: LazyLock<Mutex<HashMap<(PeerId, u64), Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static RECOVERIES: AtomicUsize = AtomicUsize::new(0);
static EXPIRED: AtomicUsize = AtomicUsize::new(0);
const MAX_GRAPH_BYTES: usize = 256 * 1024;
fn run() -> u64 {
    super::rooms::RUN_SEED.load(Ordering::Relaxed)
}
fn register(graph: Graph) -> Option<u64> {
    if !graph.sane() || bitcode::encode(&graph).len() > MAX_GRAPH_BYTES {
        return None;
    }
    let id = hash(&graph);
    let mut definitions = DEFINITIONS.lock().unwrap();
    if let std::collections::hash_map::Entry::Vacant(entry) = definitions.entry(id) {
        entry.insert(graph);
        DIRTY.lock().unwrap().push_back(id);
    }
    Some(id)
}
// set_shots is the native compiler used after card/modifier edits and gun swaps.
unsafe extern "C-unwind" fn compile(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    let f: crate::hooks::ScriptFn = unsafe { std::mem::transmute(COMPILE.load(Ordering::Relaxed)) };
    let result = unsafe { f(s, o, r, n, a) };
    if run() != 0 && !replaying() {
        if let Some(g) = G::new() {
            if let Some(gun) = unsafe { g.rt.builtin_var_of("id", s) } {
                // Enemy definitions are supplied only by the host. Mock guns compile
                // inventory edits in personal menus, even with no objPlayer present.
                let owner = g.get(gun, "Owner");
                if !(g.exists(owner)
                    && g.num(owner, "focmp_gid") > 0.0
                    && super::enemies::PUPPET_MODE.load(Ordering::Relaxed))
                {
                    let shots = g.get(gun, "shots");
                    for i in 0..g.arr_len(shots) {
                        if let Some(graph) = Graph::capture(g, g.arr_get(shots, i)) {
                            register(graph);
                        }
                    }
                }
            }
        }
    }
    result
}
static RECEIVED: LazyLock<Mutex<HashMap<(PeerId, u64), V>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
struct Depth;
impl Depth {
    fn enter() -> Self {
        DEPTH.fetch_add(1, Ordering::Relaxed);
        Self
    }
}
impl Drop for Depth {
    fn drop(&mut self) {
        DEPTH.fetch_sub(1, Ordering::Relaxed);
    }
}
static REPLAYED: AtomicUsize = AtomicUsize::new(0);
static REJECTED: AtomicUsize = AtomicUsize::new(0);
#[derive(Default)]
pub struct Projectiles {
    seq: u32,
    received: HashMap<PeerId, u32>,
    sent: u64,
    observed: u64,
}
pub fn hash(graph: &Graph) -> u64 {
    bitcode::encode(graph)
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ *b as u64).wrapping_mul(0x100000001b3)
        })
}
pub fn install_hooks(g: G) {
    super::rng::install(g);
    super::shot_events::install(g);
    if COMPILE.load(Ordering::Relaxed) == 0 {
        match crate::hooks::install(
            g.rt,
            "gml_Script_anon@1721@gml_Object_objGun_Create_0",
            compile as *const (),
        ) {
            Ok(p) => COMPILE.store(p as usize, Ordering::SeqCst),
            Err(e) => crate::log(&format!("projectiles compiler: {e}")),
        }
    }
    if FACTORY.load(Ordering::Relaxed) == 0 {
        match crate::hooks::install(g.rt, "gml_Script_create_shot", factory as *const ()) {
            Ok(p) => FACTORY.store(p as usize, Ordering::SeqCst),
            Err(e) => crate::log(&format!("projectiles: {e}")),
        }
    }
}
unsafe extern "C-unwind" fn factory(
    s: *mut c_void,
    o: *mut c_void,
    ret: *mut V,
    argc: i32,
    args: *mut *mut V,
) -> *mut V {
    if PRESENT_ONLY.load(Ordering::Relaxed) {
        if !ret.is_null() {
            unsafe { *ret = V::undefined() };
        }
        return ret;
    }
    let g = G::new();
    let source = g.and_then(|g| unsafe { g.rt.builtin_var_of("id", s) });
    let remote = REPLAY.load(Ordering::Relaxed);
    // Native prop payloads belong to the host. Remote copies arrive through
    // this same factory with REPLAY set; do not emit another local explosion.
    if !remote && super::enemies::PUPPET_MODE.load(Ordering::Relaxed)
        && g.zip(source).is_some_and(|(g,e)|g.get(e,"object_index").v==g.asset("objMockGun").v) {
        if !ret.is_null(){unsafe{*ret=V::undefined()};}return ret;
    }

    let depth = DEPTH.load(Ordering::Relaxed);
    // A received projectile may present collision effects, but only its owner
    // decides whether/where a delayed child is emitted. Immediate nested shots
    // within the received factory invocation remain part of that invocation.
    if depth == 0 && !remote && g.zip(source).is_some_and(|(g, source)| visual(g, source)) {
        if !ret.is_null() {
            unsafe { *ret = V::undefined() };
        }
        return ret;
    }
    let _depth = Depth::enter();
    let mut group = None;
    if depth == 0
        && !remote
        && argc == 8
        && !args.is_null()
        && super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0
    {
        if let Some((g, gun)) = g.zip(source) {
            let owner = g.get(gun, "Owner");
            let local = g
                .first("objPlayer")
                .is_some_and(|p| key(&p) == key(&owner) || key(&p) == key(&gun));
            let object = g.str_of(&g.call("object_get_name", &[g.get(gun, "object_index")]));
            let descendant = crate::engine::is_struct(&g.get(gun, "shot"));
            let world =
                object == "objMockGun" && !super::enemies::PUPPET_MODE.load(Ordering::Relaxed);
            let enemy = g.exists(owner)
                && g.num(owner, "focmp_gid") > 0.0
                && !super::enemies::PUPPET_MODE.load(Ordering::Relaxed);
            if local || world || descendant || enemy {
                let a: Vec<V> = (0..8).map(|i| unsafe { **args.add(i) }).collect();
                // Definitions can change after procs. Hash their current value,
                // not a once-cached pointer to a mutable native struct.
                let definition = Graph::capture(g, a[4]);
                if let (Some(graph), Some(rng)) = (definition, super::rng::capture()) {
                    let id = register(graph);
                    let mut collision_objects = Vec::new();
                    let mut collision_tiles = false;
                    if a[3].kind() == crate::gm::KIND_ARRAY {
                        for i in 0..g.arr_len(a[3]) {
                            let item = g.arr_get(a[3], i);
                            let label = g.str_of(&item);
                            if label.starts_with("ref tilemap") {
                                collision_tiles = true;
                            } else if label.starts_with("ref object") {
                                collision_objects
                                    .push(g.str_of(&g.call("object_get_name", &[item])));
                            }
                        }
                    }
                    let event = super::shot_events::next_id();
                    group = Some(super::shot_events::group(
                        super::MY_ID.load(Ordering::Relaxed),
                        event,
                        false,
                        enemy || truthy(&g.get(gun, "focmp_enemy_attack")),
                    ));
                    if let Some(id) = id {
                        OUT.lock().unwrap().push_back(ShotFire {
                            enemy: enemy || truthy(&g.get(gun, "focmp_enemy_attack")),
                            id: event,
                            definition: id,
                            collision_objects,
                            collision_tiles,
                            rng,
                            x: a[0].as_f64().unwrap_or(0.0),
                            y: a[1].as_f64().unwrap_or(0.0),
                            angle: a[2].as_f64().unwrap_or(0.0),
                            flag: truthy(&a[5]),
                            multiplier: a[6].as_f64().unwrap_or(1.0),
                            target: if g.exists(a[7]) {
                                g.num(a[7], "focmp_gid") as u16
                            } else {
                                0
                            },
                            weapon: if descendant {
                                String::new()
                            } else {
                                g.sprite_name(g.get(gun, "sprite_index"))
                            },
                        });
                    } else {
                        REJECTED.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    REJECTED.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    let f: crate::hooks::ScriptFn = unsafe { std::mem::transmute(FACTORY.load(Ordering::Relaxed)) };
    let result = unsafe { f(s, o, ret, argc, args) };
    drop(group);
    result
}
/// Called inside the local player's Step, where native scripts have valid context.
pub fn replay(g: G, s: *mut c_void) {
    let pending = std::mem::take(&mut *PENDING.lock().unwrap());
    if pending.is_empty() {
        return;
    }
    super::visual_damage::install(g);
    let Some(player) = g.first("objPlayer") else {
        return;
    };
    let gun = g.get(player, "Gun");
    if !g.exists(gun) {
        return;
    }
    let local_collisions = g.get(gun, "col_array");
    let f: crate::hooks::ScriptFn = unsafe { std::mem::transmute(FACTORY.load(Ordering::Relaxed)) };
    let mut waiting = Vec::new();
    for pending in pending {
        let Pending {
            peer,
            room,
            level,
            sub,
            received,
            fire,
        } = pending;
        if room != g.room_name() || (level, sub) != super::rooms::level_sub(g) {
            continue;
        }
        if received.elapsed() > Duration::from_secs(1) {
            EXPIRED.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let cached = RECEIVED
            .lock()
            .unwrap()
            .get(&(peer, fire.definition))
            .copied();
        let shot = cached.or_else(|| {
            let graph = REMOTE_GRAPHS
                .lock()
                .unwrap()
                .get(&(peer, fire.definition))
                .cloned()?;
            let shot = graph.restore_visual(g)?;
            g.call("array_push", &[g.global("focmp_shot_roots"), shot]);
            RECEIVED
                .lock()
                .unwrap()
                .insert((peer, fire.definition), shot);
            Some(shot)
        });
        let Some(shot) = shot else {
            waiting.push(Pending {
                peer,
                room,
                level,
                sub,
                received,
                fire,
            });
            continue;
        };
        let _rng = super::rng::Scope::replay(fire.rng);
        let _group = super::shot_events::group(peer, fire.id, true, fire.enemy);
        let collisions = g.call("array_create", &[real(0.0)]);
        for name in &fire.collision_objects {
            g.call("array_push", &[collisions, g.asset(name)]);
        }
        if fire.collision_tiles && local_collisions.kind() == crate::gm::KIND_ARRAY {
            for i in 0..g.arr_len(local_collisions) {
                let item = g.arr_get(local_collisions, i);
                if g.str_of(&item).starts_with("ref tilemap") {
                    g.call("array_push", &[collisions, item]);
                }
            }
        }
        let mut args = [
            real(fire.x),
            real(fire.y),
            real(fire.angle),
            collisions,
            shot,
            boolean(fire.flag),
            real(fire.multiplier),
            g.instances("objEnemyMASTER")
                .into_iter()
                .find(|e| fire.target != 0 && g.num(*e, "focmp_gid") as u16 == fire.target)
                .unwrap_or(real(-4.0)),
        ];
        let mut ptrs: Vec<*mut V> = args.iter_mut().map(|v| v as *mut _).collect();
        let mut ret = V::undefined();
        let _remote = remote_scope();
        let _depth = Depth::enter();
        unsafe { f(s, s, &mut ret, 8, ptrs.as_mut_ptr()) };
        REPLAYED.fetch_add(1, Ordering::Relaxed);
    }
    PENDING.lock().unwrap().extend(waiting);
}
pub fn visual(g: G, source: V) -> bool {
    if !g.exists(source) {
        return false;
    }
    let shot = g.get(source, "shot");
    truthy(&g.get(source, "focmp_visual"))
        || (crate::engine::is_struct(&shot) && truthy(&g.get(shot, "focmp_visual")))
}
pub struct Presentation(bool);
pub fn presentation_only() -> Presentation {
    Presentation(PRESENT_ONLY.swap(true, Ordering::Relaxed))
}
impl Drop for Presentation {
    fn drop(&mut self) {
        PRESENT_ONLY.store(self.0, Ordering::Relaxed);
    }
}
pub struct Remote(bool);
pub fn remote_scope() -> Remote {
    Remote(REPLAY.swap(true, Ordering::Relaxed))
}
impl Drop for Remote {
    fn drop(&mut self) {
        REPLAY.store(self.0, Ordering::Relaxed);
    }
}
pub fn replaying() -> bool {
    REPLAY.load(Ordering::Relaxed)
}
impl Projectiles {
    /// Full reset only when leaving/starting a run, never for personal rooms.
    pub fn clear(&mut self, g: G) {
        *self = Self::default();
        self.room_changed();
        super::shot_events::reset();
        DEFINITIONS.lock().unwrap().clear();
        DIRTY.lock().unwrap().clear();
        ANNOUNCED.lock().unwrap().clear();
        REMOTE_GRAPHS.lock().unwrap().clear();
        RECEIVED.lock().unwrap().clear();
        for count in [&REPLAYED, &REJECTED, &RECOVERIES, &EXPIRED] {
            count.store(0, Ordering::Relaxed);
        }
        g.call(
            "variable_global_set",
            &[
                g.s("focmp_shot_roots"),
                g.call("array_create", &[real(0.0)]),
            ],
        );
    }
    pub fn room_changed(&mut self) {
        OUT.lock().unwrap().clear();
        PENDING.lock().unwrap().clear();
        REQUESTED.lock().unwrap().clear();
    }
    pub fn capture(&mut self, g: G) -> ShotBatch {
        self.seq = self.seq.wrapping_add(1);
        let mut definitions = Vec::new();
        let mut bytes = 0;
        let cache = DEFINITIONS.lock().unwrap();
        let mut dirty = DIRTY.lock().unwrap();
        let mut announced = ANNOUNCED.lock().unwrap();
        while let Some(id) = dirty.front().copied() {
            let Some(graph) = cache.get(&id) else {
                dirty.pop_front();
                continue;
            };
            let size = bitcode::encode(graph).len();
            if definitions.len() >= 32 || bytes + size > MAX_GRAPH_BYTES {
                break;
            }
            dirty.pop_front();
            bytes += size;
            definitions.push((id, graph.clone()));
            announced.insert(id);
        }
        let mut fires = Vec::new();
        let mut out = OUT.lock().unwrap();
        while let Some(fire) = out.front() {
            if fires.len() >= 512 || !announced.contains(&fire.definition) {
                break;
            }
            fires.push(out.pop_front().unwrap());
        }
        self.sent += fires.len() as u64;
        let (level, sub) = super::rooms::level_sub(g);
        ShotBatch {
            run: run(),
            room: g.room_name(),
            level,
            sub,
            seq: self.seq,
            definitions,
            fires,
        }
    }
    fn definition(&mut self, peer: PeerId, id: u64, graph: Graph) {
        if !graph.sane() || hash(&graph) != id || bitcode::encode(&graph).len() > MAX_GRAPH_BYTES {
            REJECTED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        REMOTE_GRAPHS
            .lock()
            .unwrap()
            .entry((peer, id))
            .or_insert(graph);
        if REQUESTED.lock().unwrap().remove(&(peer, id)).is_some() {
            RECOVERIES.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn receive(&mut self, g: G, peer: PeerId, b: ShotBatch) {
        if b.run != run() || b.run == 0 {
            return;
        }
        let seq = self.received.entry(peer).or_default();
        if b.seq <= *seq {
            return;
        }
        *seq = b.seq;
        // Definition delivery is independent of room and local player existence.
        for (id, graph) in b.definitions {
            self.definition(peer, id, graph);
        }
        if b.room != g.room_name() || (b.level, b.sub) != super::rooms::level_sub(g) {
            return;
        }
        self.observed += b.fires.len() as u64;
        let mut pending = PENDING.lock().unwrap();
        for fire in b.fires {
            if pending.len() >= 1024 {
                EXPIRED.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            pending.push(Pending {
                peer,
                room: b.room.clone(),
                level: b.level,
                sub: b.sub,
                received: Instant::now(),
                fire,
            });
        }
    }
    pub fn message(&mut self, net: &Net, from: PeerId, msg: Msg) {
        match msg {
            Msg::ShotDefinitionRequest { run: r, owner, id }
                if r == run() && r != 0 && owner == net.my_id =>
            {
                if let Some(graph) = DEFINITIONS.lock().unwrap().get(&id).cloned() {
                    net.broadcast(Msg::ShotDefinition { run: r, id, graph });
                }
            }
            Msg::ShotDefinition { run: r, id, graph } if r == run() && r != 0 => {
                self.definition(from, id, graph)
            }
            _ => {}
        }
    }
    pub fn recover(&self, net: &Net) {
        let pending = PENDING.lock().unwrap();
        let graphs = REMOTE_GRAPHS.lock().unwrap();
        let mut requested = REQUESTED.lock().unwrap();
        requested.retain(|key, _| pending.iter().any(|p| (p.peer, p.fire.definition) == *key));
        let mut count = 0;
        for p in pending.iter() {
            let key = (p.peer, p.fire.definition);
            if p.received.elapsed() > Duration::from_secs(1) || graphs.contains_key(&key) {
                continue;
            }
            if requested
                .get(&key)
                .is_some_and(|t| t.elapsed() < Duration::from_millis(250))
            {
                continue;
            }
            net.broadcast(Msg::ShotDefinitionRequest {
                run: run(),
                owner: p.peer,
                id: p.fire.definition,
            });
            requested.insert(key, Instant::now());
            count += 1;
            if count >= 32 {
                break;
            }
        }
    }
    pub fn update(&mut self, _g: G) {}
    pub fn describe(&self) -> String {
        format!(
            "native shots: sent {}, received {}, replayed {}, rejected {}, local definitions {}, remote definitions {}, recoveries {}, expired {}, sequence {}",
            self.sent,
            self.observed,
            REPLAYED.load(Ordering::Relaxed),
            REJECTED.load(Ordering::Relaxed),
            DEFINITIONS.lock().unwrap().len(),
            REMOTE_GRAPHS.lock().unwrap().len(),
            RECOVERIES.load(Ordering::Relaxed),
            EXPIRED.load(Ordering::Relaxed),
            self.seq
        )
    }
}
/// Explicit diagnostic fault injection: simulate a lost cache without changing
/// sequence history, native loadouts, or sender state. Recovery must repair it.
pub fn forget_remote() {
    REMOTE_GRAPHS.lock().unwrap().clear();
    RECEIVED.lock().unwrap().clear();
}
