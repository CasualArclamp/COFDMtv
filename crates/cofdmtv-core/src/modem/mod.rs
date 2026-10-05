//! The aicodix modem (github.com/aicodix/modem): datagrams over audio.
//!
//! # Signal
//!
//! OFDM at 44.1 or 48 kHz with 7.5 Hz carrier spacing (symbols of 40 guard intervals of
//! 1/300 s), 320 tones (2400 Hz) around a carrier frequency in steps of 300 Hz. Every
//! fifth tone is a pilot ("seed tone", 64 of them, their positions shifting by three from
//! symbol to symbol); the other 256 carry data. A transmission is a noise symbol, then per
//! datagram ("frame"):
//!
//! 1. two identical sync symbols back to back (no guard interval between them): Schmidl &
//!    Cox timing and frequency, then the channel estimate;
//! 2. a meta symbol: call sign (base 40) and mode in 56 bits plus a CRC-16, a polar code of
//!    256 bits on the data tones in BPSK;
//! 3. the payload symbols: the datagram (scrambled, CRC-32) in a polar code of 2^11…2^16
//!    bits, permuted, in BPSK…QAM4096.
//!
//! Each symbol after the sync is scrambled with one of 128 patterns, the one with the
//! lowest peak-to-average power ratio; its number goes in a Hadamard code on the pilots.
//! The receiver demodulates coherently against the sync symbols' channel estimate, which
//! the pilots keep up to date.

mod decoder;
mod encoder;

pub use decoder::{ModemCodeword, ModemDecoder, ModemEvent, Datagram, decode_datagram};
pub use encoder::{ModemEncoder, ModemRequest};

use crate::coding::psk::Psk;
use crate::coding::qam::Qam;
use crate::coding::tables as t;
use crate::dsp::Cplx;

/// Sample rates the modem is defined for.
pub const RATES: [u32; 2] = [44100, 48000];
pub(crate) const DATA_TONES: usize = 256;
pub(crate) const SEED_TONES: usize = 64;
pub(crate) const TONE_COUNT: usize = DATA_TONES + SEED_TONES;
pub(crate) const BLOCK_LENGTH: usize = 5;
pub(crate) const BLOCK_SKEW: usize = 3;
pub(crate) const FIRST_SEED: usize = 4;
pub(crate) const MLS0_POLY: u32 = 0x331;
pub(crate) const MLS0_SEED: u32 = 214;
pub(crate) const MLS1_POLY: u32 = 0x43;
pub(crate) const MLS2_POLY: u32 = 0x163;
/// Bandwidth, Hz.
pub const BANDWIDTH_HZ: i32 = 2400;
/// Carrier frequencies are multiples of this, Hz.
pub const CARRIER_STEP_HZ: i32 = 300;
/// The original's usual carrier frequency, Hz.
pub const DEFAULT_CARRIER_HZ: i32 = 1500;

/// Carrier frequencies (multiples of [`CARRIER_STEP_HZ`] in this range) that keep the band
/// within a `rate` signal, as the original's encoder allows them: from half the bandwidth
/// up for a real signal, symmetric around 0 Hz for I/Q.
pub fn carrier_range(rate: u32, iq: bool) -> std::ops::RangeInclusive<i32> {
    let max = (rate as i32 / 2 - BANDWIDTH_HZ / 2) / CARRIER_STEP_HZ * CARRIER_STEP_HZ;
    let min = if iq { -max } else { BANDWIDTH_HZ / 2 };
    min..=max
}

/// The pilot position of symbol `j` (0 = the meta symbol).
pub(crate) fn seed_off(j: usize) -> usize {
    (BLOCK_SKEW * j + FIRST_SEED) % BLOCK_LENGTH
}

/// Sizes at one sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModemLayout {
    pub rate: u32,
    pub guard_len: usize,
    pub symbol_len: usize,
    pub extended_len: usize,
}

impl ModemLayout {
    pub fn new(rate: u32) -> Option<ModemLayout> {
        RATES.contains(&rate).then(|| {
            let guard_len = (rate / 300) as usize;
            let symbol_len = 40 * guard_len;
            ModemLayout { rate, guard_len, symbol_len, extended_len: symbol_len + guard_len }
        })
    }

