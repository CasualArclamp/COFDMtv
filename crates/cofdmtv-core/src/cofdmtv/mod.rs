//! COFDMTV: the aicodix picture and text transmission over audio (Shredpix/Assempix for
//! pictures, Rattlegram for text).
//!
//! # Signal
//!
//! OFDM with 160 ms symbols (6.25 Hz carrier spacing) and a 1/8 guard interval, at any of
//! the sample rates 8, 16, 32, 44.1 and 48 kHz, centred on a chosen carrier frequency. A
//! transmission is:
//!
//! 1. optional noise symbols (let a receiver's AGC and a transmitter's VOX settle);
//! 2. the Schmidl–Cox synchronisation symbol (a known sequence on every other carrier,
//!    so that it repeats after half a symbol);
//! 3. the preamble: 71 bits of meta data (call sign, mode, CRC-16) in a (255, 71) BCH
//!    codeword, differentially BPSK modulated over 256 carriers;
//! 4. for pictures (modes 6–13) a pilot symbol, then the payload symbols; for text (modes
//!    14–16) the payload follows the preamble directly; a ping (mode 0) has no payload;
//! 5. the optional "fancy header": the call sign drawn into eleven symbols, readable on a
//!    waterfall display;
//! 6. a symbol of silence.
//!
//! Payload carriers are differentially PSK modulated from symbol to symbol; the payload
//! is one shortened systematic polar code with a CRC-32, decoded by CRC-aided list
//! decoding.

mod decoder;
mod encoder;
pub mod meta;
pub mod multiframe;
mod sync;

pub use decoder::{Codeword, Decoder, Event, IMAGE_LIST, Payload, PolarDecoders, TEXT_LIST, decode_codeword};
pub use encoder::{Encoder, TxRequest};

use crate::coding::polar::ShortCode;
use crate::coding::psk::Psk;
use crate::coding::{crc, tables};

/// Sample rates the signal is defined for.
pub const RATES: [u32; 5] = [8000, 16000, 32000, 44100, 48000];
/// Image payload, bytes (one frame).
pub const IMAGE_BYTES: usize = 5380;
/// Longest text payload, bytes.
pub const TEXT_BYTES: usize = 170;

/// Payload kinds and their parameters ("operation modes" of the apps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Mode {
    /// No payload: just the call sign.
    Ping,
    /// A picture (or any 5380 bytes), modes 6–13.
    Image(u8),
    /// UTF-8 text, modes 14 (170 bytes), 15 (128 bytes) and 16 (85 bytes).
    Text(u8),
}

impl Mode {
    pub const IMAGE_MODES: [u8; 8] = [6, 7, 8, 9, 10, 11, 12, 13];

    /// The mode number carried in the preamble, if COFDMtv knows it.
    pub fn from_number(n: u8) -> Option<Mode> {
        match n {
            0 => Some(Mode::Ping),
            6..=13 => Some(Mode::Image(n)),
            14..=16 => Some(Mode::Text(n)),
            _ => None,
        }
    }

    pub fn number(self) -> u8 {
        match self {
            Mode::Ping => 0,
            Mode::Image(n) | Mode::Text(n) => n,
        }
    }

    /// The text mode that fits `len` bytes (the shortest).
    pub fn text_for(len: usize) -> Option<Mode> {
        match len {
            0 => Some(Mode::Ping),
            1..=85 => Some(Mode::Text(16)),
            86..=128 => Some(Mode::Text(15)),
            129..=TEXT_BYTES => Some(Mode::Text(14)),
            _ => None,
        }
    }

    /// Payload carriers.
    pub fn carriers(self) -> usize {
        match self.number() {
            6 => 432,
            7 | 8 => 400,
            9 => 360,
            10 => 512,
            11 | 12 => 384,
            _ => 256,
        }
    }

    /// Payload symbols.
    pub fn symbols(self) -> usize {
        match self.number() {
            6 => 50,
            7 => 54,
            8 => 81,
            9 => 90,
            10 => 42,
            11 => 56,
            12 => 84,
            13 => 126,
            14..=16 => 4,
            _ => 0,
        }
    }

    pub fn modulation(self) -> Psk {
        match self.number() {
            6 | 7 | 10 | 11 => Psk::Psk8,
            _ => Psk::Qpsk,
        }
    }

    /// Payload bytes.
    pub fn payload_bytes(self) -> usize {
        match self {
            Mode::Ping => 0,
            Mode::Image(_) => IMAGE_BYTES,
            Mode::Text(14) => 170,
            Mode::Text(15) => 128,
            Mode::Text(_) => 85,
        }
    }

    /// The polar code of the payload.
    pub fn code(self) -> Option<ShortCode> {
        Some(match self.number() {
            6..=9 => ShortCode {
                level: 16,
                frozen: &tables::IMAGE_FROZEN_64800_43072,
                mesg_bits: 43808,
                data_bits: IMAGE_BYTES * 8,
                crc_poly: crc::POLY_IMAGE,
            },
            10..=13 => ShortCode {
                level: 16,
                frozen: &tables::IMAGE_FROZEN_64512_43072,
                mesg_bits: 44096,
                data_bits: IMAGE_BYTES * 8,
                crc_poly: crc::POLY_IMAGE,
            },
            14 => ShortCode { level: 11, frozen: &tables::TEXT_FROZEN_2048_1392, mesg_bits: 1392, data_bits: 1360, crc_poly: crc::POLY_DATA },
            15 => ShortCode { level: 11, frozen: &tables::TEXT_FROZEN_2048_1056, mesg_bits: 1056, data_bits: 1024, crc_poly: crc::POLY_DATA },
            16 => ShortCode { level: 11, frozen: &tables::TEXT_FROZEN_2048_712, mesg_bits: 712, data_bits: 680, crc_poly: crc::POLY_DATA },
            _ => return None,
        })
    }

