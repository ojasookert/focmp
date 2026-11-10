//! Remote players drawn as sprite-only `objPlayerDummy` instances.
//!
//! `objPlayerDummy` has no events, so the engine's default draw renders its
//! `sprite_index` at x/y with image_angle/scale. Each remote player gets three:
//! legs, body and gun, each mirroring the sprite names and frames that peer sends.
//! They're ordinary room instances: they vanish on room change and get re-created.

use super::Remote;
use crate::engine::{G, V, boolean, real};
use crate::net::{PeerId, PlayerState};
use std::collections::HashMap;
use std::time::Duration;

pub struct Ghost {
    pub legs: V,
    pub body: V,
    pub gun: V,
    /// Smoothed position.
    pub x: f32,
    pub y: f32,
    pub dead: bool,
}

#[derive(Default)]
pub struct Ghosts {
    map: HashMap<PeerId, Ghost>,
}

const STALE: Duration = Duration::from_secs(3);

impl Ghosts {
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn get(&self, peer: PeerId) -> Option<&Ghost> {
        self.map.get(&peer)
    }

    pub fn clear(&mut self, g: G) {
        for (_, gh) in self.map.drain() {
            destroy(g, &gh);
        }
    }

    pub fn update(&mut self, g: G, remotes: &HashMap<PeerId, Remote>, room: &str) {
        // Drop ghosts for peers that left, went stale, or are in another room.
        let keep: Vec<PeerId> = remotes
            .iter()
            .filter(|(_, r)| r.seen.elapsed() < STALE && r.state.as_ref().is_some_and(|s| s.room == room))
            .map(|(id, _)| *id)
            .collect();
        let gone: Vec<PeerId> = self.map.keys().filter(|id| !keep.contains(id)).copied().collect();
        for id in gone {
            if let Some(gh) = self.map.remove(&id) {
                destroy(g, &gh);
            }
        }
        for id in keep {
            let s = remotes[&id].state.as_ref().unwrap();
            let need_new = self.map.get(&id).is_none_or(|gh| !(g.exists(gh.legs) && g.exists(gh.body) && g.exists(gh.gun)));
            if need_new {
                if let Some(old) = self.map.remove(&id) {
                    destroy(g, &old);
                }
                let dummy = g.asset("objPlayerDummy");
                let mk = |depth: f32| {
                    g.call("instance_create_depth", &[real(s.x as f64), real(s.y as f64), real(depth as f64), dummy])
                };
                let gh = Ghost { legs: mk(s.depth + 1.0), body: mk(s.depth), gun: mk(s.gun_depth), x: s.x, y: s.y, dead: s.dead };
                for inst in [gh.legs, gh.body, gh.gun] {
                    g.set(inst, "image_speed", real(0.0));
                    g.set(inst, "focmp_ghost", real(id as f64));
                }
                self.map.insert(id, gh);
            }
            let gh = self.map.get_mut(&id).unwrap();
            // Light smoothing; snap if far (teleport/room start).
            let (dx, dy) = (s.x - gh.x, s.y - gh.y);
            if dx * dx + dy * dy > 200.0 * 200.0 {
                gh.x = s.x;
                gh.y = s.y;
            } else {
                gh.x += dx * 0.5;
                gh.y += dy * 0.5;
            }
            gh.dead = s.dead;
            apply(g, gh, s);
        }
    }
}

fn sprite(g: G, name: &str) -> V {
    if name.is_empty() { real(-1.0) } else { g.asset(name) }
}

fn apply(g: G, gh: &Ghost, s: &PlayerState) {
    // AI reads Player.Gun.mag_reloading when deciding whether to rush a
    // target. Keep the non-simulating anchor's native player interface valid.
    g.set(gh.body, "Gun", gh.gun);
    g.set(gh.body, "dead", boolean(s.dead));
    g.set(gh.body, "hp", real(s.hp as f64));
    g.set(gh.body, "hp_max", real(s.hp_max as f64));
    g.set(gh.gun, "Owner", gh.body);
    g.set(gh.gun, "mag_reloading", boolean(s.gun_reloading));
    g.set(gh.body,"xCam",real(gh.x as f64));g.set(gh.body,"yCam",real(gh.y as f64));
    let (ox, oy) = (gh.x - s.x, gh.y - s.y); // smoothing offset, applied to every part
    let part = |inst: V, spr: &str, frame: f32, angle: f32, xs: f32, ys: f32, x: f32, y: f32, depth: f32| {
        g.set(inst, "sprite_index", sprite(g, spr));
        g.set(inst, "image_index", real(frame as f64));
        g.set(inst, "image_angle", real(angle as f64));
        g.set(inst, "image_xscale", real(if xs == 0.0 { 1.0 } else { xs as f64 }));
        g.set(inst, "image_yscale", real(if ys == 0.0 { 1.0 } else { ys as f64 }));
        g.set(inst, "x", real((x + ox) as f64));
        g.set(inst, "y", real((y + oy) as f64));
        g.set(inst, "depth", real(depth as f64));
        g.set(inst, "visible", boolean(!spr.is_empty()));
    };
    part(gh.legs, &s.legs_sprite, s.legs_frame, s.legs_angle, 1.0, 1.0, s.x, s.y, s.depth + 1.0);
    part(gh.body, &s.body_sprite, s.body_frame, s.body_angle, s.body_xscale, s.body_yscale, s.x, s.y, s.depth);
    if !s.avatar.is_empty() {
        g.set(gh.body,"visible",boolean(false));g.set(gh.legs,"visible",boolean(false));
    }
    part(gh.gun, &s.gun_sprite, s.gun_frame, s.gun_angle, s.gun_xscale, s.gun_yscale, s.gun_x, s.gun_y, s.gun_depth);
    // Native weapon rendering is independent of the avatar sprite stream.
    g.set(gh.gun,"visible",boolean(false));
    super::lights::apply(g,gh.body,&s.lights,ox as f64,oy as f64);
}

fn destroy(g: G, gh: &Ghost) {
    if g.exists(gh.body){super::lights::destroy(g,gh.body); }
    // Native enemy state machines dereference Player every frame. Retarget
    // before removing their anchor; the periodic AI pass is too late.
    if let Some(player)=g.first("objPlayer") {
        if let Some(cam)=g.first("objCamera"){if crate::engine::key(&g.get(cam,"follow"))==crate::engine::key(&gh.body){g.set(cam,"follow",player);}}
        for enemy in g.instances("objEnemyMASTER") {
            if crate::engine::key(&g.get(enemy,"Player"))==crate::engine::key(&gh.body) {g.set(enemy,"Player",player);}
        }
    }
    for inst in [gh.legs, gh.body, gh.gun] {
        if g.exists(inst) {
            g.call("instance_destroy", &[inst]);
        }
    }
}
