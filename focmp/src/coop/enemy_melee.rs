//! Host starts enemy melee attacks; each machine collides with its own player.
use crate::engine::{G, V, asset_id, boolean, key, real, truthy};
use crate::hooks::ScriptFn;
use bitcode::{Decode, Encode};
use std::{collections::HashMap, ffi::c_void, sync::{LazyLock, Mutex, atomic::{AtomicUsize, Ordering}}};

static ATTACK: AtomicUsize = AtomicUsize::new(0);
static END: AtomicUsize = AtomicUsize::new(0);
pub static OUTGOING: Mutex<Vec<Attack>> = Mutex::new(Vec::new());
static PENDING: LazyLock<Mutex<HashMap<u16, Vec<Attack>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug, Encode, Decode)]
pub struct Attack {
    room: String,
    level: u32,
    sub: u32,
    gid: u16,
    sprite: i32,
    mask: i32,
    x: f64,
    y: f64,
    angle: f64,
    speed: f64,
    damage: f64,
    dash: bool,
}
impl Attack {
    pub fn sane(&self) -> bool {
        self.gid != 0 && self.room.starts_with("Room_") && self.room.len() < 128
            && self.sprite >= 0 && self.mask >= 0 && self.damage >= 0.0
            && self.speed > 0.0 && self.speed <= 100.0
            && [self.x, self.y, self.angle, self.damage].iter().all(|v| v.is_finite() && v.abs() < 1.0e7)
    }
}
pub fn reset() {
    OUTGOING.lock().unwrap().clear();
    PENDING.lock().unwrap().clear();
}
pub fn install(g: G) {
    for (name, hook, slot) in [
        ("gml_Script_anon@50@gml_Object_objEnemyMASTER_Other_13", attack as *const (), &ATTACK),
        ("gml_Object_objMeleeEnemy_Other_7", end as *const (), &END),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(f) => slot.store(f as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("enemy melee: {e}")),
            }
        }
    }
}
unsafe extern "C-unwind" fn attack(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(ATTACK.load(Ordering::Relaxed)) };
    let result = unsafe { f(s, o, r, n, a) };
    if super::rooms::RUN_SEED.load(Ordering::Relaxed) == 0 || super::enemies::PUPPET_MODE.load(Ordering::Relaxed) {
        return result;
    }
    if let Some(g) = G::new() {
        if let Some(enemy) = unsafe { g.rt.builtin_var_of("id", s) } {
            let hit = g.get(enemy, "id_melee_attack");
            if g.exists(hit) {
                // Native melee targets self.Player directly. Remote anchors have
                // no native set_damage: collision must use this machine's player.
                if let Some(player) = g.first("objPlayer") { g.set(hit, "target", player); }
                if !truthy(&g.get(hit, "focmp_melee_sent")) {
                    let (level, sub) = super::rooms::level_sub(g);
                    let event = Attack { room: g.room_name(), level, sub,
                        gid: g.num(enemy, "focmp_gid") as u16,
                        sprite: asset_id(g.get(hit, "sprite_index")), mask: asset_id(g.get(hit, "mask_index")),
                        x: g.num(hit, "x"), y: g.num(hit, "y"), angle: g.num(hit, "image_angle"),
                        speed: g.num(hit, "image_speed"), damage: g.num(hit, "dmg"), dash: truthy(&g.get(hit, "dash")),
                    };
                    if event.sane() {
                        OUTGOING.lock().unwrap().push(event);
                        g.set(hit, "focmp_melee_sent", boolean(true));
                    }
                }
            }
        }
    }
    result
}
pub fn receive(g: G, attacks: Vec<Attack>) {
    let room = g.room_name();
    let level = super::rooms::level_sub(g);
    let mut pending = PENDING.lock().unwrap();
    for attack in attacks {
        if attack.room == room && (attack.level, attack.sub) == level {
            let queue = pending.entry(attack.gid).or_default();
            if queue.len() < 32 { queue.push(attack); }
        }
    }
}
pub fn replay(g: G, enemy: V) {
    let gid = g.num(enemy, "focmp_gid") as u16;
    let attacks = PENDING.lock().unwrap().remove(&gid).unwrap_or_default();
    let Some(player) = g.first("objPlayer") else { return };
    if truthy(&g.get(player, "dead")) || truthy(&g.get(enemy, "dead")) { return; }
    for attack in attacks {
        if !truthy(&g.call("sprite_exists", &[real(attack.sprite as f64)]))
            || !truthy(&g.call("sprite_exists", &[real(attack.mask as f64)])) { continue; }
        let vars = g.call("json_parse", &[g.s("{}")]);
        for (name, value) in [
            ("Owner", enemy), ("target", player), ("sprite_index", real(attack.sprite as f64)),
            ("mask_index", real(attack.mask as f64)), ("image_angle", real(attack.angle)),
            ("image_speed", real(attack.speed)), ("dmg", real(attack.damage)),
            ("dash", boolean(attack.dash)), ("focmp_melee_replay", boolean(true)),
        ] { g.set(vars, name, value); }
        let hit = g.call("instance_create_depth", &[
            real(attack.x), real(attack.y), g.get(enemy, "depth"), g.asset("objMeleeEnemy"), vars,
        ]);
        if g.exists(hit) { g.set(enemy, "id_melee_attack", hit); }
    }
}
unsafe extern "C-unwind" fn end(s: *mut c_void, o: *mut c_void) {
    if let Some(g) = G::new() {
        if let Some(hit) = unsafe { g.rt.builtin_var_of("id", s) } {
            if truthy(&g.get(hit, "focmp_melee_replay")) {
                // Native animation-end reverses the owner's lunge. The host
                // owns that movement; the client only owns its local collision.
                let owner = g.get(hit, "Owner");
                if g.exists(owner) && key(&g.get(owner, "id_melee_attack")) == key(&hit) {
                    g.set(owner, "id_melee_attack", real(-4.0));
                }
                g.call("instance_destroy", &[hit]);
                return;
            }
        }
    }
    let f: unsafe extern "C-unwind" fn(*mut c_void, *mut c_void) = unsafe { std::mem::transmute(END.load(Ordering::Relaxed)) };
    unsafe { f(s, o) };
}
