//! Session-scoped card exchange. The host reserves one transfer until the
//! inventory owner confirms it; rejected/stale operations never consume cards.
use super::{COOP, Coop};
use crate::engine::{G, V, boolean, is_struct, key, real, truthy};
use crate::net::{MAX_PLAYERS, Msg, PeerId, Role};
use bitcode::{Decode, Encode};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const X: f64 = 432.0;
const Y: f64 = 166.0;
const STATS: [&str; 12] = [
    "bullit_fire_rate",
    "damage",
    "knockback",
    "spread",
    "recoil",
    "bullit_speed",
    "bullit_friction",
    "lifetime",
    "scale_x",
    "image_spd",
    "radius",
    "damage_type",
];

/// Only saved card data crosses the wire. Native save loading restores methods,
/// artwork and descriptions, including modified cards rather than their defaults.
#[derive(Clone, Debug, PartialEq, Encode, Decode)]
pub struct Card {
    pub name: String,
    pub parent: String,
    pub cat: u8,
    pub tier: f64,
    pub mods: [f64; 2],
    pub stats: [Option<f64>; 12],
}
impl Card {
    pub fn sane(&self) -> bool {
        matches!(self.cat, 1 | 2 | 3 | 8)
            && [&self.name, &self.parent].iter().all(|s| {
                !s.is_empty()
                    && s.len() <= 96
                    && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            })
            && self.tier.is_finite()
            && (0.0..=10.0).contains(&self.tier)
            && self
                .mods
                .iter()
                .all(|v| v.is_finite() && (0.0..256.0).contains(v) && v.fract() == 0.0)
            && self
                .stats
                .iter()
                .flatten()
                .all(|v| v.is_finite() && v.abs() < 1e8)
    }
    fn capture(g: G, v: V) -> Option<Self> {
        if !is_struct(&v) {
            return None;
        }
        let mods = g.get(v, "mods");
        if !array(g, mods) || g.arr_len(mods) != 2 {
            return None;
        }
        let c = Self {
            name: g.str_of(&g.get(v, "name")),
            parent: g.str_of(&g.get(v, "parent")),
            cat: g.num(v, "cat") as u8,
            tier: g.num(v, "tier"),
            mods: [g.arr_get(mods, 0).as_f64()?, g.arr_get(mods, 1).as_f64()?],
            stats: STATS.map(|s| g.get(v, s).as_f64()),
        };
        c.sane().then_some(c)
    }
    fn restore(&self, g: G) -> Option<V> {
        if !self.sane() {
            return None;
        }
        let cards = g.first("objCards")?;
        let catalog = g.get(cards, "B");
        let group = match self.cat {
            1 => "bullits",
            2 => "booms",
            3 => "melees",
            8 => "flexmod",
            _ => return None,
        };
        let group = g.get(catalog, group);
        if !is_struct(&group) {
            return None;
        }
        let prototype = g.get(group, &self.parent);
        if !is_struct(&prototype) || g.num(prototype, "cat") != self.cat as f64 {
            return None;
        }
        let save = g.first("objSaveGame")?;
        let data = g.call("json_parse", &[g.s("{}")]);
        g.set(save, "focmp_trade_load", data);
        g.set(data, "name", g.s(&self.name));
        // A god-tier catalog variant can carry its own costs and callbacks.
        // Use it when available, then restore the saved parent identity below.
        let variant = g.get(group, &self.name);
        let source = if is_struct(&variant) && g.num(variant, "cat") == self.cat as f64 {
            &self.name
        } else {
            &self.parent
        };
        g.set(data, "parent", g.s(source));
        g.set(data, "tier", real(self.tier));
        g.set(data, "cat", real(self.cat as f64));
        g.set(data, "mods", list(g, &self.mods.map(real)));
        for (name, v) in STATS.iter().zip(self.stats) {
            if let Some(v) = v {
                g.set(data, name, real(v));
            }
        }
        let result = method(g, save, "load_ammo_card", &[data, cards]);
        // Keep the returned native object alive until inventory/visuals own it.
        g.set(save, "focmp_trade_load", result);
        if !is_struct(&result) {
            return None;
        }
        g.set(result, "name", g.s(&self.name));
        g.set(result, "parent", g.s(&self.parent));
        for (name, v) in STATS.iter().zip(self.stats) {
            if let Some(v) = v {
                g.set(result, name, real(v));
            }
        }
        method(g, cards, "create_card_string", &[result]);
        (Self::capture(g, result).as_ref() == Some(self)).then_some(result)
    }
}
#[derive(Clone, Debug, Encode, Decode)]
pub enum Packet {
    Request {
        id: u64,
        revision: u64,
        card: Option<Card>,
    },
    Grant {
        peer: PeerId,
        id: u64,
        ok: bool,
        card: Option<Card>,
    },
    Done {
        id: u64,
        applied: bool,
    },
    State {
        revision: u64,
        card: Option<Card>,
        actor: PeerId,
        taking: bool,
        x: f64,
        y: f64,
    },
}
impl Packet {
    pub fn valid_from(&self, from: PeerId) -> bool {
        match self {
            Self::Request { id, card, .. } => *id != 0 && card.as_ref().is_none_or(Card::sane),
            Self::Grant { peer, id, card, .. } => {
                from == 0
                    && (*peer as usize) < MAX_PLAYERS
                    && *id != 0
                    && card.as_ref().is_none_or(Card::sane)
            }
            Self::Done { id, .. } => *id != 0,
            Self::State {
                card, actor, x, y, ..
            } => {
                from == 0
                    && (*actor as usize) < MAX_PLAYERS
                    && card.as_ref().is_none_or(Card::sane)
                    && [x, y].iter().all(|v| v.is_finite() && v.abs() < 1e7)
            }
        }
    }
}
struct Pending {
    id: u64,
    card: Option<Card>,
    native: Option<u64>,
    applied: Option<bool>,
}
struct Transfer {
    peer: PeerId,
    id: u64,
    card: Card,
    taking: bool,
    x: f64,
    y: f64,
}
struct Animation {
    card: Card,
    taking: bool,
    x: f64,
    y: f64,
    start: Instant,
}
#[derive(Default)]
pub struct Pedestal {
    revision: u64,
    card: Option<Card>,
    next: u64,
    pending: Option<Pending>,
    active: Option<Transfer>,
    last: [u64; MAX_PLAYERS],
    replies: [Option<Packet>; MAX_PLAYERS],
    table: Option<V>,
    sprite: Option<V>,
    shadow: Option<V>,
    light: Option<V>,
    particles: Option<(V, V)>,
    visual_card: Option<Card>,
    animation: Option<Animation>,
    clock: Option<Instant>,
}
fn array(g: G, v: V) -> bool {
    truthy(&g.call("is_array", &[v]))
}
fn list(g: G, vs: &[V]) -> V {
    let a = g.call("array_create", &[real(vs.len() as f64)]);
    for (i, v) in vs.iter().enumerate() {
        g.call("array_set", &[a, real(i as f64), *v]);
    }
    a
}
fn method(g: G, target: V, name: &str, args: &[V]) -> V {
    let m = g.get(target, name);
    if !truthy(&g.call("is_method", &[m])) {
        return V::undefined();
    }
    let bound = g.call("method", &[target, m]);
    let mut out = V::undefined();
    let context = CONTEXT.load(Ordering::Relaxed) as *mut c_void;
    if context.is_null() {
        return out;
    }
    let mut argv = [bound, list(g, args)];
    if let Some(call) = g.rt.builtin("method_call") {
        unsafe {
            call(&mut out, context, context, 2, argv.as_mut_ptr());
        }
    }
    out
}
fn sound(g: G, name: &str) {
    g.call(
        "audio_play_sound",
        &[g.asset(name), real(0.0), boolean(false)],
    );
}
fn opened(g: G, i: V) -> bool {
    truthy(&g.get(i, "fullscreen"))
}
fn inventory(g: G) -> Option<(V, V)> {
    let i = g.first("objInventory")?;
    let inv = g.get(i, "inventory");
    is_struct(&inv).then_some((i, inv))
}
fn space(g: G, i: V) -> bool {
    truthy(&method(g, i, "space_in_inventory", &[]))
}
fn flush(g: G, i: V) {
    if opened(g, i) {
        method(g, i, "set_cards_on_close", &[]);
    }
}
fn refresh(g: G, i: V) {
    if opened(g, i) {
        method(g, i, "set_cards_on_open", &[]);
    }
}
fn save(g: G) {
    if let Some(s) = g.first("objSaveGame") {
        method(g, s, "save", &[]);
    }
}
fn local_position(g: G) -> Option<(f64, f64)> {
    if g.room_name() != "Room_hub_1" || super::pause::open(g) {
        return None;
    }
    let p = g.first("objPlayer")?;
    if truthy(&g.get(p, "dead")) {
        return None;
    }
    let (x, y) = (g.num(p, "x"), g.num(p, "y"));
    near(x, y).then_some((x, y))
}
// The table is 50 px tall. Stand just below its collision box.
fn near(x: f64, y: f64) -> bool {
    (x - X).abs() <= 48.0 && (y - Y).abs() <= 58.0
}
fn position(g: G, c: &Coop, peer: PeerId) -> Option<(f64, f64)> {
    if peer == 0 {
        return local_position(g);
    }
    let r = c.remotes.get(&peer)?;
    let s = r.state.as_ref()?;
    (r.room == "Room_hub_1"
        && !s.dead
        && !r.busy
        && r.seen.elapsed().as_secs() < 2
        && near(s.x as f64, s.y as f64))
    .then_some((s.x as f64, s.y as f64))
}
// Native Select (E / gamepad confirm) and mouse pickup both put a card
// into drag_card. Hovering only sets `selected` and must not enable a trade.
fn selected(g: G, i: V) -> Option<V> {
    if !opened(g, i) || !truthy(&g.get(i, "dragging")) {
        return None;
    }
    let card = g.get(i, "drag_card");
    Card::capture(g, card).map(|_| card)
}
fn find_card(g: G, inv: V, native: u64) -> Option<(usize, usize, V)> {
    let a = g.get(inv, "cards");
    if array(g, a) {
        for j in 0..g.arr_len(a) {
            let v = g.arr_get(a, j);
            if is_struct(&v) && key(&v) == native {
                return Some((0, j, v));
            }
        }
    }
    let guns = g.get(inv, "guns");
    if array(g, guns) {
        for r in 0..g.arr_len(guns).min(3) {
            let gun = g.arr_get(guns, r);
            if !is_struct(&gun) {
                continue;
            }
            let a = g.get(gun, "cards_inventory");
            if !array(g, a) {
                continue;
            }
            for j in 0..g.arr_len(a) {
                let v = g.arr_get(a, j);
                if is_struct(&v) && key(&v) == native {
                    return Some((r + 1, j, v));
                }
            }
        }
    }
    None
}

