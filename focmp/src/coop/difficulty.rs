//! Separate co-op difficulty panel, backed by an engine-rooted clone of the
//! native cards. The host orders draft edits; every edit invalidates all votes.
//! A peer can confirm only the revision it saw. No host bypass exists.
use super::{Coop, rooms};
use crate::engine::{G, V, boolean, is_struct, key, real, truthy};
use crate::net::{DifficultySelection, Msg, PeerId, Role};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const NAMES: [&str; 7] = ["ActualBoss", "ExtraEnemies", "FastEnemies", "LineOfSight", "BrutalEnemies", "NoHPRefill", "DoubleBoss"];
static ORIG_STEP: AtomicUsize = AtomicUsize::new(0);
static ORIG_DRAW: AtomicUsize = AtomicUsize::new(0);
static ORIG_GOTO: AtomicUsize = AtomicUsize::new(0);
static ORIG_SCRIBBLE: AtomicUsize = AtomicUsize::new(0);
static SELECT_UP: AtomicUsize = AtomicUsize::new(0);
static DESELECT: AtomicUsize = AtomicUsize::new(0);
static DRAWING_DIFFICULTY: AtomicBool = AtomicBool::new(false);
static READY_REQUEST: AtomicBool = AtomicBool::new(false);
type EventFn = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);

#[derive(Default)]
pub struct Difficulty {
    instance: Option<u64>,
    pub state: DifficultySelection,
    have_state: bool,
    unlocked: [u8; 3],
    dirty: bool,
    committed: bool,
    pending_commit: Option<u8>,
    applied: bool,
}

impl Difficulty {
    fn changed(&mut self) {
        self.state.revision = self.state.revision.wrapping_add(1);
        self.state.agreed = [false; 3];
        self.dirty = true;
    }
    pub fn member_left(&mut self, id: PeerId) {
        if self.committed || !self.have_state { return; }
        self.state.loaded[id as usize] = false;
        self.unlocked[id as usize] = 0;
        self.state.allowed = self.unlocked.iter().fold(0, |a, b| a | b);
        self.state.mask &= self.state.allowed;
        self.changed();
    }
    pub fn describe(&self) -> String {
        format!("difficulty: revision {} mask {:07b} allowed {:07b} loaded {:?} agreed {:?} committed {} applied {}", self.state.revision, self.state.mask, self.state.allowed, self.state.loaded, self.state.agreed, self.committed, self.applied)
    }
}

pub fn install_hooks(g: G) {
    for (name, slot) in [
        ("gml_Script_select_up@anon@6017@UI_card@Button", &SELECT_UP),
        ("gml_Script_deselect@anon@6616@UI_card@Button", &DESELECT),
    ] {
        if let Some((_, address)) = g.rt.scripts.iter().find(|(n, _)| *n == name) {
            slot.store(*address, Ordering::Relaxed);
        }
    }
    for (name, hook, slot) in [
        ("room_goto", hk_goto as *const (), &ORIG_GOTO),
        ("gml_Object_objSelectDifficulty_Step_0", hk_step as *const (), &ORIG_STEP),
        ("gml_Object_objSelectDifficulty_Draw_0", hk_draw as *const (), &ORIG_DRAW),
        ("gml_Script_scribble", hk_scribble as *const (), &ORIG_SCRIBBLE),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(p) => slot.store(p as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("difficulty: {e}")),
            }
        }
    }
}

fn run() -> u64 { rooms::RUN_SEED.load(Ordering::Relaxed) }
fn all_loaded(c: &Coop) -> bool {
    c.difficulty.have_state && c.net.as_ref().is_some_and(|n| !n.peers.is_empty() && n.peers.iter().all(|p| c.difficulty.state.loaded[p.id as usize]))
}
fn submit(c: &mut Coop, g: G, msg: Msg) {
    let Some(n) = &c.net else { return };
    if n.role == Role::Host { receive(c, g, 0, msg); } else { n.broadcast(msg); }
}

