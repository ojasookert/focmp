//! In-game COOP menu: main menu → COOP → HOST / JOIN, with a live lobby page.
//!
//! Built inside the game's own menu system, so it looks native:
//!   - every page in `objMenu.pages` is a `Menu_class` instance; we clone
//!     `page_play_menu` with `variable_clone`, then rebind only the methods
//!     that were bound to the source page (method_get_self == source) to the
//!     clone with `method(clone, method_value)`
//!   - items are `{text, script_id: "open_page"|"go_back", script_var, x1..y2, ...}`
//!     structs cloned from existing items
//!   - actions are edge-triggered when `global.page.active` *changes* to one
//!     of our pages; leaving the lobby pages ends the session
//!
//! Pages:
//!   page_coop             HOST, JOIN, back
//!   page_coop_host        lobby (hosting): status, 3 player slots, back (= stop hosting)
//!   page_coop_join        join targets: localhost, clipboard address, back
//!   page_coop_lobby_local lobby (joined 127.0.0.1)
//!   page_coop_lobby_clip  lobby (joined the address on the clipboard)
//! Later: a Steam friends/lobbies list on the JOIN page.

use super::Coop;
use crate::engine::{G, V, is_struct, real, truthy};
use crate::gm::RValue;
use crate::hooks::{self, ScriptFn};
use crate::net::{DEFAULT_PORT, MAX_PLAYERS};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::net::Role;

static ACTION: Mutex<Option<String>> = Mutex::new(None);

const OPEN_PAGE: &str = "gml_Script_open_page@Menu_class@Menu_class";
static ORIG_OPEN_PAGE: AtomicUsize = AtomicUsize::new(0);
static ORIG_DRAW: AtomicUsize = AtomicUsize::new(0);
const DRAW_PAGE: &str = "gml_Script_anon@13890@Menu_class@Menu_class";
const CONFIRM_PAGE: &str = "page_coop_confirm_start";

// The Win32 message pump also runs inside native menu construction (and Steam
// overlay callbacks). Only mutate pages at the start of a completed Menu.Step.
static MENU_STEP: AtomicUsize = AtomicUsize::new(0);
pub fn install(g: G) {
    if MENU_STEP.load(Ordering::Relaxed) == 0 {
        match hooks::install(g.rt, "gml_Object_objMenu_Step_0", menu_step as *const ()) {
            Ok(f) => MENU_STEP.store(f as usize, Ordering::SeqCst),
            Err(e) => crate::log(&format!("coop menu: {e}")),
        }
    }
}
unsafe extern "C-unwind" fn menu_step(s: *mut c_void, o: *mut c_void) {
    let _pump = crate::block_pump();
    if let Some(g) = G::new() {
        if let Ok(mut c) = super::COOP.try_lock() {
            let instance = unsafe { g.rt.builtin_var_of("id", s) }.map(|v| crate::engine::key(&v));
            if instance != c.menu.instance {
                if super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0 && g.room_name() == "Room_main_menu" {
                    c.leave(g);
                }
                super::enemies::PUPPET_MODE.store(false, Ordering::Relaxed);
                c.menu.instance = instance;
                c.menu.last_page.clear();
                *ACTION.lock().unwrap() = None;
            }
            tick(g, &mut c);
            invites(g, &mut c);
        }
    }
    let f: unsafe extern "C-unwind" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(MENU_STEP.load(Ordering::Relaxed)) };
    unsafe { f(s, o) };
}

/// `open_page(name)` ignores pages it didn't create, so our pages are opened
/// here: push the current page on `global.page.old` (what the game's back
/// button pops) and make ours active. Other pages go to the original.
unsafe extern "C-unwind" fn hk_open_page(
    self_: *mut c_void,
    other: *mut c_void,
    ret: *mut RValue,
    argc: i32,
    args: *mut *mut RValue,
) -> *mut RValue {
    let name = if argc > 0 && !args.is_null() { unsafe { (**args).as_str() } } else { None };
    if let (Some(name), Some(g)) = (name, G::new()) {
        // Native "previous_page" navigation also uses page.next, which our
        // inserted pages do not populate. Handle our history here for both
        // Escape/controller Back and button clicks.
        let current = g.str_of(&g.get(g.global("page"), "active"));
        if name == "previous_page" && current.starts_with("page_coop") {
            *ACTION.lock().unwrap() = Some("page_coop_act_back".into());
            if !ret.is_null() { unsafe { *ret = V::undefined() }; }
            return ret;
        }
        if name.starts_with("page_coop_act_") {
            *ACTION.lock().unwrap() = Some(name);
            if !ret.is_null() { unsafe { *ret = RValue::undefined() }; }
            return ret;
        }
        if name.starts_with("page_coop") {
            let gp = g.global("page");
            let old = g.get(gp, "old");
            let cur = g.get(gp, "active");
            g.call("array_push", &[old, cur]);
            activate(g, &name);
            if !ret.is_null() {
                unsafe { *ret = RValue::undefined() };
            }
            return ret;
        }
    }
    let orig: ScriptFn = unsafe { std::mem::transmute(ORIG_OPEN_PAGE.load(Ordering::Relaxed)) };
    unsafe { orig(self_, other, ret, argc, args) }
}

