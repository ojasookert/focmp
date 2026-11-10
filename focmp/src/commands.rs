//! Debug console commands.
//!
//! Values returned by `call`, `inst`, etc. are kept in a history and can be
//! passed back as arguments: `$1`, `$2`, ... (`$` alone = last value).

use crate::gm::{RValue, Runtime};
use crate::{IMAGE_BASE, MAIN_TID, PUMPS, RUNTIME, on_main};
use std::fmt::Write;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;

static HISTORY: Mutex<Vec<RValue>> = Mutex::new(Vec::new());
const TIMEOUT: Duration = Duration::from_secs(5);

const HELP: &str = "\
commands (values from earlier results can be passed as $N, or $ for the last one):
  ping                         check the console is alive (no game thread needed)
  info                         image base, scan results, main-thread pump status
  builtins [filter]            list GML built-in functions found in the exe
  scripts [filter]             list compiled GML scripts/object events (name + address)
  call <builtin> [args...]     call a built-in on the main thread, e.g. call instance_number 0
                               args: 12  -5  1.5  \"text\"  true  false  undefined  $N
  objects [all]                object indices, names and live instance counts (default: count > 0)
  inst <object|index> [max]    list instances of an object with their variables (default max 5)
  vars <$N|id>                 all variables of one instance (full values)
  get <$N|id> <var>            variable_instance_get
  set <$N|id> <var> <value>    variable_instance_set
  global                       global variables
  var <name>                   read a built-in variable (room, fps, mouse_x, current_time, ...)
  hist                         value history
navigation:
  screen                       current room, menu page, GUI/window size, mouse mode
  menu                         items of the open menu page (index, text, GUI rect, selection)
  menu pick <index|text>       click a menu item (text = case-insensitive substring)
  key <key> [hold_ms]          press+release a key (up down left right enter esc space tab a-z 0-9 f1-f12 or VK number)
  keys <key> <key> ...         several keys in sequence
  mouse <gx> <gy>              move the injected cursor (GUI coordinates)
  mouse off                    give the mouse back to the real device
  click <gx> <gy> [right]      click at GUI coordinates
  wait <ms>                    pause (for scripted sequences)
  focus force|normal           make window_has_focus() always true (drive a background instance)
  window <w> <h> [x y]         windowed mode with size/position; 'window full' for fullscreen
co-op:
  coop [status]                session status and players
  coop host [port] | coop join [addr[:port]] | coop leave
  coop ai                      enemy → target player assignments and aggro
  trace damage                 toggle logging of every enemy set_damage call
  quit                         close the connection";

