//! Personal card choices with a shared, revision-checked progression barrier.
//! Native confirmation (including loadout/stat changes) runs once, on commit.
use super::{Coop, rooms};
use crate::engine::{G, V, boolean, is_struct, key, real, truthy};
use crate::hooks::ScriptFn;
use crate::net::{Msg, PeerId, Role, SelectionPlayer, SelectionState};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

type EventFn = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);
static PERK_STEP: AtomicUsize = AtomicUsize::new(0);
static GUN_STEP: AtomicUsize = AtomicUsize::new(0);
static PERK_DRAW: AtomicUsize = AtomicUsize::new(0);
static GUN_DRAW: AtomicUsize = AtomicUsize::new(0);
static ACCEPT: AtomicUsize = AtomicUsize::new(0);
static CANCEL: AtomicUsize = AtomicUsize::new(0);
static PRESSED: AtomicUsize = AtomicUsize::new(0);
static PRESSED_MANY: AtomicUsize = AtomicUsize::new(0);
static DRAW_CARD: AtomicUsize = AtomicUsize::new(0);
static BUTTON: AtomicU64 = AtomicU64::new(0);
static RELEASING: AtomicBool = AtomicBool::new(false);
static READY_CLICK: AtomicBool = AtomicBool::new(false);
static CANCEL_READY: AtomicBool = AtomicBool::new(false);
pub static DRAWING_PERKS: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
pub struct Selection {
    pub state: SelectionState,
    local: SelectionPlayer,
    local_stage: u8,
    instance: Option<u64>,
    dirty: bool,
    acked: [bool; 3],
    frozen: Option<(u8, u32)>,
    committed_stage: u8,
    release: Option<u8>,
    applied_stage: u8,
    future: Vec<(PeerId, u8, SelectionPlayer)>,
}
impl Selection {
    pub fn describe(&self) -> String {
        format!("selection: stage {} serial {} preparing {} acked {:?} committed {} applied {}\n  {:?}", self.state.stage, self.state.serial, self.state.preparing, self.acked, self.committed_stage, self.applied_stage, self.state.players)
    }
    pub fn member_left(&mut self, id: PeerId) {
        self.state.players[id as usize] = SelectionPlayer::default();
        self.invalidate();
    }
    fn invalidate(&mut self) {
        self.state.serial = self.state.serial.wrapping_add(1);
        self.state.preparing = false;
        self.acked = [false; 3];
        self.frozen = None;
        self.dirty = true;
    }
}
fn run() -> u64 { rooms::RUN_SEED.load(Ordering::Relaxed) }
fn submit(c: &mut Coop, msg: Msg) {
    let Some(n) = &c.net else { return };
    if n.role == Role::Host { receive(c, 0, msg); } else { n.broadcast(msg); }
}
pub fn receive(c: &mut Coop, from: PeerId, msg: Msg) {
    let Some(n) = &c.net else { return };
    if !n.peers.iter().any(|p| p.id == from) { return; }
    let host = n.role == Role::Host;
    let current = run();
    if current == 0 { return; }
    let d = &mut c.selection;
    match msg {
        Msg::SelectionUpdate { run, stage, player } if host && run == current => {
            if stage > d.local_stage {
                // A faster peer can arrive at the next screen before our first
                // Step there. Retain its latest update until local initialization.
                if stage == d.local_stage + 1 && (d.local_stage == 0 || d.committed_stage == d.local_stage) {
                    d.future.retain(|(id, _, _)| *id != from);
                    d.future.push((from, stage, player));
                }
                return;
            }
            if stage != d.state.stage || stage <= d.committed_stage { return; }
            let old = &d.state.players[from as usize];
            if player.version > old.version || !old.loaded {
                d.state.players[from as usize] = player;
                d.invalidate();
            }
        }
        Msg::SelectionState { run, state } if !host && from == 0 && run == current => {
            if state.stage > d.state.stage || (state.stage == d.state.stage && state.serial >= d.state.serial) {
                if !state.preparing || d.frozen != Some((state.stage, state.serial)) { d.frozen = None; }
                d.state = state;
            }
        }
        Msg::SelectionAck { run, stage, serial } if host && run == current => {
            if d.state.stage == stage && d.state.serial == serial && d.state.preparing { d.acked[from as usize] = true; }
        }
        Msg::SelectionCommit { run, stage, serial } if !host && from == 0 && run == current => {
            if d.frozen == Some((stage, serial)) && d.state.stage == stage && d.state.serial == serial && stage > d.committed_stage {
                d.committed_stage = stage;
                d.release = Some(stage);
            }
        }
        _ => {}
    }
}
pub fn tick(c: &mut Coop) {
    let Some(n) = &c.net else { return };
    let me = n.my_id as usize;
    let d = &mut c.selection;
    if run() == 0 || d.state.stage == 0 || d.state.stage <= d.committed_stage { return; }
    if n.role == Role::Host {
        if !d.state.preparing && !n.peers.is_empty() && n.peers.iter().all(|p| {
            let q = &d.state.players[p.id as usize]; q.loaded && q.ready
        }) {
            d.state.serial = d.state.serial.wrapping_add(1);
            d.state.preparing = true;
            d.acked = [false; 3];
            d.dirty = true;
        }
        if d.state.preparing && d.local_stage == d.state.stage && d.state.players[me] == d.local {
            d.frozen = Some((d.state.stage, d.state.serial));
            d.acked[me] = true;
        }
        if d.dirty {
            n.broadcast(Msg::SelectionState { run: run(), state: d.state.clone() });
            d.dirty = false;
        }
        if d.state.preparing && n.peers.iter().all(|p| d.acked[p.id as usize]) {
            n.broadcast(Msg::SelectionCommit { run: run(), stage: d.state.stage, serial: d.state.serial });
            d.committed_stage = d.state.stage;
            d.release = Some(d.state.stage);
            crate::log(&format!("selection: all players committed stage {} serial {}", d.state.stage, d.state.serial));
        }
    } else if d.state.preparing && d.local_stage == d.state.stage && d.frozen != Some((d.state.stage, d.state.serial)) {
        if d.local == d.state.players[me] && d.local.ready {
            // Freeze this draft before acknowledging, so a later input cannot
            // race the host's commit. Mismatched drafts stay editable.
            d.frozen = Some((d.state.stage, d.state.serial));
            n.broadcast(Msg::SelectionAck { run: run(), stage: d.state.stage, serial: d.state.serial });
        } else {
            n.broadcast(Msg::SelectionUpdate { run: run(), stage: d.local_stage, player: d.local.clone() });
        }
    }
}

