//! Reading the loaded game image: sections, string search, rip-relative scans,
//! and import-table (IAT) patching.

use std::ffi::CStr;

#[derive(Clone, Copy, Debug)]
pub struct Section {
    pub start: usize,
    pub end: usize,
}

impl Section {
    pub fn contains(&self, addr: usize) -> bool {
        addr >= self.start && addr < self.end
    }
    pub fn bytes(&self) -> &'static [u8] {
        unsafe { std::slice::from_raw_parts(self.start as *const u8, self.end - self.start) }
    }

    pub fn slice(&self, addr: usize, len: usize) -> Option<&'static [u8]> {
        (addr >= self.start && addr.checked_add(len)? <= self.end)
            .then(|| unsafe { std::slice::from_raw_parts(addr as *const u8, len) })
    }
}

pub struct Image {
    pub base: usize,
    pub text: Section,
    pub rdata: Section,
    pub data: Section,
}

unsafe fn rd<T: Copy>(addr: usize) -> T {
    unsafe { std::ptr::read_unaligned(addr as *const T) }
}

impl Image {
    /// Resolve a verified native adapter only when its signature is unique.
    /// Mask only relocation displacements; instruction bytes must still match.
    pub fn unique_text(&self, pattern: &[u8], relocations: &[std::ops::Range<usize>]) -> Option<usize> {
        if pattern.is_empty() { return None; }
        let mut matches = self.text.bytes().windows(pattern.len()).enumerate().filter(|(_, bytes)| {
            bytes.iter().zip(pattern).enumerate().all(|(i, (a, b))| {
                a == b || relocations.iter().any(|range| range.contains(&i))
            })
        });
        let (offset, _) = matches.next()?;
        matches.next().is_none().then_some(self.text.start + offset)
    }

    /// Target of a RIP-relative operand with a four-byte displacement.
    pub fn rip_target(&self, instruction: usize, displacement: usize) -> Option<usize> {
        let at = instruction.checked_add(displacement)?;
        let bytes = self.text.slice(at, 4)?;
        let relative = i32::from_le_bytes(bytes.try_into().ok()?);
        Some((at + 4).wrapping_add_signed(relative as isize))
    }

    pub fn parse(base: usize) -> Result<Image, String> {
        unsafe {
            if rd::<u16>(base) != 0x5a4d {
                return Err("no MZ header".into());
            }
            let nt = base + rd::<u32>(base + 0x3c) as usize;
            if rd::<u32>(nt) != 0x4550 {
                return Err("no PE header".into());
            }
            let nsec = rd::<u16>(nt + 6) as usize;
            let opt_size = rd::<u16>(nt + 20) as usize;
            let sec_tab = nt + 24 + opt_size;
            let mut text = None;
            let mut rdata = None;
            let mut data = None;
            for i in 0..nsec {
                let s = sec_tab + i * 40;
                let name: [u8; 8] = rd(s);
                let vsize = rd::<u32>(s + 8) as usize;
                let va = rd::<u32>(s + 12) as usize;
                let sec = Section { start: base + va, end: base + va + vsize };
                match &name {
                    b".text\0\0\0" => text = Some(sec),
                    b".rdata\0\0" => rdata = Some(sec),
                    b".data\0\0\0" => data = Some(sec),
                    _ => {}
                }
            }
            Ok(Image {
                base,
                text: text.ok_or("no .text")?,
                rdata: rdata.ok_or("no .rdata")?,
                data: data.ok_or("no .data")?,
            })
        }
    }

    /// Address of a whole NUL-terminated string `s` in .rdata (it must be
    /// preceded by NUL, so we don't match the tail of a longer string).
    pub fn find_cstr(&self, s: &str) -> Option<usize> {
        let hay = self.rdata.bytes();
        let mut needle = s.as_bytes().to_vec();
        needle.push(0);
        let mut i = 0;
        while let Some(off) = find(&hay[i..], &needle) {
            let at = i + off;
            if at == 0 || hay[at - 1] == 0 {
                return Some(self.rdata.start + at);
            }
            i = at + 1;
        }
        None
    }

    /// Every `48 8D xx disp32` (lea r64, [rip+disp]) whose target is `target`.
    pub fn lea_refs(&self, modrm: u8, target: usize) -> Vec<usize> {
        let code = self.text.bytes();
        let mut out = Vec::new();
        for i in 0..code.len().saturating_sub(7) {
            if code[i] == 0x48 && code[i + 1] == 0x8d && code[i + 2] == modrm {
                let disp = i32::from_le_bytes(code[i + 3..i + 7].try_into().unwrap()) as isize;
                let ip = self.text.start + i + 7;
                if ip.wrapping_add_signed(disp) == target {
                    out.push(self.text.start + i);
                }
            }
        }
        out
    }

    /// Target of an `E8 rel32` call at `addr`.
    pub fn call_target(&self, addr: usize) -> Option<usize> {
        unsafe {
            if rd::<u8>(addr) != 0xe8 {
                return None;
            }
            let rel = rd::<i32>(addr + 1) as isize;
            Some((addr + 5).wrapping_add_signed(rel))
        }
    }

    /// Patch this module's import of `dll!func` to point at `hook`.
    /// Returns the original function pointer.
    pub unsafe fn hook_import(&self, dll: &str, func: &str, hook: usize) -> Result<usize, String> {
        unsafe {
            let base = self.base;
            let nt = base + rd::<u32>(base + 0x3c) as usize;
            // Optional header starts at nt+24; data directory 1 (imports) is at +120 for PE32+.
            let imp_rva = rd::<u32>(nt + 24 + 120) as usize;
            if imp_rva == 0 {
                return Err("no import directory".into());
            }
            let mut desc = base + imp_rva;
            loop {
                let orig_thunk = rd::<u32>(desc) as usize;
                let name_rva = rd::<u32>(desc + 12) as usize;
                let first_thunk = rd::<u32>(desc + 16) as usize;
                if name_rva == 0 {
                    break;
                }
                let name = CStr::from_ptr((base + name_rva) as *const i8).to_string_lossy();
                if name.eq_ignore_ascii_case(dll) {
                    let lookup = if orig_thunk != 0 { orig_thunk } else { first_thunk };
                    let mut i = 0;
                    loop {
                        let entry = rd::<u64>(base + lookup + i * 8);
                        if entry == 0 {
                            break;
                        }
                        if entry & (1 << 63) == 0 {
                            let fname = CStr::from_ptr((base + entry as usize + 2) as *const i8);
                            if fname.to_bytes() == func.as_bytes() {
                                let slot = (base + first_thunk + i * 8) as *mut usize;
                                let orig = *slot;
                                if !crate::win::write_ptr(slot, hook) {
                                    return Err("VirtualProtect failed".into());
                                }
                                return Ok(orig);
                            }
                        }
                        i += 1;
                    }
                    return Err(format!("{func} not imported from {dll}"));
                }
                desc += 20;
            }
            Err(format!("{dll} not imported"))
        }
    }
}

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Read a C string at `addr` if it lies inside `sec` and looks sane.
pub fn cstr_in(sec: &Section, addr: usize) -> Option<&'static str> {
    if !sec.contains(addr) {
        return None;
    }
    let bytes = &sec.bytes()[addr - sec.start..];
    let len = bytes.iter().take(512).position(|&b| b == 0)?;
    std::str::from_utf8(&bytes[..len]).ok()
}
