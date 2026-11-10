//! Named cursors shared by card-selection menus, scoped to the current screen.
use super::{Coop, colors, rooms};
use crate::engine::{G, V, asset_id, boolean, real, truthy};
use crate::net::{Msg, PeerId};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
struct Cursor { menu: u8, x: f64, y: f64, visible: bool, seen: Instant }
#[derive(Default)]
pub struct Cursors { peers: [Option<Cursor>; 3], sent: Option<Instant>, menu: u8 }

static NATIVE_CURSOR: AtomicI32 = AtomicI32::new(-1);

/// The native menu arrow is drawn as s_cursor, independently of cursor_sprite
/// and the OS cursor. Suppress only that sprite while shared pointers replace it.
pub fn hide_native_sprite(sprite: i32) -> bool {
    sprite >= 0 && sprite == NATIVE_CURSOR.load(Ordering::Relaxed)
        && rooms::RUN_SEED.load(Ordering::Relaxed) != 0
        // Check the room at draw time too: it can change after the network tick.
        && G::new().is_some_and(|g| screen(g).0 != 0)
}

fn screen(g: G) -> (u8, Option<V>) {
    match g.room_name().as_str() {
        "Room_select_difficulty" => (1, g.first("objSelectDifficulty")),
        "Room_select_perk" => (2, g.first("objSelectPerk")),
        "Room_select_gun" => (3, g.first("objSelectGuns")),
        _ => (0, None),
    }
}
pub fn receive(c: &mut Coop, from: PeerId, run: u64, menu: u8, x: u16, y: u16, visible: bool) {
    if run == rooms::RUN_SEED.load(Ordering::Relaxed) && run != 0 && (from as usize) < c.cursors.peers.len() {
        c.cursors.peers[from as usize] = if (1..=3).contains(&menu) && visible {
            Some(Cursor { menu, x: x as f64, y: y as f64, visible, seen: Instant::now() })
        } else { None };
    }
}
pub fn tick(g: G, c: &mut Coop) {
    NATIVE_CURSOR.store(-1, Ordering::Relaxed);
    let Some(n) = &c.net else { return };
    let run = rooms::RUN_SEED.load(Ordering::Relaxed);
    if run == 0 || !n.connected() { return; }
    let (menu, inst) = screen(g);
    if menu == 0 {
        // The lobby and laboratory are personal menus. Retire the previous
        // shared cursor once, then neither sample nor transmit pointer motion.
        if c.cursors.menu != 0 {
            n.broadcast(Msg::MenuCursor { run, menu: 0, x: 0, y: 0, visible: false });
        }
        c.cursors = Cursors::default();
        return;
    }
    NATIVE_CURSOR.store(asset_id(g.asset("s_cursor")), Ordering::Relaxed);
    let mut x = g.call("device_mouse_x_to_gui", &[real(0.0)]).as_f64().unwrap_or(0.0);
    let mut y = g.call("device_mouse_y_to_gui", &[real(0.0)]).as_f64().unwrap_or(0.0);
    if let Some(inst) = inst {
        let inventory = g.get(inst, "oInventory");
        if g.exists(inventory) && g.get(inventory, "cursor_active").as_f64() == Some(0.0) {
            let rows = g.get(inst, "rows");
            let i = g.num(inst, "sel_i");
            let row = if i >= 0.0 && i < g.arr_len(rows) as f64 { g.arr_get(rows, i as usize) } else { g.get(inst, "button") };
            if crate::engine::is_struct(&row) {
                x = (g.num(row, "x0") + g.num(row, "x1")) / 2.0;
                y = (g.num(row, "y0") + g.num(row, "y1")) / 2.0;
            }
        }
    }
    let visible = menu != 0 && truthy(&g.call("window_has_focus", &[]));
    c.cursors.peers[n.my_id as usize] = Some(Cursor { menu, x, y, visible, seen: Instant::now() });
    if c.cursors.menu != menu || (menu != 0 && c.cursors.sent.is_none_or(|t| t.elapsed() >= Duration::from_millis(33))) {
        n.broadcast(Msg::MenuCursor { run, menu, x: x.clamp(0.0, 640.0) as u16, y: y.clamp(0.0, 360.0) as u16, visible });
        c.cursors.sent = Some(Instant::now());
        c.cursors.menu = menu;
    }
}
pub fn draw(g: G, c: &Coop) {
    let (menu, _) = screen(g);
    if menu == 0 { return; }
    let saved = [g.call("draw_get_color", &[]), g.call("draw_get_alpha", &[]), g.call("draw_get_font", &[]), g.call("draw_get_halign", &[]), g.call("draw_get_valign", &[])];
    g.call("draw_set_alpha", &[real(1.0)]);
    g.call("draw_set_font", &[g.global("Font_main_2x")]);
    g.call("draw_set_halign", &[real(0.0)]); g.call("draw_set_valign", &[real(1.0)]);
    for (id, name, _) in c.roster() {
        if let Some(p) = c.cursors.peers[id as usize].filter(|p| p.menu == menu && p.visible && p.seen.elapsed() < Duration::from_secs(2)) {
            g.call("draw_set_color", &[real(colors::value(c.colors[id as usize]))]);
            g.call("draw_triangle", &[real(p.x), real(p.y), real(p.x + 3.0), real(p.y + 12.0), real(p.x + 10.0), real(p.y + 8.0), boolean(false)]);
            let label = format!("P{} {}", id + 1, name.chars().filter(|ch| !ch.is_control()).take(12).collect::<String>());
            g.call("draw_text", &[real((p.x + 12.0).min(512.0)), real((p.y + 15.0).min(350.0)), g.s(&label)]);
        }
    }
    for (name, value) in ["draw_set_color", "draw_set_alpha", "draw_set_font", "draw_set_halign", "draw_set_valign"].into_iter().zip(saved) { g.call(name, &[value]); }
}