pub fn install_hooks(g: G) {
    if let Some((_, addr)) = g.rt.scripts.iter().find(|(n, _)| *n == "gml_Script_draw@anon@3952@UI_card@Button") { DRAW_CARD.store(*addr, Ordering::Relaxed); }
    for (name, hook, slot) in [
        ("gml_Object_objSelectPerk_Step_0", perk_step as *const (), &PERK_STEP),
        ("gml_Object_objSelectGuns_Step_0", gun_step as *const (), &GUN_STEP),
        ("gml_Object_objSelectPerk_Draw_0", perk_draw as *const (), &PERK_DRAW),
        ("gml_Object_objSelectGuns_Draw_0", gun_draw as *const (), &GUN_DRAW),
        ("gml_Script_check_accept", accept as *const (), &ACCEPT),
        ("gml_Script_check_cancel", cancel as *const (), &CANCEL),
        ("gml_Script_InputPressed", pressed as *const (), &PRESSED),
        ("gml_Script_InputPressedMany", pressed_many as *const (), &PRESSED_MANY),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(p) => slot.store(p as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("selection: {e}")),
            }
        }
    }
}
fn choices(g: G, inst: V, stage: u8) -> [String; 2] {
    if stage == 1 {
        let i = g.get(inst, "perk1_i").as_f64();
        let names = g.get(inst, "perk_list");
        [i.filter(|i| *i >= 0.0 && *i < g.arr_len(names) as f64).map(|i| g.str_of(&g.arr_get(names, i as usize))).unwrap_or_default(), String::new()]
    } else {
        ["gun1_selected", "gun2_selected"].map(|k| {
            let row = g.get(inst, k);
            if !is_struct(&row) { return String::new(); }
            let i = g.num(row, "i") as usize;
            let names = g.get(inst, "gun_list");
            if i < g.arr_len(names) { g.str_of(&g.arr_get(names, i)) } else { String::new() }
        })
    }
}
fn initialize(g: G, c: &mut Coop, inst: V, stage: u8) {
    let host = c.net.as_ref().is_some_and(|n| n.role == Role::Host);
    let d = &mut c.selection;
    d.local_stage = stage;
    d.instance = Some(key(&inst));
    d.local = SelectionPlayer { version: 1, loaded: true, choices: choices(g, inst, stage), ready: false };
    d.frozen = None;
    if host {
        d.state = SelectionState { stage, ..Default::default() };
        d.acked = [false; 3];
        d.dirty = true;
    }
    if stage == 1 {
        let rows = g.get(inst, "rows");
        let previews = g.call("array_create", &[real(3.0)]);
        g.set(inst, "focmp_previews", previews);
        for i in 0..3 {
            let preview = super::menu::clone_ui_value(g, g.arr_get(rows, 0));
            g.call("array_set", &[previews, real(i as f64), preview]);
        }
    }
    let player = c.selection.local.clone();
    submit(c, Msg::SelectionUpdate { run: run(), stage, player });
    let future = std::mem::take(&mut c.selection.future);
    for (id, stage, player) in future { receive(c, id, Msg::SelectionUpdate { run: run(), stage, player }); }
}