#[derive(Default)]
pub struct MenuState {
    last_page: String,
    instance: Option<u64>,
    escape_down: bool,
    pending_invite: Option<u64>,
    invite_error: Option<String>,

}

enum Act {
    Open(&'static str),
    Back,
}

const LOBBY_PAGES: [&str; 4] = ["page_coop_host", "page_coop_lobby_local", "page_coop_lobby_clip", "page_coop_lobby_invite"];


pub fn tick(g: G, c: &mut Coop) {
    // Native main-menu cancellation handles only its built-in destinations.
    // Use an edge on Escape for inserted pages; held keys must not pop twice.
    let escape = truthy(&g.call("keyboard_check", &[real(27.0)]));
    if escape && !c.menu.escape_down && g.first("objMenu").is_some() {
        let page = g.str_of(&g.get(g.global("page"), "active"));
        if page.starts_with("page_coop") { *ACTION.lock().unwrap() = Some("page_coop_act_back".into()); }
    }
    c.menu.escape_down = escape;
    let action = ACTION.lock().unwrap().take();
    if let Some(action) = action {
        match action.as_str() {
            "page_coop_act_ready" => c.toggle_ready(),
            "page_coop_act_color" => c.cycle_color(),
            "page_coop_act_copy" => {if let Some(code)=c.net.as_ref().and_then(|n|n.lobby_code()){g.call("clipboard_set_text",&[g.s(&code)]);}},
            "page_coop_act_invite" => {
                let result = c.net.as_ref().and_then(|n| n.lobby_code()).and_then(|s| s.parse().ok())
                    .ok_or_else(|| "The Steam lobby is not ready yet".to_string())
                    .and_then(crate::steam::invite_friends);
                c.menu.invite_error = result.err();
            }
            "page_coop_act_back" => {
                let page = g.global("page");
                let old = g.get(page, "old");
                let previous = if g.arr_len(old) > 0 { g.call("array_pop", &[old]) } else { g.s("page_top_menu") };
                activate(g, &g.str_of(&previous));
            }
            "page_coop_act_start" | "page_coop_act_force_start" => {
                let force = action == "page_coop_act_force_start";
                let result = super::host_start_run(c, force);
                crate::log(&format!("coop lobby: {result}"));
            }
            _ => {}
        }
    }
    // FOCMP_NO_MENU=1: leave the game's menu untouched (A/B testing).
    static DISABLED: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var("FOCMP_NO_MENU").is_ok_and(|v| v == "1"));
    if *DISABLED {
        return;
    }
    let Some(menu) = g.first("objMenu") else {
        c.menu.last_page.clear();
        return;
    };
    let pages = g.get(menu, "pages");
    if !is_struct(&pages) {
        return;
    }
    if ORIG_OPEN_PAGE.load(Ordering::Relaxed) == 0 {
        match hooks::install(g.rt, OPEN_PAGE, hk_open_page as *const ()) {
            Ok(t) => ORIG_OPEN_PAGE.store(t as usize, Ordering::SeqCst),
            Err(e) => {
                crate::log(&format!("coop menu: {e}"));
                return;
            }
        }
    }
    if ORIG_DRAW.load(Ordering::Relaxed) == 0 {
        match hooks::install(g.rt, DRAW_PAGE, hk_draw_page as *const ()) {
            Ok(t) => ORIG_DRAW.store(t as usize, Ordering::SeqCst),
            Err(e) => { crate::log(&format!("coop panel: {e}")); return; }
        }
    }
    if !struct_has(g, pages, "page_coop") {
        if let Err(e) = build(g, pages) {
            crate::log(&format!("coop menu: build failed: {e}"));
            return;
        }
        crate::log("coop menu: COOP pages added to the main menu");
    }
    super::controls::extend_menu(g, pages);
    let active = g.str_of(&g.get(g.global("page"), "active"));
    if active != c.menu.last_page {
        on_enter(g, c, &active);
        c.menu.last_page = active.clone();
    }
    if LOBBY_PAGES.contains(&active.as_str()) || active == CONFIRM_PAGE {
        refresh_lobby(g, c, pages, &active);
    }
}

