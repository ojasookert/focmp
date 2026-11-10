//! Co-op session: networking state, per-frame tick, and the systems that run on it.
//!
//! Everything here runs on the game's main thread (from the PeekMessageW pump,
//! at most once per ~15 ms), so GML calls are safe.

pub mod ai;
pub(crate) mod boss_contact;
pub(crate) mod objective;
pub(crate) mod ending;
pub(crate) mod controls;
pub mod pedestal;
pub mod colors;
pub mod computer;
pub mod cursors;
pub mod difficulty;
pub mod enemies;
pub mod ghosts;
pub mod menu;
pub mod rooms;
pub mod projectiles;
pub mod shot_graph;
mod visual_damage;
mod avatars;
pub mod lights;
pub mod economy;
pub(crate) mod status_effects;
mod pause;
mod client_hits;
pub(crate) mod enemy_melee;
pub(crate) mod damage_numbers;
mod fuzz;
pub(crate) mod rng;
mod shot_events;
pub(crate) mod map_lights;
mod life;
pub(crate) mod props;
pub mod selection;
pub mod travel;

use crate::engine::{G, V, truthy};
use crate::net::{Event, Msg, Net, PeerId, PlayerState, Role, Tick};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Local peer id for code that can't take the COOP lock (GML hooks).
pub static MY_ID: AtomicU8 = AtomicU8::new(0);

pub struct Remote {
    pub room:String,
    pub busy:bool,
    pub name: String,
    pub state: Option<PlayerState>,
    pub seen: Instant,
    pub layout: Option<(u32, u32, u64)>,
}

pub struct Coop {
    pub ending: ending::Ending,
    pub pedestal: pedestal::Pedestal,
    pub net: Option<Net>,
    pub status: String,
    pub remotes: HashMap<PeerId, Remote>,
    pub ghosts: ghosts::Ghosts,
    pub ai: ai::Ai,
    pub enemies: enemies::Enemies,
    pub menu: menu::MenuState,
    pub ready: [bool; crate::net::MAX_PLAYERS],
    pub colors: [u8; crate::net::MAX_PLAYERS],
    pub difficulty: difficulty::Difficulty,
    pub cursors: cursors::Cursors,
    pub selection: selection::Selection,
    pub computer: computer::Computer,
    pub travel: travel::Travel,
    pub projectiles: projectiles::Projectiles,
    lobby_sent: Option<Instant>,
    heartbeat_sent: Option<Instant>,
    sent_gone: bool,
    /// Our own (level, sub, hash) for the current fight room.
    pub my_layout: Option<(u32, u32, u64)>,
    room: String,
    room_frames: u32,
}

pub static COOP: LazyLock<Mutex<Coop>> = LazyLock::new(|| {
    Mutex::new(Coop {
        ending: ending::Ending::default(),
        pedestal: pedestal::Pedestal::default(),
        net: None,
        status: "offline".into(),
        remotes: HashMap::new(),
        ghosts: ghosts::Ghosts::default(),
        ai: ai::Ai::default(),
        enemies: enemies::Enemies::default(),
        menu: menu::MenuState::default(),
        ready: [false; crate::net::MAX_PLAYERS],
        colors: [0, 1, 2],
        difficulty: difficulty::Difficulty::default(),
        cursors: cursors::Cursors::default(),
        selection: selection::Selection::default(),
        computer: computer::Computer::default(),
        travel: travel::Travel::default(),
        projectiles: projectiles::Projectiles::default(),
        lobby_sent: None,
        heartbeat_sent: None,
        sent_gone: false,
        my_layout: None,
        room: String::new(),
        room_frames: 0,
    })
});

impl Coop {
    pub fn host(&mut self, g: G, port: u16) {
        self.leave(g);
        match if crate::config::load().transport==crate::config::Transport::Steam {Net::steam(Role::Host,"")}else{Net::host(port, crate::net::local_name())} {
            Ok(n) => {
                self.status = format!("hosting on {}", n.addr);
                self.net = Some(n);
                MY_ID.store(0, Ordering::Relaxed);
                ai::install_hooks(g);
            }
            Err(e) => self.status = format!("host failed: {e}"),
        }
        crate::log(&format!("coop: {}", self.status));
    }

