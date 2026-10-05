//! Files larger than one payload, sent as several frames with Cauchy Reed–Solomon erasure
//! coding: any `blocks` of the frames sent rebuild the file, so frames lost to fading are
//! made up for by sending a few extra.
//!
//! Each frame's payload starts with a 14-byte header — `"CRS"`, blocks − 1 (u16), the
//! frame's ident (u16, ≥ blocks), file size − 1 (u24), CRC-32 of the file (u32), all
//! little-endian — followed by the frame's coded block. The splitting is aicodix/crs
//! `encode.cc` with chunks of the payload's size: 5380 bytes for COFDMTV pictures, where
//! the reassembly follows Assempix (`MainActivity` `decodePayload`, `crsec.hh`, at most
//! 12 blocks, 64392 bytes); a modem mode's datagram size for files over the modem (at
//! most 1024 blocks, as `crs/encode` allows).

use super::IMAGE_BYTES;
use crate::coding::crc::{Crc32, POLY_DATA};
use crate::coding::crs;

/// Header bytes of a frame.
pub const OVERHEAD: usize = 3 + 2 + 2 + 3 + 4;
/// File bytes one COFDMTV picture frame carries.
pub const BLOCK_DATA: usize = block_data(IMAGE_BYTES);
/// Most blocks Assempix accepts.
pub const MAX_BLOCKS: usize = 12;
/// Largest file Assempix accepts.
pub const MAX_BYTES: usize = BLOCK_DATA * MAX_BLOCKS;
/// Most blocks `crs/encode` makes (files over the modem).
pub const MAX_BLOCKS_ANY: usize = 1024;

/// File bytes a frame of `chunk` payload bytes carries.
pub const fn block_data(chunk: usize) -> usize {
    (chunk - OVERHEAD) & !1
}

/// The header of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Data blocks: frames needed to rebuild the file.
    pub blocks: usize,
    pub ident: u16,
    /// File size, bytes.
    pub size: usize,
    /// CRC-32 of the file.
    pub crc: u32,
}

impl Header {
    /// The header of `payload`, if it is a multi-frame payload.
    pub fn parse(payload: &[u8]) -> Option<Header> {
        if payload.len() < OVERHEAD || &payload[..3] != b"CRS" {
            return None;
        }
        Some(Header {
            blocks: usize::from(u16::from_le_bytes([payload[3], payload[4]])) + 1,
            ident: u16::from_le_bytes([payload[5], payload[6]]),
            size: (usize::from(payload[7]) | usize::from(payload[8]) << 8 | usize::from(payload[9]) << 16) + 1,
            crc: u32::from_le_bytes([payload[10], payload[11], payload[12], payload[13]]),
        })
    }
}

/// Data blocks (frames needed) for a file of `size` bytes in COFDMTV picture frames.
pub fn blocks_for(size: usize) -> usize {
    blocks_in(size, IMAGE_BYTES)
}

/// Data blocks for a file of `size` bytes in frames of `chunk` payload bytes.
pub fn blocks_in(size: usize, chunk: usize) -> usize {
    size.div_ceil(block_data(chunk)).max(1)
}

/// Split `file` into `frames` COFDMTV picture payloads ([`IMAGE_BYTES`] each; `frames` ≥
/// [`blocks_for`]`(file.len())`, extra frames add redundancy), as Assempix accepts them.
pub fn split(file: &[u8], frames: usize) -> Result<Vec<Vec<u8>>, String> {
    if file.len() > MAX_BYTES {
        return Err(format!("{} bytes: multi-frame pictures hold 1…{MAX_BYTES} bytes", file.len()));
    }
    split_chunks(file, frames, IMAGE_BYTES)
}

