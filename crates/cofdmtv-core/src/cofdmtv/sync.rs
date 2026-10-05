//! Synchronisation on the COFDMTV sync symbol (Assempix/Rattlegram `schmidl_cox.hh`).
//!
//! The sync symbol repeats after half a symbol, so the Schmidl & Cox timing metric
//! |P|²/R² peaks there; its phase gives the fractional frequency offset. Once the metric
//! falls again, the half symbol is transformed, demodulated differentially between
//! neighbouring carriers and cross-correlated (via FFT) with the known sequence: the peak
//! gives the integer frequency offset — the carrier frequency, as the receiver looks at
//! the whole audio band — and its phase the remaining timing error.

use crate::coding::mls::Mls;
use crate::coding::nrz;
use crate::dsp::{Cplx, Delay, FallingEdge, Fft, Phasor, SchmittTrigger, SlidingSum, demod_or_erase};
use std::f32::consts::{PI, TAU};

pub struct SchmidlCox {
    search_pos: usize,
    /// Half a symbol: the period of the sync symbol and the size of the FFTs here.
    half: usize,
    guard_len: usize,
    match_del: usize,
    fft: Fft,
    cor: SlidingSum<Cplx>,
    pwr: SlidingSum<f32>,
    matched: SlidingSum<f32>,
    delay: Delay<f32>,
    threshold: SchmittTrigger,
    falling: FallingEdge,
    tmp0: Vec<Cplx>,
    tmp1: Vec<Cplx>,
    kern: Vec<Cplx>,
    timing_max: f32,
    phase_max: f32,
    index_max: usize,
    /// Start of the sync symbol (after its guard interval) in the last window.
    pub symbol_pos: usize,
    /// Frequency offset of the signal, radians per sample (the carrier frequency included).
    pub cfo_rad: f32,
}

impl SchmidlCox {
    /// A correlator looking at windows of the receive buffer from `search_pos`, for
    /// symbols of `2 * half` samples with `guard_len` samples of guard interval.
    pub fn new(search_pos: usize, half: usize, guard_len: usize) -> Self {
        let match_len = guard_len | 1;
        let match_del = (match_len - 1) / 2;
        // The sequence as the half-size FFT sees it (every other carrier of the symbol).
        let mut seq = Mls::new(super::COR_SEQ_POLY);
        let mut kern = vec![Cplx::new(0.0, 0.0); half];
        let off = super::COR_SEQ_OFF / 2;
        for i in 0..super::COR_SEQ_LEN {
            let k = (i + off + half as i32).rem_euclid(half as i32) as usize;
            kern[k] = Cplx::new(nrz(seq.next()), 0.0);
        }
        let mut fft = Fft::new(half);
        fft.forward(&mut kern);
        for k in &mut kern {
            *k = k.conj() / half as f32;
        }
        Self {
            search_pos,
            half,
            guard_len,
            match_del,
            fft,
            cor: SlidingSum::new(half),
            pwr: SlidingSum::new(2 * half),
            matched: SlidingSum::new(match_len),
            delay: Delay::new(match_del),
            threshold: SchmittTrigger::new(0.17 * match_len as f32, 0.19 * match_len as f32),
            falling: FallingEdge::default(),
            tmp0: vec![Cplx::new(0.0, 0.0); half],
            tmp1: vec![Cplx::new(0.0, 0.0); half],
            kern,
            timing_max: 0.0,
            phase_max: 0.0,
            index_max: 0,
            symbol_pos: 0,
            cfo_rad: 0.0,
        }
    }

    /// Look at the newest window of the receive buffer (one more sample than last time);
    /// `true` when a sync symbol was found ([`Self::symbol_pos`], [`Self::cfo_rad`]).
    pub fn process(&mut self, samples: &[Cplx]) -> bool {
        let (sp, half) = (self.search_pos, self.half);
        let p = self.cor.push(samples[sp + half] * samples[sp + 2 * half].conj());
        let min_r = (0.0001 * half as f64) as f32;
        let r = (0.5 * self.pwr.push(samples[sp + 2 * half].norm_sqr())).max(min_r);
        let timing = self.matched.push(p.norm_sqr() / (r * r));
        let phase = self.delay.push(p.im.atan2(p.re));

        let collect = self.threshold.process(timing);
        let process = self.falling.process(collect);
        if !collect && !process {
            return false;
        }
        if self.timing_max < timing {
            self.timing_max = timing;
            self.phase_max = phase;
            self.index_max = self.match_del;
        } else if self.index_max < half + self.guard_len + self.match_del {
            self.index_max += 1;
        }
        if !process {
            return false;
        }

        let frac_cfo = self.phase_max / half as f32;
        let mut osc = Phasor::default();
        osc.omega(frac_cfo);
        let symbol_pos = sp - self.index_max;
        self.index_max = 0;
        self.timing_max = 0.0;
        for i in 0..half {
            self.tmp1[i] = samples[i + symbol_pos + half] * osc.next();
        }
        self.tmp0.copy_from_slice(&self.tmp1);
        self.fft.forward(&mut self.tmp0);
        for i in 0..half {
            self.tmp1[i] = demod_or_erase(self.tmp0[i], self.tmp0[(i + half - 1) % half]);
        }
        self.tmp0.copy_from_slice(&self.tmp1);
        self.fft.forward(&mut self.tmp0);
        for (t, k) in self.tmp0.iter_mut().zip(&self.kern) {
            *t *= k;
        }
        self.fft.backward(&mut self.tmp0);

        let (mut shift, mut peak, mut next) = (0, 0.0f32, 0.0f32);
        for (i, t) in self.tmp0.iter().enumerate() {
            let power = t.norm_sqr();
            if power > peak {
                next = peak;
                peak = power;
                shift = i;
            } else if power > next {
                next = power;
            }
        }
        if peak <= next * 4.0 {
            return false;
        }
        let t = self.tmp0[shift];
        let pos_err = (t.im.atan2(t.re) * half as f32 / TAU).round_ties_even() as i32;
        if pos_err.unsigned_abs() as usize > self.guard_len / 2 {
            return false;
        }
        self.symbol_pos = (symbol_pos as i32 - pos_err) as usize;
        let mut cfo = shift as f32 * (TAU / half as f32) - frac_cfo;
        if cfo >= PI {
            cfo -= TAU;
        }
        self.cfo_rad = cfo;
        true
    }
}
