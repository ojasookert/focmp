//! Navigation commands: see where the game is and drive its menus.

use crate::commands::{main, real, remember};
use crate::gm::{RValue, Runtime};
use crate::input;
use std::fmt::Write;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Duration;

pub fn run(cmd: &str, args: &[String]) -> Option<String> {
    Some(match cmd {
        "screen" => main(|rt| screen(rt)),
        "pad" => crate::gamepad_debug::console(args),
        "menu" => match args.first().map(String::as_str) {
            None => main(|rt| match menu_items(rt) {
                Ok(m) => format_menu(&m),
                Err(e) => format!("error: {e}"),
            }),
            Some("pick") => {
                let Some(which) = args.get(1).map(|s| s.trim_start_matches('\u{1}').to_string()) else {
                    return Some("usage: menu pick <index|text>".into());
                };
                menu_pick(&which)
            }
            Some(other) => format!("unknown menu subcommand '{other}'"),
        },
        "key" => {
            let Some(k) = args.first() else { return Some("usage: key <key> [hold_ms]".into()) };
            let hold = args.get(1).and_then(|h| h.parse().ok()).unwrap_or(80);
            press_key(k, hold)
        }
        "keys" => {
            let mut out = String::new();
            for k in args {
                let r = press_key(k, 80);
                let _ = writeln!(out, "{r}");
                sleep(Duration::from_millis(120));
            }
            out
        }
        "mouse" => match args {
            [off] if off == "off" => {
                input::mouse_release();
                "mouse: real device".into()
            }
            [x, y] => match (x.parse::<f64>(), y.parse::<f64>()) {
                (Ok(gx), Ok(gy)) => match gui_to_client(gx, gy) {
                    Ok((cx, cy)) => {
                        input::mouse_move(cx, cy);
                        format!("mouse: GUI ({gx}, {gy}) -> client ({cx}, {cy})")
                    }
                    Err(e) => format!("error: {e}"),
                },
                _ => "usage: mouse <gx> <gy> | mouse off".into(),
            },
            _ => "usage: mouse <gx> <gy> | mouse off".into(),
        },
        "click" => {
            let right = args.get(2).is_some_and(|a| a == "right");
            match (args.first().and_then(|x| x.parse().ok()), args.get(1).and_then(|y| y.parse().ok())) {
                (Some(gx), Some(gy)) => click(gx, gy, right),
                _ => "usage: click <gx> <gy> [right]".into(),
            }
        }
        "focus" => match args.first().map(String::as_str) {
            Some("force") => main(|rt| force_focus(rt, true)),
            Some("normal") => main(|rt| force_focus(rt, false)),
            _ => format!(
                "focus: {} (usage: focus force|normal)",
                if FOCUS_ORIG.lock().unwrap().is_some() { "forced" } else { "normal" }
            ),
        },
        "window" => {
            let a: Vec<String> = args.to_vec();
            main(move |rt| window(rt, &a))
        }
        "hold" => {
            // hold <gx> <gy> <ms>: aim at GUI point and hold the left button (fire).
            let (Some(gx), Some(gy)) = (args.first().and_then(|v| v.parse::<f64>().ok()), args.get(1).and_then(|v| v.parse::<f64>().ok())) else {
                return Some("usage: hold <gx> <gy> [ms]".into());
            };
            let ms = args.get(2).and_then(|v| v.parse::<u64>().ok()).unwrap_or(500).min(2500);
            match gui_to_client(gx, gy) {
                Ok((cx, cy)) => {
                    input::mouse_move(cx, cy);
                    sleep(Duration::from_millis(60));
                    input::mouse_button(false, true);
                    sleep(Duration::from_millis(ms));
                    input::mouse_button(false, false);
                    sleep(Duration::from_millis(60));
                    input::mouse_release();
                    format!("held left at GUI ({gx}, {gy}) for {ms} ms")
                }
                Err(e) => format!("error: {e}"),
            }
        }
        "wait" => {
            let ms = args.first().and_then(|m| m.parse().ok()).unwrap_or(500u64).min(30_000);
            sleep(Duration::from_millis(ms));
            format!("waited {ms} ms")
        }
        _ => return None,
    })
}