/// Handle both overlay accepts and Steam's launch-time +connect_lobby argument.
/// Wait for native pages to exist; do not read or overwrite the clipboard.
pub fn poll_invites(g: G, c: &mut Coop) {
    if let Some(id) = crate::steam::poll_invite() {
        let same = c.net.as_ref().and_then(|n|n.lobby_code()).is_some_and(|code|code == id.to_string());
        if same { c.menu.pending_invite = None; return; }
        c.menu.pending_invite = Some(id);
        crate::log(&format!("Steam invites: accepted lobby {id}"));
        if g.room_name() != "Room_main_menu" {
            g.call("show_message_async", &[g.s("Steam invite accepted. Return to the main menu to join.")]);
        }
    }
}

// Navigation is applied only by menu_step after native page construction.
fn invites(g: G, c: &mut Coop) {
    let Some(id) = c.menu.pending_invite else { return };
    if g.room_name() != "Room_main_menu" { return; }
    let Some(menu) = g.first("objMenu") else { return };
    if !struct_has(g,g.get(menu,"pages"),"page_coop_lobby_invite") { return; }
    c.menu.pending_invite = None;
    c.menu.invite_error = None;
    c.join(g,&id.to_string());
    let page = g.global("page");
    let old = g.call("array_create", &[real(1.0),g.s("page_coop")]);
    g.set(page,"old",old);
    activate(g, "page_coop_lobby_invite");
    c.menu.last_page = "page_coop_lobby_invite".into();
}

fn activate(g: G, name: &str) {
    let Some(menu) = g.first("objMenu") else { return };
    let pages = g.get(menu, "pages");
    if !struct_has(g, pages, name) { return; }
    let gp = g.global("page");
    let current = g.get(pages, &g.str_of(&g.get(gp, "active")));
    if is_struct(&current) {
        let cleanup = g.get(current, "destroy_objects");
        if truthy(&g.call("is_method", &[cleanup])) {
            g.call("method_call", &[cleanup, g.call("array_create", &[real(0.0)])]);
        }
    }
    // Match native open_page's transition bookkeeping, including its paired
    // forward/back stacks and the controller's one-frame input reset.
    g.set(gp, "next", g.call("array_create", &[real(0.0)]));
    g.set(gp, "active", g.s(name));
    let page = g.get(pages, name);
    g.set(g.get(page, "controller"), "opened_new_page", crate::engine::boolean(true));
}

fn on_enter(g: G, c: &mut Coop, page: &str) {
    let was_lobby = LOBBY_PAGES.contains(&c.menu.last_page.as_str()) || c.menu.last_page == CONFIRM_PAGE;
    match page {
        "page_coop_host" => {
            // Returning from confirmation must preserve the current session.
            if c.menu.last_page != CONFIRM_PAGE { c.host(g, DEFAULT_PORT); }
        },
        "page_coop_lobby_local" => c.join(g, &format!("127.0.0.1:{DEFAULT_PORT}")),
        "page_coop_lobby_clip" => {
            let clip = g.str_of(&g.call("clipboard_get_text", &[])).trim().to_string();
            if clip.is_empty() || clip.len() > 64 || clip.contains(char::is_whitespace) {
                c.status = if crate::config::load().transport==crate::config::Transport::Steam {"Copy the host’s Steam lobby code first"}else{"Copy the host address first (IP or host:port)"}.into();
            } else {
                let addr = if crate::config::load().transport==crate::config::Transport::Steam || clip.contains(':') { clip } else { format!("{clip}:{DEFAULT_PORT}") };
                c.join(g, &addr);
            }
        }
        // Back out of a lobby to the COOP/JOIN pages ends the session.
        // Leaving the menu entirely (starting a run) keeps it.
        "page_coop" | "page_coop_join" | "page_top_menu" if was_lobby && super::rooms::RUN_SEED.load(Ordering::Relaxed) == 0 => c.leave(g),
        _ => {}
    }
}

fn struct_has(g: G, st: V, name: &str) -> bool {
    crate::engine::truthy(&g.call("struct_exists", &[st, g.s(name)]))
}

