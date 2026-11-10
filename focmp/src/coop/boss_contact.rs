//! Boss contact is detected by the host; only the real target runs native damage.
use crate::{
    engine::{G, V, key, real, truthy},
    net::{Msg, PeerId},
};
use bitcode::{Decode, Encode};
use std::{
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

static COLLISION: AtomicUsize = AtomicUsize::new(0);
static SOUND: AtomicUsize = AtomicUsize::new(0);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static LAST: AtomicU64 = AtomicU64::new(0);
static OUT: Mutex<Vec<Hit>> = Mutex::new(Vec::new());
static PENDING: Mutex<Vec<Hit>> = Mutex::new(Vec::new());

#[derive(Clone, Debug, Encode, Decode)]
pub struct Hit {
    run: u64,
    sequence: u64,
    peer: PeerId,
    room: String,
    level: u32,
    sub: u32,
    amount: f64,
    x: f64,
    y: f64,
    kx: f64,
    ky: f64,
    kind: f64,
}
impl Hit {
    pub fn sane(&self) -> bool {
        self.run != 0
            && self.sequence != 0
            && self.peer < 3
            && self.room.starts_with("Room_fight")
            && self.room.len() <= 64
            && [self.amount, self.x, self.y, self.kx, self.ky, self.kind]
                .iter()
                .all(|x| x.is_finite() && x.abs() < 1e7)
            && self.amount >= 0.0
            && (0.0..16.0).contains(&self.kind)
    }
}
pub fn reset() {
    OUT.lock().unwrap().clear();
    PENDING.lock().unwrap().clear();
    LAST.store(0, Ordering::Relaxed);
}
pub fn install(g: G) {
    if let Some((_, address)) =
        g.rt.scripts
            .iter()
            .find(|(n, _)| *n == "gml_Script_audio_play_sound_stop_arr")
    {
        SOUND.store(*address, Ordering::Relaxed);
    }
    if COLLISION.load(Ordering::Relaxed) == 0 {
        match crate::hooks::install(g.rt, "collision_circle", collision as *const ()) {
            Ok(f) => COLLISION.store(f as usize, Ordering::SeqCst),
            Err(e) => crate::log(&format!("boss contact: {e}")),
        }
    }
}
unsafe extern "C-unwind" fn collision(
    r: *mut V,
    s: *mut c_void,
    o: *mut c_void,
    n: i32,
    a: *mut V,
) {
    let _pump = crate::block_pump();
    let original: crate::gm::BuiltinFn =
        unsafe { std::mem::transmute(COLLISION.load(Ordering::Relaxed)) };
    unsafe { original(r, s, o, n, a) };
    let run = super::rooms::RUN_SEED.load(Ordering::Relaxed);
    if run == 0 || super::MY_ID.load(Ordering::Relaxed) != 0 || n != 6 || a.is_null() {
        return;
    }
    let Some(g) = G::new() else { return };
    let Some(enemy) = (unsafe { g.rt.builtin_var_of("id", s) }) else {
        return;
    };
    if !g.exists(enemy) || !g.exists(unsafe { *r }) {
        return;
    }
    let obj = g.get(enemy, "object_index");
    let boss = g.asset("objEnemyBoss");
    if key(&obj) != key(&boss) && !truthy(&g.call("object_is_ancestor", &[obj, boss])) {
        return;
    }
    let target = g.get(enemy, "Player");
    if key(&unsafe { *a.add(3) }) != key(&target) {
        return;
    }
    // Stop the native call to target.set_damage: a remote target is a visual
    // anchor, never a player simulation. Both host and client use this route.
    unsafe { *r = real(-4.0) };
    if !g.exists(target) || truthy(&g.get(target, "dead")) {
        return;
    }
    let peer = if g
        .first("objPlayer")
        .is_some_and(|p| key(&p) == key(&target))
    {
        0
    } else if let Some(peer) = g.get(target, "focmp_ghost").as_f64() {
        peer as PeerId
    } else {
        return;
    };
    let (level, sub) = super::rooms::level_sub(g);
    let (x, y) = (g.num(enemy, "x"), g.num(enemy, "y"));
    let (dx, dy) = (g.num(target, "x") - x, g.num(target, "y") - y);
    let length = dx.hypot(dy);
    // Native Boss.Step uses level.level damage, a SetMag(0.5) vector and
    // touch_damage as its damage kind. Preserve that signature exactly.
    let hit = Hit {
        run,
        sequence: SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1,
        peer,
        room: g.room_name(),
        level,
        sub,
        amount: level as f64,
        x,
        y,
        kx: if length > 0.0 { dx / length * 0.5 } else { 0.0 },
        ky: if length > 0.0 { dy / length * 0.5 } else { 0.0 },
        kind: g.num(enemy, "touch_damage"),
    };
    if hit.sane() {
        OUT.lock().unwrap().push(hit);
        // Preserve Boss.Step's optional contact sound, which follows the
        // damage call in the native branch replaced above.
        let sounds = g.get(enemy, "touch_sound_arr");
        let sound = SOUND.load(Ordering::Relaxed);
        if sound != 0 && sounds.kind() == crate::gm::KIND_ARRAY && g.arr_len(sounds) > 0 {
            let f: crate::hooks::ScriptFn = unsafe { std::mem::transmute(sound) };
            let mut values = [
                sounds,
                real(1.0),
                real(0.0),
                V::undefined(),
                V::undefined(),
                V::undefined(),
            ];
            let mut args: Vec<*mut V> = values.iter_mut().map(|v| v as *mut V).collect();
            let mut result = V::undefined();
            unsafe { f(s, o, &mut result, 6, args.as_mut_ptr()) };
        }
    }
}
pub fn tick(g: G, c: &super::Coop) {
    let Some(net) = &c.net else { return };
    for hit in std::mem::take(&mut *OUT.lock().unwrap()) {
        net.broadcast(Msg::BossContact(hit.clone()));
        receive(g, 0, hit);
    }
}
pub fn receive(g: G, from: PeerId, hit: Hit) {
    if from != 0
        || !hit.sane()
        || hit.run != super::rooms::RUN_SEED.load(Ordering::Relaxed)
        || hit.peer != super::MY_ID.load(Ordering::Relaxed)
        || hit.room != g.room_name()
        || (hit.level, hit.sub) != super::rooms::level_sub(g)
        || hit.sequence <= LAST.load(Ordering::Relaxed)
    {
        return;
    }
    LAST.store(hit.sequence, Ordering::Relaxed);
    PENDING.lock().unwrap().push(hit);
}
pub fn apply(g: G, player: V, s: *mut c_void, o: *mut c_void) {
    let _pump = crate::block_pump();
    let hits = std::mem::take(&mut *PENDING.lock().unwrap());
    for hit in hits {
        if hit.run != super::rooms::RUN_SEED.load(Ordering::Relaxed)
            || hit.room != g.room_name()
            || (hit.level, hit.sub) != super::rooms::level_sub(g)
            || truthy(&g.get(player, "dead"))
        {
            continue;
        }
        let vector = g.call("variable_clone", &[g.get(player, "v2_knockback")]);
        g.set(vector, "x", real(hit.kx));
        g.set(vector, "y", real(hit.ky));
        // Scale enemy damage once here; this call has no projectile ancestry.
        let damage = hit.amount * crate::config::RUN_RULES.lock().unwrap().damage_scale();
        let args = g.call("array_create", &[real(5.0)]);
        for (i, value) in [
            real(damage),
            real(hit.x),
            real(hit.y),
            vector,
            real(hit.kind),
        ]
        .into_iter()
        .enumerate()
        {
            g.call("array_set", &[args, real(i as f64), value]);
        }
        if let Some(call) = g.rt.builtin("method_call") {
            let mut result = V::undefined();
            let mut argv = [g.get(player, "set_damage"), args];
            unsafe { call(&mut result, s, o, 2, argv.as_mut_ptr()) };
        }
    }
}
