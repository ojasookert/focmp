//! GameMaker (YYC, runtime 2026.0.0.23, x64) runtime access.
//!
//! - Built-in functions are found by scanning `.text` for calls to the runtime's
//!   registration function: `lea rdx,[fn]; lea rcx,[name]; lea r8d,[argc]; call Function_Add`.
//!   The registration function is found from the call site that registers "instance_number".
//! - Compiled GML scripts/events are listed in a static table in `.data`:
//!   `{ const char* name; fn ptr; YYVAR* var }`, 24 bytes per entry.
//!
//! Built-in ABI (checked by disassembling `typeof` and `instance_number`):
//!   void F(RValue* result, CInstance* self, CInstance* other, int argc, RValue* args)
//! self/other may be null for functions that don't use the calling instance.

use crate::pe::{Image, cstr_in};
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_void};

pub const KIND_REAL: u32 = 0;
pub const KIND_STRING: u32 = 1;
pub const KIND_ARRAY: u32 = 2;
pub const KIND_PTR: u32 = 3;
pub const KIND_UNDEFINED: u32 = 5;
pub const KIND_OBJECT: u32 = 6;
pub const KIND_INT32: u32 = 7;
pub const KIND_INT64: u32 = 10;
pub const KIND_BOOL: u32 = 13;
pub const KIND_REF: u32 = 15;
pub const KIND_MASK: u32 = 0x00ff_ffff;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RValue {
    pub v: u64,
    pub flags: u32,
    pub kind: u32,
}

/// Layout of the runtime's `_RefThing<const char*>`.
#[repr(C)]
struct RefString {
    s: *const u8,
    refcount: i32,
    size: i32,
}

impl RValue {
    pub fn real(x: f64) -> Self {
        RValue { v: x.to_bits(), flags: 0, kind: KIND_REAL }
    }
    pub fn undefined() -> Self {
        RValue { v: 0, flags: 0, kind: KIND_UNDEFINED }
    }
    pub fn boolean(b: bool) -> Self {
        RValue { v: (b as u64 as f64).to_bits(), flags: 0, kind: KIND_BOOL }
    }
    /// A string value. The memory is leaked on purpose and given a huge
    /// refcount, so the runtime never tries to free it. Fine for debugging.
    pub fn string(s: &str) -> Self {
        let c = CString::new(s.replace('\0', "")).unwrap();
        let size = c.as_bytes().len() as i32;
        let ptr = c.into_raw() as *const u8;
        let rs = Box::into_raw(Box::new(RefString { s: ptr, refcount: 1 << 28, size }));
        RValue { v: rs as u64, flags: 0, kind: KIND_STRING }
    }
    pub fn kind(&self) -> u32 {
        self.kind & KIND_MASK
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self.kind() {
            KIND_REAL | KIND_BOOL => Some(f64::from_bits(self.v)),
            KIND_INT32 => Some(self.v as u32 as i32 as f64),
            KIND_INT64 => Some(self.v as i64 as f64),
            _ => None,
        }
    }
    /// Read a string value directly (no runtime call).
    pub fn as_str(&self) -> Option<String> {
        if self.kind() != KIND_STRING || self.v == 0 {
            return None;
        }
        unsafe {
            let rs = &*(self.v as *const RefString);
            if rs.s.is_null() {
                return Some(String::new());
            }
            Some(CStr::from_ptr(rs.s as *const i8).to_string_lossy().into_owned())
        }
    }
    pub fn kind_name(&self) -> &'static str {
        match self.kind() {
            KIND_REAL => "real",
            KIND_STRING => "string",
            KIND_ARRAY => "array",
            KIND_PTR => "ptr",
            KIND_UNDEFINED => "undefined",
            KIND_OBJECT => "struct",
            KIND_INT32 => "int32",
            KIND_INT64 => "int64",
            KIND_BOOL => "bool",
            KIND_REF => "ref",
            _ => "?",
        }
    }
}

