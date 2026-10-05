//! Phase-shift keying: mapping bits (as ±1 values, +1 for bit 0) to constellation points,
//! hard decisions, and soft bits scaled by the estimated signal-to-noise ratio
//! (aicodix `psk.hh`).
//!
//! 8PSK uses three bits: `b[1]` and `b[2]` are the signs of the real and imaginary parts,
//! `b[0]` chooses whether the point lies closer to the real (+1) or the imaginary (−1)
//! axis.

use num_complex::Complex32 as Cplx;

const RCP_SQRT_2: f32 = std::f32::consts::FRAC_1_SQRT_2;
const COS_PI_8: f32 = 0.923_879_5;
const SIN_PI_8: f32 = 0.382_683_43;

/// The modulations in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Psk {
    Bpsk,
    Qpsk,
    Psk8,
}

impl Psk {
    pub fn bits(self) -> usize {
        match self {
            Self::Bpsk => 1,
            Self::Qpsk => 2,
            Self::Psk8 => 3,
        }
    }

    /// Distance between neighbouring points (scales the soft bits).
    pub fn distance(self) -> f32 {
        match self {
            Self::Bpsk => 2.0,
            Self::Qpsk => 2.0 * RCP_SQRT_2,
            Self::Psk8 => 2.0 * SIN_PI_8,
        }
    }

    /// The point of the ±1 bits `b`.
    pub fn map(self, b: &[f32]) -> Cplx {
        match self {
            Self::Bpsk => Cplx::new(b[0], 0.0),
            Self::Qpsk => RCP_SQRT_2 * Cplx::new(b[0], b[1]),
            Self::Psk8 => {
                let (re, im) = if b[0] < 0.0 { (SIN_PI_8, COS_PI_8) } else { (COS_PI_8, SIN_PI_8) };
                Cplx::new(re * b[1], im * b[2])
            }
        }
    }

    /// Hard decisions (±1) for `c`.
    pub fn hard(self, b: &mut [f32], c: Cplx) {
        let sign = |v: f32| if v < 0.0 { -1.0 } else { 1.0 };
        match self {
            Self::Bpsk => b[0] = sign(c.re),
            Self::Qpsk => {
                b[0] = sign(c.re);
                b[1] = sign(c.im);
            }
            Self::Psk8 => {
                b[1] = sign(c.re);
                b[2] = sign(c.im);
                b[0] = if c.re.abs() < c.im.abs() { -1.0 } else { 1.0 };
            }
        }
    }

    /// Soft bits for `c`, scaled by `precision` (signal-to-noise power ratio).
    pub fn soft(self, b: &mut [f32], c: Cplx, precision: f32) {
        let q = |v: f32| v * self.distance() * precision;
        match self {
            Self::Bpsk => b[0] = q(c.re),
            Self::Qpsk => {
                b[0] = q(c.re);
                b[1] = q(c.im);
            }
            Self::Psk8 => {
                b[1] = q(c.re);
                b[2] = q(c.im);
                b[0] = q(RCP_SQRT_2 * (c.re.abs() - c.im.abs()));
            }
        }
    }

    /// The nearest point to `c`.
    pub fn nearest(self, c: Cplx) -> Cplx {
        let mut b = [0.0; 3];
        self.hard(&mut b, c);
        self.map(&b)
    }

    /// All points of the constellation (for plots).
    pub fn points(self) -> Vec<Cplx> {
        let n = 1 << self.bits();
        (0..n)
            .map(|v: u32| {
                let b: Vec<f32> = (0..3).map(|i| if (v >> i) & 1 != 0 { -1.0 } else { 1.0 }).collect();
                self.map(&b)
            })
            .collect()
    }
}

/// BPSK soft bit quantized to `i8` like `PhaseShiftKeying<2, cmplx, int8_t>::soft` (the
/// preamble's input to the ordered-statistics decoder).
pub fn bpsk_soft_i8(c: Cplx, precision: f32) -> i8 {
    (c.re * 2.0 * precision).round_ties_even().clamp(-128.0, 127.0) as i8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_decisions_invert_map() {
        for psk in [Psk::Bpsk, Psk::Qpsk, Psk::Psk8] {
            let points = psk.points();
            assert_eq!(points.len(), 1 << psk.bits());
            for p in points {
                assert!((p.norm() - 1.0).abs() < 1e-6);
                assert!((psk.nearest(p * 0.9) - p).norm() < 1e-6, "{psk:?} {p}");
            }
        }
    }
}
