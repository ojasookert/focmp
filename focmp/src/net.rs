//! Co-op networking: star topology, up to 3 players (host = peer 0).
//!
//! Transport for now: direct TCP (`host` listens, clients connect). Frames are
//! `u32 length (LE) + bitcode(Packet)`, max 1 MiB. The host relays every client
//! message to the other clients. Later a Steam transport (lobbies +
//! SteamNetworkingMessages through the game's steam_api64.dll) slots in behind
//! the same `Net` interface.
//!
//! Threads: one accept thread (host), one reader + one writer thread per
//! connection. The game's main thread only touches `Net` through `poll()` and
//! `send()`, which never block on the network.

use bitcode::{Decode, Encode};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const PROTOCOL: u32 = 28;
pub const MAX_PLAYERS: usize = 3;
pub const DEFAULT_PORT: u16 = 47900;
const MAX_FRAME: u32 = 1 << 20;

pub type PeerId = u8;

#[derive(Encode, Decode, Clone, Debug, PartialEq)]
pub struct PeerInfo {
    pub id: PeerId,
    pub name: String,
}

#[derive(Encode,Decode,Clone,Debug,Default,PartialEq)]
pub struct AvatarPart {
    pub sprite:String,pub frame:f32,pub x:f32,pub y:f32,pub xs:f32,pub ys:f32,pub angle:f32,pub blend:u32,pub alpha:f32,pub pass:u8,
}
impl AvatarPart {
    fn sane(&self)->bool{self.sprite.len()<128&&self.sprite.starts_with("s_")&&self.pass<8&&[self.frame,self.x,self.y,self.xs,self.ys,self.angle,self.alpha].iter().all(|x|x.is_finite()&&x.abs()<1.0e7)}
}

/// What a player looks like right now (sent ~30 Hz).
#[derive(Encode, Decode, Clone, Debug, Default, PartialEq)]
pub struct PlayerState {
    pub avatar: Vec<AvatarPart>,
    pub weapon: Vec<AvatarPart>,
    pub lights: Vec<crate::coop::lights::Light>,
    pub room: String,
    pub x: f32,
    pub y: f32,
    pub depth: f32,
    pub body_sprite: String,
    pub body_frame: f32,
    pub body_angle: f32,
    pub body_xscale: f32,
    pub body_yscale: f32,
    pub legs_sprite: String,
    pub legs_frame: f32,
    pub legs_angle: f32,
    pub gun_sprite: String,
    pub gun_frame: f32,
    pub gun_angle: f32,
    pub gun_x: f32,
    pub gun_y: f32,
    pub gun_yscale: f32,
    pub gun_xscale:f32,
    pub gun_alpha:f32,
    pub gun_depth: f32,
    pub gun_reloading: bool,
    pub hp: f32,
    pub hp_max: f32,
    pub dead: bool,
    pub can_revive: bool,
}

