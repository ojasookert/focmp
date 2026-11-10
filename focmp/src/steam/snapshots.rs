//! Compact consecutive unsent fight snapshots during congestion, at the normal
//! 60 Hz cadence. Gameplay events, stage changes and partial sends are barriers.
use crate::net::{Msg, Packet, Tick};

fn quiet(t: &Tick) -> bool {
    t.hits.is_empty() && t.fires.is_empty() && t.melee.is_empty()
        && t.numbers.is_empty() && t.player_numbers.is_empty()
        && t.enemies.as_ref().is_none_or(|e| e.deaths.is_empty())
        && t.projectiles.as_ref().is_some_and(|b| b.definitions.is_empty() && b.fires.is_empty())
}

pub(super) fn key(packet: &Packet) -> Option<String> {
    match packet {
        Packet::Msg { from, msg: Msg::MenuCursor { run, menu, .. } } => Some(format!("cursor:{from}:{run}:{menu}")),
        Packet::Msg { from, msg: Msg::Tick(t) } if quiet(t) => {
            let b = t.projectiles.as_ref()?;
            Some(format!("tick:{from}:{}:{}:{}:{}:{}:{}", b.run, b.room, b.level, b.sub, t.room, t.busy))
        }
        _ => None,
    }
}

pub(super) fn merge(old: &[u8], new: &[u8]) -> Option<Vec<u8>> {
    let old: Packet = bitcode::decode(old).ok()?;
    let mut new: Packet = bitcode::decode(new).ok()?;
    if key(&old).is_none() || key(&old) != key(&new) { return None; }
    match (old, &mut new) {
        (Packet::Msg { msg: Msg::Tick(old), .. }, Packet::Msg { from, msg: Msg::Tick(t) }) => {
            if let Some(older) = old.enemies {
                if let Some(latest) = &mut t.enemies {
                    if (older.level, older.sub, &older.room) != (latest.level, latest.sub, &latest.room) { return None; }
                    latest.merge_older(older);
                } else { t.enemies = Some(older); }
            }
            if let Some(older) = old.props {
                if let Some(latest) = &mut t.props { if !latest.merge_older(older) { return None; } }
                else { t.props = Some(older); }
            }
            // These are complete snapshots, while props/enemies above are deltas.
            if t.objective.is_none() { t.objective = old.objective; }
            if t.boss.is_none() { t.boss = old.boss; }
            if t.economy.is_none() { t.economy = old.economy; }
            if t.map_lights.is_none() { t.map_lights = old.map_lights; }
            // Composition must respect the receiver's bounds. Otherwise retain
            // both packets, exactly as for an event barrier.
            if !Msg::Tick(t.clone()).valid_from(*from) { return None; }
        }
        (Packet::Msg { msg: Msg::MenuCursor { .. }, .. }, Packet::Msg { msg: Msg::MenuCursor { .. }, .. }) => {}
        _ => return None,
    }
    Some(bitcode::encode(&new))
}
