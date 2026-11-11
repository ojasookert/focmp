//! Steam lobbies + reliable NetworkingMessages using the game's existing API.
//! ABI: Valve steamnetworkingtypes.h (Identity=136 bytes, message identity offset=16).
//! We poll only our own async call handles; never drain the game's callback queue.
use crate::net::{Admission, Event, MAX_PLAYERS, Msg, PROTOCOL, Packet, PeerId, PeerInfo, Role};
use std::{
    collections::{HashMap, HashSet},
    ffi::{CString, c_void},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
const CHANNEL: i32 = 23673;
mod codec;
mod pressure;
mod outbox;
mod snapshots;
use outbox::{Outbox, Progress, CHUNK};
#[repr(C)]
struct Identity {
    kind: i32,
    size: i32,
    data: [u64; 16],
}
impl Identity {
    fn new(id: u64) -> Self {
        let mut v = Self {
            kind: 16,
            size: 8,
            data: [0; 16],
        };
        v.data[0] = id;
        v
    }
}
#[repr(C)]
struct Message {
    data: *mut u8,
    size: i32,
    connection: u32,
    identity: Identity,
}
#[derive(Clone, Copy)]
struct Api {
    mm: usize,
    utils: usize,
    messages: usize,
    me: u64,
    module: usize,
}
unsafe impl Send for Api {}
macro_rules! call{($a:expr,$name:literal,$sig:ty,$($arg:expr),*)=>{{let f:$sig=unsafe{std::mem::transmute($a.symbol($name).expect(concat!("checked Steam export ",$name)))};unsafe{f($($arg),*)}}}}
impl Api {
    fn symbol(self, name: &str) -> Result<usize, String> {
        let n = CString::new(name).unwrap();
        let p = unsafe { GetProcAddress(self.module as *mut c_void, n.as_ptr() as *const u8) };
        if p.is_null() {
            Err(format!("Steam export missing: {name}"))
        } else {
            Ok(p as usize)
        }
    }
    fn load() -> Result<Self, String> {
        let module = unsafe { GetModuleHandleA(c"steam_api64.dll".as_ptr() as *const u8) } as usize;
        if module == 0 {
            return Err("Steam is not running in this game".into());
        }
        let mut a = Self {
            module,
            mm: 0,
            utils: 0,
            messages: 0,
            me: 0,
        };
        for name in [
            "SteamAPI_SteamMatchmaking_v009",
            "SteamAPI_SteamUtils_v010",
            "SteamAPI_SteamNetworkingMessages_SteamAPI_v002",
            "SteamAPI_SteamUser_v023",
            "SteamAPI_ISteamUser_GetSteamID",
            "SteamAPI_ISteamMatchmaking_CreateLobby",
            "SteamAPI_ISteamMatchmaking_JoinLobby",
            "SteamAPI_ISteamMatchmaking_LeaveLobby",
            "SteamAPI_ISteamMatchmaking_GetLobbyOwner",
            "SteamAPI_ISteamMatchmaking_GetNumLobbyMembers",
            "SteamAPI_ISteamMatchmaking_GetLobbyMemberByIndex",
            "SteamAPI_ISteamMatchmaking_SetLobbyData",
            "SteamAPI_ISteamMatchmaking_GetLobbyData",
            "SteamAPI_ISteamMatchmaking_SetLobbyJoinable",
            "SteamAPI_ISteamUtils_IsAPICallCompleted",
            "SteamAPI_ISteamUtils_GetAPICallResult",
            "SteamAPI_ISteamNetworkingMessages_AcceptSessionWithUser",
            "SteamAPI_ISteamNetworkingMessages_CloseSessionWithUser",
            "SteamAPI_ISteamNetworkingMessages_SendMessageToUser",
            "SteamAPI_ISteamNetworkingMessages_ReceiveMessagesOnChannel",
            "SteamAPI_ISteamNetworkingMessages_GetSessionConnectionInfo",
            "SteamAPI_SteamNetworkingMessage_t_Release",
        ] {
            a.symbol(name)?;
        }
        a.mm = call!(
            a,
            "SteamAPI_SteamMatchmaking_v009",
            unsafe extern "C" fn() -> usize,
        );
        a.utils = call!(
            a,
            "SteamAPI_SteamUtils_v010",
            unsafe extern "C" fn() -> usize,
        );
        a.messages = call!(
            a,
            "SteamAPI_SteamNetworkingMessages_SteamAPI_v002",
            unsafe extern "C" fn() -> usize,
        );
        let user = call!(
            a,
            "SteamAPI_SteamUser_v023",
            unsafe extern "C" fn() -> usize,
        );
        if [a.mm, a.utils, a.messages, user].contains(&0) {
            return Err("Steam interfaces unavailable; launch the game through Steam".into());
        }
        a.me = call!(
            a,
            "SteamAPI_ISteamUser_GetSteamID",
            unsafe extern "C" fn(usize) -> u64,
            user
        );
        if a.me == 0 {
            return Err("Steam account unavailable".into());
        }
        Ok(a)
    }
    fn data(self, lobby: u64, key: &str, value: &str) {
        let (k, v) = (CString::new(key).unwrap(), CString::new(value).unwrap());
        call!(
            self,
            "SteamAPI_ISteamMatchmaking_SetLobbyData",
            unsafe extern "C" fn(usize, u64, *const i8, *const i8) -> bool,
            self.mm,
            lobby,
            k.as_ptr(),
            v.as_ptr()
        );
    }
    fn get(self, lobby: u64, key: &str) -> String {
        let k = CString::new(key).unwrap();
        let p = call!(
            self,
            "SteamAPI_ISteamMatchmaking_GetLobbyData",
            unsafe extern "C" fn(usize, u64, *const i8) -> *const i8,
            self.mm,
            lobby,
            k.as_ptr()
        );
        if p.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(p) }
                .to_string_lossy()
                .into_owned()
        }
    }
    fn owner(self, lobby: u64) -> u64 {
        call!(
            self,
            "SteamAPI_ISteamMatchmaking_GetLobbyOwner",
            unsafe extern "C" fn(usize, u64) -> u64,
            self.mm,
            lobby
        )
    }
    fn close(self, id: u64) {
        call!(
            self,
            "SteamAPI_ISteamNetworkingMessages_CloseSessionWithUser",
            unsafe extern "C" fn(usize, *const Identity) -> bool,
            self.messages,
            &Identity::new(id)
        );
    }
}
pub(crate) mod friends;

