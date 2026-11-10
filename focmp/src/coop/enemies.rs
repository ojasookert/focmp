//! Enemies: host simulates, clients mirror. Synced through the coalesced
//! per-tick message (`net::Tick`, 60 Hz):
//!
//! - host → clients: `EnemyDelta` with quantized state of enemies that changed
//!   since the last tick (keyframe with everything once a second), plus the
//!   enemy guns that fired this tick
//! - client → host: individual hit requests with native damage/status arguments
//!
//! Host: tags enemies with `focmp_gid`; a detour on objGun.fire records
//! enemy shots; client hits are applied inside the enemy's own Step detour
//! through its real `set_damage`, so death, drops and rewards run normally.
//!
//! Clients (in a co-op run): no local enemy AI. Enemy Step/Alarm, wave-manager
//! and enemy-gun events are skipped by detours. Local enemies are matched to
//! host gids (same object, nearest position — rooms come from the same seed),
//! unmatched ones are removed, missing ones created, and all are moved to the
//! host state each frame. An enemy-gun fire from the host makes the puppet's
//! gun run its own fire method for muzzle/recoil presentation. Actual bullets
//! use the shared shot factory with the host's compiled definition and RNG state.

use crate::engine::{G, V, asset_id, boolean, key, real, truthy};
use crate::gm::RValue;
use crate::net::{EnemyDeath, EnemyFire, EnemyDelta, EnemyQ, dq_angle, q_angle};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

type EventFn = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);

/// True on clients in a co-op run: enemy AI is suppressed.
pub static PUPPET_MODE: AtomicBool = AtomicBool::new(false);
static NEXT_GID: AtomicU32 = AtomicU32::new(1);

static DEATH_ENTER: AtomicUsize = AtomicUsize::new(0);
static DEATHS: LazyLock<Mutex<Vec<EnemyDeath>>> = LazyLock::new(||Mutex::new(Vec::new()));
static PENDING_DEATH: LazyLock<Mutex<HashMap<u16,EnemyDeath>>> = LazyLock::new(||Mutex::new(HashMap::new()));
static DEAD_VISUALS: AtomicU64 = AtomicU64::new(0);
unsafe extern "C-unwind" fn death_enter(s:*mut c_void,o:*mut c_void,ret:*mut V,argc:i32,args:*mut *mut V)->*mut V {
    // A predicted hit cannot create a second corpse before the host confirms it.
    if PUPPET_MODE.load(Ordering::Relaxed) { unsafe{*ret=V::undefined()}; return ret; }
    if super::rooms::RUN_SEED.load(Ordering::Relaxed)!=0 {
        if let Some(g)=G::new() {
            if let Some(e)=unsafe{g.rt.builtin_var_of("id",s)} {
                let gid=g.num(e,"focmp_gid") as u16;
                if gid!=0 {
                    let v=g.get(e,"v2_vel");let k=g.get(e,"v2_knockback");
                    DEATHS.lock().unwrap().push(EnemyDeath{gid,x:g.num(e,"x") as f32,y:g.num(e,"y") as f32,angle:g.num(e,"image_angle") as f32,
                        vx:g.num(v,"x") as f32,vy:g.num(v,"y") as f32,kx:g.num(k,"x") as f32,ky:g.num(k,"y") as f32});
                }
            }
        }
    }
    let f:crate::hooks::ScriptFn=unsafe{std::mem::transmute(DEATH_ENTER.load(Ordering::Relaxed))};unsafe{f(s,o,ret,argc,args)}
}
fn play_death(s:*mut c_void) {
    let Some(g)=G::new() else{return};let Some(e)=(unsafe{g.rt.builtin_var_of("id",s)}) else{return};
    let gid=g.num(e,"focmp_gid") as u16;
    let Some(d)=PENDING_DEATH.lock().unwrap().remove(&gid) else{return};
    for (k,v) in [("x",d.x),("y",d.y),("image_angle",d.angle)] {g.set(e,k,real(v as f64));}
    for (name,x,y) in [("v2_vel",d.vx,d.vy),("v2_knockback",d.kx,d.ky)] {
        let v=g.get(e,name);g.set(v,"x",real(x as f64));g.set(v,"y",real(y as f64));
    }
    // Gameplay attachments are resolved by the host, while debris and blood run locally.
    g.set(e,"Boompack",real(-4.0));g.set(e,"array_sticky",g.call("array_create",&[real(0.0)]));
    let f:crate::hooks::ScriptFn=unsafe{std::mem::transmute(DEATH_ENTER.load(Ordering::Relaxed))};let mut ret=V::undefined();
    unsafe{f(s,s,&mut ret,0,std::ptr::null_mut())};
    g.set(e,"focmp_death_played",boolean(true));DEAD_VISUALS.fetch_add(1,Ordering::Relaxed);
}

