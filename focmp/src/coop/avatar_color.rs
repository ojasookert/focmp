//! Preserve player identity through the scene LUT without recoloring the map.
//! Capture coverage in the native sprite pass, then recolor the lit, processed
//! scene only inside that coverage. Animation, camera transforms and alpha are
//! taken from the actual native draws; no duplicate player artwork is overlaid.
use crate::engine::{G, V, asset_id, boolean, real, truthy};
use std::{
    ffi::c_void,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
    },
};

type Sprite = unsafe extern "C-unwind" fn(*mut c_void, i32, f32, f32, f32, f32, f32, f32, u32, f32);
static POST: AtomicUsize = AtomicUsize::new(0);
static IN_WORLD: AtomicBool = AtomicBool::new(false);
static OUTPUT_TARGET: AtomicI32 = AtomicI32::new(-1);
struct Mask {
    color: u8,
    surface: V,
    camera: V,
    w: f64,
    h: f64,
    ready: bool,
    order: usize,
}
static MASKS: Mutex<Vec<Mask>> = Mutex::new(Vec::new());

pub fn install(g: G) {
    if POST.load(Ordering::Relaxed) == 0 {
        match crate::hooks::install(g.rt, "gml_Object_objCamera_Draw_77", post as *const ()) {
            Ok(f) => POST.store(f as usize, Ordering::SeqCst),
            Err(e) => crate::log(&e),
        }
    }
}
pub fn begin_frame() {
    for mask in MASKS.lock().unwrap().iter_mut() {
        mask.ready = false;
        mask.order = 0;
    }
}
unsafe extern "C-unwind" fn post(s: *mut c_void, o: *mut c_void) {
    let original: unsafe extern "C-unwind" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(POST.load(Ordering::Relaxed)) };
    IN_WORLD.store(
        super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0,
        Ordering::Relaxed,
    );
    if let Some(g) = G::new() {
        OUTPUT_TARGET.store(
            asset_id(g.call("surface_get_target", &[])),
            Ordering::Relaxed,
        );
    }
    unsafe {
        original(s, o);
    }
    IN_WORLD.store(false, Ordering::Relaxed);
    begin_frame(); // Never replay a mask when a player becomes hidden/dead.
}

pub unsafe fn mask(
    g: G,
    context: *mut c_void,
    sprite: Sprite,
    color: u8,
    sp: i32,
    frame: f32,
    x: f32,
    y: f32,
    xs: f32,
    ys: f32,
    angle: f32,
    alpha: f32,
) {
    let app = g.var("application_surface");
    let target = asset_id(g.call("surface_get_target", &[]));
    // Do not capture the separate light/line-of-sight masks as body coverage.
    if target != asset_id(app) || alpha <= 0.0 || !truthy(&g.call("surface_exists", &[app])) {
        return;
    }
    let w = g.call("surface_get_width", &[app]).as_f64().unwrap_or(0.0);
    let h = g.call("surface_get_height", &[app]).as_f64().unwrap_or(0.0);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let mut masks = MASKS.lock().unwrap();
    let i = masks
        .iter()
        .position(|m| m.color == color)
        .unwrap_or_else(|| {
            masks.push(Mask {
                color,
                surface: real(-1.0),
                camera: real(-1.0),
                w: 0.0,
                h: 0.0,
                ready: false,
                order: 0,
            });
            masks.len() - 1
        });
    let order = masks.iter().map(|m| m.order).max().unwrap_or(0) + 1;
    let m = &mut masks[i];
    if m.w != w || m.h != h || !truthy(&g.call("surface_exists", &[m.surface])) {
        if truthy(&g.call("surface_exists", &[m.surface])) {
            g.call("surface_free", &[m.surface]);
        }
        if asset_id(m.camera) >= 0 {
            g.call("camera_destroy", &[m.camera]);
        }
        m.surface = g.call("surface_create", &[real(w), real(h)]);
        m.camera = g.call(
            "camera_create_view",
            &[real(0.0), real(0.0), real(w), real(h)],
        );
        m.w = w;
        m.h = h;
        m.ready = false;
        crate::log(&format!(
            "coop player color: coverage surface {w}x{h} for color {color}"
        ));
    }
    if !truthy(&g.call("surface_exists", &[m.surface])) {
        return;
    }
    let matrices: [V; 3] = std::array::from_fn(|i| g.call("matrix_get", &[real(i as f64)]));
    let shader = g.call("shader_current", &[]);
    g.call("gpu_push_state", &[]);
    g.call("surface_set_target", &[m.surface]);
    if !m.ready {
        g.call("draw_clear_alpha", &[real(0.0), real(0.0)]);
    }
    for (i, matrix) in matrices.iter().enumerate() {
        g.call("matrix_set", &[real(i as f64), *matrix]);
    }
    g.call("shader_reset", &[]);
    g.call("gpu_set_blendenable", &[boolean(true)]);
    g.call(
        "gpu_set_colorwriteenable",
        &[boolean(true), boolean(true), boolean(true), boolean(true)],
    );
    g.call("gpu_set_ztestenable", &[boolean(false)]);
    g.call("gpu_set_zwriteenable", &[boolean(false)]);
    // Standard RGB blending; coverage alpha is src + dst * (1-src).
    g.call(
        "gpu_set_blendmode_ext_sepalpha",
        &[real(5.0), real(6.0), real(2.0), real(6.0)],
    );
    unsafe {
        sprite(context, sp, frame, x, y, xs, ys, angle, 0xffffff, alpha);
    }
    g.call("surface_reset_target", &[]);
    for (i, matrix) in matrices.iter().enumerate() {
        g.call("matrix_set", &[real(i as f64), *matrix]);
    }
    g.call("gpu_pop_state", &[]);
    if asset_id(shader) >= 0 {
        g.call("shader_set", &[shader]);
    }
    m.ready = true;
    m.order = order;
}