    pub fn join(&mut self, g: G, addr: &str) {
        self.leave(g);
        if crate::config::load().transport==crate::config::Transport::Steam {
            match Net::steam(Role::Client,addr){Ok(n)=>self.net=Some(n),Err(e)=>{self.status=e;return;}}
        }else{self.net = Some(Net::join(addr, crate::net::local_name()));}
        self.status = format!("connecting to {addr}...");
        crate::log(&format!("coop: {}", self.status));
    }

    pub fn leave(&mut self, g: G) {
        self.ending = ending::Ending::default();
        boss_contact::reset();
        props::reset();
        self.pedestal.reset(g);
        let leave_world = self.net.as_ref().is_some_and(|n|n.role==Role::Client)
            && rooms::RUN_SEED.load(Ordering::Relaxed)!=0 && g.room_name()!="Room_main_menu";
        if self.net.take().is_some() {
            crate::log("coop: left session");
        }
        self.remotes.clear();
        self.ai.reset(g);
        self.ghosts.clear(g);
        self.status = "offline".into();
        rooms::RUN_SEED.store(0, Ordering::Relaxed);
        // A client puppet never had the native AI's state-entry history. Keep
        // it inert until destruction, rather than resuming it after disconnect.
        enemies::PUPPET_MODE.store(leave_world, Ordering::Relaxed);
        if leave_world {g.call("room_goto",&[g.asset("Room_main_menu")]);}
        self.enemies.reset();
        self.my_layout = None;
        self.ready = [false; crate::net::MAX_PLAYERS];
        self.colors = [0, 1, 2];
        self.difficulty = difficulty::Difficulty::default();
        self.cursors = cursors::Cursors::default();
        self.selection = selection::Selection::default();
        self.computer = computer::Computer::default();
        self.travel = travel::Travel::default();
        self.projectiles.clear(g);
        economy::reset(g);
        life::reset();
        self.lobby_sent = None;
        self.heartbeat_sent = None;
        self.sent_gone = false;
        self.room = g.room_name();
        self.room_frames = 0;
        MY_ID.store(0, Ordering::Relaxed);
    }

    /// Everyone in the session including us: (id, name, is_me).
    pub fn roster(&self) -> Vec<(PeerId, String, bool)> {
        let Some(n) = &self.net else { return vec![] };
        let mut v: Vec<_> = n.peers.iter().map(|p| (p.id, p.name.clone(), p.id == n.my_id)).collect();
        v.sort_by_key(|p| p.0);
        v
    }

