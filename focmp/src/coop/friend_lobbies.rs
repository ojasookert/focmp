//! Native-menu friend cards. Mutations run only at the start of Menu.Step.
use crate::{engine::{G, V, real, boolean, truthy, key}, hooks::ScriptFn, steam::friends::{Browser, Lobby}};
use std::{collections::HashMap, ffi::c_void, sync::{LazyLock, Mutex, atomic::{AtomicUsize, Ordering}}, time::Instant};

#[derive(Default)]
pub(super) struct Panel {
    browser: Browser,
    lobbies: Vec<Lobby>,
    page: Option<u64>,
    preview: Option<Vec<Lobby>>,
    offset: f64,
    target: f64,
    last_tick: Option<Instant>,
    dragging: bool,
    drag_grab: f64,
    last_focus: Option<i32>,
}
// Native buttons occupy x=32..147. Keep the title and the mascot (x>=365)
// unobstructed; the viewport stays bottom-aligned with Back.
const LEFT: f64 = 165.0;
const RIGHT: f64 = 352.0;
const BOTTOM: f64 = 317.0;
const ROW_STEP: f64 = 45.0;
const ROW_HEIGHT: f64 = 40.0;
const AVATAR_SIZE: f64 = 24.0;
const NAME_X: f64 = LEFT + 44.0;
const VIEW_HEIGHT: f64 = 155.0; // Three rows, their gaps, then half a fourth.
const SCROLL_X: f64 = 358.0;
fn content_height(count: usize) -> f64 { if count == 0 { 0.0 } else { count as f64 * ROW_STEP - 5.0 } }
fn top(count: usize) -> f64 { BOTTOM - content_height(count).min(VIEW_HEIGHT) }
static BUTTON: AtomicUsize = AtomicUsize::new(0);
static LAYOUT: AtomicUsize = AtomicUsize::new(0);
struct Avatar { owner: u64, pixels: Option<Vec<u8>>, x: f64, y: f64, accent: f64, clip_top: f64 }
static AVATARS: Mutex<Vec<Avatar>> = Mutex::new(Vec::new());
static SPRITES: LazyLock<Mutex<HashMap<u64, V>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn install(g: G) {
    for (name, hook, slot) in [
        ("gml_Script_draw_button_menu@Menu_class@Menu_class", button as *const (), &BUTTON),
        ("gml_Script_update_item_ys@Menu_class@Menu_class", layout as *const (), &LAYOUT),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(p) => slot.store(p as usize, Ordering::SeqCst), Err(e) => crate::log(&e),
            }
        }
    }
}

