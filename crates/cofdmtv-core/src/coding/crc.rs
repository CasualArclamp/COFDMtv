//! Reflected (least significant bit first) cyclic redundancy checks with zero initial
//! value and no final XOR (aicodix/code `crc.hh`). Appending the CRC bits to the message,
//! least significant bit first, makes the CRC of the whole sequence zero.
//!
//! The polynomials in use, in reflected form:
//! * [`POLY_META`] `0xA8F4`, 16 bits: COFDMTV preamble and modem meta data;
//! * [`POLY_IMAGE`] `0xD419CC15`, 32 bits: COFDMTV image payloads;
//! * [`POLY_DATA`] `0x8F6E37A0`, 32 bits: COFDMTV text payloads, modem payloads and the
//!   checksum over a whole multi-frame (CRS) file.

/// 16-bit CRC of the preamble's meta data (call sign and mode).
pub const POLY_META: u16 = 0xA8F4;
/// 32-bit CRC of the COFDMTV image payloads.
pub const POLY_IMAGE: u32 = 0xD419_CC15;
/// 32-bit CRC of text and modem payloads, and of multi-frame (CRS) files.
pub const POLY_DATA: u32 = 0x8F6E_37A0;

/// A 16-bit CRC.
#[derive(Clone)]
pub struct Crc16 {
    lut: [u16; 256],
    poly: u16,
    crc: u16,
}

/// A 32-bit CRC.
#[derive(Clone)]
pub struct Crc32 {
    lut: [u32; 256],
    poly: u32,
    crc: u32,
}

// Rust note: the two types differ only in the register width; a macro writes both
// instead of a generic over an integer trait (which the standard library lacks).
macro_rules! crc_impl {
    ($name:ident, $t:ty) => {
        impl $name {
            pub fn new(poly: $t) -> Self {
                let mut lut = [0; 256];
                for (j, entry) in lut.iter_mut().enumerate() {
                    let mut tmp = j as $t;
                    for _ in 0..8 {
                        tmp = Self::step(tmp, false, poly);
                    }
                    *entry = tmp;
                }
                Self { lut, poly, crc: 0 }
            }

            #[inline]
            fn step(prev: $t, bit: bool, poly: $t) -> $t {
                let tmp = prev ^ bit as $t;
                (prev >> 1) ^ ((tmp & 1) * poly)
            }

            pub fn reset(&mut self) {
                self.crc = 0;
            }

            pub fn value(&self) -> $t {
                self.crc
            }

            /// Feed one bit.
            #[inline]
            pub fn bit(&mut self, bit: bool) -> $t {
                self.crc = Self::step(self.crc, bit, self.poly);
                self.crc
            }

            /// Feed one byte, least significant bit first.
            #[inline]
            pub fn byte(&mut self, byte: u8) -> $t {
                let tmp = self.crc ^ byte as $t;
                self.crc = (self.crc >> 8) ^ self.lut[(tmp & 255) as usize];
                self.crc
            }

            /// Feed bytes.
            pub fn bytes(&mut self, data: &[u8]) -> $t {
                for &b in data {
                    self.byte(b);
                }
                self.crc
            }

            /// Feed a 64-bit word as eight bytes, least significant first.
            pub fn u64(&mut self, data: u64) -> $t {
                self.bytes(&data.to_le_bytes())
            }
        }
    };
}

crc_impl!(Crc16, u16);
crc_impl!(Crc32, u32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_equal_bits() {
        let data = b"COFDMtv";
        let mut a = Crc32::new(POLY_DATA);
        let mut b = Crc32::new(POLY_DATA);
        a.bytes(data);
        for byte in data {
            for i in 0..8 {
                b.bit((byte >> i) & 1 != 0);
            }
        }
        assert_eq!(a.value(), b.value());
    }

    #[test]
    fn appended_crc_checks_to_zero() {
        let mut c = Crc32::new(POLY_IMAGE);
        c.bytes(b"payload");
        let sum = c.value();
        for i in 0..32 {
            c.bit((sum >> i) & 1 != 0);
        }
        assert_eq!(c.value(), 0);
        let mut m = Crc16::new(POLY_META);
        m.u64(0x1234_5678_9abc_def0);
        let sum = m.value();
        for i in 0..16 {
            m.bit((sum >> i) & 1 != 0);
        }
        assert_eq!(m.value(), 0);
    }
}