    fn handle_events(&mut self, g: G) {
        let Some(net) = self.net.as_mut() else { return };
        let events = net.poll();
        let (role, addr) = (net.role, net.addr.clone());
        for e in events {
            match e {
                Event::Connected { my_id, peers } => {
                    MY_ID.store(my_id, Ordering::Relaxed);
                    for p in peers.iter().filter(|p| p.id != my_id) {
                        self.remotes.insert(p.id, Remote { room:String::new(),busy:false,name: p.name.clone(), state: None, seen: Instant::now(), layout: None });
                    }
                    self.status = match role {
                        Role::Host => format!("hosting on {addr}"),
                        Role::Client => format!("connected to {addr} as player {}", my_id + 1),
                    };
                    self.sent_gone = false;
                }
                Event::Joined(p) => {
                    crate::log(&format!("coop: {} joined (peer {})", p.name, p.id));
                    self.ready[p.id as usize] = false;
                    self.colors[p.id as usize] = p.id;
                    self.lobby_sent = None;
                    self.remotes.insert(p.id, Remote { room:String::new(),busy:false,name: p.name, state: None, seen: Instant::now(), layout: None });
                }
                Event::Left(id) => {
                    if role == Role::Host {
                        economy::peer_left(g,id);
                        pedestal::peer_left(g,self,id);
                        self.difficulty.member_left(id);
                        self.selection.member_left(id);
                        self.ending.member_left(id);
                    }
                    self.ready[id as usize] = false;
                    self.lobby_sent = None;
                    let name = self.remotes.remove(&id).map(|r| r.name).unwrap_or_default();
                    crate::log(&format!("coop: {name} (peer {id}) left"));
                }
                Event::Msg(from, msg) => {
                    let r = self.remotes.entry(from).or_insert(Remote {
                        room:String::new(),busy:false,
                        name: format!("player {}", from + 1),
                        state: None,
                        seen: Instant::now(),
                        layout: None,
                    });
                    r.seen = Instant::now();
                    match msg {
                        Msg::BossContact(hit) => boss_contact::receive(g,from,hit),
                        Msg::Ending(p) => ending::receive(g,self,from,p),
                        Msg::Pedestal(packet) => pedestal::receive(g,self,from,packet),
                        Msg::PropInteract{committed,action} => props::receive_interact(g,self,from,committed,action),
                        Msg::PropHit{committed,hit} => props::receive(g,from,committed,hit),
                        Msg::Tick(t) => {
                            r.room=t.room.clone();r.busy=t.busy;
                            if from==0 {if let Some(s)=t.objective {objective::apply(g,s);} if let Some(s)=t.props {props::apply(g,s);} if let Some(b)=t.boss{rooms::apply_boss(g,b);}if let Some(state)=t.economy {economy::apply(g,state);}}
                            if let Some(d) = t.projectiles { self.projectiles.receive(g, from, d); }
                            match t.state {
                                Some(st) if st.sane() => r.state = Some(st),
                                Some(_) => crate::log(&format!("coop: dropped invalid state from peer {from}")),
                                None => r.state = None,
                            }
                            damage_numbers::receive(g,t.player_numbers);
                            if role == Role::Host {
                                let mut pd = enemies::PENDING_DMG.lock().unwrap();
                                for hit in t.hits {
                                    if hit.sane() && hit.room==g.room_name() && (hit.level,hit.sub)==rooms::level_sub(g) {
                                        pd.entry(hit.gid).or_default().push((from,hit));
                                    }
                                }
                            } else if from == 0 {
                                if let Some(lights)=t.map_lights{map_lights::receive(g,lights);}
                                enemy_melee::receive(g, t.melee);
                                damage_numbers::receive(g, t.numbers);
                                if !t.fires.is_empty() {
                                    self.enemies.on_fires(g, t.fires);
                                }
                                if let (Some(d), Some(g)) = (t.enemies, G::new()) {
                                    self.enemies.on_delta(g, d);
                                }
                            }
                        }
                        msg @ (Msg::ShotDefinitionRequest{..}|Msg::ShotDefinition{..})=>{
                            if let Some(net)=&self.net{self.projectiles.message(net,from,msg);}
                        }
                        msg @ (Msg::PurchaseRequest{..}|Msg::PurchaseResult{..}|Msg::PurchaseDone{..})=>economy::receive(g,self,from,msg),
                        Msg::TeamGameOver{run} => {if from==0&&run==rooms::RUN_SEED.load(Ordering::Relaxed){life::game_over();}},
                        Msg::Ping(t) => {
                            if let Some(n) = &self.net {
                                n.broadcast(Msg::Pong(t))
                            }
                        }
                        Msg::Pong(_) => {}
                        Msg::StartRun { seed, rules } => {
                            if from == 0 {
                                if rooms::RUN_SEED.load(Ordering::Relaxed) == 0 {
                                    *crate::config::RUN_RULES.lock().unwrap() = rules;
                                    start_run_local(seed);
                                }
                            }
                        }
                        Msg::Layout { level, sub, hash } => r.layout = Some((level, sub, hash)),
                        Msg::SetReady(ready) => {
                            if role == Role::Host && rooms::RUN_SEED.load(Ordering::Relaxed) == 0 {
                                self.ready[from as usize] = ready;
                                self.lobby_sent = None;
                            }
                        }
                        Msg::SetColor(color) => {
                            if role == Role::Host && rooms::RUN_SEED.load(Ordering::Relaxed) == 0 {
                                self.colors[from as usize] = color;
                                self.lobby_sent = None;
                            }
                        }
                        Msg::Lobby { ready, colors, .. } => {
                            if from == 0 { self.ready = ready; self.colors = colors; }
                        }
                        Msg::MenuCursor { run, menu, x, y, visible } => cursors::receive(self, from, run, menu, x, y, visible),
                        msg @ (Msg::SelectionUpdate { .. } | Msg::SelectionState { .. } | Msg::SelectionAck { .. } | Msg::SelectionCommit { .. }) => selection::receive(self, from, msg),
                        msg @ (Msg::ComputerEntered { .. } | Msg::ComputerRequest { .. } | Msg::ComputerAction { .. }) => computer::receive(self, g, from, msg),
                        msg @ (Msg::TravelRequest { .. } | Msg::TravelCommit { .. }) => travel::receive(self, g, from, msg),
                        msg => difficulty::receive(self, g, from, msg),
                    }
                }
                Event::Failed(e) => {
                    self.leave(g);
                    self.status = format!("failed: {e}");
                    crate::log(&format!("coop: {}", self.status));
                    break;
                }
                Event::Closed => {
                    let extracted = self.ending.extracting();
                    // The host can reach its native end screen one frame before
                    // a client executes TeamGameOver. Preserve that committed
                    // action until Player.Step runs; returning to the menu will
                    // perform the normal cleanup on the following frame.
                    if !life::ending() && !self.ending.pending() { self.leave(g); }
                    self.status = if extracted { "offline" } else { "disconnected (host closed)" }.into();
                    crate::log("coop: connection closed");
                    break;
                }
            }
        }
    }