// Windows x64 CCallbackBase ABI. MSVC reverses the two Run overloads in
// its vtable: Run(data, failure, call), Run(data), GetCallbackSizeBytes.
// The callback stays registered for the DLL/process lifetime. The game pumps
// SteamAPI_RunCallbacks; we only enqueue an ID, never touch GameMaker here.
#[repr(C)]
struct InviteCallback {
    vtable: &'static InviteVtable,
    flags: u8,
    id: i32,
}
#[repr(C)]
struct InviteVtable {
    call_result: unsafe extern "C" fn(*mut InviteCallback, *const u64, bool, u64),
    run: unsafe extern "C" fn(*mut InviteCallback, *const u64),
    size: unsafe extern "C" fn(*mut InviteCallback) -> i32,
}
static INVITE: AtomicU64 = AtomicU64::new(0);
static INVITE_VTABLE: InviteVtable = InviteVtable {
    call_result: invite_result,
    run: invite_callback,
    size: invite_size,
};
unsafe extern "C" fn invite_callback(_: *mut InviteCallback, data: *const u64) {
    if !data.is_null() {
        let lobby = unsafe { data.read_unaligned() };
        if lobby != 0 {
            INVITE.store(lobby, Ordering::Release);
        }
    }
}
unsafe extern "C" fn invite_result(
    this: *mut InviteCallback,
    data: *const u64,
    failed: bool,
    _: u64,
) {
    if !failed {
        unsafe { invite_callback(this, data) };
    }
}
unsafe extern "C" fn invite_size(_: *mut InviteCallback) -> i32 {
    16
}