pub fn receive(c: &mut Coop, _g: G, from: PeerId, msg: Msg) {
    let Some(n) = &c.net else { return };
    if !n.peers.iter().any(|p| p.id == from) { return; }
    let host = n.role == Role::Host;
    let d = &mut c.difficulty;
    let current = run();
    if current == 0 { return; }
    match msg {
        Msg::DifficultyLoaded { run, unlocked, initial } if host && run == current && !d.committed => {
            if !d.state.loaded[from as usize] || d.unlocked[from as usize] != unlocked {
                d.unlocked[from as usize] = unlocked;
                d.state.loaded[from as usize] = true;
                d.state.allowed = d.unlocked.iter().fold(0, |a, b| a | b);
                if from == 0 { d.state.mask = initial; }
                d.state.mask &= d.state.allowed;
                d.have_state = true;
                d.changed();
            }
        }
        Msg::DifficultyEdit { run, revision, index, enabled } if host && run == current && !d.committed => {
            if revision == d.state.revision && d.state.loaded[from as usize] && d.state.allowed & (1 << index) != 0 {
                let mask = if enabled { d.state.mask | (1 << index) } else { d.state.mask & !(1 << index) };
                if mask != d.state.mask { d.state.mask = mask; d.changed(); }
            } else { d.dirty = true; } // send the current draft after a stale edit
        }
        Msg::DifficultyAgree { run, revision, agree } if host && run == current && !d.committed => {
            if revision == d.state.revision && n.peers.iter().all(|p| d.state.loaded[p.id as usize]) {
                d.state.agreed[from as usize] = agree;
            }
            d.dirty = true;
        }
        Msg::DifficultyState { run, state } if !host && from == 0 && run == current && !d.committed => {
            if !d.have_state || state.revision >= d.state.revision { d.state = state; d.have_state = true; }
        }
        Msg::DifficultyCommit { run, revision, mask } if !host && from == 0 && run == current && !d.committed => {
            if d.have_state && d.state.revision == revision && d.state.mask == mask
                && n.peers.iter().all(|p| d.state.loaded[p.id as usize] && d.state.agreed[p.id as usize]) {
                d.committed = true;
                d.pending_commit = Some(mask);
            }
        }
        _ => {}
    }
}

/// Network housekeeping stays in the pump; engine transitions happen in Step.
pub fn tick(_g: G, c: &mut Coop) {
    let Some(n) = &c.net else { return };
    let d = &mut c.difficulty;
    if n.role != Role::Host || !d.have_state || d.committed || run() == 0 { return; }
    if d.dirty {
        n.broadcast(Msg::DifficultyState { run: run(), state: d.state.clone() });
        d.dirty = false;
    }
    if !n.peers.is_empty() && n.peers.iter().all(|p| d.state.loaded[p.id as usize] && d.state.agreed[p.id as usize]) {
        n.broadcast(Msg::DifficultyCommit { run: run(), revision: d.state.revision, mask: d.state.mask });
        d.committed = true;
        d.pending_commit = Some(d.state.mask);
        crate::log(&format!("difficulty: unanimous agreement at revision {}, mask {:07b}", d.state.revision, d.state.mask));
    }
}

fn initialize(g: G, c: &mut Coop, inst: V) {
    // Preserve the native card layout, tooltips, animation and input. A deep
    // clone keeps the co-op controls separate from single-player UI data.
    let native = g.get(inst, "rows");
    g.set(inst, "focmp_native_rows", native);
    let rows = super::menu::clone_ui_value(g, native);
    g.set(inst, "focmp_difficulty_rows", rows);
    g.set(inst, "rows", rows);
    g.set(g.get(g.get(g.arr_get(rows, 0), "card"), "infotxt"), "string_title", g.s("\nReady"));
    let mut unlocked = 0;
    let mut initial = 0;
    for i in 0..NAMES.len() {
        let row = g.arr_get(rows, i + 1);
        if truthy(&g.get(row, "_active")) { unlocked |= 1 << i; }
        if truthy(&g.get(row, "selected")) { initial |= 1 << i; }
    }
    c.difficulty.instance = Some(key(&inst));
    submit(c, g, Msg::DifficultyLoaded { run: run(), unlocked, initial });
    crate::log(&format!("difficulty: local unlocked mask {unlocked:07b}"));
}

