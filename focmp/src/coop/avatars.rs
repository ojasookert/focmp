//! Real native player artwork/animation. YYC Draw selects its leg sprites in
//! local variables; capture its actual draw output instead of guessing fields.
use crate::engine::{G, V, asset_id, boolean, real, truthy};
use crate::net::AvatarPart;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
static GUN_DRAW: AtomicUsize = AtomicUsize::new(0);
static GUN_ACTIVE: AtomicBool = AtomicBool::new(false);
static GUN_FRAME: LazyLock<Mutex<Vec<AvatarPart>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static GUN_SNAPSHOT: LazyLock<Mutex<Vec<AvatarPart>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static REMOTE_GUNS: LazyLock<Mutex<Vec<Vec<AvatarPart>>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static DRAW: AtomicUsize = AtomicUsize::new(0);
static SPRITE: AtomicUsize = AtomicUsize::new(0);
static SIMPLE: AtomicUsize = AtomicUsize::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static COLOR: AtomicU8 = AtomicU8::new(0);
static FRAME: LazyLock<Mutex<Vec<AvatarPart>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static SNAPSHOT: LazyLock<Mutex<Vec<AvatarPart>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static REMOTES: LazyLock<Mutex<Vec<(u8, Vec<AvatarPart>)>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static TARGETS: LazyLock<Mutex<Vec<i32>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static CACHE: LazyLock<Mutex<HashMap<(i32, u8), V>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
type Event = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);
type Sprite = unsafe extern "C-unwind" fn(*mut c_void, i32, f32, f32, f32, f32, f32, f32, u32, f32);
type Simple = unsafe extern "C-unwind" fn(*mut c_void, i32, f32, f32, f32);
pub fn install(g: G) {
    super::avatar_color::install(g);
    if DRAW.load(Ordering::Relaxed) != 0 {
        return;
    }
    // Verified native YYC adapters in this supported executable. Their ABI is
    // typed floats, unlike the RValue-array builtin wrappers used by the console.
    let base = crate::IMAGE_BASE.load(Ordering::Relaxed);
    for (name, offset, f, slot) in [
        ("yyc_draw_sprite_ext", 0xe262a0, sprite as *const (), &SPRITE),
        ("yyc_draw_sprite", 0xe26200, simple as *const (), &SIMPLE),
    ] {
        match crate::hooks::install_address(name, base + offset, f) {
            Ok(p) => slot.store(p as usize, Ordering::SeqCst),
            Err(e) => {
                crate::log(&e);
                return;
            }
        }
    }
    match crate::hooks::install(g.rt, "gml_Object_objGun_Draw_0", gun_draw as *const ()) {
        Ok(p)=>GUN_DRAW.store(p as usize,Ordering::SeqCst),Err(e)=>crate::log(&e),
    }
    match crate::hooks::install(g.rt, "gml_Object_objPlayer_Draw_0", draw as *const ()) {
        Ok(p) => DRAW.store(p as usize, Ordering::SeqCst),
        Err(e) => crate::log(&e),
    }
}
pub fn weapon_snapshot()->Vec<AvatarPart>{GUN_SNAPSHOT.lock().unwrap().clone()}
pub fn snapshot() -> Vec<AvatarPart> {
    SNAPSHOT.lock().unwrap().clone()
}
pub fn tick(c: &super::Coop) {
    COLOR.store(c.colors[super::MY_ID.load(Ordering::Relaxed) as usize], Ordering::Relaxed);
    let room = &c.room;
    *REMOTE_GUNS.lock().unwrap()=c.remotes.values().filter(|r|r.seen.elapsed().as_secs()<3).filter_map(|r|r.state.as_ref().filter(|s| &s.room==room).map(|s|{
        // The held gun is its own replicated part, independent of body artwork
        // and of whether a muzzle effect happened to be captured this frame.
        let mut parts=Vec::new();
        if !s.gun_sprite.is_empty(){
            let held=s.weapon.iter().find(|p|p.sprite==s.gun_sprite).cloned().unwrap_or_else(||AvatarPart{
                sprite:s.gun_sprite.clone(),frame:s.gun_frame,x:s.gun_x,y:s.gun_y,xs:s.gun_xscale,ys:s.gun_yscale,
                angle:s.gun_angle,blend:0xffffff,alpha:s.gun_alpha,pass:0,
            });
            parts.push(held);
        }
        parts.extend(s.weapon.iter().filter(|p|!p.sprite.starts_with("s_gun_")).cloned());
        parts
    })).collect();
    *REMOTES.lock().unwrap() = c
        .remotes
        .iter()
        .filter(|(_, r)| r.seen.elapsed().as_secs() < 3)
        .filter_map(|(id, r)| {
            r.state
                .as_ref()
                .filter(|s| &s.room == room)
                .map(|s| (c.colors[*id as usize], s.avatar.clone()))
        })
        .collect();
}
unsafe extern "C-unwind" fn gun_draw(s:*mut c_void,o:*mut c_void){
    let original:Event=unsafe{std::mem::transmute(GUN_DRAW.load(Ordering::Relaxed))};
    let local=super::rooms::RUN_SEED.load(Ordering::Relaxed)!=0 && G::new().is_some_and(|g|{
        unsafe{g.rt.builtin_var_of("id",s)}.is_some_and(|gun|g.first("objPlayer").is_some_and(|p|crate::engine::key(&p)==crate::engine::key(&g.get(gun,"Owner"))))
    });
    if local{GUN_FRAME.lock().unwrap().clear();GUN_ACTIVE.store(true,Ordering::Relaxed);}
    unsafe{original(s,o)};
    if local{
        GUN_ACTIVE.store(false,Ordering::Relaxed);*GUN_SNAPSHOT.lock().unwrap()=GUN_FRAME.lock().unwrap().clone();
        // Draw once in the native weapon pass, using the remote weapon's own
        // sprite and pose. Never depend on the local weapon's sprite calls.
        if let Some(g)=G::new(){
            let draw:Sprite=unsafe{std::mem::transmute(SPRITE.load(Ordering::Relaxed))};
            for parts in REMOTE_GUNS.lock().unwrap().clone(){for p in parts{
                unsafe{draw(s,asset_id(g.asset(&p.sprite)),p.frame,p.x,p.y,p.xs,p.ys,p.angle,p.blend,p.alpha)};
            }}
        }
    }
}
unsafe extern "C-unwind" fn draw(s: *mut c_void, o: *mut c_void) {
    let f: Event = unsafe { std::mem::transmute(DRAW.load(Ordering::Relaxed)) };
    if super::rooms::RUN_SEED.load(Ordering::Relaxed) == 0 {
        unsafe { f(s, o) };
        return;
    }
    FRAME.lock().unwrap().clear();
    super::avatar_color::begin_frame();
    TARGETS.lock().unwrap().clear();
    ACTIVE.store(true, Ordering::Relaxed);
    unsafe { f(s, o) };
    ACTIVE.store(false, Ordering::Relaxed);
    *SNAPSHOT.lock().unwrap() = FRAME.lock().unwrap().clone();
    if let Some(g) = G::new() {
        if let Some(player) = g.first("objPlayer") {
            for name in ["body_light", "eyes_light"] {
                let light = g.get(player, name);
                if crate::engine::is_struct(&light) {
                    g.set(light, "blend", real(super::colors::value(COLOR.load(Ordering::Relaxed))));
                }
            }
        }
    }
}
unsafe extern "C-unwind" fn simple(s: *mut c_void, sp: i32, fr: f32, x: f32, y: f32) {
    if super::cursors::hide_native_sprite(sp) { return; }
    if ACTIVE.load(Ordering::Relaxed) || GUN_ACTIVE.load(Ordering::Relaxed) {
        unsafe { sprite(s, sp, fr, x, y, 1.0, 1.0, 0.0, 0xffffff, 1.0) };
    } else {
        let f: Simple = unsafe { std::mem::transmute(SIMPLE.load(Ordering::Relaxed)) };
        unsafe { f(s, sp, fr, x, y) }
    }
}
unsafe extern "C-unwind" fn sprite(s: *mut c_void, sp: i32, fr: f32, x: f32, y: f32, xs: f32, ys: f32, angle: f32, blend: u32, alpha: f32) {
    if super::cursors::hide_native_sprite(sp) { return; }
    let f: Sprite = unsafe { std::mem::transmute(SPRITE.load(Ordering::Relaxed)) };
    if GUN_ACTIVE.load(Ordering::Relaxed) {
        if let Some(g)=G::new(){
            let name=g.sprite_name(real(sp as f64));
            if name.starts_with("s_"){
                let mut frame=GUN_FRAME.lock().unwrap();
                if frame.len()<32{frame.push(AvatarPart{sprite:name,frame:fr,x,y,xs,ys,angle,blend,alpha,pass:0});}
            }
        }
        unsafe{f(s,sp,fr,x,y,xs,ys,angle,blend,alpha)};return;
    }
    if !ACTIVE.load(Ordering::Relaxed) {
        unsafe { f(s, sp, fr, x, y, xs, ys, angle, blend, alpha) };
        return;
    }
    let Some(g) = G::new() else {
        unsafe { f(s, sp, fr, x, y, xs, ys, angle, blend, alpha) };
        return;
    };
    let name = g.sprite_name(real(sp as f64));
    if !name.starts_with("s_player") {
        unsafe { f(s, sp, fr, x, y, xs, ys, angle, blend, alpha) };
        return;
    }
    let target = asset_id(g.call("surface_get_target", &[]));
    let (pass, first) = {
        let mut targets = TARGETS.lock().unwrap();
        match targets.iter().position(|t| *t == target) {
            Some(i) => (i as u8, false),
            None => {
                let i = targets.len() as u8;
                targets.push(target);
                (i, true)
            }
        }
    };
    {
        let mut frame = FRAME.lock().unwrap();
        if frame.len() < 32 {
            frame.push(AvatarPart {
                sprite: name,
                frame: fr,
                x,
                y,
                xs,
                ys,
                angle,
                blend,
                alpha,
                pass,
            });
        }
    }
    // Render the remote parts into the same native pass as the real player.
    // Dummy instances remain invisible position anchors for enemy targeting.
    if first {
        for (color, parts) in REMOTES.lock().unwrap().clone() {
            for p in parts.into_iter().filter(|p| p.pass == pass) {
                let original = g.asset(&p.sprite);
                let colored = tinted(g, s, original, color);
                unsafe { f(s, asset_id(colored), p.frame, p.x, p.y, p.xs, p.ys, p.angle, p.blend, p.alpha) };
                unsafe { super::avatar_color::mask(g, s, f, color, asset_id(original), p.frame, p.x, p.y, p.xs, p.ys, p.angle, p.alpha); }
            }
        }
    }
    let colored = tinted(g, s, real(sp as f64), COLOR.load(Ordering::Relaxed));
    unsafe { f(s, asset_id(colored), fr, x, y, xs, ys, angle, blend, alpha) };
    unsafe { super::avatar_color::mask(g, s, f, COLOR.load(Ordering::Relaxed), sp, fr, x, y, xs, ys, angle, alpha); }
}
/// Bake the game's existing sprite through its native color-grading shader.
/// This preserves texture alpha and animation frames; no new artwork is used.
fn tinted(g: G, context: *mut c_void, original: V, color: u8) -> V {
    let index = asset_id(original);
    let cache_key = (index, color);
    if let Some(v) = CACHE.lock().unwrap().get(&cache_key) {
        return *v;
    }
    let w = g.call("sprite_get_width", &[original]);
    let h = g.call("sprite_get_height", &[original]);
    let count = g.call("sprite_get_number", &[original]).as_f64().unwrap_or(0.0) as usize;
    if count == 0 || count > 128 {
        return original;
    }
    let surface = g.call("surface_create", &[w, h]);
    if !truthy(&g.call("surface_exists", &[surface])) {
        return original;
    }
    let camera = g.call("camera_create_view", &[real(0.0), real(0.0), w, h]);
    let matrices: [V; 3] = std::array::from_fn(|i| g.call("matrix_get", &[real(i as f64)]));
    let shader = g.call("shader_current", &[]);
    let old_blend = g.call("gpu_get_blendenable", &[]);
    let old_color = g.call("draw_get_color", &[]);
    let old_alpha = g.call("draw_get_alpha", &[]);
    let grading = g.asset("__ppf_sh_render_color_grading");
    let uniform = |name: &str, args: &[V]| {
        let mut v = vec![g.call("shader_get_uniform", &[grading, g.s(name)])];
        v.extend_from_slice(args);
        g.call("shader_set_uniform_f", &v);
    };
    let (r, gc, b) = super::colors::PALETTE[color as usize % 8].1;
    g.call("surface_set_target", &[surface]);
    g.call("camera_apply", &[camera]);
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
    ] {
        uniform(&format!("u_{effect}_enable"), &[real(0.0)]);
    }
    uniform("u_time_n_intensity", &[real(0.0), real(1.0)]);
    uniform("u_colorize_enable", &[real(0.0)]);
    // Native player art includes white/gray highlights. A hue rotation leaves
    // those unchanged; map luminance to the chosen hue so they identify players.
    uniform("u_channel_mixer_enable", &[real(1.0)]);
    for (name, channel) in [("red", r), ("green", gc), ("blue", b)] {
        let c = channel as f64 / 255.0;
        uniform(
            &format!("u_channel_mixer_{name}"),
            &[real(c * 0.2126), real(c * 0.7152), real(c * 0.0722)],
        );
    }
    g.call("gpu_set_blendenable", &[boolean(false)]);
    let xo = g.call("sprite_get_xoffset", &[original]);
    let yo = g.call("sprite_get_yoffset", &[original]);
    let mut result = V::undefined();
    for frame in 0..count {
        g.call("draw_clear_alpha", &[real(0.0), real(0.0)]);
        let native: Sprite = unsafe { std::mem::transmute(SPRITE.load(Ordering::Relaxed)) };
        unsafe {
            native(
                context,
                index,
                frame as f32,
                xo.as_f64().unwrap_or(0.0) as f32,
                yo.as_f64().unwrap_or(0.0) as f32,
                1.0,
                1.0,
                0.0,
                0xffffff,
                1.0,
            )
        };
        if frame == 0 {
            result = g.call(
                "sprite_create_from_surface",
                &[surface, real(0.0), real(0.0), w, h, boolean(false), boolean(false), xo, yo],
            );
        } else {
            g.call(
                "sprite_add_from_surface",
                &[result, surface, real(0.0), real(0.0), w, h, boolean(false), boolean(false)],
            );
        }
    }
    uniform("u_channel_mixer_enable", &[real(0.0)]);
    g.call("shader_reset", &[]);
    if asset_id(shader) >= 0 {
        g.call("shader_set", &[shader]);
    }
    g.call("gpu_set_blendenable", &[old_blend]);
    g.call("surface_reset_target", &[]);
    for (i, m) in matrices.into_iter().enumerate() {
        g.call("matrix_set", &[real(i as f64), m]);
    }
    g.call("draw_set_color", &[old_color]);
    g.call("draw_set_alpha", &[old_alpha]);
    g.call("camera_destroy", &[camera]);
    g.call("surface_free", &[surface]);
    if truthy(&g.call("sprite_exists", &[result])) {
        CACHE.lock().unwrap().insert(cache_key, result);
        result
    } else {
        original
    }
}

pub fn describe() -> String {
    let sheets = CACHE.lock().unwrap().len();
    format!(
        "native avatar: {} parts, {} colored sprite sheets, sprites {:?}, sheets {:?}; held weapon draws {:?}",
        snapshot().len(),
        sheets,
        snapshot().iter().map(|p| p.sprite.clone()).collect::<Vec<_>>(),
        CACHE
            .lock()
            .unwrap()
            .iter()
            .map(|((sp, c), v)| (*sp, *c, asset_id(*v)))
            .collect::<Vec<_>>(),
        weapon_snapshot().iter().map(|p|(&p.sprite,p.x,p.y)).collect::<Vec<_>>()
    )
}