pub fn run(line: &str) -> String {
    let toks = match tokenize(line) {
        Ok(t) => t,
        Err(e) => return format!("error: {e}"),
    };
    let (cmd, args) = (toks[0].as_str(), &toks[1..]);
    match cmd {
        "help" | "?" => HELP.into(),
        "ping" => "pong".into(),
        "info" => info(),
        "builtins" => list_builtins(args.first().map(String::as_str)),
        "scripts" => list_scripts(args.first().map(String::as_str)),
        "hist" => {
            let h = HISTORY.lock().unwrap().clone();
            main(move |rt| {
                let mut s = String::new();
                for (i, v) in h.iter().enumerate() {
                    let _ = writeln!(s, "${} = {}", i + 1, show(rt, v, 200));
                }
                if s.is_empty() { "(empty)".into() } else { s }
            })
        }
        "call" => {
            if args.is_empty() {
                return "usage: call <builtin> [args...]".into();
            }
            let name = args[0].clone();
            let rest = args[1..].to_vec();
            main(move |rt| {
                let vals = match parse_args(&rest) {
                    Ok(v) => v,
                    Err(e) => return format!("error: {e}"),
                };
                match unsafe { rt.call(&name, &vals) } {
                    Ok(r) => {
                        let n = remember(r);
                        format!("${n} = {}", show(rt, &r, 4000))
                    }
                    Err(e) => format!("error: {e}"),
                }
            })
        }
        "objects" => {
            let all = args.first().is_some_and(|a| a == "all");
            main(move |rt| objects(rt, all))
        }
        "inst" => {
            if args.is_empty() {
                return "usage: inst <object name|index> [max]".into();
            }
            let obj = args[0].clone();
            let max: usize = args.get(1).and_then(|m| m.parse().ok()).unwrap_or(5);
            main(move |rt| instances(rt, &obj, max))
        }
        "vars" => {
            let Some(a) = args.first().cloned() else { return "usage: vars <$N|id>".into() };
            main(move |rt| match parse_arg(&a) {
                Ok(id) => dump_vars(rt, &id, usize::MAX),
                Err(e) => format!("error: {e}"),
            })
        }
        "get" => {
            if args.len() != 2 {
                return "usage: get <$N|id> <var>".into();
            }
            let (a, var) = (args[0].clone(), args[1].clone());
            main(move |rt| {
                let id = match parse_arg(&a) {
                    Ok(v) => v,
                    Err(e) => return format!("error: {e}"),
                };
                let r = unsafe { rt.call("variable_instance_get", &[id, RValue::string(&var)]) }.unwrap();
                let n = remember(r);
                format!("${n} = {}", show(rt, &r, 4000))
            })
        }
        "set" => {
            if args.len() != 3 {
                return "usage: set <$N|id> <var> <value>".into();
            }
            let a = args.to_vec();
            main(move |rt| {
                let (id, val) = match (parse_arg(&a[0]), parse_arg(&a[2])) {
                    (Ok(i), Ok(v)) => (i, v),
                    (Err(e), _) | (_, Err(e)) => return format!("error: {e}"),
                };
                unsafe { rt.call("variable_instance_set", &[id, RValue::string(&a[1]), val]) }.unwrap();
                let now = unsafe { rt.call("variable_instance_get", &[id, RValue::string(&a[1])]) }.unwrap();
                format!("{} = {}", a[1], show(rt, &now, 400))
            })
        }
        "global" => main(|rt| dump_vars(rt, &RValue::real(-5.0), 300)),
        "var" => {
            let Some(name) = args.first().cloned() else { return "usage: var <name>".into() };
            main(move |rt| match unsafe { rt.builtin_var(&name) } {
                Ok(v) => {
                    let n = remember(v);
                    format!("${n} = {}", show(rt, &v, 4000))
                }
                Err(e) => format!("error: {e}"),
            })
        }
        "coop" => {
            let a = args.to_vec();
            crate::on_main(TIMEOUT, move || crate::coop::console(&a)).unwrap_or_else(|e| format!("error: {e}"))
        }
        "trace" => match args.first().map(String::as_str) {
            Some("damage") => {
                let on = !crate::coop::ai::TRACE_DAMAGE.fetch_xor(true, Ordering::Relaxed);
                format!("damage trace {} (lines go to focmp.log)", if on { "on" } else { "off" })
            }
            _ => "usage: trace damage   (toggle)".into(),
        },
        _ => match crate::ui::run(cmd, args) {
            Some(r) => r,
            None => format!("unknown command '{cmd}', try 'help'"),
        },
    }
}