/// Only the native START card's transition is intercepted. The native Step
/// still handles all card interaction and writes its normal difficulty settings.
unsafe extern "C-unwind" fn hk_goto(ret: *mut V, s: *mut c_void, o: *mut c_void, argc: i32, args: *mut V) {
    if run() != 0 && argc > 0 && !args.is_null() {
        if let Some(g) = G::new() {
            if g.room_name() == "Room_select_difficulty" && g.str_of(&g.call("room_get_name", &[unsafe { *args }])) == "Room_select_perk" {
                READY_REQUEST.store(true, Ordering::Relaxed);
                if !ret.is_null() { unsafe { *ret = V::undefined() }; }
                return;
            }
        }
    }
    let f: crate::gm::BuiltinFn = unsafe { std::mem::transmute(ORIG_GOTO.load(Ordering::Relaxed)) };
    unsafe { f(ret, s, o, argc, args) }
}

fn set_selected(g: G, row: V, selected: bool, other: *mut c_void) {
    if truthy(&g.get(row, "selected")) == selected { return; }
    // Native selection updates the card position and animation as well as its
    // flag. Apply it on every peer, using the cloned row as the method owner.
    let address = if selected { &SELECT_UP } else { &DESELECT }.load(Ordering::Relaxed);
    if address == 0 { return; }
    let f: crate::hooks::ScriptFn = unsafe { std::mem::transmute(address) };
    let mut ret = V::undefined();
    unsafe { f(row.v as *mut c_void, other, &mut ret, 0, std::ptr::null_mut()) };
}

fn synchronize_cards(g: G, c: &Coop, inst: V, other: *mut c_void) {
    if !c.difficulty.have_state { return; }
    let rows = g.get(inst, "rows");
    for i in 0..NAMES.len() {
        let row = g.arr_get(rows, i + 1);
        let allowed = boolean(c.difficulty.state.allowed & (1 << i) != 0);
        g.set(row, "_active", allowed);
        g.set(row, "discovered", allowed);
        set_selected(g, row, c.difficulty.state.mask & (1 << i) != 0, other);
    }
    let start = g.arr_get(rows, 0);
    g.set(start, "_active", boolean(all_loaded(c) && !c.difficulty.committed));
    let me = c.net.as_ref().map_or(0, |n| n.my_id as usize);
    set_selected(g, start, c.difficulty.state.agreed[me], other);
}

fn selected_mask(g: G, inst: V) -> u8 {
    let rows = g.get(inst, "rows");
    (0..NAMES.len()).fold(0, |mask, i| mask | ((truthy(&g.get(g.arr_get(rows, i + 1), "selected")) as u8) << i))
}

unsafe extern "C-unwind" fn hk_step(s: *mut c_void, o: *mut c_void) {
    let original: EventFn = unsafe { std::mem::transmute(ORIG_STEP.load(Ordering::Relaxed)) };
    if run() == 0 { unsafe { original(s, o) }; return; }
    let Some(g) = G::new() else { return };
    let Some(inst) = (unsafe { g.rt.builtin_var_of("id", s) }) else { return };
    let (before, revision) = {
        let Ok(mut c) = super::COOP.try_lock() else { return };
        if c.difficulty.instance != Some(key(&inst)) { initialize(g, &mut c, inst); }
        synchronize_cards(g, &c, inst, o);
        if let Some(mask) = c.difficulty.pending_commit.take() {
            apply_selection(g, inst, mask, s, o);
            c.difficulty.applied = true;
            return;
        }
        if c.difficulty.committed { return; }
        (selected_mask(g, inst), c.difficulty.state.revision)
    };
    READY_REQUEST.store(false, Ordering::Relaxed);
    unsafe { original(s, o) };
    let after = selected_mask(g, inst);
    let Ok(mut c) = super::COOP.try_lock() else { return };
    for i in 0..NAMES.len() {
        if (before ^ after) & (1 << i) != 0 {
            submit(&mut c, g, Msg::DifficultyEdit { run: run(), revision, index: i as u8, enabled: after & (1 << i) != 0 });
        }
    }
    if READY_REQUEST.swap(false, Ordering::Relaxed) && all_loaded(&c) {
        let me = c.net.as_ref().map_or(0, |n| n.my_id as usize);
        let agree = !c.difficulty.state.agreed[me];
        submit(&mut c, g, Msg::DifficultyAgree { run: run(), revision, agree });
    }
}

