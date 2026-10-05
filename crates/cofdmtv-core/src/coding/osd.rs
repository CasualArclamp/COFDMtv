//! Ordered statistics decoding of order 2 for the (255, 71) BCH code (aicodix/code `osd.hh`,
//! `OrderedStatisticsDecoder<255, 71, 2>`): sort the received bits by reliability, bring
//! the generator matrix into systematic form on the most reliable independent positions,
//! and try every codeword that differs from the hard decision there in at most two bits.

use super::bch::{K, N};

/// Rows are padded to `W` bytes like the original (`(N + 7) & !7`).
const W: usize = (N + 7) & !7;

pub struct Osd {
    g: Vec<u8>,
    codeword: [u8; W],
    candidate: [u8; W],
    softperm: [i8; W],
    perm: [usize; W],
}

impl Default for Osd {
    fn default() -> Self {
        Self { g: vec![0; W * K], codeword: [0; W], candidate: [0; W], softperm: [0; W], perm: [0; W] }
    }
}

impl Osd {
    /// Decode the soft bits `soft` (positive: 0, negative: 1) with the systematic generator
    /// matrix `genmat` (`K` rows of `N` bits, see [`super::bch::generator_matrix`]). Writes
    /// the most likely codeword to `hard` (MSB-first bits) and returns whether it is the
    /// unique best candidate.
    pub fn decode(&mut self, hard: &mut [u8], soft: &[i8; N], genmat: &[u8]) -> bool {
        let perm = &mut self.perm;
        for (i, p) in perm.iter_mut().enumerate().take(N) {
            *p = i;
        }
        let reliability: Vec<i16> = soft.iter().map(|&s| i16::from(s.max(-127)).abs()).collect();
        // Most reliable first; stable like the original merge sort.
        perm[..N].sort_by(|&a, &b| reliability[b].cmp(&reliability[a]));
        for j in 0..K {
            for i in 0..N {
                self.g[W * j + i] = genmat[N * j + perm[i]];
            }
        }
        self.row_echelon();
        self.systematic();
        for i in 0..N {
            self.softperm[i] = soft[self.perm[i]].max(-127);
        }
        for s in &mut self.softperm[N..] {
            *s = 0;
        }
        for i in 0..K {
            self.codeword[i] = u8::from(self.softperm[i] < 0);
        }
        self.encode();
        self.candidate = self.codeword;
        let mut best = self.metric();
        let mut next = -1;
        for a in 0..K {
            self.flip(a);
            self.update(&mut best, &mut next);
            for b in a + 1..K {
                self.flip(b);
                self.update(&mut best, &mut next);
                self.flip(b);
            }
            self.flip(a);
        }
        for i in 0..N {
            super::bits::set_be_bit(hard, self.perm[i], self.candidate[i] != 0);
        }
        best != next
    }

    fn update(&mut self, best: &mut i32, next: &mut i32) {
        let met = self.metric();
        if met > *best {
            *next = *best;
            *best = met;
            self.candidate[..N].copy_from_slice(&self.codeword[..N]);
        } else if met > *next {
            *next = met;
        }
    }

    fn metric(&self) -> i32 {
        self.codeword
            .iter()
            .zip(&self.softperm)
            .map(|(&c, &s)| (1 - 2 * i32::from(c)) * i32::from(s))
            .sum()
    }

    fn flip(&mut self, j: usize) {
        for (c, g) in self.codeword.iter_mut().zip(&self.g[W * j..W * (j + 1)]) {
            *c ^= g;
        }
    }

    fn encode(&mut self) {
        for i in K..N {
            self.codeword[i] = self.codeword[0] & self.g[i];
        }
        for j in 1..K {
            if self.codeword[j] != 0 {
                for i in K..N {
                    self.codeword[i] ^= self.g[W * j + i];
                }
            }
        }
    }

    fn row_echelon(&mut self) {
        let g = &mut self.g;
        for k in 0..K {
            // A pivot in this column.
            for j in k..K {
                if g[W * j + k] != 0 {
                    if j != k {
                        for i in k..N {
                            g.swap(W * j + i, W * k + i);
                        }
                    }
                    break;
                }
            }
            // Otherwise a later column with a pivot (columns ≥ K if need be).
            let mut j = k + 1;
            while g[W * k + k] == 0 && j < N {
                for h in k..K {
                    if g[W * h + j] != 0 {
                        self.perm.swap(k, j);
                        for i in 0..K {
                            g.swap(W * i + k, W * i + j);
                        }
                        if h != k {
                            for i in k..N {
                                g.swap(W * h + i, W * k + i);
                            }
                        }
                        break;
                    }
                }
                j += 1;
            }
            debug_assert!(g[W * k + k] != 0);
            for j in k + 1..K {
                if g[W * j + k] != 0 {
                    for i in k..N {
                        g[W * j + i] ^= g[W * k + i];
                    }
                }
            }
        }
    }

    fn systematic(&mut self) {
        let g = &mut self.g;
        for k in (1..K).rev() {
            for j in 0..k {
                if g[W * j + k] != 0 {
                    for i in k..N {
                        g[W * j + i] ^= g[W * k + i];
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::bch::{BchEncoder, generator_matrix};
    use crate::coding::bits::get_be_bit;
    use crate::coding::xorshift::Xorshift32;

    #[test]
    fn corrects_errors_and_erasures() {
        let enc = BchEncoder::default();
        let genmat = generator_matrix();
        let mut osd = Osd::default();
        let mut rng = Xorshift32::default();
        for trial in 0..10 {
            let mut msg = [0u8; K];
            for b in &mut msg {
                *b = (rng.next() & 1) as u8;
            }
            let parity = enc.parity(&msg);
            let word: Vec<u8> = msg.iter().chain(parity.iter()).copied().collect();
            // Confident bits, 30 of them weakly wrong and 40 erased.
            let mut soft = [0i8; N];
            for (i, s) in soft.iter_mut().enumerate() {
                let sign = if word[i] != 0 { -1 } else { 1 };
                *s = sign * 64;
            }
            for k in 0..70 {
                let i = (rng.next() as usize + trial) % N;
                soft[i] = if k < 30 { -soft[i].signum() * 8 } else { 0 };
            }
            let mut hard = [0u8; N.div_ceil(8)];
            assert!(osd.decode(&mut hard, &soft, &genmat));
            for (i, &bit) in word.iter().enumerate() {
                assert_eq!(get_be_bit(&hard, i), bit != 0, "trial {trial} bit {i}");
            }
        }
    }
}
