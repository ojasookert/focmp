//! Bounded packet compression. The previous accepted packet is a dictionary,
//! removing repeated enemy/light metadata without changing simulation values.
//! This is only used on Steam's reliable, ordered channel, independently per peer.
use super::outbox::MAX;
const DICTIONARY: usize = 64 * 1024;
pub const WIRE_MAX: usize = MAX + 13;
#[derive(Default)]
pub(super) struct History {
    id: u64,
    packets: u64,
    data: Vec<u8>,
}
impl History {
    pub fn commit(&mut self, id: u64, data: &[u8]) {
        self.id = id;
        self.packets += 1;
        self.data = data[data.len().saturating_sub(DICTIONARY)..].to_vec();
    }
    pub fn encode(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.is_empty() || data.len() > MAX {
            return Err("invalid uncompressed size".into());
        }
        // An independent frame every 64 accepted messages bounds dependency
        // chains. Reconnects start with a fresh History as well.
        let dict = if self.packets % 64 == 0 {
            &[][..]
        } else {
            self.data.as_slice()
        };
        let mut compressed =
            zstd::bulk::Compressor::with_dictionary(1, dict).map_err(|e| e.to_string())?;
        compressed
            .include_checksum(true)
            .map_err(|e| e.to_string())?;
        let compressed = compressed.compress(data).map_err(|e| e.to_string())?;
        let useful = compressed.len() < data.len();
        let kind = if !useful {
            0
        } else if dict.is_empty() {
            1
        } else {
            2
        };
        let base = if kind == 2 { self.id } else { 0 };
        let mut out = Vec::with_capacity(13 + if useful { compressed.len() } else { data.len() });
        out.push(kind);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&base.to_le_bytes());
        out.extend_from_slice(if useful { &compressed } else { data });
        Ok(out)
    }
    pub fn decode(&mut self, id: u64, wire: &[u8]) -> Result<Vec<u8>, String> {
        if id <= self.id || wire.len() < 14 || wire.len() > WIRE_MAX {
            return Err("invalid packet sequence/size".into());
        }
        let size = u32::from_le_bytes(wire[1..5].try_into().unwrap()) as usize;
        let base = u64::from_le_bytes(wire[5..13].try_into().unwrap());
        if size == 0 || size > MAX {
            return Err("invalid expanded size".into());
        }
        let data = match wire[0] {
            0 if base == 0 && wire.len() == size + 13 => wire[13..].to_vec(),
            1 | 2
                if (wire[0] == 1 && base == 0)
                    || (wire[0] == 2 && base == self.id && base != 0) =>
            {
                let dict = if wire[0] == 2 {
                    self.data.as_slice()
                } else {
                    &[]
                };
                let mut d =
                    zstd::bulk::Decompressor::with_dictionary(dict).map_err(|e| e.to_string())?;
                // Bound both output allocation and the frame's advertised window.
                d.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(20))
                    .map_err(|e| e.to_string())?;
                d.decompress(&wire[13..], size).map_err(|e| e.to_string())?
            }
            _ => return Err("invalid compression mode/baseline".into()),
        };
        if data.len() != size {
            return Err("expanded size mismatch".into());
        }
        self.commit(id, &data);
        Ok(data)
    }
}
