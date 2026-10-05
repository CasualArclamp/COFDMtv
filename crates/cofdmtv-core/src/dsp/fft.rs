//! Unnormalised forward (e^−jωn) and backward (e^+jωn) FFTs of one size, like aicodix
//! `FastFourierTransform<N, cmplx, -1 / +1>`; any size, through rustfft.

use super::Cplx;
use rustfft::{FftPlanner, Fft as RustFft};
use std::sync::Arc;

pub struct Fft {
    n: usize,
    fwd: Arc<dyn RustFft<f32>>,
    bwd: Arc<dyn RustFft<f32>>,
    scratch: Vec<Cplx>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        let mut planner = FftPlanner::new();
        let fwd = planner.plan_fft_forward(n);
        let bwd = planner.plan_fft_inverse(n);
        let len = fwd.get_inplace_scratch_len().max(bwd.get_inplace_scratch_len());
        Self { n, fwd, bwd, scratch: vec![Cplx::new(0.0, 0.0); len] }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// Forward transform in place.
    pub fn forward(&mut self, buf: &mut [Cplx]) {
        self.fwd.process_with_scratch(&mut buf[..self.n], &mut self.scratch);
    }

    /// Backward transform in place (no 1/N).
    pub fn backward(&mut self, buf: &mut [Cplx]) {
        self.bwd.process_with_scratch(&mut buf[..self.n], &mut self.scratch);
    }
}
