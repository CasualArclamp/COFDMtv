//! The modem's transmitter (aicodix/modem `encode.cc`), symbol by symbol.

use super::{BLOCK_LENGTH, MLS0_POLY, MLS0_SEED, MLS1_POLY, MLS2_POLY, ModemLayout, ModemMode, SEED_TONES, TONE_COUNT};
use crate::coding::crc::{Crc16, Crc32, POLY_DATA, POLY_META};
use crate::coding::mls::Mls;
use crate::coding::tables::MODEM_FROZEN_256_72;
use crate::coding::xorshift::Xorshift32;
use crate::coding::{base40, bits, hadamard, nrz, polar};
use crate::dsp::{Cplx, Fft};

/// What to send: datagrams of the mode's size (shorter ones are zero padded), one frame
/// each, in one transmission.
#[derive(Debug, Clone)]
pub struct ModemRequest {
    pub mode: ModemMode,
    pub call_sign: String,
    /// Carrier frequency, Hz: a multiple of 300 (negative only for I/Q output).
    pub carrier_hz: i32,
    pub frames: Vec<Vec<u8>>,
    /// Noise symbols before the first frame, at least one: the original sends one; more
    /// give a radio's VOX and AGC time to settle (the lead-in of v2 pictures).
    pub noise_symbols: usize,
    /// COFDMTV's fancy header after the last frame, the call sign drawn into the waterfall
    /// (v2 pictures; not in the original).
    pub fancy_header: bool,
}

impl ModemRequest {
    /// A transmission as the original sends it: one noise symbol, no fancy header.
    pub fn new(mode: ModemMode, call_sign: impl Into<String>, carrier_hz: i32, frames: Vec<Vec<u8>>) -> Self {
        Self { mode, call_sign: call_sign.into(), carrier_hz, frames, noise_symbols: 1, fancy_header: false }
    }
}

/// Seconds of a transmission of `frames` frames in `mode` after `noise_symbols` noise
/// symbols, with COFDMTV's fancy header (2.16 s) after it if `fancy_header`. The same at
/// 44.1 and 48 kHz: a symbol with its guard interval is 41/300 s.
pub fn transmission_seconds(mode: ModemMode, frames: usize, noise_symbols: usize, fancy_header: bool) -> f64 {
    // The two sync symbols have no guard interval between them; a last one ends it all.
    let guards = (noise_symbols.max(1) + frames * (3 + mode.symbols())) * 41 - frames + 1;
    let fancy = if fancy_header { 12.0 * crate::cofdmtv::SYMBOL_SECONDS } else { 0.0 };
    guards as f64 / 300.0 + fancy
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Noise,
    Sync1,
    Sync2,
    Symbol(usize),
    Finish,
    Fancy,
    Done,
}

pub struct ModemEncoder {
    layout: ModemLayout,
    fft: Fft,
    mode: ModemMode,
    tone_off: i32,
    weight: Vec<f32>,
    guard: Vec<Cplx>,
    out: Vec<Cplx>,
    tone: Vec<Cplx>,
    temp: Vec<Cplx>,
    fdom: Vec<Cplx>,
    tdom: Vec<Cplx>,
    test: Vec<Cplx>,
    /// The meta symbol's code bits (±1), permuted.
    meta: Vec<f32>,
    /// The current frame's code bits (±1), permuted.
    perm: Vec<f32>,
    frames: Vec<Vec<u8>>,
    frame: usize,
    seq1: Mls,
    seed_off: usize,
    k: usize,
    step: Step,
    produced: usize,
    total: usize,
    /// Noise symbols still to send, and the lead-in's noise before the original one.
    noise_left: usize,
    lead: Mls,
    /// The fancy header after the last frame (COFDMTV's encoder, at this rate).
    fancy: Option<Box<crate::cofdmtv::Encoder>>,
    fancy_on: bool,
    /// PAPR of the symbols sent (the original prints it).
    pub papr_db: Vec<f32>,
}

