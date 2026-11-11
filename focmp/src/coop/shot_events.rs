//! Per-projectile event random streams. Drawing and other actors cannot consume
//! randomness belonging to this projectile's next simulation event.
use super::rng;
use crate::{
    engine::{G, boolean, real},
    hooks,
};
use std::{
    cell::RefCell,
    ffi::c_void,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
};
#[derive(Clone, Copy)]
pub struct Group {
    pub peer: u8,
    pub id: u32,
    pub ordinal: u32,
    pub remote: bool,
    pub enemy: bool,
    pub salt: u32,
}
thread_local! {static GROUP:RefCell<Option<Group>>=const{RefCell::new(None)};}
static NEXT: AtomicU32 = AtomicU32::new(1);
pub fn next_id() -> u32 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}
pub struct Scope(Option<Group>);
pub fn group(peer: u8, id: u32, remote: bool, enemy: bool) -> Scope {
    Scope(GROUP.with(|g| {
        g.replace(Some(Group {
            peer,
            id,
            remote,
            enemy,
            ordinal: 0,
            salt: rng::capture().map_or(0, |s| s.salt()),
        }))
    }))
}
impl Drop for Scope {
    fn drop(&mut self) {
        GROUP.with(|g| g.replace(self.0));
    }
}
static IN_EVENT: AtomicUsize = AtomicUsize::new(0);
pub fn active() -> bool {
    IN_EVENT.load(Ordering::Relaxed) != 0
}
struct Active;
impl Drop for Active {
    fn drop(&mut self) {
        IN_EVENT.fetch_sub(1, Ordering::Relaxed);
    }
}
static TRACE: AtomicBool = AtomicBool::new(false);
static AUDIT: Mutex<Vec<String>> = Mutex::new(Vec::new());
pub fn trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
    AUDIT.lock().unwrap().clear();
}
pub fn audit() -> String {
    format!("[{}]", AUDIT.lock().unwrap().join(","))
}
pub fn reset() {
    NEXT.store(1, Ordering::Relaxed);
    AUDIT.lock().unwrap().clear();
}
type Event = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);
static EVENTS: LazyLock<Vec<(&str, AtomicUsize)>> = LazyLock::new(|| {
    vec![
        ("gml_Object_objBoom_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objBoom_Alarm_2", AtomicUsize::new(0)),
        ("gml_Object_objBoom_Alarm_3", AtomicUsize::new(0)),
        ("gml_Object_objBoom_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objBoom_Other_7", AtomicUsize::new(0)),
        ("gml_Object_objBoom_Other_10", AtomicUsize::new(0)),
        ("gml_Object_objBoom_CleanUp_0", AtomicUsize::new(0)),
        ("gml_Object_objBullit_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objBullit_Alarm_0", AtomicUsize::new(0)),
        ("gml_Object_objBullit_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objBullit_Other_10", AtomicUsize::new(0)),
        ("gml_Object_objBullit_Other_11", AtomicUsize::new(0)),
        ("gml_Object_objBullit_Other_12", AtomicUsize::new(0)),
        ("gml_Object_objBullit_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objBullit_CleanUp_0", AtomicUsize::new(0)),
        ("gml_Object_objHomingBoom_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objHomingBoom_Destroy_0", AtomicUsize::new(0)),
        ("gml_Object_objHomingBoom_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objHomingBoom_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objLaser_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objLaser_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objLaser_Other_10", AtomicUsize::new(0)),
        ("gml_Object_objLaser_Other_11", AtomicUsize::new(0)),
        ("gml_Object_objLaser_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objLaser_CleanUp_0", AtomicUsize::new(0)),
        ("gml_Object_objLaserSweep_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objLaserSweep_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objLaserSweep_Other_7", AtomicUsize::new(0)),
        ("gml_Object_objLaserSweep_CleanUp_0", AtomicUsize::new(0)),
        ("gml_Object_objMelee_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objMelee_Alarm_0", AtomicUsize::new(0)),
        ("gml_Object_objMelee_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objMelee_Other_7", AtomicUsize::new(0)),
        ("gml_Object_objMelee_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objMelee_CleanUp_0", AtomicUsize::new(0)),
        ("gml_Object_objMeleeTwoHanded_Create_0", AtomicUsize::new(0)),
        (
            "gml_Object_objMeleeTwoHanded_Destroy_0",
            AtomicUsize::new(0),
        ),
        ("gml_Object_objMeleeTwoHanded_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objMeleeTwoHanded_Draw_0", AtomicUsize::new(0)),
        (
            "gml_Object_objMeleeTwoHanded_CleanUp_0",
            AtomicUsize::new(0),
        ),
        ("gml_Object_objPlasma_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objPlasma_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objPlasma_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objPlasma_CleanUp_0", AtomicUsize::new(0)),
        ("gml_Object_objSpawnBoom_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objArea_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objArea_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objArea_Other_10", AtomicUsize::new(0)),
        ("gml_Object_objArea_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objArea_CleanUp_0", AtomicUsize::new(0)),
        ("gml_Object_objBlackhole_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objBlackhole_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objBlackhole_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objBlackhole_CleanUp_0", AtomicUsize::new(0)),
        ("gml_Object_objDrumBullet_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objDrumBullet_Destroy_0", AtomicUsize::new(0)),
        ("gml_Object_objDrumBullet_Alarm_0", AtomicUsize::new(0)),
        ("gml_Object_objDrumBullet_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objDrumBullet_Other_10", AtomicUsize::new(0)),
        ("gml_Object_objDrumMum_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objDrumMum_Alarm_11", AtomicUsize::new(0)),
        ("gml_Object_objDrumMum_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objDrumMum_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objDrumMum_CleanUp_0", AtomicUsize::new(0)),
        ("gml_Object_objHitscan_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objHitscan_Alarm_0", AtomicUsize::new(0)),
        ("gml_Object_objHitscan_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objTeslaCoil_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objWaitTrigger_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objWaitTrigger_Destroy_0", AtomicUsize::new(0)),
        ("gml_Object_objWaitTrigger_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objWaitTrigger_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objFloating_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objFloating_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objFloating_Other_10", AtomicUsize::new(0)),
        ("gml_Object_objFloating_Other_11", AtomicUsize::new(0)),
        ("gml_Object_objFloating_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objFloating_CleanUp_0", AtomicUsize::new(0)),
        ("gml_Object_objDarkMatter_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objDarkMatter_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objDarkMatter_Draw_0", AtomicUsize::new(0)),
        ("gml_Object_objDarkMatter_1_Create_0", AtomicUsize::new(0)),
        ("gml_Object_objDarkMatter_1_Step_0", AtomicUsize::new(0)),
        ("gml_Object_objDarkMatter_1_Draw_0", AtomicUsize::new(0)),
    ]
});
macro_rules! events {($($i:literal=>$name:ident),*)=>{
 $(unsafe extern "C-unwind" fn $name(s:*mut c_void,o:*mut c_void){unsafe{event(s,o,$i)}})*
 fn functions()->Vec<*const ()>{vec![$($name as *const ()),*]}
}}
events!(0=>e0,1=>e1,2=>e2,3=>e3,4=>e4,5=>e5,6=>e6,7=>e7,8=>e8,9=>e9,10=>e10,11=>e11,12=>e12,13=>e13,14=>e14,15=>e15,16=>e16,17=>e17,18=>e18,19=>e19,20=>e20,21=>e21,22=>e22,23=>e23,24=>e24,25=>e25,26=>e26,27=>e27,28=>e28,29=>e29,30=>e30,31=>e31,32=>e32,33=>e33,34=>e34,35=>e35,36=>e36,37=>e37,38=>e38,39=>e39,40=>e40,41=>e41,42=>e42,43=>e43,44=>e44,45=>e45,46=>e46,47=>e47,48=>e48,49=>e49,50=>e50,51=>e51,52=>e52,53=>e53,54=>e54,55=>e55,56=>e56,57=>e57,58=>e58,59=>e59,60=>e60,61=>e61,62=>e62,63=>e63,64=>e64,65=>e65,66=>e66,67=>e67,68=>e68,69=>e69,70=>e70,71=>e71,72=>e72,73=>e73,74=>e74,75=>e75,76=>e76,77=>e77,78=>e78,79=>e79,80=>e80,81=>e81,82=>e82,83=>e83);
pub fn install(g: G) {
    for (i, (name, slot)) in EVENTS.iter().enumerate() {
        if slot.load(Ordering::Relaxed) == 0 {
            match hooks::install(g.rt, name, functions()[i]) {
                Ok(f) => slot.store(f as usize, Ordering::Relaxed),
                Err(e) => crate::log(&format!("shot events: {e}")),
            }
        }
    }
}
unsafe fn event(s: *mut c_void, o: *mut c_void, i: usize) {
    let f: Event = unsafe { std::mem::transmute(EVENTS[i].1.load(Ordering::Relaxed)) };
    let Some(g) = G::new().filter(|_| super::rooms::RUN_SEED.load(Ordering::Relaxed) != 0) else {
        unsafe { f(s, o) };
        return;
    };
    let Some(inst) = (unsafe { g.rt.builtin_var_of("id", s) }) else {
        unsafe { f(s, o) };
        return;
    };
    if EVENTS[i].0.ends_with("_Create_0") && g.num(inst, "focmp_shot_id") == 0.0 {
        let identity = GROUP.with(|scope| {
            let mut scope = scope.borrow_mut();
            let group = scope.as_mut()?;
            group.ordinal += 1;
            Some(*group)
        });
        if let Some(id) = identity {
            g.set(inst, "focmp_shot_peer", real(id.peer as f64));
            g.set(inst, "focmp_shot_id", real(id.id as f64));
            g.set(inst, "focmp_shot_part", real(id.ordinal as f64));
            g.set(inst, "focmp_visual", boolean(id.remote));
            g.set(inst, "focmp_enemy_attack", boolean(id.enemy));
            g.set(inst, "focmp_shot_salt", real(id.salt as f64));
        }
    }
    let id = g.num(inst, "focmp_shot_id") as u32;
    if id == 0 {
        if EVENTS[i].0 == "gml_Object_objDrumMum_Step_0" && super::props::client_step(g,inst,false) { return; }
        unsafe { f(s, o) };
        return;
    }
    let peer = g.num(inst, "focmp_shot_peer") as u8;
    let part = g.num(inst, "focmp_shot_part") as u32;
    let field = format!("focmp_rng_event_{i}");
    let count = g.num(inst, &field) as u32;
    g.set(inst, &field, real(count.wrapping_add(1) as f64));
    let seed = rng::mix(
        g.num(inst, "focmp_shot_salt") as u64
            ^ ((peer as u64) << 56)
            ^ id as u64
            ^ rng::mix(part as u64)
            ^ rng::mix(((i as u64) << 32) | count as u64),
    ) as u32;
    let _random = rng::Scope::seeded(g, seed);
    IN_EVENT.fetch_add(1, Ordering::Relaxed);
    let _active = Active;
    unsafe { f(s, o) };
    if TRACE.load(Ordering::Relaxed) && g.exists(inst) && count < 12 {
        let name = EVENTS[i].0;
        if name.ends_with("_Create_0") || name.ends_with("_Step_0") {
            let row = g.call("json_parse", &[g.s("{}")]);
            g.call("variable_global_set", &[g.s("focmp_shot_audit_row"), row]);
            for (k, v) in [
                ("peer", peer as f64),
                ("id", id as f64),
                ("part", part as f64),
                ("count", count as f64),
            ] {
                g.set(row, k, real(v));
            }
            g.set(row, "event", g.s(name));
            for k in [
                "x",
                "y",
                "angle",
                "image_angle",
                "speed",
                "spd",
                "bullit_speed",
                "damage",
                "dmg",
                "life",
            ] {
                let v = g.get(inst, k);
                if v.as_f64().is_some() {
                    g.set(row, k, v);
                }
            }
            let mut audit = AUDIT.lock().unwrap();
            if audit.len() < 8192 {
                audit.push(g.str_of(&g.call("json_stringify", &[row])));
            }
        }
    }
}