/// Run on the main thread with the runtime; formats errors as text.
pub(crate) fn main(f: impl FnOnce(&'static Runtime) -> String + Send + 'static) -> String {
    let Some(rt) = RUNTIME.get() else { return "error: runtime scan failed, see focmp.log".into() };
    on_main(TIMEOUT, move || f(rt)).unwrap_or_else(|e| format!("error: {e}"))
}

pub(crate) fn remember(v: RValue) -> usize {
    // Rust's history is invisible to GameMaker's collector. Root returned
    // structs/arrays/strings before another frame can collect a console-created
    // value (particularly cloned weapons and bound methods used by live checks).
    if matches!(v.kind(),crate::gm::KIND_OBJECT|crate::gm::KIND_ARRAY|crate::gm::KIND_STRING) {
        if let Some(g)=crate::engine::G::new(){
            let mut roots=g.global("focmp_console_roots");
            if roots.kind()!=crate::gm::KIND_ARRAY {
                roots=g.call("array_create",&[RValue::real(0.0)]);
                g.call("variable_global_set",&[g.s("focmp_console_roots"),roots]);
            }
            g.call("array_push",&[roots,v]);
        }
    }
    let mut h = HISTORY.lock().unwrap();
    h.push(v);
    h.len()
}

pub(crate) fn show(rt: &Runtime, v: &RValue, max: usize) -> String {
    let mut s = unsafe { rt.to_string(v) };
    if s.chars().count() > max {
        s = s.chars().take(max).collect::<String>() + "…";
    }
    format!("{s}  ({})", v.kind_name())
}

fn info() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "image base:      0x{:x}", IMAGE_BASE.load(Ordering::Relaxed));
    match RUNTIME.get() {
        Some(rt) => {
            let _ = writeln!(s, "Function_Add:    0x{:x}", rt.function_add);
            let _ = writeln!(s, "builtins:        {}", rt.builtins.len());
            let _ = writeln!(s, "scripts/events:  {} (table 0x{:x})", rt.scripts.len(), rt.script_table);
        }
        None => s.push_str("runtime:         scan FAILED\n"),
    }
    let _ = writeln!(s, "main thread id:  {}", MAIN_TID.load(Ordering::Relaxed));
    let _ = write!(s, "pump calls:      {}", PUMPS.load(Ordering::Relaxed));
    s
}

fn list_builtins(filter: Option<&str>) -> String {
    let Some(rt) = RUNTIME.get() else { return "error: runtime scan failed".into() };
    let mut s = String::new();
    let mut n = 0;
    for (name, addr) in &rt.builtins {
        if filter.is_none_or(|f| name.contains(f)) {
            let _ = writeln!(s, "0x{addr:x}  {name}");
            n += 1;
        }
    }
    let _ = write!(s, "{n} builtins");
    s
}

fn list_scripts(filter: Option<&str>) -> String {
    let Some(rt) = RUNTIME.get() else { return "error: runtime scan failed".into() };
    let mut s = String::new();
    let mut n = 0;
    for (name, addr) in &rt.scripts {
        if filter.is_none_or(|f| name.contains(f)) {
            n += 1;
            if n <= 500 {
                let _ = writeln!(s, "0x{addr:x}  {name}");
            }
        }
    }
    let _ = write!(s, "{n} matches{}", if n > 500 { " (first 500 shown)" } else { "" });
    s
}

pub(crate) fn real(rt: &Runtime, f: &str, args: &[RValue]) -> f64 {
    unsafe { rt.call(f, args) }.ok().and_then(|r| r.as_f64()).unwrap_or(f64::NAN)
}

fn objects(rt: &Runtime, all: bool) -> String {
    let mut s = String::new();
    let mut total = 0.0;
    let mut i = 0;
    while real(rt, "object_exists", &[RValue::real(i as f64)]) > 0.5 {
        let idx = RValue::real(i as f64);
        let name = unsafe { rt.to_string(&rt.call("object_get_name", &[idx]).unwrap()) };
        let count = real(rt, "instance_number", &[idx]);
        total += count;
        if all || count > 0.0 {
            let _ = writeln!(s, "{i:4}  {name:<36} {count}");
        }
        i += 1;
    }
    let _ = write!(s, "{i} objects, {total} instances (counts include parents' children)");
    s
}