impl ModemEncoder {
    pub fn new(rate: u32) -> Option<Self> {
        let layout = ModemLayout::new(rate)?;
        let g = layout.guard_len;
        let mut weight = vec![0.0f32; g];
        for (i, w) in weight.iter_mut().enumerate() {
            *w = if i < g / 4 {
                0.0
            } else if i < g / 4 + g / 2 {
                let x = (i - g / 4) as f32 / (g / 2 - 1) as f32;
                0.5 * (1.0 - (std::f32::consts::PI * x).cos())
            } else {
                1.0
            };
        }
        let n = layout.symbol_len;
        let zero = Cplx::new(0.0, 0.0);
        Some(Self {
            layout,
            fft: Fft::new(n),
            mode: ModemMode { modulation: super::Modulation::Qpsk, rate: super::CodeRate::Half, normal: false },
            tone_off: 0,
            weight,
            guard: vec![zero; g],
            out: Vec::with_capacity(layout.extended_len),
            tone: vec![zero; TONE_COUNT],
            temp: vec![zero; TONE_COUNT],
            fdom: vec![zero; n],
            tdom: vec![zero; n],
            test: vec![zero; n],
            meta: vec![0.0; 256],
            perm: Vec::new(),
            frames: Vec::new(),
            frame: 0,
            seq1: Mls::new(MLS1_POLY),
            seed_off: 0,
            k: 0,
            step: Step::Done,
            produced: 0,
            total: 0,
            noise_left: 0,
            lead: Mls::new(crate::cofdmtv::NOISE_POLY),
            fancy: None,
            fancy_on: false,
            papr_db: Vec::new(),
        })
    }

    pub fn layout(&self) -> ModemLayout {
        self.layout
    }

    pub fn configure(&mut self, req: &ModemRequest) -> Result<(), String> {
        let call = base40::check(&req.call_sign)?;
        if req.frames.is_empty() {
            return Err("nothing to send".into());
        }
        if let Some(f) = req.frames.iter().find(|f| f.len() > req.mode.data_bytes()) {
            return Err(format!("a datagram of {} bytes: {} holds {}", f.len(), req.mode.label(), req.mode.data_bytes()));
        }
        if req.carrier_hz % super::CARRIER_STEP_HZ != 0 {
            return Err(format!("carrier {} Hz: the modem needs a multiple of 300 Hz", req.carrier_hz));
        }
        let rate = self.layout.rate as i32;
        if req.carrier_hz.abs() + super::BANDWIDTH_HZ / 2 > rate / 2 {
            return Err(format!("carrier {} Hz: the signal must stay within ±{} Hz", req.carrier_hz, rate / 2));
        }
        self.mode = req.mode;
        let offset = (i64::from(req.carrier_hz) * self.layout.symbol_len as i64 / i64::from(self.layout.rate)) as i32;
        self.tone_off = offset - TONE_COUNT as i32 / 2;
        // Meta data: call sign and mode, CRC-16, a polar code of 256 bits.
        let md = (call << 8) | u64::from(req.mode.byte());
        let mut mesg = vec![0u8; 72];
        for (i, m) in mesg.iter_mut().enumerate().take(56) {
            *m = ((md >> i) & 1) as u8;
        }
        let mut crc0 = Crc16::new(POLY_META);
        let cs = crc0.u64(md << 8);
        for i in 0..16 {
            mesg[56 + i] = ((cs >> i) & 1) as u8;
        }
        let mut code = vec![0u8; 256];
        polar::encode(&mut code, &mesg, &MODEM_FROZEN_256_72);
        let code: Vec<f32> = code.iter().map(|&b| nrz(b != 0)).collect();
        super::shuffle(&mut self.meta, &code, 8);
        self.fancy_on = req.fancy_header;
        if req.fancy_header {
            let rate = self.layout.rate;
            let fancy = self.fancy.get_or_insert_with(|| Box::new(crate::cofdmtv::Encoder::new(rate).expect("COFDMTV runs at the modem's rates")));
            fancy.configure_fancy_header(&req.call_sign, req.carrier_hz)?;
        }
        self.frames = req.frames.clone();
        self.frame = 0;
        self.guard.fill(Cplx::new(0.0, 0.0));
        self.noise_left = req.noise_symbols.max(1);
        self.lead = Mls::new(crate::cofdmtv::NOISE_POLY);
        self.step = Step::Noise;
        self.produced = 0;
        self.total = self.noise_left + self.frames.len() * (3 + self.mode.symbols()) + 1 + if self.fancy_on { 12 } else { 0 };
        self.papr_db.clear();
        Ok(())
    }

    /// Chunks of the transmission (symbols with their guard intervals) so far, and in all.
    pub fn produced_chunks(&self) -> usize {
        self.produced
    }

