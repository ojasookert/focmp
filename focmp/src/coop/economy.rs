//! Host-owned team rewards and serialized personal-shop purchases.
use crate::{
    engine::{G, V, boolean, is_struct, real, truthy},
    hooks::ScriptFn,
    net::{Msg, PeerId, Role},
};
use bitcode::{Decode, Encode};
use std::{
    ffi::c_void,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};
const FIELDS: &[&str] = &[
    "points",
    "points_base",
    "points_hits",
    "points_interest",
    "points_level",
    "points_multi",
    "points_overdmg",
    "kill_points",
    "kills",
    "kills_level",
    "total_enemies",
    "total_hp",
    "multiplier",
    "frames_combo_window",
    "damage",
    "damage_level",
    "hits_level",
    "hp_base_value",
];
#[derive(Clone, Debug, Encode, Decode)]
pub struct State {
    pub run: u64,
    pub values: Vec<f64>,
}
impl State {
    pub fn sane(&self) -> bool {
        self.run != 0
            && self.values.len() == FIELDS.len()
            && self.values.iter().all(|x| x.is_finite() && x.abs() < 1e12)
    }
}
fn run() -> u64 {
    super::rooms::RUN_SEED.load(Ordering::Relaxed)
}
fn client() -> bool {
    super::MY_ID.load(Ordering::Relaxed) != 0
}
fn points(g: G) -> Option<V> {
    let p = g.get(g.first("objInventory")?, "points");
    is_struct(&p).then_some(p)
}
pub fn capture(g: G) -> Option<State> {
    let p = points(g)?;
    let state = State {
        run: run(),
        values: FIELDS.iter().map(|f| g.num(p, f)).collect(),
    };
    state.sane().then_some(state)
}
pub fn apply(g: G, state: State) {
    if state.run != run() || !state.sane() {
        return;
    }
    if let Some(p) = points(g) {
        for (f, v) in FIELDS.iter().zip(state.values) {
            g.set(p, f, real(v));
        }
    }
}
static SLOTS: [AtomicUsize; 10] = [const { AtomicUsize::new(0) }; 10];
static EXECUTING: AtomicBool = AtomicBool::new(false);
static PERK_STEP: AtomicBool = AtomicBool::new(false);
static BLOCK_REROLL: AtomicBool = AtomicBool::new(false);
static CHARGED: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(0);
struct Pending {
    context: usize,
    other: usize,
    id: u64,
    method: V,
    args: V,
    cost: f64,
    room: String,
    level: u32,
    sub: u32,
    requested: bool,
    post: Option<(V, f64, V, f64)>,
}
static PENDING: LazyLock<Mutex<Option<Pending>>> = LazyLock::new(|| Mutex::new(None));
// One request at a time per player. Reliable transport and monotonically increasing
// IDs prevent repeated clicks/replayed packets from charging twice.
static LAST: LazyLock<Mutex<[u64; 3]>> = LazyLock::new(|| Mutex::new([0; 3]));
static RESERVED: LazyLock<Mutex<[Option<(u64, f64)>; 3]>> = LazyLock::new(|| Mutex::new([None; 3]));
fn charge(g: G, cost: f64) -> bool {
    let Some(p) = points(g) else { return false };
    let cash = g.num(p, "points");
    if !cost.is_finite() || cost < 0.0 || cash < cost {
        return false;
    }
    g.set(p, "points", real(cash - cost));
    true
}
fn queue(
    g: G,
    target: V,
    method: &str,
    n: i32,
    a: *mut *mut V,
    cost: f64,
    s: *mut c_void,
    o: *mut c_void,
) {
    if PENDING.lock().unwrap().is_some() || !cost.is_finite() || cost < 0.0 {
        return;
    }
    let m = g.get(target, method);
    if !truthy(&g.call("is_method", &[m])) {
        return;
    }
    let m = g.call("method", &[target, m]);
    let args = g.call("array_create", &[real(n as f64)]);
    for i in 0..n {
        g.call(
            "array_set",
            &[args, real(i as f64), unsafe { **a.add(i as usize) }],
        );
    }
    let roots = g.call("array_create", &[real(0.0)]);
    g.call("array_push", &[roots, m, args]);
    g.call("variable_global_set", &[g.s("focmp_purchase"), roots]);
    let (level, sub) = super::rooms::level_sub(g);
    *PENDING.lock().unwrap() = Some(Pending {
        context: s as usize,
        other: o as usize,
        id: SEQ.fetch_add(1, Ordering::Relaxed) + 1,
        method: m,
        args,
        cost,
        room: g.room_name(),
        level,
        sub,
        requested: false,
        post: None,
    });
}
unsafe extern "C-unwind" fn reward(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    unsafe { reward_impl(0, s, o, r, n, a) }
}
unsafe extern "C-unwind" fn add_points(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    unsafe { reward_impl(1, s, o, r, n, a) }
}
unsafe fn reward_impl(
    i: usize,
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    if run() != 0 && client() {
        // Points.add is an expression: lethal hits immediately read its
        // returned .value and .scale for the reward popup. Suppressing its
        // accounting must preserve that return contract (including explosions).
        let result = if i == 0 {
            if let Some(g) = G::new() {
                let mut value = g.global("focmp_no_reward");
                if !is_struct(&value) {
                    value = g.call("json_parse", &[g.s("{\"value\":0,\"scale\":1}")]);
                    g.call("variable_global_set", &[g.s("focmp_no_reward"), value]);
                }
                value
            } else { V::undefined() }
        } else { V::undefined() };
        unsafe { *r = result };
        return r;
    }
    let f: ScriptFn = unsafe { std::mem::transmute(SLOTS[i].load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}
unsafe extern "C-unwind" fn use_points(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    if run() != 0 && EXECUTING.load(Ordering::Relaxed) {
        CHARGED.store(true, Ordering::Relaxed);
        unsafe { *r = V::undefined() };
        return r;
    }
    if run() != 0 && client() && PERK_STEP.load(Ordering::Relaxed) && n > 0 {
        if let Some(g) = G::new() {
            if let Some(menu) = g.first("objSelectPerkGame") {
                let cost = unsafe { (**a).as_f64() }.unwrap_or(-1.0);
                queue(g, menu, "reroll_perks", 0, std::ptr::null_mut(), cost, s, o);
                BLOCK_REROLL.store(true, Ordering::Relaxed);
                unsafe { *r = V::undefined() };
                return r;
            }
        }
    }
    let f: ScriptFn = unsafe { std::mem::transmute(SLOTS[2].load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}
unsafe extern "C-unwind" fn spot_charge(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(SLOTS[3].load(Ordering::Relaxed)) };
    if run() == 0 || n < 1 {
        return unsafe { f(s, o, r, n, a) };
    };
    if EXECUTING.load(Ordering::Relaxed) {
        CHARGED.store(true, Ordering::Relaxed);
        unsafe { *r = boolean(true) };
        return r;
    }
    let Some(g) = G::new() else { return r };
    let cost = unsafe { (**a).as_f64() }.unwrap_or(-1.0);
    if client() {
        queue(
            g,
            V {
                v: s as u64,
                flags: 0,
                kind: crate::gm::KIND_OBJECT,
            },
            "activate",
            0,
            std::ptr::null_mut(),
            cost,
            s,
            o,
        );
        unsafe { *r = boolean(false) }
    } else {
        unsafe { *r = boolean(charge(g, cost)) }
    };
    r
}
macro_rules! purchases{($($i:literal=>$name:ident,$method:literal,$structure:literal),*)=>{$(
    unsafe extern "C-unwind" fn $name(s:*mut c_void,o:*mut c_void,r:*mut V,n:i32,a:*mut *mut V)->*mut V{
        let f:ScriptFn=unsafe{std::mem::transmute(SLOTS[$i].load(Ordering::Relaxed))};
        if run()==0||EXECUTING.load(Ordering::Relaxed){return unsafe{f(s,o,r,n,a)}};
        // Native reroll_shop(true) populates stock for free on entering the
        // laboratory. Only its paid/no-argument variant is a transaction.
        if $i==4 && n>0 && !a.is_null() && truthy(&unsafe{**a}) {return unsafe{f(s,o,r,n,a)}};
        let Some(g)=G::new()else{return r};
        let target=if $structure{V{v:s as u64,flags:0,kind:crate::gm::KIND_OBJECT}}else{unsafe{g.rt.builtin_var_of("id",s)}.unwrap_or_else(V::undefined)};
        let cost=if $structure{g.num(target,"cost")*g.num(target,"discount")}else{g.num(target,"reroll_cost")};
        if client(){queue(g,target,$method,n,a,cost,s,o);unsafe{*r=V::undefined()};return r;}
        // On the host the original native affordability check and debit are atomic.
        unsafe{f(s,o,r,n,a)}
    }
)*}}
purchases!(4=>lab_reroll,"reroll_shop",false,5=>shop_reroll,"reroll",false,6=>pack_buy,"buy",true);
unsafe extern "C-unwind" fn perk_reroll(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    if BLOCK_REROLL.load(Ordering::Relaxed) && !EXECUTING.load(Ordering::Relaxed) {
        unsafe { *r = V::undefined() };
        return r;
    }
    let f: ScriptFn = unsafe { std::mem::transmute(SLOTS[7].load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}
unsafe extern "C-unwind" fn perk_step(s: *mut c_void, o: *mut c_void) {
    let f: unsafe extern "C-unwind" fn(*mut c_void, *mut c_void) =
        unsafe { std::mem::transmute(SLOTS[8].load(Ordering::Relaxed)) };
    let Some(g) = G::new().filter(|_| run() != 0 && client()) else {
        unsafe { f(s, o) };
        return;
    };
    let Some(menu) = (unsafe { g.rt.builtin_var_of("id", s) }) else {
        unsafe { f(s, o) };
        return;
    };
    let Some(progress) = g.first("objGameProgress") else {
        unsafe { f(s, o) };
        return;
    };
    let cost = g.get(menu, "reroll_cost");
    let factor = g.get(progress, "perk_reroll_factor");
    PERK_STEP.store(true, Ordering::Relaxed);
    BLOCK_REROLL.store(false, Ordering::Relaxed);
    unsafe { f(s, o) };
    PERK_STEP.store(false, Ordering::Relaxed);
    if BLOCK_REROLL.swap(false, Ordering::Relaxed) {
        if let Some(p) = PENDING.lock().unwrap().as_mut() {
            p.post = Some((
                menu,
                g.num(menu, "reroll_cost"),
                progress,
                g.num(progress, "perk_reroll_factor"),
            ));
        }
        g.set(menu, "reroll_cost", cost);
        g.set(progress, "perk_reroll_factor", factor);
    }
}
unsafe extern "C-unwind" fn spot_activate(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(SLOTS[9].load(Ordering::Relaxed)) };
    if run() != 0 && client() && !EXECUTING.load(Ordering::Relaxed) {
        if let Some(g) = G::new() {
            let spot = V {
                v: s as u64,
                flags: 0,
                kind: crate::gm::KIND_OBJECT,
            };
            if matches!(g.num(spot, "type") as i32, 1 | 2) && truthy(&g.get(spot, "possible")) {
                // Wait before running native success/failure animations or granting
                // an item. The eventual approved action uses this exact same card.
                queue(
                    g,
                    spot,
                    "activate",
                    n,
                    a,
                    g.num(spot, "cost") * g.num(spot, "discount"),
                    s,
                    o,
                );
                unsafe { *r = boolean(false) };
                return r;
            }
        }
    }
    unsafe { f(s, o, r, n, a) }
}
pub fn peer_left(g: G, peer: PeerId) {
    if let Some((_, cost)) = RESERVED.lock().unwrap()[peer as usize].take() {
        if let Some(p) = points(g) {
            g.set(p, "points", real(g.num(p, "points") + cost));
        }
    }
}
pub fn install(g: G) {
    let hooks = [
        (
            "gml_Script_anon@7952@Points@gml_Object_objInventory_Other_14",
            reward as *const (),
        ),
        (
            "gml_Script_anon@6151@Points@gml_Object_objInventory_Other_14",
            add_points as *const (),
        ),
        (
            "gml_Script_anon@6043@Points@gml_Object_objInventory_Other_14",
            use_points as *const (),
        ),
        (
            "gml_Script_charge_points@anon@9652@ShopSpot@gml_Object_objLab_Other_10",
            spot_charge as *const (),
        ),
        (
            "gml_Script_anon@2684@gml_Object_objLab_Create_0",
            lab_reroll as *const (),
        ),
        (
            "gml_Script_anon@2870@gml_Object_objShop_Create_0",
            shop_reroll as *const (),
        ),
        (
            "gml_Script_anon@8262@CardPacks@gml_Object_objShop_Create_0",
            pack_buy as *const (),
        ),
        (
            "gml_Script_anon@1333@gml_Object_objSelectPerkGame_Other_4",
            perk_reroll as *const (),
        ),
        (
            "gml_Object_objSelectPerkGame_Step_0",
            perk_step as *const (),
        ),
        (
            "gml_Script_activate@anon@8290@ShopSpot@gml_Object_objLab_Other_10",
            spot_activate as *const (),
        ),
    ];
    for (i, (name, hook)) in hooks.into_iter().enumerate() {
        if SLOTS[i].load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(f) => SLOTS[i].store(f as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("economy: {e}")),
            }
        }
    }
}
pub fn reset(g: G) {
    *PENDING.lock().unwrap() = None;
    *LAST.lock().unwrap() = [0; 3];
    *RESERVED.lock().unwrap() = [None; 3];
    g.call(
        "variable_global_set",
        &[g.s("focmp_purchase"), V::undefined()],
    );
}
pub fn receive(g: G, c: &super::Coop, from: PeerId, msg: Msg) {
    let Some(net) = &c.net else { return };
    match msg {
        Msg::PurchaseRequest {
            run: r,
            id,
            cost,
            room,
            level,
            sub,
        } if r == run() && net.role == Role::Host => {
            let mut last = LAST.lock().unwrap();
            if id <= last[from as usize] {
                return;
            }
            last[from as usize] = id;
            let mut reserved = RESERVED.lock().unwrap();
            let ok = reserved[from as usize].is_none()
                && !c.travel.committed()
                && (room == g.room_name()
                    || (room == "Room_shop" && g.room_name().starts_with("Room_hub"))
                    || (room.starts_with("Room_hub") && g.room_name() == "Room_shop"))
                && (level, sub) == super::rooms::level_sub(g)
                && charge(g, cost);
            if ok {
                reserved[from as usize] = Some((id, cost));
            }
            net.broadcast(Msg::PurchaseResult {
                run: r,
                peer: from,
                id,
                ok,
            });
        }
        Msg::PurchaseResult {
            run: r,
            peer,
            id,
            ok,
        } if r == run() && from == 0 && peer == net.my_id => {
            let pending = {
                let mut p = PENDING.lock().unwrap();
                if p.as_ref().is_some_and(|p| p.id == id) {
                    p.take()
                } else {
                    None
                }
            };
            if let Some(p) = pending {
                let valid =
                    p.room == g.room_name() && (p.level, p.sub) == super::rooms::level_sub(g);
                CHARGED.store(false, Ordering::Relaxed);
                if ok && valid {
                    if let Some(wallet) = points(g) {
                        let cash = g.num(wallet, "points");
                        // Reserved funds count for native affordability checks; the
                        // host has already charged them, so use_points is suppressed.
                        g.set(wallet, "points", real(cash + p.cost));
                        EXECUTING.store(true, Ordering::Relaxed);
                        // Tween callbacks need a real calling context as well as
                        // a bound method; a null builtin caller crashes TGMX.
                        if let Some(call) = g.rt.builtin("method_call") {
                            let mut result = V::undefined();
                            let mut argv = [p.method, p.args];
                            unsafe {
                                call(
                                    &mut result,
                                    p.context as *mut c_void,
                                    p.other as *mut c_void,
                                    2,
                                    argv.as_mut_ptr(),
                                )
                            };
                        }
                        EXECUTING.store(false, Ordering::Relaxed);
                        if let Some((menu, cost, progress, factor)) = p.post {
                            g.set(menu, "reroll_cost", real(cost));
                            g.set(progress, "perk_reroll_factor", real(factor));
                            CHARGED.store(true, Ordering::Relaxed);
                        }
                        g.set(wallet, "points", real(cash));
                    }
                }
                net.broadcast(Msg::PurchaseDone {
                    run: r,
                    id,
                    applied: ok && valid && CHARGED.load(Ordering::Relaxed),
                });
                g.call(
                    "variable_global_set",
                    &[g.s("focmp_purchase"), V::undefined()],
                );
            }
        }
        Msg::PurchaseDone {
            run: r,
            id,
            applied,
        } if r == run() && net.role == Role::Host => {
            let mut reserved = RESERVED.lock().unwrap();
            if let Some((expected, cost)) = reserved[from as usize] {
                if id == expected {
                    reserved[from as usize] = None;
                    if !applied {
                        if let Some(p) = points(g) {
                            g.set(p, "points", real(g.num(p, "points") + cost));
                        }
                    }
                }
            }
        }
        _ => {}
    }
}
pub fn tick(g: G, c: &super::Coop) {
    if run() == 0 {
        return;
    }
    let Some(net) = &c.net else { return };
    let request = {
        let mut p = PENDING.lock().unwrap();
        p.as_mut().filter(|p| !p.requested).map(|p| {
            p.requested = true;
            Msg::PurchaseRequest {
                run: run(),
                id: p.id,
                cost: p.cost,
                room: p.room.clone(),
                level: p.level,
                sub: p.sub,
            }
        })
    };
    if let Some(msg) = request {
        net.broadcast(msg);
    }
    let _ = g;
}
pub fn describe(g: G) -> String {
    format!(
        "team cash {}, pending purchase {}, reservations {:?}",
        points(g).map(|p| g.num(p, "points")).unwrap_or(0.0),
        PENDING.lock().unwrap().as_ref().map(|p| p.id).unwrap_or(0),
        *RESERVED.lock().unwrap()
    )
}

pub fn busy(g:G)->bool{PENDING.lock().unwrap().is_some()||RESERVED.lock().unwrap().iter().any(Option::is_some)||g.first("objLab").is_some_and(|p|truthy(&g.get(p,"buying")))}

pub fn local_purchase_pending()->bool{PENDING.lock().unwrap().is_some()}
