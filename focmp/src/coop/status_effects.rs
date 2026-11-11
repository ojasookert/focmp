//! Host-owned enemy status lifetime; clients run only native fire presentation.
use crate::{
    engine::{G, V, boolean, is_struct, real, truthy},
    hooks::ScriptFn,
};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
static ENTER: AtomicUsize = AtomicUsize::new(0);
static EXIT: AtomicUsize = AtomicUsize::new(0);
static CHANGE: AtomicUsize = AtomicUsize::new(0);
static APPLYING: AtomicBool = AtomicBool::new(false);
pub fn install(g: G) {
    if CHANGE.load(Ordering::Relaxed) == 0 {
        match crate::hooks::install(g.rt, "gml_Script_anon@19232@SnowState@SnowState", change as *const ()) {
            Ok(p) => CHANGE.store(p as usize, Ordering::SeqCst),
            Err(e) => crate::log(&e),
        }
    }
    if ENTER.load(Ordering::Relaxed) != 0 {
        return;
    }
    match crate::hooks::install(
        g.rt,
        "gml_Script_anon@21803@___struct___600@gml_Object_objEnemyMASTER_Create_0",
        enter as *const (),
    ) {
        Ok(p) => ENTER.store(p as usize, Ordering::SeqCst),
        Err(e) => crate::log(&e),
    }
    if let Some(p) = g.rt.script("gml_Script_anon@22737@___struct___600@gml_Object_objEnemyMASTER_Create_0") {
        EXIT.store(p, Ordering::SeqCst);
    }
}
// Clients receive the host's FSM state without running its gameplay entry.
// Native predicted hits must not run leave/enter callbacks either: e.g. stun
// leave restores was_shielded, which was never initialized on that client.
unsafe extern "C-unwind" fn change(s:*mut c_void,o:*mut c_void,r:*mut V,n:i32,a:*mut *mut V)->*mut V {
    if !s.is_null() && super::life::suppress_hibernate() && n > 0 && !a.is_null()
        && unsafe { !(*a).is_null() && (**a).as_str().as_deref() == Some("hybernate") } {
        unsafe { *r = V { v: s as u64, flags: 0, kind: crate::gm::KIND_OBJECT }; }
        return r;
    }
    if !s.is_null() && super::enemies::PUPPET_MODE.load(Ordering::Relaxed) {
        if let Some(g)=G::new() {
            let fsm=V{v:s as u64,flags:0,kind:crate::gm::KIND_OBJECT};
            if truthy(&g.get(fsm,"focmp_host_owned")) {
                // SnowState.change returns self for fluent calls.
                unsafe{*r=fsm}; return r;
            }
        }
    }
    let f:ScriptFn=unsafe{std::mem::transmute(CHANGE.load(Ordering::Relaxed))};
    unsafe{f(s,o,r,n,a)}
}
unsafe extern "C-unwind" fn enter(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    if super::enemies::PUPPET_MODE.load(Ordering::Relaxed) && !APPLYING.load(Ordering::Relaxed) {
        unsafe { *r = V::undefined() };
        return r;
    }
    let f: ScriptFn = unsafe { std::mem::transmute(ENTER.load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) }
}
pub fn visual_step(s: *mut c_void) {
    let Some(g) = G::new() else { return };
    let Some(e) = (unsafe { g.rt.builtin_var_of("id", s) }) else {
        return;
    };
    let active = truthy(&g.get(e, "focmp_host_onfire")) && !truthy(&g.get(e, "dead"));
    let present = truthy(&g.get(e, "focmp_fire_present"));
    if active != present {
        let address = if active {
            ENTER.load(Ordering::Relaxed)
        } else {
            EXIT.load(Ordering::Relaxed)
        };
        if address != 0 {
            let f: ScriptFn = unsafe { std::mem::transmute(address) };
            let mut r = V::undefined();
            APPLYING.store(true, Ordering::Relaxed);
            crate::log(&format!("fire presentation: enter={active} begin"));
            unsafe { f(s, s, &mut r, 0, std::ptr::null_mut()) };
            crate::log("fire presentation: native transition returned");
            APPLYING.store(false, Ordering::Relaxed);
            g.set(e, "focmp_fire_present", boolean(active));
        }
    }
    if active {
        let fire = g.get(e, "fire_struct");
        if is_struct(&fire) {
            let args = g.call("array_create", &[real(3.0)]);
            for (i, v) in [g.get(e, "x"), g.get(e, "y"), g.get(e, "image_angle")]
                .into_iter()
                .enumerate()
            {
                g.call("array_set", &[args, real(i as f64), v]);
            }
            if !present {
                crate::log("fire presentation: first update");
            }
            // OnFire.update is a static constructor method, so method_call
            // needs an explicit self binding (native dot calls supply it).
            let update = g.call("method", &[fire, g.get(fire, "update")]);
            g.call("method_call", &[update, args]);
            if !present {
                crate::log("fire presentation: first update returned");
            }
        }
    }
    g.set(e, "onfire", boolean(active));
}

// Native drawing dispatches through SnowState.__history[0]. Mirror that state
// without calling change()/step(), which would run AI and damage a second time.
// The confusion/stun draw callback reads x_off, initialized and tweened by
// the host state entry. Clients skip that entry, so they need its actual value.
const STATUS_NUMBERS:[&str;11]=["frozen_amount","poison_stack","poisoned_tick_size","dmg_factor_frozen","count_i","mod_i","weak_i","leg_i","image_blend","hp_dmg_taken","x_off"];
const STATUS_FLAGS:[&str;7]=["frozen","poisoned","weak","shielded","nailed","draw_legs","draw_enemy"];
#[derive(Clone,Debug,Default,PartialEq,bitcode::Encode,bitcode::Decode)]
pub struct Status{pub state:String,pub values:[f32;11],pub flags:[bool;7]}
impl Status{pub fn sane(&self)->bool{self.state.len()<64&&self.state.bytes().all(|b|b.is_ascii_graphic()||b==b' ')&&self.values.iter().all(|v|v.is_finite()&&v.abs()<2.0e7)}}
pub fn capture(g:G,e:V)->Status{
 let fsm=g.get(e,"fsm");let history=if is_struct(&fsm){g.get(fsm,"__history")}else{V::undefined()};
 let state=if g.arr_len(history)>0{g.str_of(&g.arr_get(history,0))}else{String::new()};
 let status=Status{state,values:STATUS_NUMBERS.map(|k|g.num(e,k) as f32),flags:STATUS_FLAGS.map(|k|truthy(&g.get(e,k)))};
 if status.sane(){status}else{Status::default()}
}
pub fn apply(g:G,e:V,status:&Status){
 let fsm=g.get(e,"fsm");if is_struct(&fsm)&&!status.state.is_empty(){
  g.set(fsm,"focmp_host_owned",boolean(true));
  let states=g.get(fsm,"__states");let history=g.get(fsm,"__history");
  if is_struct(&states)&&g.arr_len(history)>0&&truthy(&g.call("variable_struct_exists",&[states,g.s(&status.state)])){
   g.call("array_set",&[history,real(0.0),g.s(&status.state)]);
  }
 }
 for(k,v)in STATUS_NUMBERS.iter().zip(status.values){g.set(e,k,real(v as f64));}
 for(k,v)in STATUS_FLAGS.iter().zip(status.flags){g.set(e,k,boolean(v));}
}