fn s(x: &str) -> RValue {
    RValue::string(x)
}

unsafe fn struct_get(rt: &Runtime, st: RValue, field: &str) -> RValue {
    unsafe { rt.call("variable_struct_get", &[st, s(field)]) }.unwrap_or(RValue::undefined())
}

/// The game window handle, fetched once with `window_handle()`.
unsafe fn ensure_hwnd(rt: &Runtime) {
    if input::HWND.load(Ordering::Relaxed) == 0 {
        if let Ok(h) = unsafe { rt.call("window_handle", &[]) } {
            input::HWND.store(h.v as isize, Ordering::Relaxed);
        }
    }
}

fn screen(rt: &'static Runtime) -> String {
    unsafe {
        ensure_hwnd(rt);
        let mut out = String::new();
        let room = rt.builtin_var("room").unwrap_or(RValue::undefined());
        let room_name = rt.call("room_get_name", &[room]).map(|r| rt.to_string(&r)).unwrap_or_default();
        let page = rt.call("variable_global_get", &[s("page")]).unwrap();
        let active = rt.to_string(&struct_get(rt, page, "active"));
        let (cw, ch) = input::client_size();
        let _ = writeln!(out, "room:        {room_name}");
        let _ = writeln!(out, "menu page:   {active}");
        let _ = writeln!(
            out,
            "GUI size:    {} x {}",
            real(rt, "display_get_gui_width", &[]),
            real(rt, "display_get_gui_height", &[])
        );
        let _ = writeln!(out, "window:      {cw} x {ch} (client), hwnd 0x{:x}", input::HWND.load(Ordering::Relaxed));
        let fps = rt.builtin_var("fps").ok().and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
        let _ = writeln!(out, "fps:         {fps}");
        let _ = write!(out, "mouse:       {}", input::mouse_state());
        let gx = real(rt, "device_mouse_x_to_gui", &[RValue::real(0.0)]);
        let gy = real(rt, "device_mouse_y_to_gui", &[RValue::real(0.0)]);
        let _ = write!(out, "  (game sees GUI {gx:.0}, {gy:.0})");
        out
    }
}

struct MenuItem {
    i: usize,
    text: String,
    rect: (f64, f64, f64, f64),
}

struct Menu {
    page: String,
    title: String,
    selected: String,
    items: Vec<MenuItem>,
}

fn menu_items(rt: &'static Runtime) -> Result<Menu, String> {
    unsafe {
        ensure_hwnd(rt);
        let shared = rt.call("asset_get_index", &[s("objMenuShared")])?;
        if real(rt, "instance_number", &[shared]) < 1.0 {
            return Err("no menu open (no objMenuShared instance: objMenu/objPause/objGameEnd/objDemoEnd)".into());
        }
        let menu = rt.call("instance_find", &[shared, RValue::real(0.0)])?;
        let gpage = rt.call("variable_global_get", &[s("page")])?;
        let active = rt.to_string(&struct_get(rt, gpage, "active"));
        let pages = rt.call("variable_instance_get", &[menu, s("pages")])?;
        let page = struct_get(rt, pages, &active);
        if page.kind() != crate::gm::KIND_OBJECT {
            return Err(format!("active page '{active}' not found in pages"));
        }
        remember(page);
        let title = rt.to_string(&struct_get(rt, page, "page_title"));
        let selected = rt.to_string(&struct_get(rt, page, "i_infocus"));
        let items = struct_get(rt, page, "items");
        let n = real(rt, "array_length", &[items]);
        let mut out = Vec::new();
        for i in 0..(if n.is_nan() { 0 } else { n as usize }) {
            let it = rt.call("array_get", &[items, RValue::real(i as f64)])?;
            let f = |k: &str| struct_get(rt, it, k).as_f64().unwrap_or(f64::NAN);
            out.push(MenuItem {
                i,
                text: rt.to_string(&struct_get(rt, it, "text")),
                rect: (f("x1"), f("y1"), f("x2"), f("y2")),
            });
        }
        Ok(Menu { page: active, title, selected, items: out })
    }
}