/// All `call <register>` sites: name from `lea rcx,[rip+x]`, function from
/// `lea rdx,[rip+y]`, both within the 40 bytes before the call.
fn registrations(img: &Image, register: usize) -> BTreeMap<String, usize> {
    let code = img.text.bytes();
    let mut out = BTreeMap::new();
    for i in 40..code.len().saturating_sub(5) {
        if code[i] != 0xe8 || img.call_target(img.text.start + i) != Some(register) {
            continue;
        }
        let (mut rcx, mut rdx) = (None, None);
        for back in (7..=40).rev().map(|b| i - b) {
            let w = &code[back..back + 7];
            if w[0] == 0x48 && w[1] == 0x8d && (w[2] == 0x0d || w[2] == 0x15) {
                let disp = i32::from_le_bytes(w[3..7].try_into().unwrap()) as isize;
                let tgt = (img.text.start + back + 7).wrapping_add_signed(disp);
                if w[2] == 0x0d { rcx = Some(tgt) } else { rdx = Some(tgt) }
            }
        }
        if let (Some(n), Some(f)) = (rcx, rdx) {
            if let Some(s) = cstr_in(&img.rdata, n) {
                if img.text.contains(f) {
                    out.insert(s.to_string(), f);
                }
            }
        }
    }
    out
}

pub type BuiltinFn =
    unsafe extern "C-unwind" fn(*mut RValue, *mut c_void, *mut c_void, i32, *mut RValue);

/// Built-in variable getter: `bool get(CInstance* self, int array_index, RValue* out)`.
pub type VarGetFn = unsafe extern "C-unwind" fn(*mut c_void, i32, *mut RValue) -> bool;

pub struct Runtime {
    pub builtins: BTreeMap<String, usize>,
    /// Built-in variables (room, fps, mouse_x, ...) → getter address.
    pub builtin_vars: BTreeMap<String, usize>,
    /// (name, function address) for every compiled GML script and object event.
    pub scripts: Vec<(&'static str, usize)>,
    /// Stable native-code identity, independent of ASLR. Shot method/asset
    /// references must never cross between different game builds.
    pub game_build: u64,
    pub function_add: usize,
    pub script_table: usize,
}

// Only addresses inside the game image, which never moves while the game runs.
unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}

impl Runtime {
    /// Verified anonymous-method renames between v1.0.01 hotfix and v1.0.02.
    /// Never guess by method order or strip numeric suffixes: siblings share an ABI.
    pub fn script(&self, name: &str) -> Option<usize> {
        let renamed = match name {
            "gml_Script_anon@6600@gml_Object_objMusic_Create_0" => "gml_Script_anon@6687@gml_Object_objMusic_Create_0",
            "gml_Script_anon@9629@gml_Object_objPlayer_Other_11" => "gml_Script_anon@9819@gml_Object_objPlayer_Other_11",
            "gml_Script_anon@1721@gml_Object_objGun_Create_0" => "gml_Script_anon@1740@gml_Object_objGun_Create_0",
            "gml_Script_anon@22921@___struct___601@gml_Object_objEnemyMASTER_Create_0" => "gml_Script_anon@22972@___struct___601@gml_Object_objEnemyMASTER_Create_0",
            "gml_Script_anon@21803@___struct___600@gml_Object_objEnemyMASTER_Create_0" => "gml_Script_anon@21854@___struct___600@gml_Object_objEnemyMASTER_Create_0",
            "gml_Script_anon@22737@___struct___600@gml_Object_objEnemyMASTER_Create_0" => "gml_Script_anon@22788@___struct___600@gml_Object_objEnemyMASTER_Create_0",
            _ => name,
        };
        self.scripts.iter().find(|(n, _)| *n == name)
            .or_else(|| self.scripts.iter().find(|(n, _)| *n == renamed))
            .map(|(_, address)| *address)
    }

    pub fn scan(img: &Image) -> Result<Runtime, String> {
        // 1) Find Function_Add from the "instance_number" registration.
        let name = img.find_cstr("instance_number").ok_or("string instance_number not found")?;
        let refs = img.lea_refs(0x0d, name);
        let mut function_add = None;
        'outer: for r in refs {
            for k in 7..40 {
                if let Some(t) = img.call_target(r + k) {
                    if img.text.contains(t) {
                        function_add = Some(t);
                        break 'outer;
                    }
                }
            }
        }
        let function_add = function_add.ok_or("Function_Add not found")?;

        // 2) Every call to Function_Add: look back for lea rcx (name) and lea rdx (fn).
        let builtins = registrations(img, function_add);
        if builtins.len() < 1000 {
            return Err(format!("only {} builtins found", builtins.len()));
        }

