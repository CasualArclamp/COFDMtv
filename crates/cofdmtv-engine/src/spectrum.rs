//! The input spectrum for the plots: a Hann-windowed FFT one symbol long (6.25 Hz bins,
//! fine enough to read the fancy header's call sign on the waterfall), every half symbol
//! with its guard interval (90 ms, eleven waterfall rows a second), and an exponentially
//! averaged copy for the spectrum line.

use cofdmtv_core::dsp::{Cplx, Fft};

/// Spectra of a stream at one sample rate.
pub struct SpectrumAnalyzer {
    n: usize,
    hop: usize,
    iq: bool,
    rate: u32,
    fft: Fft,
    window: Vec<f32>,
    norm: f32,
    ring: Vec<Cplx>,
    pos: usize,
    filled: usize,
    since: usize,
    work: Vec<Cplx>,
    avg: Vec<f32>,
    rows: Vec<Vec<f32>>,
    seq: u64,
}

/// Floor of the dB values (an all-zero input).
pub const FLOOR_DB: f32 = -150.0;

impl SpectrumAnalyzer {
    /// `n`-point spectra every `hop` samples; `iq`: complex input (both sidebands shown).
    pub fn new(rate: u32, n: usize, hop: usize, iq: bool) -> Self {
        let window: Vec<f32> = (0..n).map(|i| 0.5 * (1.0 - (std::f32::consts::TAU * i as f32 / n as f32).cos())).collect();
        let sum: f32 = window.iter().sum();
        let bins = if iq { n } else { n / 2 };
        Self {
            n,
            hop,
            iq,
            rate,
            fft: Fft::new(n),
            window,
            norm: 1.0 / (sum * sum),
            ring: vec![Cplx::new(0.0, 0.0); n],
            pos: 0,
            filled: 0,
            since: 0,
            work: vec![Cplx::new(0.0, 0.0); n],
            avg: vec![FLOOR_DB; bins],
            rows: Vec::new(),
            seq: 0,
        }
    }

    /// For a COFDMTV signal at `rate`: one symbol long, every half extended symbol.
    pub fn for_rate(rate: u32, iq: bool) -> Self {
        let layout = cofdmtv_core::cofdmtv::Layout::new(rate).expect("a COFDMTV rate");
        Self::new(rate, layout.symbol_len, layout.extended_len / 2, iq)
    }

    #[inline]
    pub fn push(&mut self, z: Cplx) {
        self.ring[self.pos] = z;
        self.pos = (self.pos + 1) % self.n;
        self.filled = (self.filled + 1).min(self.n);
        self.since += 1;
        if self.since >= self.hop && self.filled == self.n {
            self.since = 0;
            self.analyse();
        }
    }

    fn analyse(&mut self) {
        for i in 0..self.n {
            self.work[i] = self.ring[(self.pos + i) % self.n] * self.window[i];
        }
        self.fft.forward(&mut self.work);
        let n = self.n;
        let row: Vec<f32> = if self.iq {
            (0..n).map(|j| self.db(self.work[(j + n / 2) % n])).collect()
        } else {
            // A real signal's power splits between ±f: count both halves.
            (0..n / 2).map(|j| self.db(self.work[j]) + 3.0103).collect()
        };
        let lambda = if self.seq < 4 { 0.5 } else { 0.8 };
        for (a, &r) in self.avg.iter_mut().zip(&row) {
            // Average powers, not decibels.
            let p = lambda * 10f32.powf(*a / 10.0) + (1.0 - lambda) * 10f32.powf(r / 10.0);
            *a = (10.0 * p.log10()).max(FLOOR_DB);
        }
        self.rows.push(row);
        if self.rows.len() > 64 {
            self.rows.remove(0);
        }
        self.seq += 1;
    }

    #[inline]
    fn db(&self, c: Cplx) -> f32 {
        (10.0 * (c.norm_sqr() * self.norm).log10()).max(FLOOR_DB)
    }

    /// Rows computed since the last call, oldest first, and the count of all rows so far.
    pub fn take_rows(&mut self) -> (Vec<Vec<f32>>, u64) {
        (std::mem::take(&mut self.rows), self.seq)
    }

    /// The averaged spectrum, dB.
    pub fn average(&self) -> &[f32] {
        &self.avg
    }

    /// Frequency of the first bin and the bin spacing, Hz.
    pub fn axis(&self) -> (f64, f64) {
        let bin = f64::from(self.rate) / self.n as f64;
        (if self.iq { -f64::from(self.rate) / 2.0 } else { 0.0 }, bin)
    }

    /// Seconds between rows.
    pub fn row_seconds(&self) -> f64 {
        self.hop as f64 / f64::from(self.rate)
    }

    pub fn is_iq(&self) -> bool {
        self.iq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_lands_in_its_bin() {
        let mut s = SpectrumAnalyzer::for_rate(8000, false);
        for i in 0..20_000 {
            let x = 0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / 8000.0).cos();
            s.push(Cplx::new(x, 0.0));
        }
        let (rows, seq) = s.take_rows();
        assert!(!rows.is_empty() && seq as usize >= rows.len());
        let (first, bin) = s.axis();
        let peak = s.average().iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert!((first + peak as f64 * bin - 1000.0).abs() < 7.0);
        // A 0.5 amplitude sine: −9 dB of full-scale power (−6 dB amplitude, −3 dB RMS).
        assert!((s.average()[peak] - -9.0).abs() < 1.0, "{}", s.average()[peak]);
    }
}
