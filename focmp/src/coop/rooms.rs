//! Shared run seed and deterministic room generation.
//!
//! The host picks a run seed when the run starts (lobby → "start run") and
//! sends it to everyone. Each machine then re-seeds GameMaker's RNG at the
//! entry of every generation step with
//!
//!   seed = hash(run_seed, global.level.level, global.level.sub, step)
//!
//! so peers on the same level/sub build the same room without any
//! mid-transition messaging. Generation steps (detoured):
//!   1. objRoomBuilder's builder method (shuffles the room fragments)
//!   2. objTileBuilder Create (tiles, enemy spots)
//!   3. objWaveManager Create (waves)
//! After entering a fight room each peer hashes `objTileBuilder.array_enemy_spots`
//! and sends it; `coop` shows whether layouts match.

use crate::engine::{G, V, real};
use crate::gm::RValue;
use crate::hooks::{self, ScriptFn};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// 0 = no co-op run in progress.
pub static RUN_SEED: AtomicU64 = AtomicU64::new(0);

const BUILDER: &str = "gml_Script_anon@2046@gml_Object_objRoomBuilder_Create_0";
const TILES: &str = "gml_Object_objTileBuilder_Create_0";
const WAVES: &str = "gml_Object_objWaveManager_Create_0";

type EventFn = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void);
static ORIG_BUILDER: AtomicUsize = AtomicUsize::new(0);
static ORIG_TILES: AtomicUsize = AtomicUsize::new(0);
static ORIG_WAVES: AtomicUsize = AtomicUsize::new(0);
static ORIG_BOSS: AtomicUsize = AtomicUsize::new(0);

pub fn install_hooks(g: G) {
    let list: [(&str, *const (), &AtomicUsize); 4] = [
        (BUILDER, hk_builder as *const (), &ORIG_BUILDER),
        (TILES, hk_tiles as *const (), &ORIG_TILES),
        (WAVES, hk_waves as *const (), &ORIG_WAVES),
        ("gml_Script_anon@2778@___struct___628@gml_Object_objGameProgress_Create_0", hk_boss as *const (), &ORIG_BOSS),
    ];
    for (name, f, orig) in list {
        if orig.load(Ordering::Relaxed) != 0 {
            continue;
        }
        match hooks::install(g.rt, name, f) {
            Ok(t) => orig.store(t as usize, Ordering::SeqCst),
            Err(e) => crate::log(&format!("rooms: {e}")),
        }
    }
}

unsafe extern "C-unwind" fn hk_boss(s:*mut c_void,o:*mut c_void,ret:*mut V,argc:i32,args:*mut *mut V)->*mut V {
    let original:ScriptFn=unsafe{std::mem::transmute(ORIG_BOSS.load(Ordering::Relaxed))};
    let run=RUN_SEED.load(Ordering::Relaxed);
    let Some(g)=G::new().filter(|_|run!=0) else{return unsafe{original(s,o,ret,argc,args)}};
    let level=V{v:s as u64,flags:0,kind:crate::gm::KIND_OBJECT};
    let previous_seed=g.get(level,"focmp_boss_run");
    if previous_seed.v!=run {
        // Native level structs can predate the co-op run. Their menu-time random
        // previous condition must not alter the no-repeat selection loop.
        g.set(level,"boss_i",real(-1.0));g.set(level,"boss_i2",real(-1.0));
        g.set(level,"focmp_boss_run",V{v:run,flags:0,kind:crate::gm::KIND_INT64});
    }
    let seed=mix(run^mix(((g.num(level,"level") as u64)<<32)|g.num(level,"sub") as u64)^mix(8))&0x7fff_ffff;
    super::rng::install(g);
    let _random=super::rng::Scope::seeded(g,seed as u32);
    let result=unsafe{original(s,o,ret,argc,args)};
    crate::log(&format!("rooms: boss conditions {}.{} = {}/{}",g.num(level,"level"),g.num(level,"sub"),g.num(level,"boss_i"),g.num(level,"boss_i2")));
    result
}

/// (level, sub) from `global.level`.
pub fn level_sub(g: G) -> (u32, u32) {
    let lv = g.global("level");
    if !crate::engine::is_struct(&lv) {
        return (0, 0);
    }
    (g.num(lv, "level") as u32, g.num(lv, "sub") as u32)
}

