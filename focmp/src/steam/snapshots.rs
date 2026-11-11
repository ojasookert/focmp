//! A replaceable snapshot and an immutable, ordered combat journal. Only
//! consecutive, wholly unsent batches in the same run/stage may be compacted.
use crate::net::{Msg, Packet, Tick};
use bitcode::{Decode, Encode};
const MAX_EVENT_TICKS: usize = 64;
#[derive(Encode, Decode)]
struct Batch {
    latest: Packet,
    events: Vec<Tick>,
}
fn has_events(t: &Tick) -> bool {
    !t.hits.is_empty()
        || !t.fires.is_empty()
        || !t.melee.is_empty()
        || !t.numbers.is_empty()
        || !t.player_numbers.is_empty()
        || t.enemies.as_ref().is_some_and(|e| !e.deaths.is_empty())
        || t.projectiles
            .as_ref()
            .is_some_and(|b| !b.definitions.is_empty() || !b.fires.is_empty())
}
pub(super) fn key(packet: &Packet) -> Option<String> {
    match packet {
        Packet::Msg {
            from,
            msg: Msg::MenuCursor { run, menu, .. },
        } => Some(format!("cursor:{from}:{run}:{menu}")),
        Packet::Msg {
            from,
            msg: Msg::Tick(t),
        } => {
            let b = t.projectiles.as_ref()?;
            Some(format!(
                "tick:{from}:{}:{}:{}:{}:{}:{}",
                b.run, b.room, b.level, b.sub, t.room, t.busy
            ))
        }
        _ => None,
    }
}
pub(super) fn encode(packet: &Packet) -> Vec<u8> {
    let mut batch = Batch {
        latest: packet.clone(),
        events: vec![],
    };
    if key(packet).is_some() {
        if let Packet::Msg {
            msg: Msg::Tick(t), ..
        } = &mut batch.latest
        {
            if has_events(t) {
                let mut event = Tick {
                    room: t.room.clone(),
                    busy: t.busy,
                    state: t.state.clone(),
                    hits: std::mem::take(&mut t.hits),
                    fires: std::mem::take(&mut t.fires),
                    melee: std::mem::take(&mut t.melee),
                    numbers: std::mem::take(&mut t.numbers),
                    player_numbers: std::mem::take(&mut t.player_numbers),
                    ..Tick::default()
                };
                if let Some(b) = &mut t.projectiles {
                    event.projectiles = Some(b.clone());
                    b.definitions.clear();
                    b.fires.clear();
                }
                if let Some(d) = &mut t.enemies {
                    if !d.deaths.is_empty() {
                        event.enemies = Some(crate::net::EnemyDelta {
                            room: d.room.clone(),
                            level: d.level,
                            sub: d.sub,
                            deaths: std::mem::take(&mut d.deaths),
                            ..Default::default()
                        });
                    }
                }
                batch.events.push(event);
            }
        }
    }
    bitcode::encode(&batch)
}
pub(super) fn decode(bytes: &[u8]) -> Result<Vec<Packet>, String> {
    let mut batch: Batch = bitcode::decode(bytes).map_err(|e| e.to_string())?;
    if batch.events.len() > MAX_EVENT_TICKS {
        return Err("too many combat ticks".into());
    }
    if !batch.events.is_empty() {
        let Packet::Msg {
            msg: Msg::Tick(t), ..
        } = &batch.latest
        else {
            return Err("combat without snapshot".into());
        };
        let context = key(&batch.latest).ok_or("combat without stage")?;
        if has_events(t) {
            return Err("events outside journal".into());
        }
        let from = match &batch.latest {
            Packet::Msg { from, .. } => *from,
            _ => unreachable!(),
        };
        for event in &batch.events {
            let packet = Packet::Msg {
                from,
                msg: Msg::Tick(event.clone()),
            };
            if key(&packet).as_ref() != Some(&context) {
                return Err("combat stage mismatch".into());
            }
        }
        // Reunite the newest event with its snapshot when they are the same
        // tick. Uncongested delivery is then byte-for-byte the original Tick,
        // with no extra native callbacks or changes to their ordering.
        if batch
            .events
            .last()
            .and_then(|e| e.projectiles.as_ref())
            .map(|b| b.seq)
            == t.projectiles.as_ref().map(|b| b.seq)
        {
            let event = batch.events.pop().unwrap();
            let Packet::Msg {
                msg: Msg::Tick(t), ..
            } = &mut batch.latest
            else {
                unreachable!()
            };
            t.hits = event.hits;
            t.fires = event.fires;
            t.melee = event.melee;
            t.numbers = event.numbers;
            t.player_numbers = event.player_numbers;
            t.projectiles = event.projectiles;
            if let Some(deaths) = event.enemies {
                if let Some(enemies) = &mut t.enemies {
                    enemies.deaths = deaths.deaths;
                } else {
                    t.enemies = Some(deaths);
                }
            }
        }
    }
    // Older event ticks retain their original pose and projectile sequence;
    // all are delivered in order, before the latest snapshot, in this poll.
    let mut packets = Vec::with_capacity(batch.events.len() + 1);
    for event in batch.events {
        let Packet::Msg { from, .. } = &batch.latest else {
            unreachable!()
        };
        packets.push(Packet::Msg {
            from: *from,
            msg: Msg::Tick(event),
        });
    }
    packets.push(batch.latest);
    Ok(packets)
}
pub(super) fn merge(old: &[u8], new: &[u8]) -> Option<Vec<u8>> {
    let old: Batch = bitcode::decode(old).ok()?;
    let mut new: Batch = bitcode::decode(new).ok()?;
    if key(&old.latest).is_none()
        || key(&old.latest) != key(&new.latest)
        || old.events.len() + new.events.len() > MAX_EVENT_TICKS
    {
        return None;
    }
    match (old.latest, &mut new.latest) {
        (
            Packet::Msg {
                msg: Msg::Tick(old),
                ..
            },
            Packet::Msg {
                from,
                msg: Msg::Tick(t),
            },
        ) => {
            if let Some(older) = old.enemies {
                if let Some(latest) = &mut t.enemies {
                    if (older.level, older.sub, &older.room)
                        != (latest.level, latest.sub, &latest.room)
                    {
                        return None;
                    }
                    latest.complete |= older.complete;
                    latest.merge_older(older);
                } else {
                    t.enemies = Some(older);
                }
            }
            if let Some(older) = old.props {
                if let Some(latest) = &mut t.props {
                    if !latest.merge_older(older) {
                        return None;
                    }
                } else {
                    t.props = Some(older);
                }
            }
            if t.objective.is_none() {
                t.objective = old.objective;
            }
            if t.boss.is_none() {
                t.boss = old.boss;
            }
            if t.economy.is_none() {
                t.economy = old.economy;
            }
            if t.map_lights.is_none() {
                t.map_lights = old.map_lights;
            }
            if !Msg::Tick(t.clone()).valid_from(*from) {
                return None;
            }
        }
        (
            Packet::Msg {
                msg: Msg::MenuCursor { .. },
                ..
            },
            Packet::Msg {
                msg: Msg::MenuCursor { .. },
                ..
            },
        ) => {}
        _ => return None,
    }
    let mut events = old.events;
    events.append(&mut new.events);
    new.events = events;
    Some(bitcode::encode(&new))
}
