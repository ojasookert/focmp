//! Use the game's bound actions and active device for mod-only controls.
use crate::engine::{G, V, real, truthy};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// Native verbs occupy 0..24 in the supported game build. Register before
// devices/clusters initialize and before saved bindings are imported.
const PLACE_CARD: u8 = 24;
static CONFIG: AtomicUsize = AtomicUsize::new(0);
static REGISTERED: AtomicBool = AtomicBool::new(false);
static LEGEND: AtomicUsize = AtomicUsize::new(0);

pub fn install() {
    let Some(g) = G::new() else { return };
    match crate::hooks::install(g.rt, "gml_Script_anon@406@gml_Object_objControls_Create_0", legend as *const ()) {
        Ok(f) => LEGEND.store(f as usize, Ordering::SeqCst),
        Err(e) => crate::log(&format!("controls legend: {e}")),
    }
    match crate::hooks::install(g.rt, "gml_Script___InputConfigVerbs", configure as *const ()) {
        Ok(f) => CONFIG.store(f as usize, Ordering::SeqCst),
        Err(e) => crate::log(&format!("controls: {e}")),
    }
}
unsafe extern "C-unwind" fn configure(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    let original: crate::hooks::ScriptFn = unsafe { std::mem::transmute(CONFIG.load(Ordering::Relaxed)) };
    let result = unsafe { original(s, o, r, n, a) };
    if let Some(g) = G::new() {
        // A stable export name lets native imports retain this action across
        // builds, while older binding files simply leave its defaults intact.
        // Native rebinding always exposes two slots. A scalar default would
        // create a one-slot action and make selecting Alternate invalid.
        let keyboard = g.call("array_create", &[real(2.0), V::undefined()]);
        g.call("variable_global_set", &[g.s("focmp_place_keyboard_defaults"), keyboard]);
        g.call("array_set", &[keyboard, real(0.0), real(71.0)]);
        let gamepad = g.call("array_create", &[real(2.0), V::undefined()]);
        g.call("variable_global_set", &[g.s("focmp_place_gamepad_defaults"), gamepad]);
        g.call("array_set", &[gamepad, real(0.0), real(32780.0)]);
        unsafe { script(g, "gml_Script_InputDefineVerb", s, o, &mut [
            real(PLACE_CARD as f64), g.s("focmp_place_card"), keyboard, gamepad, V::undefined(),
        ]); }
        REGISTERED.store(true, Ordering::Release);
        crate::log("controls: registered Place card (G / right-stick click)");
    }
    result
}

/// Extend the existing UI binding page. Its native rebinding object owns
/// capture, primary/alternate slots, per-device editing, saving and reset.
pub fn extend_menu(g: G, pages: V) {
    if !REGISTERED.load(Ordering::Acquire) { return; }
    let page = g.get(pages, "page_bindings_UI");
    if !crate::engine::is_struct(&page) || crate::engine::is_struct(&g.get(page, "focmp_place_binding")) { return; }
    let items = g.get(page, "items");
    let count = g.arr_len(items);
    if count < 2 { return; }
    let item = g.call("variable_clone", &[g.arr_get(items, 0)]);
    g.set(page, "focmp_place_binding", item); // Root before allocating the array.
    g.set(item, "text", g.s("place card"));
    g.set(item, "script_var", real(PLACE_CARD as f64));
    let at = (0..count).find(|i| g.str_of(&g.get(g.arr_get(items, *i), "text")) == "sell").map_or(count - 1, |i| i + 1);
    g.call("array_insert", &[items, real(at as f64), item]);
    super::menu::layout(g, items);
    g.set(page, "items", items);
}
fn call(g: G, name: &str, args: &[V]) -> V {
    let mut values = Vec::with_capacity(args.len() + 1);
    values.push(g.asset(name));
    values.extend_from_slice(args);
    g.call("script_execute", &values)
}
pub fn pressed(g: G, action: u8) -> bool {
    truthy(&call(g, "InputPressed", &[real(action as f64)]))
}
pub fn hint(g: G, action: u8) -> String {
    g.str_of(&call(g, "input_get_binding_text", &[real(action as f64)]))
}
// Native action 6 is inventory (Tab / View-Back).
pub fn inventory_pressed(g: G) -> bool {
    pressed(g, 6)
}
pub fn place_pressed(g: G) -> bool {
    REGISTERED.load(Ordering::Acquire) && pressed(g, PLACE_CARD)
}

