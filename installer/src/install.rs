use crate::{
    Result,
    manifest::{self, Manifest, PAYLOAD},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};
#[derive(Clone, Deserialize, Serialize)]
pub struct Record {
    pub version: String,
    pub files: BTreeMap<String, String>,
    pub backups: BTreeMap<String, String>,
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
pub fn safe(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            Err(format!("Refusing symbolic link: {}", path.display()))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(err(e)),
    }
}
fn paths(game: &Path) -> Result<()> {
    for n in [
        "version.dll",
        "focmp.dll",
        "focmp.ini",
        ".focmp",
        ".focmp/backup",
        ".focmp/install.json",
        ".focmp/steam.json",
    ] {
        safe(&game.join(n))?;
    }
    for n in PAYLOAD {
        safe(&game.join(".focmp/backup").join(n))?;
    }
    Ok(())
}
pub fn record(game: &Path) -> Result<Option<Record>> {
    paths(game)?;
    let p = game.join(".focmp/install.json");
    if !p.exists() {
        return Ok(None);
    }
    let r: Record = serde_json::from_slice(&fs::read(p).map_err(err)?).map_err(err)?;
    if manifest::version(&r.version).is_none()
        || r.files
            .keys()
            .chain(r.backups.keys())
            .any(|n| !PAYLOAD.contains(&n.as_str()))
    {
        return Err("Invalid installation record".into());
    }
    Ok(Some(r))
}
struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn lock(game: &Path) -> Result<Lock> {
    let p = game.join(".focmp-install.lock");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&p)
        .map_err(|e| format!("Cannot lock installation (another installer may be running): {e}"))?;
    Ok(Lock(p))
}
// Replace a file only after its entire contents have reached a staging file on the same volume.
pub fn write(path: &Path, data: &[u8]) -> Result<()> {
    safe(path)?;
    let tmp = path.with_file_name(format!(
        ".{}.focmp-tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp)
        .map_err(err)?;
    let result = (|| {
        f.write_all(data).map_err(err)?;
        f.sync_all().map_err(err)?;
        drop(f);
        #[cfg(windows)]
        { // Rust rename replaces regular files on Windows as well as Unix.
        }
        fs::rename(&tmp, path).map_err(err)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
fn unpack(data: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut z = zip::ZipArchive::new(Cursor::new(data)).map_err(err)?;
    if z.len() > 12 {
        return Err("Too many archive entries".into());
    }
    let mut files = BTreeMap::new();
    let mut names = std::collections::BTreeSet::new();
    for i in 0..z.len() {
        let f = z.by_index(i).map_err(err)?;
        let n = f.name().map_err(err)?.into_owned();
        if !names.insert(n.clone())
            || !["version.dll", "focmp.dll", "focmp.ini"].contains(&n.as_str())
            || f.is_dir()
            || f.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
            || f.size() > 64 * 1024 * 1024
        {
            return Err(format!("Unexpected archive entry: {n}"));
        }
        let mut b = Vec::new();
        f.take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut b)
            .map_err(err)?;
        if b.len() > 64 * 1024 * 1024 {
            return Err("Oversized archive entry".into());
        }
        if PAYLOAD.contains(&n.as_str()) {
            files.insert(n, b);
        }
    }
    if files.len() != 3 {
        return Err("Incomplete mod archive".into());
    }
    for n in ["version.dll", "focmp.dll"] {
        let b = &files[n];
        let off = b
            .get(60..64)
            .map(|s| u32::from_le_bytes(s.try_into().unwrap()) as usize)
            .unwrap_or(0);
        if !b.starts_with(b"MZ")
            || b.get(off..off.saturating_add(6)) != Some(b"PE\0\0\x64\x86".as_slice())
        {
            return Err(format!("{n} is not a Windows x86-64 DLL"));
        }
    }
    Ok(files)
}
pub fn install(game: &Path, m: &Manifest, local: Option<&Path>) -> Result<String> {
    let game = game.canonicalize().map_err(err)?;
    paths(&game)?;
    let _lock = lock(&game)?;
    if crate::steam::game_running(&game)? {
        return Err("Close Flashes of Chaos before changing the installation.".into());
    }
    let (exe, data) = manifest::game_hashes(&game)?;
    if !manifest::supports(m, &exe, &data) {
        return Err(format!(
            "This game version is not supported by FOCMP {}, which supports {}. Check for a matching FOCMP release.",
            m.version,
            manifest::supported_names(m)
        ));
    }
    let payload = unpack(&manifest::package(m, local)?)?;
    let prior = record(&game)?;
    let mut r = prior.clone().unwrap_or(Record {
        version: m.version.clone(),
        files: BTreeMap::new(),
        backups: BTreeMap::new(),
    });
    if let Some(p) = &prior {
        if manifest::version(&p.version) > manifest::version(&m.version) {
            return Err("A newer mod is already installed; refusing to downgrade.".into());
        }
    }
    for (n, h) in &r.files {
        if n != "focmp.ini" && game.join(n).exists() && manifest::file_hash(&game.join(n))? != *h {
            return Err(format!(
                "{n} was changed outside the installer; restore it before updating."
            ));
        }
    }
    let state = game.join(".focmp");
    fs::create_dir_all(state.join("backup")).map_err(err)?;
    let mut before = BTreeMap::new();
    for n in PAYLOAD {
        let p = game.join(n);
        before.insert(
            n.to_owned(),
            if p.exists() {
                Some(fs::read(&p).map_err(err)?)
            } else {
                None
            },
        );
        if n == "focmp.ini" && p.exists() {
            continue;
        }
        if !r.files.contains_key(n) && p.exists() {
            let b = before[n].as_ref().unwrap();
            let backup = state.join("backup").join(n);
            if backup.exists() {
                // A previous failed install may have rolled back after saving this backup.
                if fs::read(&backup).map_err(err)? != *b {
                    return Err(format!(
                        "A different original backup exists: {}. Preserve it before reinstalling.",
                        backup.display()
                    ));
                }
            } else {
                write(&backup, b)?;
            }
            r.backups.insert(n.into(), manifest::hash(b));
        }
    }
    let result = (|| {
        for n in PAYLOAD {
            if n == "focmp.ini" && before[n].is_some() {
                continue;
            }
            write(&game.join(n), &payload[n])?;
            r.files.insert(n.into(), manifest::hash(&payload[n]));
        }
        r.version = m.version.clone();
        write(
            &state.join("install.json"),
            &serde_json::to_vec_pretty(&r).map_err(err)?,
        )
    })();
    if let Err(e) = result {
        let mut failures = Vec::new();
        for (n, b) in before {
            let p = game.join(n);
            let restore = if let Some(b) = b {
                write(&p, &b)
            } else if p.exists() {
                fs::remove_file(p).map_err(err)
            } else {
                Ok(())
            };
            if let Err(e) = restore {
                failures.push(e)
            }
        }
        return Err(format!(
            "Install failed: {e}. Rollback {}. Backups are in {}",
            if failures.is_empty() {
                "completed".into()
            } else {
                failures.join("; ")
            },
            state.display()
        ));
    }
    Ok(format!(
        "FOCMP {} installed. Existing settings were preserved. Launch from Steam as usual.",
        m.version
    ))
}
pub fn uninstall(game: &Path) -> Result<String> {
    let game = game.canonicalize().map_err(err)?;
    paths(&game)?;
    let _lock = lock(&game)?;
    if crate::steam::game_running(&game)? {
        return Err("Close Flashes of Chaos before uninstalling.".into());
    }
    let r = record(&game)?.ok_or("No installer-managed FOCMP installation found")?;
    let mut changes = Vec::new();
    let mut keep_config = false;
    for (n, h) in &r.files {
        let p = game.join(n);
        let current = if p.exists() {
            Some(manifest::file_hash(&p)?)
        } else {
            None
        };
        if current
            .as_ref()
            .is_some_and(|current| current != h && Some(current) != r.backups.get(n))
        {
            if n == "focmp.ini" {
                keep_config = true;
                continue;
            }
            return Err(format!(
                "{n} was modified; refusing to remove another program's file."
            ));
        }
        let backup = if let Some(h) = r.backups.get(n) {
            let b = fs::read(game.join(".focmp/backup").join(n)).map_err(err)?;
            if manifest::hash(&b) != *h {
                return Err(format!("Backup of {n} was modified; uninstall stopped."));
            }
            Some(b)
        } else {
            None
        };
        changes.push((p, backup));
    }
    // Undo the Proton setup first. This also checks Proton is closed.
    crate::steam::restore(&game)?;
    for (p, b) in changes {
        if let Some(b) = b {
            write(&p, &b)?
        } else if p.exists() {
            fs::remove_file(p).map_err(err)?
        }
    }
    for n in r.backups.keys() {
        fs::remove_file(game.join(".focmp/backup").join(n)).map_err(err)?;
    }
    fs::remove_file(game.join(".focmp/install.json")).map_err(err)?;
    let _ = fs::remove_dir(game.join(".focmp/backup"));
    let _ = fs::remove_dir(game.join(".focmp"));
    Ok(format!(
        "FOCMP removed; original files restored. Saves were kept.{}",
        if keep_config {
            " Your edited focmp.ini was also kept."
        } else {
            ""
        }
    ))
}
