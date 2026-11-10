use crate::{engine::{G, V}, hooks::{self, ScriptFn}};
use std::{ffi::c_void, sync::atomic::{AtomicUsize, Ordering}};

pub const BUILD: &str = env!("FOCMP_BUILD_VERSION");
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

pub fn install() {
    let Some(rt) = crate::RUNTIME.get() else { return };
    match hooks::install(rt, "gml_Script_get_version_text", version_text as *const ()) {
        Ok(p) => ORIGINAL.store(p as usize, Ordering::SeqCst),
        Err(e) => crate::log(&format!("version display: {e}")),
    }
}

unsafe extern "C-unwind" fn version_text(s: *mut c_void, o: *mut c_void, ret: *mut V, argc: i32, args: *mut *mut V) -> *mut V {
    let original: ScriptFn = unsafe { std::mem::transmute(ORIGINAL.load(Ordering::Relaxed)) };
    let result = unsafe { original(s, o, ret, argc, args) };
    if !result.is_null() {
        if let Some(g) = G::new() {
            let text = g.str_of(unsafe { &*result });
            unsafe { *result = g.s(&format!("{text} [c_purple]FOCMP {BUILD}[/c]")) };
        }
    }
    result
}