/// Inventory mutation happens once, after host grant. A failed final capacity
/// check returns a negative acknowledgment, leaving the original pedestal intact.
fn apply(g: G, p: &Pending, take: Option<&Card>) -> bool {
    let Some((i, inv)) = inventory(g) else {
        return false;
    };
    flush(g, i);
    let ok = if let Some(card) = &p.card {
        if let Some((row, col, v)) = p.native.and_then(|n| find_card(g, inv, n)) {
            if Card::capture(g, v).as_ref() == Some(card) {
                method(
                    g,
                    i,
                    "destroy_card_from_pos",
                    &[list(g, &[real(row as f64), real(col as f64)])],
                );
                p.native.is_some_and(|n| find_card(g, inv, n).is_none())
            } else {
                false
            }
        } else {
            false
        }
    } else if space(g, i) {
        if let Some(v) = take.and_then(|c| c.restore(g)) {
            method(g, inv, "add_card", &[v]);
            find_card(g, inv, key(&v)).is_some()
        } else {
            false
        }
    } else {
        false
    };
    refresh(g, i);
    if ok {
        if p.card.is_some() {
            if let Some(player) = g.first("objPlayer") {
                method(g, player, "select_gun", &[g.get(player, "gun_i")]);
            }
        }
        save(g);
    }
    g.set(i, "focmp_trade_pending", V::undefined());
    ok
}
fn publish(g: G, c: &mut Coop, t: Transfer, applied: bool) {
    if !applied {
        return;
    }
    let card = if t.taking { None } else { Some(t.card.clone()) };
    let revision = c.pedestal.revision + 1;
    crate::log(&format!(
        "pedestal: peer {} {} card {} (revision {revision})",
        t.peer,
        if t.taking { "took" } else { "placed" },
        t.card.name
    ));
    let packet = Packet::State {
        revision,
        card,
        actor: t.peer,
        taking: t.taking,
        x: t.x,
        y: t.y,
    };
    if let Some(n) = &c.net {
        n.broadcast(Msg::Pedestal(packet.clone()));
    }
    receive(g, c, 0, packet);
}
pub fn peer_left(g: G, c: &mut Coop, peer: PeerId) {
    if c.pedestal.active.as_ref().is_some_and(|t| t.peer == peer) {
        // Only acknowledged offers enter the shared pedestal. A reserved take
        // may already have reached the departing player's save, so retire it.
        let t = c.pedestal.active.take().unwrap();
        if t.taking {
            publish(g, c, t, true);
        }
    }
}
pub fn receive(g: G, c: &mut Coop, from: PeerId, packet: Packet) {
    let Some(n) = &c.net else {
        return;
    };
    let role = n.role;
    let me = n.my_id;
    match packet {
        Packet::Request { id, revision, card } if role == Role::Host => {
            if id <= c.pedestal.last[from as usize] {
                if id == c.pedestal.last[from as usize] {
                    if let Some(p) = &c.pedestal.replies[from as usize] {
                        n.broadcast(Msg::Pedestal(p.clone()));
                    }
                }
                return;
            }
            let pos = position(g, c, from);
            let ok = c.pedestal.active.is_none()
                && revision == c.pedestal.revision
                && pos.is_some()
                && card.as_ref().is_none_or(|v| v.restore(g).is_some())
                && card.is_some() != c.pedestal.card.is_some();
            let take = if ok && card.is_none() {
                c.pedestal.card.clone()
            } else {
                None
            };
            if ok {
                let (x, y) = pos.unwrap();
                c.pedestal.active = Some(Transfer {
                    peer: from,
                    id,
                    card: card.clone().or(take.clone()).unwrap(),
                    taking: card.is_none(),
                    x,
                    y,
                });
            }
            c.pedestal.last[from as usize] = id;
            let reply = Packet::Grant {
                peer: from,
                id,
                ok,
                card: take,
            };
            c.pedestal.replies[from as usize] = Some(reply.clone());
            if from == me {
                receive(g, c, 0, reply);
            } else {
                c.net.as_ref().unwrap().broadcast(Msg::Pedestal(reply));
            }
        }
        Packet::Grant { peer, id, ok, card } if from == 0 && peer == me => {
            let Some(mut p) = c.pedestal.pending.take() else {
                return;
            };
            if p.id != id {
                c.pedestal.pending = Some(p);
                return;
            }
            let applied = *p.applied.get_or_insert_with(|| {
                ok && apply(
                    g,
                    &Pending {
                        id: p.id,
                        card: p.card.clone(),
                        native: p.native,
                        applied: None,
                    },
                    card.as_ref(),
                )
            });
            if !ok || !applied {
                sound(g, "sound_card_error");
            }
            // Keep the receipt until the next request; duplicates only resend ACK.
            c.pedestal.pending = Some(p);
            if ok {
                let done = Packet::Done { id, applied };
                if role == Role::Host {
                    receive(g, c, me, done);
                } else {
                    c.net.as_ref().unwrap().broadcast(Msg::Pedestal(done));
                }
            }
        }
        Packet::Done { id, applied } if role == Role::Host => {
            if c.pedestal
                .active
                .as_ref()
                .is_some_and(|t| t.peer == from && t.id == id)
            {
                let t = c.pedestal.active.take().unwrap();
                publish(g, c, t, applied);
            }
        }
        Packet::State {
            revision,
            card,
            actor: _,
            taking,
            x,
            y,
        } if from == 0 => {
            if revision <= c.pedestal.revision {
                return;
            }
            let moving = if taking {
                c.pedestal.card.clone()
            } else {
                card.clone()
            };
            c.pedestal.revision = revision;
            c.pedestal.card = card;
            if g.room_name() == "Room_hub_1" {
                c.pedestal.animation = moving.map(|card| Animation {
                    card,
                    taking,
                    x,
                    y,
                    start: Instant::now(),
                });
                sound(
                    g,
                    if taking {
                        "sound_card_snap_init"
                    } else {
                        "sound_card_snap"
                    },
                );
            }
        }
        _ => {}
    }
}
fn waiting(p: &Pedestal) -> bool {
    p.pending.as_ref().is_some_and(|p| p.applied.is_none())
}
fn request(g: G, c: &mut Coop) {
    if local_position(g).is_none() || waiting(&c.pedestal) {
        return;
    }
    let Some((i, _)) = inventory(g) else {
        return;
    };
    let selected = if c.pedestal.card.is_some() {
        if opened(g, i) {
            return;
        }
        if !space(g, i) {
            sound(g, "sound_card_error");
            return;
        }
        None
    } else {
        let Some(v) = selected(g, i) else {
            return;
        };
        Some(v)
    };
    let card = selected.and_then(|v| Card::capture(g, v));
    // Root the offered object while the network response is pending.
    g.set(i, "focmp_trade_pending", selected.unwrap_or(V::undefined()));
    c.pedestal.next += 1;
    let id = c.pedestal.next;
    c.pedestal.pending = Some(Pending {
        id,
        card: card.clone(),
        native: selected.map(|v| key(&v)),
        applied: None,
    });
    let packet = Packet::Request {
        id,
        revision: c.pedestal.revision,
        card,
    };
    let n = c.net.as_ref().unwrap();
    if n.role == Role::Host {
        receive(g, c, 0, packet);
    } else {
        n.broadcast(Msg::Pedestal(packet));
    }
}