        // 2b) Built-in variables use the same shape with a different registration
        //     function (`lea rdx,[getter]; lea r8,[setter]; lea rcx,[name]; call`).
        //     Found from the "room_speed" registration.
        let mut builtin_vars = BTreeMap::new();
        if let Some(name) = img.find_cstr("room_speed") {
            let mut add_var = None;
            'o: for r in img.lea_refs(0x0d, name) {
                for k in 7..24 {
                    if let Some(t) = img.call_target(r + k) {
                        if img.text.contains(t) {
                            add_var = Some(t);
                            break 'o;
                        }
                    }
                }
            }
            if let Some(av) = add_var {
                builtin_vars = registrations(img, av);
            }
        }

        // 3) Script table: find a run of valid {name, fn, var} entries in .data.
        let valid = |e: usize| -> Option<(&'static str, usize)> {
            if e < img.data.start || e + 24 > img.data.end {
                return None;
            }
            let (n, f) = unsafe { (*(e as *const usize), *((e + 8) as *const usize)) };
            if !img.text.contains(f) {
                return None;
            }
            let s = cstr_in(&img.rdata, n).or_else(|| cstr_in(&img.data, n))?;
            s.starts_with("gml_").then_some((s, f))
        };
        let mut found = None;
        let mut e = img.data.start;
        while e + 24 * 4 < img.data.end {
            if valid(e).is_some() && valid(e + 24).is_some() && valid(e + 48).is_some() && valid(e + 72).is_some() {
                found = Some(e);
                break;
            }
            e += 8;
        }
        let mut start = found.ok_or("script table not found")?;
        while valid(start - 24).is_some() {
            start -= 24;
        }
        let mut scripts = Vec::new();
        let mut e = start;
        while let Some(entry) = valid(e) {
            scripts.push(entry);
            e += 24;
        }

        let mut game_build = 0xcbf29ce484222325u64;
        for (name, address) in &scripts {
            for byte in name.bytes().chain([0]).chain((*address - img.base).to_le_bytes()) {
                game_build = (game_build ^ byte as u64).wrapping_mul(0x100000001b3);
            }
        }
        Ok(Runtime { builtins, builtin_vars, scripts, game_build, function_add, script_table: start })
    }

    /// Read a built-in variable of a specific instance (raw `CInstance*`), e.g.
    /// `id` or `object_index` of the `self`/`other` passed to a detoured GML
    /// function. **Main thread only.**
    pub unsafe fn builtin_var_of(&self, name: &str, inst: *mut c_void) -> Option<RValue> {
        if inst.is_null() {
            return None;
        }
        let &a = self.builtin_vars.get(name)?;
        let get: VarGetFn = unsafe { std::mem::transmute(a) };
        let mut out = RValue::undefined();
        unsafe { get(inst, i32::MIN, &mut out) }.then_some(out)
    }

    /// Read a built-in variable such as `room` or `mouse_x`. **Main thread only.**
    pub unsafe fn builtin_var(&self, name: &str) -> Result<RValue, String> {
        let &a = self.builtin_vars.get(name).ok_or_else(|| format!("unknown builtin variable '{name}'"))?;
        let get: VarGetFn = unsafe { std::mem::transmute(a) };
        let mut out = RValue::undefined();
        // Array index: GameMaker passes ARRAY_INDEX_NO_INDEX (i32::MIN) for scalars.
        unsafe { get(std::ptr::null_mut(), i32::MIN, &mut out) };
        Ok(out)
    }

    pub fn builtin(&self, name: &str) -> Option<BuiltinFn> {
        self.builtins.get(name).map(|&a| unsafe { std::mem::transmute::<usize, BuiltinFn>(a) })
    }

    /// Call a built-in by name. **Main thread only.**
    pub unsafe fn call(&self, name: &str, args: &[RValue]) -> Result<RValue, String> {
        let f = self.builtin(name).ok_or_else(|| format!("unknown builtin '{name}'"))?;
        let mut result = RValue::undefined();
        let mut args = args.to_vec();
        let argp = if args.is_empty() { std::ptr::null_mut() } else { args.as_mut_ptr() };
        unsafe {
            f(&mut result, std::ptr::null_mut(), std::ptr::null_mut(), args.len() as i32, argp);
        }
        Ok(result)
    }

    /// Text form of any value, using the game's own `string()`.
    pub unsafe fn to_string(&self, v: &RValue) -> String {
        if let Some(s) = v.as_str() {
            return s;
        }
        match unsafe { self.call("string", &[*v]) } {
            Ok(r) => r.as_str().unwrap_or_else(|| format!("<{}>", v.kind_name())),
            Err(_) => format!("<{} 0x{:x}>", v.kind_name(), v.v),
        }
    }
}
