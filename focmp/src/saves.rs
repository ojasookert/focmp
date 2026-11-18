//! Isolate modded saves before the first native load. Never import an active run.
//!
//! A co-op run saves exactly where a modded single-player run does: the
//! player's modded `savefile.json`, rewritten by the game's own autosave. There
//! is one save, the latest; "Start from save" resumes it and a team wipe ends it.
use crate::{
    engine::{G, V},
    hooks::ScriptFn,
};
use std::{
    ffi::c_void,
    path::PathBuf,
    sync::{Mutex, atomic::{AtomicUsize, Ordering}},
};
// While the native validator inspects a single-player save that is about to be
// imported, savefile.json resolves to a scratch copy instead: even the game's
// legacy save migration can then never write to the single-player file.
static PROBE: Mutex<Option<String>> = Mutex::new(None);
// The load that starts a saved co-op run: the file it reads and the host's stage.
static LOAD_WORLD: Mutex<Option<(String, World)>> = Mutex::new(None);
static LOAD_FILE: AtomicUsize = AtomicUsize::new(0);
static MAKE_SAVE: AtomicUsize = AtomicUsize::new(0);
const MAX_RUN_SAVE: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, bitcode::Encode, bitcode::Decode)]
pub struct Checkpoint {
    pub level: u32,
    pub sub: u32,
    pub total: u32,
    pub difficulty: u8,
}
impl Checkpoint {
    pub fn sane(&self) -> bool { self.level < 1000 && self.sub < 1000 && self.total < 100000 && self.difficulty < 128 }
    pub fn label(&self) -> String { format!("Save: {}.{}", self.level, self.sub) }
}

