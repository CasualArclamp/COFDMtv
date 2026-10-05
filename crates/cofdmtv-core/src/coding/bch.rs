//! The (255, 71) BCH code protecting the COFDMTV preamble: a systematic encoder (aicodix/code
//! `bose_chaudhuri_hocquenghem_encoder.hh`) and the systematic generator matrix the
//! ordered-statistics decoder works with (`osd.hh`, `BoseChaudhuriHocquenghemGenerator`).
//!
//! Codewords are 255 bits: the 71 message bits, then the 184 parity bits.

/// Code length.
pub const N: usize = 255;
/// Message length.
pub const K: usize = 71;
/// Parity bits.
pub const NP: usize = N - K;

/// The minimal polynomials whose product is the generator polynomial.
pub const MINIMAL_POLYNOMIALS: [u32; 24] = [
    0b100011101, 0b101110111, 0b111110011, 0b101101001, 0b110111101, 0b111100111, 0b100101011, 0b111010111,
    0b000010011, 0b101100101, 0b110001011, 0b101100011, 0b100011011, 0b100111111, 0b110001101, 0b100101101,
    0b101011111, 0b111111001, 0b111000011, 0b100111001, 0b110101001, 0b000011111, 0b110000111, 0b110110001,
];

/// Coefficients of the generator polynomial, `g[NP - i]` = coefficient of `x^i`
/// (so `g[0]` is the leading coefficient), as `poly()` of the generator class builds it.
fn generator_polynomial() -> [u8; NP + 1] {
    let mut g = [0u8; NP + 1];
    g[NP] = 1;
    let mut degree = 1;
    for &m in &MINIMAL_POLYNOMIALS {
        let m_degree = 31 - m.leading_zeros() as usize;
        for i in (0..=degree).rev() {
            if g[NP - i] == 0 {
                continue;
            }
            g[NP - i] = (m & 1) as u8;
            for j in 1..=m_degree {
                g[NP - (i + j)] ^= ((m >> j) & 1) as u8;
            }
        }
        degree += m_degree;
    }
    debug_assert_eq!(degree, NP + 1);
    debug_assert!(g[0] == 1 && g[NP] == 1);
    g
}

/// Systematic encoder: the 184 parity bits of 71 message bits (one bit per byte).
#[derive(Clone)]
pub struct BchEncoder {
    /// The generator polynomial without its leading term: `gen[k]` = coefficient of
    /// `x^(NP - 1 - k)`.
    generator: [u8; NP],
}

impl Default for BchEncoder {
    fn default() -> Self {
        let g = generator_polynomial();
        let mut generator = [0u8; NP];
        generator.copy_from_slice(&g[1..]);
        Self { generator }
    }
}

impl BchEncoder {
    /// `parity` = (message · x^NP) mod generator, highest power first.
    pub fn parity(&self, message: &[u8; K]) -> [u8; NP] {
        let mut parity = [0u8; NP];
        for &bit in message {
            let feedback = bit ^ parity[0];
            parity.copy_within(1.., 0);
            parity[NP - 1] = 0;
            if feedback != 0 {
                for (p, g) in parity.iter_mut().zip(&self.generator) {
                    *p ^= g;
                }
            }
        }
        parity
    }
}

/// The systematic generator matrix, `K` rows of `N` bits (one bit per byte): row `j` is
/// the codeword of the message with only bit `j` set.
pub fn generator_matrix() -> Vec<u8> {
    let g = generator_polynomial();
    let mut m = vec![0u8; N * K];
    // Row j: the generator polynomial shifted right by j.
    for j in 0..K {
        m[N * j + j..N * j + j + NP + 1].copy_from_slice(&g);
    }
    // Eliminate above the diagonal to make the first K columns the identity.
    for k in (1..K).rev() {
        for j in 0..k {
            if m[N * j + k] != 0 {
                for i in k..N {
                    m[N * j + i] ^= m[N * k + i];
                }
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::xorshift::Xorshift32;

    #[test]
    fn encoder_matches_generator_matrix() {
        let enc = BchEncoder::default();
        let g = generator_matrix();
        let mut rng = Xorshift32::default();
        for _ in 0..20 {
            let mut msg = [0u8; K];
            for b in &mut msg {
                *b = (rng.next() & 1) as u8;
            }
            let parity = enc.parity(&msg);
            let mut word = [0u8; N];
            for (j, &bit) in msg.iter().enumerate() {
                if bit != 0 {
                    for (w, r) in word.iter_mut().zip(&g[N * j..N * (j + 1)]) {
                        *w ^= r;
                    }
                }
            }
            assert_eq!(&word[..K], &msg[..]);
            assert_eq!(&word[K..], &parity[..]);
        }
    }
}