static CONTEXT: AtomicUsize = AtomicUsize::new(0);
static STEP: AtomicUsize = AtomicUsize::new(0);
static INVENTORY_DRAW: AtomicUsize = AtomicUsize::new(0);
static SCRIBBLE: AtomicUsize = AtomicUsize::new(0);
thread_local! {static INVENTORY_HINT: std::cell::Cell<bool> = const {std::cell::Cell::new(false)};}
static INPUT: AtomicUsize = AtomicUsize::new(0);
type Event = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);
pub fn install(g: G) {
    for (name, fun, slot) in [
        ("gml_Object_objController_Step_0", step as *const (), &STEP),
        (
            "gml_Object_objInventory_Draw_64",
            inventory_draw as *const (),
            &INVENTORY_DRAW,
        ),
        ("gml_Script_scribble", scribble as *const (), &SCRIBBLE),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, fun) {
                Ok(f) => slot.store(f as usize, Ordering::Relaxed),
                Err(e) => crate::log(&format!("pedestal: {e}")),
            }
        }
    }
    if let Some((_, a)) =
        g.rt.scripts
            .iter()
            .find(|(n, _)| *n == "gml_Script_InputPressed")
    {
        INPUT.store(*a, Ordering::Relaxed);
    }
}
unsafe extern "C-unwind" fn step(s: *mut c_void, o: *mut c_void) {
    CONTEXT.store(s as usize, Ordering::Relaxed);
    let f: Event = unsafe { std::mem::transmute(STEP.load(Ordering::Relaxed)) };
    unsafe {
        f(s, o);
    }
    let Some(g) = G::new() else {
        return;
    };
    let Ok(mut c) = COOP.try_lock() else {
        return;
    };
    if c.net.is_none() || local_position(g).is_none() {
        return;
    }
    if inventory(g).is_some_and(|(i, _)| opened(g, i)) {
        if super::controls::place_pressed(g) {
            request(g, &mut c);
        }
        return;
    }
    let a = INPUT.load(Ordering::Relaxed);
    if a == 0 {
        return;
    }
    let input: crate::hooks::ScriptFn = unsafe { std::mem::transmute(a) };
    let mut v = real(8.0);
    let mut args = [&mut v as *mut V];
    let mut ret = V::undefined();
    unsafe {
        input(s, o, &mut ret, 1, args.as_mut_ptr());
    }
    if truthy(&ret) {
        request(g, &mut c);
    }
}
// Extend the native controls line so its alignment, font, colour, scaling and
// controller hint layout remain owned by the game.
unsafe extern "C-unwind" fn inventory_draw(s: *mut c_void, o: *mut c_void) {
    let show = G::new().is_some_and(|g| {
        COOP.try_lock().ok().is_some_and(|c| {
            c.net.is_some()
                && c.pedestal.card.is_none()
                && !waiting(&c.pedestal)
                && local_position(g).is_some()
                && inventory(g).is_some_and(|(i, _)| selected(g, i).is_some())
        })
    });
    struct Scope(bool);
    impl Drop for Scope {
        fn drop(&mut self) {
            INVENTORY_HINT.with(|v| v.set(self.0));
        }
    }
    let _scope = Scope(INVENTORY_HINT.with(|v| v.replace(show)));
    let f: Event = unsafe { std::mem::transmute(INVENTORY_DRAW.load(Ordering::Relaxed)) };
    unsafe {
        f(s, o);
    }
}
unsafe extern "C-unwind" fn scribble(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    let f: crate::hooks::ScriptFn =
        unsafe { std::mem::transmute(SCRIBBLE.load(Ordering::Relaxed)) };
    if INVENTORY_HINT.with(|v| v.get()) && n > 0 && !a.is_null() {
        if let (Some(g), Some(text)) = (G::new(), unsafe { (**a).as_str() }) {
            if text.contains("CONTROLS:") {
                let binding = unsafe { super::controls::place_hint(g, s, o) };
                let mut text = g.s(&format!("{text}Place: {binding}  "));
                let mut args = unsafe { std::slice::from_raw_parts(a, n as usize) }.to_vec();
                args[0] = &mut text;
                return unsafe { f(s, o, r, n, args.as_mut_ptr()) };
            }
        }
    }
    unsafe { f(s, o, r, n, a) }
}
unsafe fn draw_prompt(g: G, c: &Coop, s: *mut c_void, o: *mut c_void) {
    if c.net.is_none() || local_position(g).is_none() {
        return;
    }
    let Some((i, _)) = inventory(g) else {
        return;
    };
    let inventory_hint = super::controls::hint(g, 6);
    let interact_hint = super::controls::hint(g, 8);
    let label = if waiting(&c.pedestal) {
        "Trading..."
    } else if c.pedestal.card.is_some() {
        if !space(g, i) {
            "Inventory full"
        } else if opened(g, i) {
            return;
        } else {
            &format!("{interact_hint} Take card")
        }
    } else if opened(g, i) {
        return;
    } else {
        &format!("{inventory_hint} Choose a card")
    };
    let saved = [
        g.call("draw_get_color", &[]),
        g.call("draw_get_alpha", &[]),
        g.call("draw_get_font", &[]),
        g.call("draw_get_halign", &[]),
        g.call("draw_get_valign", &[]),
    ];
    g.call("draw_set_font", &[g.global("Font_main_2x")]);
    g.call("draw_set_halign", &[real(1.0)]);
    g.call("draw_set_valign", &[real(1.0)]);
    g.call("draw_set_alpha", &[real(1.0)]);
    let y = if opened(g, i) { 324.0 } else { 343.0 };
    g.call("draw_set_color", &[real(0.0)]);
    unsafe { super::controls::draw_text(g, s, o, 321.0, y + 1.0, label); }
    g.call("draw_set_color", &[real(16777215.0)]);
    unsafe { super::controls::draw_text(g, s, o, 320.0, y, label); }
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
}

