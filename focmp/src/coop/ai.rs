//! Host-side enemy targeting: nearest living player, biased toward recent attackers.
//!
//! Enemies chase whatever instance is in their `Player` variable (verified
//! live: pointing it at an objPlayerDummy retargets the AI). Every few frames
//! the host picks, for each enemy, the candidate with the lowest score:
//!
//!   score = distance − AGGRO_BONUS × aggro_weight(attacker, age)
//!
//! where aggro_weight decays linearly from 1 to 0 over AGGRO_TIME after the
//! player last damaged that enemy. The current target is kept unless another
//! candidate beats it by HYSTERESIS px, so enemies don't flip-flop.
//!
//! Attribution comes from a detour on the enemy damage method (`set_damage`,
//! gml_Script_anon@741@gml_Object_objEnemyMASTER_Other_13): after the original
//! runs, we look at `other` (the instance that applied the damage) and map it
//! to a peer. Remote players' hits will report their peer id once shots are
//! replicated (bullets tagged `focmp_peer`). Unknown sources give no aggro.

use super::Remote;
use super::ghosts::Ghosts;
use crate::engine::{G, V, key, truthy};
use crate::hooks::{self, ScriptFn};
use crate::gm::RValue;
use crate::net::PeerId;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const EVERY_N_FRAMES: u32 = 6;
const AGGRO_BONUS: f64 = 320.0;
const AGGRO_TIME: Duration = Duration::from_secs(5);
const HYSTERESIS: f64 = 48.0;
const SET_DAMAGE: &str = "gml_Script_anon@741@gml_Object_objEnemyMASTER_Other_13";

/// enemy instance key → (attacker peer, when).
static AGGRO: LazyLock<Mutex<HashMap<u64, (PeerId, Instant)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
pub static ORIG_SET_DAMAGE: AtomicUsize = AtomicUsize::new(0);
pub static TRACE_DAMAGE: AtomicBool = AtomicBool::new(false);
static CHAINSAW_STEP: AtomicUsize = AtomicUsize::new(0);
static DOG_STEP: AtomicUsize = AtomicUsize::new(0);
static BOOM_STEP: AtomicUsize = AtomicUsize::new(0);
static RIOT_STEP: AtomicUsize = AtomicUsize::new(0);
static FIND_INSTANCE: AtomicUsize = AtomicUsize::new(0);
static CHASE_SELF: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
pub struct Ai {
    frame: u32,
    /// enemy key → (enemy ref, target peer) as last assigned.
    assigned: HashMap<u64, (V, PeerId)>,
}

struct Candidate {
    peer: PeerId,
    inst: V,
    x: f64,
    y: f64,
}

impl Ai {
    pub fn reset(&mut self, g: G) {
        // Hand every enemy back to the local player so single-player behaviour resumes.
        if let Some(p) = g.first("objPlayer") {
            for (_, (e, _)) in self.assigned.drain() {
                if g.exists(e) {
                    g.set(e, "Player", p);
                }
            }
        }
        self.assigned.clear();
        AGGRO.lock().unwrap().clear();
    }

    pub fn update(&mut self, g: G, ghosts: &Ghosts, remotes: &HashMap<PeerId, Remote>) {
        self.frame = self.frame.wrapping_add(1);
        if self.frame % EVERY_N_FRAMES != 0 {
            return;
        }
        let me = super::MY_ID.load(Ordering::Relaxed);
        let mut cands = Vec::new();
        if let Some(p) = g.first("objPlayer") {
            if !truthy(&g.get(p, "dead")) {
                cands.push(Candidate { peer: me, inst: p, x: g.num(p, "x"), y: g.num(p, "y") });
            }
        }
        for (&peer, _) in remotes {
            if let Some(gh) = ghosts.get(peer) {
                if !gh.dead && g.exists(gh.body) {
                    cands.push(Candidate { peer, inst: gh.body, x: gh.x as f64, y: gh.y as f64 });
                }
            }
        }
        if cands.is_empty() {
            // No living target; the shared game-over path handles a team wipe.
            self.assigned.retain(|_, (e, _)| g.exists(*e));
            return;
        }
        let aggro = AGGRO.lock().unwrap().clone();
        let mut seen = Vec::new();
        for e in g.instances("objEnemyMASTER") {
            if truthy(&g.get(e, "dead")) {
                continue;
            }
            let (ex, ey) = (g.num(e, "x"), g.num(e, "y"));
            let k = key(&e);
            seen.push(k);
            let score = |c: &Candidate| {
                let d = ((c.x - ex).powi(2) + (c.y - ey).powi(2)).sqrt();
                let bonus = match aggro.get(&k) {
                    Some((p, t)) if *p == c.peer && t.elapsed() < AGGRO_TIME => {
                        AGGRO_BONUS * (1.0 - t.elapsed().as_secs_f64() / AGGRO_TIME.as_secs_f64())
                    }
                    _ => 0.0,
                };
                d - bonus
            };
            let best = cands.iter().min_by(|a, b| score(a).total_cmp(&score(b))).unwrap();
            let cur = g.get(e, "Player");
            let cur_c = cands.iter().find(|c| key(&c.inst) == key(&cur));
            let pick = match cur_c {
                Some(c) if score(c) <= score(best) + HYSTERESIS => c,
                _ => best,
            };
            if cur_c.is_none_or(|c| c.peer != pick.peer) {
                g.set(e, "Player", pick.inst);
            }
            self.assigned.insert(k, (e, pick.peer));
        }
        self.assigned.retain(|k, _| seen.contains(k));
        AGGRO.lock().unwrap().retain(|k, (_, t)| seen.contains(k) && t.elapsed() < AGGRO_TIME * 2);
    }