#[derive(Default)]
struct InviteListener {
    registered: bool,
    startup_read: bool,
    attempt: Option<Instant>,
}
static LISTENER: Mutex<InviteListener> = Mutex::new(InviteListener {
    registered: false,
    startup_read: false,
    attempt: None,
});
static STEAM_ENABLED: LazyLock<bool> =
    LazyLock::new(|| crate::config::get().transport == crate::config::Transport::Steam);
/// Called on the game thread once its startup services are ready.
pub fn poll_invite() -> Option<u64> {
    if !*STEAM_ENABLED {
        return None;
    }
    let mut listener = LISTENER.lock().unwrap();
    if !listener.startup_read {
        listener.startup_read = true;
        let mut args = std::env::args_os();
        while let Some(arg) = args.next() {
            if arg == "+connect_lobby" {
                if let Some(id) = args
                    .next()
                    .and_then(|v| v.to_string_lossy().parse::<u64>().ok())
                    .filter(|id| *id != 0)
                {
                    INVITE.store(id, Ordering::Release);
                }
            }
        }
    }
    if !listener.registered
        && listener
            .attempt
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(2))
    {
        let first = listener.attempt.is_none();
        listener.attempt = Some(Instant::now());
        let result = Api::load().and_then(|api| {
            let register: unsafe extern "C" fn(*mut InviteCallback, i32) =
                unsafe { std::mem::transmute(api.symbol("SteamAPI_RegisterCallback")?) };
            let callback = Box::into_raw(Box::new(InviteCallback {
                vtable: &INVITE_VTABLE,
                flags: 0,
                id: 333,
            }));
            unsafe { register(callback, 333) };
            Ok(())
        });
        match result {
            Ok(()) => {
                listener.registered = true;
                crate::log("Steam invites: listening for lobby join requests");
            }
            Err(e) if first => crate::log(&format!("Steam invites: waiting for API: {e}")),
            Err(_) => {}
        }
    }
    let id = INVITE.swap(0, Ordering::AcqRel);
    (id != 0).then_some(id)
}

pub fn invite_friends(lobby: u64) -> Result<(), String> {
    if lobby == 0 {
        return Err("The Steam lobby is not ready yet".into());
    }
    let api = Api::load()?;
    let friends: unsafe extern "C" fn() -> usize =
        unsafe { std::mem::transmute(api.symbol("SteamAPI_SteamFriends_v018")?) };
    let enabled: unsafe extern "C" fn(usize) -> bool =
        unsafe { std::mem::transmute(api.symbol("SteamAPI_ISteamUtils_IsOverlayEnabled")?) };
    let open: unsafe extern "C" fn(usize, u64) = unsafe {
        std::mem::transmute(api.symbol("SteamAPI_ISteamFriends_ActivateGameOverlayInviteDialog")?)
    };
    let friends = unsafe { friends() };
    if friends == 0 {
        return Err("Steam friends unavailable".into());
    }
    if !unsafe { enabled(api.utils) } {
        return Err("Enable the Steam overlay, or use Copy lobby code".into());
    }
    unsafe { open(friends, lobby) };
    Ok(())
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}
struct Partial {
    time: Instant,
    total: usize,
    parts: Vec<Option<Vec<u8>>>,
}
pub struct Steam {
    api: Api,
    role: Role,
    pub lobby: u64,
    owner: u64,
    handle: u64,
    started: Instant,
    hello: Option<Instant>,
    peers: HashMap<u64, PeerId>,
    allowed: HashSet<u64>,
    partial: HashMap<(u64, u64), Partial>,
    seq: u64,
    outboxes: HashMap<u64, Outbox>,
    decoders: HashMap<u64, codec::History>,
    pressure_log: HashMap<u64, Instant>,
    my_id: PeerId,
    name: String,
    events: Vec<Event>,
    admission: Arc<Mutex<Admission>>,
    failed: bool,
    published_members: Option<i32>,
}
impl Steam {
    pub fn new(role: Role, code: &str, admission: Arc<Mutex<Admission>>) -> Result<Self, String> {
        let api = Api::load()?;
        let name = crate::net::steam_persona().ok_or("Steam persona unavailable")?;
        let lobby = if role == Role::Client {
            code.trim()
                .trim_start_matches("steam:")
                .parse::<u64>()
                .map_err(|_| "Paste a Steam lobby code (the numeric ID)")?
        } else {
            0
        };
        let handle = if role == Role::Host {
            call!(
                api,
                "SteamAPI_ISteamMatchmaking_CreateLobby",
                unsafe extern "C" fn(usize, i32, i32) -> u64,
                api.mm,
                1,
                MAX_PLAYERS as i32
            )
        } else {
            call!(
                api,
                "SteamAPI_ISteamMatchmaking_JoinLobby",
                unsafe extern "C" fn(usize, u64) -> u64,
                api.mm,
                lobby
            )
        };
        if handle == 0 {
            return Err("Steam refused the lobby request".into());
        }
        Ok(Self {
            api,
            role,
            lobby,
            owner: 0,
            handle,
            started: Instant::now(),
            hello: None,
            peers: HashMap::new(),
            allowed: HashSet::new(),
            partial: HashMap::new(),
            seq: 0,
            outboxes: HashMap::new(),
            decoders: HashMap::new(),
            pressure_log: HashMap::new(),
            my_id: if role == Role::Host { 0 } else { 255 },
            name,
            events: vec![],
            admission,
            failed: false,
            published_members: None,
        })
    }
    /// Steam measures this below the game's frame-driven polling loop.
    pub fn ping(&self, peer: PeerId) -> Option<u16> {
        let (&id, _) = self.peers.iter().find(|(_, p)| **p == peer)?;
        let mut status = pressure::Status::default();
        let state = call!(self.api, "SteamAPI_ISteamNetworkingMessages_GetSessionConnectionInfo",
            unsafe extern "C" fn(usize, *const Identity, *mut c_void, *mut pressure::Status) -> i32,
            self.api.messages, &Identity::new(id), std::ptr::null_mut(), &mut status);
        (state == 3 && status.ping >= 0).then(|| status.ping.min(u16::MAX as i32) as u16)
    }

