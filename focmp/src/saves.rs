//! Isolate modded saves before the first native load. Never import an active run.
use crate::{
    engine::{G, V},
    hooks::ScriptFn,
};
use std::{
    ffi::c_void,
    path::PathBuf,
    sync::{Mutex, atomic::{AtomicUsize, Ordering}},
};
// Keep the fork active through disconnects and returns to the menu. A host
// continuing offline must never autosave over the source file.
static RUN_FILE: Mutex<Option<String>> = Mutex::new(None);
static LOAD_WORLD: Mutex<Option<World>> = Mutex::new(None);
static LOAD_FILE: AtomicUsize = AtomicUsize::new(0);
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
    path: String,
    pub world: World,
}

pub fn clear_run_file() { *RUN_FILE.lock().unwrap() = None; }

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

fn create_working_copy(g: G, data: &[u8]) -> Result<String, String> {
    use std::io::Write;
    if PATH.load(Ordering::Relaxed) == 0 { return Err("Save isolation hook is unavailable".into()); }
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let relative = format!("{}/coop_runs/{nonce:x}/savefile.json", save_base(g)?);
    let path = root().ok_or("save directory unavailable")?.join(&relative);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| e.to_string())?;
    file.write_all(data).map_err(|e| e.to_string())?;
    Ok(relative)
}

/// Fresh co-op runs also use their own destination, so turning the checkbox
/// off cannot subsequently overwrite the saved characters used by the lobby.
pub fn prepare_fresh(g: G) -> Result<(), String> {
    if g.room_name() != "Room_main_menu" { return Err("Start from the main menu".into()); }
    let path = create_working_copy(g, &[])?;
    *RUN_FILE.lock().unwrap() = Some(path);
    Ok(())
}

/// Copy first, then let the native loader validate the copy. Even its legacy
/// save migration cannot write to the source. Prefer the modded save, falling
/// back to the original only when there is no modded save.
pub fn prepare_local(g: G) -> Result<Prepared, String> {
    use std::io::Read;
    if g.room_name() != "Room_main_menu" { return Err("Start from the main menu".into()); }
    if PATH.load(Ordering::Relaxed) == 0 || LOAD_FILE.load(Ordering::Relaxed) == 0 {
        return Err("Save isolation hooks are unavailable".into());
    }
    let base = save_base(g)?;
    let root = root().ok_or("save directory unavailable")?;
    let modded = root.join(&base).join("savefile.json");
    let source = if modded.exists() { modded } else { root.join(base.trim_end_matches("_focmp")).join("savefile.json") };
    let file = std::fs::File::open(source).map_err(|_| "No saved run found".to_string())?;
    let mut data = Vec::new();
    file.take((MAX_RUN_SAVE + 1) as u64).read_to_end(&mut data).map_err(|e| e.to_string())?;
    if data.is_empty() || data.len() > MAX_RUN_SAVE { return Err("Save is empty or too large".into()); }
    let relative = create_working_copy(g, &data)?;
    let save = g.first("objSaveGame").ok_or("save system is not ready")?;
    let previous = RUN_FILE.lock().unwrap().replace(relative.clone());
    let valid = crate::engine::truthy(&g.call("method_call", &[g.get(save, "valid_save_file_exist")]));
    *RUN_FILE.lock().unwrap() = previous;
    if !valid { return Err("Save is invalid or the run has ended".into()); }
    let data = method(g, save, "load_file_hmac", &[g.s(&relative)]);
    if !crate::engine::is_struct(&data) { return Err("Cannot read saved run".into()); }
    g.set(save, "focmp_save_probe", data);
    let result = (|| {
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
        Ok(Prepared { path: relative.clone(), world })
    })();
    g.set(save, "focmp_save_probe", V::undefined());
    if result.is_ok() { crate::log(&format!("saves: prepared own co-op copy {relative}")); }
    result
}

pub fn load_run(g: G, local: &Prepared, world: &World) {
    *RUN_FILE.lock().unwrap() = Some(local.path.clone());
    *LOAD_WORLD.lock().unwrap() = Some(world.clone());
    if let Some(save) = g.first("objSaveGame") {
        g.call("method_call", &[g.get(save, "load")]);
    }
    *LOAD_WORLD.lock().unwrap() = None;
}

unsafe extern "C-unwind" fn load_file(s: *mut c_void, o: *mut c_void, r: *mut V, n: i32, a: *mut *mut V) -> *mut V {
    let original: ScriptFn = unsafe { std::mem::transmute(LOAD_FILE.load(Ordering::Relaxed)) };
    unsafe { original(s, o, r, n, a) };
    if let Some(g) = G::new() {
        let world = LOAD_WORLD.lock().unwrap().clone();
        let path = RUN_FILE.lock().unwrap().clone();
        if n > 0 && !a.is_null() && !r.is_null() && crate::engine::is_struct(&unsafe { *r })
            && path.as_deref() == Some(g.str_of(&unsafe { **a }).as_str()) {
            if let Some(world) = world {
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
            if let Some(path) = RUN_FILE.lock().unwrap().as_ref() {
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