/// Original client hits, kept separately so damage types and impulses survive.
pub static HITS: LazyLock<Mutex<Vec<crate::net::EnemyHit>>> = LazyLock::new(|| Mutex::new(Vec::new()));
pub static FIRES: LazyLock<Mutex<Vec<EnemyFire>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static PENDING_FIRE: LazyLock<Mutex<HashMap<u16, Vec<EnemyFire>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static FIRED: AtomicU64 = AtomicU64::new(0);
static REPLAYED: AtomicU64 = AtomicU64::new(0);
pub static PENDING_DMG: LazyLock<Mutex<HashMap<u16, Vec<(u8, crate::net::EnemyHit)>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Events skipped on clients. (name, original trampoline)
static SKIP: LazyLock<Vec<(&'static str, AtomicUsize)>> = LazyLock::new(|| {
    [
        "gml_Object_objEnemyMASTER_Step_0",
        "gml_Object_objEnemyMASTER_Alarm_7",
        "gml_Object_objEnemyMASTER_Alarm_8",
        "gml_Object_objEnemyMASTER_Alarm_9",
        "gml_Object_objEnemyMASTER_Alarm_10",
        "gml_Object_objEnemyMASTER_Alarm_11",
        "gml_Object_objEnemyBoss_Step_0",
        "gml_Object_objEnemyStatue_Step_0",
        "gml_Object_objEnemyTutorial_Step_0",
        "gml_Object_objEnemyBoomBackpack_Step_0",
        "gml_Object_objEnemyFodder_Alarm_0",
        "gml_Object_objEnemyFodder_Alarm_1",
        "gml_Object_objEnemyMeleeRiot_Alarm_0",
        "gml_Object_objWaveManager_Step_0",
        "gml_Object_objWaveManager_Alarm_0",
        "gml_Object_objWaveManager_Alarm_1",
    ]
    .into_iter()
    .map(|n| (n, AtomicUsize::new(0)))
    .collect()
});
/// Gun events: skipped on clients when the gun belongs to an enemy.
static GUN_SKIP: LazyLock<Vec<(&'static str, AtomicUsize)>> = LazyLock::new(|| {
    ["gml_Object_objGun_Step_0", "gml_Object_objGun_Step_1", "gml_Object_objGun_Alarm_0", "gml_Object_objGun_Alarm_1", "gml_Object_objGun_Alarm_2"]
        .into_iter()
        .map(|n| (n, AtomicUsize::new(0)))
        .collect()
});
const GUN_FIRE: &str = "gml_Script_anon@1376@gml_Object_objGun_Other_12";
static ORIG_GUN_FIRE: AtomicUsize = AtomicUsize::new(0);

// One detour per hooked event (each needs its own trampoline slot).
macro_rules! skip_hooks {
    ($table:ident, $check:ident, $list:ident; $($i:literal => $f:ident),*) => {
        $(unsafe extern "C-unwind" fn $f(s: *mut c_void, o: *mut c_void) {
            if !$check(s, $i) {
                let t: EventFn = unsafe { std::mem::transmute($table[$i].1.load(Ordering::Relaxed)) };
                unsafe { t(s, o) }
            }
        })*
        fn $list() -> Vec<*const ()> { vec![$($f as *const ()),*] }
    };
}

fn skip_enemy(s: *mut c_void, i: usize) -> bool {
    if super::fuzz::active() && i >= 12 { return true; } // freeze QA wave spawning
    if PUPPET_MODE.load(Ordering::Relaxed) {
        if matches!(i, 0 | 6 | 7 | 8 | 9) {
            if let Some(g) = G::new() {
                if let Some(e) = unsafe { g.rt.builtin_var_of("id", s) } { super::enemy_melee::replay(g, e); }
            }
            super::status_effects::visual_step(s); play_death(s);
        }
        return true;
    }
    if matches!(i, 0 | 6 | 7 | 8 | 9) {
        apply_remote_hits(s); // Every overriding enemy Step, including hub dummies
    }
    false
}

