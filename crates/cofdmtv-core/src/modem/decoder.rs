//! The modem's receiver (aicodix/modem `decode.cc`), sample by sample.
//!
//! The original reads its input in one go; here the same steps run as samples arrive: the
//! synchroniser looks at every sample; once it finds a frame, the two sync symbols give the
//! channel estimate (corrected for the sample-rate offset with a Theil–Sen fit across the
//! tones) and the meta symbol is decoded straight from the buffer; then every symbol is
//! taken as soon as it is complete — at the same sample positions as the original.

use super::{BLOCK_LENGTH, MLS0_POLY, MLS0_SEED, MLS1_POLY, MLS2_POLY, ModemLayout, ModemMode, SEED_TONES, TONE_COUNT};
use crate::coding::crc::{Crc16, Crc32, POLY_DATA, POLY_META};
use crate::coding::mls::Mls;
use crate::coding::polar::CaScl;
use crate::coding::tables::MODEM_FROZEN_256_72;
use crate::coding::xorshift::Xorshift32;
use crate::coding::{base40, bits, hadamard, nrz};
use crate::dsp::schmidl_cox::{SchmidlCox, SyncParams};
use crate::dsp::{BipBuffer, BlockDc, Cplx, Fft, Hilbert, Phasor, TheilSen};

/// The modem's list size (the original runs 32 paths in 16-bit lanes).
pub const MODEM_LIST: usize = 32;

/// What a sample brought.
#[derive(Debug, Clone, PartialEq)]
pub enum ModemEvent {
    /// A frame starts: its meta data decoded.
    Sync { mode: ModemMode, call: String, cfo_hz: f32 },
    /// A sync symbol, but the meta data could not be decoded (or names no valid mode).
    MetaFailed,
    /// A frame's pilots could not be read (seed damaged): the frame is lost.
    Lost,
    /// All payload symbols are in: take the codeword.
    Done,
}

/// The soft bits of a complete frame, in code order.
#[derive(Debug, Clone)]
pub struct ModemCodeword {
    pub mode: ModemMode,
    pub call: String,
    pub cfo_hz: f32,
    pub soft: Vec<f32>,
    /// Lowest, median and highest Es/N0 of the symbols, dB.
    pub snr_db: (f32, f32, f32),
}

/// A decoded datagram.
#[derive(Debug, Clone, PartialEq)]
pub struct Datagram {
    pub mode: ModemMode,
    pub call: String,
    pub cfo_hz: f32,
    /// The mode's data bytes (zero padded by the sender if shorter).
    pub data: Vec<u8>,
    pub snr_db: (f32, f32, f32),
}

/// Decode a complete frame: list decoding, CRC-32 check, descrambling. `decoder` is
/// reused between calls (it takes ~40 MB).
pub fn decode_datagram(cw: &ModemCodeword, decoder: &mut Option<Box<CaScl<MODEM_LIST>>>) -> Option<Datagram> {
    let mode = cw.mode;
    let order = mode.code_order();
    let dec = decoder.get_or_insert_with(|| Box::new(CaScl::new(16)));
    dec.decode_plain(&cw.soft, mode.frozen(), order);
    let crc_bits = mode.data_bits() + 32;
    let mut crc = Crc32::new(POLY_DATA);
    let path = (0..MODEM_LIST).find(|&k| {
        crc.reset();
        for i in 0..crc_bits {
            crc.bit(dec.message_bit(i, k));
        }
        crc.value() == 0
    })?;
    let mut data = vec![0u8; mode.data_bytes()];
    for i in 0..mode.data_bits() {
        bits::set_le_bit(&mut data, i, dec.message_bit(i, path));
    }
    Xorshift32::scramble(&mut data);
    Some(Datagram { mode, call: cw.call.clone(), cfo_hz: cw.cfo_hz, data, snr_db: cw.snr_db })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Search,
    /// Receiving a frame: samples until the next symbol is complete.
    Frame { wait: usize },
}