fn apply_selection(g: G, inst: V, mask: u8, s: *mut c_void, o: *mut c_void) {
    let global = g.global("difficulty");
    let rows = g.get(inst, "rows");
    for (i, name) in NAMES.iter().enumerate() {
        let on = boolean(mask & (1 << i) != 0);
        let card = g.get(global, name);
        if is_struct(&card) { g.set(card, "activated", on); }
        g.set(g.get(g.arr_get(rows, i + 1), "card"), "activated", on);
    }
    // Everyone has already used native START for this revision, including its
    // settings update. Release the blocked room transition exactly once.
    let mut room = g.get(inst, "difficulty_room_next");
    let mut ret = V::undefined();
    let f: crate::gm::BuiltinFn = unsafe { std::mem::transmute(ORIG_GOTO.load(Ordering::Relaxed)) };
    unsafe { f(&mut ret, s, o, 1, &mut room) };
    crate::log(&format!("difficulty: applied agreed mask {mask:07b}, moving to perk selection"));
}

unsafe extern "C-unwind" fn hk_draw(s: *mut c_void, o: *mut c_void) {
    let f: EventFn = unsafe { std::mem::transmute(ORIG_DRAW.load(Ordering::Relaxed)) };
    DRAWING_DIFFICULTY.store(run() != 0, Ordering::Relaxed);
    unsafe { f(s, o) };
    DRAWING_DIFFICULTY.store(false, Ordering::Relaxed);
    if run() != 0 {
        if let Some(g) = G::new() { if let Ok(c) = super::COOP.try_lock() {
            if let Some(inst) = unsafe { g.rt.builtin_var_of("id", s) } { draw(g, &c, inst); }
        } }
    }
}

unsafe extern "C-unwind" fn hk_scribble(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    let f: crate::hooks::ScriptFn = unsafe { std::mem::transmute(ORIG_SCRIBBLE.load(Ordering::Relaxed)) };
    if (DRAWING_DIFFICULTY.load(Ordering::Relaxed) || super::selection::DRAWING_PERKS.load(Ordering::Relaxed)) && argc > 0 && !args.is_null() {
        for i in 0..argc as usize {
            let arg = unsafe { *args.add(i) };
            let text = if arg.is_null() { None } else { unsafe { (*arg).as_str() } };
            let hide = (DRAWING_DIFFICULTY.load(Ordering::Relaxed) && text.as_deref() == Some("[fa_center]Select your difficulty modifiers"))
                || (super::selection::DRAWING_PERKS.load(Ordering::Relaxed) && text.as_deref() == Some("[fa_center]Select a starting perk"));
            if hide {
                if let Some(g) = G::new() {
                    let mut blank = g.s("");
                    let mut copied = unsafe { std::slice::from_raw_parts(args, argc as usize) }.to_vec();
                    copied[i] = &mut blank;
                    return unsafe { f(s, o, ret, argc, copied.as_mut_ptr()) };
                }
            }
        }
    }
    unsafe { f(s, o, ret, argc, args) }
}

fn draw(g: G, c: &Coop, inst: V) {
    let saved = [g.call("draw_get_color", &[]), g.call("draw_get_alpha", &[]), g.call("draw_get_font", &[]), g.call("draw_get_halign", &[]), g.call("draw_get_valign", &[])];
    g.call("draw_set_alpha", &[real(1.0)]); g.call("draw_set_font", &[g.global("Font_main_2x")]);
    g.call("draw_set_halign", &[real(0.0)]); g.call("draw_set_valign", &[real(1.0)]);
    let d = &c.difficulty;
    let me = c.net.as_ref().map_or(0, |n| n.my_id as usize);
    if d.state.agreed[me] {
        // Native Draw omits the selected label for Start. Match the modifier
        // label's main font and transform, keeping READY below this card.
        let start = g.arr_get(g.get(inst, "rows"), 0);
        g.call("draw_set_font", &[g.global("Font_main")]);
        g.call("draw_set_halign", &[real(1.0)]);
        g.call("draw_set_valign", &[real(0.0)]);
        g.call("draw_set_color", &[real(16777215.0)]);
        g.call("draw_text_transformed", &[real(g.num(start, "x0") + 21.0), real(g.num(start, "y1") + 2.0), g.s("READY"), real(1.0), real(2.0), real(0.0)]);
        g.call("draw_set_font", &[g.global("Font_main_2x")]);
        g.call("draw_set_halign", &[real(0.0)]);
        g.call("draw_set_valign", &[real(1.0)]);
    }
    for (name, value) in ["draw_set_color", "draw_set_alpha", "draw_set_font", "draw_set_halign", "draw_set_valign"].into_iter().zip(saved) { g.call(name, &[value]); }
}