unsafe extern "C-unwind" fn button(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    if let Some(g) = G::new() {
        let page = V { v: s as u64, flags: 0, kind: crate::gm::KIND_OBJECT };
        if g.str_of(&g.get(page, "page_name")) == "page_coop" && n > 0 && !a.is_null() && !unsafe { *a }.is_null() {
            if let Some(i) = unsafe { **a }.as_f64() {
                let items = g.get(page, "items");
                if i >= 0.0 && i < g.arr_len(items) as f64 && truthy(&g.get(g.arr_get(items, i as usize), "focmp_friend_card")) {
                    if !r.is_null() { unsafe { *r = V::undefined() }; }
                    return r;
                }
            }
        }
    }
    let f: ScriptFn = unsafe { std::mem::transmute(BUTTON.load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}

unsafe extern "C-unwind" fn layout(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    if let Some(g) = G::new() {
        let page = V { v: s as u64, flags: 0, kind: crate::gm::KIND_OBJECT };
        if g.str_of(&g.get(page, "page_name")) == "page_coop" {
            // Native y_pos2 uses the total item count. Cards must never push
            // the existing bottom-aligned host/join/back column upwards.
            let items = g.get(page, "items");
            for i in 0..3.min(g.arr_len(items)) {
                let it = g.arr_get(items, i);
                let y = 237.0 + i as f64 * 29.0;
                for (name, value) in [("y1", y), ("y_sel", y), ("y2", y + 23.0), ("y_text", y + 12.5)] { g.set(it, name, real(value)); }
            }
            if !r.is_null() { unsafe { *r = V::undefined() }; }
            return r;
        }
    }
    let f: ScriptFn = unsafe { std::mem::transmute(LAYOUT.load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}

impl Panel {
    pub fn lobby(&self, index: usize) -> Option<u64> { self.lobbies.get(index).map(|l| l.id) }

    pub fn preview(&mut self, ids: &[&str]) -> String {
        if !crate::config::load().debug_console { return "debug console required".into(); }
        if ids == ["off"] { self.preview = None; self.browser.pause(); return "friend preview disabled".into(); }
        if ids.len() > 99 { return "preview IDs (1..99), or off".into(); }
        let mut lobbies = Vec::new();
        for id in ids {
            let Some(id) = id.parse::<u64>().ok().filter(|id| (1..100).contains(id)) else { return "preview IDs must be 1..99".into() };
            if lobbies.iter().any(|l: &Lobby| l.id == id) { return "duplicate preview ID".into(); }
            self.browser.preview_avatar(id);
            lobbies.push(Lobby { id, owner: id, name: format!("Friend {id}"), players: 1 + (id % 2) as u8 });
        }
        self.preview = Some(lobbies);
        "friend preview queued for next Menu.Step (Steam transport only)".into()
    }

    pub fn update(&mut self, g: G, page: V, active: bool) {
        if !crate::engine::is_struct(&page) { return; }
        let enabled = active && crate::config::load().transport == crate::config::Transport::Steam;
        let next = if enabled {
            self.preview.clone().unwrap_or_else(|| self.browser.poll().unwrap_or_else(|| self.lobbies.clone()))
        } else {
            self.browser.pause();
            AVATARS.lock().unwrap().clear();
            vec![]
        };
        let items = g.get(page, "items");
        if g.arr_len(items) < 3 { return; }
        if self.lobbies != next || self.page != Some(key(&page)) || g.arr_len(items) != 3 + next.len() {
            let focus = g.num(page, "i_infocus") as i32;
            let selected = (focus >= 3).then(|| self.lobby((focus - 3) as usize)).flatten();
            g.call("array_resize", &[items, real(3.0)]);
            super::menu::layout(g, items);
            for (i, lobby) in next.iter().enumerate() {
                let item = super::menu::clone_ui_value(g, g.arr_get(items, 0));
                g.set(item, "focmp_friend_card", boolean(true));
                g.set(item, "script_var", g.s(&format!("page_coop_act_friend_{i}")));
                g.set(item, "text", g.s(&lobby.name));
                g.set(item, "i", real((3 + i) as f64));
                g.call("array_push", &[items, item]);
            }
            let focus = selected.and_then(|id| next.iter().position(|l| l.id == id)).map(|i| (3 + i) as i32)
                .unwrap_or(if (0..3).contains(&focus) { focus } else { 2 });
            g.set(page, "i_infocus", real(focus as f64));
            g.set(page, "item_sel", boolean(false));
            g.set(page, "item_sel_i", real(-4.0));
            self.lobbies = next;
            self.page = Some(key(&page));
            self.last_focus = None;
        }
        self.scroll(g, page, items);
    }

    fn scroll(&mut self, g: G, page: V, items: V) {
        let count = self.lobbies.len();
        let top = top(count);
        let height = BOTTOM - top;
        let max_offset = (content_height(count) - height).max(0.0);
        let focus = g.num(page, "i_infocus") as i32;
        if self.last_focus != Some(focus) && focus >= 3
            && !truthy(&g.get(g.get(page, "controller"), "mouse_active")) {
            let y = (focus - 3) as f64 * ROW_STEP;
            if y < self.target { self.target = y; }
            if y + ROW_HEIGHT > self.target + height { self.target = y + ROW_HEIGHT - height; }
        }
        self.last_focus = Some(focus);
        let mx = g.call("device_mouse_x_to_gui", &[real(0.0)]).as_f64().unwrap_or(-1.0);
        let my = g.call("device_mouse_y_to_gui", &[real(0.0)]).as_f64().unwrap_or(-1.0);
        if max_offset > 0.0 && (LEFT..=SCROLL_X + 4.0).contains(&mx) && (top..=BOTTOM).contains(&my) {
            if truthy(&g.call("mouse_wheel_down", &[])) { self.target += ROW_STEP; }
            if truthy(&g.call("mouse_wheel_up", &[])) { self.target -= ROW_STEP; }
        }
        if max_offset > 0.0 {
            let thumb_h = (height * height / content_height(count)).max(12.0);
            let thumb_y = top + self.offset / max_offset * (height - thumb_h);
            if (SCROLL_X - 4.0..=SCROLL_X + 4.0).contains(&mx) && (top..=BOTTOM).contains(&my)
                && truthy(&g.call("mouse_check_button_pressed", &[real(1.0)])) {
                self.dragging = true;
                self.drag_grab = if (thumb_y..=thumb_y + thumb_h).contains(&my) { my - thumb_y } else { thumb_h / 2.0 };
            }
            if !truthy(&g.call("mouse_check_button", &[real(1.0)])) { self.dragging = false; }
            if self.dragging { self.target = (my - top - self.drag_grab) / (height - thumb_h) * max_offset; }
        } else { self.dragging = false; }
        self.target = self.target.clamp(0.0, max_offset);
        let now = Instant::now();
        let dt = self.last_tick.map_or(1.0 / 60.0, |t| now.duration_since(t).as_secs_f64().min(0.1));
        self.last_tick = Some(now);
        self.offset += (self.target - self.offset) * (1.0 - (-20.0 * dt).exp());
        if (self.target - self.offset).abs() < 0.15 || self.dragging { self.offset = self.target; }
        self.offset = self.offset.clamp(0.0, max_offset);
        for i in 0..count {
            let y = top + i as f64 * ROW_STEP - self.offset.round();
            let visible = y < BOTTOM && y + ROW_HEIGHT > top;
            // Clip hitboxes too: hidden portions must never intercept clicks.
            let x = if visible { LEFT } else { -1000.0 };
            let y1 = if visible { y.max(top) } else { -1000.0 };
            let y2 = if visible { (y + ROW_HEIGHT).min(BOTTOM) } else { -960.0 };
            let item = g.arr_get(items, i + 3);
            for (name, value) in [("x1", x), ("x2", x + RIGHT - LEFT), ("y1", y1), ("y2", y2),
                ("x_text", x + NAME_X - LEFT), ("y_text", y + ROW_HEIGHT / 2.0), ("y_sel", y)] { g.set(item, name, real(value)); }
        }
    }

    pub fn draw(&self, g: G, page: V) {
        if g.str_of(&g.get(page, "page_name")) != "page_coop" || self.lobbies.is_empty() { return; }
        let saved = ["draw_get_color", "draw_get_alpha", "draw_get_font", "draw_get_halign", "draw_get_valign"].map(|s| g.call(s, &[]));
        g.call("draw_set_alpha", &[real(1.0)]);
        g.call("draw_set_font", &[g.get(page, "font_menu")]);
        g.call("draw_set_halign", &[real(0.0)]);
        g.call("draw_set_valign", &[real(1.0)]);
        let accent = g.num(page, "main_color");
        let text = |x, y, s: &str, color| { g.call("draw_set_color", &[real(color)]); g.call("draw_text", &[real(x), real(y), g.s(s)]); };
        let top = top(self.lobbies.len());
        let scissor = clip(g, top);
        let mut avatars = AVATARS.lock().unwrap();
        avatars.clear();
        for (i, lobby) in self.lobbies.iter().enumerate() {
            let y = top + i as f64 * ROW_STEP - self.offset.round();
            if y >= BOTTOM || y + ROW_HEIGHT <= top { continue; }
            let selected = g.num(page, "i_infocus") as usize == i + 3;
            g.call("draw_set_color", &[real(if selected { accent } else { rgb(17, 26, 29) })]);
            g.call("draw_rectangle", &[real(LEFT), real(y), real(RIGHT), real(y + ROW_HEIGHT), boolean(false)]);
            g.call("draw_set_color", &[real(if selected { accent } else { rgb(52, 80, 82) })]);
            g.call("draw_rectangle", &[real(LEFT), real(y), real(RIGHT), real(y + ROW_HEIGHT), boolean(true)]);
            let occupancy = format!("{} / 3", lobby.players);
            let slots_width = g.call("string_width", &[g.s(&occupancy)]).as_f64().unwrap_or(36.0);
            let mut name = lobby.name.clone();
            while !name.is_empty() && g.call("string_width", &[g.s(&name)]).as_f64().unwrap_or(0.0) > RIGHT - 16.0 - slots_width - NAME_X { name.pop(); }
            text(NAME_X, y + ROW_HEIGHT / 2.0, &name, if selected { rgb(8, 14, 17) } else { rgb(239, 246, 241) });
            g.call("draw_set_halign", &[real(2.0)]);
            text(RIGHT - 8.0, y + ROW_HEIGHT / 2.0, &occupancy, if selected { rgb(8, 14, 17) } else { rgb(146, 163, 163) });
            g.call("draw_set_halign", &[real(0.0)]);
            let pixels = if SPRITES.lock().unwrap().contains_key(&lobby.owner) { None } else { self.browser.pixels(lobby.owner).map(|p| p.to_vec()) };
            avatars.push(Avatar { owner: lobby.owner, pixels, x: LEFT + 8.0, y: y + 8.0, accent, clip_top: top });
        }
        g.call("gpu_set_scissor", &[scissor]);
        let height = BOTTOM - top;
        let content = content_height(self.lobbies.len());
        if content > height {
            let thumb_h = (height * height / content).max(12.0);
            let thumb_y = (top + self.offset / (content - height) * (height - thumb_h)).round();
            g.call("draw_set_color", &[real(rgb(40, 62, 64))]);
            g.call("draw_rectangle", &[real(SCROLL_X), real(top), real(SCROLL_X + 2.0), real(BOTTOM), boolean(false)]);
            g.call("draw_set_color", &[real(rgb(146, 163, 163))]);
            g.call("draw_rectangle", &[real(SCROLL_X), real(thumb_y), real(SCROLL_X + 2.0), real(thumb_y + thumb_h), boolean(false)]);
        }
        for (s, v) in ["draw_set_color", "draw_set_alpha", "draw_set_font", "draw_set_halign", "draw_set_valign"].into_iter().zip(saved) { g.call(s, &[v]); }
    }
}

// Scissors use render-target pixels, not GUI coordinates. Both the menu's
// PPFX target and the final shader-free avatar pass can have different sizes.
fn clip(g: G, top: f64) -> V {
    let old = g.call("gpu_get_scissor", &[]);
    let target = g.call("surface_get_target", &[]);
    let (w, h) = if truthy(&g.call("surface_exists", &[target])) {
        (g.call("surface_get_width", &[target]), g.call("surface_get_height", &[target]))
    } else { (g.call("window_get_width", &[]), g.call("window_get_height", &[])) };
    let sx = w.as_f64().unwrap_or(640.0) / 640.0;
    let sy = h.as_f64().unwrap_or(360.0) / 360.0;
    let x = (LEFT * sx).floor().max(g.num(old, "x"));
    let y = (top * sy).floor().max(g.num(old, "y"));
    let right = ((RIGHT + 1.0) * sx).ceil().min(g.num(old, "x") + g.num(old, "w"));
    let bottom = ((BOTTOM + 1.0) * sy).ceil().min(g.num(old, "y") + g.num(old, "h"));
    g.call("gpu_set_scissor", &[real(x), real(y), real((right - x).max(0.0)), real((bottom - y).max(0.0))]);
    old
}

fn rgb(r: u32, g: u32, b: u32) -> f64 { (r | g << 8 | b << 16) as f64 }

fn upload(g: G, pixels: &[u8]) -> Option<V> {
    if pixels.len() != 32 * 32 * 4 { return None; }
    let buffer = g.call("buffer_create", &[real(pixels.len() as f64), real(0.0), real(1.0)]);
    let address = g.call("buffer_get_address", &[buffer]);
    if address.kind() != crate::gm::KIND_PTR || address.v == 0 { g.call("buffer_delete", &[buffer]); return None; }
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), address.v as *mut u8, pixels.len()); }
    let surface = g.call("surface_create", &[real(32.0), real(32.0)]);
    let sprite = if truthy(&g.call("surface_exists", &[surface])) {
        g.call("buffer_set_surface", &[buffer, surface, real(0.0)]);
        let sprite = g.call("sprite_create_from_surface", &[surface, real(0.0), real(0.0), real(32.0), real(32.0), boolean(false), boolean(false), real(0.0), real(0.0)]);
        g.call("surface_free", &[surface]);
        truthy(&g.call("sprite_exists", &[sprite])).then_some(sprite)
    } else { None };
    g.call("buffer_delete", &[buffer]);
    sprite
}

pub fn draw_avatars(g: G, caller: *mut c_void, other: *mut c_void) {
    let avatars = std::mem::take(&mut *AVATARS.lock().unwrap());
    if avatars.is_empty() { return; }
    let saved = ["draw_get_color", "draw_get_alpha", "shader_current", "gpu_get_texfilter"].map(|n| g.call(n, &[]));
    g.call("shader_reset", &[]);
    g.call("draw_set_alpha", &[real(1.0)]);
    g.call("gpu_set_texfilter", &[boolean(false)]);
    for avatar in avatars {
        let mut sprites = SPRITES.lock().unwrap();
        if !sprites.contains_key(&avatar.owner) {
            if let Some(sprite) = avatar.pixels.as_deref().and_then(|p| upload(g, p)) { sprites.insert(avatar.owner, sprite); }
        }
        let sprite = sprites.get(&avatar.owner).copied();
        drop(sprites);
        let scissor = clip(g, avatar.clip_top);
        g.call("draw_set_color", &[real(rgb(40, 62, 64))]);
        g.call("draw_rectangle", &[real(avatar.x), real(avatar.y), real(avatar.x + AVATAR_SIZE - 1.0), real(avatar.y + AVATAR_SIZE - 1.0), boolean(false)]);
        if let (Some(sprite), Some(draw)) = (sprite, g.rt.builtin("draw_sprite_ext")) {
            let mut args = [sprite, real(0.0), real(avatar.x), real(avatar.y), real(AVATAR_SIZE / 32.0), real(AVATAR_SIZE / 32.0), real(0.0), real(0xffffff as f64), real(1.0)];
            let mut result = V::undefined();
            unsafe { draw(&mut result, caller, other, 9, args.as_mut_ptr()); }
        }
        g.call("draw_set_color", &[real(avatar.accent)]);
        g.call("draw_rectangle", &[real(avatar.x - 1.0), real(avatar.y - 1.0), real(avatar.x + AVATAR_SIZE), real(avatar.y + AVATAR_SIZE), boolean(true)]);
        g.call("gpu_set_scissor", &[scissor]);
    }
    g.call("draw_set_color", &[saved[0]]); g.call("draw_set_alpha", &[saved[1]]);
    g.call("gpu_set_texfilter", &[saved[3]]);
    if crate::engine::asset_id(saved[2]) >= 0 { g.call("shader_set", &[saved[2]]); }
}