    pub fn total_chunks(&self) -> usize {
        self.total
    }

    /// Seconds of the whole transmission.
    pub fn total_seconds(&self) -> f64 {
        let lead_in = self.total - self.frames.len() * (3 + self.mode.symbols()) - 1 - if self.fancy_on { 12 } else { 0 };
        transmission_seconds(self.mode, self.frames.len(), lead_in, self.fancy_on)
    }

    /// The next piece of the signal (complex; the real part is the audio), or `None`.
    pub fn next_chunk(&mut self) -> Option<&[Cplx]> {
        self.out.clear();
        match self.step {
            Step::Noise if self.noise_left > 1 => {
                // A lead-in symbol before the original's noise symbol: noise from a running
                // sequence, so no two are alike (repeated symbols could look like a sync).
                self.noise_left -= 1;
                for t in &mut self.tone {
                    *t = Cplx::new(nrz(self.lead.next()), 0.0);
                }
                self.symbol(-3);
            }
            Step::Noise => {
                let mut noise = Mls::new(MLS2_POLY);
                for t in &mut self.tone {
                    *t = Cplx::new(nrz(noise.next()), 0.0);
                }
                self.symbol(-3);
                self.step = Step::Sync1;
            }
            Step::Sync1 => {
                self.start_frame();
                let mut seq0 = Mls::with_seed(MLS0_POLY, MLS0_SEED);
                for t in &mut self.tone {
                    *t = Cplx::new(nrz(seq0.next()), 0.0);
                }
                self.symbol(-2);
                self.step = Step::Sync2;
            }
            Step::Sync2 => {
                self.symbol(-1);
                self.step = Step::Symbol(0);
            }
            Step::Symbol(j) => {
                self.data_symbol(j);
                self.step = if j < self.mode.symbols() {
                    Step::Symbol(j + 1)
                } else {
                    self.frame += 1;
                    if self.frame < self.frames.len() { Step::Sync1 } else { Step::Finish }
                };
            }
            Step::Finish => {
                for (g, w) in self.guard.iter_mut().zip(&self.weight) {
                    *g *= 1.0 - w;
                }
                self.out.extend_from_slice(&self.guard);
                self.guard.fill(Cplx::new(0.0, 0.0));
                self.step = if self.fancy_on { Step::Fancy } else { Step::Done };
            }
            Step::Fancy => {
                // COFDMTV's symbols carry about 3 dB less than the modem's (an RMS of 0.35
                // against 0.5): matched, so the header is as loud as the frames.
                let gain = std::f32::consts::SQRT_2;
                match self.fancy.as_mut().and_then(|f| f.next_symbol()) {
                    Some(symbol) => self.out.extend(symbol.iter().map(|&c| c * gain)),
                    None => {
                        self.step = Step::Done;
                        return None;
                    }
                }
            }
            Step::Done => return None,
        }
        self.produced += 1;
        Some(&self.out)
    }

    /// Encode the current frame's datagram.
    fn start_frame(&mut self) {
        let mode = self.mode;
        let mut data = self.frames[self.frame].clone();
        data.resize(mode.data_bytes(), 0);
        Xorshift32::scramble(&mut data);
        let data_bits = mode.data_bits();
        let mut mesg = vec![0u8; data_bits + 32];
        for (i, m) in mesg.iter_mut().enumerate().take(data_bits) {
            *m = u8::from(bits::get_le_bit(&data, i));
        }
        let mut crc1 = Crc32::new(POLY_DATA);
        let sum = crc1.bytes(&data);
        for i in 0..32 {
            mesg[data_bits + i] = ((sum >> i) & 1) as u8;
        }
        let order = mode.code_order();
        let mut code = vec![0u8; 1 << order];
        polar::encode(&mut code, &mesg, mode.frozen());
        let code: Vec<f32> = code.iter().map(|&b| nrz(b != 0)).collect();
        self.perm = vec![0.0; 1 << order];
        super::shuffle(&mut self.perm, &code, order);
        self.seq1 = Mls::new(MLS1_POLY);
        self.k = 0;
    }

