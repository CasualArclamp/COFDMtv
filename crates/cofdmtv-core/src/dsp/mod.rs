//! Signal processing building blocks, ported from aicodix/dsp (BSD Zero Clause License).
//!
//! Everything runs in `f32` like the original apps: the signals are a few kilohertz wide
//! and the decisions are coded, so single precision is plenty, and staying with the
//! original's arithmetic keeps the two implementations comparable.

mod fft;
mod filters;
mod papr;
mod theil_sen;
mod window;

pub use fft::Fft;
pub use filters::{BlockDc, Hilbert};
pub use papr::improve_papr;
pub use theil_sen::TheilSen;
pub use window::{BipBuffer, Delay, FallingEdge, Phasor, SchmittTrigger, SlidingSum};

pub use num_complex::Complex32 as Cplx;

/// Divide `curr` by `prev` (differential demodulation), or 0 when `prev` is no reference
/// or the quotient is implausibly large (an erasure).
#[inline]
pub fn demod_or_erase(curr: Cplx, prev: Cplx) -> Cplx {
    if !(prev.norm_sqr() > 0.0) {
        return Cplx::new(0.0, 0.0);
    }
    let cons = div(curr, prev);
    if !(cons.norm_sqr() <= 4.0) {
        return Cplx::new(0.0, 0.0);
    }
    cons
}

/// Complex division as the original computes it (no scaling tricks).
#[inline]
pub fn div(a: Cplx, b: Cplx) -> Cplx {
    let d = b.re * b.re + b.im * b.im;
    Cplx::new((a.re * b.re + a.im * b.im) / d, (a.im * b.re - a.re * b.im) / d)
}

/// Power in decibels (−inf for 0).
#[inline]
pub fn decibel(power: f32) -> f32 {
    10.0 * power.log10()
}
