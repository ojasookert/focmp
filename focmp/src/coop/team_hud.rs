//! Compact teammate health below the native player HUD and its objective line.
//! Uses existing snapshots.
use crate::engine::{G, V, real, truthy};
use std::ffi::c_void;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

static UI_END: AtomicUsize = AtomicUsize::new(0);
static SURFACE: AtomicUsize = AtomicUsize::new(0);
static COMPOSITING: AtomicBool = AtomicBool::new(false);
static ROWS: Mutex<Option<Rows>> = Mutex::new(None);
#[derive(Clone)]
struct Rows { frame: f64, colors: Vec<u8>, top: f64, right: f64 }
// Native lines under the local health bar: the objective ("OBJECTIVE: DEFEND
// CENTER", objDefendArea Draw GUI, y 32..43) and the boss conditions
// (objBossText Draw GUI, one 16 px line each from boss_text_y_start = 32).
// Teammate rows always start below the objective and a single boss line, so
// the HUD sits in one place; only a taller native stack pushes it further.
const ROWS_TOP: f64 = 50.0;
const BOSS_LINE: f64 = 16.0;
fn rows_top(g: G) -> f64 {
    let Some(boss) = g.first("objBossText") else { return ROWS_TOP };
    let lines = g.arr_len(g.get(boss, "boss_icon_indexes")).min(4) as f64;
    if lines == 0.0 { return ROWS_TOP; }
    let start = g.get(boss, "boss_text_y_start").as_f64().filter(|y| (0.0..200.0).contains(y)).unwrap_or(32.0);
    ROWS_TOP.max(start + BOSS_LINE * lines + 2.0)
}
const ROW_HEIGHT: f64 = 20.0;
const TINT_LEFT: f64 = 8.0;
const TINT_RIGHT: f64 = 136.0;
const BAR_TEXT_LEFT: f64 = 60.0;
// Steam names run to 32 characters; show 20 before cutting.
const NAME_CHARS: usize = 20;

pub fn install(g: G) {
    for (name, hook, slot) in [
        ("gml_Object_objUI_Draw_75", ui_end as *const (), &UI_END),
        ("draw_surface_stretched", composite as *const (), &SURFACE),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(p) => slot.store(p as usize, Ordering::SeqCst),
                Err(e) => crate::log(&e),
            }
        }
    }
}

pub unsafe fn draw(g: G, c: &super::Coop, caller: *mut c_void, other: *mut c_void) {
    *ROWS.lock().unwrap() = None;
    let Some(net) = &c.net else { return };
    let room = g.room_name();
    if super::rooms::RUN_SEED.load(std::sync::atomic::Ordering::Relaxed) == 0
        || super::life::ending()
        || !(room.starts_with("Room_fight") || matches!(room.as_str(), "Room_hub" | "Room_hub_1"))
        || g.first("objPlayer").is_none()
        || (!super::pause::open(g)
            && g.first("objInventory")
                .is_some_and(|i| truthy(&g.get(i, "fullscreen"))))
    {
        return;
    }
    let mut peers: Vec<_> = net.peers.iter().filter(|p| p.id != net.my_id).collect();
    peers.sort_by_key(|p| p.id);
    if peers.is_empty() {
        return;
    }

    let Some(draw_sprite) = g.rt.builtin("draw_sprite_ext") else {
        return;
    };
    let Some(ui) = g.first("objUI") else { return };
    // Player.Draw GUI targets objUI's surface. The GUI-end pass textures it
    // with the native noise, then applies the selected UI post-processing.
    // Drawing straight to the backbuffer skips that entire treatment.
    g.call(
        "method_call",
        &[
            g.get(ui, "surface_set"),
            g.call("array_create", &[real(0.0)]),
        ],
    );
    // The native sprite wrapper dereferences its calling instance even with an
    // explicit frame index. Preserve the camera Draw GUI context.
    let sprite = |mut args: [V; 9]| {
        let mut result = V::undefined();
        unsafe {
            draw_sprite(&mut result, caller, other, 9, args.as_mut_ptr());
        }
    };
    let saved = [
        "draw_get_color",
        "draw_get_alpha",
        "draw_get_font",
        "draw_get_halign",
        "draw_get_valign",
    ]
    .map(|name| g.call(name, &[]));
    let player = g.first("objPlayer").unwrap();
    g.call("draw_set_font", &[g.get(player, "font_mag")]);
    g.call("draw_set_halign", &[real(0.0)]);
    g.call("draw_set_valign", &[real(1.0)]);
    g.call("draw_set_alpha", &[real(1.0)]);

    let mut tint_right = TINT_RIGHT;
    let top = rows_top(g);
    for (row, peer) in peers.iter().enumerate() {
        let x = 10.0;
        let y = top + 6.0 + row as f64 * ROW_HEIGHT;
        let scale = 0.75;
        let name: String = peer
            .name
            .chars()
            .filter(|ch| !ch.is_control() && *ch != '#')
            .take(NAME_CHARS)
            .collect();
        // A long name runs past the bar; widen the player-colour pass to match.
        let width = g.call("string_width", &[g.s(&name)]).as_f64().unwrap_or(0.0) * scale;
        tint_right = tint_right.max((x + width + 2.0).ceil()).min(640.0);
        let remote = c.remotes.get(&peer.id);
        let fresh = remote.is_some_and(|r| r.seen.elapsed().as_secs_f32() < 3.0);
        let state = remote.filter(|_| fresh).and_then(|r| r.state.as_ref());
        let (fraction, value) = match state {
            Some(s) if s.dead => (0.0, "DOWN".to_owned()),
            Some(s) if s.hp_max > 0.0 => (
                (s.hp as f64 / s.hp_max as f64).clamp(0.0, 1.0),
                format!("{:.0}/{:.0}", s.hp.max(0.0), s.hp_max),
            ),
            _ if !fresh => (0.0, "...".to_owned()),
            _ => (0.0, "IN MENU".to_owned()),
        };
        // Exactly the two sprites used by Player.Draw GUI, at 75% scale.
        // Native geometry: 16x horizontal stretch, 2.4x vertical stretch;
        // the back sprite is offset two pixels up/left of the health fill.
        let bar_y = y + 10.0;
        sprite([
            g.asset("s_health_bar_fill_back_left"),
            real(0.0),
            real(x),
            real(bar_y - 2.0 * scale),
            real(16.0 * scale),
            real(2.4 * scale),
            real(0.0),
            real(0xffffff as f64),
            real(1.0),
        ]);
        if fraction > 0.0 {
            sprite([
                g.asset("s_health_bar_fill_left"),
                real(0.0),
                real(x + 2.0 * scale),
                real(bar_y),
                real(16.0 * scale * fraction),
                real(2.4 * scale),
                real(0.0),
                real(0xffffff as f64),
                real(1.0),
            ]);
        }
        native_text(g, x, y, &name, scale);
        native_text(g, x + 68.0 * scale, bar_y - scale, &value, scale);
    }
    for (name, value) in [
        "draw_set_color",
        "draw_set_alpha",
        "draw_set_font",
        "draw_set_halign",
        "draw_set_valign",
    ]
    .into_iter()
    .zip(saved)
    {
        g.call(name, &[value]);
    }
    g.call("surface_reset_target", &[]);
    *ROWS.lock().unwrap() = Some(Rows {
        frame: g.num(ui, "frame"),
        colors: peers.iter().map(|p| c.colors[p.id as usize]).collect(),
        top,
        right: tint_right,
    });
}