impl Pedestal {
    fn clear_visuals(&mut self, g: G) {
        if let Some(v) = self.light.take() {
            if self.table.is_some_and(|t| g.exists(t)) {
                g.set(v, "visible", boolean(false));
                g.set(v, "__destroyed", boolean(true));
            }
        }
        for v in [&mut self.sprite, &mut self.shadow, &mut self.table] {
            if let Some(v) = v.take() {
                if g.exists(v) {
                    g.call("instance_destroy", &[v]);
                }
            }
        }
        if let Some((system, kind)) = self.particles.take() {
            if truthy(&g.call("part_system_exists", &[system])) {
                g.call("part_system_destroy", &[system]);
            }
            if truthy(&g.call("part_type_exists", &[kind])) {
                g.call("part_type_destroy", &[kind]);
            }
        }
        self.visual_card = None;
    }
    pub fn reset(&mut self, g: G) {
        self.clear_visuals(g);
        *self = Self::default();
    }
}
fn create(g: G, obj: &str, depth: f64) -> V {
    g.call(
        "instance_create_depth",
        &[real(X), real(Y), real(depth), g.asset(obj)],
    )
}
fn set_sprite(g: G, v: V, sprite: V, frame: f64, scale: f64) {
    g.set(v, "sprite_index", sprite);
    g.set(v, "image_index", real(frame));
    g.set(v, "image_speed", real(0.0));
    g.set(v, "image_xscale", real(scale));
    g.set(v, "image_yscale", real(scale));
}
pub fn tick(g: G, c: &mut Coop) {
    let p = &mut c.pedestal;
    if c.net.is_none()
        || super::rooms::RUN_SEED.load(Ordering::Relaxed) == 0
        || g.room_name() != "Room_hub_1"
    {
        p.clear_visuals(g);
        p.animation = None;
        return;
    }
    if p.table.is_none_or(|v| !g.exists(v)) {
        p.clear_visuals(g);
        let table = create(g, "objTableMachine", 400.0);
        if !g.exists(table) {
            return;
        }
        set_sprite(g, table, g.asset("s_table_machine"), 3.0, 1.0);
        p.table = Some(table);
        p.clock = Some(Instant::now());
    }
    if p.animation
        .as_ref()
        .is_some_and(|a| a.start.elapsed().as_secs_f64() >= 0.3)
    {
        p.animation = None;
    }
    let display = p
        .animation
        .as_ref()
        .map(|a| a.card.clone())
        .or(p.card.clone());
    if display != p.visual_card {
        for v in [&mut p.sprite, &mut p.shadow] {
            if let Some(v) = v.take() {
                if g.exists(v) {
                    g.call("instance_destroy", &[v]);
                }
            }
        }
        p.visual_card = None;
        if let Some(card) = display.as_ref() {
            if let Some(v) = card.restore(g) {
                let sprite = create(g, "objPlayerDummy", 390.0);
                let shadow = create(g, "objPlayerDummy", 392.0);
                set_sprite(g, sprite, g.get(v, "card_sprite"), g.num(v, "card_i"), 0.5);
                set_sprite(g, shadow, g.asset("s_card_shadow"), 0.0, 0.5);
                g.set(shadow, "image_alpha", real(0.45));
                p.sprite = Some(sprite);
                p.shadow = Some(shadow);
                p.visual_card = display;
            }
        }
    }
    let occupied = p.card.is_some() && p.animation.is_none();
    let phase = p.clock.unwrap().elapsed().as_secs_f64() * std::f64::consts::TAU / 1.1;
    let bob = phase.sin() * 2.0;
    let (mut x, mut y) = (X, 156.0 + bob);
    if let Some(a) = &p.animation {
        let t = (a.start.elapsed().as_secs_f64() / 0.3).clamp(0.0, 1.0);
        let u = if a.taking { 1.0 - t } else { t };
        x = a.x + (X - a.x) * u;
        y = a.y + (156.0 - a.y) * u - 20.0 * (std::f64::consts::PI * t).sin();
    }
    if let Some(v) = p.sprite {
        g.set(v, "x", real(x - 10.5));
        g.set(v, "y", real(y - 16.0));
    }
    if let Some(v) = p.shadow {
        g.set(v, "x", real(x - 9.5));
        g.set(v, "y", real(y - 10.0));
    }
    if p.light.is_none() && occupied {
        // Native BulbLight contains renderer state. Clone the hub's bulb, reset
        // its surface cache, root it on the table and bind its renderer method.
        if let (Some(button), Some(controller)) =
            (g.first("objIngameButton"), g.first("objLightController"))
        {
            let template = g.get(button, "light");
            if is_struct(&template) {
                let light = g.call("variable_clone", &[template]);
                g.set(p.table.unwrap(), "focmp_pedestal_light", light);
                g.set(light, "__destroyed", boolean(false));
                g.set(light, "surf", real(-4.0));
                g.set(light, "__oldSprite", real(-4.0));
                g.set(light, "sprite", g.asset("s_light_round_tiny"));
                g.set(light, "x", real(X));
                g.set(light, "y", real(158.0));
                g.set(light, "xscale", real(0.6));
                g.set(light, "yscale", real(0.6));
                method(g, light, "AddToRenderer", &[g.get(controller, "renderer")]);
                p.light = Some(light);
            }
        }
    }
    if let Some(light) = p.light {
        g.set(light, "visible", boolean(occupied));
        g.set(light, "alpha", real(0.8 + 0.2 * phase.sin()));
    }
    if occupied && p.particles.is_none() {
        let system = g.call("part_system_create", &[]);
        let kind = g.call("part_type_create", &[]);
        g.call("part_system_depth", &[system, real(395.0)]);
        g.call("part_type_shape", &[kind, real(0.0)]);
        g.call(
            "part_type_size",
            &[kind, real(1.0), real(1.0), real(0.0), real(0.0)],
        );
        g.call(
            "part_type_direction",
            &[kind, real(0.0), real(360.0), real(0.0), real(0.0)],
        );
        g.call(
            "part_type_speed",
            &[kind, real(0.45), real(0.45), real(0.0), real(0.0)],
        );
        g.call("part_type_life", &[kind, real(55.0), real(55.0)]);
        g.call("part_type_alpha3", &[kind, real(0.9), real(0.6), real(0.0)]);
        g.call(
            "part_type_colour2",
            &[kind, real(16777215.0), real(13434111.0)],
        );
        let emitter = g.call("part_emitter_create", &[system]);
        g.call(
            "part_emitter_region",
            &[
                system,
                emitter,
                real(X),
                real(X),
                real(158.0),
                real(158.0),
                real(0.0),
                real(0.0),
            ],
        );
        g.call("part_emitter_stream", &[system, emitter, kind, real(-2.0)]);
        p.particles = Some((system, kind));
    } else if !occupied {
        if let Some((system, kind)) = p.particles.take() {
            g.call("part_system_destroy", &[system]);
            g.call("part_type_destroy", &[kind]);
        }
    }
}
/// World feedback; the TAB action is appended to the native controls line.
pub unsafe fn draw_world_prompt(g: G, c: &Coop, s: *mut c_void, o: *mut c_void) {
    unsafe { draw_prompt(g, c, s, o); }
}

pub fn console(g: G, c: &mut Coop, a: &[&str]) -> String {
    if a == ["interact"] && c.net.is_some() {
        request(g, c);
    } else if !a.is_empty() {
        return "usage: coop pedestal [interact]".into();
    }
    let p = &c.pedestal;
    format!(
        "revision={} card={} reserved={} pending={} applied={:?} near={} inventory_open={} space={} selected={}",
        p.revision,
        p.card.as_ref().map_or("empty", |c| c.name.as_str()),
        p.active.is_some(),
        waiting(p),
        p.pending.as_ref().and_then(|p| p.applied),
        local_position(g).is_some(),
        inventory(g).is_some_and(|(i, _)| opened(g, i)),
        inventory(g).is_some_and(|(i, _)| space(g, i)),
        inventory(g)
            .and_then(|(i, _)| selected(g, i))
            .and_then(|v| Card::capture(g, v))
            .map_or("none".into(), |c| c.name)
    )
}
