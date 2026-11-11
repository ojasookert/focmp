//! Polled friend-lobby discovery. Does not register callbacks or join lobbies.
use super::Api;
use crate::net::{MAX_PLAYERS, PROTOCOL};
use std::{collections::{HashMap, HashSet}, time::{Duration, Instant}};

// SDK 1.63, FriendGameInfo_t, Windows x64 / pack(8). The bundled DLL matches
// that SDK's redistributable byte-for-byte (file version 10.24.16.75).
#[repr(C)]
#[derive(Default)]
struct FriendGameInfo {
    game_id: u64, game_ip: u32, game_port: u16, query_port: u16, lobby: u64,
}
const _: () = {
    assert!(std::mem::size_of::<FriendGameInfo>() == 24);
    assert!(std::mem::align_of::<FriendGameInfo>() == 8);
    assert!(std::mem::offset_of!(FriendGameInfo, lobby) == 16);
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Lobby {
    pub id: u64,
    pub owner: u64,
    pub name: String,
    pub players: u8,
}

#[derive(Default)]
pub(crate) struct Browser {
    refreshed: Option<Instant>,
    requested: HashMap<u64, Instant>,
    // None means a confirmed missing avatar. Pending loads are retried.
    avatars: HashMap<u64, Option<Vec<u8>>>,
}

impl Browser {
    pub fn pause(&mut self) { self.refreshed = None; }
    pub fn pixels(&self, id: u64) -> Option<&[u8]> { self.avatars.get(&id)?.as_deref() }

    /// Synthetic portraits for the opt-in development console preview only.
    pub fn preview_avatar(&mut self, id: u64) {
        if !crate::config::get().debug_console || self.avatars.contains_key(&id) { return; }
        let mut rgba = vec![255; 64 * 64 * 4];
        for y in 0..64 { for x in 0..64 {
            let p = (y * 64 + x) * 4;
            let value = ((x * 4 + y + id as usize * 17) % 256) as u8;
            rgba[p..p + 3].fill(value);
        }}
        self.avatars.insert(id, (id != 4).then(|| stylize(&rgba)));
    }

    pub fn poll(&mut self) -> Option<Vec<Lobby>> {
        if self.refreshed.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) { return None; }
        self.refreshed = Some(Instant::now());
        Some(self.refresh().unwrap_or_default())
    }

    fn refresh(&mut self) -> Result<Vec<Lobby>, String> {
        let api = Api::load()?;
        for symbol in [
            "SteamAPI_SteamFriends_v018", "SteamAPI_ISteamFriends_GetFriendCount",
            "SteamAPI_ISteamFriends_GetFriendByIndex", "SteamAPI_ISteamFriends_GetFriendGamePlayed",
            "SteamAPI_ISteamFriends_GetFriendPersonaName", "SteamAPI_ISteamMatchmaking_RequestLobbyData",
            "SteamAPI_ISteamUtils_GetAppID", "SteamAPI_ISteamFriends_GetMediumFriendAvatar",
            "SteamAPI_ISteamUtils_GetImageSize", "SteamAPI_ISteamUtils_GetImageRGBA",
        ] { api.symbol(symbol)?; }
        let friends = call!(api, "SteamAPI_SteamFriends_v018", unsafe extern "C" fn() -> usize,);
        if friends == 0 { return Err("Steam friends not ready".into()); }
        let app = call!(api, "SteamAPI_ISteamUtils_GetAppID", unsafe extern "C" fn(usize) -> u32, api.utils);
        if app == 0 { return Ok(vec![]); }
        let count = call!(api, "SteamAPI_ISteamFriends_GetFriendCount", unsafe extern "C" fn(usize, i32) -> i32, friends, 4).clamp(0, 500);
        let mut referenced = HashSet::new();
        let mut listed = HashSet::new();
        let mut lobbies = Vec::new();
        for index in 0..count {
            let id = call!(api, "SteamAPI_ISteamFriends_GetFriendByIndex", unsafe extern "C" fn(usize, i32, i32) -> u64, friends, index, 4);
            if id == 0 || id == api.me { continue; }
            let mut game = FriendGameInfo::default();
            if !call!(api, "SteamAPI_ISteamFriends_GetFriendGamePlayed", unsafe extern "C" fn(usize, u64, *mut FriendGameInfo) -> bool, friends, id, &mut game)
                || game.game_id != app as u64 || game.lobby == 0 { continue; }
            let lobby = game.lobby;
            referenced.insert(lobby);
            if self.requested.get(&lobby).is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
                call!(api, "SteamAPI_ISteamMatchmaking_RequestLobbyData", unsafe extern "C" fn(usize, u64) -> bool, api.mm, lobby);
                self.requested.insert(lobby, Instant::now());
            }
            // For non-member lobbies, owner and population come from lobby data.
            // Missing/unreceived metadata fails closed, including old hosts.
            if api.get(lobby, "focmp") != PROTOCOL.to_string() || api.get(lobby, "running") != "0"
                || api.get(lobby, "host").parse::<u64>().ok() != Some(id) { continue; }
            let Ok(players) = api.get(lobby, "players").parse::<u8>() else { continue };
            if players == 0 || players as usize >= MAX_PLAYERS || !listed.insert(lobby) { continue; }
            let name = call!(api, "SteamAPI_ISteamFriends_GetFriendPersonaName", unsafe extern "C" fn(usize, u64) -> *const i8, friends, id);
            let name = if name.is_null() { String::new() } else { unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy().into_owned() };
            lobbies.push(Lobby { id: lobby, owner: id, name: name.chars().filter(|c| !c.is_control() && *c != '#').take(14).collect(), players });
        }
        self.requested.retain(|id, _| referenced.contains(id));
        lobbies.sort_by(|a, b| b.players.cmp(&a.players).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then(a.id.cmp(&b.id)));
        for lobby in &lobbies {
            if !self.avatars.contains_key(&lobby.owner) {
                if let Some(pixels) = avatar(api, friends, lobby.owner) { self.avatars.insert(lobby.owner, pixels); }
            }
        }
        Ok(lobbies)
    }
}