fn build(g: G, pages: V) -> Result<(), String> {
    let top = g.get(pages, "page_top_menu");
    let play = g.get(pages, "page_play_menu");
    if !is_struct(&top) || !is_struct(&play) {
        return Err("page_top_menu/page_play_menu missing".into());
    }
    let top_items = g.get(top, "items");
    let n = g.arr_len(top_items);
    // Templates: an open_page item and a go_back item.
    let mut open_tmpl = None;
    let mut back_tmpl = None;
    for p in ["page_top_menu", "page_play_menu", "page_options"] {
        let items = g.get(g.get(pages, p), "items");
        for i in 0..g.arr_len(items) {
            let it = g.arr_get(items, i);
            match g.str_of(&g.get(it, "script_id")).as_str() {
                "open_page" if open_tmpl.is_none() => open_tmpl = Some(it),
                "go_back" if back_tmpl.is_none() => back_tmpl = Some(it),
                _ => {}
            }
        }
    }
    let (open_tmpl, back_tmpl) = (open_tmpl.ok_or("no open_page item")?, back_tmpl.ok_or("no go_back item")?);

    let item = |text: &str, act: &Act| -> V {
        let src = if matches!(act, Act::Back) { back_tmpl } else { open_tmpl };
        let root=g.s("focmp_ui_item");
        let it = g.call("variable_clone", &[src]);
        g.call("variable_global_set", &[root,it]);
        g.set(it, "text", g.s(text));
        match act {
            Act::Open(page) => {
                g.set(it, "script_id", g.s("open_page"));
                g.set(it, "script_var", g.s(page));
            }
            Act::Back => {
                g.set(it, "script_id", g.s("open_page"));
                g.set(it, "script_var", g.s("page_coop_act_back"));
            }

        }
        it
    };

    let mk_page = |name: &str, title: &str, items: &[(&str, Act)]| -> Result<(), String> {
        let page = clone_page(g, play)?;
        g.set(pages, name, page);
        g.set(page, "page_name", g.s(name));
        g.set(page, "page_title", g.s(title));
        let arr = g.call("array_create", &[real(0.0)]);
        g.set(page, "items", arr);
        for (text, act) in items {
            g.call("array_push", &[arr, item(text, act)]);
        }
        layout(g, arr);
        for i in 0..g.arr_len(arr) {
            let it = g.arr_get(arr, i);
            g.set(it, "x2", real(g.num(it, "x1") + 240.0));
        }
        g.set(page, "items", arr);
        g.set(page, "len_items", real(items.len() as f64));
        g.call("struct_set", &[pages, g.s(name), page]);
        Ok(())
    };

    mk_page("page_coop", "COOP", &[("host", Act::Open("page_coop_host")), ("join", Act::Open("page_coop_join")), ("back", Act::Back)])?;
    if crate::config::load().transport == crate::config::Transport::Steam {
        mk_page("page_coop_join", "JOIN", &[
            ("Paste Steam lobby code", Act::Open("page_coop_lobby_clip")),
            ("back", Act::Back),
        ])?;
    } else {
        mk_page("page_coop_join", "JOIN", &[
            ("join on this computer", Act::Open("page_coop_lobby_local")),
            ("paste address", Act::Open("page_coop_lobby_clip")),
            ("back", Act::Back),
        ])?;
    }

    for name in LOBBY_PAGES {
        let mut actions = if name == "page_coop_host" {
            vec![
                ("Ready", Act::Open("page_coop_act_ready")),
                ("Start run", Act::Open("page_coop_act_start")),
                ("Start anyway...", Act::Open(CONFIRM_PAGE)),
                ("Leave lobby", Act::Back),
                ("Color", Act::Open("page_coop_act_color")),
            ]
        } else {
            vec![("Ready", Act::Open("page_coop_act_ready")), ("Leave lobby", Act::Back), ("Color", Act::Open("page_coop_act_color"))]
        };
        let color_index=actions.len()-1;
        if crate::config::load().transport==crate::config::Transport::Steam {
            actions.push(("Copy lobby code",Act::Open("page_coop_act_copy")));
            actions.push(("Invite friends",Act::Open("page_coop_act_invite")));
        }
        mk_page(name, "CO-OP LOBBY", &actions)?;
        let arr = g.get(g.get(pages, name), "items");
        if actions.len()>color_index+1 {
            let copy=g.arr_get(arr,color_index+1);
            position_button(g,copy,266.0,265.0,168.0,16.0);
            g.set(copy,"focmp_secondary",real(1.0));
            let invite=g.arr_get(arr,color_index+2);
            position_button(g,invite,184.0,286.0,118.0,28.0);
        }
        let color = g.arr_get(arr, color_index);
        position_button(g, color, 52.0, 265.0, 200.0, 16.0);
        g.set(color, "focmp_secondary", real(1.0));
        g.set(color, "focmp_color_button", real(1.0));
        if name == "page_coop_host" {
            position_button(g, g.arr_get(arr, 0), 310.0, 286.0, 128.0, 28.0);
            position_button(g, g.arr_get(arr, 1), 454.0, 286.0, 134.0, 28.0);
            position_button(g, g.arr_get(arr, 2), 446.0, 265.0, 142.0, 16.0);
            g.set(g.arr_get(arr, 2), "focmp_secondary", real(1.0));
            position_button(g, g.arr_get(arr, 3), 52.0, 286.0, 124.0, 28.0);
        } else {
            position_button(g, g.arr_get(arr, 0), 454.0, 286.0, 134.0, 28.0);
            position_button(g, g.arr_get(arr, 1), 52.0, 286.0, 124.0, 28.0);
        }
    }
    mk_page(CONFIRM_PAGE, "START RUN?", &[
        ("Cancel", Act::Back), ("Start anyway", Act::Open("page_coop_act_force_start")),
    ])?;
    let arr = g.get(g.get(pages, CONFIRM_PAGE), "items");
    position_button(g, g.arr_get(arr, 0), 178.0, 246.0, 130.0, 28.0);
    position_button(g, g.arr_get(arr, 1), 326.0, 246.0, 136.0, 28.0);

    // Insert COOP directly under PLAY, wherever PLAY is (e.g. below a
    // CONTINUE item when a run is in progress). Fallback: second slot.
    let play_i = (0..n).find(|&i| g.str_of(&g.get(g.arr_get(top_items, i), "text")).eq_ignore_ascii_case("play"));
    let at = play_i.map_or(1.min(n), |i| i + 1);
    let coop = item("coop", &Act::Open("page_coop"));
    g.call("array_insert", &[top_items, real(at as f64), coop]);
    layout(g, top_items);
    g.set(top, "len_items", real((n + 1) as f64));
    Ok(())
}

