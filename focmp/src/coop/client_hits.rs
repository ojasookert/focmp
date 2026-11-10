//! Client enemy hits are requests plus presentation, never local enemy damage.
//! In particular, calling set_damage and restoring HP is not a rollback: its
//! status callbacks, timers, rewards and nested hits have already run by then.
use crate::engine::{G, V, boolean, is_struct, key, real, truthy};
use crate::net::EnemyHit;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

static REPORTED: AtomicU64 = AtomicU64::new(0);
static UNMAPPED: AtomicU64 = AtomicU64::new(0);
static REFLECTED: AtomicU64 = AtomicU64::new(0);
static FEEDBACK: LazyLock<Mutex<HashMap<u64, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn reset() {
    FEEDBACK.lock().unwrap().clear();
    for counter in [&REPORTED, &UNMAPPED, &REFLECTED] {
        counter.store(0, Ordering::Relaxed);
    }
}
pub fn describe() -> String {
    format!(
        "client hits: {} reported, {} unmapped, {} shield reflections; native enemy damage disabled on clients",
        REPORTED.load(Ordering::Relaxed),
        UNMAPPED.load(Ordering::Relaxed),
        REFLECTED.load(Ordering::Relaxed)
    )
}

/// Always returns a defined bool, matching the native damage method's dead
/// result. Projectiles perform their collision/trigger work outside that call.
/// We cannot predict a kill: only an already confirmed dead enemy returns true.
pub unsafe fn hit(s: *mut c_void, n: i32, args: *mut *mut V) -> V {
    let Some(g) = G::new() else {
        return boolean(false);
    };
    let Some(enemy) = (unsafe { g.rt.builtin_var_of("id", s) }) else {
        return boolean(false);
    };
    if truthy(&g.get(enemy, "dead")) {
        return boolean(true);
    }
    if n < 8 || args.is_null() {
        return boolean(false);
    }
    let mut a = Vec::with_capacity(n.min(9) as usize);
    for i in 0..n.min(9) as usize {
        let p = unsafe { *args.add(i) };
        if p.is_null() {
            return boolean(false);
        }
        a.push(unsafe { *p });
    }
    let gid = g.num(enemy, "focmp_gid");
    if !(1.0..=u16::MAX as f64).contains(&gid) {
        if UNMAPPED.fetch_add(1, Ordering::Relaxed) == 0 {
            crate::log("client hits: ignored contact before enemy received a host ID");
        }
        return boolean(false);
    }
    let (level, sub) = super::rooms::level_sub(g);
    let hit = EnemyHit {
        gid: gid as u16,
        room: g.room_name(),
        level,
        sub,
        amount: a[0].as_f64().unwrap_or(f64::NAN),
        x: a[1].as_f64().unwrap_or(f64::NAN),
        y: a[2].as_f64().unwrap_or(f64::NAN),
        kx: g.num(a[3], "x"),
        ky: g.num(a[3], "y"),
        dtype: a[4].as_f64().unwrap_or(1.0) as u8,
        scale: a[5].as_f64().unwrap_or(0.0),
        flag: truthy(&a[6]),
        extra: a.get(8).and_then(V::as_f64),
    };
    if !hit.sane() {
        return boolean(false);
    }
    // Capture first: reflection may mutate/destroy the projectile. Never hold
    // the hit queue lock across native presentation or projectile callbacks.
    super::enemies::HITS.lock().unwrap().push(hit.clone());
    REPORTED.fetch_add(1, Ordering::Relaxed);

    let blocked = shield_blocks(g, enemy, &hit);
    if blocked && hit.scale == 1.0 && g.exists(a[7]) {
        let excluded = g.get(enemy, "shielded_excluded_dmg_type_array");
        let excluded = (0..g.arr_len(excluded))
            .any(|i| g.arr_get(excluded, i).as_f64() == Some(hit.dtype as f64));
        if !excluded
            && method(
                g,
                a[7],
                "reflect_from_shield",
                &[g.get(enemy, "x"), g.get(enemy, "y"), enemy],
            )
        {
            REFLECTED.fetch_add(1, Ordering::Relaxed);
        }
    }
    // Only presentation helpers audited below. No damage numbers based on an
    // unconfirmed amount, gameplay state transitions, knockback or rewards.
    if hit.amount > 0.0 {
        let now = Instant::now();
        let show = {
            let mut times = FEEDBACK.lock().unwrap();
            let show = times
                .get(&key(&enemy))
                .is_none_or(|last| now.duration_since(*last) >= Duration::from_millis(60));
            if show {
                times.insert(key(&enemy), now);
            }
            show
        };
        if show {
            method(
                g,
                enemy,
                if blocked {
                    "play_block"
                } else {
                    "play_normal_hit"
                },
                &[],
            );
            // Own this brief tint directly. Native flash_red starts a tween
            // whose constructor also assumes a valid ambient GML caller.
            draw_feedback(g, enemy);
        }
    }
    boolean(false)
}

/// Read-only prediction of the native directional shield test. The host still
/// evaluates the actual damage with its own current shield and facing state.
fn shield_blocks(g: G, enemy: V, hit: &EnemyHit) -> bool {
    if !truthy(&g.get(enemy, "shielded")) {
        return false;
    }
    let incoming = (-hit.ky).atan2(hit.kx).to_degrees();
    let back = g.num(enemy, "image_angle") + 180.0;
    let angle = (incoming - back + 180.0).rem_euclid(360.0) - 180.0;
    angle.abs() < g.num(enemy, "shielded_radius")
}

/// Overlay a brief local hit tint after applying the host's visual snapshot.
/// Expiration needs no gameplay event/timer: the next snapshot restores color.
pub fn draw_feedback(g: G, enemy: V) {
    let active = FEEDBACK
        .lock()
        .unwrap()
        .get(&key(&enemy))
        .is_some_and(|t| t.elapsed() < Duration::from_millis(100));
    if active {
        g.set(enemy, "image_blend", real(0x8080ff as f64));
    }
}

/// The selected enemy helpers only play audio;
/// reflect_from_shield is a projectile-owned collision response. None calls
/// enemy set_damage. Bind self explicitly for static constructor methods.
fn method(g: G, target: V, name: &str, args: &[V]) -> bool {
    let m = g.get(target, name);
    if !is_struct(&m) || !truthy(&g.call("is_method", &[m])) {
        return false;
    }
    let bound = g.call("method", &[target, m]);
    let argv = g.call("array_create", &[real(args.len() as f64)]);
    for (i, arg) in args.iter().enumerate() {
        g.call("array_set", &[argv, real(i as f64), *arg]);
    }
    g.call("method_call", &[bound, argv]);
    true
}
