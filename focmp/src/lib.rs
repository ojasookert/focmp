#![cfg(windows)]
//! focmp.dll: in-game half of the Flashes of Chaos multiplayer mod.
//!
//! Current scope (spike): prove we're loaded, find the GameMaker runtime, and,
//! when `[debug] console = true` in focmp.ini, expose a line-based debug console
//! on 127.0.0.1:47811 (FOCMP_DEBUG_PORT picks the port) for inspecting game
//! objects. Connect with `nc 127.0.0.1 47811` or `tools/focmp_console.py`.
//!
//! Threading: GameMaker is single-threaded. Every command that touches the
//! runtime is queued and runs inside our `PeekMessageW` import hook, which the
//! runner calls from its main loop between frames.

mod commands;
mod config;
mod saves;
mod version;
mod coop;
mod engine;
mod gm;
mod hooks;
mod input;
mod net;
mod steam;
mod pe;
mod ui;
mod win;

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub(crate) static RUNTIME: OnceLock<gm::Runtime> = OnceLock::new();
pub(crate) static IMAGE_BASE: AtomicUsize = AtomicUsize::new(0);
static ORIG_PEEK: AtomicUsize = AtomicUsize::new(0);
pub(crate) static PUMPS: AtomicU64 = AtomicU64::new(0);
pub(crate) static MAIN_TID: AtomicU32 = AtomicU32::new(0);
static IN_PUMP: AtomicBool = AtomicBool::new(false);
static INITIALIZED: AtomicBool = AtomicBool::new(false);

type Job = Box<dyn FnOnce() + Send>;
static JOBS: Mutex<Vec<Job>> = Mutex::new(Vec::new());
static LAST_FRAME: Mutex<Option<std::time::Instant>> = Mutex::new(None);

pub(crate) fn log(msg: &str) {
    let line = format!("[focmp] {msg}\n");
    let mut c = line.clone().into_bytes();
    c.push(0);
    unsafe { win::OutputDebugStringA(c.as_ptr()) };
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{}focmp{}.log", win::exe_dir(), instance_suffix()))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// "_2", "_3", ... when FOCMP_INSTANCE is set above 1, so instances sharing
/// the game folder don't write to the same log.
fn instance_suffix() -> String {
    match std::env::var("FOCMP_INSTANCE").ok().and_then(|v| v.parse::<u32>().ok()) {
        Some(n) if n > 1 => format!("_{n}"),
        _ => String::new(),
    }
}

/// Run `f` on the game's main thread and wait for its result.
pub(crate) fn on_main<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    JOBS.lock().unwrap().push(Box::new(move || {
        let result = if engine::G::new().is_some_and(coop::game_ready) {
            Ok(f())
        } else {
            Err("game is still starting; retry once the main menu appears".into())
        };
        let _ = tx.send(result);
    }));
    rx.recv_timeout(timeout)
        .map_err(|_| String::from("timed out waiting for the game's main thread (is it frozen or loading?)"))?
}

/// Native UI callbacks can pump Windows messages while their GML state is
/// incomplete. Do not execute console/network mutations reentrantly there.
pub(crate) struct PumpBlock(bool);
pub(crate) fn block_pump() -> PumpBlock { PumpBlock(IN_PUMP.swap(true, Ordering::Acquire)) }
impl Drop for PumpBlock { fn drop(&mut self) { IN_PUMP.store(self.0, Ordering::Release); } }

type PeekMessageW = unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32) -> i32;