/// Deep-clone a Menu_class page and make the copy self-contained.
///
/// `variable_clone` copies nested structs (e.g. the page's `controller`,
/// which tracks mouse focus), but every method value still points at the
/// *original* struct it was bound to. So walk original and clone side by side,
/// record original→clone for every struct, then rebind each method whose
/// owner was cloned to that owner's copy with `method(copy, method_value)`.
/// (`method_get_index` + `method()` is wrong in this YYC build: the index is
/// read as a built-in, e.g. camera_apply.)
fn clone_page(g: G, src: V) -> Result<V, String> {
    let root=clone_roots(g);
    let page = g.call("variable_clone", &[src]);
    g.call("variable_global_set", &[root, page]);
    if !is_struct(&page) || !truthy(&g.call("is_struct", &[page])) {
        return Err("variable_clone failed".into());
    }
    let mut map: Vec<(u64, V)> = Vec::new();
    pair_structs(g, src, page, 0, &mut map);
    rebind(g, page, &map, 0);
    Ok(page)
}

/// Clone UI data, rooting the returned value is the caller's responsibility.
/// Rebind methods whose owners were cloned, preserving external bindings.
pub fn clone_ui_value(g: G, src: V) -> V {
    let root=clone_roots(g);
    let copy = g.call("variable_clone", &[src]);
    g.call("variable_global_set", &[root, copy]);
    let mut map = Vec::new();
    pair_values(g, src, copy, 0, &mut map);
    for (_, st) in &map { rebind(g, *st, &map, 0); }
    copy
}

fn clone_roots(g:G)->V {
    let root=g.s("focmp_ui_clone");
    let work=g.s("focmp_ui_work");
    g.call("variable_global_set", &[work,g.call("array_create",&[real(0.0)])]);
    root
}
fn clone_names(g:G,st:V)->V {
    let names=g.call("struct_get_names",&[st]);
    g.call("array_push",&[g.global("focmp_ui_work"),names]);
    names
}

fn is_plain_struct(g: G, v: V) -> bool {
    is_struct(&v) && truthy(&g.call("is_struct", &[v])) && !truthy(&g.call("is_method", &[v]))
}

