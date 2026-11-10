//! Detours on compiled GML functions (from the script table).
//!
//! Signatures in this YYC build:
//!   events  (`gml_Object_<obj>_<event>`): `void(CInstance* self, CInstance* other)`
//!   scripts/methods (`gml_Script_*`):     `RValue*(CInstance* self, CInstance* other,
//!                                                   RValue* ret, int argc, RValue** args)`

use crate::gm::{RValue, Runtime};
use retour::RawDetour;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex};

pub type ScriptFn =
    unsafe extern "C-unwind" fn(*mut c_void, *mut c_void, *mut RValue, i32, *mut *mut RValue) -> *mut RValue;

struct Hook {
    detour: RawDetour,
}
// The detour only holds code addresses.
unsafe impl Send for Hook {}

static HOOKS: LazyLock<Mutex<HashMap<String, Hook>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Install a detour on the GML function `name`. Returns the trampoline that
/// calls the original. Idempotent: a second call returns the existing trampoline.
pub fn install(rt: &Runtime, name: &str, hook: *const ()) -> Result<*const (), String> {
    let mut hooks = HOOKS.lock().unwrap();
    if let Some(h) = hooks.get(name) {
        return Ok(h.detour.trampoline() as *const ());
    }
    let addr = rt.scripts.iter().find(|(n, _)| *n == name).map(|(_, a)| *a)
        .or_else(|| rt.builtins.get(name).copied())
        .ok_or_else(|| format!("runtime function {name} not found"))?;
    let detour = unsafe { RawDetour::new(addr as *const (), hook) }.map_err(|e| format!("detour {name}: {e}"))?;
    unsafe { detour.enable() }.map_err(|e| format!("enable {name}: {e}"))?;
    let tramp = detour.trampoline() as *const ();
    hooks.insert(name.to_string(), Hook { detour });
    crate::log(&format!("hooked {name} at 0x{addr:x}"));
    Ok(tramp)
}

/// Hook a verified YYC runtime entry that has no builtin-table entry.
pub fn install_address(name:&str, addr:usize, hook:*const ())->Result<*const (),String>{
    let mut hooks=HOOKS.lock().unwrap();
    if let Some(h)=hooks.get(name){return Ok(h.detour.trampoline() as *const ());}
    let detour=unsafe{RawDetour::new(addr as *const (),hook)}.map_err(|e|format!("detour {name}: {e}"))?;
    unsafe{detour.enable()}.map_err(|e|format!("enable {name}: {e}"))?;
    let original=detour.trampoline() as *const ();hooks.insert(name.into(),Hook{detour});
    crate::log(&format!("hooked {name} at 0x{addr:x}"));Ok(original)
}

pub fn list() -> Vec<String> {
    let mut v: Vec<String> = HOOKS.lock().unwrap().keys().cloned().collect();
    v.sort();
    v
}
