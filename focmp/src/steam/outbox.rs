//! Bounded reliable sends. A rejected chunk stays at the head until accepted;
//! accepted chunks are never resent and later packets cannot overtake them.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub const CHUNK: usize = 48 * 1024;
pub const MAX: usize = 1 << 20;
const MAX_QUEUED_BYTES: usize = 8 * MAX;
const MAX_QUEUED_PACKETS: usize = 4096;
const MAX_AGE: Duration = Duration::from_secs(15);
const RETRY: Duration = Duration::from_millis(50);
const CHUNKS_PER_POLL: usize = 8;

struct Pending {
    id: u64,
    data: Vec<u8>,
    chunk: usize,
    wire: Option<Vec<u8>>,
    queued: Instant,
    replace: Option<String>,
}
#[derive(Default)]
pub struct Outbox {
    packets: VecDeque<Pending>,
    bytes: usize,
    retry_at: Option<Instant>,
    congested: bool,
    history: super::codec::History,
    pub raw_bytes: u64,
    pub wire_bytes: u64,
    pub budget_stalls: u64,
    pub retries: u64,
    pub replaced: u64,
}
#[derive(Debug)]
pub enum Error {
    PacketSize(usize),
    Capacity,
    Expired,
    Send(i32),
    Codec(String),
}
#[derive(Debug, PartialEq)]
pub enum Progress {
    Idle,
    Sending,
    Blocked,
    Recovered,
}
impl Outbox {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn len(&self) -> usize {
        self.packets.len()
    }
    pub fn enqueue(
        &mut self,
        id: u64,
        data: Vec<u8>,
        replace: Option<String>,
        now: Instant,
    ) -> Result<(), Error> {
        self.enqueue_with(id, data, replace, now, |_, new| Some(new.to_vec()))
    }
    pub fn enqueue_with(
        &mut self,
        id: u64,
        mut data: Vec<u8>,
        replace: Option<String>,
        now: Instant,
        merge: impl FnOnce(&[u8], &[u8]) -> Option<Vec<u8>>,
    ) -> Result<(), Error> {
        if data.is_empty() || data.len() > MAX {
            return Err(Error::PacketSize(data.len()));
        }
        // Never cross a control message or a partially sent batch. The merger
        // preserves the combat journal and composes deltas: a stationary
        // enemy may only occur in the older snapshot.
        let candidate = self
            .packets
            .back()
            .filter(|p| p.chunk == 0 && replace.is_some() && p.replace == replace);
        let merged = candidate
            .and_then(|p| merge(&p.data, &data))
            .filter(|v| !v.is_empty() && v.len() <= MAX);
        let old = candidate.filter(|_| merged.is_some());
        let old_bytes = old.map_or(0, |p| p.data.len());
        if let Some(merged) = merged {
            data = merged;
        }
        if self.bytes - old_bytes + data.len() > MAX_QUEUED_BYTES
            || (old.is_none() && self.packets.len() >= MAX_QUEUED_PACKETS)
        {
            return Err(Error::Capacity);
        }
        // Retain the first enqueue time so replacement cannot hide a stalled link.
        let queued = old.map_or(now, |p| p.queued);
        if old.is_some() {
            self.packets.pop_back();
            self.bytes -= old_bytes;
            self.replaced += 1;
        }
        self.bytes += data.len();
        self.packets.push_back(Pending {
            id,
            data,
            chunk: 0,
            wire: None,
            queued,
            replace,
        });
        Ok(())
    }
    pub fn flush_budget(
        &mut self,
        now: Instant,
        mut budget: usize,
        mut send: impl FnMut(&[u8]) -> i32,
    ) -> Result<Progress, Error> {
        if self
            .packets
            .front()
            .is_some_and(|p| now.saturating_duration_since(p.queued) >= MAX_AGE)
        {
            return Err(Error::Expired);
        }
        if self.retry_at.is_some_and(|t| now < t) {
            return Ok(Progress::Idle);
        }
        self.retry_at = None;
        for _ in 0..CHUNKS_PER_POLL {
            let Some(p) = self.packets.front_mut() else {
                return Ok(if std::mem::take(&mut self.congested) {
                    Progress::Recovered
                } else {
                    Progress::Idle
                });
            };
            if budget == 0 {
                self.budget_stalls += 1;
                let first = !self.congested;
                self.congested = true;
                return Ok(if first {
                    Progress::Blocked
                } else {
                    Progress::Idle
                });
            }
            if p.wire.is_none() {
                p.wire = Some(self.history.encode(&p.data).map_err(Error::Codec)?);
            }
            let wire = p.wire.as_ref().unwrap();
            let count = wire.len().div_ceil(CHUNK);
            let start = p.chunk * CHUNK;
            let end = (start + CHUNK).min(wire.len());
            // Permit one chunk into an empty Steam queue (including large
            // definitions). Further chunks wait until there is room again.
            if 20 + end - start > budget {
                self.budget_stalls += 1;
                let first = !self.congested;
                self.congested = true;
                return Ok(if first {
                    Progress::Blocked
                } else {
                    Progress::Idle
                });
            }
            let mut bytes = Vec::with_capacity(20 + end - start);
            bytes.extend_from_slice(b"FMP2");
            bytes.extend_from_slice(&p.id.to_le_bytes());
            bytes.extend_from_slice(&(p.chunk as u16).to_le_bytes());
            bytes.extend_from_slice(&(count as u16).to_le_bytes());
            bytes.extend_from_slice(&(wire.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&wire[start..end]);
            match send(&bytes) {
                1 => {
                    budget = budget.saturating_sub(bytes.len());
                    self.wire_bytes += bytes.len() as u64;
                    p.chunk += 1;
                    if p.chunk == count {
                        self.history.commit(p.id, &p.data);
                        self.raw_bytes += p.data.len() as u64;
                        self.bytes -= p.data.len();
                        self.packets.pop_front();
                    }
                }
                25 => {
                    self.retries += 1;
                    self.retry_at = Some(now + RETRY);
                    let first = !self.congested;
                    self.congested = true;
                    return Ok(if first {
                        Progress::Blocked
                    } else {
                        Progress::Idle
                    });
                }
                result => return Err(Error::Send(result)),
            }
        }
        Ok(Progress::Sending)
    }
}