// The game's icon set stops at the triggers/menu buttons. Supply the missing
// right-stick click glyph to Scribble, so it lays out like the native icons.
static STICK_ICON: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
unsafe fn script(g: G, name: &str, s: *mut std::ffi::c_void, o: *mut std::ffi::c_void, values: &mut [V]) -> V {
    let Some((_, address)) = g.rt.scripts.iter().find(|(n, _)| *n == name) else { return V::undefined() };
    let f: crate::hooks::ScriptFn = unsafe { std::mem::transmute(*address) };
    let mut args: Vec<_> = values.iter_mut().map(|v| v as *mut V).collect();
    let mut result = V::undefined();
    unsafe { f(s, o, &mut result, args.len() as i32, args.as_mut_ptr()); }
    result
}
pub unsafe fn draw_text(g: G, s: *mut std::ffi::c_void, o: *mut std::ffi::c_void, x: f64, y: f64, text: &str) {
    unsafe { script(g, "gml_Script_draw_text_scribble", s, o, &mut [real(x), real(y), g.s(text)]); }
}
pub unsafe fn place_hint(g: G, s: *mut std::ffi::c_void, o: *mut std::ffi::c_void) -> String {
    use std::sync::atomic::Ordering;
    if !REGISTERED.load(Ordering::Acquire) { return String::new(); }
    let binding = hint(g, PLACE_CARD);
    if !right_stick_binding(&binding) { return binding; }
    if STICK_ICON.load(Ordering::Relaxed) < 0 {
        const PIXELS: [&str; 13] = [
            "    #####    ",
            "  ##     ##  ",
            " #         # ",
            "#  ### ###  #",
            "#  # #   #  #",
            "#  ###  ##  #",
            "#  ##    #  #",
            "#  # # ###  #",
            "#           #",
            " #         # ",
            " #         # ",
            "  ##     ##  ",
            "    #####    ",
        ];
        let surface = g.call("surface_create", &[real(13.0), real(13.0)]);
        if !truthy(&g.call("surface_exists", &[surface])) { return binding; }
        let color = g.call("draw_get_color", &[]);
        let alpha = g.call("draw_get_alpha", &[]);
        g.call("surface_set_target", &[surface]);
        g.call("draw_clear_alpha", &[real(0.0), real(0.0)]);
        g.call("draw_set_color", &[real(16777215.0)]);
        g.call("draw_set_alpha", &[real(1.0)]);
        for (y, line) in PIXELS.iter().enumerate() {
            for (x, pixel) in line.bytes().enumerate() {
                if pixel == b'#' { g.call("draw_point", &[real(x as f64), real(y as f64)]); }
            }
        }
        g.call("surface_reset_target", &[]);
        g.call("draw_set_color", &[color]);
        g.call("draw_set_alpha", &[alpha]);
        let sprite = g.call("sprite_create_from_surface", &[surface, real(0.0), real(0.0), real(13.0), real(13.0), crate::engine::boolean(false), crate::engine::boolean(false), real(0.0), real(0.0)]);
        g.call("surface_free", &[surface]);
        if !truthy(&g.call("sprite_exists", &[sprite])) { return binding; }
        unsafe { script(g, "gml_Script_scribble_external_sprite_add", s, o, &mut [sprite, g.s("focmp_right_stick_click")]); }
        STICK_ICON.store(crate::engine::asset_id(sprite), Ordering::Relaxed);
    }
    "[pulse][focmp_right_stick_click][/pulse]".into()
}

fn right_stick_binding(binding: &str) -> bool {
    binding.contains("thumbstick r click") || binding.contains("[R3]")
}

