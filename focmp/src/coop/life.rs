//! Keep a downed player in the session; only the host may end the team run.
use crate::{
    engine::{G, V, boolean, real, truthy},
    hooks::ScriptFn,
    net::{Msg, Role},
};
use std::{
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
static MUSIC_STOP: AtomicUsize = AtomicUsize::new(0);
static END: AtomicUsize = AtomicUsize::new(0);
static ROOM: AtomicUsize = AtomicUsize::new(0);
static HUD: AtomicUsize = AtomicUsize::new(0);
static CAMERA_HUD: AtomicUsize = AtomicUsize::new(0);
static GAME_OVER: AtomicBool = AtomicBool::new(false);
static ENDED: AtomicBool = AtomicBool::new(false);
static IN_DAMAGE: AtomicBool = AtomicBool::new(false);
static DEATH_STEP: AtomicBool = AtomicBool::new(false);
static DOWN: AtomicBool = AtomicBool::new(false);
static REVIVE: AtomicBool = AtomicBool::new(false);
static SPECTATING: Mutex<Option<(u8, String)>> = Mutex::new(None);
fn active() -> bool {
    super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0
}
// Native Player.Step puts every enemy fighter into "hybernate" after death.
// Co-op owns the team-wipe decision; one dead player must not suspend the AI.
pub struct DamageScope(bool);
pub fn damage_scope() -> DamageScope {
    DamageScope(IN_DAMAGE.swap(true, Ordering::Relaxed))
}
impl Drop for DamageScope {
    fn drop(&mut self) {
        IN_DAMAGE.store(self.0, Ordering::Relaxed);
    }
}
pub struct DeathStep(bool);
pub fn death_step(_g: G, _p: V) -> DeathStep {
    DeathStep(DEATH_STEP.swap(active() && !ending(), Ordering::Relaxed))
}
impl Drop for DeathStep {
    fn drop(&mut self) {
        DEATH_STEP.store(self.0, Ordering::Relaxed);
    }
}
pub fn suppress_hibernate() -> bool {
    // Poison/fire can kill during this very Step, after the scope was entered.
    DEATH_STEP.load(Ordering::Relaxed)
        && G::new().is_some_and(|g| {
            g.first("objPlayer")
                .is_some_and(|p| truthy(&g.get(p, "dead")))
        })
}
pub fn reset() {
    for flag in [&GAME_OVER, &ENDED, &DOWN, &REVIVE] {
        flag.store(false, Ordering::Relaxed);
    }
    *SPECTATING.lock().unwrap() = None;
}
pub fn ending() -> bool {
    GAME_OVER.load(Ordering::Relaxed)
}
pub fn game_over() {
    GAME_OVER.store(true, Ordering::Relaxed);
}
pub fn install(g: G) {
    for (name, hook, slot) in [
        (
            "gml_Script_anon@6600@gml_Object_objMusic_Create_0",
            music_stop as *const (),
            &MUSIC_STOP,
        ),
        (
            "gml_Script_anon@9629@gml_Object_objPlayer_Other_11",
            end as *const (),
            &END,
        ),
        ("gml_Object_objPlayer_Other_4", room as *const (), &ROOM),
        ("gml_Object_objPlayer_Draw_64", hud as *const (), &HUD),
        (
            "gml_Object_objCamera_Draw_64",
            camera_hud as *const (),
            &CAMERA_HUD,
        ),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(p) => slot.store(p as usize, Ordering::SeqCst),
                Err(e) => crate::log(&e),
            }
        }
    }
}
unsafe extern "C-unwind" fn music_stop(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    if active() && !ending() && IN_DAMAGE.load(Ordering::Relaxed) {
        unsafe { *r = V::undefined() };
        return r;
    }
    if active() {
        if let Some(g) = G::new() {
            if let Some(owner) = unsafe { g.rt.builtin_var_of("id", s) } {
                if g.get(owner, "music_audio_id").kind() == crate::gm::KIND_UNDEFINED {
                    unsafe { *r = V::undefined() };
                    return r;
                }
            }
        }
    }
    let f: ScriptFn = unsafe { std::mem::transmute(MUSIC_STOP.load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}
unsafe extern "C-unwind" fn end(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    if active() && !GAME_OVER.load(Ordering::Relaxed) {
        unsafe { *r = V::undefined() };
        return r;
    }
    let f: ScriptFn = unsafe { std::mem::transmute(END.load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}
fn restore(g: G, p: V) {
    let hp = (g.num(p, "hp_max") * 0.5).max(1.0);
    g.set(p, "hp", real(hp));
    g.set(p, "dead", boolean(false));
    g.set(p, "state", g.s("idle"));
    g.set(p, "visible", boolean(true));
    for field in ["onfire", "poisoned", "frozen", "stunned", "electrocuted"] {
        g.set(p, field, boolean(false));
    }
    g.set(p, "poison_stack", real(0.0));
    if let Some(cam) = g.first("objCamera") {
        g.set(cam, "follow", p);
        g.set(cam, "desaturate", boolean(false));
    }
    DOWN.store(false, Ordering::Relaxed);
    *SPECTATING.lock().unwrap() = None;
    crate::log(&format!("life: revived in {} with {hp} HP", g.room_name()));
}
// Clear-room continuation and door collisions require a living native player.
// A downed player is revived as that committed transition is executed.
pub fn prepare_exit(g: G) {
    if let Some(p) = g.first("objPlayer") {
        if truthy(&g.get(p, "dead")) || DOWN.load(Ordering::Relaxed) {
            REVIVE.store(true, Ordering::Relaxed);
            restore(g, p);
        }
    }
}
unsafe extern "C-unwind" fn room(s: *mut c_void, o: *mut c_void) {
    let pending = REVIVE.swap(false, Ordering::Relaxed);
    let revive = active() && (DOWN.load(Ordering::Relaxed) || pending);
    let f: unsafe extern "C-unwind" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(ROOM.load(Ordering::Relaxed)) };
    unsafe { f(s, o) };
    if revive && !GAME_OVER.load(Ordering::Relaxed) {
        if let Some(g) = G::new() {
            if let Some(p) = unsafe { g.rt.builtin_var_of("id", s) } {
                restore(g, p);
            }
        }
    }
}
pub fn before_step(g: G, s: *mut c_void, o: *mut c_void) -> bool {
    if !active() {
        return false;
    }
    if GAME_OVER.load(Ordering::Relaxed) {
        if !ENDED.swap(true, Ordering::Relaxed) {
            let f: ScriptFn = unsafe { std::mem::transmute(END.load(Ordering::Relaxed)) };
            let mut r = V::undefined();
            unsafe { f(s, o, &mut r, 0, std::ptr::null_mut()) };
        }
        return true;
    }
    if let Some(p) = unsafe { g.rt.builtin_var_of("id", s) } {
        DOWN.store(truthy(&g.get(p, "dead")), Ordering::Relaxed);
    }
    false
}
pub fn tick(g: G, c: &super::Coop) {
    if !active() || GAME_OVER.load(Ordering::Relaxed) {
        return;
    }
    let Some(p) = g.first("objPlayer") else {
        return;
    };
    let dead = truthy(&g.get(p, "dead"));
    DOWN.store(dead, Ordering::Relaxed);
    let Some(net) = &c.net else { return };
    let room = g.room_name();
    if dead
        && !truthy(&g.get(p, "revive"))
        && net.role == Role::Host
        && room.starts_with("Room_")
        && net.peers.iter().filter(|p| p.id != net.my_id).all(|p| {
            c.remotes
                .get(&p.id)
                .and_then(|r| r.state.as_ref())
                .is_some_and(|s| s.room == room && s.dead && !s.can_revive)
        })
    {
        game_over();
        net.broadcast(Msg::TeamGameOver {
            run: super::rooms::RUN_SEED.load(Ordering::Relaxed),
        });
        crate::log("life: all players down, shared game over");
        return;
    }
    if !dead {
        if SPECTATING.lock().unwrap().take().is_some() {
            if let Some(cam) = g.first("objCamera") {
                g.set(cam, "follow", p);
            }
        }
        return;
    }
    let mut living: Vec<_> = c
        .remotes
        .iter()
        .filter(|(_, r)| r.state.as_ref().is_some_and(|s| s.room == room && !s.dead))
        .filter_map(|(id, r)| {
            c.ghosts
                .get(*id)
                .filter(|gh| g.exists(gh.body))
                .map(|gh| (*id, r.name.clone(), gh.body))
        })
        .collect();
    living.sort_by_key(|x| x.0);
    if living.is_empty() {
        return;
    }
    let mut selected = SPECTATING.lock().unwrap();
    let mut index = selected
        .as_ref()
        .and_then(|(id, _)| living.iter().position(|x| x.0 == *id))
        .unwrap_or(0);
    if truthy(&g.call("keyboard_check_pressed", &[real(9.0)])) {
        index = (index + 1) % living.len();
    }
    let (id, name, body) = &living[index];
    if let Some(cam) = g.first("objCamera") {
        g.set(cam, "follow", *body);
        g.set(cam, "desaturate", boolean(false));
    }
    *selected = Some((*id, name.clone()));
}
unsafe extern "C-unwind" fn hud(s: *mut c_void, o: *mut c_void) {
    let f: unsafe extern "C-unwind" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(HUD.load(Ordering::Relaxed)) };
    let Some(g) = G::new().filter(|_| active()) else {
        unsafe { f(s, o) };
        return;
    };
    let dead = unsafe { g.rt.builtin_var_of("id", s) }
        .is_some_and(|p| truthy(&g.get(p, "dead")) && !truthy(&g.get(p, "revive")));
    if !dead {
        unsafe { f(s, o) };
        return;
    }
    // The spectator overlay belongs to the persistent camera, even if a native
    // death effect hides the local player's instance (and its Draw GUI event).
}
unsafe extern "C-unwind" fn camera_hud(s: *mut c_void, o: *mut c_void) {
    let f: unsafe extern "C-unwind" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(CAMERA_HUD.load(Ordering::Relaxed)) };
    unsafe { f(s, o) };
    let Some(g) = G::new().filter(|g| {
        active()
            && !ending()
            && g.first("objPlayer")
                .is_some_and(|p| truthy(&g.get(p, "dead")) && !truthy(&g.get(p, "revive")))
    }) else {
        return;
    };
    let selected = SPECTATING.lock().unwrap().clone();
    let text = if let Some((_, name)) = selected {
        format!("SPECTATING {name}  [TAB]\nRevive in the next room")
    } else {
        "Waiting for the team".into()
    };
    let color = g.call("draw_get_color", &[]);
    let align = g.call("draw_get_halign", &[]);
    g.call("draw_set_color", &[real(0xffffff as f64)]);
    g.call("draw_set_halign", &[real(1.0)]);
    g.call("draw_text", &[real(320.0), real(324.0), g.s(&text)]);
    g.call("draw_set_color", &[color]);
    g.call("draw_set_halign", &[align]);
}