fn instances(rt: &Runtime, obj: &str, max: usize) -> String {
    let obj_val = match obj.parse::<f64>() {
        Ok(n) => RValue::real(n),
        Err(_) => unsafe { rt.call("asset_get_index", &[RValue::string(obj)]) }.unwrap(),
    };
    if obj_val.as_f64().is_some_and(|v| v < 0.0) {
        return format!("no asset named '{obj}'");
    }
    let n = real(rt, "instance_number", &[obj_val]);
    let mut s = format!("{obj}: {n} instance(s)\n");
    for k in 0..(n as usize).min(max) {
        let id = unsafe { rt.call("instance_find", &[obj_val, RValue::real(k as f64)]) }.unwrap();
        let slot = remember(id);
        let _ = writeln!(s, "--- [{k}] ${slot} = {}", show(rt, &id, 100));
        s.push_str(&dump_vars(rt, &id, 120));
    }
    if (n as usize) > max {
        let _ = writeln!(s, "... {} more (use: inst {obj} <max>)", n as usize - max);
    }
    s
}

const BUILTIN_VARS: &[&str] = &[
    "object_index", "x", "y", "direction", "speed", "sprite_index", "image_index", "depth", "visible",
];

fn dump_vars(rt: &Runtime, id: &RValue, max: usize) -> String {
    let mut s = String::new();
    let is_global = id.as_f64() == Some(-5.0);
    if !is_global {
        for v in BUILTIN_VARS {
            let r = unsafe { rt.call("variable_instance_get", &[*id, RValue::string(v)]) }.unwrap();
            let mut val = show(rt, &r, max);
            if *v == "object_index" {
                if let Ok(n) = unsafe { rt.call("object_get_name", &[r]) } {
                    val = format!("{} ({})", unsafe { rt.to_string(&n) }, val);
                }
            }
            let _ = writeln!(s, "  {v:<24} {val}");
        }
    }
    let names = unsafe { rt.call("variable_instance_get_names", &[*id]) }.unwrap();
    let len = real(rt, "array_length", &[names]);
    if len.is_nan() {
        let _ = writeln!(s, "  (variable_instance_get_names returned {})", show(rt, &names, 80));
        return s;
    }
    let mut vars: Vec<(String, String)> = (0..len as usize)
        .map(|k| {
            let n = unsafe { rt.call("array_get", &[names, RValue::real(k as f64)]) }.unwrap();
            let name = unsafe { rt.to_string(&n) };
            let val = unsafe { rt.call("variable_instance_get", &[*id, n]) }.unwrap();
            (name, show(rt, &val, max))
        })
        .collect();
    vars.sort();
    for (n, v) in vars {
        let _ = writeln!(s, "  {n:<24} {v}");
    }
    let _ = writeln!(s, "  ({len} user variables)");
    s
}

fn parse_args(toks: &[String]) -> Result<Vec<RValue>, String> {
    toks.iter().map(|t| parse_arg(t)).collect()
}

fn parse_arg(t: &str) -> Result<RValue, String> {
    if let Some(rest) = t.strip_prefix('\u{1}') {
        return Ok(RValue::string(rest)); // quoted string from tokenize()
    }
    match t {
        "true" => return Ok(RValue::boolean(true)),
        "false" => return Ok(RValue::boolean(false)),
        "undefined" => return Ok(RValue::undefined()),
        _ => {}
    }
    if let Some(n) = t.strip_prefix('$') {
        let h = HISTORY.lock().unwrap();
        let idx = if n.is_empty() { h.len() } else { n.parse().map_err(|_| format!("bad ref {t}"))? };
        return h.get(idx.wrapping_sub(1)).copied().ok_or_else(|| format!("no value {t}"));
    }
    if let Ok(x) = t.parse::<f64>() {
        return Ok(RValue::real(x));
    }
    Ok(RValue::string(t)) // bare word -> string
}

/// Split on spaces, honoring "double quotes". Quoted tokens get a \x01 marker
/// so `"12"` stays a string.
fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut s = String::from('\u{1}');
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some('\\') => s.extend(chars.next()),
                    Some(ch) => s.push(ch),
                    None => return Err("unterminated string".into()),
                }
            }
            out.push(s);
        } else {
            let mut s = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                s.push(ch);
                chars.next();
            }
            out.push(s);
        }
    }
    if out.is_empty() { Err("empty command".into()) } else { Ok(out) }
}