pub struct ModemDecoder {
    layout: ModemLayout,
    fft: Fft,
    correlator: SchmidlCox,
    block_dc: BlockDc,
    hilbert: Hilbert,
    buffer: BipBuffer,
    tse: TheilSen,
    osc: Phasor,
    meta_decoder: CaScl<MODEM_LIST>,
    state: State,
    window: usize,
    tone_off: i32,
    // The frame.
    mode: Option<ModemMode>,
    call: String,
    cfo_rad: f32,
    j: usize,
    k: usize,
    /// Pilot position of the last symbol demodulated.
    seed_off_last: usize,
    seq1: Mls,
    chan: Vec<Cplx>,
    tone: Vec<Cplx>,
    demod: Vec<Cplx>,
    tdom: Vec<Cplx>,
    index: Vec<f32>,
    phase: Vec<f32>,
    perm: Vec<f32>,
    snr: Vec<f32>,
    codeword: Option<ModemCodeword>,
    /// The last symbol's demodulated data tones (for a constellation display).
    pub points: Vec<Cplx>,
}

impl ModemDecoder {
    pub fn new(rate: u32) -> Option<Self> {
        let layout = ModemLayout::new(rate)?;
        let n = layout.symbol_len;
        // The sync symbol's sequence after differential demodulation between tones.
        let tone_off = -(TONE_COUNT as i32) / 2;
        let mut seq0 = Mls::with_seed(MLS0_POLY, MLS0_SEED);
        let mut sequence = vec![Cplx::new(0.0, 0.0); n];
        let mut prv = 0.0f32;
        for i in 0..TONE_COUNT {
            let cur = nrz(seq0.next());
            sequence[layout.bin(i as i32 + tone_off)] = Cplx::new(prv * cur, 0.0);
            prv = cur;
        }
        let zero = Cplx::new(0.0, 0.0);
        Some(Self {
            layout,
            fft: Fft::new(n),
            correlator: SchmidlCox::new(layout.extended_len, n, layout.guard_len, sequence, SyncParams::MODEM),
            block_dc: BlockDc::new(129),
            hilbert: Hilbert::new(129),
            buffer: BipBuffer::new(5 * layout.extended_len),
            tse: TheilSen::default(),
            osc: Phasor::default(),
            meta_decoder: CaScl::new(8),
            state: State::Search,
            window: 0,
            tone_off,
            mode: None,
            call: String::new(),
            cfo_rad: 0.0,
            j: 0,
            k: 0,
            seed_off_last: 0,
            seq1: Mls::new(MLS1_POLY),
            chan: vec![zero; TONE_COUNT],
            tone: vec![zero; TONE_COUNT],
            demod: vec![zero; TONE_COUNT],
            tdom: vec![zero; n],
            index: vec![0.0; TONE_COUNT],
            phase: vec![0.0; TONE_COUNT],
            perm: Vec::new(),
            snr: Vec::new(),
            codeword: None,
            points: Vec::new(),
        })
    }

    pub fn layout(&self) -> ModemLayout {
        self.layout
    }

    /// Feed a sample of a real signal.
    #[inline]
    pub fn push_real(&mut self, x: f32) -> Option<ModemEvent> {
        let z = self.hilbert.process(self.block_dc.process(x));
        self.push(z)
    }

    /// Feed a sample of an analytic (I/Q) signal.
    pub fn push(&mut self, z: Cplx) -> Option<ModemEvent> {
        self.window = self.buffer.push(z);
        let found = self.correlator.process(self.buffer.at(self.window));
        match self.state {
            State::Search if found => Some(self.begin_frame()),
            State::Search => None,
            State::Frame { wait } if wait > 1 => {
                self.state = State::Frame { wait: wait - 1 };
                None
            }
            State::Frame { .. } => self.next_symbol(),
        }
    }

    /// The frame being received: mode, payload symbols in and expected.
    pub fn progress(&self) -> Option<(ModemMode, usize, usize)> {
        let mode = self.mode?;
        Some((mode, self.j.saturating_sub(1), mode.symbols()))
    }

    pub fn current(&self) -> Option<(&str, f32)> {
        self.mode.map(|_| (self.call.as_str(), self.cfo_rad * self.layout.rate as f32 / std::f32::consts::TAU))
    }

    pub fn take_codeword(&mut self) -> Option<ModemCodeword> {
        self.codeword.take()
    }

    pub fn last_snr_db(&self) -> Option<f32> {
        self.snr.last().map(|p| 10.0 * p.log10())
    }