    pub fn toggle_ready(&mut self) {
        let Some(n) = &self.net else { return };
        if !n.connected() || n.my_id as usize >= self.ready.len() || rooms::RUN_SEED.load(Ordering::Relaxed) != 0 { return; }
        let value = !self.ready[n.my_id as usize];
        if n.role == Role::Host {
            self.ready[n.my_id as usize] = value;
            self.lobby_sent = None;
        } else {
            n.broadcast(Msg::SetReady(value));
        }
    }

    pub fn cycle_color(&mut self) {
        let Some(n) = &self.net else { return };
        if !n.connected() || n.my_id as usize >= self.colors.len() || rooms::RUN_SEED.load(Ordering::Relaxed) != 0 { return; }
        let next = (self.colors[n.my_id as usize] + 1) % colors::PALETTE.len() as u8;
        if n.role == Role::Host {
            self.colors[n.my_id as usize] = next;
            self.lobby_sent = None;
        } else { n.broadcast(Msg::SetColor(next)); }
    }

    pub fn everyone_ready(&self) -> bool {
        self.net.as_ref().is_some_and(|n| !n.peers.is_empty()
            && n.peers.iter().all(|p| self.ready[p.id as usize]))
    }

    fn send_lobby(&mut self) {
        let Some(n) = &self.net else { return };
        if n.role == Role::Host && self.lobby_sent.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
            n.broadcast(Msg::Lobby { ready: self.ready, colors: self.colors, running: rooms::RUN_SEED.load(Ordering::Relaxed) != 0 });
            self.lobby_sent = Some(Instant::now());
        }
    }

    fn check_connections(&mut self, g: G) {
        let Some(n) = &self.net else { return };
        if !n.connected() { return; }
        if self.heartbeat_sent.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
            n.broadcast(Msg::Ping(0));
            self.heartbeat_sent = Some(Instant::now());
        }
        let timed_out: Vec<_> = self.remotes.iter().filter(|(_, r)| r.seen.elapsed() >= Duration::from_secs(15)).map(|(id, _)| *id).collect();
        if n.role == Role::Client && timed_out.contains(&0) {
            self.leave(g);
            self.status = "disconnected: host stopped responding".into();
            crate::log("net: host heartbeat timed out");
        } else if n.role == Role::Host {
            for id in timed_out {
                n.disconnect(id);
                // Avoid repeating the message while the normal Left event is
                // being delivered by the reader thread.
                if let Some(r) = self.remotes.get_mut(&id) { r.seen = Instant::now(); }
                crate::log(&format!("net: peer {id} heartbeat timed out"));
            }
        }
    }

    /// One coalesced tick per co-op frame (60 Hz): our player, our hits on
    /// enemies (client), enemy shots + enemy delta (host). Empty ticks are
    /// skipped, except one to say our player is gone.
    fn send_tick(&mut self, g: G) {
        let Some(net) = &self.net else { return };
        if !net.connected() {
            return;
        }
        let in_run = rooms::RUN_SEED.load(Ordering::Relaxed) != 0;
        let mut t = Tick { room:g.room_name(),busy:economy::busy(g),state: capture_local(g), projectiles: in_run.then(|| self.projectiles.capture(g)), ..Default::default() };
        t.player_numbers=std::mem::take(&mut *damage_numbers::PLAYER_OUTGOING.lock().unwrap());
        if in_run && net.role==Role::Host {t.objective=objective::capture(g);t.props=props::capture(g);t.map_lights=map_lights::capture(g);t.economy=economy::capture(g);t.boss=rooms::capture_boss(g);}
        if net.role == Role::Client {
            t.hits = std::mem::take(&mut *enemies::HITS.lock().unwrap());
        } else if in_run && g.first("objPlayer").is_some() {
            t.fires = std::mem::take(&mut *enemies::FIRES.lock().unwrap());
            t.melee = std::mem::take(&mut *enemy_melee::OUTGOING.lock().unwrap());
            t.numbers = std::mem::take(&mut *damage_numbers::OUTGOING.lock().unwrap());
            t.enemies = Some(self.enemies.host_delta(g));
        }
        let empty = t.state.is_none() && t.projectiles.is_none() && t.hits.is_empty() && t.fires.is_empty() && t.enemies.is_none();
        if !in_run && empty && self.sent_gone {
            return;
        }
        self.sent_gone = t.state.is_none();
        let msg = Msg::Tick(t);
        // Diagnose malformed host snapshots locally too: a client would reject
        // the whole tick, leaving its authoritative world stale.
        if msg.checked_from(net.my_id) { net.broadcast(msg); }
    }
}