/// Split `file` into `frames` payloads of `chunk` bytes (aicodix/crs `encode.cc`).
pub fn split_chunks(file: &[u8], frames: usize, chunk: usize) -> Result<Vec<Vec<u8>>, String> {
    if chunk <= OVERHEAD + 1 {
        return Err(format!("{chunk}-byte frames are too small"));
    }
    let max = block_data(chunk) * MAX_BLOCKS_ANY;
    if file.is_empty() || file.len() > max || file.len() > 1 << 24 {
        return Err(format!("{} bytes: frames of {chunk} bytes hold files of 1…{} bytes", file.len(), max.min(1 << 24)));
    }
    let blocks = blocks_in(file.len(), chunk);
    if frames < blocks {
        return Err(format!("{} bytes need at least {blocks} frames", file.len()));
    }
    if blocks + frames > 65536 {
        return Err("too many frames".into());
    }
    let dirty = file.len().div_ceil(blocks);
    let block_bytes = dirty.div_ceil(64) * 64;
    let mut data = vec![0u8; blocks * block_bytes];
    for (i, chunk) in file.chunks(dirty).enumerate() {
        data[i * block_bytes..i * block_bytes + chunk.len()].copy_from_slice(chunk);
    }
    let mut crc = Crc32::new(POLY_DATA);
    let sum = crc.bytes(file);
    let size = (file.len() - 1) as u32;
    let mut block = vec![0u8; block_bytes];
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let ident = (blocks + i) as u16;
        crs::encode(&data, &mut block, ident, block_bytes);
        let mut payload = vec![0u8; chunk];
        payload[..3].copy_from_slice(b"CRS");
        payload[3..5].copy_from_slice(&((blocks - 1) as u16).to_le_bytes());
        payload[5..7].copy_from_slice(&ident.to_le_bytes());
        payload[7..10].copy_from_slice(&size.to_le_bytes()[..3]);
        payload[10..14].copy_from_slice(&sum.to_le_bytes());
        let copy = dirty + (dirty & 1);
        payload[OVERHEAD..OVERHEAD + copy].copy_from_slice(&block[..copy]);
        out.push(payload);
    }
    Ok(out)
}

/// What a received frame did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Not a multi-frame payload.
    NotMultiFrame,
    /// A header Assempix would not accept.
    Unsupported,
    /// This frame was received before.
    Duplicate,
    /// The file is complete already.
    Redundant,
    /// `have` of `need` frames.
    Partial { have: usize, need: usize },
    /// The file, rebuilt.
    Complete(Vec<u8>),
    /// Enough frames, but the file's CRC fails (frames of different files mixed?).
    Corrupted,
}

/// Collects frames of one file at a time (a frame of another file starts afresh).
#[derive(Debug, Clone)]
pub struct Reassembler {
    /// Payload bytes of a frame, and the most blocks accepted.
    chunk: usize,
    max_blocks: usize,
    current: Option<Header>,
    idents: Vec<u16>,
    slots: Vec<u8>,
}

impl Default for Reassembler {
    /// For COFDMTV pictures, as Assempix accepts them.
    fn default() -> Self {
        Self::new(IMAGE_BYTES, MAX_BLOCKS)
    }
}

impl Reassembler {
    /// For frames of `chunk` payload bytes, files of at most `max_blocks` blocks.
    pub fn new(chunk: usize, max_blocks: usize) -> Self {
        Self { chunk, max_blocks, current: None, idents: Vec::new(), slots: Vec::new() }
    }

    /// Payload bytes of the frames this collects.
    pub fn chunk(&self) -> usize {
        self.chunk
    }

    /// The file being collected and the frames in hand.
    pub fn state(&self) -> Option<(Header, usize)> {
        self.current.map(|h| (h, self.idents.len()))
    }

    pub fn clear(&mut self) {
        *self = Self::new(self.chunk, self.max_blocks);
    }