    pub fn describe(&self, g: G) -> String {
        let aggro = AGGRO.lock().unwrap().clone();
        let mut s = String::new();
        let _ = writeln!(s, "{} enemies tracked (bonus {AGGRO_BONUS}px decaying over {:?})", self.assigned.len(), AGGRO_TIME);
        for (k, (e, peer)) in &self.assigned {
            let ag = aggro
                .get(k)
                .map(|(p, t)| format!("  aggro P{} {:.1}s ago", p + 1, t.elapsed().as_secs_f32()))
                .unwrap_or_default();
            let _ = writeln!(
                s,
                "  {} at ({:.0}, {:.0}) -> P{}{}",
                g.str_of(e),
                g.num(*e, "x"),
                g.num(*e, "y"),
                peer + 1,
                ag
            );
        }
        let _ = write!(s, "damage hook: {}", if ORIG_SET_DAMAGE.load(Ordering::Relaxed) != 0 { "installed" } else { "NOT installed" });
        s
    }
}

/// Credit `peer` with aggro on `enemy` (used when the host applies a remote hit).
pub fn note_aggro(enemy: V, peer: PeerId) {
    AGGRO.lock().unwrap().insert(key(&enemy), (peer, Instant::now()));
}

pub fn install_hooks(g: G) {
    for (name, hook, slot) in [
        (SET_DAMAGE, hk_set_damage as *const (), &ORIG_SET_DAMAGE),
        ("gml_Script_anon@1333@___struct___609@gml_Object_objEnemyMeleeBig_Create_0",
            chainsaw_step as *const (), &CHAINSAW_STEP),
        ("gml_Script_anon@1369@___struct___588@gml_Object_objEnemyDog_Create_0",
            dog_step as *const (), &DOG_STEP),
        ("gml_Script_anon@1212@___struct___574@gml_Object_objEnemyBoom_Create_0",
            boom_step as *const (), &BOOM_STEP),
        ("gml_Script_anon@1062@___struct___610@gml_Object_objEnemyMeleeRiot_Create_0",
            riot_step as *const (), &RIOT_STEP),
        ("instance_find", find_instance as *const (), &FIND_INSTANCE),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match hooks::install(g.rt, name, hook) {
                Ok(t) => slot.store(t as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("ai: {e}")),
            }
        }
    }
}

// These four native chase states bypass self.Player with objPlayer[0]. Scope
// the override to their calls only; wave spawning, menus and player code still
// need the real local player. Restore the scope even if native code unwinds.
struct ChaseScope(usize);
impl Drop for ChaseScope {
    fn drop(&mut self) { CHASE_SELF.store(self.0, Ordering::Relaxed); }
}
macro_rules! chase_hooks {
    ($($hook:ident => $original:ident),* $(,)?) => {
        $(unsafe extern "C-unwind" fn $hook(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
            let f: ScriptFn = unsafe { std::mem::transmute($original.load(Ordering::Relaxed)) };
            let active = super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0
                && !super::enemies::PUPPET_MODE.load(Ordering::Relaxed);
            let _scope = ChaseScope(CHASE_SELF.swap(if active { s as usize } else { 0 }, Ordering::Relaxed));
            unsafe { f(s, o, r, n, a) }
        })*
    };
}
chase_hooks! { chainsaw_step => CHAINSAW_STEP, dog_step => DOG_STEP, boom_step => BOOM_STEP, riot_step => RIOT_STEP }