/// Client gun events: skip for enemy-owned guns; fire them on host request.
fn skip_gun(s: *mut c_void, i: usize) -> bool {
    if !PUPPET_MODE.load(Ordering::Relaxed) {
        return false;
    }
    let Some(g) = G::new() else { return false };
    let Some(gun) = (unsafe { g.rt.builtin_var_of("id", s) }) else { return false };
    let owner = g.get(gun, "Owner");
    if !g.exists(owner) || g.first("objPlayer").is_some_and(|p| key(&p) == key(&owner)) {
        return false; // the local player's gun
    }
    // Begin Step resets native gun-tip caches. Skipping it pinned subsequent
    // flashes and bullets to the very first firing position.
    if i == 1 { return false; }
    if i == 0 {
        let gid = g.num(owner, "focmp_gid") as u16;
        let pending = PENDING_FIRE.lock().unwrap().remove(&gid).unwrap_or_default();
        let address = ORIG_GUN_FIRE.load(Ordering::Relaxed);
        if address != 0 {
            let fire: crate::hooks::ScriptFn = unsafe { std::mem::transmute(address) };
            for shot in pending {
                let shots = g.get(gun, "shots");
                if shot.shot as usize >= g.arr_len(shots) { continue; }
                for (name, value) in [("x", shot.x), ("y", shot.y), ("image_angle", shot.angle),
                    ("current_spread", shot.spread), ("shot_i", shot.shot as f32), ("mag_bullets", shot.magazine), ("gun_wait", 0.0)] {
                    g.set(gun, name, real(value as f64));
                }
                g.set(gun, "mag_reloading", boolean(false));
                g.set(gun,"xofflen",real(shot.recoil_x as f64));g.set(gun,"yofflen",real(shot.recoil_y as f64));
                let tip=g.get(gun,"gun_tip");g.set(tip,"x",real(shot.tip_x as f64));g.set(tip,"y",real(shot.tip_y as f64));
                g.set(gun,"gun_tip_setted",boolean(true));
                // Restore the host's state for each invocation, including multiple
                // shots from one gun in a network tick; never coalesce into a set.
                let _random=super::rng::Scope::replay(shot.rng);
                let _group=super::shot_events::group(0,shot.id,true,true);
                let _remote=super::projectiles::remote_scope();
                let _presentation=super::projectiles::presentation_only();
                let mut ret = V::undefined();
                unsafe { fire(s, s, &mut ret, 0, std::ptr::null_mut()) };

                REPLAYED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    true
}

skip_hooks!(SKIP, skip_enemy, skip_fns; 0 => sk0, 1 => sk1, 2 => sk2, 3 => sk3, 4 => sk4, 5 => sk5, 6 => sk6, 7 => sk7, 8 => sk8, 9 => sk9, 10 => sk10, 11 => sk11, 12 => sk12, 13 => sk13, 14 => sk14, 15 => sk15);
skip_hooks!(GUN_SKIP, skip_gun, gun_skip_fns; 0 => gk0, 1 => gk1, 2 => gk2, 3 => gk3, 4 => gk4);

/// Host: an enemy gun fired → include it in the next tick.
unsafe extern "C-unwind" fn hk_gun_fire(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    let f: crate::hooks::ScriptFn = unsafe { std::mem::transmute(ORIG_GUN_FIRE.load(Ordering::Relaxed)) };
    let shot = if !PUPPET_MODE.load(Ordering::Relaxed) && super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0 {
        G::new().and_then(|g| {
            let gun = unsafe { g.rt.builtin_var_of("id", s) }?;
            let owner = g.get(gun, "Owner");
            if !g.exists(owner) { return None; }
            let gid = g.num(owner, "focmp_gid") as u16;
            if gid == 0 { return None; }
            let (level, sub) = super::rooms::level_sub(g);
            Some(EnemyFire {
                tip_x:0.0,tip_y:0.0,recoil_x:g.num(gun,"xofflen") as f32,recoil_y:g.num(gun,"yofflen") as f32,
                room: g.room_name(), gid, level: level as u16, sub: sub as u16,
                x: g.num(gun, "x") as f32, y: g.num(gun, "y") as f32,
                angle: g.num(gun, "image_angle") as f32, spread: g.num(gun, "current_spread") as f32,
                shot: g.num(gun, "shot_i") as u16, magazine: g.num(gun, "mag_bullets") as f32,
                id:super::shot_events::next_id(),rng:super::rng::capture()?,
            })
        })
    } else { None };
    let _group=shot.as_ref().map(|shot|super::shot_events::group(0,shot.id,false,true));
    let result = unsafe { f(s, o, ret, argc, args) };
    if let Some(mut shot) = shot {
        // AI calls fire every frame while aiming. The method itself enforces
        // cooldown/reload; replicate only calls that actually consumed ammo.
        if let Some(g) = G::new() {
            if let Some(gun) = unsafe { g.rt.builtin_var_of("id", s) } {
                if (g.num(gun, "mag_bullets") as f32) < shot.magazine {
                    let tip=g.get(gun,"gun_tip");shot.tip_x=g.num(tip,"x") as f32;shot.tip_y=g.num(tip,"y") as f32;
                    FIRES.lock().unwrap().push(shot);
                    FIRED.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    result
}

/// Host, inside an enemy's Step (valid GML context): apply queued client hits
/// through the enemy's own set_damage (signature traced live:
/// `set_damage(dmg, x, y, knockback{x,y}, type, 1, true, source_instance, _)`).
fn apply_remote_hits(s: *mut c_void) {
    if PENDING_DMG.lock().unwrap().is_empty() {
        return;
    }
    let Some(g) = G::new() else { return };
    let Some(e) = (unsafe { g.rt.builtin_var_of("id", s) }) else { return };
    let gid = g.num(e, "focmp_gid") as u16;
    let Some(hits) = PENDING_DMG.lock().unwrap().remove(&gid) else { return };
    let f = super::ai::ORIG_SET_DAMAGE.load(Ordering::Relaxed);
    if f == 0 {
        return;
    }
    let set_damage: crate::hooks::ScriptFn = unsafe { std::mem::transmute(f) };
    for (peer, hit) in hits {
        if hit.room!=g.room_name() || (hit.level,hit.sub)!=super::rooms::level_sub(g) {continue;}
        // The knockback argument must be a real `Vector` (constructor struct with
        // methods: damage numbers call .Copy on it). Clone the enemy's own.
        let kv = g.get(e, "v2_knockback");
        if !crate::engine::is_struct(&kv) {
            break;
        }
        let knock = g.call("variable_clone", &[kv]);
        g.set(knock,"x",real(hit.kx));g.set(knock,"y",real(hit.ky));
        if truthy(&g.get(e, "dead")) {
            break;
        }
        let mut argv = [
            real(hit.amount),
            real(hit.x),
            real(hit.y),
            knock,
            real(hit.dtype as f64),
            real(hit.scale),
            boolean(hit.flag),
            // The originating projectile exists only on the reporting peer.
            // A player stand-in is not a projectile: shield handling would call
            // its nonexistent reflect_from_shield method. The shooter handles
            // reflection locally; host checks the shield using this knockback.
            real(-4.0),
            hit.extra.map(real).unwrap_or_else(V::undefined),
        ];
        let mut ptrs: Vec<*mut RValue> = argv.iter_mut().map(|a| a as *mut _).collect();
        let mut ret = RValue::undefined();
        {
            let _presentation = super::damage_numbers::host_damage();
            unsafe { set_damage(s, s, &mut ret, ptrs.len() as i32, ptrs.as_mut_ptr()) };
        }
        super::ai::note_aggro(e, peer);
    }
}

pub fn install_hooks(g: G) {
    if DEATH_ENTER.load(Ordering::Relaxed)==0 {
        match crate::hooks::install(g.rt,"gml_Script_anon@22921@___struct___601@gml_Object_objEnemyMASTER_Create_0",death_enter as *const ()) {
            Ok(f)=>DEATH_ENTER.store(f as usize,Ordering::SeqCst),Err(e)=>crate::log(&format!("death visuals: {e}")),
        }
    }
    let groups: [(&Vec<(&'static str, AtomicUsize)>, Vec<*const ()>); 2] = [(&SKIP, skip_fns()), (&GUN_SKIP, gun_skip_fns())];
    for (table, fns) in groups {
        for ((name, slot), f) in table.iter().zip(fns) {
            if slot.load(Ordering::Relaxed) == 0 {
                match crate::hooks::install(g.rt, name, f) {
                    Ok(t) => slot.store(t as usize, Ordering::SeqCst),
                    Err(e) => crate::log(&format!("enemies: {e}")),
                }
            }
        }
    }
    if ORIG_GUN_FIRE.load(Ordering::Relaxed) == 0 {
        match crate::hooks::install(g.rt, GUN_FIRE, hk_gun_fire as *const ()) {
            Ok(t) => ORIG_GUN_FIRE.store(t as usize, Ordering::SeqCst),
            Err(e) => crate::log(&format!("enemies: {e}")),
        }
    }
}

const KEYFRAME_EVERY: u32 = 60; // ticks (1 s at 60 Hz)

#[derive(Default)]
pub struct Enemies {
    tick: u32,
    /// Host: what clients last received, per gid.
    sent: HashMap<u16, EnemyQ>,
    /// Client: merged host state for the current room.
    table: HashMap<u16, EnemyQ>,
    have_keyframe: bool,
    /// Client: gid → local puppet instance.
    puppets: HashMap<u16, V>,
    pub ticks_sent: u64,
    pub bytes_hint: u64,
    pub deltas_applied: u64,
}

fn alive_enemies(g: G) -> Vec<V> {
    let dummy = g.asset("objEnemyVisualDummy");
    g.instances("objEnemyMASTER")
        .into_iter()
        .filter(|e| key(&g.get(*e, "object_index")) != key(&dummy))
        .collect()
}

/// Remove an enemy and the gun it holds (an orphaned objGun's Draw event
/// looks up its Owner and raises a GML error).
fn destroy_enemy(g: G, v: V, run_destroy_event: bool) {
    let gun = g.get(v, "Gun");
    if g.exists(gun) {
        g.call("instance_destroy", &[gun, boolean(false)]);
    }
    if g.exists(v) {
        g.call("instance_destroy", &[v, boolean(run_destroy_event)]);
    }
}

fn clamp16(v: f64) -> i16 {
    v.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16
}

impl Enemies {
    pub fn reset(&mut self) {
        self.tick = 0;
        DEATHS.lock().unwrap().clear();PENDING_DEATH.lock().unwrap().clear();
        self.sent.clear();
        self.table.clear();
        self.puppets.clear();
        self.have_keyframe = false;
        PENDING_FIRE.lock().unwrap().clear();
        HITS.lock().unwrap().clear();
        super::client_hits::reset();
        super::enemy_melee::reset();
        super::damage_numbers::reset();
        super::map_lights::reset();
        FIRES.lock().unwrap().clear();
        PENDING_DMG.lock().unwrap().clear();
    }

    /// Host: enemy changes since the last tick (every tick; keyframe each second).
    pub fn host_delta(&mut self, g: G) -> EnemyDelta {
        self.tick = self.tick.wrapping_add(1);
        let keyframe = self.tick % KEYFRAME_EVERY == 1;
        let mut now: HashMap<u16, EnemyQ> = HashMap::new();
        for e in alive_enemies(g) {
            if !truthy(&g.get(e, "focmp_health_scaled")) {
                let scale = crate::config::RUN_RULES.lock().unwrap().health_scale();
                for field in ["hp", "hp_start", "local_enemy_health_factor"] {
                    g.set(e, field, real(g.num(e, field) * scale));
                }
                g.set(e, "focmp_health_scaled", boolean(true));
            }
            let mut gid = g.num(e, "focmp_gid") as u32;
            if gid == 0 {
                gid = NEXT_GID.fetch_add(1, Ordering::Relaxed) & 0xffff;
                g.set(e, "focmp_gid", real(gid as f64));
            }
            let gun = g.get(e, "Gun");
            let q = EnemyQ {
                gid: gid as u16,
                obj: asset_id(g.get(e, "object_index")) as i16,
                x: clamp16(g.num(e, "x")),
                y: clamp16(g.num(e, "y")),
                angle: q_angle(g.num(e, "image_angle")),
                sprite: asset_id(g.get(e, "sprite_index")) as i16,
                frame: g.num(e, "image_index") as u8,
                flip: g.num(e, "image_xscale") < 0.0,
                hp: g.num(e, "hp") as f32,
                hp_start: g.num(e, "hp_start") as f32,
                dead: truthy(&g.get(e, "dead")),
                onfire: truthy(&g.get(e,"onfire")),
                status:super::status_effects::capture(g,e),
                gun_angle: if g.exists(gun) { q_angle(g.num(gun, "image_angle")) } else { 0 },
                gun_x: if g.exists(gun) { clamp16(g.num(gun, "x")) } else { 0 },
                gun_y: if g.exists(gun) { clamp16(g.num(gun, "y")) } else { 0 },
                gun_frame: if g.exists(gun) { g.num(gun, "image_index") as u8 } else { 0 },
                gun_recoil_x: if g.exists(gun){clamp16(g.num(gun,"xofflen")*256.0)}else{0},
                gun_recoil_y: if g.exists(gun){clamp16(g.num(gun,"yofflen")*256.0)}else{0},
                gun_nozzle: g.exists(gun)&&truthy(&g.get(gun,"draw_nozzle")),
                gun_nozzle_frame: if g.exists(gun){(g.num(gun,"draw_nozzle_i")*256.0).round().clamp(0.0,65535.0) as u16}else{0},
                gun_sprite: if g.exists(gun){asset_id(g.get(gun,"sprite_index")) as i16}else{-1},
                gun_len: if g.exists(gun){(g.num(gun,"gun_len")*256.0).round().clamp(0.0,65535.0) as u16}else{0},
                gun_flip: g.exists(gun) && g.num(gun, "image_yscale") < 0.0,
            };
            now.insert(q.gid, q);
        }
        let changed: Vec<EnemyQ> = now.values().filter(|q| keyframe || self.sent.get(&q.gid) != Some(q)).cloned().collect();
        let gone: Vec<u16> = self.sent.keys().filter(|k| !now.contains_key(k)).copied().collect();
        self.sent = now;
        let (level, sub) = super::rooms::level_sub(g);
        self.ticks_sent += 1;
        self.bytes_hint += (changed.len() * 18 + gone.len() * 2 + 8) as u64;
        EnemyDelta { deaths: std::mem::take(&mut *DEATHS.lock().unwrap()), room: g.room_name(), complete: g.first("objPlayer").is_some_and(|p| truthy(&g.get(p, "room_complete"))), level: level as u16, sub: sub as u16, keyframe, changed, gone }
    }

    /// Client: merge a host delta.
    pub fn on_delta(&mut self, g: G, d: EnemyDelta) {
        let (level, sub) = super::rooms::level_sub(g);
        if (d.level as u32, d.sub as u32) != (level, sub) || d.room != g.room_name() {
            return;
        }
        for death in d.deaths { PENDING_DEATH.lock().unwrap().insert(death.gid,death); }
        if d.complete {
            if let Some(p) = g.first("objPlayer") { g.set(p, "room_complete", boolean(true)); }
        }
        if d.keyframe {
            let keep: HashSet<u16> = d.changed.iter().map(|q| q.gid).collect();
            self.table.retain(|k, _| keep.contains(k));
            self.have_keyframe = true;
        }
        for gid in d.gone {
            self.table.remove(&gid);
        }
        for q in d.changed {
            self.table.insert(q.gid, q);
        }
        self.deltas_applied += 1;
    }

    pub fn on_fires(&mut self, g: G, fires: Vec<EnemyFire>) {
        let room = g.room_name();
        let (level, sub) = super::rooms::level_sub(g);
        let mut pending = PENDING_FIRE.lock().unwrap();
        for shot in fires {
            if shot.room == room && (shot.level as u32, shot.sub as u32) == (level, sub) {
                let queue = pending.entry(shot.gid).or_default();
                if queue.len() < 128 { queue.push(shot); }
            }
        }
    }

    /// Client, every frame: make local enemies mirror the host table.
    pub fn client_apply(&mut self, g: G) {
        if !self.have_keyframe {
            return;
        }
        let locals = alive_enemies(g);
        let mut claimed: HashSet<u64> = self.puppets.values().filter(|v| g.exists(**v)).map(key).collect();
        for q in self.table.values() {
            let known = self.puppets.get(&q.gid).copied().filter(|v| g.exists(*v));
            let inst = match known {
                Some(v) => v,
                None => {
                    let (qx, qy) = (q.x as f64, q.y as f64);
                    let best = locals
                        .iter()
                        .filter(|l| !claimed.contains(&key(l)) && asset_id(g.get(**l, "object_index")) as i16 == q.obj)
                        .map(|l| (*l, (g.num(*l, "x") - qx).hypot(g.num(*l, "y") - qy)))
                        .filter(|(_, d)| *d < 96.0)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(l, _)| l);
                    let v = best.unwrap_or_else(|| g.call("instance_create_depth", &[real(qx), real(qy), real(400.0), real(q.obj as f64)]));
                    if !g.exists(v) {
                        continue;
                    }
                    g.set(v, "focmp_gid", real(q.gid as f64));
                    claimed.insert(key(&v));
                    v
                }
            };
            self.puppets.insert(q.gid, inst);
            g.set(inst, "x", real(q.x as f64));
            g.set(inst, "y", real(q.y as f64));
            g.set(inst, "image_angle", real(dq_angle(q.angle)));
            if q.sprite >= 0 && truthy(&g.call("sprite_exists", &[real(q.sprite as f64)])) {
                g.set(inst, "sprite_index", real(q.sprite as f64));
            }
            g.set(inst, "image_index", real(q.frame as f64));
            let xs = g.num(inst, "image_xscale").abs().max(0.01);
            g.set(inst, "image_xscale", real(if q.flip { -xs } else { xs }));
            g.set(inst, "hp", real(q.hp as f64));
            g.set(inst, "hp_start", real(q.hp_start as f64));
            g.set(inst,"focmp_host_onfire",boolean(q.onfire));
            super::status_effects::apply(g,inst,&q.status);
            super::client_hits::draw_feedback(g, inst);
            update_lights(g, inst, &["body_light", "eyes_light"]);
            let gun = g.get(inst, "Gun");
            if g.exists(gun) {
                g.set(gun, "x", real(q.gun_x as f64));
                g.set(gun, "y", real(q.gun_y as f64));
                g.set(gun, "image_index", real(q.gun_frame as f64));
                g.set(gun, "image_yscale", real(if q.gun_flip { -1.0 } else { 1.0 }));
                g.set(gun, "image_angle", real(dq_angle(q.gun_angle)));
                g.set(gun,"xofflen",real(q.gun_recoil_x as f64/256.0));g.set(gun,"yofflen",real(q.gun_recoil_y as f64/256.0));
                g.set(gun,"draw_nozzle",boolean(q.gun_nozzle));g.set(gun,"draw_nozzle_i",real(q.gun_nozzle_frame as f64/256.0));
                g.set(gun,"gun_len",real(q.gun_len as f64/256.0));g.set(gun,"gun_tip_setted",boolean(false));
                if q.gun_sprite>=0{g.set(gun,"sprite_index",real(q.gun_sprite as f64));}
                update_lights(g, gun, &["body_light"]);
            }
        }
        // Puppets the host no longer has → died there: remove.
        let gone: Vec<u16> = self.puppets.keys().filter(|k| !self.table.contains_key(k)).copied().collect();
        for gid in gone {
            if PENDING_DEATH.lock().unwrap().contains_key(&gid) { continue; }
            if let Some(v) = self.puppets.remove(&gid) {
                if g.exists(v) {
                    destroy_enemy(g, v, false); // corpse/splatter already emitted by the native visual callback
                }
            }
        }
        // Local enemies the host doesn't have: remove silently (no Destroy event).
        for l in locals {
            if !claimed.contains(&key(&l)) && g.exists(l) && g.num(l, "focmp_gid") == 0.0 {
                destroy_enemy(g, l, false);
            }
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "enemies: puppet mode {}, {} puppets, table {}, ticks sent {} (~{} KB), deltas applied {}, shots captured {}, replayed {}, local deaths {}",
            PUPPET_MODE.load(Ordering::Relaxed),
            self.puppets.len(),
            self.table.len(),
            self.ticks_sent,
            self.bytes_hint / 1024,
            self.deltas_applied,
            FIRED.load(Ordering::Relaxed),
            REPLAYED.load(Ordering::Relaxed),
            DEAD_VISUALS.load(Ordering::Relaxed)
        )
    }
}

/// Suppressed native Steps normally move these render objects along with the
/// enemy/gun. Keep their transforms current when applying authoritative state.
fn update_lights(g: G, inst: V, names: &[&str]) {
    for name in names {
        let light = g.get(inst, name);
        if !crate::engine::is_struct(&light) { continue; }
        if *name == "body_light" { g.set(light, "sprite", g.get(inst, "sprite_index")); }
        for (dst, src) in [("x", "x"), ("y", "y"), ("angle", "image_angle"), ("image", "image_index"),
            ("xscale", "image_xscale"), ("yscale", "image_yscale")] { g.set(light, dst, g.get(inst, src)); }
    }
}