unsafe extern "C-unwind" fn ui_end(s: *mut c_void, o: *mut c_void) {
    let original: unsafe extern "C-unwind" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(UI_END.load(Ordering::Relaxed)) };
    COMPOSITING.store(true, Ordering::Relaxed);
    unsafe { original(s, o) };
    COMPOSITING.store(false, Ordering::Relaxed);
    if let Some(g) = G::new() {
        super::menu::draw_color_labels(g, s, o);
        if let Ok(c) = super::COOP.try_lock() {
            super::cursors::draw(g, &c);
        }
    }
}

// The game's LUT replaces input hues. Apply player colors only to our rows
// when the fully processed UI surface is composited, preserving its native
// noise, bloom, scanlines and alpha. Split into non-overlapping rectangles so
// translucent pixels are never blended twice. No extra surface or FX pass.
unsafe extern "C-unwind" fn composite(
    r: *mut V,
    s: *mut c_void,
    o: *mut c_void,
    n: i32,
    a: *mut V,
) {
    let original: crate::gm::BuiltinFn =
        unsafe { std::mem::transmute(SURFACE.load(Ordering::Relaxed)) };
    let handled = if COMPOSITING.load(Ordering::Relaxed) && n == 5 && !a.is_null() {
        G::new()
            .is_some_and(|g| unsafe { colored_surface(g, s, o, std::slice::from_raw_parts(a, 5)) })
    } else {
        false
    };
    if handled {
        unsafe {
            *r = V::undefined();
        }
    } else {
        unsafe { original(r, s, o, n, a) };
        if n == 5 && !a.is_null() {
            if let Some(g) = G::new() {
                unsafe {
                    super::avatar_color::composite(g, s, o, std::slice::from_raw_parts(a, 5));
                }
            }
        }
    }
}