/// Begin a co-op run locally with `seed`: install generation hooks and send
/// the player to the game's own run setup (PLAY page → run).
pub fn start_run_local(seed: u64) {
    props::reset();
    boss_contact::reset();
    rooms::RUN_SEED.store(seed, Ordering::Relaxed);
    crate::log(&format!("coop: run started, seed {seed:016x}"));
    if let Some(g) = G::new() {
        pedestal::install(g);
        difficulty::install_hooks(g);
        selection::install_hooks(g);
        computer::install_hooks(g);
        travel::install_hooks(g);
        enemy_melee::install(g);
        boss_contact::install(g);
        objective::install(g);
        ending::install(g);
        damage_numbers::install(g);
        map_lights::install(g);
        projectiles::install_hooks(g);
        avatars::install(g);
        pause::install(g);
        life::install(g);
        props::install(g);
        rooms::install_hooks(g);
        enemies::install_hooks(g);
        status_effects::install(g);
        economy::reset(g);
        life::reset();
        economy::install(g);
        ai::install_hooks(g);
        let is_client = COOP.try_lock().ok().and_then(|c| c.net.as_ref().map(|n| n.role == Role::Client)).unwrap_or(MY_ID.load(Ordering::Relaxed) != 0);
        enemies::PUPPET_MODE.store(is_client, Ordering::Relaxed);
        // Do not leave a lobby action page active across menu destruction:
        // returning to the menu would otherwise silently host/join again.
        let page = g.global("page");
        if crate::engine::is_struct(&page) {
            g.set(page, "active", g.s("page_top_menu"));
            g.set(page, "old", g.call("array_create", &[V::real(0.0)]));
        }
        // The native PLAY/run item uses start_game with val = this room.
        // Starting here runs the same RoomStart initialization on every peer.
        g.call("room_goto", &[g.asset("Room_select_difficulty")]);
    }
}

/// Host: pick a run seed, tell everyone, start locally.
pub fn host_start_run(c: &mut Coop, force: bool) -> String {
    if rooms::RUN_SEED.load(Ordering::Relaxed) != 0 { return "run already started".into(); }
    let Some(n) = &mut c.net else { return "not in a session".into() };
    if n.role != Role::Host {
        return "only the host can start the run".into();
    }
    // Admission and the roster snapshot are locked together, so a peer cannot
    // join between the ready check and StartRun and miss the message.
    match n.begin_run(force, &c.ready) {
        Ok(peers) => n.peers = peers,
        Err(e) => return e,
    }
    let seed = {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.finish() | 1
    };
    let mut rules = crate::config::load().rules;
    rules.players = n.peers.len() as u8;
    *crate::config::RUN_RULES.lock().unwrap() = rules;
    crate::log(&format!("run settings: {rules:?}"));
    n.broadcast(Msg::StartRun { seed, rules });
    start_run_local(seed);
    format!("run started (seed {seed:016x})")
}