fn avatar(api: Api, friends: usize, id: u64) -> Option<Option<Vec<u8>>> {
    let handle = call!(api, "SteamAPI_ISteamFriends_GetMediumFriendAvatar", unsafe extern "C" fn(usize, u64) -> i32, friends, id);
    if handle < 0 { return None; }
    if handle == 0 { return Some(None); }
    let (mut w, mut h) = (0, 0);
    if !call!(api, "SteamAPI_ISteamUtils_GetImageSize", unsafe extern "C" fn(usize, i32, *mut u32, *mut u32) -> bool, api.utils, handle, &mut w, &mut h) { return None; }
    if (w, h) != (64, 64) { return Some(None); }
    let mut pixels = vec![0u8; 64 * 64 * 4];
    if !call!(api, "SteamAPI_ISteamUtils_GetImageRGBA", unsafe extern "C" fn(usize, i32, *mut u8, i32) -> bool, api.utils, handle, pixels.as_mut_ptr(), pixels.len() as i32) { return None; }
    Some(Some(stylize(&pixels)))
}

fn stylize(rgba: &[u8]) -> Vec<u8> {
    let ramp = [[8u8, 14, 17], [30, 90, 95], [102, 224, 210], [255, 60, 150]];
    let mut out = vec![0u8; 32 * 32 * 4];
    for y in 0..16 {
        for x in 0..16 {
            let mut sum = [0u32; 4];
            for dy in 0..4 { for dx in 0..4 {
                let p = ((y * 4 + dy) * 64 + x * 4 + dx) * 4;
                for c in 0..4 { sum[c] += rgba[p + c] as u32; }
            }}
            let luminance = (2126 * sum[0] + 7152 * sum[1] + 722 * sum[2]) / (10000 * 16);
            let color = ramp[(luminance / 64).min(3) as usize];
            for dy in 0..2 { for dx in 0..2 {
                let p = ((y * 2 + dy) * 32 + x * 2 + dx) * 4;
                for c in 0..3 { out[p + c] = if dy == 1 { (color[c] as u16 * 3 / 4) as u8 } else { color[c] }; }
                out[p + 3] = (sum[3] / 16) as u8;
            }}
        }
    }
    out
}
