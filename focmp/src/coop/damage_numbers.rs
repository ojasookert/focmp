//! Relay native enemy and player damage popups without repeating damage callbacks.
use crate::{engine::{G, V, real, is_struct}, hooks::ScriptFn};
use bitcode::{Decode, Encode};
use std::{ffi::c_void, sync::{Mutex, atomic::{AtomicUsize, Ordering}}};

static CREATE: AtomicUsize = AtomicUsize::new(0);
static DRAW: AtomicUsize = AtomicUsize::new(0);
static PLAYER_DEPTH: AtomicUsize = AtomicUsize::new(0);
pub static PLAYER_OUTGOING: Mutex<Vec<Number>> = Mutex::new(Vec::new());
static DAMAGE_DEPTH: AtomicUsize = AtomicUsize::new(0);
pub static OUTGOING: Mutex<Vec<Number>> = Mutex::new(Vec::new());
static PENDING: Mutex<Vec<Number>> = Mutex::new(Vec::new());

#[derive(Clone, Debug, Encode, Decode)]
pub struct Number { room: String, level: u32, sub: u32, x: f64, y: f64, amount: f64, vx: f64, vy: f64 }
impl Number {
    pub fn sane(&self) -> bool {
        self.room.starts_with("Room_") && self.room.len() < 128
            && [self.x,self.y,self.amount,self.vx,self.vy].iter().all(|v| v.is_finite() && v.abs() < 1.0e7)
    }
}
pub struct PlayerDamage;
pub fn player_damage()->PlayerDamage{PLAYER_DEPTH.fetch_add(1,Ordering::Relaxed);PlayerDamage}
impl Drop for PlayerDamage{fn drop(&mut self){PLAYER_DEPTH.fetch_sub(1,Ordering::Relaxed);}}
pub struct HostDamage;
pub fn host_damage() -> HostDamage { DAMAGE_DEPTH.fetch_add(1, Ordering::Relaxed); HostDamage }
impl Drop for HostDamage { fn drop(&mut self) { DAMAGE_DEPTH.fetch_sub(1, Ordering::Relaxed); } }
pub fn reset() { PLAYER_OUTGOING.lock().unwrap().clear(); OUTGOING.lock().unwrap().clear(); PENDING.lock().unwrap().clear(); }
pub fn install(g: G) {
    for (name, hook, slot) in [
        ("gml_Script_anon@1052@gml_Object_objDmgNumbers_Create_0", create as *const (), &CREATE),
        ("gml_Object_objDmgNumbers_Draw_64", draw as *const (), &DRAW),
    ] {
        if slot.load(Ordering::Relaxed) == 0 {
            match crate::hooks::install(g.rt, name, hook) {
                Ok(f) => slot.store(f as usize, Ordering::SeqCst),
                Err(e) => crate::log(&format!("damage numbers: {e}")),
            }
        }
    }
}
unsafe extern "C-unwind" fn create(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    // The native caller has already calculated the displayed amount. Capture
    // enemy/player damage presentation, excluding value rewards.
    let player=PLAYER_DEPTH.load(Ordering::Relaxed)!=0;
    if (player || (DAMAGE_DEPTH.load(Ordering::Relaxed)!=0&&!super::enemies::PUPPET_MODE.load(Ordering::Relaxed)))
        && super::rooms::RUN_SEED.load(Ordering::Relaxed)!=0 && n>=4 && !a.is_null() {
        if let Some(g) = G::new() {
            let args = unsafe { std::slice::from_raw_parts(a, 4) };
            if args.iter().all(|p| !p.is_null()) {
                let values: Vec<V> = args.iter().map(|p| unsafe { **p }).collect();
                let (level, sub) = super::rooms::level_sub(g);
                let number = Number { room: g.room_name(), level, sub,
                    x: values[0].as_f64().unwrap_or(f64::NAN), y: values[1].as_f64().unwrap_or(f64::NAN),
                    amount: values[2].as_f64().unwrap_or(f64::NAN), vx: g.num(values[3],"x"), vy: g.num(values[3],"y"),
                };
                if number.sane() {
                    let mut outgoing = if player{PLAYER_OUTGOING.lock().unwrap()}else{OUTGOING.lock().unwrap()};
                    if outgoing.len() < 4096 { outgoing.push(number); }
                }
            }
        }
    }
    let f: ScriptFn = unsafe { std::mem::transmute(CREATE.load(Ordering::Relaxed)) };
    unsafe { f(s,o,r,n,a) }
}
pub fn receive(g: G, numbers: Vec<Number>) {
    let room = g.room_name(); let level = super::rooms::level_sub(g);
    let mut pending = PENDING.lock().unwrap();
    for number in numbers {
        if number.room == room && (number.level,number.sub) == level && pending.len() < 4096 { pending.push(number); }
    }
}
unsafe extern "C-unwind" fn draw(s: *mut c_void, o: *mut c_void) {
    let pending = std::mem::take(&mut *PENDING.lock().unwrap());
    if !pending.is_empty() && CREATE.load(Ordering::Relaxed) != 0 {
        if let Some(g) = G::new() {
            if let (Some(owner), Some(player)) = (unsafe { g.rt.builtin_var_of("id",s) }, g.first("objPlayer")) {
                let mut vector = g.get(owner,"focmp_number_vector");
                if !is_struct(&vector) {
                    vector = g.call("variable_clone", &[g.get(player,"v2_knockback")]);
                    g.set(owner,"focmp_number_vector",vector);
                }
                if is_struct(&vector) {
                    let create: ScriptFn = unsafe { std::mem::transmute(CREATE.load(Ordering::Relaxed)) };
                    for number in pending {
                        if number.room != g.room_name() || (number.level,number.sub) != super::rooms::level_sub(g) { continue; }
                        g.set(vector,"x",real(number.vx)); g.set(vector,"y",real(number.vy));
                        let mut args = [real(number.x),real(number.y),real(number.amount),vector];
                        let mut ptrs: Vec<*mut V> = args.iter_mut().map(|v| v as *mut V).collect();
                        let mut result = V::undefined();
                        unsafe { create(s,o,&mut result,4,ptrs.as_mut_ptr()) };
                    }
                }
            }
        }
    }
    let f: unsafe extern "C-unwind" fn(*mut c_void,*mut c_void) = unsafe { std::mem::transmute(DRAW.load(Ordering::Relaxed)) };
    unsafe { f(s,o) };
}