/// Only shared stage state crosses the wire. Health, perks, guns, cards and
/// personal lab state always come from this player's own save.
#[derive(Clone, Debug, bitcode::Encode, bitcode::Decode)]
pub struct World {
    pub checkpoint: Checkpoint,
    pub bosses: [i32; 2],
    pub target: Option<String>,
    pub target_type: Option<u8>,
    pub pick_perk: bool,
}
impl World {
    pub fn sane(&self) -> bool {
        self.checkpoint.sane() && self.bosses.iter().all(|v| (-1..1000).contains(v))
            && self.target.as_ref().is_none_or(|s| s.len() <= 128 && s.starts_with("R") && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    }
}
pub struct Prepared {
    /// A single-player save to copy into the (missing) modded save when the run starts.
    import: Option<Vec<u8>>,
    pub world: World,
}

fn save_base(g: G) -> Result<String, String> {
    let save = g.first("objSaveGame").ok_or("save system is not ready")?;
    let base = g.str_of(&g.get(save, "file_path")).replace('\\', "/");
    if !base.starts_with("game/") || !base.ends_with("_focmp") || base.contains("..") {
        return Err("save directory is not ready".into());
    }
    Ok(base)
}

fn method(g: G, owner: V, name: &str, args: &[V]) -> V {
    let values = g.call("array_create", &[V::real(0.0)]);
    for arg in args { g.call("array_push", &[values, *arg]); }
    let method = g.call("method", &[owner, g.global(name)]);
    g.call("method_call", &[method, values])
}

/// A fresh co-op run saves into the modded save and replaces the run that was
/// there, as starting a new single-player run does. Only refuse to start when
/// the saves could land outside the modded directory.
pub fn prepare_fresh(g: G) -> Result<(), String> {
    if g.room_name() != "Room_main_menu" { return Err("Start from the main menu".into()); }
    if PATH.load(Ordering::Relaxed) == 0 { return Err("Save isolation hook is unavailable".into()); }
    save_base(g).map(|_| ())
}

/// Read this player's saved run for the lobby without changing it. The modded
/// save is checked in place, as the main menu does for "continue". A player
/// with only a single-player save gets that one checked through a scratch
/// copy; it is imported into the modded save when the run actually starts.
pub fn prepare_local(g: G) -> Result<Prepared, String> {
    use std::io::Read;
    if g.room_name() != "Room_main_menu" { return Err("Start from the main menu".into()); }
    if PATH.load(Ordering::Relaxed) == 0 || LOAD_FILE.load(Ordering::Relaxed) == 0 {
        return Err("Save isolation hooks are unavailable".into());
    }
    let base = save_base(g)?;
    let root = root().ok_or("save directory unavailable")?;
    let own = format!("{base}/savefile.json");
    let scratch = format!("{base}/coop_import/savefile.json");
    let import = if root.join(&own).exists() { None } else {
        let source = root.join(base.trim_end_matches("_focmp")).join("savefile.json");
        let file = std::fs::File::open(source).map_err(|_| "No saved run found".to_string())?;
        let mut data = Vec::new();
        file.take((MAX_RUN_SAVE + 1) as u64).read_to_end(&mut data).map_err(|e| e.to_string())?;
        if data.is_empty() || data.len() > MAX_RUN_SAVE { return Err("Save is empty or too large".into()); }
        let path = root.join(&scratch);
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&path, &data).map_err(|e| e.to_string())?;
        Some(data)
    };
    let probe = if import.is_some() { scratch } else { own };
    let save = g.first("objSaveGame").ok_or("save system is not ready")?;
    let result = (|| {
        *PROBE.lock().unwrap() = import.is_some().then(|| probe.clone());
        let valid = crate::engine::truthy(&g.call("method_call", &[g.get(save, "valid_save_file_exist")]));
        *PROBE.lock().unwrap() = None;
        if !valid { return Err("Save is invalid or the run has ended".to_string()); }
        let data = method(g, save, "load_file_hmac", &[g.s(&probe)]);
        if !crate::engine::is_struct(&data) { return Err("Cannot read saved run".into()); }
        g.set(save, "focmp_save_probe", data);
        let integer = |name: &str| -> Result<u32, String> {
            let n = g.get(data, name).as_f64().ok_or_else(|| format!("Save has no {name}"))?;
            if !n.is_finite() || n < 0.0 || n > 100000.0 || n.fract() != 0.0 { return Err(format!("Invalid saved {name}")); }
            Ok(n as u32)
        };
        let difficulty = g.get(data, "difficulty");
        if g.arr_len(difficulty) != 7 { return Err("Invalid saved difficulty".into()); }
        let checkpoint = Checkpoint {
            level: integer("level")?, sub: integer("sub")?, total: integer("total")?,
            difficulty: (0..7).fold(0, |mask, i| mask | ((crate::engine::truthy(&g.arr_get(difficulty, i)) as u8) << i)),
        };
        let target = g.get(data, "room_target_string");
        let target = if target.kind == crate::gm::KIND_UNDEFINED { None }
            else { Some(g.str_of(&g.call("room_get_name", &[target]))) };
        let world = World {
            checkpoint,
            bosses: [g.num(data, "boss_i") as i32, g.num(data, "boss_i2") as i32],
            target, target_type: g.get(data, "room_target_type").as_f64().map(|n| n as u8),
            pick_perk: crate::engine::truthy(&g.get(data, "pick_perk")),
        };
        if !world.sane() { return Err("Unsupported saved progression".into()); }
        Ok(world)
    })();
    g.set(save, "focmp_save_probe", V::undefined());
    if import.is_some() { let _ = std::fs::remove_file(root.join(&probe)); }
    result.map(|world| Prepared { import, world })
}

/// Start the saved co-op run: load this player's own modded save with the
/// host's shared stage state. Autosaves keep going to that same file.
pub fn load_run(g: G, local: &Prepared, world: &World) {
    let (Ok(base), Some(root)) = (save_base(g), root()) else {
        crate::log("saves: cannot start the saved run: save directory is not ready");
        return;
    };
    let own = format!("{base}/savefile.json");
    if let Some(data) = &local.import {
        // Never replaces a modded save: this player had none when the lobby checked.
        use std::io::Write;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(root.join(&own)).and_then(|mut file| file.write_all(data)) {
            Ok(()) => crate::log("saves: imported the single-player run into the modded save"),
            Err(error) => crate::log(&format!("saves: single-player run not imported: {error}")),
        }
    }
    *LOAD_WORLD.lock().unwrap() = Some((own, world.clone()));
    if let Some(save) = g.first("objSaveGame") {
        g.call("method_call", &[g.get(save, "load")]);
    }
    *LOAD_WORLD.lock().unwrap() = None;
}