/// Record original→clone for `orig`/`copy` and every nested struct (through arrays too).
fn pair_structs(g: G, orig: V, copy: V, depth: u32, map: &mut Vec<(u64, V)>) {
    if depth > 6 || map.iter().any(|(k, _)| *k == crate::engine::key(&orig)) {
        return;
    }
    map.push((crate::engine::key(&orig), copy));
    let names = clone_names(g, orig);
    for i in 0..g.arr_len(names) {
        let name = g.arr_get(names, i);
        let (ov, cv) = (g.call("struct_get", &[orig, name]), g.call("struct_get", &[copy, name]));
        pair_values(g, ov, cv, depth + 1, map);
    }
}

fn pair_values(g: G, ov: V, cv: V, depth: u32, map: &mut Vec<(u64, V)>) {
    if is_plain_struct(g, ov) && is_plain_struct(g, cv) && crate::engine::key(&ov) != crate::engine::key(&cv) {
        pair_structs(g, ov, cv, depth, map);
    } else if truthy(&g.call("is_array", &[ov])) && truthy(&g.call("is_array", &[cv])) && depth <= 6 {
        let n = g.arr_len(ov).min(g.arr_len(cv)).min(64);
        for i in 0..n {
            pair_values(g, g.arr_get(ov, i), g.arr_get(cv, i), depth + 1, map);
        }
    }
}

/// Rebind methods inside `st` (and nested structs/arrays) to the cloned owners.
fn rebind(g: G, st: V, map: &[(u64, V)], depth: u32) {
    if depth > 6 {
        return;
    }
    let names = clone_names(g, st);
    for i in 0..g.arr_len(names) {
        let name = g.arr_get(names, i);
        let v = g.call("struct_get", &[st, name]);
        if truthy(&g.call("is_method", &[v])) {
            let owner = g.call("method_get_self", &[v]);
            if let Some((_, copy)) = map.iter().find(|(k, _)| *k == crate::engine::key(&owner)) {
                let m = g.call("method", &[*copy, v]);
                g.call("struct_set", &[st, name, m]);
            }
        } else if is_plain_struct(g, v) && map.iter().any(|(_, c)| crate::engine::key(c) == crate::engine::key(&v)) {
            rebind(g, v, map, depth + 1);
        } else if truthy(&g.call("is_array", &[v])) {
            for j in 0..g.arr_len(v).min(64) {
                let e = g.arr_get(v, j);
                if is_plain_struct(g, e) && map.iter().any(|(_, c)| crate::engine::key(c) == crate::engine::key(&e)) {
                    rebind(g, e, map, depth + 1);
                }
            }
        }
    }
}

/// Bottom-aligned column layout used by the game's menus (29 px rows, last row at y 295).
pub(super) fn layout(g: G, items: V) {
    let n = g.arr_len(items);
    for i in 0..n {
        let it = g.arr_get(items, i);
        let y1 = 295.0 - 29.0 * (n - 1 - i) as f64;
        g.set(it, "i", real(i as f64));
        g.set(it, "y1", real(y1));
        g.set(it, "y_sel", real(y1));
        g.set(it, "y2", real(y1 + 23.0));
        g.set(it, "y_text", real(y1 + 12.5));
    }
}

fn position_button(g: G, item: V, x: f64, y: f64, w: f64, h: f64) {
    for (name, value) in [("x1", x), ("x2", x + w), ("x_text", x + 12.0), ("y1", y), ("y2", y + h), ("y_sel", y), ("y_text", y + h / 2.0)] {
        g.set(item, name, real(value));
    }
    g.set(item, "focmp_enabled", real(1.0));
}

fn action(g: G, item: V, enabled: bool, target: &str) {
    let script = if enabled { "open_page" } else { "" };
    if g.str_of(&g.get(item, "script_id")) != script {
        g.set(item, "script_id", g.s(script));
    }
    g.set(item, "script_var", g.s(target));
    g.set(item, "focmp_enabled", real(enabled as u8 as f64));
}

