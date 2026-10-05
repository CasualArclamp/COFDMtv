//! Quadrature amplitude modulation with 16 to 4096 points (aicodix/modem `qam.hh`).
//!
//! Square constellations of unit average power, Gray coded per axis: bits 0 and 1 are the
//! signs of the real and imaginary parts, each following pair halves the remaining
//! distance to the axis ("folding"), so the soft bit of every level is a distance from a
//! threshold. Bits are ±1 values (+1 for 0), as in [`super::psk`].

use num_complex::Complex32 as Cplx;

/// Normalisation factors of the original (`FAC`), by bits per axis (2…6).
const FAC: [f32; 5] = [1.054_092_6, 0.925_820_1, 0.869_226_96, 0.842_423_5, 0.829_355_6];

/// A square QAM constellation of `1 << bits` points (`bits` even, 4…12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qam {
    bits: usize,
}

impl Qam {
    pub fn new(bits: usize) -> Qam {
        assert!(bits % 2 == 0 && (4..=12).contains(&bits), "QAM16 … QAM4096");
        Qam { bits }
    }

    pub fn bits(self) -> usize {
        self.bits
    }

    /// Bits per axis.
    fn levels(self) -> usize {
        self.bits / 2
    }

    /// The unit of the grid (`AMP`): points lie at odd multiples of it.
    pub fn amp(self) -> f32 {
        let m = self.levels();
        let rcp = ((1u32 << m) - 1) as f32 * FAC[m - 2];
        1.0 / rcp
    }

    pub fn distance(self) -> f32 {
        2.0 * self.amp()
    }

    /// The point of the ±1 bits `b`.
    pub fn map(self, b: &[f32]) -> Cplx {
        let m = self.levels();
        let amp = self.amp();
        // b0·(b2·(b4·(… ·(b_last + 2) …) + 2^(m−2)) + 2^(m−1)), built from the innermost
        // level outwards.
        let axis = |first: usize| {
            let mut v = 1.0f32;
            for level in (1..m).rev() {
                v = b[first + 2 * level] * v + (1u32 << (m - level)) as f32;
            }
            b[first] * v
        };
        amp * Cplx::new(axis(0), axis(1))
    }

    /// Hard decisions (±1).
    pub fn hard(self, b: &mut [f32], c: Cplx) {
        let m = self.levels();
        let amp = self.amp();
        for (first, x) in [(0, c.re), (1, c.im)] {
            b[first] = if x < 0.0 { -1.0 } else { 1.0 };
            let mut v = x.abs();
            for level in 1..m {
                let t = amp * (1u32 << (m - level)) as f32;
                b[first + 2 * level] = if v < t { -1.0 } else { 1.0 };
                v = (v - t).abs();
            }
        }
    }

    /// Soft bits scaled by `precision`.
    pub fn soft(self, b: &mut [f32], c: Cplx, precision: f32) {
        let m = self.levels();
        let amp = self.amp();
        let q = |v: f32| v * self.distance() * precision;
        for (first, x) in [(0, c.re), (1, c.im)] {
            b[first] = q(x);
            let mut v = x.abs();
            for level in 1..m {
                let t = amp * (1u32 << (m - level)) as f32;
                b[first + 2 * level] = q(v - t);
                v = (v - t).abs();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_power_and_inverse_mapping() {
        for bits in [4, 6, 8, 10, 12] {
            let q = Qam::new(bits);
            let n = 1usize << bits;
            let mut power = 0.0f64;
            for v in 0..n {
                let b: Vec<f32> = (0..bits).map(|i| if (v >> i) & 1 != 0 { -1.0 } else { 1.0 }).collect();
                let p = q.map(&b);
                power += f64::from(p.norm_sqr());
                let mut h = vec![0.0; bits];
                q.hard(&mut h, p * 1.0001);
                assert_eq!(h, b, "QAM{n} point {v}");
                let mut s = vec![0.0; bits];
                q.soft(&mut s, p, 1.0);
                assert!(s.iter().zip(&b).all(|(s, b)| s * b > 0.0), "soft signs QAM{n}");
            }
            assert!((power / n as f64 - 1.0).abs() < 1e-3, "QAM{n} power {}", power / n as f64);
        }
    }

    #[test]
    fn matches_the_originals_formula() {
        // QAM64: AMP·(b0·(b2·(b4+2)+4), …).
        let q = Qam::new(6);
        let b = [1.0, -1.0, -1.0, 1.0, 1.0, -1.0];
        let a = q.amp();
        let re = a * (b[0] * (b[2] * (b[4] + 2.0) + 4.0));
        let im = a * (b[1] * (b[3] * (b[5] + 2.0) + 4.0));
        assert!((q.map(&b) - Cplx::new(re, im)).norm() < 1e-6);
    }
}