/// Record the run as ended, through the game's own routine: its death code
/// calls `save(true)`, and the flag becomes the record's "dead". A team wipe
/// uses this, because the players' individual deaths were saved as downed.
pub fn save_dead(g: G) {
    if let Some(save) = g.first("objSaveGame") {
        let routine = g.get(save, "save");
        if crate::engine::truthy(&g.call("is_method", &[routine])) {
            let arguments = g.call("array_create", &[V::real(1.0), crate::engine::boolean(true)]);
            g.call("method_call", &[routine, arguments]);
        }
    }
}

unsafe extern "C-unwind" fn load_file(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    let original: ScriptFn = unsafe { std::mem::transmute(LOAD_FILE.load(Ordering::Relaxed)) };
    unsafe { original(s, o, r, n, a) };
    if let Some(g) = G::new() {
        let pending = LOAD_WORLD.lock().unwrap().clone();
        if let Some((path, world)) = pending {
            if n > 0 && !a.is_null() && !r.is_null() && crate::engine::is_struct(&unsafe { *r })
                && path == g.str_of(&unsafe { **a }) {
                let data = unsafe { *r };
                // The lobby already matched level/sub/total/difficulty. Use
                // the host's stage target and boss choices to generate one
                // shared world without replacing anyone's personal loadout.
                g.set(data, "boss_i", V::real(world.bosses[0] as f64));
                g.set(data, "boss_i2", V::real(world.bosses[1] as f64));
                g.set(data, "room_target_string", world.target.as_ref().map_or(V::undefined(), |s| g.asset(s)));
                g.set(data, "room_target_type", world.target_type.map_or(V::undefined(), |n| V::real(n as f64)));
                g.set(data, "pick_perk", crate::engine::boolean(world.pick_perk));
            }
        }
    }
    r
}

/// The game's save record, built by its `MakeSaveFile` constructor. In co-op
/// a player who goes down is revived in the next room, so their save must not
/// say "dead" (that ends a run for good) while the team is still playing:
/// record what the revive will give them instead. A team wipe saves as dead.
unsafe extern "C-unwind" fn make_save(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    let original: ScriptFn = unsafe { std::mem::transmute(MAKE_SAVE.load(Ordering::Relaxed)) };
    let result = unsafe { original(s, o, r, n, a) };
    if crate::coop::rooms::RUN_SEED.load(Ordering::Relaxed) != 0 && !crate::coop::team_wiped() && !s.is_null() {
        if let Some(g) = G::new() {
            let record = V { v: s as u64, flags: 0, kind: crate::gm::KIND_OBJECT };
            // Also a downed player's ordinary autosave (a cleared room saves
            // them with the 1 HP the clear bookkeeping lends them).
            if crate::engine::truthy(&g.get(record, "dead")) || crate::coop::downed() {
                g.set(record, "dead", crate::engine::boolean(false));
                if let Some(player) = g.first("objPlayer") {
                    g.set(record, "hp_player_current", V::real((g.num(player, "hp_max") * 0.5).max(1.0)));
                }
            }
        }
    }
    result
}

