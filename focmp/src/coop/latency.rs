//! Lobby ping readings: TCP heartbeat RTT is timestamped on the socket thread;
//! Steam supplies its native transport RTT. Neither waits for a render frame.
use crate::net::{MAX_PLAYERS, PeerId};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Latency {
    pub ms: [Option<u16>; MAX_PLAYERS],
    sequence: u64,
    probes: VecDeque<(u64, Instant, [bool; MAX_PLAYERS])>,
    updated: [Option<Instant>; MAX_PLAYERS],
    accepted: [u64; MAX_PLAYERS],
    samples: [VecDeque<u16>; MAX_PLAYERS],
    published: Option<Instant>,
}

impl Latency {
    pub fn probe(&mut self) -> u64 {
        self.sequence += 1; // Zero is reserved for client keepalives.
        self.probes.push_back((self.sequence, Instant::now(), [false; MAX_PLAYERS]));
        while self.probes.len() > 4 { self.probes.pop_front(); }
        self.sequence
    }

    pub fn receive(&mut self, from: PeerId, token: u64, received: Instant) {
        let i = from as usize;
        if i == 0 || i >= MAX_PLAYERS || token <= self.accepted[i] { return; }
        let Some((_, sent, answered)) = self.probes.iter_mut().find(|p| p.0 == token) else { return };
        if answered[i] { return; }
        answered[i] = true;
        self.accepted[i] = token;
        let ms = received.saturating_duration_since(*sent).as_micros().div_ceil(1000).min(u16::MAX as u128) as u16;
        self.sample(from, ms, received);
    }

    pub fn sample(&mut self, peer: PeerId, ms: u16, received: Instant) {
        let i = peer as usize;
        self.samples[i].push_back(ms);
        while self.samples[i].len() > 5 { self.samples[i].pop_front(); }
        self.updated[i] = Some(received);
    }

    pub fn publish(&mut self) {
        self.ms[0] = Some(0);
        let publish = self.published.is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
        for i in 1..MAX_PLAYERS {
            if self.updated[i].is_none_or(|t| t.elapsed() >= Duration::from_secs(6)) {
                self.ms[i] = None;
                self.samples[i].clear();
            } else if publish {
                let mut samples: Vec<_> = self.samples[i].iter().copied().collect();
                samples.sort_unstable();
                if !samples.is_empty() {
                    let middle = samples.len() / 2;
                    self.ms[i] = Some(((samples[middle] as u32 + samples[(samples.len() - 1) / 2] as u32) / 2) as u16);
                }
            }
        }
        if publish { self.published = Some(Instant::now()); }
    }

    pub fn reset_peer(&mut self, id: PeerId) {
        let i = id as usize;
        self.ms[i] = None;
        self.updated[i] = None;
        self.samples[i].clear();
        // A reused slot must not inherit an earlier occupant's delayed reply.
        self.accepted[i] = self.sequence;
    }
}

pub(super) fn text(ms: Option<u16>) -> String {
    match ms { None => "...".into(), Some(0) => "<1".into(), Some(ms) => ms.to_string() }
}