unsafe fn step(s: *mut c_void, o: *mut c_void, stage: u8, slot: &AtomicUsize) {
    let original: EventFn = unsafe { std::mem::transmute(slot.load(Ordering::Relaxed)) };
    if run() == 0 { unsafe { original(s, o) }; return; }
    let Some(g) = G::new() else { return };
    let Some(inst) = (unsafe { g.rt.builtin_var_of("id", s) }) else { return };
    let release = {
        let Ok(mut c) = super::COOP.try_lock() else { return };
        if c.selection.instance != Some(key(&inst)) { initialize(g, &mut c, inst, stage); }
        let release = c.selection.release == Some(stage);
        if !release && (c.selection.frozen.is_some() || c.selection.applied_stage >= stage) { return; }
        release
    };
    BUTTON.store(key(&g.get(inst, "button")), Ordering::Relaxed);
    RELEASING.store(release, Ordering::Relaxed);
    READY_CLICK.store(false, Ordering::Relaxed);
    CANCEL_READY.store(false, Ordering::Relaxed);
    unsafe { original(s, o) };
    BUTTON.store(0, Ordering::Relaxed);
    RELEASING.store(false, Ordering::Relaxed);
    let Ok(mut c) = super::COOP.try_lock() else { return };
    if release {
        c.selection.release = None;
        c.selection.applied_stage = stage;
        crate::log(&format!("selection: native confirmation applied once for stage {stage}"));
        return;
    }
    let picked = choices(g, inst, stage);
    let all_loaded = c.selection.state.stage == stage && c.net.as_ref().is_some_and(|n| n.peers.iter().all(|p| c.selection.state.players[p.id as usize].loaded));
    let d = &mut c.selection;
    let changed = picked != d.local.choices;
    let old_ready = d.local.ready;
    if changed { d.local.choices = picked; d.local.ready = false; }
    if CANCEL_READY.swap(false, Ordering::Relaxed) { d.local.ready = false; }
    let valid = !d.local.choices[0].is_empty() && (stage == 1 || !d.local.choices[1].is_empty());
    if READY_CLICK.swap(false, Ordering::Relaxed) && valid && all_loaded && !changed { d.local.ready = !d.local.ready; }
    if changed || old_ready != d.local.ready {
        d.local.version = d.local.version.wrapping_add(1);
        let player = d.local.clone();
        submit(&mut c, Msg::SelectionUpdate { run: run(), stage, player });
    }
    let button = g.get(inst, "button");
    g.set(button, "text", g.s(if c.selection.local.ready { "UNREADY" } else { "READY" }));
}
unsafe extern "C-unwind" fn perk_step(s: *mut c_void, o: *mut c_void) { unsafe { step(s, o, 1, &PERK_STEP) } }
unsafe extern "C-unwind" fn gun_step(s: *mut c_void, o: *mut c_void) { unsafe { step(s, o, 2, &GUN_STEP) } }

unsafe extern "C-unwind" fn accept(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(ACCEPT.load(Ordering::Relaxed)) };
    let active = BUTTON.load(Ordering::Relaxed);
    let button = active != 0 && argc > 0 && !args.is_null() && !unsafe { *args }.is_null() && key(&unsafe { **args }) == active;
    if active != 0 && RELEASING.load(Ordering::Relaxed) {
        unsafe { *ret = boolean(button) }; return ret;
    }
    let result = unsafe { f(s, o, ret, argc, args) };
    if button {
        if !result.is_null() && truthy(&unsafe { *result }) { READY_CLICK.store(true, Ordering::Relaxed); }
        unsafe { *ret = boolean(false) }; return ret;
    }
    result
}
unsafe extern "C-unwind" fn cancel(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(CANCEL.load(Ordering::Relaxed)) };
    if BUTTON.load(Ordering::Relaxed) == 0 { return unsafe { f(s, o, ret, argc, args) }; }
    if !RELEASING.load(Ordering::Relaxed) {
        let result = unsafe { f(s, o, ret, argc, args) };
        if !result.is_null() && truthy(&unsafe { *result }) { CANCEL_READY.store(true, Ordering::Relaxed); }
    }
    // A player's Back key must not take just that player to an earlier stage.
    // While selecting, it cancels their readiness instead.
    unsafe { *ret = boolean(false) }; ret
}
unsafe extern "C-unwind" fn pressed(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    if RELEASING.load(Ordering::Relaxed) || super::pause::block_player_input() { unsafe { *ret = boolean(false) }; return ret; }
    let f: ScriptFn = unsafe { std::mem::transmute(PRESSED.load(Ordering::Relaxed)) };
    let result = unsafe { f(s, o, ret, argc, args) };
    super::computer::filter_input(result);
    unsafe {super::props::filter_input(s,argc,args,result);}
    result
}
unsafe extern "C-unwind" fn pressed_many(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    if RELEASING.load(Ordering::Relaxed) || super::pause::block_player_input() { unsafe { *ret = boolean(false) }; return ret; }
    let f: ScriptFn = unsafe { std::mem::transmute(PRESSED_MANY.load(Ordering::Relaxed)) };
    unsafe { f(s, o, ret, argc, args) }
}