    /// FFT of the symbol starting at `start` in the current window, mixed down; the
    /// oscillator then steps over the following guard interval as well, unless `no_guard`.
    fn symbol_at(&mut self, start: usize, no_guard: bool) {
        let n = self.layout.symbol_len;
        let window = self.buffer.at(self.window);
        for (t, &w) in self.tdom.iter_mut().zip(&window[start..start + n]) {
            *t = w * self.osc.next();
        }
        if !no_guard {
            for _ in 0..self.layout.guard_len {
                self.osc.next();
            }
        }
        self.fft.forward(&mut self.tdom);
        for i in 0..TONE_COUNT {
            self.tone[i] = self.tdom[self.layout.bin(i as i32 + self.tone_off)];
        }
    }

    fn begin_frame(&mut self) -> ModemEvent {
        let pos = self.correlator.symbol_pos;
        self.cfo_rad = self.correlator.cfo_rad;
        self.osc.omega(-self.cfo_rad);
        let n = self.layout.symbol_len;
        // The two sync symbols: channel estimate, sample-rate offset.
        self.symbol_at(pos, true);
        let first = self.tone.clone();
        self.symbol_at(pos + n, false);
        self.chan.copy_from_slice(&self.tone);
        for i in 0..TONE_COUNT {
            self.index[i] = (self.tone_off + i as i32) as f32;
            let d = demod(self.chan[i], first[i]);
            self.phase[i] = d.im.atan2(d.re);
        }
        self.tse.compute(&self.index, &self.phase);
        let mut seq0 = Mls::with_seed(MLS0_POLY, MLS0_SEED);
        for i in 0..TONE_COUNT {
            let a = self.tse.eval((i as i32 + self.tone_off) as f32);
            let t = first[i] * Cplx::new(a.cos(), a.sin());
            self.chan[i] = 0.5 * (self.chan[i] + t);
            self.chan[i] *= nrz(seq0.next());
        }
        // The meta symbol, right away from the buffer.
        self.seq1 = Mls::new(MLS1_POLY);
        self.mode = None;
        self.j = 0;
        self.k = 0;
        self.perm = vec![0.0; 256];
        self.snr.clear();
        self.symbol_at(pos + n + self.layout.extended_len, false);
        if !self.demodulate(0, 1) {
            return ModemEvent::Lost;
        }
        let mut code = vec![0.0f32; 256];
        super::unshuffle(&mut code, &self.perm, 8);
        self.meta_decoder.decode_plain(&code, &MODEM_FROZEN_256_72, 8);
        let mut crc0 = Crc16::new(POLY_META);
        let Some(path) = (0..MODEM_LIST).find(|&k| {
            crc0.reset();
            for i in 0..72 {
                crc0.bit(self.meta_decoder.message_bit(i, k));
            }
            crc0.value() == 0
        }) else {
            return ModemEvent::MetaFailed;
        };
        let md = (0..56).fold(0u64, |md, i| md | u64::from(self.meta_decoder.message_bit(i, path)) << i);
        let call = md >> 8;
        let Some(mode) = ModemMode::from_byte((md & 255) as u8).filter(|_| call > 0 && call < base40::LIMIT) else {
            return ModemEvent::MetaFailed;
        };
        self.mode = Some(mode);
        self.call = base40::decode(call).trim().to_string();
        self.perm = vec![0.0; 1 << mode.code_order()];
        self.k = 0;
        self.j = 1;
        self.update_pilots();
        // The original consumes symbol_pos + symbol_len + extended_len samples, then a
        // symbol's worth before each payload symbol, which then starts at the window's
        // first sample.
        self.state = State::Frame { wait: pos + n + 2 * self.layout.extended_len };
        ModemEvent::Sync { mode, call: self.call.clone(), cfo_hz: self.cfo_rad * self.layout.rate as f32 / std::f32::consts::TAU }
    }

    fn next_symbol(&mut self) -> Option<ModemEvent> {
        let Some(mode) = self.mode else {
            self.state = State::Search;
            return None;
        };
        self.symbol_at(0, false);
        if !self.demodulate(self.j, mode.modulation.bits()) {
            self.mode = None;
            self.state = State::Search;
            return Some(ModemEvent::Lost);
        }
        self.update_pilots();
        self.j += 1;
        if self.j > mode.symbols() {
            self.state = State::Search;
            self.finish(mode);
            return Some(ModemEvent::Done);
        }
        self.state = State::Frame { wait: self.layout.extended_len };
        None
    }