impl PlayerState {
    /// Reject what a well-behaved peer would never send: non-finite numbers,
    /// absurd coordinates, overlong strings.
    pub fn sane(&self) -> bool {
        let nums = [
            self.x, self.y, self.depth, self.body_frame, self.body_angle, self.body_xscale, self.body_yscale,
            self.legs_frame, self.legs_angle, self.gun_frame, self.gun_angle, self.gun_x, self.gun_y,
            self.gun_yscale, self.gun_xscale, self.gun_alpha, self.gun_depth, self.hp, self.hp_max,
        ];
        let strs = [&self.room, &self.body_sprite, &self.legs_sprite, &self.gun_sprite];
        self.avatar.len()<=32 && self.avatar.iter().all(AvatarPart::sane) && self.weapon.len()<=32 && self.weapon.iter().all(AvatarPart::sane) && self.lights.len()<=6 && self.lights.iter().all(crate::coop::lights::Light::sane) && nums.iter().all(|v| v.is_finite() && v.abs() < 1.0e7)
            && strs.iter().all(|s| s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    }
}

#[derive(Encode, Decode, Clone, Debug)]
pub enum Msg {
    PropInteract {committed:bool,action:crate::coop::props::Interact},
    PropHit { committed:bool, hit:crate::coop::props::Hit },
    /// Everything a peer has to say for one network tick (30 Hz), coalesced.
    Tick(Tick),
    ShotDefinitionRequest {run:u64,owner:PeerId,id:u64},
    ShotDefinition {run:u64,id:u64,graph:crate::coop::shot_graph::Graph},
    PurchaseRequest {run:u64,id:u64,cost:f64,room:String,level:u32,sub:u32},
    PurchaseResult {run:u64,peer:PeerId,id:u64,ok:bool},
    PurchaseDone {run:u64,id:u64,applied:bool},
    TeamGameOver { run:u64 },
    Ping(u64),
    Pong(u64),
    /// Host started a co-op run with this run seed (rooms.rs).
    StartRun { seed: u64, rules: crate::config::RunRules },
    SetReady(bool),
    SetColor(u8),
    Lobby { ready: [bool; MAX_PLAYERS], colors: [u8; MAX_PLAYERS], running: bool },
    DifficultyLoaded { run: u64, unlocked: u8, initial: u8 },
    DifficultyEdit { run: u64, revision: u32, index: u8, enabled: bool },
    DifficultyAgree { run: u64, revision: u32, agree: bool },
    DifficultyState { run: u64, state: DifficultySelection },
    DifficultyCommit { run: u64, revision: u32, mask: u8 },
    MenuCursor { run: u64, menu: u8, x: u16, y: u16, visible: bool },
    SelectionUpdate { run: u64, stage: u8, player: SelectionPlayer },
    SelectionState { run: u64, state: SelectionState },
    SelectionAck { run: u64, stage: u8, serial: u32 },
    SelectionCommit { run: u64, stage: u8, serial: u32 },
    TravelRequest { run: u64, epoch: u32, room: String, level: u32, sub: u32, door: i16 },
    TravelCommit { run: u64, epoch: u32, room: String, level: u32, sub: u32, door: i16 },
    ComputerEntered { run: u64, visit: u32 },
    ComputerRequest { run: u64, visit: u32, revision: u32, proceed: bool },
    ComputerAction { run: u64, visit: u32, revision: u32, proceed: bool },
    /// Hash of the fight-room layout a peer generated for (level, sub).
    Layout { level: u32, sub: u32, hash: u64 },
}

impl Msg {
    pub(crate) fn valid_from(&self, from: PeerId) -> bool {
        if from as usize >= MAX_PLAYERS { return false; }
        match self {
            Self::PropInteract{committed,action} => (!committed || from==0) && action.sane(),
            Self::PropHit{committed,hit} => (!committed || from==0) && hit.sane(),
            Self::ShotDefinitionRequest{run,owner,..}=>*run!=0&&(*owner as usize)<MAX_PLAYERS,
            Self::ShotDefinition{run,graph,..}=>*run!=0&&graph.sane(),
            Self::PurchaseRequest{run,id,cost,room,..}=>*run!=0&&*id!=0&&cost.is_finite()&&(0.0..1e9).contains(cost)&&valid_room(room),
            Self::PurchaseResult{run,peer,id,..}=>from==0&&*run!=0&&*id!=0&&(*peer as usize)<MAX_PLAYERS,
            Self::PurchaseDone{run,id,..}=>*run!=0&&*id!=0,
            Self::TeamGameOver{run} => from==0&&*run!=0,
            Self::StartRun { seed, rules } => from == 0 && *seed != 0 && rules.valid(),
            Self::Lobby { colors, .. } => from == 0 && colors.iter().all(|c| *c < 8),
            Self::SetColor(color) => *color < 8,
            Self::DifficultyLoaded { run, unlocked, initial } => *run != 0 && *unlocked < 128 && *initial < 128,
            Self::DifficultyEdit { run, index, .. } => *run != 0 && *index < 7,
            Self::DifficultyAgree { run, .. } => *run != 0,
            Self::DifficultyState { run, state } => from == 0 && *run != 0 && state.mask < 128 && state.allowed < 128 && state.mask & !state.allowed == 0,
            Self::DifficultyCommit { run, mask, .. } => from == 0 && *run != 0 && *mask < 128,
            Self::MenuCursor { run, menu, x, y, .. } => *run != 0 && *menu <= 3 && *x <= 640 && *y <= 360,
            Self::SelectionUpdate { run, stage, player } => *run != 0 && (1..=2).contains(stage) && player.sane_for(*stage),
            Self::SelectionState { run, state } => from == 0 && *run != 0 && (1..=2).contains(&state.stage) && state.players.iter().all(|p| p.sane_for(state.stage)),
            Self::SelectionAck { run, stage, .. } => *run != 0 && (1..=2).contains(stage),
            Self::SelectionCommit { run, stage, .. } => from == 0 && *run != 0 && (1..=2).contains(stage),
            Self::TravelRequest { run, room, door, .. } => *run != 0 && valid_room(room) && (-2..64).contains(door),
            Self::TravelCommit { run, room, door, .. } => from == 0 && *run != 0 && valid_room(room) && (-2..64).contains(door),
            Self::ComputerEntered { run, visit } | Self::ComputerRequest { run, visit, .. } => *run != 0 && *visit != 0,
            Self::ComputerAction { run, visit, revision, .. } => from == 0 && *run != 0 && *visit != 0 && *revision != 0,
            Self::Tick(t) => t.props.as_ref().is_none_or(|s|from==0&&s.sane()) && valid_room(&t.room) && t.boss.as_ref().is_none_or(|b|from==0&&b.sane()) && t.economy.as_ref().is_none_or(|s|from==0&&s.sane()) && t.state.as_ref().is_none_or(PlayerState::sane)
                && t.projectiles.as_ref().is_none_or(ShotBatch::sane)
                && t.hits.len() <= 4096 && t.fires.len() <= 4096
                && t.fires.iter().all(EnemyFire::sane)
                && t.melee.len() <= 4096 && t.melee.iter().all(crate::coop::enemy_melee::Attack::sane)
                && t.map_lights.as_ref().is_none_or(|s|from==0&&s.sane())
                && t.player_numbers.len()<=4096 && t.player_numbers.iter().all(crate::coop::damage_numbers::Number::sane)
                && t.numbers.len() <= 4096 && t.numbers.iter().all(crate::coop::damage_numbers::Number::sane)
                && t.hits.iter().all(EnemyHit::sane)
                && (from == 0 || (t.fires.is_empty() && t.melee.is_empty() && t.numbers.is_empty() && t.enemies.is_none()))
                && t.enemies.as_ref().is_none_or(|d| valid_room(&d.room) && d.changed.len() <= 4096 && d.gone.len() <= 4096 && d.deaths.len()<=4096 && d.deaths.iter().all(|e| e.gid!=0 && [e.x,e.y,e.angle,e.vx,e.vy,e.kx,e.ky].iter().all(|v|v.is_finite()&&v.abs()<1.0e7))
                    && d.changed.iter().all(|q| q.gid != 0 && q.status.sane() && q.hp.is_finite() && q.hp.abs() < 1.0e7 && q.hp_start.is_finite() && (0.0..1.0e7).contains(&q.hp_start))),
            _ => true,
        }
    }
}

fn valid_room(room: &str) -> bool {
    room.starts_with("Room_") && room.len() <= 64 && room.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The host serializes edits. Confirmations always refer to this exact revision.
#[derive(Encode, Decode, Clone, Debug, Default)]
pub struct DifficultySelection {
    pub revision: u32,
    pub mask: u8,
    pub allowed: u8,
    pub loaded: [bool; MAX_PLAYERS],
    pub agreed: [bool; MAX_PLAYERS],
}

#[derive(Encode, Decode, Clone, Debug, Default, PartialEq)]
pub struct SelectionPlayer {
    pub version: u32,
    pub loaded: bool,
    pub choices: [String; 2],
    pub ready: bool,
}
impl SelectionPlayer {
    fn sane_for(&self, stage: u8) -> bool {
        self.choices.iter().all(|s| s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
            && (!self.ready || (self.loaded && !self.choices[0].is_empty() && (stage == 1 || !self.choices[1].is_empty())))
    }
}

#[derive(Encode, Decode, Clone, Debug, Default)]
pub struct SelectionState {
    pub stage: u8,
    pub serial: u32,
    pub players: [SelectionPlayer; MAX_PLAYERS],
    pub preparing: bool,
}

/// One network tick from one peer. Empty parts cost about a byte each.
#[derive(Encode, Decode, Clone, Debug, Default)]
pub struct Tick {
    pub props:Option<crate::coop::props::Snapshot>,
    pub room:String,
    pub busy:bool,
    pub boss:Option<crate::coop::rooms::BossState>,
    pub economy:Option<crate::coop::economy::State>,

    /// Our player (None = no player in a room).
    pub state: Option<PlayerState>,
    pub projectiles: Option<ShotBatch>,
    /// Client → host: original individual hits, before native enemy modifiers.
    pub hits: Vec<EnemyHit>,
    /// Host → clients: enemy guns that fired this tick.
    pub fires: Vec<EnemyFire>,
    /// Host → clients: native enemy melee attacks, once per hitbox.
    pub melee: Vec<crate::coop::enemy_melee::Attack>,
    /// Host → clients: native enemy damage-number presentation.
    pub numbers: Vec<crate::coop::damage_numbers::Number>,
    /// Every player owns presentation of damage taken by their local player.
    pub player_numbers: Vec<crate::coop::damage_numbers::Number>,
    pub map_lights: Option<crate::coop::map_lights::Snapshot>,
    /// Host → clients: enemy changes.
    pub enemies: Option<EnemyDelta>,
}

#[derive(Encode, Decode, Clone, Debug)]
pub struct EnemyHit {
    pub gid:u16, pub room:String, pub level:u32, pub sub:u32,
    pub amount:f64, pub x:f64, pub y:f64, pub kx:f64, pub ky:f64,
    pub dtype:u8, pub scale:f64, pub flag:bool, pub extra:Option<f64>,
}
impl EnemyHit {
    pub fn sane(&self)->bool {
        self.gid!=0 && valid_room(&self.room) && self.amount>=0.0 && self.amount<1.0e6
            && [self.amount,self.x,self.y,self.kx,self.ky,self.scale].iter().all(|v|v.is_finite()&&v.abs()<1.0e7)
            && self.extra.is_none_or(|v|v.is_finite()&&v.abs()<1.0e7)
    }
}

#[derive(Encode, Decode, Clone, Debug)]
pub struct ShotFire {
    pub enemy: bool,
    pub id:u32, pub definition: u64, pub x:f64, pub y:f64, pub angle:f64, pub flag:bool,
    pub multiplier:f64, pub rng:crate::coop::rng::State, pub target:u16, pub weapon:String,
    pub collision_objects:Vec<String>, pub collision_tiles:bool,
}
#[derive(Encode, Decode, Clone, Debug)]
pub struct ShotBatch {
    pub run:u64,
    pub room:String, pub level:u32, pub sub:u32, pub seq:u32,
    pub definitions:Vec<(u64,crate::coop::shot_graph::Graph)>, pub fires:Vec<ShotFire>,
}
impl ShotBatch {
    fn sane(&self)->bool {
        self.run!=0 && valid_room(&self.room) && self.definitions.len()<=128 && self.fires.len()<=512
            && self.definitions.iter().all(|(_,g)|g.sane())
            && self.fires.iter().all(|f|f.id!=0 && f.rng.sane() && f.weapon.len()<128 && f.collision_objects.len()<=32 && f.collision_objects.iter().all(|s|s.starts_with("obj")&&s.len()<128) && [f.x,f.y,f.angle,f.multiplier].iter().all(|x|x.is_finite()&&x.abs()<1.0e7))
    }
}

/// One actual invocation of an enemy gun's fire method, in firing order.
#[derive(Encode, Decode, Clone, Debug)]
pub struct EnemyFire {
    pub tip_x:f32, pub tip_y:f32, pub recoil_x:f32, pub recoil_y:f32,
    pub room: String,
    pub gid: u16,
    pub level: u16,
    pub sub: u16,
    pub x: f32,
    pub y: f32,
    pub angle: f32,
    pub spread: f32,
    pub shot: u16,
    pub magazine: f32,
    pub id:u32,
    pub rng:crate::coop::rng::State,
}
impl EnemyFire {
    fn sane(&self) -> bool {
        valid_room(&self.room) && self.id!=0 && self.rng.sane() && self.gid != 0 && self.shot < 1024
            && [self.x, self.y, self.angle, self.spread, self.magazine, self.tip_x,self.tip_y,self.recoil_x,self.recoil_y].iter().all(|x| x.is_finite() && x.abs() < 1.0e7)
    }
}

#[derive(Encode, Decode, Clone, Debug)]
pub struct EnemyDeath { pub gid:u16, pub x:f32, pub y:f32, pub angle:f32, pub vx:f32, pub vy:f32, pub kx:f32, pub ky:f32 }

/// Enemy changes since the previous tick. `keyframe` = full list (every ~1 s):
/// the receiver drops enemies that aren't in it.
#[derive(Encode, Decode, Clone, Debug, Default)]
pub struct EnemyDelta {
    pub deaths: Vec<EnemyDeath>,
    pub room: String,
    pub complete: bool,
    pub level: u16,
    pub sub: u16,
    pub keyframe: bool,
    pub changed: Vec<EnemyQ>,
    pub gone: Vec<u16>,
}

/// Quantized enemy state (same game build on all peers, so asset ids are numeric).
#[derive(Encode, Decode, Clone, Debug, PartialEq, Default)]
pub struct EnemyQ {
    pub gid: u16,
    pub obj: i16,
    pub x: i16,
    pub y: i16,
    /// degrees / 360 * 256
    pub angle: u8,
    pub sprite: i16,
    pub frame: u8,
    pub flip: bool,
    pub hp: f32,
    pub hp_start: f32,
    pub dead: bool,
    pub onfire: bool,
    pub status:crate::coop::status_effects::Status,
    pub gun_angle: u8,
    pub gun_x: i16,
    pub gun_y: i16,
    pub gun_frame: u8,
    pub gun_flip: bool,
    pub gun_recoil_x:i16, pub gun_recoil_y:i16, pub gun_nozzle:bool, pub gun_nozzle_frame:u16,
    pub gun_sprite:i16, pub gun_len:u16,
}


pub fn q_angle(deg: f64) -> u8 {
    ((deg.rem_euclid(360.0) / 360.0 * 256.0).round() as u32 % 256) as u8
}

pub fn dq_angle(a: u8) -> f64 {
    a as f64 * 360.0 / 256.0
}

#[derive(Encode, Decode, Clone, Debug)]
pub(crate) enum Packet {
    Hello { protocol: u32, name: String },
    Welcome { your_id: PeerId, peers: Vec<PeerInfo> },
    Refused { reason: String },
    Joined(PeerInfo),
    Left(PeerId),
    Msg { from: PeerId, msg: Msg },
}

#[derive(Debug)]
pub enum Event {
    Connected { my_id: PeerId, peers: Vec<PeerInfo> },
    Joined(PeerInfo),
    Left(PeerId),
    Msg(PeerId, Msg),
    Failed(String),
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Host,
    Client,
}

struct Writer {
    tx: SyncSender<Vec<u8>>,
    socket: TcpStream,
    connection: u64,
}
impl Drop for Writer {
    fn drop(&mut self) { let _ = self.socket.shutdown(std::net::Shutdown::Both); }
}
type Writers = Arc<Mutex<HashMap<PeerId, Writer>>>;

#[derive(Default)]
pub(crate) struct Admission {
    pub(crate) peers: Vec<PeerInfo>,
    pub(crate) running: bool,
}

/// Frames queued per connection before disconnecting a stalled peer.
const SEND_QUEUE: usize = 256;

pub struct Net {
    steam: Option<Mutex<crate::steam::Steam>>,
    pub role: Role,
    pub my_id: PeerId,
    pub peers: Vec<PeerInfo>,
    pub addr: String,
    events: Receiver<Event>,
    writers: Writers,
    stop: Arc<AtomicBool>,
    admission: Arc<Mutex<Admission>>,
    forced_left: Mutex<Vec<PeerId>>,
}

/// Queue without blocking. Disconnect on overflow: gameplay events and
/// control messages must never be silently discarded.
fn queue(writer: &Writer, frame: Vec<u8>) {
    match writer.tx.try_send(frame) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(_)) => {
            // Ticks contain hits/fires as well as snapshots. Losing one silently
            // would change gameplay. Disconnect instead of pretending delivery.
            crate::log("net: stalled peer disconnected (send queue full)");
            let _ = writer.socket.shutdown(std::net::Shutdown::Both);
        },
    }
}

fn encode(p: &Packet) -> Vec<u8> {
    let body = bitcode::encode(p);
    let mut out = (body.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&body);
    out
}

fn read_packet(s: &mut TcpStream) -> std::io::Result<Packet> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        return Err(std::io::Error::other(format!("frame too large: {len}")));
    }
    let mut buf = vec![0u8; len as usize];
    s.read_exact(&mut buf)?;
    bitcode::decode(&buf).map_err(|e| std::io::Error::other(format!("bad packet: {e}")))
}