    #[inline]
    pub(crate) fn bin(&self, carrier: i32) -> usize {
        carrier.rem_euclid(self.symbol_len as i32) as usize
    }
}

/// Modulations, as numbered in the mode byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modulation {
    Bpsk,
    Qpsk,
    Psk8,
    Qam16,
    Qam64,
    Qam256,
    Qam1024,
    Qam4096,
}

impl Modulation {
    pub const ALL: [Modulation; 8] =
        [Self::Bpsk, Self::Qpsk, Self::Psk8, Self::Qam16, Self::Qam64, Self::Qam256, Self::Qam1024, Self::Qam4096];

    pub fn bits(self) -> usize {
        match self {
            Self::Bpsk => 1,
            Self::Qpsk => 2,
            Self::Psk8 => 3,
            Self::Qam16 => 4,
            Self::Qam64 => 6,
            Self::Qam256 => 8,
            Self::Qam1024 => 10,
            Self::Qam4096 => 12,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bpsk => "BPSK",
            Self::Qpsk => "QPSK",
            Self::Psk8 => "8PSK",
            Self::Qam16 => "QAM16",
            Self::Qam64 => "QAM64",
            Self::Qam256 => "QAM256",
            Self::Qam1024 => "QAM1024",
            Self::Qam4096 => "QAM4096",
        }
    }
}

/// Code rates, as numbered in the mode byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeRate {
    Half,
    TwoThirds,
    ThreeQuarters,
    FiveSixths,
}

impl CodeRate {
    pub const ALL: [CodeRate; 4] = [Self::Half, Self::TwoThirds, Self::ThreeQuarters, Self::FiveSixths];

    pub fn name(self) -> &'static str {
        match self {
            Self::Half => "1/2",
            Self::TwoThirds => "2/3",
            Self::ThreeQuarters => "3/4",
            Self::FiveSixths => "5/6",
        }
    }
}

/// A modem mode: modulation, code rate and frame size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModemMode {
    pub modulation: Modulation,
    pub rate: CodeRate,
    /// Normal (long) frames rather than short ones.
    pub normal: bool,
}

impl ModemMode {
    /// All 64 modes: modulations, code rates, short and normal frames.
    pub fn all() -> impl Iterator<Item = ModemMode> {
        Modulation::ALL.into_iter().flat_map(|modulation| {
            CodeRate::ALL.into_iter().flat_map(move |rate| [false, true].map(|normal| ModemMode { modulation, rate, normal }))
        })
    }

    /// Bits per second of a frame's data over the frame's duration (as the original reports).
    pub fn bitrate(self) -> f64 {
        self.data_bits() as f64 / self.duration_s()
    }

    /// The mode in the meta data's byte (bit 7, "analog", is not supported).
    pub fn from_byte(b: u8) -> Option<ModemMode> {
        if b & 0x80 != 0 {
            return None;
        }
        let modulation = Modulation::ALL[usize::from((b >> 4) & 7)];
        let rate = *CodeRate::ALL.get(usize::from((b >> 1) & 7))?;
        Some(ModemMode { modulation, rate, normal: b & 1 != 0 })
    }

    pub fn byte(self) -> u8 {
        let m = Modulation::ALL.iter().position(|&m| m == self.modulation).unwrap_or(0) as u8;
        let r = CodeRate::ALL.iter().position(|&r| r == self.rate).unwrap_or(0) as u8;
        (m << 4) | (r << 1) | u8::from(self.normal)
    }

    /// Payload symbols and the code's order (log2 of its length).
    pub fn shape(self) -> (usize, u32) {
        let (symbols, order) = match self.modulation {
            Modulation::Bpsk => (8, 11),
            Modulation::Qpsk => (4, 11),
            Modulation::Psk8 => (11, 13),
            Modulation::Qam16 => (4, 12),
            Modulation::Qam64 => (11, 14),
            Modulation::Qam256 => (8, 14),
            Modulation::Qam1024 => (13, 15),
            Modulation::Qam4096 => (11, 15),
        };
        if !self.normal {
            (symbols, order)
        } else if symbols == 4 {
            (symbols * 4, order + 2)
        } else {
            (symbols * 2, order + 1)
        }
    }

    pub fn symbols(self) -> usize {
        self.shape().0
    }

    pub fn code_order(self) -> u32 {
        self.shape().1
    }