// Only the user-drawn additions are embedded. The installed game's controller
// artwork and dynamic labels remain the source of the rest of the legend.
unsafe extern "C-unwind" fn legend(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    let original: crate::hooks::ScriptFn = unsafe { std::mem::transmute(LEGEND.load(Ordering::Relaxed)) };
    let result = unsafe { original(s, o, r, n, a) };
    if let Some(g) = G::new() {
        unsafe { draw_legend_addition(g, s, o); }
    }
    result
}
unsafe fn draw_legend_addition(g: G, s: *mut c_void, o: *mut c_void) {
    if !REGISTERED.load(Ordering::Acquire) { return; }
    // A hub diagram and an options diagram can coexist. Update this instance.
    let Some(controls) = (unsafe { g.rt.builtin_var_of("id", s) }) else { return };
    if g.num(controls, "device") < 0.0 { return; }
    let surface = g.get(controls, "surf");
    if !truthy(&g.call("surface_exists", &[surface])) { return; }
    let binding = hint(g, PLACE_CARD);
    let stick = right_stick_binding(&binding);
    let saved = [g.call("draw_get_color", &[]), g.call("draw_get_alpha", &[]), g.call("draw_get_font", &[]), g.call("draw_get_halign", &[]), g.call("draw_get_valign", &[])];
    g.call("surface_set_target", &[surface]);
    g.call("draw_set_color", &[real(16777215.0)]);
    g.call("draw_set_alpha", &[real(1.0)]);
    g.call("draw_set_font", &[g.global("Font_main_2x")]);
    g.call("draw_set_halign", &[real(0.0)]);
    g.call("draw_set_valign", &[real(1.0)]);
    if stick {
        // Native controller types 3/4 use the PlayStation picture.
        let pixels = if matches!(g.num(controls, "gamepad_type") as i32, 3 | 4) { PLACE_PS } else { PLACE_XBOX };
        for &(x, y) in pixels { g.call("draw_point", &[real(x as f64), real(y as f64)]); }
    }
    let label = if stick { "place card".to_owned() } else { format!("{binding} place card") };
    unsafe { draw_text(g, s, o, 239.0, 102.0, &label); }
    g.call("surface_reset_target", &[]);
    for (name, value) in ["draw_set_color", "draw_set_alpha", "draw_set_font", "draw_set_halign", "draw_set_valign"].into_iter().zip(saved) { g.call(name, &[value]); }
}
const PLACE_XBOX: &[(u16, u16)] = &[
    (182, 86), (183, 86), (183, 87), (184, 87), (184, 88), (185, 88), (185, 89), (186, 89),
    (186, 90), (187, 90), (187, 91), (188, 91), (188, 92), (189, 92), (189, 93), (190, 93),
    (190, 94), (191, 94), (191, 95), (192, 95), (192, 96), (193, 96), (193, 97), (194, 97),
    (194, 98), (195, 98), (195, 99), (196, 99), (196, 100), (197, 100), (197, 101), (198, 101),
    (198, 102), (199, 102), (200, 102), (201, 102), (202, 102), (203, 102), (204, 102), (205, 102),
    (206, 102), (207, 102), (208, 102), (209, 102), (210, 102), (211, 102), (212, 102), (213, 102),
    (214, 102), (215, 102), (216, 102), (217, 102), (218, 102), (220, 102), (221, 102), (222, 102),
    (223, 102), (224, 102), (225, 102), (226, 102), (227, 102), (228, 102), (229, 102), (230, 102),
    (231, 102), (232, 102), (233, 102), (234, 102), (235, 102), (236, 102),
];
const PLACE_PS: &[(u16, u16)] = &[
    (186, 81), (186, 82), (186, 83), (186, 84), (186, 86), (186, 87), (186, 88), (186, 89),
    (186, 90), (186, 91), (186, 92), (186, 93), (186, 94), (186, 95), (186, 96), (186, 97),
    (186, 98), (186, 99), (186, 100), (186, 101), (186, 102), (187, 102), (188, 102), (189, 102),
    (190, 102), (191, 102), (192, 102), (193, 102), (194, 102), (195, 102), (196, 102), (197, 102),
    (198, 102), (200, 102), (201, 102), (202, 102), (203, 102), (204, 102), (205, 102), (206, 102),
    (207, 102), (208, 102), (209, 102), (210, 102), (211, 102), (212, 102), (213, 102), (214, 102),
    (215, 102), (216, 102), (217, 102), (218, 102), (219, 102), (220, 102), (222, 102), (223, 102),
    (224, 102), (225, 102), (226, 102), (227, 102), (228, 102), (229, 102), (230, 102), (231, 102),
    (232, 102), (233, 102), (234, 102), (235, 102), (236, 102),
];
