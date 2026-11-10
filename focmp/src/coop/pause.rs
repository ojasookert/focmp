//! Multiplayer ESC menu: native menu UI, live world, local gameplay input blocked.
use crate::engine::{G, V, boolean, real};
use crate::hooks::ScriptFn;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
type Event = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);
static CREATE: AtomicUsize = AtomicUsize::new(0);
static STEP: AtomicUsize = AtomicUsize::new(0);
static DESTROY: AtomicUsize = AtomicUsize::new(0);
static MOUSE_POSITION: AtomicUsize = AtomicUsize::new(0);
static DEACTIVATE: AtomicUsize = AtomicUsize::new(0);
static BACKGROUND: AtomicUsize = AtomicUsize::new(0);
static DRAW_GUI: AtomicUsize = AtomicUsize::new(0);
static RECTANGLE: AtomicUsize = AtomicUsize::new(0);
static DIM_BACKGROUND: AtomicBool = AtomicBool::new(false);
static DIRECTION: AtomicUsize = AtomicUsize::new(0);
static DISTANCE: AtomicUsize = AtomicUsize::new(0);
static MOUSE_LOCK: AtomicUsize = AtomicUsize::new(0);
static CREATING: AtomicBool = AtomicBool::new(false);
static BLOCK_PLAYER: AtomicBool = AtomicBool::new(false);
fn multiplayer() -> bool {
    super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0
}
pub fn open(g: G) -> bool {
    multiplayer() && g.first("objPause").is_some()
}
pub fn block_player_input() -> bool {
    BLOCK_PLAYER.load(Ordering::Relaxed)
}
pub fn begin_player(g: G) {
    BLOCK_PLAYER.store(open(g) || super::fuzz::active(), Ordering::Relaxed);
}
pub fn block_for_step() { BLOCK_PLAYER.store(true, Ordering::Relaxed); }
pub fn end_player() {
    BLOCK_PLAYER.store(false, Ordering::Relaxed);
}
pub fn install(g: G) {
    for (name, f, slot) in [
        ("gml_Object_objPause_Create_0", create as *const (), &CREATE),
        ("gml_Object_objPause_Destroy_0", destroy as *const (), &DESTROY),
        ("gml_Object_objPause_Step_0", step as *const (), &STEP),
        ("gml_Script_get_mouse_pos_game", mouse_position as *const (), &MOUSE_POSITION),
        ("gml_Script_instance_deactivate_all_hook", deactivate as *const (), &DEACTIVATE),
        ("gml_Object_objPause_Draw_74", background as *const (), &BACKGROUND),
        ("gml_Object_objPause_Draw_64", draw_gui as *const (), &DRAW_GUI),
        ("draw_rectangle_colour", rectangle as *const (), &RECTANGLE),
        ("gml_Script_InputDirection", direction as *const (), &DIRECTION),
        ("gml_Script_InputDistance", distance as *const (), &DISTANCE),
        ("gml_Script_set_mouse_lock", mouse_lock as *const (), &MOUSE_LOCK),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, f) {
                Ok(p) => slot.store(p as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("coop escape: {e}")),
            }
        }
    }
}
// Single player deactivates Player/Inventory while paused. Multiplayer keeps
// them alive, and their lock requests can override Pause.Step's unlock in the
// same frame. Let the native pause menu own the mouse until it is destroyed.
unsafe extern "C-unwind" fn mouse_lock(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(MOUSE_LOCK.load(Ordering::Relaxed)) };
    if multiplayer() && G::new().is_some_and(open) {
        let mut unlock = boolean(false);
        let mut args = [&mut unlock as *mut V];
        return unsafe { f(s, o, r, 1, args.as_mut_ptr()) };
    }
    unsafe { f(s, o, r, n, a) }
}
unsafe extern "C-unwind" fn create(s: *mut c_void, o: *mut c_void) {
    let f: Event = unsafe { std::mem::transmute(CREATE.load(Ordering::Relaxed)) };
    let previous_cursor = G::new().filter(|_| multiplayer())
        .map(|g| g.call("window_get_cursor", &[]));
    CREATING.store(multiplayer(), Ordering::Relaxed);
    unsafe { f(s, o) };
    CREATING.store(false, Ordering::Relaxed);
    if multiplayer() {
        if let Some(g) = G::new() {
            if let Some(menu) = unsafe { g.rt.builtin_var_of("id", s) } {
                if let Some(cursor) = previous_cursor {
                    g.set(menu, "focmp_previous_cursor", cursor);
                }
                // Native Pause.Create hides both the system cursor and cursor_sprite.
                // Use the standard arrow while this local menu owns the mouse.
                g.call("window_set_cursor", &[real(-2.0)]); // cr_arrow
                if let Some(player) = g.first("objPlayer") {
                    // Absolute mouse mode aliases global.game_cursor. Detach it
                    // once, otherwise objMouse can move our saved aim in place.
                    let aim = g.get(player, "cursor_pos");
                    let frozen = g.call("variable_clone", &[aim]);
                    if frozen.kind == crate::gm::KIND_OBJECT {
                        g.set(player, "cursor_pos", frozen);
                    }
                }
                // The native pause background is a still image of the room. Keep
                // its lifetime managed by the menu but display the live room instead.
                let bg = g.get(menu, "background");
                if g.exists(bg) {
                    g.set(bg, "visible", boolean(false));
                }
                foreground_menu(g, menu);
                crate::log("coop escape: opened native menu without deactivating the world");
            }
        }
    }
}
// Pause and its page widgets draw into the same native UI surface as the live
// HUD. Keep their original relative ordering, but in a foreground depth band.
// The single-player game avoids this overlap by deactivating the world.
fn foreground(g: G, instance: V) {
    if !g.exists(instance) { return; }
    let original = match g.get(instance, "focmp_pause_depth").as_f64() {
        Some(depth) => depth,
        None => {
            let depth = g.num(instance, "depth");
            g.set(instance, "focmp_pause_depth", real(depth));
            depth
        }
    };
    let depth = original - 1_000_000.0;
    if g.num(instance, "depth") != depth { g.set(instance, "depth", real(depth)); }
}
fn foreground_menu(g: G, menu: V) {
    foreground(g, menu);
    let pages = g.get(menu, "pages");
    if !crate::engine::is_struct(&pages) { return; }
    super::controls::extend_menu(g, pages);
    let active = g.str_of(&g.get(g.global("page"), "active"));
    let page = g.get(pages, &active);
    if !crate::engine::is_struct(&page) { return; }
    let objects = g.get(page, "objects");
    if !crate::engine::truthy(&g.call("is_array", &[objects])) { return; }
    for i in 0..g.arr_len(objects) {
        foreground(g, g.get(g.arr_get(objects, i), "instance"));
    }
}
unsafe extern "C-unwind" fn step(s: *mut c_void, o: *mut c_void) {
    let f: Event = unsafe { std::mem::transmute(STEP.load(Ordering::Relaxed)) };
    let menu = G::new().filter(|_| multiplayer()).and_then(|g| {
        unsafe { g.rt.builtin_var_of("id", s) }.map(|menu| (g, menu))
    });
    unsafe { f(s, o) };
    // Continue/Stop can destroy this instance during the native Step.
    if let Some((g, menu)) = menu.filter(|(g, menu)| g.exists(*menu)) {
        foreground_menu(g, menu);
    }
}
unsafe extern "C-unwind" fn destroy(s: *mut c_void, o: *mut c_void) {
    let f: Event = unsafe { std::mem::transmute(DESTROY.load(Ordering::Relaxed)) };
    let saved = G::new().and_then(|g| {
        let menu = unsafe { g.rt.builtin_var_of("id", s) }?;
        g.get(menu, "focmp_previous_cursor").as_f64().map(|cursor| (g, cursor))
    });
    unsafe { f(s, o) };
    if let Some((g, cursor)) = saved {
        g.call("window_set_cursor", &[real(cursor)]);
    }
}
unsafe extern "C-unwind" fn mouse_position(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    // Player.Step passes its previous cursor_pos as argument 0. Preserve that
    // aim target before native body/weapon/light updates consume it. Menu mouse
    // reads outside Player.Step still run normally, including relative mode.
    if block_player_input() && n > 0 && !a.is_null() && unsafe { !(*a).is_null() } {
        unsafe { *r = **a };
        return r;
    }
    let f: ScriptFn = unsafe { std::mem::transmute(MOUSE_POSITION.load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}
unsafe extern "C-unwind" fn deactivate(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    if CREATING.load(Ordering::Relaxed) {
        unsafe { *r = V::undefined() };
        return r;
    }
    let f: ScriptFn = unsafe { std::mem::transmute(DEACTIVATE.load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}
unsafe extern "C-unwind" fn background(s: *mut c_void, o: *mut c_void) {
    if multiplayer() {
        return;
    }
    let f: Event = unsafe { std::mem::transmute(BACKGROUND.load(Ordering::Relaxed)) };
    unsafe { f(s, o) };
}
unsafe extern "C-unwind" fn draw_gui(s: *mut c_void, o: *mut c_void) {
    let f: Event = unsafe { std::mem::transmute(DRAW_GUI.load(Ordering::Relaxed)) };
    DIM_BACKGROUND.store(multiplayer(), Ordering::Relaxed);
    unsafe { f(s, o) };
    DIM_BACKGROUND.store(false, Ordering::Relaxed);
}
unsafe extern "C-unwind" fn rectangle(r: *mut V, s: *mut c_void, o: *mut c_void, n: i32, a: *mut V) {
    let f: crate::gm::BuiltinFn = unsafe { std::mem::transmute(RECTANGLE.load(Ordering::Relaxed)) };
    // Pause.Draw GUI's first rectangle is its full-screen black backdrop.
    // Restore alpha immediately so native menu text and controls stay opaque.
    if DIM_BACKGROUND.swap(false, Ordering::Relaxed) {
        if let Some(g) = G::new() {
            let alpha = g.call("draw_get_alpha", &[]);
            g.call("draw_set_alpha", &[real(0.1)]);
            unsafe { f(r, s, o, n, a) };
            g.call("draw_set_alpha", &[alpha]);
            return;
        }
    }
    unsafe { f(r, s, o, n, a) };
}
unsafe fn axis(slot: &AtomicUsize, s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    if block_player_input() {
        unsafe { *r = real(0.0) };
        return r;
    }
    let f: ScriptFn = unsafe { std::mem::transmute(slot.load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}
unsafe extern "C-unwind" fn direction(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    unsafe { axis(&DIRECTION, s, o, r, n, a) }
}
unsafe extern "C-unwind" fn distance(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    unsafe { axis(&DISTANCE, s, o, r, n, a) }
}