pub unsafe fn composite(g: G, s: *mut c_void, o: *mut c_void, args: &[V]) {
    if !IN_WORLD.load(Ordering::Relaxed) {
        return;
    }
    // PPFX also stretches surfaces while building its intermediate passes.
    // Only correct the final presentation to the original output target.
    if asset_id(g.call("surface_get_target", &[])) != OUTPUT_TARGET.load(Ordering::Relaxed) {
        return;
    }
    let Some(camera) = g.first("objCamera") else {
        return;
    };
    let fx = g.get(camera, "ppfx_id");
    let source = g.arr_get(
        g.get(fx, "__stack_surface"),
        g.num(fx, "__stack_index") as usize,
    );
    if crate::engine::key(&source) != crate::engine::key(&args[0]) {
        return;
    }
    let Some(draw_surface) = g.rt.builtin("draw_surface_ext") else {
        return;
    };
    let (Some(x), Some(y), Some(w), Some(h)) = (
        args[1].as_f64(),
        args[2].as_f64(),
        args[3].as_f64(),
        args[4].as_f64(),
    ) else {
        return;
    };
    let sw = g
        .call("surface_get_width", &[source])
        .as_f64()
        .unwrap_or(0.0);
    let sh = g
        .call("surface_get_height", &[source])
        .as_f64()
        .unwrap_or(0.0);
    if sw <= 0.0 || sh <= 0.0 {
        return;
    }
    let mut masks = MASKS.lock().unwrap();
    // When characters overlap, preserve the native sprite draw order rather
    // than the order in which their cached surfaces happened to be created.
    masks.sort_by_key(|m| m.order);
    if !masks.iter().any(|m| m.ready) {
        return;
    }
    let matrices: [V; 3] = std::array::from_fn(|i| g.call("matrix_get", &[real(i as f64)]));
    let shader = g.call("shader_current", &[]);
    let grading = g.asset("__ppf_sh_render_color_grading");
    let uniform = |name: &str, values: &[V]| {
        let mut a = vec![g.call("shader_get_uniform", &[grading, g.s(name)])];
        a.extend_from_slice(values);
        g.call("shader_set_uniform_f", &a);
    };
    let draw = |surface: V, x: f64, y: f64, xs: f64, ys: f64| {
        let mut a = [
            surface,
            real(x),
            real(y),
            real(xs),
            real(ys),
            real(0.0),
            real(0xffffff as f64),
            real(1.0),
        ];
        let mut r = V::undefined();
        unsafe {
            draw_surface(&mut r, s, o, 8, a.as_mut_ptr());
        }
    };
    g.call("gpu_push_state", &[]);
    g.call("gpu_set_blendenable", &[boolean(true)]);
    g.call(
        "gpu_set_colorwriteenable",
        &[boolean(true), boolean(true), boolean(true), boolean(true)],
    );
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
        "color_curves",
        "tone_mapping",
        "colorize",
    ] {
        uniform(&format!("u_{effect}_enable"), &[real(0.0)]);
    }
    uniform("u_time_n_intensity", &[real(0.0), real(1.0)]);
    for m in masks.iter_mut().filter(|m| m.ready) {
        if !truthy(&g.call("surface_exists", &[m.surface])) {
            m.ready = false;
            continue;
        }
        g.call("surface_set_target", &[m.surface]);
        g.call("camera_apply", &[m.camera]);
        g.call("shader_set", &[grading]);
        uniform("u_channel_mixer_enable", &[real(1.0)]);
        let (r, green, b) = super::colors::PALETTE[m.color as usize % 8].1;
        for (name, channel) in [("red", r), ("green", green), ("blue", b)] {
            let c = channel as f64 / 255.0;
            uniform(
                &format!("u_channel_mixer_{name}"),
                &[real(c * 0.2126), real(c * 0.7152), real(c * 0.0722)],
            );
        }
        // Replace coverage RGB with the lit scene's luminance in player hue.
        // Destination alpha restricts this to actual sprite pixels.
        g.call("gpu_set_blendmode_ext", &[real(7.0), real(1.0)]);
        draw(source, 0.0, 0.0, m.w / sw, m.h / sh);
        g.call("surface_reset_target", &[]);
        for (i, matrix) in matrices.iter().enumerate() {
            g.call("matrix_set", &[real(i as f64), *matrix]);
        }
        uniform("u_channel_mixer_enable", &[real(0.0)]);
        g.call("shader_reset", &[]);
        // The coverage operation produced premultiplied RGB.
        g.call("gpu_set_blendmode_ext", &[real(2.0), real(6.0)]);
        draw(m.surface, x, y, w / m.w, h / m.h);
        m.ready = false;
    }
    g.call("gpu_pop_state", &[]);
    if asset_id(shader) >= 0 {
        g.call("shader_set", &[shader]);
    }
}