/// Writer thread: owns the write half, so a slow peer never blocks the game.
fn spawn_writer(mut s: TcpStream) -> std::io::Result<Writer> {
    static CONNECTION: AtomicU64 = AtomicU64::new(1);
    let socket = s.try_clone()?;
    let (tx, rx) = sync_channel::<Vec<u8>>(SEND_QUEUE);
    s.set_write_timeout(Some(Duration::from_secs(10))).ok();
    std::thread::spawn(move || {
        for frame in rx {
            if s.write_all(&frame).is_err() {
                break;
            }
        }
        let _ = s.shutdown(std::net::Shutdown::Both);
    });
    Ok(Writer { tx, socket, connection: CONNECTION.fetch_add(1, Ordering::Relaxed) })
}

impl Net {
    pub fn steam(role:Role,code:&str)->Result<Self,String>{
        let (_,events)=sync_channel(1);
        let admission=Arc::new(Mutex::new(Admission::default()));
        let steam=crate::steam::Steam::new(role,code,admission.clone())?;
        Ok(Self{steam:Some(Mutex::new(steam)),role,my_id:if role==Role::Host{0}else{255},peers:vec![],addr:"Steam lobby pending".into(),events,writers:Arc::new(Mutex::new(HashMap::new())),stop:Arc::new(AtomicBool::new(false)),admission,forced_left:Mutex::new(vec![])})
    }
    pub fn is_steam(&self)->bool{self.steam.is_some()}
    pub fn lobby_code(&self)->Option<String>{self.steam.as_ref().and_then(|s|{let s=s.lock().unwrap();(s.lobby!=0).then(||s.lobby.to_string())})}

