//! Debug input injection, used for driving menus from the console.
//!
//! - Keyboard: GameMaker's own `keyboard_key_press` / `keyboard_key_release`
//!   (main thread), so the Input library sees a normal key press.
//! - Mouse: import-table hooks on GetCursorPos / GetAsyncKeyState / GetKeyState
//!   make the game see an injected position and button state while an override
//!   is active. We also post WM_MOUSEMOVE / WM_xBUTTONDOWN/UP to the game
//!   window. The real cursor is never moved.

use crate::pe::Image;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicUsize, Ordering};

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn ClientToScreen(hwnd: *mut c_void, p: *mut Point) -> i32;
    fn GetClientRect(hwnd: *mut c_void, r: *mut Rect) -> i32;
    fn PostMessageW(hwnd: *mut c_void, msg: u32, w: usize, l: isize) -> i32;
}

const WM_MOUSEMOVE: u32 = 0x200;
const WM_LBUTTONDOWN: u32 = 0x201;
const WM_LBUTTONUP: u32 = 0x202;
const WM_RBUTTONDOWN: u32 = 0x204;
const WM_RBUTTONUP: u32 = 0x205;
const VK_LBUTTON: i32 = 1;
const VK_RBUTTON: i32 = 2;

pub static HWND: AtomicIsize = AtomicIsize::new(0);
static OVERRIDE: AtomicBool = AtomicBool::new(false);
static CLIENT_X: AtomicI32 = AtomicI32::new(0);
static CLIENT_Y: AtomicI32 = AtomicI32::new(0);
static LBTN: AtomicBool = AtomicBool::new(false);
/// Injected key held down (VK code, 0 = none). Reported by the GetKeyState hooks.
static KEY_DOWN: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
static RBTN: AtomicBool = AtomicBool::new(false);

static ORIG_GET_CURSOR_POS: AtomicUsize = AtomicUsize::new(0);
static ORIG_GET_ASYNC_KEY_STATE: AtomicUsize = AtomicUsize::new(0);
static ORIG_GET_KEY_STATE: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn hk_get_cursor_pos(p: *mut Point) -> i32 {
    let hwnd = HWND.load(Ordering::Relaxed) as *mut c_void;
    if OVERRIDE.load(Ordering::Relaxed) && !hwnd.is_null() && !p.is_null() {
        let mut pt = Point { x: CLIENT_X.load(Ordering::Relaxed), y: CLIENT_Y.load(Ordering::Relaxed) };
        unsafe {
            ClientToScreen(hwnd, &mut pt);
            *p = pt;
        }
        return 1;
    }
    let orig: unsafe extern "system" fn(*mut Point) -> i32 =
        unsafe { std::mem::transmute(ORIG_GET_CURSOR_POS.load(Ordering::Relaxed)) };
    unsafe { orig(p) }
}

fn injected_button(vk: i32) -> Option<bool> {
    let k = KEY_DOWN.load(Ordering::Relaxed);
    if k != 0 && k == vk {
        return Some(true);
    }
    if !OVERRIDE.load(Ordering::Relaxed) {
        return None;
    }
    match vk {
        VK_LBUTTON => Some(LBTN.load(Ordering::Relaxed)),
        VK_RBUTTON => Some(RBTN.load(Ordering::Relaxed)),
        _ => None,
    }
}

unsafe extern "system" fn hk_get_async_key_state(vk: i32) -> i16 {
    if let Some(down) = injected_button(vk) {
        return if down { i16::MIN } else { 0 };
    }
    let orig: unsafe extern "system" fn(i32) -> i16 =
        unsafe { std::mem::transmute(ORIG_GET_ASYNC_KEY_STATE.load(Ordering::Relaxed)) };
    unsafe { orig(vk) }
}

unsafe extern "system" fn hk_get_key_state(vk: i32) -> i16 {
    if let Some(down) = injected_button(vk) {
        return if down { i16::MIN } else { 0 };
    }
    let orig: unsafe extern "system" fn(i32) -> i16 =
        unsafe { std::mem::transmute(ORIG_GET_KEY_STATE.load(Ordering::Relaxed)) };
    unsafe { orig(vk) }
}

pub fn install(img: &Image) {
    let hooks: [(&str, usize, &AtomicUsize); 3] = [
        ("GetCursorPos", hk_get_cursor_pos as *const () as usize, &ORIG_GET_CURSOR_POS),
        ("GetAsyncKeyState", hk_get_async_key_state as *const () as usize, &ORIG_GET_ASYNC_KEY_STATE),
        ("GetKeyState", hk_get_key_state as *const () as usize, &ORIG_GET_KEY_STATE),
    ];
    for (name, hook, orig) in hooks {
        match unsafe { img.hook_import("USER32.dll", name, hook) } {
            Ok(o) => {
                orig.store(o, Ordering::SeqCst);
                crate::log(&format!("hooked USER32!{name} (IAT)"));
            }
            Err(e) => crate::log(&format!("{name} hook FAILED: {e}")),
        }
    }
}

