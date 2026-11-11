//! Shared exits. Execute the native exit on every machine, including its
//! progression/logging side effects; the host orders competing exit requests.
use super::{Coop, rooms};
use crate::engine::{G, V, boolean, key, truthy};
use crate::hooks::ScriptFn;
use crate::net::{Msg, PeerId, Role};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

type EventFn = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);
static STEP: AtomicUsize = AtomicUsize::new(0);
static CHECK: AtomicUsize = AtomicUsize::new(0);
static COLLISION: AtomicUsize = AtomicUsize::new(0);
static DRAW: AtomicUsize = AtomicUsize::new(0);
static SHOP_EXIT: AtomicUsize = AtomicUsize::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static RELEASE: AtomicBool = AtomicBool::new(false);
static REQUEST: AtomicBool = AtomicBool::new(false);
static PLAYER_PTR: AtomicUsize = AtomicUsize::new(0);
static PLAYER_KEY: AtomicU64 = AtomicU64::new(0);

#[derive(Default, Debug)]
pub struct Travel {
    epoch: u32,
    source: String,
    pending: Option<i16>,
    applied: bool,
    waiting:Option<(PeerId,Msg)>,
}
impl Travel {
    pub fn committed(&self) -> bool { self.pending.is_some() }
    pub fn room_changed(&mut self) { self.applied = false; self.pending = None; }
}
fn run() -> u64 { rooms::RUN_SEED.load(Ordering::Relaxed) }
fn everyone_in_elevator(c: &Coop, g: G, room: &str, door: i16) -> bool {
    let Some(net) = &c.net else { return false };
    let Some(area) = g.instances("objEntrance").into_iter()
        .find(|e| g.num(*e, "door_i") as i16 == door && !truthy(&g.get(*e, "block")))
        else { return false };
    // Match the native player/entrance collision geometry. The player's mask
    // can touch the platform before its origin crosses the entrance's edge.
    let Some(player) = g.first("objPlayer") else { return false };
    let x=g.num(player,"x"); let y=g.num(player,"y");
    let (dx1,dx2,dy1,dy2)=(g.num(player,"bbox_left")-x,g.num(player,"bbox_right")-x,
        g.num(player,"bbox_top")-y,g.num(player,"bbox_bottom")-y);
    let (left, right, top, bottom) = (
        g.num(area, "bbox_left"), g.num(area, "bbox_right"),
        g.num(area, "bbox_top"), g.num(area, "bbox_bottom"),
    );
    let inside = |x: f64, y: f64| x+dx2 >= left && x+dx1 <= right && y+dy2 >= top && y+dy1 <= bottom;
    net.peers.iter().all(|peer| {
        if peer.id == net.my_id {
            g.first("objPlayer").is_some_and(|p| !truthy(&g.get(p, "dead"))
                && inside(g.num(p, "x"), g.num(p, "y")))
        } else {
            c.remotes.get(&peer.id).is_some_and(|r| r.room == room && !r.busy
                && r.seen.elapsed() < Duration::from_secs(1)
                && r.state.as_ref().is_some_and(|s| s.room == room && !s.dead
                    && inside(s.x as f64, s.y as f64)))
        }
    })
}
// Personal menus never consume the shared travel epoch, even on repeated visits.
fn personal_exit(room: &str, door: i16) -> bool {
    (room.starts_with("Room_hub") && matches!(door, 0 | 1))
        || (room == "Room_shop" && door == -2)
}
fn request(c: &mut Coop, g: G, door: i16) {
    let Some(n) = &c.net else { return };
    let (level, sub) = rooms::level_sub(g);
    let msg = Msg::TravelRequest { run: run(), epoch: c.travel.epoch, room: g.room_name(), level, sub, door };
    if n.role == Role::Host { receive(c, g, 0, msg); } else { n.broadcast(msg); }
}
pub fn receive(c: &mut Coop, g: G, from: PeerId, msg: Msg) {
    let Some(n) = &c.net else { return };
    if run() == 0 || !n.peers.iter().any(|p| p.id == from) { return; }
    match msg {
        Msg::TravelRequest { run: r, epoch, room, level, sub, door } if r == run() && n.role == Role::Host => {
            if personal_exit(&room, door) || epoch != c.travel.epoch || (level, sub) != rooms::level_sub(g)
                || c.travel.pending.is_some()
                || (c.travel.source == room && (c.travel.applied || g.room_name() != room)) { return; }
            // Personal purchases/unlock screens may temporarily separate peers.
            // Retain the first exit request until everyone returns; never drop a
            // committed transition on a client that is finishing a personal menu.
            if room!=g.room_name()||super::economy::busy(g)||n.peers.iter().filter(|p|p.id!=n.my_id).any(|p|c.remotes.get(&p.id).is_none_or(|r|r.room!=room||r.busy)){
                if c.travel.waiting.is_none(){c.travel.waiting=Some((from,Msg::TravelRequest{run:r,epoch,room,level,sub,door}));}return;
            }
            let valid = if door == -1 {
                room.starts_with("Room_fight") && g.first("objPlayer").is_some_and(|p| truthy(&g.get(p, "room_complete")))
            } else {
                g.instances("objEntrance").iter().any(|e| g.num(*e, "door_i") as i16 == door)
            };
            if !valid { return; }
            // Hub door 2 is the next-level elevator. Presence is current, not a
            // latched vote: walking away cancels it. Other exits retain their rule.
            if room.starts_with("Room_hub") && door == 2
                && !everyone_in_elevator(c, g, &room, door) {
                c.travel.waiting = None;
                return;
            }
            c.travel.waiting=None;
            c.travel.epoch += 1;
            c.travel.source = room.clone();
            c.travel.pending = Some(door);
            c.travel.applied = false;
            n.broadcast(Msg::TravelCommit { run: r, epoch: c.travel.epoch, room, level, sub, door });
        }
        Msg::TravelCommit { run: r, epoch, room, level, sub, door } if r == run() && from == 0 && n.role == Role::Client => {
            if !personal_exit(&room, door) && epoch == c.travel.epoch + 1 && room == g.room_name() && (level, sub) == rooms::level_sub(g) {
                c.travel.epoch = epoch;
                c.travel.source = room;
                c.travel.pending = Some(door);
                c.travel.applied = false;
            }
        }
        _ => {}
    }
}
pub fn tick(c:&mut Coop,g:G){
    if let Some((peer,msg))=c.travel.waiting.take(){receive(c,g,peer,msg);}
    // Collision callbacks can stop once a body rests against a solid edge.
    // Re-evaluate current presence every frame; do not latch old entry votes.
    if g.room_name().starts_with("Room_hub")
        && c.travel.pending.is_none() && !c.travel.applied
        && c.net.as_ref().is_some_and(|n|n.role==Role::Host)
        && everyone_in_elevator(c,g,&g.room_name(),2) {
        request(c,g,2);
    }
}
pub fn install_hooks(g: G) {
    for (name, hook, slot) in [
        ("gml_Script_anon@14594@gml_Object_objLab_Other_10", shop_exit as *const (), &SHOP_EXIT),
        ("gml_Object_objPlayer_Step_0", step as *const (), &STEP),
        ("gml_Script_InputCheck", check as *const (), &CHECK),
        ("gml_Object_objEntrance_Collision_objPlayer", collision as *const (), &COLLISION),
        ("gml_Object_objEntrance_Draw_0", draw as *const (), &DRAW),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(p) => slot.store(p as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("travel: {e}")),
            }
        }
    }
}
unsafe extern "C-unwind" fn check(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(CHECK.load(Ordering::Relaxed)) };
    // Native cleared-room continuation checks verb 8 (interact / E).
    let intercept = ACTIVE.load(Ordering::Relaxed) && argc > 0 && !args.is_null()
        && !unsafe { *args }.is_null() && unsafe { (**args).as_f64() } == Some(8.0);
    // A committed exit is a shared world action. The local pause menu (also
    // opened by controller disconnect) must not swallow its one-frame release.
    // Keep every other gameplay input blocked while the menu owns input.
    if intercept && RELEASE.load(Ordering::Relaxed) { unsafe { *ret = boolean(true) }; return ret; }
    if super::pause::block_player_input() { unsafe { *ret=boolean(false) }; return ret; }
    if !intercept { return unsafe { f(s, o, ret, argc, args) }; }
    let result = unsafe { f(s, o, ret, argc, args) };
    if !result.is_null() && truthy(&unsafe { *result }) { REQUEST.store(true, Ordering::Relaxed); }
    unsafe { *ret = boolean(false) }; ret
}
unsafe extern "C-unwind" fn step(s: *mut c_void, o: *mut c_void) {
    let f: EventFn = unsafe { std::mem::transmute(STEP.load(Ordering::Relaxed)) };
    if run() == 0 { unsafe { f(s, o) }; return; }
    let Some(g) = G::new() else { return };
    let Some(player) = (unsafe { g.rt.builtin_var_of("id", s) }) else { return };
    super::boss_contact::apply(g,player,s,o);
    if super::life::before_step(g,s,o){return;}
    super::projectiles::replay(g, s);
    PLAYER_PTR.store(s as usize, Ordering::Relaxed);
    PLAYER_KEY.store(key(&player), Ordering::Relaxed);
    let release = {
        let Ok(mut c) = super::COOP.try_lock() else { return };
        if c.travel.source != g.room_name() { c.travel.applied = false; }
        let release = c.travel.pending == Some(-1) && c.travel.source == g.room_name();
        if release {
            c.travel.pending = None;
            c.travel.applied = true;
            g.set(player, "room_complete", boolean(true));
        }
        release
    };
    let active = g.room_name().starts_with("Room_fight") && truthy(&g.get(player, "room_complete"));
    ACTIVE.store(active, Ordering::Relaxed);
    RELEASE.store(release, Ordering::Relaxed);
    REQUEST.store(false, Ordering::Relaxed);
    if release { super::life::prepare_exit(g); rooms::reseed(5); }
    // Native clear bookkeeping (level.next, stats and rewards) is inside an
    // alive-only branch. Run that branch once for downed peers too, otherwise
    // their level/sub lags behind the survivor and rejects the shared exit.
    let down_clear = active && !release && truthy(&g.get(player, "dead"))
        && !truthy(&g.get(player, "level_clear"));
    let saved = (g.get(player, "hp"), g.get(player, "state"));
    if down_clear {
        g.set(player, "dead", boolean(false));
        g.set(player, "hp", crate::engine::real(1.0));
        g.set(player, "state", g.s("idle"));
    }
    super::pause::begin_player(g);
    if down_clear { super::pause::block_for_step(); }
    {
        let _death = super::life::death_step(g, player);
        unsafe { f(s, o) };
    }
    super::pause::end_player();
    if down_clear {
        g.set(player, "dead", boolean(true));
        g.set(player, "hp", saved.0);
        g.set(player, "state", saved.1);
    }
    ACTIVE.store(false, Ordering::Relaxed);
    RELEASE.store(false, Ordering::Relaxed);
    if release { crate::log("travel: applied shared cleared-room continuation"); }
    if REQUEST.swap(false, Ordering::Relaxed) {
        if let Ok(mut c) = super::COOP.try_lock() { request(&mut c, g, -1); }
    }
}
unsafe extern "C-unwind" fn collision(s: *mut c_void, o: *mut c_void) {
    let f: EventFn = unsafe { std::mem::transmute(COLLISION.load(Ordering::Relaxed)) };
    if run() == 0 { unsafe { f(s, o) }; return; }
    let Some(g) = G::new() else { return };
    let Some(door) = (unsafe { g.rt.builtin_var_of("id", s) }) else { return };
    let Some(player) = (unsafe { g.rt.builtin_var_of("id", o) }) else { return };
    if truthy(&g.get(door, "block")) || truthy(&g.get(player, "dead")) { return; }
    // Hub shop entry and documentation belong only to the touching player.
    // Run the native transition locally; do not enqueue or broadcast travel.
    if personal_exit(&g.room_name(), g.num(door, "door_i") as i16) {
        unsafe { f(s, o) }; return;
    }
    if let Ok(mut c) = super::COOP.try_lock() { request(&mut c, g, g.num(door, "door_i") as i16); }
}
unsafe extern "C-unwind" fn draw(s: *mut c_void, o: *mut c_void) {
    let f: EventFn = unsafe { std::mem::transmute(DRAW.load(Ordering::Relaxed)) };
    unsafe { f(s, o) };
    if run() == 0 { return; }
    let Some(g) = G::new() else { return };
    let Some(door) = (unsafe { g.rt.builtin_var_of("id", s) }) else { return };
    let Some(player) = g.first("objPlayer") else { return };
    if key(&player) != PLAYER_KEY.load(Ordering::Relaxed) || PLAYER_PTR.load(Ordering::Relaxed) == 0 { return; }
    let release = {
        let Ok(mut c) = super::COOP.try_lock() else { return };
        if c.travel.source == g.room_name() && c.travel.pending == Some(g.num(door, "door_i") as i16) {
            c.travel.pending = None; c.travel.applied = true; true
        } else { false }
    };
    if release {
        super::life::prepare_exit(g);
        g.set(door, "block", boolean(false));
        rooms::reseed(6);
        let collision: EventFn = unsafe { std::mem::transmute(COLLISION.load(Ordering::Relaxed)) };
        unsafe { collision(s, PLAYER_PTR.load(Ordering::Relaxed) as *mut c_void) };
        crate::log("travel: applied shared entrance collision");
    }
}

unsafe extern "C-unwind" fn shop_exit(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(SHOP_EXIT.load(Ordering::Relaxed)) };
    if run() != 0 && G::new().is_some_and(|g| g.room_name() == "Room_shop") {
        // Keep the queued purchase callback's menu alive until its reply arrives.
        if super::economy::local_purchase_pending() {
            unsafe { *ret = V::undefined() }; return ret;
        }
        // Each player returns independently to the same generated hub.
        rooms::reseed(7);
    }
    unsafe { f(s, o, ret, argc, args) }
}
