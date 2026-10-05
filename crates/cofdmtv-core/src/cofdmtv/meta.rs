//! The preamble's meta data: call sign and mode in 55 bits plus a CRC-16, protected by
//! the (255, 71) BCH code (Shredpix `preamble()`, Assempix `preamble()`).
//!
//! On the air the 255 code bits are differentially BPSK modulated over carriers −127…127
//! (carrier −128 is the reference) and scrambled with a maximum length sequence.

use crate::coding::bch::{self, BchEncoder};
use crate::coding::bits::get_be_bit;
use crate::coding::crc::{Crc16, POLY_META};
use crate::coding::osd::Osd;
use crate::coding::{base37, psk};
use crate::dsp::{Cplx, demod_or_erase};

/// What the preamble says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    /// The mode number (0 ping, 6–13 pictures, 14–16 text; others unknown to COFDMtv).
    pub mode: u8,
    /// The call sign, base-37 packed.
    pub call: u64,
}

impl Meta {
    pub fn call_sign(&self) -> String {
        base37::decode(self.call).trim().to_string()
    }

    /// The 55 bits on the air.
    fn word(&self) -> u64 {
        (self.call << 8) | u64::from(self.mode)
    }

    /// The 255 BCH code bits (0/1) of this meta data.
    pub fn codeword(&self, enc: &BchEncoder) -> [u8; bch::N] {
        let md = self.word();
        let mut crc = Crc16::new(POLY_META);
        let cs = crc.u64(md << 9);
        let mut msg = [0u8; bch::K];
        for (i, m) in msg.iter_mut().enumerate() {
            *m = if i < 55 { ((md >> i) & 1) as u8 } else { ((cs >> (i - 55)) & 1) as u8 };
        }
        let parity = enc.parity(&msg);
        let mut word = [0u8; bch::N];
        word[..bch::K].copy_from_slice(&msg);
        word[bch::K..].copy_from_slice(&parity);
        word
    }
}

/// Why a preamble was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaError {
    /// No unique BCH codeword, or the CRC failed.
    Damaged,
    /// The meta data is intact but names no valid call sign.
    BadCall(Meta),
}

/// Decodes preambles.
pub struct MetaDecoder {
    osd: Osd,
    genmat: Vec<u8>,
    soft: [i8; bch::N],
}

impl Default for MetaDecoder {
    fn default() -> Self {
        Self { osd: Osd::default(), genmat: bch::generator_matrix(), soft: [0; bch::N] }
    }
}

impl MetaDecoder {
    /// Decode the preamble from the spectrum `freq` of its symbol (carrier `c` in bin
    /// `bin(c)`), mixed down to baseband.
    pub fn decode(&mut self, freq: &mut [Cplx], bin: impl Fn(i32) -> usize) -> Result<Meta, MetaError> {
        let mut seq = crate::coding::mls::Mls::new(super::PRE_SEQ_POLY);
        for i in 0..super::PRE_SEQ_LEN {
            freq[bin(i + super::PRE_SEQ_OFF)] *= crate::coding::nrz(seq.next());
        }
        for i in 0..super::PRE_SEQ_LEN {
            let c = demod_or_erase(freq[bin(i + super::PRE_SEQ_OFF)], freq[bin(i - 1 + super::PRE_SEQ_OFF)]);
            self.soft[i as usize] = psk::bpsk_soft_i8(c, 32.0);
        }
        let mut data = [0u8; bch::N.div_ceil(8)];
        if !self.osd.decode(&mut data, &self.soft, &self.genmat) {
            return Err(MetaError::Damaged);
        }
        let md = (0..55).fold(0u64, |md, i| md | u64::from(get_be_bit(&data, i)) << i);
        let cs = (0..16).fold(0u16, |cs, i| cs | u16::from(get_be_bit(&data, i + 55)) << i);
        let mut crc = Crc16::new(POLY_META);
        if crc.u64(md << 9) != cs {
            return Err(MetaError::Damaged);
        }
        let meta = Meta { mode: (md & 255) as u8, call: md >> 8 };
        if meta.call == 0 || meta.call >= base37::LIMIT {
            return Err(MetaError::BadCall(meta));
        }
        Ok(meta)
    }
}