fn refresh_lobby(g: G, c: &mut Coop, pages: V, active: &str) {
    let items = g.get(g.get(pages, active), "items");
    let connected = c.net.as_ref().is_some_and(|n| n.connected() && n.my_id < MAX_PLAYERS as u8);
    let host = c.net.as_ref().is_some_and(|n| n.role == Role::Host);
    if active!=CONFIRM_PAGE && crate::config::load().transport==crate::config::Transport::Steam {
        let steam = connected && c.net.as_ref().is_some_and(|n| n.is_steam());
        action(g,g.arr_get(items,g.arr_len(items)-2),steam,"page_coop_act_copy");
        action(g,g.arr_get(items,g.arr_len(items)-1),steam,"page_coop_act_invite");
    }
    if active == CONFIRM_PAGE {
        action(g, g.arr_get(items, 1), connected && host, "page_coop_act_force_start");
        return;
    }
    let ready = c.net.as_ref().is_some_and(|n| c.ready.get(n.my_id as usize).copied().unwrap_or(false));
    let ready_item = g.arr_get(items, 0);
    let text = if ready { "Cancel ready" } else { "Ready" };
    if g.str_of(&g.get(ready_item, "text")) != text { g.set(ready_item, "text", g.s(text)); }
    action(g, ready_item, connected, "page_coop_act_ready");
    let color_item = g.arr_get(items, if active=="page_coop_host" {4}else{2});
    action(g, color_item, connected, "page_coop_act_color");
    let color = c.net.as_ref().and_then(|n| c.colors.get(n.my_id as usize)).copied().unwrap_or(0);
    let color_text = format!("Color: {}  >", super::colors::name(color));
    if g.str_of(&g.get(color_item, "text")) != color_text { g.set(color_item, "text", g.s(&color_text)); }
    g.set(color_item, "focmp_color", real(super::colors::value(color)));
    if active == "page_coop_host" {
        action(g, g.arr_get(items, 1), connected && host && c.everyone_ready(), "page_coop_act_start");
        action(g, g.arr_get(items, 2), connected && host && !c.everyone_ready(), CONFIRM_PAGE);
    }
}

/// Replace only the lobby page's drawing. The native menu still owns mouse,
/// keyboard and controller input, selection, navigation and button actions.
unsafe extern "C-unwind" fn hk_draw_page(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    if let Some(g) = G::new() {
        let page = V { v: s as u64, flags: 0, kind: crate::gm::KIND_OBJECT };
        let name = g.str_of(&g.get(page, "page_name"));
        if LOBBY_PAGES.contains(&name.as_str()) || name == CONFIRM_PAGE {
            if let Ok(c) = super::COOP.try_lock() {
                draw_panel(g, page, &c, name == CONFIRM_PAGE);
                if !ret.is_null() { unsafe { *ret = V::undefined() }; }
                return ret;
            }
        }
    }
    let f: ScriptFn = unsafe { std::mem::transmute(ORIG_DRAW.load(Ordering::Relaxed)) };
    unsafe { f(s, o, ret, argc, args) }
}

fn rgb(r: u32, g: u32, b: u32) -> f64 { (r | g << 8 | b << 16) as f64 }