fn format_menu(m: &Menu) -> String {
    let mut out = format!("page {} \"{}\" (focus {})\n", m.page, m.title, m.selected);
    for it in &m.items {
        let (x1, y1, x2, y2) = it.rect;
        let _ = writeln!(out, "  [{:2}] {:<28} GUI ({x1:.0},{y1:.0})-({x2:.0},{y2:.0})", it.i, it.text);
    }
    out
}

fn menu_pick(which: &str) -> String {
    let m = match main_res(menu_items) {
        Ok(m) => m,
        Err(e) => return format!("error: {e}"),
    };
    let item = match which.parse::<usize>() {
        Ok(i) => m.items.iter().find(|it| it.i == i),
        Err(_) => {
            let w = which.to_lowercase();
            m.items.iter().find(|it| it.text.to_lowercase() == w).or_else(|| m.items.iter().find(|it| it.text.to_lowercase().contains(&w)))
        }
    };
    let Some(it) = item else { return format!("no item '{which}' on page {}\n{}", m.page, format_menu(&m)) };
    // Don't write item_sel here: changing the selection behind the menu's
    // back leaves i_infocus = noone and the next click indexes items[-4]
    // (GML error in Menu_class update). Rely on the calibrated cursor instead.
    let (x1, y1, x2, y2) = it.rect;
    let r = click((x1 + x2) / 2.0, (y1 + y2) / 2.0, false);
    format!("picked [{}] {} on {}: {r}", it.i, it.text, m.page)
}

/// Like `main`, but for jobs that return a Result value instead of text.
fn main_res<T: Send + 'static>(f: fn(&'static Runtime) -> Result<T, String>) -> Result<T, String> {
    let Some(rt) = crate::RUNTIME.get() else { return Err("runtime scan failed".into()) };
    crate::on_main(Duration::from_secs(5), move || f(rt))?
}

fn gui_to_client(gx: f64, gy: f64) -> Result<(i32, i32), String> {
    let (gw, gh) = main_res(|rt| unsafe {
        ensure_hwnd(rt);
        Ok((real(rt, "display_get_gui_width", &[]), real(rt, "display_get_gui_height", &[])))
    })?;
    let (cw, ch) = input::client_size();
    if gw <= 0.0 || gh <= 0.0 || cw <= 0 || ch <= 0 {
        return Err(format!("bad sizes: GUI {gw}x{gh}, client {cw}x{ch}"));
    }
    Ok(((gx * cw as f64 / gw).round() as i32, (gy * ch as f64 / gh).round() as i32))
}

fn click(gx: f64, gy: f64, right: bool) -> String {
    let (cx, cy) = match gui_to_client(gx, gy) {
        Ok(p) => p,
        Err(e) => return format!("error: {e}"),
    };
    // Hover first so the UI can register focus, then press and release
    // with a few frames in between.
    input::mouse_move(cx, cy);
    sleep(Duration::from_millis(100));
    // Calibrate: when sway moves a window, Wine's idea of its position can be
    // stale, so the game sees the injected cursor shifted. Read back what the
    // game sees and correct (up to 3 passes).
    let (mut cx, mut cy) = (cx, cy);
    for _ in 0..3 {
        let seen = main_res(|rt| {
            Ok((
                real(rt, "device_mouse_x_to_gui", &[RValue::real(0.0)]),
                real(rt, "device_mouse_y_to_gui", &[RValue::real(0.0)]),
            ))
        });
        let Ok((sx, sy)) = seen else { break };
        let (ex, ey) = (gx - sx, gy - sy);
        if ex.abs() < 1.5 && ey.abs() < 1.5 {
            break;
        }
        let Ok((ax, ay)) = gui_to_client(gx + ex, gy + ey) else { break };
        let (bx, by) = gui_to_client(gx, gy).unwrap_or((cx, cy));
        cx += ax - bx;
        cy += ay - by;
        input::mouse_move(cx, cy);
        sleep(Duration::from_millis(80));
    }
    input::mouse_button(right, true);
    sleep(Duration::from_millis(80));
    input::mouse_button(right, false);
    sleep(Duration::from_millis(100));
    input::mouse_release();
    format!("clicked GUI ({gx:.0}, {gy:.0}) = client ({cx}, {cy}){}", if right { " [right]" } else { "" })
}