    /// Occupied bandwidth as the apps advertise it, Hz.
    pub fn bandwidth_hz(self) -> u32 {
        match self.number() {
            6 => 2700,
            7 | 8 => 2500,
            9 => 2250,
            10 => 3200,
            11 | 12 => 2400,
            13 => 1600,
            _ => 1600,
        }
    }

    /// Symbols on the air without noise or fancy header: sync, preamble, pilot (pictures),
    /// payload, the closing silence.
    pub fn air_symbols(self) -> usize {
        match self {
            Mode::Image(_) => 3 + self.symbols() + 1,
            Mode::Text(_) => 2 + self.symbols() + 1,
            Mode::Ping => 2 + 1,
        }
    }

    /// Bits per second of payload.
    pub fn bitrate(self) -> f64 {
        8.0 * self.payload_bytes() as f64 / (self.air_symbols() as f64 * SYMBOL_SECONDS)
    }

    /// Carrier frequencies (Hz, 50 Hz steps as in the apps) that keep the signal inside
    /// the band of a `rate` signal: from half the bandwidth up for a real signal (below
    /// that, its lower edge would fold over 0 Hz), symmetric around 0 Hz for I/Q.
    /// `ultrasonic` allows carriers above 3 kHz, as Shredpix does on request.
    pub fn carrier_range(self, rate: u32, iq: bool, ultrasonic: bool) -> std::ops::RangeInclusive<i32> {
        let bw = self.bandwidth_hz().max(1600).div_ceil(100) as i32 * 100;
        let max = if ultrasonic { (rate as i32 - bw) / 2 } else { 3000.min((rate as i32 - bw) / 2) };
        let min = if iq { -max } else { bw / 2 };
        min..=max.max(min)
    }

    pub fn label(self) -> String {
        match self {
            Mode::Ping => "Ping".into(),
            Mode::Image(n) | Mode::Text(n) => {
                let m = match self.modulation() {
                    Psk::Psk8 => "8PSK",
                    Psk::Qpsk => "QPSK",
                    Psk::Bpsk => "BPSK",
                };
                format!("{n} ({m}, {} Hz)", self.bandwidth_hz())
            }
        }
    }
}

/// Duration of one symbol with its guard interval, seconds (180 ms).
pub const SYMBOL_SECONDS: f64 = 0.18;

/// Sizes of the signal at one sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub rate: u32,
    /// FFT size: 160 ms.
    pub symbol_len: usize,
    pub guard_len: usize,
    pub extended_len: usize,
}

impl Layout {
    pub fn new(rate: u32) -> Option<Layout> {
        RATES.contains(&rate).then(|| {
            let symbol_len = (1280 * rate / 8000) as usize;
            let guard_len = symbol_len / 8;
            Layout { rate, symbol_len, guard_len, extended_len: symbol_len + guard_len }
        })
    }

    /// Carrier spacing, Hz (6.25).
    pub fn spacing_hz(&self) -> f64 {
        f64::from(self.rate) / self.symbol_len as f64
    }

    /// FFT bin of carrier `carrier` relative to `offset` (both in carriers).
    #[inline]
    pub fn bin(&self, carrier: i32) -> usize {
        carrier.rem_euclid(self.symbol_len as i32) as usize
    }
}

/// Sequences and offsets of the sync symbol and the preamble.
pub(crate) const COR_SEQ_LEN: i32 = 127;
pub(crate) const COR_SEQ_OFF: i32 = 1 - COR_SEQ_LEN;
pub(crate) const COR_SEQ_POLY: u32 = 0b1000_1001;
pub(crate) const PRE_SEQ_LEN: i32 = 255;
pub(crate) const PRE_SEQ_OFF: i32 = -PRE_SEQ_LEN / 2;
pub(crate) const PRE_SEQ_POLY: u32 = 0b1_0010_1011;
pub(crate) const PILOT_POLY: u32 = 0b1_0010_1011;
pub(crate) const NOISE_POLY: u32 = 0b1001_0101_0001;
/// First carrier of the fancy header (9 characters × 8 pixels, every third carrier).
pub(crate) const FANCY_OFF: i32 = -(8 * 9 * 3) / 2;
/// Text payload carriers start here (the preamble's 256 carriers).
pub(crate) const TEXT_CAR_OFF: i32 = -128;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_fill_their_codes() {
        for m in Mode::IMAGE_MODES.map(Mode::Image).into_iter().chain([14, 15, 16].map(Mode::Text)) {
            let code = m.code().unwrap();
            assert_eq!(m.carriers() * m.symbols() * m.modulation().bits(), code.sent_bits(), "{m:?}");
            assert_eq!(code.data_bits, 8 * m.payload_bytes());
        }
        assert_eq!(Mode::text_for(85), Some(Mode::Text(16)));
        assert_eq!(Mode::text_for(86), Some(Mode::Text(15)));
        assert_eq!(Mode::text_for(171), None);
        assert_eq!(Layout::new(44100).unwrap().symbol_len, 7056);
        assert!(Layout::new(22050).is_none());
    }
}