    /// Data bits of a frame.
    pub fn data_bits(self) -> usize {
        let base = match self.rate {
            CodeRate::Half => 1024,
            CodeRate::TwoThirds => 1368,
            CodeRate::ThreeQuarters => 1536,
            CodeRate::FiveSixths => 1704,
        };
        base << (self.code_order() - 11)
    }

    pub fn data_bytes(self) -> usize {
        self.data_bits() / 8
    }

    /// The frozen bits of the payload code.
    pub fn frozen(self) -> &'static [u32] {
        match (self.rate, self.code_order()) {
            (CodeRate::Half, 11) => &t::MODEM_FROZEN_2048_1056,
            (CodeRate::Half, 12) => &t::MODEM_FROZEN_4096_2080,
            (CodeRate::Half, 13) => &t::MODEM_FROZEN_8192_4128,
            (CodeRate::Half, 14) => &t::MODEM_FROZEN_16384_8224,
            (CodeRate::Half, 15) => &t::MODEM_FROZEN_32768_16416,
            (CodeRate::Half, _) => &t::MODEM_FROZEN_65536_32800,
            (CodeRate::TwoThirds, 11) => &t::MODEM_FROZEN_2048_1400,
            (CodeRate::TwoThirds, 12) => &t::MODEM_FROZEN_4096_2768,
            (CodeRate::TwoThirds, 13) => &t::MODEM_FROZEN_8192_5504,
            (CodeRate::TwoThirds, 14) => &t::MODEM_FROZEN_16384_10976,
            (CodeRate::TwoThirds, 15) => &t::MODEM_FROZEN_32768_21920,
            (CodeRate::TwoThirds, _) => &t::MODEM_FROZEN_65536_43808,
            (CodeRate::ThreeQuarters, 11) => &t::MODEM_FROZEN_2048_1568,
            (CodeRate::ThreeQuarters, 12) => &t::MODEM_FROZEN_4096_3104,
            (CodeRate::ThreeQuarters, 13) => &t::MODEM_FROZEN_8192_6176,
            (CodeRate::ThreeQuarters, 14) => &t::MODEM_FROZEN_16384_12320,
            (CodeRate::ThreeQuarters, 15) => &t::MODEM_FROZEN_32768_24608,
            (CodeRate::ThreeQuarters, _) => &t::MODEM_FROZEN_65536_49184,
            (CodeRate::FiveSixths, 11) => &t::MODEM_FROZEN_2048_1736,
            (CodeRate::FiveSixths, 12) => &t::MODEM_FROZEN_4096_3440,
            (CodeRate::FiveSixths, 13) => &t::MODEM_FROZEN_8192_6848,
            (CodeRate::FiveSixths, 14) => &t::MODEM_FROZEN_16384_13664,
            (CodeRate::FiveSixths, 15) => &t::MODEM_FROZEN_32768_27296,
            (CodeRate::FiveSixths, _) => &t::MODEM_FROZEN_65536_54560,
        }
    }

    /// Seconds of one frame (sync, meta, payload) plus the noise symbol, as the original
    /// reports it.
    pub fn duration_s(self) -> f64 {
        41.0 / 300.0 * (3 + self.symbols()) as f64
    }

    /// Seconds a frame adds after the first.
    pub fn frame_s(self) -> f64 {
        41.0 / 300.0 * (2 + self.symbols()) as f64
    }

    pub fn label(self) -> String {
        format!("{} {} {}", self.modulation.name(), self.rate.name(), if self.normal { "normal" } else { "short" })
    }
}

/// Bits carried by the tone at code bit `k` (the original uses a smaller constellation now
/// and then so that the bits fill the code exactly).
pub(crate) fn tone_bits(mod_bits: usize, k: usize) -> usize {
    match mod_bits {
        3 if k % 32 == 30 => 2,
        6 if k % 64 == 60 => 4,
        10 | 12 if k % 128 == 120 => 8,
        b => b,
    }
}

/// Point of the ±1 bits `b` with `bits` bits per tone.
pub(crate) fn map_bits(b: &[f32], bits: usize) -> Cplx {
    match bits {
        1 => Psk::Bpsk.map(b),
        2 => Psk::Qpsk.map(b),
        3 => Psk::Psk8.map(b),
        n => Qam::new(n).map(b),
    }
}

