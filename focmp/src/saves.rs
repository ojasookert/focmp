//! Isolate modded saves before the first native load. Never import an active run.
use crate::{
    engine::{G, V},
    hooks::ScriptFn,
};
use std::{
    ffi::c_void,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
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
