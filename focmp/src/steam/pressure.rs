//! Valve's internal queue is separate from Outbox. Leave excess snapshots in
//! Outbox, where they can still be compacted, instead of buffering seconds in GNS.
use super::outbox::CHUNK;
#[repr(C)]
#[derive(Default, Debug)]
pub(super) struct Status {
    pub state: i32,
    pub ping: i32,
    quality_local: f32,
    quality_remote: f32,
    out_packets: f32,
    pub out_bytes: f32,
    in_packets: f32,
    in_bytes: f32,
    pub send_rate: i32,
    pub pending_unreliable: i32,
    pub pending_reliable: i32,
    pub unacked_reliable: i32,
    pub queue_us: i64,
    // Older Steam SDKs reserve all 16 words; newer ones use the first for jitter.
    reserved: [u32; 16],
}
const _: () = assert!(std::mem::size_of::<Status>() == 120);
impl Status {
    pub fn budget(&self) -> usize {
        let pending =
            self.pending_reliable.max(0) as usize + self.pending_unreliable.max(0) as usize;
        if pending == 0 {
            return CHUNK + 20;
        }
        if self.queue_us > 100_000 {
            return 0;
        }
        let target = (self.send_rate.max(0) as usize / 10).clamp(4096, 64 * 1024);
        target.saturating_sub(pending)
    }
}