    /// Meta (j = 0) or payload symbol `j`.
    fn data_symbol(&mut self, j: usize) {
        self.seed_off = super::seed_off(j);
        let mod_bits = self.mode.modulation.bits();
        let mut m = 0;
        for i in 0..TONE_COUNT {
            self.tone[i] = if i % BLOCK_LENGTH == self.seed_off {
                Cplx::new(nrz(self.seq1.next()), 0.0)
            } else if j > 0 {
                let bits = super::tone_bits(mod_bits, self.k);
                let c = super::map_bits(&self.perm[self.k..self.k + bits], bits);
                self.k += bits;
                c
            } else {
                let c = super::map_bits(&self.meta[m..m + 1], 1);
                m += 1;
                c
            };
        }
        self.symbol(j as i32);
    }

    #[inline]
    fn bin(&self, i: usize) -> usize {
        self.layout.bin(i as i32 + self.tone_off)
    }

    /// Build symbol `n` from `self.tone` (noise −3, sync −2 and −1, else with the PAPR
    /// search) and append it, with its guard interval, to `self.out`.
    fn symbol(&mut self, n: i32) {
        let scale = 0.5 / (TONE_COUNT as f32).sqrt();
        let zero = Cplx::new(0.0, 0.0);
        if n < 0 {
            self.fdom.fill(zero);
            for i in 0..TONE_COUNT {
                let b = self.bin(i);
                self.fdom[b] = self.tone[i];
            }
            self.tdom.copy_from_slice(&self.fdom);
            self.fft.backward(&mut self.tdom);
            for t in &mut self.tdom {
                *t *= scale;
            }
        } else {
            let mut best = 1000.0f32;
            for seed_value in 0..128u32 {
                self.temp.copy_from_slice(&self.tone);
                let seed = hadamard::encode(seed_value);
                for i in 0..SEED_TONES {
                    self.temp[i * BLOCK_LENGTH + self.seed_off] *= f32::from(seed[i]);
                }
                if seed_value != 0 {
                    let mut seq = Mls::with_seed(MLS2_POLY, seed_value);
                    for i in 0..TONE_COUNT {
                        if i % BLOCK_LENGTH != self.seed_off {
                            self.temp[i] *= nrz(seq.next());
                        }
                    }
                }
                self.fdom.fill(zero);
                for i in 0..TONE_COUNT {
                    let b = self.bin(i);
                    self.fdom[b] = self.temp[i];
                }
                self.test.copy_from_slice(&self.fdom);
                self.fft.backward(&mut self.test);
                let (mut peak, mut mean) = (0.0f32, 0.0f32);
                for t in &mut self.test {
                    *t *= scale;
                    let p = t.norm_sqr();
                    peak = peak.max(p);
                    mean += p;
                }
                mean /= self.layout.symbol_len as f32;
                let papr = peak / mean;
                if papr < best {
                    best = papr;
                    self.tdom.copy_from_slice(&self.test);
                    if papr < 5.0 {
                        break;
                    }
                }
            }
            self.papr_db.push(10.0 * best.log10());
        }
        self.clipping_and_filtering(scale);
        let (g, s) = (self.layout.guard_len, self.layout.symbol_len);
        if n != -1 {
            for i in 0..g {
                let w = self.weight[i];
                self.guard[i] = (1.0 - w) * self.guard[i] + w * self.tdom[i + s - g];
            }
            self.out.extend_from_slice(&self.guard);
        }
        self.guard.copy_from_slice(&self.tdom[..g]);
        self.out.extend_from_slice(&self.tdom);
    }

    fn clipping_and_filtering(&mut self, scale: f32) {
        for t in &mut self.tdom {
            let p = t.norm_sqr();
            if p > 1.0 {
                *t /= p.sqrt();
            }
        }
        self.fdom.copy_from_slice(&self.tdom);
        self.fft.forward(&mut self.fdom);
        let n = self.layout.symbol_len;
        let factor = 1.0 / (scale * n as f32);
        for i in 0..n {
            let j = self.bin(i);
            if i >= TONE_COUNT {
                self.fdom[j] = Cplx::new(0.0, 0.0);
            } else {
                self.fdom[j] *= factor;
            }
        }
        self.tdom.copy_from_slice(&self.fdom);
        self.fft.backward(&mut self.tdom);
        for t in &mut self.tdom {
            *t *= scale;
            *t = Cplx::new(t.re.clamp(-1.0, 1.0), t.im.clamp(-1.0, 1.0));
        }
    }
}
