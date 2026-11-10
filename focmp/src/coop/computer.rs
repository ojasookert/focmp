//! Shared terminal actions: one press skips text, a subsequent press continues.
//! Requests carry the displayed revision so simultaneous presses cannot do both.
use super::{Coop, rooms};
use crate::engine::{G, V, boolean, key, truthy};
use crate::hooks::ScriptFn;
use crate::net::{Msg, PeerId, Role};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

type EventFn = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);
static STEP: AtomicUsize = AtomicUsize::new(0);
static NEXT: AtomicUsize = AtomicUsize::new(0);
static DOUBLE: AtomicUsize = AtomicUsize::new(0);
static IN_STEP: AtomicBool = AtomicBool::new(false);
static REQUEST: AtomicBool = AtomicBool::new(false);

#[derive(Default, Debug)]
pub struct Computer {
    instance: Option<u64>,
    visit: u32,
    entered: [u32; 3],
    revision: u32,
    pending: Option<bool>,
    continued: bool,
}
fn run() -> u64 { rooms::RUN_SEED.load(Ordering::Relaxed) }
fn submit(c: &mut Coop, g: G, msg: Msg) {
    let Some(n) = &c.net else { return };
    if n.role == Role::Host { receive(c, g, 0, msg); } else { n.broadcast(msg); }
}
pub fn receive(c: &mut Coop, g: G, from: PeerId, msg: Msg) {
    let Some(n) = &c.net else { return };
    if run() == 0 || !n.peers.iter().any(|p| p.id == from) { return; }
    let d = &mut c.computer;
    match msg {
        Msg::ComputerEntered { run: r, visit } if r == run() && n.role == Role::Host => {
            if visit >= d.entered[from as usize] && visit <= d.visit + 1 { d.entered[from as usize] = visit; }
        }
        Msg::ComputerRequest { run: r, visit, revision, proceed } if r == run() && n.role == Role::Host => {
            if visit != d.visit || revision != d.revision || d.pending.is_some() || d.continued
                || !n.peers.iter().all(|p| d.entered[p.id as usize] == visit) { return; }
            let Some(inst) = g.first("objComputer") else { return };
            let proceed = proceed && truthy(&g.get(inst, "draw_done"));
            d.revision += 1;
            d.pending = Some(proceed);
            n.broadcast(Msg::ComputerAction { run: r, visit, revision: d.revision, proceed });
        }
        Msg::ComputerAction { run: r, visit, revision, proceed } if r == run() && from == 0 && n.role == Role::Client => {
            if visit == d.visit && revision == d.revision + 1 && !d.continued {
                d.revision = revision;
                d.pending = Some(proceed);
            }
        }
        _ => {}
    }
}

pub fn install_hooks(g: G) {
    for (name, hook, slot) in [
        ("gml_Object_objComputer_Step_0", step as *const (), &STEP),
        ("gml_Script_anon@873@gml_Object_objComputer_Create_0", next as *const (), &NEXT),
        ("gml_Script_InputPressedDouble", double as *const (), &DOUBLE),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(p) => slot.store(p as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("computer: {e}")),
            }
        }
    }
}

/// Called by the shared InputPressed hook, only during the terminal's Step.
pub fn filter_input(result: *mut V) {
    if IN_STEP.load(Ordering::Relaxed) && !result.is_null() {
        if truthy(&unsafe { *result }) { REQUEST.store(true, Ordering::Relaxed); }
        unsafe { *result = boolean(false) };
    }
}
unsafe extern "C-unwind" fn double(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(DOUBLE.load(Ordering::Relaxed)) };
    let result = unsafe { f(s, o, ret, argc, args) };
    filter_input(result);
    result
}
unsafe extern "C-unwind" fn next(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    if run() != 0 && IN_STEP.load(Ordering::Relaxed) {
        REQUEST.store(true, Ordering::Relaxed);
        unsafe { *ret = V::undefined() }; return ret;
    }
    let f: ScriptFn = unsafe { std::mem::transmute(NEXT.load(Ordering::Relaxed)) };
    unsafe { f(s, o, ret, argc, args) }
}
unsafe extern "C-unwind" fn step(s: *mut c_void, o: *mut c_void) {
    let f: EventFn = unsafe { std::mem::transmute(STEP.load(Ordering::Relaxed)) };
    if run() == 0 { unsafe { f(s, o) }; return; }
    let Some(g) = G::new() else { return };
    if g.room_name() != "Room_computer" {
        if let Ok(mut c) = super::COOP.try_lock() { c.computer.instance = None; }
        unsafe { f(s, o) }; return;
    }
    let Some(inst) = (unsafe { g.rt.builtin_var_of("id", s) }) else { return };
    let action = {
        let Ok(mut c) = super::COOP.try_lock() else { return };
        if c.computer.instance != Some(key(&inst)) {
            c.computer.instance = Some(key(&inst));
            c.computer.visit += 1;
            c.computer.revision = 0;
            c.computer.pending = None;
            c.computer.continued = false;
            let visit = c.computer.visit;
            submit(&mut c, g, Msg::ComputerEntered { run: run(), visit });
        }
        if c.computer.continued { return; }
        let action = c.computer.pending.take();
        if action == Some(true) { c.computer.continued = true; }
        action
    };
    if let Some(proceed) = action {
        crate::log(&format!("computer: shared {}", if proceed { "continue" } else { "skip" }));
        if proceed {
            // The native method advances level data and chooses the next room.
            // Run once per peer in its own valid event context, with the same RNG.
            rooms::reseed(4);
            let next: ScriptFn = unsafe { std::mem::transmute(NEXT.load(Ordering::Relaxed)) };
            let mut ret = V::undefined();
            let mut arg = boolean(false);
            let mut args = [&mut arg as *mut V];
            unsafe { next(s, o, &mut ret, 1, args.as_mut_ptr()) };
            return;
        }
        // This is exactly the native skip operation; Draw then renders all text.
        g.set(inst, "draw_done", boolean(true));
    }
    REQUEST.store(false, Ordering::Relaxed);
    IN_STEP.store(true, Ordering::Relaxed);
    unsafe { f(s, o) };
    IN_STEP.store(false, Ordering::Relaxed);
    if REQUEST.swap(false, Ordering::Relaxed) && action.is_none() {
        let Ok(mut c) = super::COOP.try_lock() else { return };
        let (visit, revision) = (c.computer.visit, c.computer.revision);
        submit(&mut c, g, Msg::ComputerRequest { run: run(), visit, revision, proceed: truthy(&g.get(inst, "draw_done")) });
    }
}