unsafe extern "C-unwind" fn find_instance(r: *mut V, s: *mut c_void, o: *mut c_void, n: i32, a: *mut V) {
    let enemy = CHASE_SELF.load(Ordering::Relaxed);
    if enemy != 0 && s as usize == enemy && n == 2 && !a.is_null()
        && unsafe { (*a.add(1)).as_f64() == Some(0.0) } {
        if let Some(g) = G::new() {
            // These chase routines look up objPlayer[0] instead of using
            // self.Player. Make their sight/raycast/attack decisions use the same
            // target as the rest of the enemy FSM, including a remote player.
            if key(&unsafe { *a }) == key(&g.asset("objPlayer")) {
                if let Some(id) = unsafe { g.rt.builtin_var_of("id", enemy as *mut c_void) } {
                    let target = g.get(id, "Player");
                    if g.exists(target) && !truthy(&g.get(target, "dead")) {
                        unsafe { *r = target };
                        return;
                    }
                }
            }
        }
    }
    let f: crate::gm::BuiltinFn = unsafe { std::mem::transmute(FIND_INSTANCE.load(Ordering::Relaxed)) };
    unsafe { f(r, s, o, n, a) };
}

/// Peer responsible for a damage source instance (bullet, melee, explosion...).
fn attacker_peer_inst(g: G, id: V) -> Option<PeerId> {
    if let Some(p) = g.get(id, "focmp_peer").as_f64() {
        return Some(p as PeerId);
    }
    let me = super::MY_ID.load(Ordering::Relaxed);
    let player = g.first("objPlayer");
    if player.is_some_and(|p| key(&p) == key(&id)) || truthy(&g.get(id, "player_made")) {
        return Some(me);
    }
    let owner = g.get(id, "Owner");
    if g.exists(owner) && player.is_some_and(|p| key(&p) == key(&owner)) {
        return Some(me);
    }
    None
}

/// Which peer is responsible for damage applied by `other`, if we can tell.
fn attacker_peer(g: G, other: *mut c_void) -> Option<PeerId> {
    let id = unsafe { g.rt.builtin_var_of("id", other) }?;
    if !g.exists(id) {
        return None;
    }
    let tagged = g.get(id, "focmp_peer");
    if let Some(p) = tagged.as_f64() {
        return Some(p as PeerId);
    }
    let me = super::MY_ID.load(Ordering::Relaxed);
    let obj = unsafe { g.rt.builtin_var_of("object_index", other) }?;
    let player = g.asset("objPlayer");
    if key(&obj) == key(&player) {
        return Some(me);
    }
    if truthy(&g.get(id, "player_made")) {
        return Some(me);
    }
    let owner = g.get(id, "Owner");
    if g.exists(owner) && g.first("objPlayer").is_some_and(|p| key(&p) == key(&owner)) {
        return Some(me);
    }
    None
}

unsafe extern "C-unwind" fn hk_set_damage(
    self_: *mut c_void,
    other: *mut c_void,
    ret: *mut RValue,
    argc: i32,
    args: *mut *mut RValue,
) -> *mut RValue {
    let _random=super::shot_events::active().then(super::rng::Scope::preserve);
    let orig: ScriptFn = unsafe { std::mem::transmute(ORIG_SET_DAMAGE.load(Ordering::Relaxed)) };
    if super::visual_damage::suppress(other, argc, args) { unsafe { *ret = crate::engine::real(0.0) }; return ret; }
    // This branch must never fall through to native enemy gameplay, even if
    // the instance/arguments are not ready. Native hit callbacks cannot be
    // rolled back by restoring HP/dead after they have already executed.
    if super::enemies::PUPPET_MODE.load(Ordering::Relaxed) {
        let value = unsafe { super::client_hits::hit(self_, argc, args) };
        if !ret.is_null() { unsafe { *ret = value }; }
        return ret;
    }
    let r = {
        let _presentation = super::damage_numbers::host_damage();
        unsafe { orig(self_, other, ret, argc, args) }
    };
    if let Some(g) = G::new() {
        let enemy = unsafe { g.rt.builtin_var_of("id", self_) };
        // set_damage(dmg, x, y, knockback, type, _, _, source_instance, _): the
        // source is argument 7; `other` is the enemy itself (bound method).
        let src = if argc > 7 && !args.is_null() { unsafe { **args.add(7) } } else { RValue::undefined() };
        let peer = if g.exists(src) {
            attacker_peer_inst(g, src)
        } else {
            attacker_peer(g, other)
        };
        if TRACE_DAMAGE.load(Ordering::Relaxed) {
            let other_obj = unsafe { g.rt.builtin_var_of("object_index", other) }
                .map(|o| g.str_of(&g.call("object_get_name", &[o])))
                .unwrap_or_else(|| "null".into());
            let a0 = if argc > 0 && !args.is_null() { unsafe { g.str_of(&**args) } } else { "-".into() };
            crate::log(&format!(
                "set_damage: enemy {} other={other_obj} argc={argc} arg0={a0} -> peer {peer:?}",
                enemy.map(|e| g.str_of(&e)).unwrap_or_default()
            ));
        }
        if let (Some(e), Some(p)) = (enemy, peer) {
            AGGRO.lock().unwrap().insert(key(&e), (p, Instant::now()));
        }
    }
    r
}

