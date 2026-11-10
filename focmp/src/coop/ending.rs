//! Unanimous votes on the native victory buttons, followed by one transition.
use super::Coop;
use crate::{
    engine::{G, V, boolean, is_struct, real},
    hooks::ScriptFn,
    net::{Msg, PeerId, Role},
};
use bitcode::{Decode, Encode};
use std::time::{Duration, Instant};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
static START: AtomicUsize = AtomicUsize::new(0);
static STOP: AtomicUsize = AtomicUsize::new(0);
static STEP: AtomicUsize = AtomicUsize::new(0);
static RELEASING: AtomicBool = AtomicBool::new(false);
#[derive(Clone, Debug, Encode, Decode)]
pub enum Action {
    Vote {
        version: u32,
        choice: u8,
    },
    State {
        revision: u32,
        votes: [u8; 3],
        versions: [u32; 3],
        preparing: bool,
    },
    Ack {
        revision: u32,
    },
    Commit {
        revision: u32,
        choice: u8,
    },
}
#[derive(Clone, Debug, Encode, Decode)]
pub struct Packet {
    run: u64,
    level: u32,
    sub: u32,
    action: Action,
}
impl Packet {
    pub fn valid_from(&self, from: PeerId) -> bool {
        self.run != 0
            && match &self.action {
                Action::Vote { version, choice } => *version > 0 && (1..=2).contains(choice),
                Action::State { votes, .. } => from == 0 && votes.iter().all(|v| *v <= 2),
                Action::Ack { .. } => true,
                Action::Commit { choice, .. } => from == 0 && (1..=2).contains(choice),
            }
    }
}
#[derive(Default)]
pub struct Ending {
    stage: Option<(u32, u32)>,
    votes: [u8; 3],
    versions: [u32; 3],
    local_version: u32,
    revision: u32,
    preparing: bool,
    acked: [bool; 3],
    choice: u8,
    applied: bool,
    local_choice: u8,
    sent: Option<Instant>,
    sent_state: Option<Instant>,
}
impl Ending {
    pub fn extracting(&self) -> bool {
        self.choice == 2
    }
    pub fn pending(&self) -> bool {
        self.choice != 0 && !self.applied
    }
    pub fn member_left(&mut self, peer: PeerId) {
        if self.choice != 0 {
            return;
        }
        self.votes[peer as usize] = 0;
        self.versions[peer as usize] = 0;
        self.invalidate();
    }
    fn invalidate(&mut self) {
        self.revision += 1;
        self.preparing = false;
        self.acked = [false; 3];
    }
}
fn packet(g: G, action: Action) -> Packet {
    let (level, sub) = super::rooms::level_sub(g);
    Packet {
        run: super::rooms::RUN_SEED.load(Ordering::Relaxed),
        level,
        sub,
        action,
    }
}
fn ensure(g: G, c: &mut Coop) -> bool {
    if super::rooms::RUN_SEED.load(Ordering::Relaxed) == 0 || g.room_name() != "Room_game_end" {
        return false;
    }
    let stage = super::rooms::level_sub(g);
    if c.ending.stage != Some(stage) {
        c.ending = Ending {
            stage: Some(stage),
            ..Default::default()
        };
    }
    true
}
fn publish(g: G, c: &Coop) {
    if let Some(n) = &c.net {
        n.broadcast(Msg::Ending(packet(
            g,
            Action::State {
                revision: c.ending.revision,
                votes: c.ending.votes,
                versions: c.ending.versions,
                preparing: c.ending.preparing,
            },
        )));
    }
}
pub fn receive(g: G, c: &mut Coop, from: PeerId, p: Packet) {
    if !p.valid_from(from)
        || p.run != super::rooms::RUN_SEED.load(Ordering::Relaxed)
        || (p.level, p.sub) != super::rooms::level_sub(g)
        || !ensure(g, c)
    {
        return;
    }
    let Some(n) = &c.net else { return };
    if !n.peers.iter().any(|p| p.id == from) {
        return;
    }
    let host = n.role == Role::Host;
    let id = n.my_id as usize;
    let e = &mut c.ending;
    if e.choice != 0 {
        return;
    }
    match p.action {
        Action::Vote { version, choice } if host && version > e.versions[from as usize] => {
            e.versions[from as usize] = version;
            e.votes[from as usize] = choice;
            e.invalidate();
            publish(g, c);
        }
        Action::State {
            revision,
            votes,
            versions,
            preparing,
        } if !host && from == 0 && revision >= e.revision => {
            e.revision = revision;
            e.votes = votes;
            e.versions = versions;
            e.preparing = preparing;
            // A pending local vote change must be acknowledged by the host
            // before this peer can approve any earlier unanimous snapshot.
            if preparing && versions[id] == e.local_version {
                n.broadcast(Msg::Ending(packet(g, Action::Ack { revision })));
            }
        }
        Action::Ack { revision } if host && e.preparing && revision == e.revision => {
            e.acked[from as usize] = true;
        }
        Action::Commit { revision, choice }
            if !host && from == 0 && e.preparing && revision == e.revision =>
        {
            e.choice = choice;
        }
        _ => {}
    }
}
pub fn tick(g: G, c: &mut Coop) {
    // Keep the transport alive until every peer has executed native Extract.
    // Closing it immediately could strand clients before their Menu.Step.
    if c.ending.extracting() && c.ending.applied && g.room_name() == "Room_main_menu" {
        if c.net.as_ref().is_some_and(|n| {
            n.role == Role::Host
                && n.peers.iter().all(|p| {
                    p.id == n.my_id
                        || c.remotes
                            .get(&p.id)
                            .is_some_and(|r| r.room == "Room_main_menu")
                })
        }) {
            c.leave(g);
        }
        return;
    }
    if !ensure(g, c) {
        return;
    }
    // A peer can reach this screen after the host has already voted. Publish
    // the current dots without requiring that peer to cast a vote first.
    if c.net.as_ref().is_some_and(|n| n.role == Role::Host)
        && c.ending.choice == 0
        && c.ending
            .sent_state
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(500))
    {
        c.ending.sent_state = Some(Instant::now());
        publish(g, c);
    }
    let Some(n) = &c.net else { return };
    if n.role != Role::Host {
        let e = &mut c.ending;
        if e.choice == 0
            && e.local_version > e.versions[n.my_id as usize]
            && e.sent
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(250))
        {
            n.broadcast(Msg::Ending(packet(
                g,
                Action::Vote {
                    version: e.local_version,
                    choice: e.local_choice,
                },
            )));
            e.sent = Some(Instant::now());
        }
        return;
    }
    let e = &mut c.ending;
    if e.choice != 0 {
        return;
    }
    let choice = e.votes[n.my_id as usize];
    if choice != 0 && n.peers.iter().all(|p| e.votes[p.id as usize] == choice) {
        if !e.preparing {
            e.preparing = true;
            e.acked[n.my_id as usize] = true;
            publish(g, c);
            return;
        }
        if n.peers.iter().all(|p| e.acked[p.id as usize]) {
            e.choice = choice;
            n.broadcast(Msg::Ending(packet(
                g,
                Action::Commit {
                    revision: e.revision,
                    choice,
                },
            )));
            crate::log(&format!("ending: unanimous choice {choice}"));
        }
    }
}
pub fn page(g: G) -> Option<V> {
    let menu = g.first("objMenuShared")?;
    let pages = g.get(menu, "pages");
    if !is_struct(&pages) {
        return None;
    }
    let page = g.get(pages, "page_top_menu");
    is_struct(&page).then_some(page)
}
pub fn pointer(g: G) -> Option<(f64, f64)> {
    let page = page(g)?;
    let i = g.num(page, "i_infocus");
    if !(0.0..2.0).contains(&i) {
        return None;
    }
    let item = g.arr_get(g.get(page, "items"), i as usize);
    Some((
        (g.num(item, "x1") + g.num(item, "x2")) / 2.0,
        (g.num(item, "y1") + g.num(item, "y2")) / 2.0,
    ))
}
pub fn install(g: G) {
    for (name, hook, slot) in [
        (
            "gml_Script_start_game@Menu_class@Menu_class",
            start as *const (),
            &START,
        ),
        (
            "gml_Script_anon@12689@Menu_class@Menu_class",
            stop as *const (),
            &STOP,
        ),
        ("gml_Object_objGameEnd_Step_0", end_step as *const (), &STEP),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(f) => slot.store(f as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("ending: {e}")),
            }
        }
    }
}
unsafe fn vote(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
    choice: u8,
    slot: &AtomicUsize,
) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(slot.load(Ordering::Relaxed)) };
    if !RELEASING.load(Ordering::Relaxed) {
        if let Some(g) = G::new().filter(|g| {
            super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0 && g.room_name() == "Room_game_end"
        }) {
            if let Ok(mut c) = super::COOP.try_lock() {
                if ensure(g, &mut c) && c.ending.choice == 0 {
                    c.ending.local_version += 1;
                    c.ending.local_choice = choice;
                    c.ending.sent = Some(Instant::now());
                    let p = packet(
                        g,
                        Action::Vote {
                            version: c.ending.local_version,
                            choice,
                        },
                    );
                    if let Some(net) = &c.net {
                        if net.role == Role::Host {
                            receive(g, &mut c, 0, p);
                        } else {
                            net.broadcast(Msg::Ending(p));
                        }
                    }
                }
            }
            unsafe { *r = V::undefined() };
            return r;
        }
    }
    unsafe { f(s, o, r, n, a) }
}
unsafe extern "C-unwind" fn start(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    unsafe { vote(s, o, r, n, a, 1, &START) }
}
unsafe extern "C-unwind" fn stop(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    unsafe { vote(s, o, r, n, a, 2, &STOP) }
}
pub fn step(g: G, s: *mut c_void, o: *mut c_void) {
    if g.room_name() != "Room_game_end" {
        return;
    }
    let Some(page) = page(g) else { return };
    let choice = {
        let Ok(mut c) = super::COOP.try_lock() else {
            return;
        };
        if !c.ending.pending() {
            return;
        }
        c.ending.applied = true;
        c.ending.choice
    };
    let item = g.arr_get(g.get(page, "items"), (choice - 1) as usize);
    let args = g.call("array_create", &[real(1.0)]);
    g.call("array_set", &[args, real(0.0), item]);
    if let Some(call) = g.rt.builtin("method_call") {
        let mut result = V::undefined();
        let mut argv = [
            g.get(
                page,
                if choice == 1 {
                    "start_game"
                } else {
                    "stop_game"
                },
            ),
            args,
        ];
        RELEASING.store(true, Ordering::Relaxed);
        unsafe { call(&mut result, s, o, 2, argv.as_mut_ptr()) };
        RELEASING.store(false, Ordering::Relaxed);
    }
}
unsafe extern "C-unwind" fn end_step(s: *mut c_void, o: *mut c_void) {
    let _pump = crate::block_pump();
    let f: unsafe extern "C-unwind" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(STEP.load(Ordering::Relaxed)) };
    unsafe { f(s, o) };
    if let Some(g) = G::new() {
        step(g, s, o);
    }
}
pub fn draw(g: G, page: V, c: &Coop) {
    if g.room_name() != "Room_game_end" || super::rooms::RUN_SEED.load(Ordering::Relaxed) == 0 {
        return;
    }
    let saved = [g.call("draw_get_color", &[]), g.call("draw_get_alpha", &[])];
    g.call("draw_set_alpha", &[real(1.0)]);
    let items = g.get(page, "items");
    for (id, _, _) in c.roster() {
        let choice = c.ending.votes[id as usize];
        if choice == 0 {
            continue;
        }
        let item = g.arr_get(items, (choice - 1) as usize);
        g.call(
            "draw_set_color",
            &[real(super::colors::value(c.colors[id as usize]))],
        );
        g.call(
            "draw_circle",
            &[
                real(g.num(item, "x2") + 8.0 + id as f64 * 8.0),
                real((g.num(item, "y1") + g.num(item, "y2")) / 2.0),
                real(2.5),
                boolean(false),
            ],
        );
    }
    g.call("draw_set_color", &[saved[0]]);
    g.call("draw_set_alpha", &[saved[1]]);
    super::cursors::draw(g, c);
}
