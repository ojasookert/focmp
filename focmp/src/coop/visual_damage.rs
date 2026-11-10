//! Guard native damage methods against cosmetic remote projectile copies.
use crate::engine::{G, V, real, truthy};
use crate::hooks::ScriptFn;
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
static SLOTS: [AtomicUsize; 16] = [const { AtomicUsize::new(0) }; 16];
static PLAYER_SLOTS: [AtomicUsize; 16] = [const { AtomicUsize::new(0) }; 16];
static SEEN: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
pub fn suppress(other: *mut c_void, argc: i32, args: *mut *mut V) -> bool {
    if super::projectiles::replaying() {
        return true;
    }
    let Some(g) = G::new() else { return false };
    if !other.is_null() {
        if let Some(id) = unsafe { g.rt.builtin_var_of("id", other) } {
            if super::projectiles::visual(g, id) {
                return true;
            }
        }
    }
    if argc > 7 && !args.is_null() {
        let source = unsafe { **args.add(7) };
        if super::projectiles::visual(g, source) {
            return true;
        }
    }
    false
}
// Scale only enemy-origin damage at the local player's authoritative damage
// entry. The wire tags projectile ancestry, so remote explosions/child shots
// receive the same factor exactly once; friendly fire and self-costs do not.
fn enemy_source(g: G, mut source: V) -> bool {
    for _ in 0..4 {
        if !g.exists(source) { return false; }
        if truthy(&g.get(source, "focmp_enemy_attack")) || g.num(source, "focmp_gid") > 0.0 { return true; }
        source = g.get(source, "Owner");
    }
    false
}
unsafe fn scaled_damage(o: *mut c_void, n: i32, a: *mut *mut V) -> Option<Vec<V>> {
    if super::rooms::RUN_SEED.load(Ordering::Relaxed) == 0 || n < 1 || a.is_null() { return None; }
    let g = G::new()?;
    let other = unsafe { g.rt.builtin_var_of("id", o) }.unwrap_or(V::undefined());
    let source = if n > 7 && !unsafe { *a.add(7) }.is_null() { unsafe { **a.add(7) } } else { V::undefined() };
    if !enemy_source(g, other) && !enemy_source(g, source) { return None; }
    let mut values = Vec::new();
    for i in 0..n as usize { let p = unsafe { *a.add(i) }; if p.is_null() { return None; } values.push(unsafe { *p }); }
    values[0] = real(values[0].as_f64()? * crate::config::RUN_RULES.lock().unwrap().damage_scale());
    Some(values)
}
macro_rules! guards {($($i:literal=>$name:ident),*)=>{
    $(unsafe extern "C-unwind" fn $name(s:*mut c_void,o:*mut c_void,r:*mut V,n:i32,a:*mut *mut V)->*mut V{
        if PLAYER_SLOTS[$i].load(Ordering::Relaxed)==0 && suppress(o,n,a){unsafe{*r=real(0.0)};return r;}
        let prop=if PLAYER_SLOTS[$i].load(Ordering::Relaxed)==0 {
            G::new().and_then(|g| unsafe {g.rt.builtin_var_of("id",s)}.and_then(|e|unsafe{super::props::damage(g,e,n,a)}))
        }else{None};
        if super::enemies::PUPPET_MODE.load(Ordering::Relaxed) {
            if let Some(hit)=prop.as_ref() {super::props::publish(hit.clone());unsafe{*r=real(0.0)};return r;}
        }
        let _number=(PLAYER_SLOTS[$i].load(Ordering::Relaxed)!=0).then(super::damage_numbers::player_damage);
        let _life=(PLAYER_SLOTS[$i].load(Ordering::Relaxed)!=0).then(super::life::damage_scope);
        let _random=super::shot_events::active().then(super::rng::Scope::preserve);
        let f:ScriptFn=unsafe{std::mem::transmute(SLOTS[$i].load(Ordering::Relaxed))};
        if PLAYER_SLOTS[$i].load(Ordering::Relaxed)!=0 {
            if let Some(mut values)=unsafe{scaled_damage(o,n,a)} {
                let mut ptrs:Vec<*mut V>=values.iter_mut().map(|v|v as *mut V).collect();
                return unsafe{f(s,o,r,n,ptrs.as_mut_ptr())};
            }
        }
        let result=unsafe{f(s,o,r,n,a)};
        if let Some(hit)=prop {super::props::publish(hit);}
        result
    })*
    fn functions()->Vec<*const ()>{vec![$($name as *const ()),*]}
}}
guards!(0=>g0,1=>g1,2=>g2,3=>g3,4=>g4,5=>g5,6=>g6,7=>g7,8=>g8,9=>g9,10=>g10,11=>g11,12=>g12,13=>g13,14=>g14,15=>g15);
pub fn ensure_player(g:G){if !PLAYER_SLOTS.iter().any(|s|s.load(Ordering::Relaxed)!=0)&&g.first("objPlayer").is_some(){install(g);}}
pub fn install(g: G) {
    for obj in [
        "objPlayer",
        "objColHitables",
        "objDrumMum",
        "objFlyingTankMum",
        "objStaticTankMum",
        "objBlackholeDomeWall",
        "objIngameButton_shoot",
    ] {
        for inst in g.instances(obj) {
            let method = g.get(inst, "set_damage");
            if !crate::engine::is_struct(&method) || !truthy(&g.call("is_method", &[method])) {
                continue;
            }
            let index = g.call("method_get_index", &[method]);
            let name = g.str_of(&g.call("script_get_name", &[index]));
            let name = if name.starts_with("gml_") {
                name
            } else {
                format!("gml_Script_{name}")
            };
            let mut seen = SEEN.lock().unwrap();
            if seen.contains(&name) {
                continue;
            }
            if crate::hooks::list().contains(&name) {
                seen.insert(name);
                continue;
            }
            let Some(i) = SLOTS.iter().position(|s| s.load(Ordering::Relaxed) == 0) else {
                return;
            };
            match crate::hooks::install(g.rt, &name, functions()[i]) {
                Ok(f) => {
                    PLAYER_SLOTS[i].store((obj=="objPlayer") as usize,Ordering::SeqCst);
                    SLOTS[i].store(f as usize, Ordering::SeqCst);
                    seen.insert(name);
                }
                Err(e) => {
                    crate::log(&format!("visual damage: {e}"));
                    seen.insert(name);
                }
            }
        }
    }
}
