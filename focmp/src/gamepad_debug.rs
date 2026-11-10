//! Console-only virtual XInput controller. Injection goes through the runner's
//! normal device discovery, deadzones and Input library, not menu actions.
use crate::{commands::main, hooks};
use std::ffi::{CString, c_void};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::Duration;

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
struct Pad {
    buttons: u16,
    lt: u8,
    rt: u8,
    lx: i16,
    ly: i16,
    rx: i16,
    ry: i16,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct State {
    packet: u32,
    pad: Pad,
}
#[repr(C)]
#[derive(Default)]
struct Capabilities {
    kind: u8,
    subtype: u8,
    flags: u16,
    pad: Pad,
    vibration: [u16; 2],
}
#[derive(Default)]
struct Virtual {
    enabled: bool,
    index: u32,
    state: State,
}
static PAD: Mutex<Virtual> = Mutex::new(Virtual {
    enabled: false,
    index: 0,
    state: State {
        packet: 0,
        pad: Pad {
            buttons: 0,
            lt: 0,
            rt: 0,
            lx: 0,
            ly: 0,
            rx: 0,
            ry: 0,
        },
    },
});
static ACTIVE_WINDOW: AtomicUsize = AtomicUsize::new(0);
static POLLS: AtomicU64 = AtomicU64::new(0);
static GET: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];
static CAPS: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];
static SET: [AtomicUsize; 5] = [const { AtomicUsize::new(0) }; 5];
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}
#[link(name = "user32")]
unsafe extern "system" {
    fn PostMessageW(hwnd: *mut c_void, msg: u32, w: usize, l: isize) -> i32;
}
fn notify_device_change() {
    main(|rt| {
        let hwnd = unsafe { rt.call("window_handle", &[]) }
            .map(|v| v.v)
            .unwrap_or(0) as *mut c_void;
        if !hwnd.is_null() {
            crate::input::HWND.store(hwnd as isize, Ordering::Relaxed);
            unsafe {
                PostMessageW(hwnd, 0x219, 7, 0);
            }
        }
        "gamepad hotplug notification sent".into()
    });
}
unsafe extern "system" fn get<const N: usize>(index: u32, out: *mut State) -> u32 {
    let p = PAD.lock().unwrap();
    if p.enabled && index == p.index {
        if out.is_null() {
            return 87;
        }
        unsafe {
            *out = p.state;
        }
        POLLS.fetch_add(1, Ordering::Relaxed);
        return 0;
    }
    drop(p);
    let original = GET[N].load(Ordering::Acquire);
    if original == 0 { return 1167; }
    let f: unsafe extern "system" fn(u32, *mut State) -> u32 =
        unsafe { std::mem::transmute(original) };
    unsafe { f(index, out) }
}
unsafe extern "system" fn capabilities<const N: usize>(
    index: u32,
    flags: u32,
    out: *mut Capabilities,
) -> u32 {
    let p = PAD.lock().unwrap();
    if p.enabled && index == p.index {
        if out.is_null() {
            return 87;
        }
        unsafe {
            *out = Capabilities {
                kind: 1,
                subtype: 1,
                flags: 0,
                pad: Pad {
                    buttons: 0xf3ff,
                    lt: 255,
                    rt: 255,
                    lx: i16::MAX,
                    ly: i16::MAX,
                    rx: i16::MAX,
                    ry: i16::MAX,
                },
                vibration: [u16::MAX; 2],
            };
        }
        return 0;
    }
    drop(p);
    let original = CAPS[N].load(Ordering::Acquire);
    if original == 0 { return 1167; }
    let f: unsafe extern "system" fn(u32, u32, *mut Capabilities) -> u32 =
        unsafe { std::mem::transmute(original) };
    unsafe { f(index, flags, out) }
}
unsafe extern "system" fn set<const N: usize>(index: u32, vibration: *const c_void) -> u32 {
    let p = PAD.lock().unwrap();
    if p.enabled && index == p.index {
        return 0;
    }
    drop(p);
    let original = SET[N].load(Ordering::Acquire);
    if original == 0 { return 1167; }
    let f: unsafe extern "system" fn(u32, *const c_void) -> u32 =
        unsafe { std::mem::transmute(original) };
    unsafe { f(index, vibration) }
}
unsafe extern "system" fn active_window() -> *mut c_void {
    if PAD.lock().unwrap().enabled {
        let hwnd = crate::input::HWND.load(Ordering::Relaxed) as *mut c_void;
        if !hwnd.is_null() {
            return hwnd;
        }
    }
    let original = ACTIVE_WINDOW.load(Ordering::Acquire);
    if original == 0 { return std::ptr::null_mut(); }
    let f: unsafe extern "system" fn() -> *mut c_void =
        unsafe { std::mem::transmute(original) };
    unsafe { f() }
}
fn install() -> Result<usize, String> {
    if ACTIVE_WINDOW.load(Ordering::Relaxed) == 0 {
        let img = crate::pe::Image::parse(crate::win::exe_base())?;
        let original = unsafe {
            img.hook_import(
                "USER32.dll",
                "GetActiveWindow",
                active_window as *const () as usize,
            )
        }?;
        ACTIVE_WINDOW.store(original, Ordering::SeqCst);
    }

    let gets = [
        get::<0> as *const (),
        get::<1> as *const (),
        get::<2> as *const (),
        get::<3> as *const (),
        get::<4> as *const (),
    ];
    let caps = [
        capabilities::<0> as *const (),
        capabilities::<1> as *const (),
        capabilities::<2> as *const (),
        capabilities::<3> as *const (),
        capabilities::<4> as *const (),
    ];
    let sets = [
        set::<0> as *const (),
        set::<1> as *const (),
        set::<2> as *const (),
        set::<3> as *const (),
        set::<4> as *const (),
    ];
    let mut seen = std::collections::HashSet::new();
    let mut found = 0;
    for (i, dll) in [
        "xinput1_4.dll",
        "xinput1_3.dll",
        "xinput9_1_0.dll",
        "xinput1_2.dll",
        "xinput1_1.dll",
    ]
    .iter()
    .enumerate()
    {
        let name = CString::new(*dll).unwrap();
        let module = unsafe { GetModuleHandleA(name.as_ptr() as *const u8) };
        if module.is_null() {
            continue;
        }
        for (name, hook, slot) in [
            ("XInputGetState", gets[i], &GET[i]),
            ("XInputGetCapabilities", caps[i], &CAPS[i]),
            ("XInputSetState", sets[i], &SET[i]),
        ] {
            let export = CString::new(name).unwrap();
            let address = unsafe { GetProcAddress(module, export.as_ptr() as *const u8) } as usize;
            if address == 0 || !seen.insert(address) {
                continue;
            }
            if slot.load(Ordering::Relaxed) == 0 {
                let f = hooks::install_address(&format!("debug {dll}!{name}"), address, hook)?;
                slot.store(f as usize, Ordering::SeqCst);
            }
            if name == "XInputGetState" {
                found += 1;
            }
        }
    }
    if found == 0 {
        Err("no loaded XInput provider; wait for the main menu".into())
    } else {
        Ok(found)
    }
}
fn button(name: &str) -> Option<u16> {
    Some(match name {
        "up" => 1,
        "down" => 2,
        "left" => 4,
        "right" => 8,
        "start" => 0x10,
        "back" | "select" => 0x20,
        "ls" | "l3" => 0x40,
        "rs" | "r3" => 0x80,
        "lb" => 0x100,
        "rb" => 0x200,
        "a" => 0x1000,
        "b" => 0x2000,
        "x" => 0x4000,
        "y" => 0x8000,
        _ => return None,
    })
}
fn edit(f: impl FnOnce(&mut Pad)) -> Result<(), String> {
    let mut p = PAD.lock().unwrap();
    if !p.enabled {
        return Err("enable a virtual controller with 'pad on' first".into());
    }
    f(&mut p.state.pad);
    p.state.packet = p.state.packet.wrapping_add(1);
    Ok(())
}
fn status() -> String {
    let p = PAD.lock().unwrap();
    let b = p.state.pad;
    format!(
        "virtual gamepad {} slot={} packet={} polls={} buttons=0x{:04x} left=({:.3},{:.3}) right=({:.3},{:.3}) triggers=({:.3},{:.3})",
        if p.enabled { "on" } else { "off" },
        p.index,
        p.state.packet,
        POLLS.load(Ordering::Relaxed),
        b.buttons,
        b.lx as f64 / 32767.0,
        -(b.ly as f64) / 32767.0,
        b.rx as f64 / 32767.0,
        -(b.ry as f64) / 32767.0,
        b.lt as f64 / 255.0,
        b.rt as f64 / 255.0
    )
}
pub fn console(args: &[String]) -> String {
    let a: Vec<&str> = args.iter().map(|s| s.trim_start_matches('\u{1}')).collect();
    match command(&a) {
        Ok(()) => status(),
        Err(e) => format!("error: {e}"),
    }
}
fn command(a: &[&str]) -> Result<(), String> {
    match a {
        []|["state"]=>Ok(()),
        ["on"]|["on",_]=>{
            let index=a.get(1).unwrap_or(&"0").parse::<u32>().map_err(|_|"slot must be 0..3")?;
            if index>3{return Err("slot must be 0..3".into());}
            let result=main(|_|match install(){Ok(n)=>format!("ok: {n} XInput providers"),Err(e)=>e});
            if !result.starts_with("ok:"){return Err(result);}
            let mut p=PAD.lock().unwrap();p.enabled=true;p.index=index;p.state.pad=Pad::default();p.state.packet=p.state.packet.wrapping_add(1);
            drop(p);notify_device_change();crate::log(&format!("debug virtual gamepad connected at slot {index}"));Ok(())
        }
        ["off"]=>{let mut p=PAD.lock().unwrap();p.enabled=false;p.state.pad=Pad::default();p.state.packet=p.state.packet.wrapping_add(1);drop(p);notify_device_change();Ok(())}
        ["neutral"]=>edit(|p|*p=Pad::default()),
        [op,name] if *op=="down"||*op=="up"=>{
            let mask=button(name).ok_or("button: a b x y lb rb ls rs start back up down left right")?;
            edit(|p|if *op=="down"{p.buttons|=mask}else{p.buttons&=!mask})
        }
        ["press",name]|["press",name,_]=>{
            let mask=button(name).ok_or("unknown gamepad button")?;
            let ms=a.get(2).unwrap_or(&"100").parse::<u64>().map_err(|_|"hold_ms must be 20..2500")?;
            if !(20..=2500).contains(&ms){return Err("hold_ms must be 20..2500".into());}
            edit(|p|p.buttons|=mask)?;std::thread::sleep(Duration::from_millis(ms));edit(|p|p.buttons&=!mask)?;
            std::thread::sleep(Duration::from_millis(80));Ok(())
        }
        ["stick",side,x,y]=>{
            let parse=|s:&str|s.parse::<f64>().ok().filter(|v|v.is_finite()&&(-1.0..=1.0).contains(v)).ok_or("stick axes must be -1..1 (right/down positive)");
            let x=(parse(x)?*32767.0).round() as i16;let y=(-parse(y)?*32767.0).round() as i16;
            match *side{"left"=>edit(|p|{p.lx=x;p.ly=y;}),"right"=>edit(|p|{p.rx=x;p.ry=y;}),_=>Err("stick: left or right".into())}
        }
        ["trigger",side,value]=>{
            let v=value.parse::<f64>().ok().filter(|v|v.is_finite()&&(0.0..=1.0).contains(v)).ok_or("trigger value must be 0..1")?;
            match *side{"left"|"lt"=>edit(|p|p.lt=(v*255.0).round() as u8),"right"|"rt"=>edit(|p|p.rt=(v*255.0).round() as u8),_=>Err("trigger: left or right".into())}
        }
        _=>Err("pad on [slot] | off | state | neutral | press BUTTON [ms] | down/up BUTTON | stick left/right X Y | trigger left/right 0..1".into()),
    }
}