fn draw_panel(g: G, page: V, c: &Coop, confirm: bool) {
    let saved = [g.call("draw_get_color", &[]), g.call("draw_get_alpha", &[]), g.call("draw_get_font", &[]), g.call("draw_get_halign", &[]), g.call("draw_get_valign", &[])];
    g.call("draw_set_alpha", &[real(1.0)]);
    g.call("draw_set_font", &[g.get(page, "font_menu")]);
    g.call("draw_set_halign", &[real(0.0)]);
    g.call("draw_set_valign", &[real(1.0)]);
    let accent = g.num(page, "main_color");
    let white = rgb(239, 246, 241);
    let muted = rgb(146, 163, 163);
    let green = rgb(102, 224, 162);
    let rect = |x1: f64, y1: f64, x2: f64, y2: f64, color: f64, outline: bool| {
        g.call("draw_set_color", &[real(color)]);
        g.call("draw_rectangle", &[real(x1), real(y1), real(x2), real(y2), crate::engine::boolean(outline)]);
    };
    let text = |x: f64, y: f64, label: &str, color: f64| {
        g.call("draw_set_color", &[real(color)]);
        g.call("draw_text", &[real(x), real(y), g.s(label)]);
    };
    rect(32.0, 28.0, 608.0, 330.0, rgb(8, 14, 17), false);
    rect(32.0, 28.0, 608.0, 330.0, accent, true);
    rect(32.0, 28.0, 608.0, 31.0, accent, false);

    let host = c.net.as_ref().is_some_and(|n| n.role == Role::Host);
    let connected = c.net.as_ref().is_some_and(|n| n.connected());
    let roster = c.roster();
    let waiting = roster.iter().filter(|(id, _, _)| !c.ready[*id as usize]).count();
    if confirm {
        text(52.0, 54.0, if waiting == 0 { "START RUN?" } else { "START WITHOUT EVERYONE READY?" }, accent);
        let readiness = if waiting == 0 { "Everyone is ready.".into() } else { format!("{waiting} player{} not ready.", if waiting == 1 { " is" } else { "s are" }) };
        text(72.0, 113.0, &readiness, white);
        text(72.0, 150.0, "Everyone will move to difficulty selection.", white);
        text(72.0, 178.0, "Players cannot join once the run starts.", muted);
        if !connected { text(72.0, 210.0, "The lobby is no longer connected.", accent); }
    } else {
        text(52.0, 54.0, "CO-OP LOBBY", accent);
        let badge = if !connected { "NOT CONNECTED" } else if host { "YOU ARE HOSTING" } else { "JOINED" };
        g.call("draw_set_halign", &[real(2.0)]);
        text(588.0, 54.0, badge, muted);
        g.call("draw_set_halign", &[real(0.0)]);
        let subtitle = match &c.net {
            Some(n) if connected && host => format!("{}  /  {} of 3 players", n.addr, roster.len()),
            Some(n) if connected => format!("Connected to {}  /  {} of 3 players", n.addr, roster.len()),
            _ => c.status.chars().take(66).collect(),
        };
        text(52.0, 80.0, &subtitle, muted);
        text(68.0, 113.0, "PLAYER", muted);
        text(470.0, 113.0, "STATUS", muted);
        for slot in 0..MAX_PLAYERS {
            let y = 142.0 + slot as f64 * 35.0;
            rect(52.0, y - 14.0, 588.0, y + 14.0, rgb(17, 26, 29), false);
            match roster.get(slot) {
                Some((id, name, me)) => {
                    let name: String = name.chars().filter(|ch| !ch.is_control()).take(22).collect();
                    text(68.0, y, &format!("{}  {}", id + 1, name), super::colors::value(c.colors[*id as usize]));
                    let tag = match (*id == 0, me) { (true, true) => "YOU / HOST", (true, false) => "HOST", (false, true) => "YOU", _ => "" };
                    text(330.0, y, tag, muted);
                    text(470.0, y, if c.ready[*id as usize] { "Ready" } else { "Not ready" }, if c.ready[*id as usize] { green } else { muted });
                }
                None => { text(68.0, y, "Open slot", muted); text(470.0, y, "Waiting", muted); }
            }
        }
        let message = if !connected { "Leave this lobby to host or join another.".into() }
            else if host && waiting == 0 { "Everyone is ready. Start the run when you're ready.".into() }
            else if host { format!("Waiting for {waiting} player{} to ready up.", if waiting == 1 { "" } else { "s" }) }
            else if c.net.as_ref().is_some_and(|n| c.ready.get(n.my_id as usize).copied().unwrap_or(false)) { "You're ready. Waiting for the host to start.".into() }
            else { "Select Ready when you're ready to start a run.".into() };
        text(52.0, 248.0, c.menu.invite_error.as_deref().unwrap_or(&message), if waiting == 0 && connected { green } else { muted });
    }

    let items = g.get(page, "items");
    let focus = g.num(page, "i_infocus") as i32;
    for i in 0..g.arr_len(items) {
        let item = g.arr_get(items, i);
        let enabled = truthy(&g.get(item, "focmp_enabled"));
        let secondary = truthy(&g.get(item, "focmp_secondary"));
        let selected = focus == i as i32 && enabled;
        let (x1, y1, x2, y2) = (g.num(item, "x1"), g.num(item, "y1"), g.num(item, "x2"), g.num(item, "y2"));
        if !secondary {
            rect(x1, y1, x2, y2, if selected { accent } else { rgb(17, 26, 29) }, false);
            rect(x1, y1, x2, y2, if enabled { accent } else { rgb(60, 70, 72) }, true);
        }
        let label = g.str_of(&g.get(item, "text"));
        g.call("draw_set_halign", &[real(1.0)]);
        text((x1 + x2) / 2.0, (y1 + y2) / 2.0, &label,
            if !enabled { rgb(92, 104, 105) } else if selected && !secondary { rgb(8, 14, 17) } else if truthy(&g.get(item, "focmp_color_button")) { g.num(item, "focmp_color") } else if secondary { accent } else { white });
        g.call("draw_set_halign", &[real(0.0)]);
    }
    for (name, value) in ["draw_set_color", "draw_set_alpha", "draw_set_font", "draw_set_halign", "draw_set_valign"].into_iter().zip(saved) { g.call(name, &[value]); }
}