    /// Start hosting on `port` (all interfaces).
    pub fn host(port: u16, name: String) -> Result<Net, String> {
        let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("bind :{port}: {e}"))?;
        let listener_port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let (ev_tx, events) = sync_channel(1024);
        let writers: Writers = Arc::new(Mutex::new(HashMap::new()));
        let admission = Arc::new(Mutex::new(Admission { peers: vec![PeerInfo { id: 0, name: name.clone() }], running: false }));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (writers, admission, stop, ev_tx) = (writers.clone(), admission.clone(), stop.clone(), ev_tx.clone());
            listener.set_nonblocking(true).ok();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((s, addr)) => {
                            s.set_nonblocking(false).ok();
                            let (writers, admission, stop, ev_tx) = (writers.clone(), admission.clone(), stop.clone(), ev_tx.clone());
                            std::thread::spawn(move || host_conn(s, addr, writers, admission, stop, ev_tx));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(50))
                        }
                        Err(e) => {
                            let _ = ev_tx.send(Event::Failed(format!("accept: {e}")));
                            break;
                        }
                    }
                }
            });
        }
        let _ = ev_tx.send(Event::Connected { my_id: 0, peers: vec![PeerInfo { id: 0, name }] });
        Ok(Net {
            steam: None,
            role: Role::Host,
            my_id: 0,
            peers: vec![],
            addr: format!("port {}", listener_port),
            events,
            writers,
            stop,
            admission,
            forced_left: Mutex::new(Vec::new()),
        })
    }

    /// Connect to a host. Connecting happens on a background thread; watch
    /// `poll()` for `Connected` or `Failed`.
    pub fn join(addr: &str, name: String) -> Net {
        let (ev_tx, events) = sync_channel(1024);
        let writers: Writers = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let a = addr.to_string();
        {
            let (writers, stop) = (writers.clone(), stop.clone());
            std::thread::spawn(move || {
                crate::log(&format!("net: client reader connecting to {a}"));
                let res = (|| -> Result<(), String> {
                    let sa: SocketAddr = a
                        .to_socket_addrs()
                        .map_err(|e| format!("resolve {a}: {e}"))?
                        .next()
                        .ok_or(format!("resolve {a}: no address"))?;
                    let mut s = TcpStream::connect_timeout(&sa, Duration::from_secs(5)).map_err(|e| format!("connect {a}: {e}"))?;
                    s.set_nodelay(true).ok();
                    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    s.set_write_timeout(Some(Duration::from_secs(5))).ok();
                    s.write_all(&encode(&Packet::Hello { protocol: PROTOCOL, name })).map_err(|e| e.to_string())?;
                    match read_packet(&mut s).map_err(|e| e.to_string())? {
                        Packet::Welcome { your_id, peers } => {
                            if stop.load(Ordering::Relaxed) { return Ok(()); }
                            s.set_read_timeout(None).ok();
                            let writer = spawn_writer(s.try_clone().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                            let mut w = writers.lock().unwrap();
                            if stop.load(Ordering::Relaxed) { return Ok(()); }
                            w.insert(0, writer);
                            drop(w);
                            let _ = ev_tx.send(Event::Connected { my_id: your_id, peers });
                            crate::log(&format!("net: client reader ready as peer {your_id}"));
                        }
                        Packet::Refused { reason } => return Err(reason),
                        other => return Err(format!("unexpected {other:?}")),
                    }
                    while !stop.load(Ordering::Relaxed) {
                        match read_packet(&mut s) {
                            Ok(Packet::Joined(p)) => drop(ev_tx.send(Event::Joined(p))),
                            Ok(Packet::Left(id)) => drop(ev_tx.send(Event::Left(id))),
                            Ok(Packet::Msg { from, msg }) if msg.valid_from(from) => {
                                if let Msg::StartRun { seed, .. } = &msg { crate::log(&format!("net: received run start {seed:016x} from peer {from}")); }
                                if ev_tx.try_send(Event::Msg(from, msg)).is_err() { break; }
                            }
                            Ok(Packet::Refused { reason }) => return Err(reason),
                            Ok(_) => {}
                            Err(e) => { crate::log(&format!("net: client reader stopped: {e}")); break; },
                        }
                    }
                    Ok(())
                })();
                crate::log(&format!("net: client reader exit: {res:?}"));
                match res {
                    Ok(()) => drop(ev_tx.send(Event::Closed)),
                    Err(e) => drop(ev_tx.send(Event::Failed(e))),
                }
            });
        }
        Net { steam: None, role: Role::Client, my_id: 255, peers: vec![], addr: addr.to_string(), events, writers, stop, admission: Arc::new(Mutex::new(Admission::default())), forced_left: Mutex::new(Vec::new()) }
    }

    /// Drain network events (main thread, non-blocking).
    pub fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        for id in std::mem::take(&mut *self.forced_left.lock().unwrap()) {
            self.peers.retain(|p| p.id != id);
            out.push(Event::Left(id));
        }
        let incoming=if let Some(steam)=&self.steam {let mut steam=steam.lock().unwrap();let events=steam.poll();if steam.lobby!=0{self.addr=format!("Steam lobby {}",steam.lobby);}events}else{self.events.try_iter().take(128).collect()};
        for e in incoming {
            match &e {
                Event::Connected { my_id, peers } => {
                    self.my_id = *my_id;
                    self.peers = peers.clone();
                }
                Event::Joined(p) => {
                    self.peers.retain(|q| q.id != p.id);
                    self.peers.push(p.clone());
                }
                Event::Left(id) => self.peers.retain(|q| q.id != *id),
                Event::Msg(from, _) if !self.peers.iter().any(|p| p.id == *from) => continue,
                _ => {}
            }
            out.push(e);
        }
        out
    }

    /// Send to everyone else (host: all clients; client: host relays).
    pub fn broadcast(&self, msg: Msg) {
        if let Some(steam)=&self.steam{steam.lock().unwrap().broadcast(self.my_id,msg);return;}
        let frame = encode(&Packet::Msg { from: self.my_id, msg });
        for tx in self.writers.lock().unwrap().values() {
            queue(tx, frame.clone());
        }
    }

    /// Close admission atomically with the roster snapshot. A concurrent Hello
    /// is either included here or refused; it cannot miss StartRun.
    pub fn begin_run(&self, force: bool, ready: &[bool; MAX_PLAYERS]) -> Result<Vec<PeerInfo>, String> {
        let mut a = self.admission.lock().unwrap();
        if a.running { return Err("run already started".into()); }
        if !force && a.peers.iter().any(|p| !ready[p.id as usize]) {
            return Err("players not ready".into());
        }
        a.running = true;
        let peers=a.peers.clone();drop(a);
        if let Some(steam)=&self.steam{steam.lock().unwrap().begin_run();}
        Ok(peers)
    }

    pub fn connected(&self) -> bool {
        if let Some(steam)=&self.steam{return steam.lock().unwrap().connected();}
        self.role == Role::Host || !self.writers.lock().unwrap().is_empty()
    }

    /// End a connection whose application-level heartbeat stopped arriving.
    /// Retire the roster entry immediately even if a native socket read stays
    /// blocked. Its connection token prevents late cleanup touching a new peer.
    pub fn disconnect(&self, peer: PeerId) {
        if let Some(steam)=&self.steam{steam.lock().unwrap().disconnect(peer);return;}
        let mut admission = self.admission.lock().unwrap();
        let mut writers = self.writers.lock().unwrap();
        if writers.remove(&peer).is_none() { return; }
        admission.peers.retain(|p| p.id != peer);
        if self.role == Role::Host {
            let left = encode(&Packet::Left(peer));
            for writer in writers.values() { queue(writer, left.clone()); }
            self.forced_left.lock().unwrap().push(peer);
        }
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.writers.lock().unwrap().clear(); // closes writer threads → sockets
    }
}