unsafe extern "system" fn hk_peek_message(
    msg: *mut c_void,
    hwnd: *mut c_void,
    min: u32,
    max: u32,
    remove: u32,
) -> i32 {
    let tid = unsafe { win::GetCurrentThreadId() };
    let main = MAIN_TID.load(Ordering::Relaxed);
    if main == 0 {
        MAIN_TID.store(tid, Ordering::Relaxed);
        log("first PeekMessageW call seen; main-thread pump active");
    }
    // Only the game's main thread runs GML work, and never re-entrantly.
    if (main == 0 || main == tid) && !IN_PUMP.swap(true, Ordering::Acquire) {
        PUMPS.fetch_add(1, Ordering::Relaxed);
        let jobs: Vec<Job> = std::mem::take(&mut *JOBS.lock().unwrap());
        for job in jobs {
            job();
        }
        // Co-op systems: at most once per ~15 ms (PeekMessageW can run several
        // times per frame).
        let now = std::time::Instant::now();
        let mut last = LAST_FRAME.lock().unwrap();
        if last.is_none_or(|t| now.duration_since(t) >= Duration::from_millis(15)) {
            *last = Some(now);
            drop(last);
            coop::frame();
        }
        IN_PUMP.store(false, Ordering::Release);
    }
    let orig: PeekMessageW = unsafe { std::mem::transmute(ORIG_PEEK.load(Ordering::Relaxed)) };
    unsafe { orig(msg, hwnd, min, max, remove) }
}

/// Called by the version.dll loader on its own thread.
#[unsafe(no_mangle)]
pub extern "C" fn focmp_init() {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }
    log(&format!("focmp {} loaded; game dir {}", version::BUILD, win::exe_dir()));
    // panic = "abort": make sure the reason lands in the log before the game dies.
    std::panic::set_hook(Box::new(|info| {
        log(&format!("PANIC: {info}"));
    }));
    let base = win::exe_base();
    IMAGE_BASE.store(base, Ordering::Relaxed);
    let img = match pe::Image::parse(base) {
        Ok(i) => i,
        Err(e) => return log(&format!("PE parse failed: {e}")),
    };
    log(&format!(
        "image base 0x{base:x}, .text 0x{:x}-0x{:x}",
        img.text.start, img.text.end
    ));

    let t = std::time::Instant::now();
    match gm::Runtime::scan(&img) {
        Ok(rt) => {
            log(&format!(
                "runtime scan ok in {:?}: Function_Add=0x{:x}, {} builtins, {} scripts/events (table 0x{:x})",
                t.elapsed(),
                rt.function_add,
                rt.builtins.len(),
                rt.scripts.len(),
                rt.script_table
            ));
            let _ = RUNTIME.set(rt);
            version::install();
            saves::install();
        }
        Err(e) => log(&format!("runtime scan FAILED: {e} (console will run with limited commands)")),
    }

    match unsafe { img.hook_import("USER32.dll", "PeekMessageW", hk_peek_message as *const () as usize) } {
        Ok(orig) => {
            ORIG_PEEK.store(orig, Ordering::SeqCst);
            log("hooked USER32!PeekMessageW (IAT)");
        }
        Err(e) => log(&format!("PeekMessageW hook FAILED: {e}")),
    }

    input::install(&img);

    if config::load().debug_console {
        std::thread::spawn(serve);
    } else {
        log("debug console off (enable with [debug] console = true in focmp.ini)");
    }
}

fn serve() {
    let port: u16 = std::env::var("FOCMP_DEBUG_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(47811);
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => return log(&format!("debug console bind 127.0.0.1:{port} failed: {e}")),
    };
    log(&format!("debug console listening on 127.0.0.1:{port}"));
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || {
            if let Err(e) = client(stream) {
                log(&format!("console client error: {e}"));
            }
        });
    }
}

fn client(stream: TcpStream) -> std::io::Result<()> {
    let peer = stream.peer_addr()?;
    log(&format!("console client connected: {peer}"));
    let mut out = stream.try_clone()?;
    writeln!(out, "focmp debug console {}. Type 'help'.", version::BUILD)?;
    writeln!(out, ".")?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "quit" || line == "exit" {
            break;
        }
        let reply = commands::run(line);
        out.write_all(reply.as_bytes())?;
        if !reply.ends_with('\n') {
            out.write_all(b"\n")?;
        }
        // A lone "." line ends each reply, so scripts can parse the stream.
        writeln!(out, ".")?;
    }
    log(&format!("console client disconnected: {peer}"));
    Ok(())
}