static PATH: AtomicUsize = AtomicUsize::new(0);
static FILES: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];
fn root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Flashes_of_Chaos"))
}
fn copy_once(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    if to.exists() || !from.is_file() {
        return Ok(());
    }
    let mut src = std::fs::File::open(from)?;
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    std::io::copy(&mut src, &mut dst)?;
    Ok(())
}
pub fn install() {
    let Some(g) = G::new() else { return };
    if let Some(root) = root() {
        if let Err(e) = copy_once(
            &root.join("settings.json"),
            &root.join("settings_focmp.json"),
        ) {
            crate::log(&format!("saves: settings migration: {e}"));
        }
    }
    match crate::hooks::install(g.rt, "gml_Script_load_file_hmac", load_file as *const ()) {
        Ok(p) => LOAD_FILE.store(p as usize, Ordering::SeqCst),
        Err(e) => crate::log(&e),
    }
    match crate::hooks::install(g.rt, "gml_Script_MakeSaveFile@gml_Object_objSaveGame_Create_0", make_save as *const ()) {
        Ok(p) => MAKE_SAVE.store(p as usize, Ordering::SeqCst),
        Err(e) => crate::log(&e),
    }
    match crate::hooks::install(g.rt, "gml_Script_get_filepath", path as *const ()) {
        Ok(p) => PATH.store(p as usize, Ordering::SeqCst),
        Err(e) => crate::log(&e),
    }
    for (i, (name, f)) in [
        ("file_text_open_read", read as *const ()),
        ("file_text_open_write", write as *const ()),
        ("file_text_open_append", append as *const ()),
        ("file_exists", exists as *const ()),
        ("file_delete", delete as *const ()),
    ]
    .into_iter()
    .enumerate()
    {
        match crate::hooks::install(g.rt, name, f) {
            Ok(p) => FILES[i].store(p as usize, Ordering::SeqCst),
            Err(e) => crate::log(&e),
        }
    }
}
unsafe extern "C-unwind" fn path(
    s: *mut c_void,
    o: *mut c_void,
    r: *mut V,
    n: i32,
    a: *mut *mut V,
) -> *mut V {
    let f: ScriptFn = unsafe { std::mem::transmute(PATH.load(Ordering::Relaxed)) };
    unsafe { f(s, o, r, n, a) };
    if let Some(g) = G::new() {
        let old = g.str_of(&unsafe { *r });
        if let Some((base, file)) = old.rsplit_once(['/', '\\']) {
            if !base.ends_with("_focmp") {
                let new_base = format!("{base}_focmp");
                if let Some(root) = root() {
                    let src = root.join(base.replace('\\', "/"));
                    let dst = root.join(new_base.replace('\\', "/"));
                    let marker = dst.join(".focmp_initialized");
                    if !marker.exists() {
                        let result = (|| -> std::io::Result<()> {
                            std::fs::create_dir_all(&dst)?;
                            for file in [
                                "stats.json",
                                "stuffseen.json",
                                "bindings_GP.json",
                                "bindings_KBM.json",
                            ] {
                                copy_once(&src.join(file), &dst.join(file))?;
                            }
                            std::fs::write(
                                &marker,
                                b"Imported progression only; fresh modded run.\n",
                            )
                        })();
                        if let Err(e) = result {
                            crate::log(&format!("saves: migration {base}: {e}"));
                        }
                    }
                }
                g.call("directory_create", &[g.s(&new_base)]);
                unsafe { *r = g.s(&format!("{new_base}/{file}")) };
                if let Some(id) = unsafe { g.rt.builtin_var_of("id", s) } {
                    g.set(id, "file_path", g.s(&new_base));
                }
                crate::log(&format!("saves: {base} -> {new_base}"));
            }
        }
        let current = g.str_of(&unsafe { *r });
        if current.rsplit(['/', '\\']).next() == Some("savefile.json") {
            if let Some(path) = PROBE.lock().unwrap().as_ref() {
                unsafe { *r = g.s(path) };
            }
        }
    }
    r
}
// Builtins have a contiguous argument array, unlike compiled GML scripts.
type Builtin = unsafe extern "C-unwind" fn(*mut V, *mut c_void, *mut c_void, i32, *mut V);
unsafe fn file(i: usize, r: *mut V, s: *mut c_void, o: *mut c_void, n: i32, a: *mut V) {
    let f: Builtin = unsafe { std::mem::transmute(FILES[i].load(Ordering::Relaxed)) };
    if n > 0 && !a.is_null() {
        if let Some(g) = G::new() {
            let name = g.str_of(&unsafe { *a });
            if name == "settings.json"
                || name.ends_with("/settings.json")
                || name.ends_with("\\settings.json")
            {
                let mut args = unsafe { std::slice::from_raw_parts(a, n as usize) }.to_vec();
                args[0] = g.s(&format!("{}settings_focmp.json", &name[..name.len() - 13]));
                unsafe { f(r, s, o, n, args.as_mut_ptr()) };
                if i == 0 && unsafe { (*r).as_f64() } == Some(-1.0) {
                    // A first-time player has only the bundled settings.json,
                    // not a saved AppData copy for install() to migrate. Let
                    // GameMaker resolve its original read path (including its
                    // sandbox fallback); every write still goes to _focmp.
                    crate::log("saves: modded settings unavailable; reading native settings/defaults");
                    unsafe { f(r, s, o, n, a) };
                }
                return;
            }
        }
    }
    unsafe { f(r, s, o, n, a) };
}
macro_rules! files{($($name:ident=>$i:literal),*)=>{$(unsafe extern "C-unwind" fn $name(r:*mut V,s:*mut c_void,o:*mut c_void,n:i32,a:*mut V){unsafe{file($i,r,s,o,n,a)}})*}}
files!(read=>0,write=>1,append=>2,exists=>3,delete=>4);