fn host_conn(
    mut s: TcpStream,
    addr: SocketAddr,
    writers: Writers,
    admission: Arc<Mutex<Admission>>,
    stop: Arc<AtomicBool>,
    ev_tx: SyncSender<Event>,
) {
    s.set_nodelay(true).ok();
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    s.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let name = match read_packet(&mut s) {
        Ok(Packet::Hello { protocol, name }) if protocol == PROTOCOL => name.chars().take(32).collect::<String>(),
        Ok(Packet::Hello { protocol, .. }) => {
            let _ = s.write_all(&encode(&Packet::Refused {
                reason: format!("version mismatch (host {PROTOCOL}, you {protocol})"),
            }));
            return;
        }
        _ => return,
    };
    s.set_read_timeout(None).ok();
    let (me, connection) = {
        let mut a = admission.lock().unwrap();
        if a.running || a.peers.len() >= MAX_PLAYERS || stop.load(Ordering::Relaxed) {
            let reason = if a.running { "run in progress" } else { "lobby full or closed" };
            let _ = s.write_all(&encode(&Packet::Refused { reason: reason.into() }));
            return;
        }
        let id = (1..MAX_PLAYERS as u8).find(|i| !a.peers.iter().any(|q| q.id == *i)).unwrap();
        let info = PeerInfo { id, name };
        let mut roster = a.peers.clone();
        roster.push(info.clone());
        if s.write_all(&encode(&Packet::Welcome { your_id: id, peers: roster })).is_err() { return; }
        let Ok(writer) = s.try_clone().and_then(spawn_writer) else { return };
        let connection = writer.connection;
        let mut w = writers.lock().unwrap();
        if stop.load(Ordering::Relaxed) { return; }
        let joined = encode(&Packet::Joined(info.clone()));
        for tx in w.values() { queue(tx, joined.clone()); }
        w.insert(id, writer);
        a.peers.push(info.clone());
        (info, connection)
    };
    crate::log(&format!("net: {} joined from {addr} as peer {}", me.name, me.id));
    let _ = ev_tx.send(Event::Joined(me.clone()));
    while !stop.load(Ordering::Relaxed) {
        match read_packet(&mut s) {
            Ok(Packet::Msg { msg, .. }) if msg.valid_from(me.id) => {
                // Relay to other clients with the sender's real id (never trust `from`).
                let frame = encode(&Packet::Msg { from: me.id, msg: msg.clone() });
                let w = writers.lock().unwrap();
                if !w.get(&me.id).is_some_and(|writer| writer.connection == connection) { break; }
                for (id, tx) in w.iter() {
                    if *id != me.id {
                        queue(tx, frame.clone());
                    }
                }
                if ev_tx.try_send(Event::Msg(me.id, msg)).is_err() { break; }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    {
        let mut a = admission.lock().unwrap();
        let mut w = writers.lock().unwrap();
        if !w.get(&me.id).is_some_and(|writer| writer.connection == connection) { return; }
        w.remove(&me.id);
        a.peers.retain(|q| q.id != me.id);
        let left = encode(&Packet::Left(me.id));
        for tx in w.values() { queue(tx, left.clone()); }
    }
    crate::log(&format!("net: peer {} ({}) left", me.id, me.name));
    let _ = ev_tx.send(Event::Left(me.id));
}

/// Local player name: FOCMP_NAME, else Steam persona name, else "player".
pub fn local_name() -> String {
    if let Ok(n) = std::env::var("FOCMP_NAME") {
        return n;
    }
    steam_persona().unwrap_or_else(|| "player".into())
}

#[cfg(not(windows))]
pub(crate) fn steam_persona() -> Option<String> { None }

#[cfg(windows)]
pub(crate) fn steam_persona() -> Option<String> {
    use std::ffi::{CStr, c_char, c_void};
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleA(name: *const u8) -> *mut c_void;
        fn GetProcAddress(m: *mut c_void, name: *const u8) -> *mut c_void;
    }
    unsafe {
        let m = GetModuleHandleA(c"steam_api64.dll".as_ptr() as *const u8);
        if m.is_null() {
            return None;
        }
        let friends_fn = GetProcAddress(m, c"SteamAPI_SteamFriends_v018".as_ptr() as *const u8);
        let name_fn = GetProcAddress(m, c"SteamAPI_ISteamFriends_GetPersonaName".as_ptr() as *const u8);
        if friends_fn.is_null() || name_fn.is_null() {
            return None;
        }
        let friends: extern "C" fn() -> *mut c_void = std::mem::transmute(friends_fn);
        let get_name: extern "C" fn(*mut c_void) -> *const c_char = std::mem::transmute(name_fn);
        let f = friends();
        if f.is_null() {
            return None;
        }
        let p = get_name(f);
        (!p.is_null()).then(|| CStr::from_ptr(p).to_string_lossy().into_owned())
    }
}
