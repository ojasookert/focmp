use crate::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path, time::Duration};
pub const REPO: &str = "https://github.com/ojasookert/focmp";
pub const EMBEDDED_ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.zip"));
pub const EMBEDDED_MANIFEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/manifest.json"));
pub const PAYLOAD: [&str; 3] = ["version.dll", "focmp.dll", "focmp.ini"];
#[derive(Clone, Deserialize, Serialize)]
pub struct Download {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub schema: u32,
    pub mod_id: String,
    pub version: String,
    pub game_app_id: u32,
    pub game_executable: String,
    pub supported_game_sha256: Vec<String>,
    /// Supported game versions by both files' hashes (manifests before 0.1.74 have none: then the exe hash list).
    #[serde(default)]
    pub game_versions: Vec<GameVersion>,
    pub package: Download,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct GameVersion {
    pub name: String,
    #[serde(default)]
    pub steam_build: Option<u64>,
    pub exe_sha256: String,
    pub data_sha256: String,
}
/// The game's two version-defining files: the GameMaker runner and data.win (game code and assets).
pub fn game_hashes(game: &Path) -> Result<(String, String)> {
    Ok((file_hash(&game.join("Flashes_of_Chaos.exe"))?, file_hash(&game.join("data.win"))?))
}
/// The supported version these hashes are, if any.
pub fn identify<'a>(m: &'a Manifest, exe: &str, data: &str) -> Option<&'a GameVersion> {
    m.game_versions.iter().find(|v| v.exe_sha256 == exe && v.data_sha256 == data)
}
/// Whether this manifest supports the game build with these hashes (older manifests: the exe hash only).
pub fn supports(m: &Manifest, exe: &str, data: &str) -> bool {
    if m.game_versions.is_empty() {
        m.supported_game_sha256.iter().any(|h| h == exe)
    } else {
        identify(m, exe, data).is_some()
    }
}
/// "v1.0.01 hotfix", or the list of supported names.
pub fn supported_names(m: &Manifest) -> String {
    let n: Vec<&str> = m.game_versions.iter().map(|v| v.name.as_str()).collect();
    if n.is_empty() { "the game build it was made for".into() } else { n.join(", ") }
}
pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn file_hash(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
pub fn version(v: &str) -> Option<(u64, u64, u64)> {
    let p: Vec<_> = v.split('.').collect();
    if p.len() != 3 {
        return None;
    }
    Some((p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?))
}
pub fn parse(data: &[u8]) -> Result<Manifest> {
    let m: Manifest = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    if m.schema != 1
        || m.mod_id != "focmp"
        || m.game_app_id != 3399100
        || m.game_executable != "Flashes_of_Chaos.exe"
        || version(&m.version).is_none()
    {
        return Err("Unsupported FOCMP manifest".into());
    }
    let valid_hash = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if m.package.size == 0
        || m.package.size > 64 * 1024 * 1024
        || !valid_hash(&m.package.sha256)
        || m.supported_game_sha256.is_empty()
        || !m.supported_game_sha256.iter().all(|s| valid_hash(s))
        || !m.game_versions.iter().all(|v| valid_hash(&v.exe_sha256) && valid_hash(&v.data_sha256) && !v.name.is_empty())
    {
        return Err("Invalid package size or checksum".into());
    }
    let expected = format!(
        "{REPO}/releases/download/v{}/FOCMP-{}.zip",
        m.version, m.version
    );
    if m.package.url != expected {
        return Err("Package URL is not an official FOCMP release asset".into());
    }
    Ok(m)
}
fn get(url: &str, limit: u64) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| format!("Download unavailable: {e}. The bundled version works offline; private GitHub releases require repository access."))?;
    let mut data = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() as u64 > limit {
        return Err("Download exceeds its allowed size".into());
    }
    Ok(data)
}
pub fn latest() -> Result<Manifest> {
    parse(&get(
        &format!("{REPO}/releases/latest/download/manifest.json"),
        65536,
    )?)
}
pub fn package(m: &Manifest, local: Option<&Path>) -> Result<Vec<u8>> {
    let data = if let Some(p) = local {
        if fs::metadata(p).map_err(|e| e.to_string())?.len() != m.package.size {
            return Err("Package size mismatch".into());
        }
        fs::read(p).map_err(|e| e.to_string())?
    } else if hash(EMBEDDED_ZIP) == m.package.sha256 {
        EMBEDDED_ZIP.to_vec()
    } else {
        get(&m.package.url, m.package.size)?
    };
    if data.len() as u64 != m.package.size || hash(&data) != m.package.sha256 {
        return Err("Package checksum or size mismatch".into());
    }
    Ok(data)
}
