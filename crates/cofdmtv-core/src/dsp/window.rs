//! Small stateful helpers: oscillator, sliding buffers and sums, triggers (aicodix/dsp
//! `phasor.hh`, `bip_buffer.hh`, `swa.hh`, `sma.hh`, `delay.hh`, `trigger.hh`).

use super::Cplx;

/// Numerically controlled oscillator: successive powers of e^{jω}, renormalised every
/// step (`Phasor`).
#[derive(Debug, Clone)]
pub struct Phasor {
    prev: Cplx,
    delta: Cplx,
}

impl Default for Phasor {
    fn default() -> Self {
        Self { prev: Cplx::new(1.0, 0.0), delta: Cplx::new(1.0, 0.0) }
    }
}

impl Phasor {
    /// Step by `omega` radians per sample.
    pub fn omega(&mut self, omega: f32) {
        self.delta = Cplx::new(omega.cos(), omega.sin());
    }

    /// Step by `n / big_n` of a turn per sample.
    pub fn omega_ratio(&mut self, n: i32, big_n: i32) {
        let w = std::f64::consts::TAU * f64::from(n) / f64::from(big_n);
        self.delta = Cplx::new(w.cos() as f32, w.sin() as f32);
    }

    pub fn reset(&mut self) {
        self.prev = Cplx::new(1.0, 0.0);
    }

    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Cplx {
        let tmp = self.prev;
        self.prev *= self.delta;
        self.prev /= self.prev.norm();
        tmp
    }
}

/// The last `len` values as one contiguous window, oldest first (`BipBuffer`): each value
/// is stored twice, `len` apart, so a window never wraps.
#[derive(Debug, Clone)]
pub struct BipBuffer {
    buf: Vec<Cplx>,
    len: usize,
    pos0: usize,
    pos1: usize,
}

impl BipBuffer {
    pub fn new(len: usize) -> Self {
        Self { buf: vec![Cplx::new(0.0, 0.0); 2 * len], len, pos0: 0, pos1: len }
    }

    /// Start of the current window in [`Self::at`].
    #[inline]
    pub fn start(&self) -> usize {
        self.pos0.min(self.pos1)
    }

    /// The window starting at `start` (a value [`Self::start`] returned, still valid until
    /// that many samples have been pushed since).
    #[inline]
    pub fn at(&self, start: usize) -> &[Cplx] {
        &self.buf[start..start + self.len]
    }

    /// Push a value; returns the start of the new window.
    #[inline]
    pub fn push(&mut self, value: Cplx) -> usize {
        self.buf[self.pos0] = value;
        self.buf[self.pos1] = value;
        self.pos0 += 1;
        if self.pos0 >= 2 * self.len {
            self.pos0 = 0;
        }
        self.pos1 += 1;
        if self.pos1 >= 2 * self.len {
            self.pos1 = 0;
        }
        self.start()
    }
}

/// Values that can be summed in a [`SlidingSum`].
pub trait Summable: Copy + std::ops::Add<Output = Self> {
    fn zero() -> Self;
}

impl Summable for f32 {
    fn zero() -> Self {
        0.0
    }
}

impl Summable for Cplx {
    fn zero() -> Self {
        Cplx::new(0.0, 0.0)
    }
}

/// Sum of the last `len` values, kept in a binary tree so that rounding errors do not
/// accumulate (`SWA` / `SMA4` without normalisation).
#[derive(Debug, Clone)]
pub struct SlidingSum<T> {
    tree: Vec<T>,
    leaf: usize,
    len: usize,
}

impl<T: Summable> SlidingSum<T> {
    pub fn new(len: usize) -> Self {
        Self { tree: vec![T::zero(); 2 * len], leaf: len, len }
    }

    #[inline]
    pub fn push(&mut self, input: T) -> T {
        self.tree[self.leaf] = input;
        let (mut child, mut parent) = (self.leaf, self.leaf / 2);
        while parent > 0 {
            self.tree[parent] = self.tree[child] + self.tree[child ^ 1];
            child = parent;
            parent /= 2;
        }
        self.leaf += 1;
        if self.leaf >= 2 * self.len {
            self.leaf = self.len;
        }
        self.tree[1]
    }
}

/// Delay line of `len` samples (`Delay`).
#[derive(Debug, Clone)]
pub struct Delay<T> {
    buf: Vec<T>,
    pos: usize,
}

impl<T: Summable> Delay<T> {
    pub fn new(len: usize) -> Self {
        Self { buf: vec![T::zero(); len.max(1)], pos: 0 }
    }

    #[inline]
    pub fn push(&mut self, input: T) -> T {
        let tmp = self.buf[self.pos];
        self.buf[self.pos] = input;
        self.pos += 1;
        if self.pos >= self.buf.len() {
            self.pos = 0;
        }
        tmp
    }
}

/// Comparator with hysteresis (`SchmittTrigger`).
#[derive(Debug, Clone)]
pub struct SchmittTrigger {
    low: f32,
    high: f32,
    previous: bool,
}

impl SchmittTrigger {
    pub fn new(low: f32, high: f32) -> Self {
        Self { low, high, previous: false }
    }

    #[inline]
    pub fn process(&mut self, input: f32) -> bool {
        if self.previous {
            if input < self.low {
                self.previous = false;
            }
        } else if input > self.high {
            self.previous = true;
        }
        self.previous
    }
}

/// True for one step after the input falls from true to false (`FallingEdgeTrigger`).
#[derive(Debug, Clone, Default)]
pub struct FallingEdge {
    previous: bool,
}

impl FallingEdge {
    #[inline]
    pub fn process(&mut self, input: bool) -> bool {
        let tmp = self.previous;
        self.previous = input;
        tmp && !input
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bip_buffer_windows_are_chronological() {
        let mut b = BipBuffer::new(4);
        let mut start = 0;
        for i in 1..=10 {
            start = b.push(Cplx::new(i as f32, 0.0));
        }
        let w: Vec<f32> = b.at(start).iter().map(|c| c.re).collect();
        assert_eq!(w, vec![7.0, 8.0, 9.0, 10.0]);
    }

    #[test]
    fn sliding_sum_sums_the_window() {
        let mut s = SlidingSum::<f32>::new(3);
        let out: Vec<f32> = (1..=5).map(|i| s.push(i as f32)).collect();
        assert_eq!(out, vec![1.0, 3.0, 6.0, 9.0, 12.0]);
    }
}
