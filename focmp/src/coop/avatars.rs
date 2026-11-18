//! Real native player artwork/animation. YYC Draw selects its leg sprites in
//! local variables; capture its actual draw output instead of guessing fields.
use crate::engine::{G, V, asset_id, boolean, real, truthy};
use crate::net::{AvatarPart, ReloadText};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
static GUN_DRAW: AtomicUsize = AtomicUsize::new(0);
static GUN_ACTIVE: AtomicBool = AtomicBool::new(false);
static GUN_FRAME: LazyLock<Mutex<Vec<AvatarPart>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static GUN_SNAPSHOT: LazyLock<Mutex<Vec<AvatarPart>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static REMOTE_GUNS: LazyLock<Mutex<Vec<Vec<AvatarPart>>>> = LazyLock::new(|| Mutex::new(Vec::new()));
// objGun Draw prints its "RELOADING" label with a built-in text call, which the
// sprite capture above never sees. Replicate that one call for teammates.
static TEXT: AtomicUsize = AtomicUsize::new(0);
static GUN_TEXT_FRAME: Mutex<Option<ReloadText>> = Mutex::new(None);
static GUN_TEXT_SNAPSHOT: Mutex<Option<(Instant, ReloadText)>> = Mutex::new(None);
static REMOTE_GUN_TEXTS: Mutex<Vec<ReloadText>> = Mutex::new(Vec::new());
static DRAW: AtomicUsize = AtomicUsize::new(0);
static SPRITE: AtomicUsize = AtomicUsize::new(0);
static SIMPLE: AtomicUsize = AtomicUsize::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static DEAD_SPRITE: AtomicI32 = AtomicI32::new(-1);
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
    // Verified typed-float YYC adapters, not the RValue-array builtin wrappers.
    // Their RVAs move when game code changes; require a unique instruction signature.
    let Ok(image) = crate::pe::Image::parse(crate::win::exe_base()) else { return; };
    const EXT: &[u8] = &[
        0x48, 0x89, 0x5c, 0x24, 0x10, 0x57, 0x48, 0x83, 0xec, 0x70, 0x0f, 0x29,
        0x74, 0x24, 0x60, 0x0f, 0x57, 0xc0, 0x0f, 0x2f, 0xc2, 0x8b, 0xfa, 0x48,
        0x8b, 0xd9, 0x0f, 0x29, 0x7c, 0x24, 0x50, 0x0f, 0x28, 0xf2, 0x0f, 0x28,
        0xfb, 0x76, 0x1f, 0x83, 0x79, 0x74, 0x01, 0x74, 0x11,
    ];
    const BASIC: &[u8] = &[
        0x40, 0x53, 0x48, 0x83, 0xec, 0x40, 0x0f, 0x29, 0x74, 0x24, 0x30, 0x0f,
        0x57, 0xc0, 0x0f, 0x2f, 0xc2, 0x8b, 0xda, 0x0f, 0x29, 0x7c, 0x24, 0x20,
        0x0f, 0x28, 0xf2, 0x0f, 0x28, 0xfb, 0x76, 0x29, 0x83, 0x79, 0x74, 0x01,
        0x74, 0x1b,
    ];
    for (name, signature, f, slot) in [
        ("yyc_draw_sprite_ext", EXT, sprite as *const (), &SPRITE),
        ("yyc_draw_sprite", BASIC, simple as *const (), &SIMPLE),
    ] {
        if slot.load(Ordering::Relaxed) != 0 { continue; }
        let Some(address) = image.unique_text(signature, &[]) else {
            crate::log(&format!("avatars: unsupported or ambiguous {name} adapter"));
            return;
        };
        match crate::hooks::install_address(name, address, f) {
            Ok(p) => slot.store(p as usize, Ordering::SeqCst),
            Err(e) => {
                crate::log(&e);
                return;
            }
        }
    }
    match crate::hooks::install(g.rt, "draw_text_transformed_color", gun_text as *const ()) {
        Ok(p)=>TEXT.store(p as usize,Ordering::SeqCst),Err(e)=>crate::log(&e),
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
/// The label from the latest local gun draw. A gun that stopped drawing (dead,
/// hidden, another room) must not leave a stale label on teammates' screens.
pub fn reload_text_snapshot()->Option<ReloadText>{
    GUN_TEXT_SNAPSHOT.lock().unwrap().as_ref().filter(|(at,_)|at.elapsed()<Duration::from_millis(250)).map(|(_,text)|text.clone())
}
/// A peer names the font its label used. Resolve at most a handful of names,
/// so a hostile stream of made-up names cannot grow a cache without bound.
fn label_font(g:G,name:&str)->Option<V>{
    static FONTS:Mutex<Vec<(String,Option<V>)>>=Mutex::new(Vec::new());
    if name.is_empty(){return None;}
    let mut fonts=FONTS.lock().unwrap();
    if let Some((_,font))=fonts.iter().find(|(known,_)|known==name){return *font;}
    if fonts.len()>=8{return None;}
    let font=g.call("asset_get_index",&[g.s(name)]);
    let font=(font.as_f64().is_none_or(|v|v>=0.0)&&truthy(&g.call("font_exists",&[font]))).then_some(font);
    fonts.push((name.to_owned(),font));
    font
}
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
    *REMOTE_GUN_TEXTS.lock().unwrap()=c.remotes.values().filter(|r|r.seen.elapsed().as_secs()<3)
        .filter_map(|r|r.state.as_ref().filter(|s|&s.room==room&&!s.dead).and_then(|s|s.gun_reload_text.clone())).collect();
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
    if local{GUN_FRAME.lock().unwrap().clear();*GUN_TEXT_FRAME.lock().unwrap()=None;GUN_ACTIVE.store(true,Ordering::Relaxed);}
    unsafe{original(s,o)};
    if local{
        GUN_ACTIVE.store(false,Ordering::Relaxed);*GUN_SNAPSHOT.lock().unwrap()=GUN_FRAME.lock().unwrap().clone();
        *GUN_TEXT_SNAPSHOT.lock().unwrap()=GUN_TEXT_FRAME.lock().unwrap().take().map(|text|(Instant::now(),text));
        // Draw once in the native weapon pass, using the remote weapon's own
        // sprite and pose. Never depend on the local weapon's sprite calls.
        if let Some(g)=G::new(){
            let draw:Sprite=unsafe{std::mem::transmute(SPRITE.load(Ordering::Relaxed))};
            for parts in REMOTE_GUNS.lock().unwrap().clone(){for p in parts{
                unsafe{draw(s,asset_id(g.asset(&p.sprite)),p.frame,p.x,p.y,p.xs,p.ys,p.angle,p.blend,p.alpha)};
            }}
            // Native guns set no font or alignment for this label: it inherits
            // whatever the previous drawer left behind. Use the sender's state.
            let address=TEXT.load(Ordering::Relaxed);
            let label=unsafe{g.rt.builtin_var_of("id",s)}.map(|gun|g.get(gun,"txt_reloading"));
            let texts=REMOTE_GUN_TEXTS.lock().unwrap().clone();
            if let (true,false,Some(label))=(address!=0,texts.is_empty(),label.filter(|v|v.as_str().is_some())){
                let text:crate::gm::BuiltinFn=unsafe{std::mem::transmute(address)};
                let state=["draw_get_font","draw_get_halign","draw_get_valign"].map(|name|g.call(name,&[]));
                for t in texts{
                    if let Some(font)=label_font(g,&t.font){g.call("draw_set_font",&[font]);}
                    g.call("draw_set_halign",&[real(t.halign as f64)]);g.call("draw_set_valign",&[real(t.valign as f64)]);
                    let mut args=[real(t.x as f64),real(t.y as f64),label,real(t.xs as f64),real(t.ys as f64),real(t.angle as f64),
                        real(t.colors[0] as f64),real(t.colors[1] as f64),real(t.colors[2] as f64),real(t.colors[3] as f64),real(t.alpha as f64)];
                    let mut result=V::undefined();
                    unsafe{text(&mut result,s,o,args.len() as i32,args.as_mut_ptr())};
                }
                for (name,value) in ["draw_set_font","draw_set_halign","draw_set_valign"].into_iter().zip(state){g.call(name,&[value]);}
            }
        }
    }
}
unsafe extern "C-unwind" fn gun_text(r:*mut V,s:*mut c_void,o:*mut c_void,n:i32,a:*mut V){
    let original:crate::gm::BuiltinFn=unsafe{std::mem::transmute(TEXT.load(Ordering::Relaxed))};
    // draw_text_transformed_color(x, y, string, xscale, yscale, angle, c1, c2, c3, c4, alpha)
    if GUN_ACTIVE.load(Ordering::Relaxed)&&n==11&&!a.is_null(){
        let args=unsafe{std::slice::from_raw_parts(a,11)};
        let number=|i:usize|args[i].as_f64().unwrap_or(0.0);
        if let Some(g)=G::new(){
            let align=|name:&str|g.call(name,&[]).as_f64().filter(|v|(0.0..=2.0).contains(v)).unwrap_or(0.0) as u8;
            let font=g.call("draw_get_font",&[]);
            let font=if font.as_f64().is_none_or(|v|v>=0.0)&&truthy(&g.call("font_exists",&[font])){g.str_of(&g.call("font_get_name",&[font]))}else{String::new()};
            *GUN_TEXT_FRAME.lock().unwrap()=Some(ReloadText{
                x:number(0) as f32,y:number(1) as f32,xs:number(3) as f32,ys:number(4) as f32,angle:number(5) as f32,
                colors:[6,7,8,9].map(|i|number(i) as u32&0xff_ffff),alpha:number(10) as f32,
                halign:align("draw_get_halign"),valign:align("draw_get_valign"),font,
            });
        }
    }
    unsafe{original(r,s,o,n,a)}
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
    // Player death switches to native enemy corpse artwork. The old s_player
    // filter captured only its shadow, then hid the fallback body on peers.
    let dead_sprite = G::new().and_then(|g| {
        g.first("objPlayer").filter(|p| truthy(&g.get(*p, "dead")))
            .map(|p| asset_id(g.get(p, "sprite_index")))
    }).unwrap_or(-1);
    DEAD_SPRITE.store(dead_sprite, Ordering::Relaxed);
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
    let tint = name.starts_with("s_player");
    let status = matches!(name.as_str(), "s_frozen_status" | "s_weak_status");
    if !tint && !status && sp != DEAD_SPRITE.load(Ordering::Relaxed) {
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
                let tint = p.sprite.starts_with("s_player");
                let colored = if tint { tinted(g, s, original, color) } else { original };
                unsafe { f(s, asset_id(colored), p.frame, p.x, p.y, p.xs, p.ys, p.angle, p.blend, p.alpha) };
                if tint { unsafe { super::avatar_color::mask(g, s, f, color, asset_id(original), p.frame, p.x, p.y, p.xs, p.ys, p.angle, p.alpha); } }
            }
        }
    }
    let colored = if tint { tinted(g, s, real(sp as f64), COLOR.load(Ordering::Relaxed)) } else { real(sp as f64) };
    unsafe { f(s, asset_id(colored), fr, x, y, xs, ys, angle, blend, alpha) };
    if tint { unsafe { super::avatar_color::mask(g, s, f, COLOR.load(Ordering::Relaxed), sp, fr, x, y, xs, ys, angle, alpha); } }
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
        "native avatar: {} parts, {} colored sprite sheets, sprites {:?}, sheets {:?}; held weapon draws {:?}; reload label {:?}; remote reload labels {:?}",
        snapshot().len(),
        sheets,
        snapshot().iter().map(|p| p.sprite.clone()).collect::<Vec<_>>(),
        CACHE
            .lock()
            .unwrap()
            .iter()
            .map(|((sp, c), v)| (*sp, *c, asset_id(*v)))
            .collect::<Vec<_>>(),
        weapon_snapshot().iter().map(|p|(&p.sprite,p.x,p.y)).collect::<Vec<_>>(),
        reload_text_snapshot(),
        REMOTE_GUN_TEXTS.lock().unwrap().clone()
    )
}