fn press_key(name: &str, hold_ms: u64) -> String {
    let Some(vk) = input::vk(name.trim_start_matches('\u{1}')) else { return format!("unknown key '{name}'") };
    // Make sure we know our own window, then inject into it only (see input::key).
    if let Err(e) = main_res_dyn(|rt| {
        unsafe { ensure_hwnd(rt) };
        Ok(())
    }) {
        return format!("error: {e}");
    }
    input::key(vk, true);
    sleep(Duration::from_millis(hold_ms));
    input::key(vk, false);
    sleep(Duration::from_millis(60));
    format!("key {name} (vk {vk}) held {hold_ms} ms (this window only)")
}

fn main_res_dyn<T: Send + 'static>(
    f: impl FnOnce(&'static Runtime) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let Some(rt) = crate::RUNTIME.get() else { return Err("runtime scan failed".into()) };
    crate::on_main(Duration::from_secs(5), move || f(rt))?
}


// ---- focus override ---------------------------------------------------------

/// Original first bytes of `window_has_focus`, while the override is active.
static FOCUS_ORIG: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

/// Replacement body for the `window_has_focus` built-in: always true.
unsafe extern "C-unwind" fn always_focused(
    result: *mut RValue,
    _self: *mut std::ffi::c_void,
    _other: *mut std::ffi::c_void,
    _argc: i32,
    _args: *mut RValue,
) {
    unsafe { *result = RValue::real(1.0) };
}

/// Make `window_has_focus()` return true, so a background instance keeps
/// reading injected input. Patched on the main thread, between frames.
fn force_focus(rt: &'static Runtime, on: bool) -> String {
    let Some(&addr) = rt.builtins.get("window_has_focus") else { return "error: window_has_focus not found".into() };
    let mut orig = FOCUS_ORIG.lock().unwrap();
    match (on, orig.is_some()) {
        (true, true) => "focus already forced".into(),
        (false, false) => "focus already normal".into(),
        (true, false) => match unsafe { crate::win::patch_code(addr, &crate::win::abs_jmp(always_focused as *const () as usize)) } {
            Some(prev) => {
                *orig = Some(prev);
                crate::log("window_has_focus forced to true");
                "focus forced: window_has_focus() now always returns true".into()
            }
            None => "error: patch failed".into(),
        },
        (false, true) => {
            let prev = orig.take().unwrap();
            unsafe { crate::win::patch_code(addr, &prev) };
            crate::log("window_has_focus restored");
            "focus normal: window_has_focus() restored".into()
        }
    }
}

// ---- window -----------------------------------------------------------------

fn window(rt: &'static Runtime, a: &[String]) -> String {
    let num = |i: usize| a.get(i).and_then(|v| v.parse::<f64>().ok());
    unsafe {
        match (a.first().map(|s| s.as_str()), num(0), num(1)) {
            (Some("full"), _, _) => {
                let _ = rt.call("window_set_fullscreen", &[RValue::boolean(true)]);
                "window: fullscreen".into()
            }
            (_, Some(w), Some(h)) => {
                let _ = rt.call("window_set_fullscreen", &[RValue::boolean(false)]);
                let _ = rt.call("window_set_size", &[RValue::real(w), RValue::real(h)]);
                if let (Some(x), Some(y)) = (num(2), num(3)) {
                    let _ = rt.call("window_set_position", &[RValue::real(x), RValue::real(y)]);
                }
                format!("window: {w} x {h}{}", match (num(2), num(3)) {
                    (Some(x), Some(y)) => format!(" at ({x}, {y})"),
                    _ => String::new(),
                })
            }
            _ => "usage: window <w> <h> [x y] | window full".into(),
        }
    }
}
