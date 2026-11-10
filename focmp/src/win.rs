//! Minimal hand-written Win32 bindings, so the crate has no dependencies.

use std::ffi::c_void;

pub const PAGE_READWRITE: u32 = 0x04;

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    pub fn GetModuleFileNameW(module: *mut c_void, buf: *mut u16, size: u32) -> u32;
    pub fn VirtualProtect(addr: *mut c_void, size: usize, new: u32, old: *mut u32) -> i32;
    pub fn OutputDebugStringA(s: *const u8);
    pub fn GetCurrentThreadId() -> u32;
}

/// Base address of the game executable.
pub fn exe_base() -> usize {
    unsafe { GetModuleHandleW(std::ptr::null()) as usize }
}

/// Folder that contains the game executable, with a trailing separator.
pub fn exe_dir() -> String {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    let path = String::from_utf16_lossy(&buf[..n as usize]);
    match path.rfind(['\\', '/']) {
        Some(i) => path[..=i].to_string(),
        None => String::new(),
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn FlushInstructionCache(process: *mut c_void, addr: *const c_void, size: usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
}

const PAGE_EXECUTE_READWRITE: u32 = 0x40;

/// Overwrite code bytes. Returns the previous bytes.
pub unsafe fn patch_code(addr: usize, bytes: &[u8]) -> Option<Vec<u8>> {
    let mut old = 0u32;
    unsafe {
        if VirtualProtect(addr as *mut c_void, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return None;
        }
        let prev = std::slice::from_raw_parts(addr as *const u8, bytes.len()).to_vec();
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
        VirtualProtect(addr as *mut c_void, bytes.len(), old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), addr as *const c_void, bytes.len());
        Some(prev)
    }
}

/// 14-byte absolute jump: `jmp [rip+0]; dq target`.
pub fn abs_jmp(target: usize) -> [u8; 14] {
    let mut b = [0xff, 0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    b[6..].copy_from_slice(&(target as u64).to_le_bytes());
    b
}

/// Overwrite one pointer in (possibly read-only) memory.
pub unsafe fn write_ptr(slot: *mut usize, value: usize) -> bool {
    let mut old = 0u32;
    unsafe {
        if VirtualProtect(slot.cast(), 8, PAGE_READWRITE, &mut old) == 0 {
            return false;
        }
        std::ptr::write_volatile(slot, value);
        VirtualProtect(slot.cast(), 8, old, &mut old);
    }
    true
}
