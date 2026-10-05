//! Channel coding shared by COFDMTV and the modem: CRCs, sequences, the BCH code of the
//! preamble and its ordered-statistics decoder, polar codes with CRC-aided list decoding,
//! phase-shift keying, Cauchy Reed–Solomon erasure coding and call-sign packing.
//!
//! Ported from aicodix/code (BSD Zero Clause License); the doc comment of each item names
//! the header it came from.

pub mod base37;
pub mod bch;
pub mod bits;
pub mod crc;
pub mod crs;
pub mod mls;
pub mod osd;
pub mod polar;
pub mod psk;
pub mod tables;
pub mod xorshift;

/// NRZ mapping of a bit: 0 → +1, 1 → −1.
#[inline]
pub fn nrz(bit: bool) -> f32 {
    if bit { -1.0 } else { 1.0 }
}
