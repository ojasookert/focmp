use crate::{
    Result, install,
    vdf::{self, Kv},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
pub const EXE: &str = "Flashes_of_Chaos.exe";
#[derive(Clone)]
pub struct Steam {
    pub libraries: Vec<PathBuf>,
}
impl Steam {
    pub fn find(explicit: Option<&Path>) -> Option<Self> {
        let mut roots = Vec::new();
        if let Some(p) = explicit {
            roots.push(p.to_owned())
        } else {
            #[cfg(windows)]
            {
                if let Ok(o) = Command::new("powershell.exe")
                    .creation_flags(0x08000000) // CREATE_NO_WINDOW
                    .args(["-NoProfile", "-Command", r"[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); [Console]::Write((Get-ItemProperty 'HKCU:\Software\Valve\Steam' -Name SteamPath -ErrorAction Stop).SteamPath)"])
                    .output()
                {
                    if o.status.success() {
                        if let Ok(value) = String::from_utf8(o.stdout) {
                            roots.push(PathBuf::from(value.trim()));
                        }
                    }
                }
                for e in ["ProgramFiles(x86)", "ProgramFiles"] {
                    if let Some(p) = std::env::var_os(e) {
                        roots.push(PathBuf::from(p).join("Steam"))
                    }
                }
            }
            #[cfg(not(windows))]
            {
                if let Some(h) = std::env::var_os("HOME") {
                    let h = PathBuf::from(h);
                    for p in [
                        ".local/share/Steam",
                        ".steam/steam",
                        ".steam/root",
                        ".var/app/com.valvesoftware.Steam/.local/share/Steam",
                    ] {
                        roots.push(h.join(p));
                    }
                }
            }
        }
        let root = roots
            .into_iter()
            .find(|p| p.join("steamapps").is_dir())?
            .canonicalize()
            .ok()?;
        let mut libraries = vec![root.clone()];
        if let Ok(text) = fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            if let Some(k) = vdf::parse(&text).get("libraryfolders") {
                for (_, v) in k.entries() {
                    if let Some(p) = v.get("path").and_then(Kv::str).or_else(|| v.str()) {
                        let p = PathBuf::from(p);
                        if p.join("steamapps").is_dir() && !libraries.contains(&p) {
                            libraries.push(p)
                        }
                    }
                }
            }
        }
        Some(Self { libraries })
    }
    /// Steam's build id of the installed game, from the library's appmanifest next to the game folder.
    pub fn build_id(game: &Path) -> Option<u64> {
        let steamapps = game.parent()?.parent()?;
        let text = fs::read_to_string(steamapps.join("appmanifest_3399100.acf")).ok()?;
        vdf::parse(&text).path(&["AppState", "buildid"])?.str()?.parse().ok()
    }

    pub fn game(&self) -> Option<PathBuf> {
        self.libraries.iter().find_map(|l| {
            let text = fs::read_to_string(l.join("steamapps/appmanifest_3399100.acf")).ok()?;
            let doc = vdf::parse(&text);
            let folder = doc.path(&["AppState", "installdir"])?.str()?;
            let p = l.join("steamapps/common").join(folder);
            p.join(EXE).is_file().then_some(p)
        })
    }
}
#[cfg(not(windows))]
fn process_match(f: impl Fn(&str, &[u8]) -> bool) -> bool {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| {
            if !e
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|c| c.is_ascii_digit())
            {
                return false;
            }
            let comm = fs::read_to_string(e.path().join("comm")).unwrap_or_default();
            let cmd = fs::read(e.path().join("cmdline")).unwrap_or_default();
            f(comm.trim(), &cmd)
        })
}
pub fn game_running(game: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        let _ = game;
        let out = Command::new("tasklist")
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .args([
                "/FI",
                "IMAGENAME eq Flashes_of_Chaos.exe",
                "/FO",
                "CSV",
                "/NH",
            ])
            .output()
            .map_err(|e| format!("Cannot check running game: {e}"))?;
        if !out.status.success() {
            return Err("Cannot check whether the game is running".into());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .to_ascii_lowercase()
            .contains("flashes_of_chaos.exe"))
    }
    #[cfg(not(windows))]
    {
        let exe = game.join(EXE).to_string_lossy().into_owned();
        Ok(process_match(|comm, cmd| {
            let args = cmd
                .split(|b| *b == 0)
                .map(String::from_utf8_lossy)
                .collect::<Vec<_>>();
            args.iter()
                .any(|a| a.as_ref() == exe || a.replace('\\', "/").ends_with(&exe))
                || comm.to_lowercase().starts_with("flashes_of_ch")
                || args.iter().any(|a| {
                    a.replace('\\', "/")
                        .rsplit('/')
                        .next()
                        .is_some_and(|name| name.eq_ignore_ascii_case(EXE))
                })
        }))
    }
}
// Proton: Wine prefers its builtin version.dll, so the game's per-app DllOverrides in the prefix's registry
// (user.reg) select ours. Steam's launch options aren't touched.
const OVERRIDE_KEY: &str = r"Software\\Wine\\AppDefaults\\Flashes_of_Chaos.exe\\DllOverrides";
const OVERRIDE: &str = "\"version\"=\"native,builtin\"";
#[derive(Serialize, Deserialize)]
struct ProtonRecord {
    registry: PathBuf,
    /// The previous "version" value line, verbatim (None: there was none).
    before: Option<String>,
}
/// The game's Proton prefix registry: compatdata next to the game's library, or in another Steam library.
pub fn prefix_registry(steam: Option<&Steam>, game: &Path) -> Option<PathBuf> {
    let rel = "compatdata/3399100/pfx/user.reg";
    let mut candidates: Vec<PathBuf> = game.parent().and_then(Path::parent).map(|s| s.join(rel)).into_iter().collect();
    for l in steam.map(|s| s.libraries.as_slice()).unwrap_or_default() {
        candidates.push(l.join("steamapps").join(rel));
    }
    candidates.into_iter().find(|p| p.is_file())
}
fn reg_section(lines: &[&str]) -> Option<(usize, usize)> {
    let header = lines.iter().position(|l| {
        l.strip_prefix('[')
            .and_then(|l| l.split(']').next())
            .is_some_and(|k| k.eq_ignore_ascii_case(OVERRIDE_KEY))
    })?;
    let end = (header + 1..lines.len())
        .find(|&i| lines[i].trim().is_empty() || lines[i].starts_with('['))
        .unwrap_or(lines.len());
    Some((header, end))
}
fn reg_value_line(lines: &[&str], (header, end): (usize, usize)) -> Option<usize> {
    (header + 1..end).find(|&i| lines[i].to_ascii_lowercase().starts_with("\"version\"="))
}
/// The "version" value line of the game's DllOverrides key.
fn reg_get(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let i = reg_value_line(&lines, reg_section(&lines)?)?;
    Some(lines[i].trim_end_matches('\r').to_owned())
}
fn reg_set(text: &str, value: Option<&str>) -> String {
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    let view: Vec<&str> = lines.iter().map(String::as_str).collect();
    match (reg_section(&view), value) {
        (Some(section), value) => {
            let (header, end) = section;
            match (reg_value_line(&view, section), value) {
                (Some(i), Some(v)) => lines[i] = v.to_owned(),
                (Some(i), None) => {
                    let others = (header + 1..end).any(|j| j != i && !view[j].starts_with('#'));
                    if others {
                        lines.remove(i);
                    } else {
                        // with the blank line after it, unless that is the file's final newline
                        let blank = end + 1 < lines.len() && view[end].trim().is_empty();
                        lines.drain(header..end + blank as usize);
                    }
                }
                (None, Some(v)) => lines.insert(end, v.to_owned()),
                (None, None) => {}
            }
            lines.join("\n")
        }
        (None, Some(v)) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let mut out = text.to_owned();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out + &format!("[{OVERRIDE_KEY}] {now}\n{v}\n")
        }
        (None, None) => text.to_owned(),
    }
}
/// Wine keeps the registry in memory while the prefix runs and writes it back when it exits.
fn prefix_idle(registry: &Path) -> Result<()> {
    #[cfg(not(windows))]
    {
        let pfx = registry.parent().and_then(|p| p.canonicalize().ok());
        let busy = fs::read_dir("/proc").into_iter().flatten().flatten().any(|e| {
            let env = fs::read(e.path().join("environ")).unwrap_or_default();
            env.split(|b| *b == 0).any(|v| {
                v.strip_prefix(b"WINEPREFIX=")
                    .and_then(|p| PathBuf::from(String::from_utf8_lossy(p).as_ref()).canonicalize().ok())
                    .is_some_and(|p| Some(p) == pfx)
            })
        });
        if busy {
            return Err("Proton is still running for the game. Wait a few seconds after closing it, then try again.".into());
        }
    }
    let _ = registry;
    Ok(())
}
pub fn prepare(steam: Option<&Steam>, game: &Path) -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    let registry = prefix_registry(steam, game)
        .ok_or("Proton prefix not found. Start the game once through Steam, quit it, then install again.")?;
    install::safe(&registry)?;
    prefix_idle(&registry)
}
pub fn configure(steam: Option<&Steam>, game: &Path) -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    prepare(steam, game)?;
    let registry = prefix_registry(steam, game).ok_or("Proton prefix not found")?;
    let text = fs::read_to_string(&registry).map_err(|e| e.to_string())?;
    let current = reg_get(&text);
    let record_path = game.join(".focmp/proton.json");
    let previous: Option<ProtonRecord> = fs::read(&record_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(|r: &ProtonRecord| r.registry == registry);
    let before = match previous {
        Some(r) => r.before,
        None => current.clone(),
    };
    let record = ProtonRecord { registry: registry.clone(), before };
    install::write(&record_path, &serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?)?;
    if current.as_deref() != Some(OVERRIDE) {
        let backup = registry.with_file_name("user.reg.focmp-backup");
        if !backup.exists() {
            install::write(&backup, text.as_bytes())?;
        }
        install::write(&registry, reg_set(&text, Some(OVERRIDE)).as_bytes())?;
    }
    Ok(())
}
/// Undoes the Proton setup: the prefix's override, only if it is still ours.
pub fn restore(game: &Path) -> Result<()> {
    let p = game.join(".focmp/proton.json");
    if !p.exists() {
        return Ok(());
    }
    let r: ProtonRecord = serde_json::from_slice(&fs::read(&p).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if r.registry.is_file() {
        install::safe(&r.registry)?;
        prefix_idle(&r.registry)?;
        let text = fs::read_to_string(&r.registry).map_err(|e| e.to_string())?;
        if reg_get(&text).as_deref() == Some(OVERRIDE) && r.before.as_deref() != Some(OVERRIDE) {
            install::write(&r.registry, reg_set(&text, r.before.as_deref()).as_bytes())?;
        }
    }
    fs::remove_file(p).map_err(|e| e.to_string())
}
pub fn launch() -> Result<()> {
    #[cfg(windows)]
    let result = Command::new("cmd")
        .args(["/C", "start", "", "steam://rungameid/3399100"])
        .spawn();
    #[cfg(not(windows))]
    let result = Command::new("xdg-open")
        .arg("steam://rungameid/3399100")
        .spawn();
    result.map(|_| ()).map_err(|e| e.to_string())
}