pub(crate) fn demap_hard(b: &mut [f32], c: Cplx, bits: usize) {
    match bits {
        1 => Psk::Bpsk.hard(b, c),
        2 => Psk::Qpsk.hard(b, c),
        3 => Psk::Psk8.hard(b, c),
        n => Qam::new(n).hard(b, c),
    }
}

pub(crate) fn demap_soft(b: &mut [f32], c: Cplx, precision: f32, bits: usize) {
    match bits {
        1 => Psk::Bpsk.soft(b, c, precision),
        2 => Psk::Qpsk.soft(b, c, precision),
        3 => Psk::Psk8.soft(b, c, precision),
        n => Qam::new(n).soft(b, c, precision),
    }
}

/// The shift constants of the code-bit permutation, by code order.
fn shuffle_shifts(order: u32) -> (u32, u32, u32) {
    match order {
        8 => (1, 1, 2),
        11 => (1, 3, 4),
        12 => (1, 1, 4),
        13 => (1, 1, 9),
        14 => (1, 5, 10),
        15 => (1, 1, 3),
        _ => (1, 1, 14),
    }
}

/// Permute the code bits for sending: `dest[i] = src[seq_i]` (`shuffle` in encode.cc).
pub(crate) fn shuffle<T: Copy>(dest: &mut [T], src: &[T], order: u32) {
    let (a, b, c) = shuffle_shifts(order);
    let mut seq = crate::coding::xorshift::XorShiftMask::new(order, a, b, c);
    dest[0] = src[0];
    for d in dest.iter_mut().take(1 << order).skip(1) {
        *d = src[seq.next() as usize];
    }
}

/// Undo [`shuffle`]: `dest[seq_i] = src[i]` (`shuffle` in decode.cc).
pub(crate) fn unshuffle<T: Copy>(dest: &mut [T], src: &[T], order: u32) {
    let (a, b, c) = shuffle_shifts(order);
    let mut seq = crate::coding::xorshift::XorShiftMask::new(order, a, b, c);
    dest[0] = src[0];
    for s in src.iter().take(1 << order).skip(1) {
        dest[seq.next() as usize] = *s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_match_the_originals_table() {
        // (modulation, normal, rate) → payload bytes and seconds, from the modem's README.
        let cases = [
            (Modulation::Bpsk, false, CodeRate::Half, 128, 1.5),
            (Modulation::Qpsk, false, CodeRate::Half, 128, 1.0),
            (Modulation::Psk8, false, CodeRate::Half, 512, 1.9),
            (Modulation::Qam16, true, CodeRate::Half, 1024, 2.6),
            (Modulation::Qam1024, true, CodeRate::TwoThirds, 5472, 4.0),
            (Modulation::Qam4096, true, CodeRate::FiveSixths, 6816, 3.4),
            (Modulation::Qam256, false, CodeRate::ThreeQuarters, 1536, 1.5),
        ];
        for (modulation, normal, rate, bytes, seconds) in cases {
            let m = ModemMode { modulation, rate, normal };
            assert_eq!(m.data_bytes(), bytes, "{}", m.label());
            assert!((m.duration_s() - seconds).abs() < 0.06, "{}: {}", m.label(), m.duration_s());
            assert_eq!(ModemMode::from_byte(m.byte()), Some(m));
            // The bits fill the code exactly.
            let mut k = 0;
            for _ in 0..m.symbols() * DATA_TONES {
                k += tone_bits(m.modulation.bits(), k);
            }
            assert_eq!(k, 1 << m.code_order(), "{}", m.label());
            assert_eq!(crate::coding::polar::info_count(m.frozen(), m.code_order()), m.data_bits() + 32);
        }
        assert_eq!(ModemMode::from_byte(0x80), None);
        assert_eq!(ModemMode::from_byte(0b0011_1000), None, "code rate 4 is not defined");
    }

    #[test]
    fn unshuffle_inverts_shuffle() {
        let src: Vec<u32> = (0..256).collect();
        let mut a = vec![0; 256];
        let mut b = vec![0; 256];
        shuffle(&mut a, &src, 8);
        unshuffle(&mut b, &a, 8);
        assert_eq!(b, src);
        assert_ne!(a, src);
    }
}