    pub fn push(&mut self, payload: &[u8]) -> Progress {
        let Some(h) = Header::parse(payload) else { return Progress::NotMultiFrame };
        let slot = block_data(self.chunk);
        if payload.len() < self.chunk || h.blocks > self.max_blocks || usize::from(h.ident) < h.blocks || h.size > slot * h.blocks {
            return Progress::Unsupported;
        }
        let same = self.current.is_some_and(|c| c.blocks == h.blocks && c.size == h.size && c.crc == h.crc);
        if !same {
            self.current = Some(h);
            self.idents.clear();
            self.slots = vec![0; h.blocks * slot];
        }
        if self.idents.contains(&h.ident) {
            return Progress::Duplicate;
        }
        if self.idents.len() == h.blocks {
            return Progress::Redundant;
        }
        let at = self.idents.len() * slot;
        self.slots[at..at + slot].copy_from_slice(&payload[OVERHEAD..OVERHEAD + slot]);
        self.idents.push(h.ident);
        if self.idents.len() < h.blocks {
            return Progress::Partial { have: self.idents.len(), need: h.blocks };
        }
        let mut data = vec![0u8; h.blocks * slot];
        crs::decode(&mut data, &self.slots, &self.idents, slot);
        let copy = h.size.div_ceil(h.blocks);
        let mut file = Vec::with_capacity(h.size);
        for i in 0..h.blocks {
            let n = copy.min(h.size - file.len());
            file.extend_from_slice(&data[i * slot..i * slot + n]);
        }
        let mut crc = Crc32::new(POLY_DATA);
        if crc.bytes(&file) == h.crc {
            // Kept, as in Assempix: more frames of this file are redundant.
            Progress::Complete(file)
        } else {
            self.current = None;
            self.idents.clear();
            Progress::Corrupted
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::xorshift::Xorshift32;

    #[test]
    fn any_enough_frames_rebuild_the_file() {
        let mut rng = Xorshift32::default();
        for size in [1, 5366, 5367, 20000, MAX_BYTES] {
            let file: Vec<u8> = (0..size).map(|_| rng.next() as u8).collect();
            let blocks = blocks_for(size);
            let frames = split(&file, blocks + 3).unwrap();
            assert!(frames.iter().all(|f| f.len() == IMAGE_BYTES));
            // Lose the first two frames.
            let mut r = Reassembler::default();
            let mut last = Progress::NotMultiFrame;
            for f in &frames[2..2 + blocks] {
                last = r.push(f);
            }
            assert_eq!(last, Progress::Complete(file.clone()), "size {size}");
            // Frames of a completed file are redundant (Assempix's "chunk_redundant").
            assert_eq!(r.push(&frames[0]), Progress::Redundant);
        }
    }

    #[test]
    fn duplicates_and_limits() {
        let file = vec![7u8; 9000];
        let frames = split(&file, 3).unwrap();
        let mut r = Reassembler::default();
        assert_eq!(r.push(&frames[0]), Progress::Partial { have: 1, need: 2 });
        assert_eq!(r.push(&frames[0]), Progress::Duplicate);
        assert_eq!(r.push(&frames[2]), Progress::Complete(file));
        assert_eq!(r.push(&frames[1]), Progress::Redundant);
        assert!(split(&vec![0; MAX_BYTES + 1], 13).is_err());
        assert!(split(&[1, 2, 3], 0).is_err());
        assert_eq!(r.push(&[0u8; IMAGE_BYTES]), Progress::NotMultiFrame);
    }

    #[test]
    fn modem_sized_chunks() {
        // QPSK 2/3 short frames hold 171 bytes (odd): 156 file bytes each.
        let mut rng = Xorshift32::default();
        let file: Vec<u8> = (0..2000).map(|_| rng.next() as u8).collect();
        let blocks = blocks_in(file.len(), 171);
        assert_eq!(blocks, 13);
        let frames = split_chunks(&file, blocks + 2, 171).unwrap();
        assert!(frames.iter().all(|f| f.len() == 171));
        let mut r = Reassembler::new(171, MAX_BLOCKS_ANY);
        let mut last = Progress::NotMultiFrame;
        for f in frames.iter().rev().take(blocks) {
            last = r.push(f);
        }
        assert_eq!(last, Progress::Complete(file));
    }
}