/// Snapshot of the local `objPlayer`, or None when there isn't one.
pub fn capture_local(g: G) -> Option<PlayerState> {
    let p = g.first("objPlayer")?;
    let f = |n: &str| g.num(p, n) as f32;
    let v2 = g.get(p, "v2_vel");
    let (vx, vy) = if crate::engine::is_struct(&v2) { (g.num(v2, "x"), g.num(v2, "y")) } else { (0.0, 0.0) };
    let body_angle = f("image_angle");
    let legs_angle = if vx * vx + vy * vy > 0.01 {
        g.call("point_direction", &[V::real(0.0), V::real(0.0), V::real(vx), V::real(vy)]).as_f64().unwrap_or(0.0) as f32
    } else {
        body_angle
    };
    let mut s = PlayerState {
        avatar: avatars::snapshot(),
        weapon: avatars::weapon_snapshot(),
        lights: lights::capture(g,p),
        room: g.room_name(),
        x: f("x"),
        y: f("y"),
        depth: f("depth"),
        body_sprite: g.sprite_name(g.get(p, "sprite_index")),
        body_frame: f("image_index"),
        body_angle,
        body_xscale: f("image_xscale"),
        body_yscale: f("image_yscale"),
        legs_sprite: g.sprite_name(g.get(p, "legs_sprite")),
        legs_frame: f("legs_i"),
        legs_angle,
        hp: f("hp"),
        hp_max: f("hp_max"),
        dead: truthy(&g.get(p, "dead")),
        can_revive:truthy(&g.get(p,"revive")),
        ..Default::default()
    };
    let gun = g.get(p, "Gun");
    if g.exists(gun) {
        s.gun_sprite = g.sprite_name(g.get(gun, "sprite_index"));
        s.gun_frame = g.num(gun, "image_index") as f32;
        s.gun_angle = g.num(gun, "image_angle") as f32;
        s.gun_x = g.num(gun, "x") as f32;
        s.gun_y = g.num(gun, "y") as f32;
        s.gun_xscale = g.num(gun,"image_xscale") as f32;
        s.gun_alpha = g.num(gun,"image_alpha") as f32;
        s.gun_yscale = g.num(gun, "image_yscale") as f32;
        s.gun_depth = g.num(gun, "depth") as f32;
        s.gun_reloading = truthy(&g.get(gun, "mag_reloading"));
    }
    Some(s)
}

