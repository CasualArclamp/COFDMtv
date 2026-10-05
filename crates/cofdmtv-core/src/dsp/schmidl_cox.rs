//! Schmidl & Cox synchronisation (Assempix/Rattlegram and aicodix/modem
//! `schmidl_cox.hh`).
//!
//! The sync symbol repeats after `len` samples (half a COFDMTV symbol; two whole modem
//! symbols back to back), so the timing metric |P|²/R² peaks there; its phase gives the
//! fractional frequency offset. Once the metric falls again, `len` samples are
//! transformed, demodulated differentially between neighbouring bins and
//! cross-correlated (via FFT) with the known sequence: the peak gives the integer
//! frequency offset — the carrier frequency, as the receiver looks at the whole audio
//! band — and its phase the remaining timing error.

use crate::dsp::{Cplx, Delay, FallingEdge, Fft, Phasor, SchmittTrigger, SlidingSum, demod_or_erase};
use std::f32::consts::{PI, TAU};

/// What differs between the COFDMTV and the modem synchroniser.
#[derive(Debug, Clone, Copy)]
pub struct SyncParams {
    /// Schmitt trigger thresholds of the timing metric, per sample of the matched sum.
    pub low: f64,
    pub high: f64,
    /// Smallest power of the denominator, per sample of `len`.
    pub min_r: f64,
    /// Erase differential pairs whose bins are below the mean power (the modem).
    pub erase_weak: bool,
    /// Largest timing error accepted, in guard intervals.
    pub max_pos_err: f32,
}

impl SyncParams {
    pub const COFDMTV: SyncParams = SyncParams { low: 0.17, high: 0.19, min_r: 0.0001, erase_weak: false, max_pos_err: 0.5 };
    pub const MODEM: SyncParams = SyncParams { low: 0.07, high: 0.09, min_r: 0.00001, erase_weak: true, max_pos_err: 1.0 };
}

pub struct SchmidlCox {
    search_pos: usize,
    /// The period of the sync symbol and the size of the FFTs here.
    half: usize,
    params: SyncParams,
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
    /// A correlator looking at windows of the receive buffer from `search_pos`, for a
    /// sync symbol repeating after `half` samples, with `guard_len` samples of guard
    /// interval; `sequence` is what its differential demodulation yields in each bin.
    pub fn new(search_pos: usize, half: usize, guard_len: usize, sequence: Vec<Cplx>, params: SyncParams) -> Self {
        assert_eq!(sequence.len(), half);
        let match_len = guard_len | 1;
        let match_del = (match_len - 1) / 2;
        let mut kern = sequence;
        let mut fft = Fft::new(half);
        fft.forward(&mut kern);
        for k in &mut kern {
            *k = k.conj() / half as f32;
        }
        Self {
            search_pos,
            half,
            params,
            guard_len,
            match_del,
            fft,
            cor: SlidingSum::new(half),
            pwr: SlidingSum::new(2 * half),
            matched: SlidingSum::new(match_len),
            delay: Delay::new(match_del),
            threshold: SchmittTrigger::new((params.low * match_len as f64) as f32, (params.high * match_len as f64) as f32),
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
        let min_r = (self.params.min_r * half as f64) as f32;
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
        let floor = if self.params.erase_weak { self.tmp0.iter().map(|c| c.norm_sqr()).sum::<f32>() / half as f32 } else { 0.0 };
        for i in 0..half {
            let (curr, prev) = (self.tmp0[i], self.tmp0[(i + half - 1) % half]);
            self.tmp1[i] = if self.params.erase_weak {
                // The modem: both bins above the mean power, quotient below 2.
                if curr.norm_sqr() > floor && prev.norm_sqr() > floor {
                    let c = super::div(curr, prev);
                    if c.norm_sqr() < 4.0 { c } else { Cplx::new(0.0, 0.0) }
                } else {
                    Cplx::new(0.0, 0.0)
                }
            } else {
                demod_or_erase(curr, prev)
            };
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
        // COFDMTV allows half a guard interval (integer division), the modem a whole one.
        let limit = if self.params.max_pos_err < 1.0 { self.guard_len / 2 } else { self.guard_len };
        if pos_err.unsigned_abs() as usize > limit {
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