unsafe fn colored_surface(g: G, s: *mut c_void, o: *mut c_void, a: &[V]) -> bool {
    let Some(Rows { frame, colors, top: rows_top, right }) = ROWS.lock().unwrap().clone() else {
        return false;
    };
    let Some(ui) = g.first("objUI") else {
        return false;
    };
    if frame != g.num(ui, "frame") || colors.is_empty() {
        return false;
    }
    let fx = g.get(ui, "ppfx_ui_id");
    let surface = g.arr_get(
        g.get(fx, "__stack_surface"),
        g.num(fx, "__stack_index") as usize,
    );
    if crate::engine::key(&surface) != crate::engine::key(&a[0]) {
        return false;
    }
    let Some(part) = g.rt.builtin("draw_surface_part_ext") else {
        return false;
    };
    let w = g
        .call("surface_get_width", &[surface])
        .as_f64()
        .unwrap_or(0.0);
    let h = g
        .call("surface_get_height", &[surface])
        .as_f64()
        .unwrap_or(0.0);
    let (Some(x), Some(y), Some(dw), Some(dh)) =
        (a[1].as_f64(), a[2].as_f64(), a[3].as_f64(), a[4].as_f64())
    else {
        return false;
    };
    if w <= 0.0 || h <= 0.0 || dw <= 0.0 || dh <= 0.0 {
        return false;
    }
    let shader = g.call("shader_current", &[]);
    // Do not replace an unrelated caller's shader.
    if crate::engine::asset_id(shader) >= 0 {
        return false;
    }
    let grading = g.asset("__ppf_sh_render_color_grading");
    let uniform = |name: &str, values: &[V]| {
        let mut args = vec![g.call("shader_get_uniform", &[grading, g.s(name)])];
        args.extend_from_slice(values);
        g.call("shader_set_uniform_f", &args);
    };
    let draw_part = |left: f64, top: f64, right: f64, bottom: f64| {
        // Layout is in the game's 640x360 GUI space; allow PPFX resolution scaling.
        let mut args = [
            surface,
            real(left * w / 640.0),
            real(top * h / 360.0),
            real((right - left) * w / 640.0),
            real((bottom - top) * h / 360.0),
            real(x + left * dw / 640.0),
            real(y + top * dh / 360.0),
            real(dw / w),
            real(dh / h),
            real(0xffffff as f64),
            g.call("draw_get_alpha", &[]),
        ];
        let mut result = V::undefined();
        unsafe {
            part(&mut result, s, o, args.len() as i32, args.as_mut_ptr());
        }
    };
    let bottom = rows_top + colors.len() as f64 * ROW_HEIGHT;
    draw_part(0.0, 0.0, 640.0, rows_top);
    draw_part(0.0, rows_top, TINT_LEFT, bottom);
    if right < 640.0 {
        draw_part(right, rows_top, 640.0, bottom);
    }
    draw_part(0.0, bottom, 640.0, 360.0);
    g.call("shader_set", &[grading]);
    for effect in [
        "lut",
        "color_balance",
        "exposure",
        "brightness",
        "contrast",
        "channel_mixer",
        "lift_gamma_gain",
        "hueshift",
        "saturation",
        "colortint",
        "posterization",
        "invert_colors",
        "curves",
        "tone_mapping",
        "colorize",
    ] {
        uniform(&format!("u_{effect}_enable"), &[real(0.0)]);
    }
    uniform("u_time_n_intensity", &[real(0.0), real(1.0)]);
    uniform("u_channel_mixer_enable", &[real(1.0)]);
    for (row, color) in colors.iter().enumerate() {
        let (r, green, b) = super::colors::PALETTE[*color as usize % 8].1;
        let top = rows_top + row as f64 * ROW_HEIGHT;
        // Native text has a pink face and a white offset underneath. Map its
        // red/green contrast to the player tint, preserving neutral highlights
        // as white instead of tinting the shadow along with the foreground.
        for bright in [true, false] {
            for (name, channel) in [("red", r), ("green", green), ("blue", b)] {
                let c = channel as f64 / 255.0;
                let c = if bright { 0.25 + c * 0.75 } else { c };
                let weights = if bright {
                    [c, 1.0 - c, 0.0]
                } else {
                    [c * 0.2126, c * 0.7152, c * 0.0722]
                };
                uniform(&format!("u_channel_mixer_{name}"), &weights.map(real));
            }
            if bright {
                draw_part(TINT_LEFT, top, right, top + 10.0);
                draw_part(BAR_TEXT_LEFT, top + 10.0, right, top + ROW_HEIGHT);
            } else {
                draw_part(TINT_LEFT, top + 10.0, BAR_TEXT_LEFT, top + ROW_HEIGHT);
            }
        }
    }
    uniform("u_channel_mixer_enable", &[real(0.0)]);
    g.call("shader_reset", &[]);
    true
}

// Native health text: white layer one unscaled pixel up/left of #ff0546. The final color pass maps
// the foreground to the player color while keeping the lower layer near white.
fn native_text(g: G, x: f64, y: f64, text: &str, scale: f64) {
    for (dx, dy, color) in [(-1.0, -1.0, 0xffffff), (0.0, 0.0, 0x4605ff)] {
        g.call("draw_set_color", &[real(color as f64)]);
        g.call(
            "draw_text_transformed",
            &[
                real(x + dx),
                real(y + dy),
                g.s(text),
                real(scale),
                real(scale),
                real(0.0),
            ],
        );
    }
}