static FIRST_PUMP: LazyLock<Instant> = LazyLock::new(Instant::now);
static READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The runtime is unsafe to call during engine start-up (the first pumps happen
/// before any room exists). Wait until a room is running and the game's
/// persistent controller exists.
pub(crate) fn game_ready(g: G) -> bool {
    if READY.load(Ordering::Relaxed) {
        return true;
    }
    if FIRST_PUMP.elapsed() < Duration::from_secs(2) {
        return false;
    }
    let room = g.var("room");
    if room.kind() != crate::gm::KIND_REF && room.as_f64().is_none_or(|r| r < 0.0) {
        return false;
    }
    // The startup room already has a controller, but its globals and native
    // services are still being initialized. Console probes must wait too.
    if g.room_name() != "Room_main_menu" { return false; }
    if g.first("objController").is_none() {
        return false;
    }
    crate::log(&format!("coop: game ready (room {})", g.room_name()));
    READY.store(true, Ordering::Relaxed);
    true
}

/// Called from the main-thread pump.
pub fn frame() {
    let Some(g) = G::new() else { return };
    if !game_ready(g) {
        return;
    }
    let mut c = COOP.lock().unwrap();
    // Native objMenu may persist across rooms: cleanup must follow the room
    // transition, not a change of menu instance identity.
    if rooms::RUN_SEED.load(Ordering::Relaxed)!=0 && g.room_name()=="Room_main_menu"
        && !c.room.is_empty() && c.room!="Room_main_menu" && !c.ending.extracting() {c.leave(g);}
    // Discard old-room puppets and queued attacks before consuming new packets.
    if g.room_name() != c.room {
        c.enemies.reset();
        c.ai.reset(g);
        c.travel.room_changed();
        c.projectiles.room_changed();
    }
    if let Some(net)=&c.net {props::tick(g,net);}
    menu::install(g);
    menu::poll_invites(g, &mut c);
    c.handle_events(g);
    fuzz::tick(g);
    if rooms::RUN_SEED.load(Ordering::Relaxed)!=0 {visual_damage::ensure_player(g);}
    c.check_connections(g);
    economy::tick(g,&c);
    boss_contact::tick(g,&c);
    ending::tick(g,&mut c);
    pedestal::tick(g,&mut c);
    travel::tick(&mut c,g);
    c.send_lobby();
    difficulty::tick(g, &mut c);
    selection::tick(&mut c);
    cursors::tick(g, &mut c);
    if c.net.is_none() {
        if !c.ghosts.is_empty() {
            c.ghosts.clear(g);
        }
        return;
    }
    if let Some(net)=&c.net{c.projectiles.recover(net);}
    c.send_tick(g);
    let room = g.room_name();
    if room != c.room {
        c.room = room.clone();
        c.room_frames = 0;
        c.my_layout = None;
    }
    c.room_frames += 1;
    // ~0.5 s after entering a room, publish our layout hash (fight rooms only).
    if c.room_frames == 30 && rooms::RUN_SEED.load(Ordering::Relaxed) != 0 {
        if let Some(hash) = rooms::layout_hash(g) {
            let (level, sub) = rooms::level_sub(g);
            c.my_layout = Some((level, sub, hash));
            if let Some(n) = &c.net {
                n.broadcast(Msg::Layout { level, sub, hash });
            }
            crate::log(&format!("rooms: layout {level}.{sub} in {room} = {hash:016x}"));
        }
    }
    c.projectiles.update(g);
    avatars::tick(&c);
    let in_run = rooms::RUN_SEED.load(Ordering::Relaxed) != 0;
    {let Coop {ghosts,remotes,..}=&mut *c;ghosts.update(g,remotes,&room);}
    life::tick(g,&c);
    let Coop { ghosts, remotes, ai, net, enemies, .. } = &mut *c;
    let is_host = net.as_ref().is_some_and(|n| n.role == Role::Host);
    if is_host {
        ai.update(g, ghosts, remotes);
    } else if in_run {
        enemies.client_apply(g);
    }
}

/// `coop ...` console command (runs on the main thread).
pub fn console(args: &[String]) -> String {
    let Some(g) = G::new() else { return "error: runtime not ready".into() };
    let a: Vec<&str> = args.iter().map(|s| s.trim_start_matches('\u{1}')).collect();
    if a==["map-lights"] {return map_lights::describe(g);}
    if a==["rng-check"] {return rng::check(g);}
    if a==["projectile-cache","forget"]{projectiles::forget_remote();return "remote shot cache discarded for recovery check".into();}
    if a==["shot-audit"] {return shot_events::audit();}
    if a==["shot-trace","on"] {shot_events::trace(true);return "shot trace enabled".into();}
    if a==["shot-trace","off"] {shot_events::trace(false);return "shot trace disabled".into();}
    if let ["fuzz", rest @ ..] = a.as_slice() { return fuzz::console(g, rest); }
    let mut c = COOP.lock().unwrap();
    match a.as_slice() {
        [] | ["status"] => status(&c),
        ["pedestal", rest @ ..] => pedestal::console(g,&mut c,rest),
        ["host"] => {
            c.host(g, crate::net::DEFAULT_PORT);
            status(&c)
        }
        ["host", port] => match port.parse() {
            Ok(p) => {
                c.host(g, p);
                status(&c)
            }
            Err(_) => "usage: coop host [port]".into(),
        },
        ["join"] => {
            c.join(g, &format!("127.0.0.1:{}", crate::net::DEFAULT_PORT));
            status(&c)
        }
        ["join", addr] => {
            let addr = if crate::config::load().transport==crate::config::Transport::Steam || addr.contains(':') { addr.to_string() } else { format!("{addr}:{}", crate::net::DEFAULT_PORT) };
            c.join(g, &addr);
            status(&c)
        }
        ["leave"] => {
            c.leave(g);
            status(&c)
        }
        ["code"] => c.net.as_ref().and_then(Net::lobby_code).unwrap_or_else(||"no Steam lobby".into()),
        ["ai"] => c.ai.describe(g),
        ["enemies"] => c.enemies.describe(),
        ["hits"] => client_hits::describe(),
        ["difficulty"] => c.difficulty.describe(),
        ["selection"] => c.selection.describe(),
        ["projectiles"] => c.projectiles.describe(),
        ["avatars"] => avatars::describe(),
        ["economy"] => economy::describe(g),
        ["travel"] => format!("{:?}", c.travel),
        ["computer"] => format!("{:?}", c.computer),
        ["rules"] => {
            let r=*crate::config::RUN_RULES.lock().unwrap();
            format!("players {} enemy health {:.3}x enemy damage {:.3}x",r.players,r.health_scale(),r.damage_scale())
        },
        // Test: report a hit on enemy `gid` exactly like a real client hit would.
        ["testhit", gid, amount] | ["testhit", gid, amount, _] => match (gid.parse::<u16>(), amount.parse::<f64>(), a.get(3).unwrap_or(&"1").parse::<u8>()) {
            (Ok(gid), Ok(amount), Ok(dtype)) if dtype < 16 => {
                let (level,sub)=rooms::level_sub(g);
                let hit=crate::net::EnemyHit{gid,room:g.room_name(),level,sub,amount,x:0.0,y:0.0,kx:0.0,ky:0.0,dtype,scale:1.0,flag:true,extra:None};
                if !hit.sane() || rooms::RUN_SEED.load(Ordering::Relaxed)==0 || c.net.is_none() {
                    return "test hit requires a running session, nonzero enemy ID and finite damage below 1000000".into();
                }
                // Execute inside Enemy.Step even for host-side diagnostics. A
                // direct console method_call lacks the native damage context.
                if c.net.as_ref().is_some_and(|n|n.role==Role::Host) {
                    enemies::PENDING_DMG.lock().unwrap().entry(gid).or_default().push((0,hit));
                } else {
                    enemies::HITS.lock().unwrap().push(hit);
                }
                format!("queued test hit: enemy {gid} -{amount} type {dtype}")
            }
            _ => "usage: coop testhit <gid> <amount> [damage type 0..15]".into(),
        },
        ["ready"] => { c.toggle_ready(); status(&c) }
        ["start"] => host_start_run(&mut c, true),
        _ => "usage: coop [status] | coop host [port] | coop join [addr[:port]] | coop leave | coop start | coop ai".into(),
    }
}

fn status(c: &Coop) -> String {
    let mut s = format!("status: {}\n", c.status);
    for (id, name, me) in c.roster() {
        let st = if me {
            "(you)".to_string()
        } else {
            match c.remotes.get(&id).and_then(|r| r.state.as_ref()) {
                Some(p) => format!("in {} at ({:.0}, {:.0}) hp {:.0}/{:.0}{}", p.room, p.x, p.y, p.hp, p.hp_max, if p.dead { " DEAD" } else { "" }),
                None => "no player in a room".into(),
            }
        };
        s += &format!("  P{} {:<20} {} [{}]\n", id + 1, name, st, if c.ready[id as usize] { "ready" } else { "not ready" });
    }
    let seed = rooms::RUN_SEED.load(Ordering::Relaxed);
    if seed != 0 {
        s += &format!("run seed {seed:016x}\n");
        if let Some((l, sb, h)) = c.my_layout {
            s += &format!("layout {l}.{sb} {h:016x}");
            for (id, r) in &c.remotes {
                match r.layout {
                    Some((rl, rs, rh)) if (rl, rs) == (l, sb) => {
                        s += &format!("  P{}: {}", id + 1, if rh == h { "match" } else { "MISMATCH" })
                    }
                    Some((rl, rs, _)) => s += &format!("  P{}: on {rl}.{rs}", id + 1),
                    None => s += &format!("  P{}: -", id + 1),
                }
            }
            s += "\n";
        }
    }
    s += &format!("hooks: {}", crate::hooks::list().join(", "));
    s
}