    pub fn connected(&self) -> bool {
        !self.failed
            && self.handle == 0
            && self.lobby != 0
            && (self.role == Role::Host || self.my_id < MAX_PLAYERS as u8)
    }
    fn fail(&mut self, reason: impl Into<String>) {
        if !self.failed {
            self.failed = true;
            self.events.push(Event::Failed(reason.into()));
        }
    }
    fn send(&mut self, to: u64, p: &Packet) {
        if self.failed { return; }
        self.seq = self.seq.wrapping_add(1);
        let data = snapshots::encode(p);
        let queue = self.outboxes.entry(to).or_default();
        let now = Instant::now();
        let replace = snapshots::key(p);
        let result = if replace.is_some() {
            queue.enqueue_with(self.seq, data, replace, now, snapshots::merge)
        } else {
            queue.enqueue(self.seq, data, None, now)
        };
        if let Err(e) = result { self.send_failed(to, e); }
    }
    fn send_failed(&mut self, to: u64, error: outbox::Error) {
        let detail = match error {
            outbox::Error::PacketSize(bytes) => format!("packet size {bytes} exceeds 1 MiB"),
            outbox::Error::Capacity => "outgoing queue reached its bounded capacity".into(),
            outbox::Error::Expired => "outgoing queue stalled for 15 seconds".into(),
            outbox::Error::Send(result) => format!("send failed ({result})"),
            outbox::Error::Codec(reason) => format!("packet codec failed: {reason}"),
        };
        let (bytes, packets) = self.outboxes.get(&to).map_or((0, 0), |q| (q.bytes(), q.len()));
        let reason = format!("Steam {detail}; peer {to}, lobby {}, queued {bytes} bytes / {packets} packets", self.lobby);
        crate::log(&reason);
        self.outboxes.remove(&to);
        self.decoders.remove(&to);
        self.pressure_log.remove(&to);
        self.partial.retain(|(sender, _), _| *sender != to);
        if self.role == Role::Host {
            // A slow/broken client must not tear down the host and healthy peer.
            if let Some(peer) = self.peers.get(&to).copied() { self.disconnect(peer); }
            else { self.api.close(to); }
        } else { self.fail(reason); }
    }
    fn flush_sends(&mut self) {
        let ids: Vec<_> = self.outboxes.keys().copied().collect();
        let api = self.api;
        let now = Instant::now();
        for id in ids {
            if self.failed { break; }
            let Some(queue) = self.outboxes.get_mut(&id) else { continue };
            let mut status = pressure::Status::default();
            let state = call!(api, "SteamAPI_ISteamNetworkingMessages_GetSessionConnectionInfo",
                unsafe extern "C" fn(usize, *const Identity, *mut c_void, *mut pressure::Status) -> i32,
                api.messages, &Identity::new(id), std::ptr::null_mut(), &mut status);
            let budget = if state == 3 { status.budget() } else { CHUNK + 20 };
            if (status.queue_us > 100_000 || (budget == 0 && queue.len() > 0))
                && self.pressure_log.get(&id).is_none_or(|t| now.duration_since(*t) >= Duration::from_secs(5)) {
                crate::log(&format!("Steam queue: peer {id}, pending {} bytes, unacked {}, delay {} ms, rate {} B/s, app {} bytes / {} batches, raw {} / wire {} bytes, compacted {}",
                    status.pending_reliable, status.unacked_reliable, status.queue_us / 1000, status.send_rate,
                    queue.bytes(), queue.len(), queue.raw_bytes, queue.wire_bytes, queue.replaced));
                self.pressure_log.insert(id, now);
            }
            let result = queue.flush_budget(now, budget, |bytes| call!(
                api,
                "SteamAPI_ISteamNetworkingMessages_SendMessageToUser",
                unsafe extern "C" fn(usize, *const Identity, *const u8, u32, i32, i32) -> i32,
                api.messages, &Identity::new(id), bytes.as_ptr(), bytes.len() as u32, 8, CHANNEL
            ));
            match result {
                Ok(Progress::Blocked) if self.pressure_log.get(&id)
                    .is_none_or(|t| now.duration_since(*t) >= Duration::from_secs(5)) => {
                    crate::log(&format!("Steam congestion: peer {id}, queued {} bytes / {} batches, pending {}, delay {} ms, retries {}, compacted {}",
                        queue.bytes(), queue.len(), status.pending_reliable, status.queue_us / 1000, queue.retries, queue.replaced));
                    self.pressure_log.insert(id, now);
                }
                Err(e) => self.send_failed(id, e),
                _ => {}
            }
        }
    }
    fn everyone(&mut self, p: &Packet, except: u64) {
        let ids: Vec<_> = self
            .peers
            .keys()
            .copied()
            .filter(|id| *id != except)
            .collect();
        for id in ids {
            self.send(id, p);
        }
    }
    pub fn broadcast(&mut self, from: PeerId, msg: Msg) {
        let p = Packet::Msg { from, msg };
        if self.role == Role::Host {
            self.everyone(&p, 0)
        } else if self.connected() {
            self.send(self.owner, &p)
        }
    }
    pub fn begin_run(&mut self) {
        call!(
            self.api,
            "SteamAPI_ISteamMatchmaking_SetLobbyJoinable",
            unsafe extern "C" fn(usize, u64, bool) -> bool,
            self.api.mm,
            self.lobby,
            false
        );
        self.api.data(self.lobby, "running", "1");
    }
    pub fn disconnect(&mut self, peer: PeerId) {
        if let Some(id) = self
            .peers
            .iter()
            .find_map(|(id, p)| (*p == peer).then_some(*id))
        {
            self.outboxes.remove(&id);
            self.decoders.remove(&id);
            self.pressure_log.remove(&id);
            self.partial.retain(|(sender, _), _| *sender != id);
            self.peers.remove(&id);
            self.api.close(id);
            self.admission
                .lock()
                .unwrap()
                .peers
                .retain(|p| p.id != peer);
            self.events.push(Event::Left(peer));
            if self.role == Role::Host {
                self.everyone(&Packet::Left(peer), 0);
            }
        }
    }
    fn packet(&mut self, sender: u64, p: Packet) {
        if self.role == Role::Host {
            match p {
                Packet::Hello { protocol, .. } => {
                    if protocol != PROTOCOL {
                        self.send(
                            sender,
                            &Packet::Refused {
                                reason: format!(
                                    "Mod protocol mismatch: host {PROTOCOL}, client {protocol}"
                                ),
                            },
                        );
                        return;
                    }
                    if let Some(&id) = self.peers.get(&sender) {
                        let peers = self.admission.lock().unwrap().peers.clone();
                        self.send(sender, &Packet::Welcome { your_id: id, peers });
                        return;
                    }
                    let mut a = self.admission.lock().unwrap();
                    if a.running || a.peers.len() >= MAX_PLAYERS {
                        drop(a);
                        self.send(
                            sender,
                            &Packet::Refused {
                                reason: "Lobby is full or the run already started".into(),
                            },
                        );
                        return;
                    }
                    let id = (1..MAX_PLAYERS as u8)
                        .find(|id| !a.peers.iter().any(|p| p.id == *id))
                        .unwrap();
                    let name = persona(sender).unwrap_or_else(|| "Steam player".into());
                    let peer = PeerInfo { id, name };
                    a.peers.push(peer.clone());
                    let peers = a.peers.clone();
                    drop(a);
                    self.everyone(&Packet::Joined(peer.clone()), sender);
                    self.peers.insert(sender, id);
                    self.send(sender, &Packet::Welcome { your_id: id, peers });
                    self.events.push(Event::Joined(peer));
                }
                Packet::Msg { msg, .. } => {
                    if let Some(&id) = self.peers.get(&sender) {
                        if msg.checked_from(id) {
                            self.everyone(
                                &Packet::Msg {
                                    from: id,
                                    msg: msg.clone(),
                                },
                                sender,
                            );
                            self.events.push(Event::Msg(id, msg));
                        }
                    }
                }
                _ => {}
            }
        } else if sender == self.owner {
            match p {
                Packet::Welcome { your_id, peers }
                    if self.my_id == 255
                        && your_id > 0
                        && (your_id as usize) < MAX_PLAYERS
                        && peers.len() <= MAX_PLAYERS
                        && peers.iter().all(|p| (p.id as usize) < MAX_PLAYERS) =>
                {
                    self.my_id = your_id;
                    self.peers.insert(sender, 0);
                    self.events.push(Event::Connected {
                        my_id: your_id,
                        peers,
                    });
                }
                Packet::Msg { from, msg } if msg.checked_from(from) => {
                    self.events.push(Event::Msg(from, msg))
                }
                Packet::Joined(p) if (p.id as usize) < MAX_PLAYERS => {
                    self.events.push(Event::Joined(p))
                }
                Packet::Left(id) if (id as usize) < MAX_PLAYERS => {
                    self.events.push(Event::Left(id))
                }
                Packet::Refused { reason } => self.fail(reason),
                _ => {}
            }
        }
    }
    pub fn poll(&mut self) -> Vec<Event> {
        if self.failed {
            return std::mem::take(&mut self.events);
        }
        if self.handle != 0 {
            let mut failed = false;
            let done = call!(
                self.api,
                "SteamAPI_ISteamUtils_IsAPICallCompleted",
                unsafe extern "C" fn(usize, u64, *mut bool) -> bool,
                self.api.utils,
                self.handle,
                &mut failed
            );
            if failed {
                self.fail("Steam lobby request failed");
            } else if done {
                let mut bytes = [0u64; 3];
                let (size, callback) = if self.role == Role::Host {
                    (16, 513)
                } else {
                    (24, 504)
                };
                let ok = call!(
                    self.api,
                    "SteamAPI_ISteamUtils_GetAPICallResult",
                    unsafe extern "C" fn(usize, u64, *mut c_void, i32, i32, *mut bool) -> bool,
                    self.api.utils,
                    self.handle,
                    bytes.as_mut_ptr() as *mut c_void,
                    size,
                    callback,
                    &mut failed
                );
                self.handle = 0;
                if !ok || failed {
                    self.fail("Could not read Steam lobby result");
                } else if self.role == Role::Host {
                    if bytes[0] as u32 != 1 {
                        self.fail(format!("Steam create lobby failed ({})", bytes[0] as u32));
                    } else {
                        self.lobby = bytes[1];
                        self.owner = self.api.me;
                        self.api.data(self.lobby, "focmp", &PROTOCOL.to_string());
                        self.api.data(self.lobby, "version", crate::version::BUILD);
                        self.api.data(self.lobby, "running", "0");
                        self.api.data(self.lobby, "host", &self.api.me.to_string());
                        self.api.data(self.lobby, "players", "1");
                        self.published_members = Some(1);
                        let me = PeerInfo {
                            id: 0,
                            name: self.name.clone(),
                        };
                        self.admission.lock().unwrap().peers = vec![me.clone()];
                        self.events.push(Event::Connected {
                            my_id: 0,
                            peers: vec![me],
                        });
                        crate::log(&format!("Steam lobby code: {}", self.lobby));
                    }
                } else if bytes[2] as u32 != 1 {
                    self.fail(format!("Steam join lobby failed ({})", bytes[2] as u32));
                } else {
                    self.lobby = bytes[0];
                    self.owner = self.api.owner(self.lobby);
                    if self.owner == self.api.me {
                        self.fail("Use distinct Steam accounts to join; use TCP for local testing");
                    } else if self.api.get(self.lobby, "focmp") != PROTOCOL.to_string() {
                        self.fail("Lobby is not running the same FOCMP protocol");
                    }
                }
            } else if self.started.elapsed() > Duration::from_secs(30) {
                self.fail("Steam lobby request timed out");
            }
        }
        if self.handle == 0 && !self.failed {
            if self.api.owner(self.lobby) != self.owner {
                self.fail("Steam host left the lobby");
                return std::mem::take(&mut self.events);
            }
            let n = call!(
                self.api,
                "SteamAPI_ISteamMatchmaking_GetNumLobbyMembers",
                unsafe extern "C" fn(usize, u64) -> i32,
                self.api.mm,
                self.lobby
            );
            let mut allowed = HashSet::new();
            if self.role == Role::Host && n >= 1 && self.published_members != Some(n) {
                self.api.data(self.lobby, "players", &n.to_string());
                self.published_members = Some(n);
            }
            for i in 0..n.clamp(0, MAX_PLAYERS as i32) {
                let id = call!(
                    self.api,
                    "SteamAPI_ISteamMatchmaking_GetLobbyMemberByIndex",
                    unsafe extern "C" fn(usize, u64, i32) -> u64,
                    self.api.mm,
                    self.lobby,
                    i
                );
                if id != 0 && id != self.api.me && (self.role == Role::Host || id == self.owner) {
                    allowed.insert(id);
                    call!(
                        self.api,
                        "SteamAPI_ISteamNetworkingMessages_AcceptSessionWithUser",
                        unsafe extern "C" fn(usize, *const Identity) -> bool,
                        self.api.messages,
                        &Identity::new(id)
                    );
                }
            }
            let left: Vec<_> = self
                .peers
                .iter()
                .filter_map(|(id, p)| (!allowed.contains(id)).then_some(*p))
                .collect();
            for id in left {
                self.disconnect(id);
            }
            self.outboxes.retain(|id, _| allowed.contains(id));
            self.allowed = allowed;
            if self.role == Role::Client && self.my_id == 255 {
                if self.started.elapsed() > Duration::from_secs(30) {
                    self.fail("Steam host handshake timed out");
                } else if self
                    .hello
                    .is_none_or(|t| t.elapsed() > Duration::from_secs(1))
                {
                    self.hello = Some(Instant::now());
                    self.send(
                        self.owner,
                        &Packet::Hello {
                            protocol: PROTOCOL,
                            name: self.name.clone(),
                        },
                    );
                }
            }
            self.partial
                .retain(|_, p| p.time.elapsed() < Duration::from_secs(30));
            let mut messages = [std::ptr::null_mut::<Message>(); 128];
            let count = call!(
                self.api,
                "SteamAPI_ISteamNetworkingMessages_ReceiveMessagesOnChannel",
                unsafe extern "C" fn(usize, i32, *mut *mut Message, i32) -> i32,
                self.api.messages,
                CHANNEL,
                messages.as_mut_ptr(),
                128
            );
            for ptr in messages.into_iter().take(count.clamp(0, 128) as usize) {
                if ptr.is_null() {
                    continue;
                }
                let m = unsafe { &*ptr };
                if m.identity.kind == 16
                    && m.identity.size == 8
                    && self.allowed.contains(&m.identity.data[0])
                    && m.size >= 20
                    && (m.size as usize) <= CHUNK + 20
                    && !m.data.is_null()
                {
                    let bytes = unsafe { std::slice::from_raw_parts(m.data, m.size as usize) };
                    if &bytes[..4] == b"FMP2" {
                        let id = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
                        let index = u16::from_le_bytes(bytes[12..14].try_into().unwrap()) as usize;
                        let count = u16::from_le_bytes(bytes[14..16].try_into().unwrap()) as usize;
                        let total = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
                        let sender = m.identity.data[0];
                        if total > 0
                            && total <= codec::WIRE_MAX
                            && count == total.div_ceil(CHUNK)
                            && index < count
                            && bytes.len() - 20
                                == if index + 1 == count {
                                    total - index * CHUNK
                                } else {
                                    CHUNK
                                }
                            && (self.partial.len() < 64 || self.partial.contains_key(&(sender, id)))
                        {
                            let entry =
                                self.partial.entry((sender, id)).or_insert_with(|| Partial {
                                    time: Instant::now(),
                                    total,
                                    parts: vec![None; count],
                                });
                            if entry.total == total && entry.parts.len() == count {
                                entry.time = Instant::now();
                                entry.parts[index] = Some(bytes[20..].to_vec());
                                if entry.parts.iter().all(Option::is_some) {
                                    let entry = self.partial.remove(&(sender, id)).unwrap();
                                    let data: Vec<u8> =
                                        entry.parts.into_iter().flatten().flatten().collect();
                                    let decoded = self.decoders.entry(sender).or_default().decode(id, &data)
                                        .and_then(|data| snapshots::decode(&data));
                                    match decoded {
                                        Ok(packets) => for packet in packets { self.packet(sender, packet); },
                                        Err(reason) => self.send_failed(sender, outbox::Error::Codec(reason)),
                                    }
                                }
                            }
                        }
                    }
                }
                call!(
                    self.api,
                    "SteamAPI_SteamNetworkingMessage_t_Release",
                    unsafe extern "C" fn(*mut Message),
                    ptr
                );
            }
        }
        if self.handle == 0 && !self.failed { self.flush_sends(); }
        std::mem::take(&mut self.events)
    }
}
impl Drop for Steam {
    fn drop(&mut self) {
        for id in &self.allowed {
            self.api.close(*id);
        }
        if self.lobby != 0 {
            call!(
                self.api,
                "SteamAPI_ISteamMatchmaking_LeaveLobby",
                unsafe extern "C" fn(usize, u64),
                self.api.mm,
                self.lobby
            );
        }
    }
}
fn persona(id: u64) -> Option<String> {
    let a = Api::load().ok()?;
    let p = a.symbol("SteamAPI_SteamFriends_v018").ok()?;
    let friends: unsafe extern "C" fn() -> usize = unsafe { std::mem::transmute(p) };
    let f = unsafe { friends() };
    if f == 0 {
        return None;
    }
    let p = a
        .symbol("SteamAPI_ISteamFriends_GetFriendPersonaName")
        .ok()?;
    let get: unsafe extern "C" fn(usize, u64) -> *const i8 = unsafe { std::mem::transmute(p) };
    let name = unsafe { get(f, id) };
    if name.is_null() {
        None
    } else {
        Some(
            unsafe { std::ffi::CStr::from_ptr(name) }
                .to_string_lossy()
                .chars()
                .take(32)
                .collect(),
        )
    }
}