    /// Demodulate symbol `j` (in `self.tone`) against the channel estimate into soft bits
    /// at `self.k`; `false` if its pilots cannot be read.
    fn demodulate(&mut self, j: usize, mod_bits: usize) -> bool {
        let seed_off = super::seed_off(j);
        for i in (seed_off..TONE_COUNT).step_by(BLOCK_LENGTH) {
            self.tone[i] *= nrz(self.seq1.next());
        }
        for i in 0..TONE_COUNT {
            self.demod[i] = demod(self.tone[i], self.chan[i]);
        }
        let mut seed = [0i8; SEED_TONES];
        for (i, s) in seed.iter_mut().enumerate() {
            *s = (127.0 * self.demod[i * BLOCK_LENGTH + seed_off].re).round_ties_even().clamp(-127.0, 127.0) as i8;
        }
        let Some(seed_value) = hadamard::decode(&seed) else { return false };
        let code = hadamard::encode(seed_value);
        for i in 0..SEED_TONES {
            let at = BLOCK_LENGTH * i + seed_off;
            self.tone[at] *= f32::from(code[i]);
            self.demod[at] *= f32::from(code[i]);
            self.index[i] = (self.tone_off + at as i32) as f32;
            self.phase[i] = self.demod[at].im.atan2(self.demod[at].re);
        }
        self.tse.compute(&self.index[..SEED_TONES], &self.phase[..SEED_TONES]);
        for i in 0..TONE_COUNT {
            let a = self.tse.eval((i as i32 + self.tone_off) as f32);
            self.demod[i] *= Cplx::new(a.cos(), -a.sin());
            self.chan[i] *= Cplx::new(a.cos(), a.sin());
        }
        if seed_value != 0 {
            let mut seq = Mls::with_seed(MLS2_POLY, seed_value);
            for i in 0..TONE_COUNT {
                if i % BLOCK_LENGTH != seed_off {
                    self.demod[i] *= nrz(seq.next());
                }
            }
        }
        // Signal-to-noise ratio from the decision errors (pilots count as +1).
        let (mut sp, mut np) = (0.0f32, 0.0f32);
        let mut l = self.k;
        let mut hard = [0.0f32; 12];
        for i in 0..TONE_COUNT {
            let mut point = Cplx::new(1.0, 0.0);
            if i % BLOCK_LENGTH != seed_off {
                let bits = super::tone_bits(mod_bits, l);
                super::demap_hard(&mut hard[..bits], self.demod[i], bits);
                point = super::map_bits(&hard[..bits], bits);
                l += bits;
            }
            sp += point.norm_sqr();
            np += (self.demod[i] - point).norm_sqr();
        }
        let precision = sp / np;
        self.snr.push(precision);
        let precision = precision.min(1023.0);
        self.points.clear();
        for i in 0..TONE_COUNT {
            if i % BLOCK_LENGTH != seed_off {
                let bits = super::tone_bits(mod_bits, self.k);
                super::demap_soft(&mut self.perm[self.k..self.k + bits], self.demod[i], precision, bits);
                self.k += bits;
                self.points.push(self.demod[i]);
            }
        }
        // Keep the pilots for the channel update after this symbol.
        self.seed_off_last = seed_off;
        true
    }

    /// Move the channel estimate halfway to the last symbol's pilots.
    fn update_pilots(&mut self) {
        for i in (self.seed_off_last..TONE_COUNT).step_by(BLOCK_LENGTH) {
            self.chan[i] = 0.5 * (self.chan[i] + self.tone[i]);
        }
    }

    fn finish(&mut self, mode: ModemMode) {
        let mut snr = self.snr.clone();
        snr.sort_by(f32::total_cmp);
        let db = |p: f32| 10.0 * p.log10();
        let snr_db = (db(snr[0]), db(snr[snr.len() / 2]), db(snr[snr.len() - 1]));
        let mut soft = vec![0.0f32; self.perm.len()];
        super::unshuffle(&mut soft, &self.perm, mode.code_order());
        self.codeword = Some(ModemCodeword {
            mode,
            call: std::mem::take(&mut self.call),
            cfo_hz: self.cfo_rad * self.layout.rate as f32 / std::f32::consts::TAU,
            soft,
            snr_db,
        });
        self.mode = None;
    }
}

/// The decoder's division: `curr / prev`, or 0 for no reference or an implausible result.
#[inline]
fn demod(curr: Cplx, prev: Cplx) -> Cplx {
    if prev.norm_sqr() > 0.0 {
        let d = crate::dsp::div(curr, prev);
        if d.norm_sqr() < 4.0 {
            return d;
        }
    }
    Cplx::new(0.0, 0.0)
}
