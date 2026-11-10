//! Convenience layer over the GameMaker runtime for game systems.
//!
//! - Name strings and variable hashes are created once and cached forever, so
//!   per-frame access doesn't leak a new string each call.
//! - Instance/struct variables are read and written through
//!   `struct_get_from_hash` / `struct_set_from_hash`, which also work on
//!   instances (verified live, including built-ins like `x`).
//!
//! **Main thread only.** Everything here calls into the runtime.

use crate::gm::{KIND_OBJECT, KIND_REF, RValue, Runtime};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

pub use crate::gm::RValue as V;

struct Name {
    s: RValue,
    hash: RValue,
}

static NAMES: LazyLock<Mutex<HashMap<String, Name>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy)]
pub struct G {
    pub rt: &'static Runtime,
}

impl G {
    pub fn new() -> Option<G> {
        crate::RUNTIME.get().map(|rt| G { rt })
    }

    /// Call a built-in; errors become `undefined`.
    pub fn call(&self, f: &str, args: &[RValue]) -> RValue {
        unsafe { self.rt.call(f, args) }.unwrap_or(RValue::undefined())
    }

    /// Cached string value (never freed).
    pub fn s(&self, name: &str) -> RValue {
        let mut m = NAMES.lock().unwrap();
        if let Some(n) = m.get(name) {
            return n.s;
        }
        let s = RValue::string(name);
        let hash = self.call("variable_get_hash", &[s]);
        m.insert(name.to_string(), Name { s, hash });
        s
    }

    fn hash(&self, name: &str) -> RValue {
        self.s(name);
        NAMES.lock().unwrap()[name].hash
    }

    /// `target.name` for an instance or struct.
    pub fn get(&self, target: RValue, name: &str) -> RValue {
        let h = self.hash(name);
        self.call("struct_get_from_hash", &[target, h])
    }

    pub fn set(&self, target: RValue, name: &str, v: RValue) {
        let h = self.hash(name);
        self.call("struct_set_from_hash", &[target, h, v]);
    }

    pub fn num(&self, target: RValue, name: &str) -> f64 {
        self.get(target, name).as_f64().unwrap_or(0.0)
    }

    pub fn str_of(&self, v: &RValue) -> String {
        unsafe { self.rt.to_string(v) }
    }

    pub fn var(&self, name: &str) -> RValue {
        unsafe { self.rt.builtin_var(name) }.unwrap_or(RValue::undefined())
    }

    pub fn global(&self, name: &str) -> RValue {
        let s = self.s(name);
        self.call("variable_global_get", &[s])
    }


    /// Asset (object/sprite/room) by name; cached.
    pub fn asset(&self, name: &str) -> RValue {
        static ASSETS: LazyLock<Mutex<HashMap<String, RValue>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
        if let Some(v) = ASSETS.lock().unwrap().get(name) {
            return *v;
        }
        let s = self.s(name);
        let v = self.call("asset_get_index", &[s]);
        ASSETS.lock().unwrap().insert(name.to_string(), v);
        v
    }

    pub fn exists(&self, inst: RValue) -> bool {
        is_ref(&inst) && truthy(&self.call("instance_exists", &[inst]))
    }

    /// All instances of an object (including children).
    pub fn instances(&self, obj: &str) -> Vec<RValue> {
        let o = self.asset(obj);
        let n = self.call("instance_number", &[o]).as_f64().unwrap_or(0.0) as usize;
        (0..n).map(|i| self.call("instance_find", &[o, real(i as f64)])).collect()
    }

    pub fn first(&self, obj: &str) -> Option<RValue> {
        let o = self.asset(obj);
        let v = self.call("instance_find", &[o, real(0.0)]);
        self.exists(v).then_some(v)
    }

    pub fn room_name(&self) -> String {
        let r = self.var("room");
        self.str_of(&self.call("room_get_name", &[r]))
    }

    /// Name of a sprite, or "" if `spr` isn't a valid sprite.
    ///
    /// Built-ins throw GML errors as C++ exceptions (YYGMLException); outside
    /// GML (our pump) nothing catches them and the game is terminated. So
    /// validate first: sprite_get_name(undefined) killed the game.
    pub fn sprite_name(&self, spr: RValue) -> String {
        let numeric = spr.kind() == KIND_REF || spr.as_f64().is_some();
        if !numeric || spr.as_f64().is_some_and(|v| v < 0.0) {
            return String::new();
        }
        if !truthy(&self.call("sprite_exists", &[spr])) {
            return String::new();
        }
        self.str_of(&self.call("sprite_get_name", &[spr]))
    }

    pub fn arr_len(&self, a: RValue) -> usize {
        self.call("array_length", &[a]).as_f64().unwrap_or(0.0) as usize
    }

    pub fn arr_get(&self, a: RValue, i: usize) -> RValue {
        self.call("array_get", &[a, real(i as f64)])
    }
}

pub fn real(x: f64) -> RValue {
    RValue::real(x)
}

pub fn boolean(b: bool) -> RValue {
    RValue::boolean(b)
}

pub fn truthy(v: &RValue) -> bool {
    v.as_f64().is_some_and(|x| x > 0.5)
}

pub fn is_ref(v: &RValue) -> bool {
    v.kind() == KIND_REF || v.as_f64().is_some_and(|x| x >= 0.0)
}

pub fn is_struct(v: &RValue) -> bool {
    v.kind() == KIND_OBJECT
}

/// YYC asset references encode their index in the low 32 bits. They are not
/// real numbers; `as_f64()` deliberately does not coerce reference values.
/// Use only for known assets, never for an instance reference or arbitrary data.
pub fn asset_id(v: V) -> i32 {
    if v.kind() == KIND_REF { v.v as u32 as i32 }
    else { v.as_f64().unwrap_or(-1.0) as i32 }
}



/// Stable key for an instance/asset ref (for maps).
pub fn key(v: &RValue) -> u64 {
    v.v
}