pub fn client_size() -> (i32, i32) {
    let hwnd = HWND.load(Ordering::Relaxed) as *mut c_void;
    let mut r = Rect::default();
    if hwnd.is_null() || unsafe { GetClientRect(hwnd, &mut r) } == 0 {
        return (0, 0);
    }
    (r.right - r.left, r.bottom - r.top)
}

fn lparam(x: i32, y: i32) -> isize {
    ((y as u32 as isize & 0xffff) << 16) | (x as u32 as isize & 0xffff)
}

/// Move the injected cursor to client coordinates and turn the override on.
pub fn mouse_move(x: i32, y: i32) {
    CLIENT_X.store(x, Ordering::Relaxed);
    CLIENT_Y.store(y, Ordering::Relaxed);
    OVERRIDE.store(true, Ordering::Relaxed);
    let hwnd = HWND.load(Ordering::Relaxed) as *mut c_void;
    if !hwnd.is_null() {
        unsafe { PostMessageW(hwnd, WM_MOUSEMOVE, 0, lparam(x, y)) };
    }
}

pub fn mouse_button(right: bool, down: bool) {
    let (flag, msg) = match (right, down) {
        (false, true) => (&LBTN, WM_LBUTTONDOWN),
        (false, false) => (&LBTN, WM_LBUTTONUP),
        (true, true) => (&RBTN, WM_RBUTTONDOWN),
        (true, false) => (&RBTN, WM_RBUTTONUP),
    };
    flag.store(down, Ordering::Relaxed);
    let hwnd = HWND.load(Ordering::Relaxed) as *mut c_void;
    if !hwnd.is_null() {
        let wparam = if down { if right { 2 } else { 1 } } else { 0 };
        let p = lparam(CLIENT_X.load(Ordering::Relaxed), CLIENT_Y.load(Ordering::Relaxed));
        unsafe { PostMessageW(hwnd, msg, wparam, p) };
    }
}

const WM_KEYDOWN: u32 = 0x100;
const WM_KEYUP: u32 = 0x101;

/// Press or release a key in THIS game window only.
///
/// GameMaker's keyboard_key_press uses `keybd_event`, which is system-wide:
/// the key goes to whichever window has OS focus — with several instances
/// open that's usually the wrong one. So post the key messages to our own
/// window and report the key as down through the GetKeyState hooks.
pub fn key(vk: u32, down: bool) {
    let hwnd = HWND.load(Ordering::Relaxed) as *mut c_void;
    if down {
        KEY_DOWN.store(vk as i32, Ordering::Relaxed);
    } else if KEY_DOWN.load(Ordering::Relaxed) == vk as i32 {
        KEY_DOWN.store(0, Ordering::Relaxed);
    }
    if !hwnd.is_null() {
        // lParam: repeat count 1; for key-up also the previous-state and transition bits.
        let l: isize = if down { 1 } else { 1 | (1 << 30) | (1 << 31) };
        unsafe { PostMessageW(hwnd, if down { WM_KEYDOWN } else { WM_KEYUP }, vk as usize, l) };
    }
}

/// Give the mouse back to the real device.
pub fn mouse_release() {
    LBTN.store(false, Ordering::Relaxed);
    RBTN.store(false, Ordering::Relaxed);
    OVERRIDE.store(false, Ordering::Relaxed);
}

pub fn mouse_state() -> String {
    if OVERRIDE.load(Ordering::Relaxed) {
        format!(
            "injected at client ({}, {}), L={} R={}",
            CLIENT_X.load(Ordering::Relaxed),
            CLIENT_Y.load(Ordering::Relaxed),
            LBTN.load(Ordering::Relaxed),
            RBTN.load(Ordering::Relaxed)
        )
    } else {
        "real device".into()
    }
}

/// Virtual-key code from a name: up/down/left/right/enter/esc/space/tab/
/// backspace/shift/ctrl/alt/f1..f12, a single letter or digit, or a number.
pub fn vk(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    let k = match n.as_str() {
        "left" => 37,
        "up" => 38,
        "right" => 39,
        "down" => 40,
        "enter" | "return" => 13,
        "esc" | "escape" => 27,
        "space" => 32,
        "tab" => 9,
        "backspace" => 8,
        "shift" => 16,
        "ctrl" | "control" => 17,
        "alt" => 18,
        _ if n.len() == 1 && n.as_bytes()[0].is_ascii_alphanumeric() => n.to_ascii_uppercase().as_bytes()[0] as u32,
        _ if n.starts_with('f') && n[1..].parse::<u32>().is_ok_and(|f| (1..=12).contains(&f)) => {
            111 + n[1..].parse::<u32>().unwrap()
        }
        _ => return n.parse().ok(),
    };
    Some(k)
}