/// Personal screens belonging to a hub visit, including its post-fight rewards.
/// The caller must also match the run and level/sub; this is not permission to
/// cross a shared stage transition. Keep run setup, fights and endings out.
pub fn personal_hub_room(room: &str) -> bool {
    matches!(room,
        "Room_hub" | "Room_hub_1" | "Room_shop"
        | "Room_select_card" | "Room_select_tool" | "Room_show_cards"
        | "Room_combine" | "Room_duplicate" | "Room_flexmod" | "Room_flexsticker"
        | "Room_mod_fusion" | "Room_random_multiplier" | "Room_remove_mod"
        | "Room_rotate_mods" | "Room_slot_machine" | "Room_splitcard"
        | "Room_sticker" | "Room_swap_mods" | "Room_transfer" | "Room_transfer_tutorial" | "Room_upgrade"
        | "Room_select_perk_game" | "Room_show_perk_game_gambler" | "Room_new_unlocks"
    )
}

fn mix(mut x: u64) -> u64 {
    // splitmix64 finalizer
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

pub(super) fn reseed(step: u64) {
    let run = RUN_SEED.load(Ordering::Relaxed);
    if run == 0 {
        return;
    }
    let Some(g) = G::new() else { return };
    let (level, sub) = level_sub(g);
    let seed = mix(run ^ mix(((level as u64) << 32) | sub as u64) ^ mix(step)) & 0x7fff_ffff;
    g.call("random_set_seed", &[real(seed as f64)]);
    crate::log(&format!("rooms: step {step} level {level}.{sub} seed {seed}"));
}

unsafe extern "C-unwind" fn hk_builder(s: *mut c_void, o: *mut c_void, ret: *mut RValue, argc: i32, args: *mut *mut RValue) -> *mut RValue {
    reseed(1);
    let f: ScriptFn = unsafe { std::mem::transmute(ORIG_BUILDER.load(Ordering::Relaxed)) };
    unsafe { f(s, o, ret, argc, args) }
}

unsafe extern "C-unwind" fn hk_tiles(s: *mut c_void, o: *mut c_void) {
    reseed(2);
    let f: EventFn = unsafe { std::mem::transmute(ORIG_TILES.load(Ordering::Relaxed)) };
    unsafe { f(s, o) }
}

unsafe extern "C-unwind" fn hk_waves(s: *mut c_void, o: *mut c_void) {
    reseed(3);
    let f: EventFn = unsafe { std::mem::transmute(ORIG_WAVES.load(Ordering::Relaxed)) };
    unsafe { f(s, o) }
}

/// Hash of the current fight room layout (enemy spots), if there is one.
pub fn layout_hash(g: G) -> Option<u64> {
    let tb = g.first("objTileBuilder")?;
    let spots: V = g.get(tb, "array_enemy_spots");
    let n = g.arr_len(spots);
    if n == 0 {
        return None;
    }
    let mut h = mix(n as u64);
    for i in 0..n {
        let p = g.arr_get(spots, i);
        let (x, y) = (g.num(p, "x") as i64, g.num(p, "y") as i64);
        h = mix(h ^ ((x as u64) << 32 | (y as u64 & 0xffff_ffff)));
    }
    Some(h)
}

#[derive(Clone,Debug,bitcode::Encode,bitcode::Decode)]
pub struct BossState{pub run:u64,pub level:u32,pub sub:u32,pub first:i16,pub second:i16}
impl BossState{pub fn sane(&self)->bool{self.run!=0&&(0..64).contains(&self.first)&&(0..64).contains(&self.second)}}
pub fn capture_boss(g:G)->Option<BossState>{
    let lv=g.global("level");if !crate::engine::is_struct(&lv){return None;}
    let state=BossState{run:RUN_SEED.load(Ordering::Relaxed),level:g.num(lv,"level") as u32,sub:g.num(lv,"sub") as u32,first:g.num(lv,"boss_i") as i16,second:g.num(lv,"boss_i2") as i16};
    state.sane().then_some(state)
}
pub fn apply_boss(g:G,b:BossState){
    if b.run!=RUN_SEED.load(Ordering::Relaxed)||(b.level,b.sub)!=level_sub(g)||!b.sane(){return;}
    let Some(progress)=g.first("objGameProgress")else{return};let list=g.get(progress,"bosslist");
    let lv=g.global("level");
    for (field,index,name) in [("boss_i",b.first,"boss"),("boss_i2",b.second,"boss2")]{
        if (index as usize)<g.arr_len(list){g.set(lv,field,real(index as f64));g.set(lv,name,g.arr_get(list,index as usize));}
    }
}