unsafe fn draw(s: *mut c_void, o: *mut c_void, stage: u8, slot: &AtomicUsize) {
    let f: EventFn = unsafe { std::mem::transmute(slot.load(Ordering::Relaxed)) };
    DRAWING_PERKS.store(run() != 0 && stage == 1, Ordering::Relaxed);
    unsafe { f(s, o) };
    DRAWING_PERKS.store(false, Ordering::Relaxed);
    if run() == 0 { return; }
    let Some(g) = G::new() else { return };
    let Ok(c) = super::COOP.try_lock() else { return };
    if let Some(inst) = unsafe { g.rt.builtin_var_of("id", s) } {
        if stage == 1 && c.selection.instance == Some(key(&inst)) { previews(g, &c, inst, o); }
    }
    super::cursors::draw(g, &c);
}
unsafe extern "C-unwind" fn perk_draw(s: *mut c_void, o: *mut c_void) { unsafe { draw(s, o, 1, &PERK_DRAW) } }
unsafe extern "C-unwind" fn gun_draw(s: *mut c_void, o: *mut c_void) { unsafe { draw(s, o, 2, &GUN_DRAW) } }

fn previews(g: G, c: &Coop, inst: V, other: *mut c_void) {
    let rows = g.get(inst, "rows");
    let names = g.get(inst, "perk_list");
    let previews = g.get(inst, "focmp_previews");
    let address = DRAW_CARD.load(Ordering::Relaxed);
    if address == 0 { return; }
    let f: ScriptFn = unsafe { std::mem::transmute(address) };
    let saved = [g.call("draw_get_color", &[]), g.call("draw_get_alpha", &[]), g.call("draw_get_font", &[]), g.call("draw_get_halign", &[]), g.call("draw_get_valign", &[])];
    for (slot, (id, name, me)) in c.roster().into_iter().enumerate() {
        let state = if me { &c.selection.local } else { &c.selection.state.players[id as usize] };
        let found = (0..g.arr_len(names)).find(|i| g.str_of(&g.arr_get(names, *i)) == state.choices[0]);
        let row = g.arr_get(previews, slot);
        let source = g.arr_get(rows, found.unwrap_or(0));
        let x = 394.0 + slot as f64 * 78.0;
        let y = if state.ready { 188.0 } else { 208.0 };
        g.set(row, "card", g.get(source, "card"));
        for (k, v) in [("x0", x), ("x1", x + 42.0), ("y0", y), ("y1", y + 64.0), ("xcardinfo", x + 21.0), ("ycardinfo", y + 69.0)] { g.set(row, k, real(v)); }
        g.set(row, "selected", boolean(state.ready));
        g.set(row, "_active", boolean(found.is_some()));
        g.set(row, "discovered", boolean(found.is_some()));
        g.set(row, "_hoover", boolean(false));
        g.set(row, "visible", boolean(true));
        let mut ret = V::undefined();
        unsafe { f(row.v as *mut c_void, other, &mut ret, 0, std::ptr::null_mut()) };
        g.call("draw_set_alpha", &[real(1.0)]);
        g.call("draw_set_font", &[g.global("Font_main_2x")]);
        g.call("draw_set_halign", &[real(1.0)]); g.call("draw_set_valign", &[real(0.0)]);
        g.call("draw_set_color", &[real(super::colors::value(c.colors[id as usize]))]);
        let label: String = name.chars().filter(|c| !c.is_control()).take(11).collect();
        g.call("draw_text", &[real(x + 21.0), real(279.0), g.s(&label)]);
    }
    for (name, value) in ["draw_set_color", "draw_set_alpha", "draw_set_font", "draw_set_halign", "draw_set_valign"].into_iter().zip(saved) { g.call(name, &[value]); }
}
