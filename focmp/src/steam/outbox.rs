//! Bounded reliable sends. A rejected chunk stays at the head until accepted;
//! accepted chunks are never resent and later packets cannot overtake them.
use std::{collections::VecDeque, time::{Duration, Instant}};

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
    queued: Instant,
    replace: Option<String>,
}
#[derive(Default)]
pub struct Outbox {
    packets: VecDeque<Pending>,
    bytes: usize,
    retry_at: Option<Instant>,
    congested: bool,
    pub retries: u64,
    pub replaced: u64,
}
#[derive(Debug)]
pub enum Error {
    PacketSize(usize),
    Capacity,
    Expired,
    Send(i32),
}
#[derive(Debug, PartialEq)]
pub enum Progress { Idle, Sending, Blocked, Recovered }
impl Outbox {
    pub fn bytes(&self) -> usize { self.bytes }
    pub fn len(&self) -> usize { self.packets.len() }
    pub fn enqueue(&mut self, id: u64, data: Vec<u8>, replace: Option<String>, now: Instant) -> Result<(), Error> {
        self.enqueue_with(id, data, replace, now, |_, new| Some(new.to_vec()))
    }
    pub fn enqueue_with(&mut self, id: u64, mut data: Vec<u8>, replace: Option<String>, now: Instant,
        merge: impl FnOnce(&[u8], &[u8]) -> Option<Vec<u8>>) -> Result<(), Error> {
        if data.is_empty() || data.len() > MAX { return Err(Error::PacketSize(data.len())); }
        // Never cross an event or a partially sent packet. Delta snapshots must
        // be merged, not replaced: a stationary enemy may only occur in the old one.
        let candidate = self.packets.back().filter(|p| p.chunk == 0 && replace.is_some() && p.replace == replace);
        let merged = candidate.and_then(|p| merge(&p.data, &data)).filter(|v| !v.is_empty() && v.len() <= MAX);
        let old = candidate.filter(|_| merged.is_some());
        let old_bytes = old.map_or(0, |p| p.data.len());
        if let Some(merged) = merged { data = merged; }
        if self.bytes - old_bytes + data.len() > MAX_QUEUED_BYTES
            || (old.is_none() && self.packets.len() >= MAX_QUEUED_PACKETS) {
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
        self.packets.push_back(Pending { id, data, chunk: 0, queued, replace });
        Ok(())
    }
    pub fn flush(&mut self, now: Instant, mut send: impl FnMut(&[u8]) -> i32) -> Result<Progress, Error> {
        if self.packets.front().is_some_and(|p| now.saturating_duration_since(p.queued) >= MAX_AGE) {
            return Err(Error::Expired);
        }
        if self.retry_at.is_some_and(|t| now < t) { return Ok(Progress::Idle); }
        self.retry_at = None;
        for _ in 0..CHUNKS_PER_POLL {
            let Some(p) = self.packets.front_mut() else {
                return Ok(if std::mem::take(&mut self.congested) { Progress::Recovered } else { Progress::Idle });
            };
            let count = p.data.len().div_ceil(CHUNK);
            let start = p.chunk * CHUNK;
            let end = (start + CHUNK).min(p.data.len());
            let mut bytes = Vec::with_capacity(20 + end - start);
            bytes.extend_from_slice(b"FMP1");
            bytes.extend_from_slice(&p.id.to_le_bytes());
            bytes.extend_from_slice(&(p.chunk as u16).to_le_bytes());
            bytes.extend_from_slice(&(count as u16).to_le_bytes());
            bytes.extend_from_slice(&(p.data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&p.data[start..end]);
            match send(&bytes) {
                1 => {
                    p.chunk += 1;
                    if p.chunk == count {
                        self.bytes -= p.data.len();
                        self.packets.pop_front();
                    }
                }
                25 => {
                    self.retries += 1;
                    self.retry_at = Some(now + RETRY);
                    let first = !self.congested;
                    self.congested = true;
                    return Ok(if first { Progress::Blocked } else { Progress::Idle });
                }
                result => return Err(Error::Send(result)),
            }
        }
        Ok(Progress::Sending)
    }
}
