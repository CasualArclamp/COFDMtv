//! The COFDMTV receiver: Assempix `decoder.hh` (pictures) and Rattlegram `decoder.hh`
//! (text) in one, block by block like Rattlegram's `feed`/`process`.
//!
//! Samples go through a DC blocker and a Hilbert filter (real input) into a buffer of four
//! symbols; the synchroniser looks at every new sample. Every [`Layout::extended_len`]
//! samples a block is complete ([`Decoder::push`] returns `true`) and [`Decoder::process`]
//! handles it: it decodes a preamble found during the block, then takes the next symbol at
//! the position the synchroniser found — always a whole symbol later, as the buffer has
//! moved on by one symbol. Payload carriers are demodulated against the previous symbol,
//! their common phase slope (timing and frequency drift) removed with a Theil–Sen fit, and
//! turned into soft bits scaled by the symbol's signal-to-noise estimate.
//!
//! Once a payload is complete the soft bits are handed out ([`Decoder::take_codeword`]) to
//! be decoded by [`decode_codeword`] — a list decoder run over up to 65536 bits, which a
//! caller may want on another thread while the receiver listens on.

use super::meta::{Meta, MetaDecoder, MetaError};
use crate::dsp::schmidl_cox::{SchmidlCox, SyncParams};
use super::{Layout, Mode};
use crate::coding::base37;
use crate::coding::polar::CaScl;
use crate::coding::psk::Psk;
use crate::coding::xorshift::Xorshift32;
use crate::dsp::{BipBuffer, BlockDc, Cplx, Fft, Hilbert, Phasor, TheilSen, demod_or_erase};

/// What a block brought.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A sync symbol, but its preamble could not be decoded.
    PreambleFailed,
    /// A preamble with a mode COFDMtv does not know, or an invalid call sign.
    Unsupported { mode: u8, call: String, cfo_hz: f32 },
    /// A ping: a call sign, nothing more.
    Ping { call: String, cfo_hz: f32 },
    /// A transmission starts: its payload symbols follow.
    Sync { mode: Mode, call: String, cfo_hz: f32 },
    /// All payload symbols are in: take the codeword and decode it.
    Done,
}

/// The soft bits of a complete payload.
#[derive(Debug, Clone)]
pub struct Codeword {
    pub mode: Mode,
    pub call: String,
    pub cfo_hz: f32,
    /// Soft bits, positive for 0, as many as the code sends.
    pub soft: Vec<f32>,
    /// Median signal-to-noise ratio of the payload symbols, dB.
    pub snr_db: f32,
}

/// A decoded payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Payload {
    pub mode: Mode,
    pub call: String,
    pub cfo_hz: f32,
    /// The payload bytes (5380 for pictures; text up to its first zero byte).
    pub data: Vec<u8>,
    /// Bits the decoder had to correct.
    pub flips: u32,
    pub snr_db: f32,
}

/// List sizes: pictures (65536-bit code) and text (2048-bit code). The apps use 4 or 8
/// (pictures) and 16 or 32 (text) depending on the phone's SIMD width.
pub const IMAGE_LIST: usize = 16;
pub const TEXT_LIST: usize = 32;

/// Decoders for [`decode_codeword`], allocated once (the picture decoder takes ~20 MB).
pub struct PolarDecoders {
    image: Option<Box<CaScl<IMAGE_LIST>>>,
    text: Option<Box<CaScl<TEXT_LIST>>>,
}

impl Default for PolarDecoders {
    fn default() -> Self {
        Self { image: None, text: None }
    }
}

/// Decode a complete payload; `None` when no list path passes the CRC.
pub fn decode_codeword(cw: &Codeword, decoders: &mut PolarDecoders) -> Option<Payload> {
    let code = cw.mode.code()?;
    let decoded = match cw.mode {
        Mode::Image(_) => decoders.image.get_or_insert_with(|| Box::new(CaScl::new(16))).decode(&cw.soft, &code)?,
        Mode::Text(_) => decoders.text.get_or_insert_with(|| Box::new(CaScl::new(11))).decode(&cw.soft, &code)?,
        Mode::Ping => return None,
    };
    let mut data = decoded.data;
    Xorshift32::scramble(&mut data);
    if let Mode::Text(_) = cw.mode {
        let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
        data.truncate(end);
    }
    Some(Payload { mode: cw.mode, call: cw.call.clone(), cfo_hz: cw.cfo_hz, data, flips: decoded.flips, snr_db: cw.snr_db })
}

#[derive(Debug, Clone, Copy)]
struct Detection {
    cfo_rad: f32,
    position: usize,
}

/// The receiver.
pub struct Decoder {
    layout: Layout,
    fft: Fft,
    correlator: SchmidlCox,
    block_dc: BlockDc,
    hilbert: Hilbert,
    buffer: BipBuffer,
    tse: TheilSen,
    osc: Phasor,
    meta: MetaDecoder,
    accumulated: usize,
    stored: Option<Detection>,
    staged: Option<Detection>,
    /// The window of the buffer at the last block boundary.
    window: usize,
    // The transmission being received.
    mode: Option<Mode>,
    call: String,
    cfo_rad: f32,
    psk: Psk,
    symbol_position: usize,
    symbol_number: i32,
    symbol_count: i32,
    carrier_count: usize,
    carrier_offset: i32,
    temp: Vec<Cplx>,
    freq: Vec<Cplx>,
    prev: Vec<Cplx>,
    cons: Vec<Cplx>,
    index: Vec<f32>,
    phase: Vec<f32>,
    code: Vec<f32>,
    snr: Vec<f32>,
    /// Payload symbols demodulated so far (see [`Self::points_seq`]).
    points_seq: u64,
    codeword: Option<Codeword>,
}

impl Decoder {
    /// A receiver for one of the [`super::RATES`].
    pub fn new(rate: u32) -> Option<Self> {
        let layout = Layout::new(rate)?;
        let n = layout.symbol_len;
        let filter_len = (((33 * rate / 8000) & !3) | 1) as usize;
        let search_pos = layout.extended_len;
        Some(Self {
            layout,
            fft: Fft::new(n),
            correlator: SchmidlCox::new(search_pos, n / 2, layout.guard_len, super::sync_sequence(n / 2), SyncParams::COFDMTV),
            block_dc: BlockDc::new(filter_len),
            hilbert: Hilbert::new(filter_len),
            buffer: BipBuffer::new(4 * layout.extended_len),
            tse: TheilSen::default(),
            osc: Phasor::default(),
            meta: MetaDecoder::default(),
            accumulated: 0,
            stored: None,
            staged: None,
            window: 0,
            mode: None,
            call: String::new(),
            cfo_rad: 0.0,
            psk: Psk::Qpsk,
            symbol_position: 0,
            symbol_number: 0,
            symbol_count: 0,
            carrier_count: 0,
            carrier_offset: 0,
            temp: vec![Cplx::new(0.0, 0.0); layout.extended_len],
            freq: vec![Cplx::new(0.0, 0.0); n],
            prev: Vec::new(),
            cons: Vec::new(),
            index: Vec::new(),
            phase: Vec::new(),
            code: Vec::new(),
            snr: Vec::new(),
            points_seq: 0,
            codeword: None,
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Feed one sample of a real signal; `true` when a block is complete (call
    /// [`Self::process`]).
    #[inline]
    pub fn push_real(&mut self, x: f32) -> bool {
        let z = self.hilbert.process(self.block_dc.process(x));
        self.push(z)
    }

    /// Feed one sample of an analytic (I/Q) signal; `true` when a block is complete.
    #[inline]
    pub fn push(&mut self, z: Cplx) -> bool {
        let start = self.buffer.push(z);
        if self.correlator.process(self.buffer.at(start)) {
            self.stored = Some(Detection { cfo_rad: self.correlator.cfo_rad, position: self.correlator.symbol_pos + self.accumulated });
        }
        self.accumulated += 1;
        if self.accumulated == self.layout.extended_len {
            self.accumulated = 0;
            self.window = start;
            if let Some(d) = self.stored.take() {
                self.staged = Some(d);
            }
            return true;
        }
        false
    }

    /// Handle a complete block.
    pub fn process(&mut self) -> Option<Event> {
        let mut event = None;
        if let Some(det) = self.staged.take() {
            event = Some(self.preamble(det));
        }
        if self.mode.is_some() && self.symbol_number < self.symbol_count {
            let window = self.buffer.at(self.window);
            for (t, &w) in self.temp.iter_mut().zip(&window[self.symbol_position..]) {
                *t = w * self.osc.next();
            }
            self.freq.copy_from_slice(&self.temp[..self.layout.symbol_len]);
            self.fft.forward(&mut self.freq);
            if self.symbol_number >= 0 {
                for i in 0..self.carrier_count {
                    let c = self.freq[self.bin(i as i32 + self.carrier_offset)];
                    self.cons[i] = demod_or_erase(c, self.prev[i]);
                }
                self.compensate();
                self.demap();
            }
            if self.symbol_number >= -1 {
                // The reference (pilot or preamble) and every payload symbol become the
                // reference of the next.
                for i in 0..self.carrier_count {
                    self.prev[i] = self.freq[self.bin(i as i32 + self.carrier_offset)];
                }
            }
            self.symbol_number += 1;
            if self.symbol_number == self.symbol_count {
                self.finish();
                event = Some(Event::Done);
            }
        }
        event
    }

    /// The complete payload's soft bits, once [`Event::Done`] came.
    pub fn take_codeword(&mut self) -> Option<Codeword> {
        self.codeword.take()
    }

    /// The transmission being received: mode, payload symbols received and expected.
    pub fn progress(&self) -> Option<(Mode, usize, usize)> {
        let mode = self.mode?;
        (self.symbol_number < self.symbol_count).then(|| (mode, self.symbol_number.max(0) as usize, self.symbol_count as usize))
    }

    /// Call sign and frequency offset (Hz) of the transmission being received.
    pub fn current(&self) -> Option<(&str, f32)> {
        self.mode.map(|_| (self.call.as_str(), self.cfo_hz(self.cfo_rad)))
    }

    /// The last payload symbol's constellation (after phase correction), with its
    /// modulation; erased carriers are 0.
    pub fn constellation(&self) -> (&[Cplx], Psk) {
        (&self.cons, self.psk)
    }

    /// Counts the payload symbols demodulated, each of which replaces
    /// [`Self::constellation`]: a display collecting the points takes them when this
    /// changes.
    pub fn points_seq(&self) -> u64 {
        self.points_seq
    }

    /// Signal-to-noise ratio of the last payload symbol, dB.
    pub fn last_snr_db(&self) -> Option<f32> {
        self.snr.last().map(|p| crate::dsp::decibel(*p))
    }

    fn cfo_hz(&self, rad: f32) -> f32 {
        rad * self.layout.rate as f32 / std::f32::consts::TAU
    }

    #[inline]
    fn bin(&self, carrier: i32) -> usize {
        self.layout.bin(carrier)
    }

    fn preamble(&mut self, det: Detection) -> Event {
        let cfo_hz = self.cfo_hz(det.cfo_rad);
        let mut nco = Phasor::default();
        nco.omega(-det.cfo_rad);
        let window = self.buffer.at(self.window);
        let n = self.layout.symbol_len;
        for (f, &w) in self.freq.iter_mut().zip(&window[det.position..det.position + n]) {
            *f = w * nco.next();
        }
        self.fft.forward(&mut self.freq);
        let layout = self.layout;
        let meta = match self.meta.decode(&mut self.freq, |c| layout.bin(c)) {
            Ok(m) => m,
            Err(MetaError::Damaged) => return Event::PreambleFailed,
            Err(MetaError::BadCall(m)) => return Event::Unsupported { mode: m.mode, call: base37::decode(m.call).trim().to_string(), cfo_hz },
        };
        let call = meta.call_sign();
        let Some(mode) = Mode::from_number(meta.mode) else {
            return Event::Unsupported { mode: meta.mode, call, cfo_hz };
        };
        if mode == Mode::Ping {
            return Event::Ping { call, cfo_hz };
        }
        self.start(mode, &meta, det);
        Event::Sync { mode, call, cfo_hz }
    }

    fn start(&mut self, mode: Mode, meta: &Meta, det: Detection) {
        self.mode = Some(mode);
        self.call = meta.call_sign();
        self.cfo_rad = det.cfo_rad;
        self.osc.omega(-det.cfo_rad);
        self.symbol_position = det.position;
        self.psk = mode.modulation();
        self.carrier_count = mode.carriers();
        self.carrier_offset = match mode {
            Mode::Text(_) => super::TEXT_CAR_OFF,
            _ => -(self.carrier_count as i32) / 2,
        };
        self.symbol_count = mode.symbols() as i32;
        // Pictures: this block holds the preamble (skipped), the next the pilot. Text:
        // the preamble itself is the reference.
        self.symbol_number = if matches!(mode, Mode::Image(_)) { -2 } else { -1 };
        let zero = Cplx::new(0.0, 0.0);
        self.prev = vec![zero; self.carrier_count];
        self.cons = vec![zero; self.carrier_count];
        self.index = vec![0.0; self.carrier_count];
        self.phase = vec![0.0; self.carrier_count];
        self.code = vec![0.0; self.carrier_count * mode.symbols() * self.psk.bits()];
        self.snr.clear();
        self.codeword = None;
    }

    /// Remove the phase slope across the carriers (timing and frequency drift since the
    /// previous symbol), fitted to the decision errors.
    fn compensate(&mut self) {
        let mut count = 0;
        for i in 0..self.carrier_count {
            let con = self.cons[i];
            if con.re != 0.0 && con.im != 0.0 {
                let hard = self.psk.nearest(con);
                self.index[count] = (i as i32 + self.carrier_offset) as f32;
                let e = con * hard.conj();
                self.phase[count] = e.im.atan2(e.re);
                count += 1;
            }
        }
        self.tse.compute(&self.index[..count], &self.phase[..count]);
        for i in 0..self.carrier_count {
            let a = -self.tse.eval((i as i32 + self.carrier_offset) as f32);
            self.cons[i] *= Cplx::new(a.cos(), a.sin());
        }
    }

    /// Soft bits of the current symbol, scaled by its signal-to-noise estimate.
    fn demap(&mut self) {
        self.points_seq += 1;
        let (mut sp, mut np) = (0.0f32, 0.0f32);
        for &c in &self.cons {
            let hard = self.psk.nearest(c);
            sp += hard.norm_sqr();
            np += (c - hard).norm_sqr();
        }
        let precision = sp / np;
        self.snr.push(precision);
        // A noiseless loopback would give infinity; 40 dB is decided anyway.
        let precision = precision.min(1e4);
        let bits = self.psk.bits();
        let base = bits * self.carrier_count * self.symbol_number as usize;
        for (i, &c) in self.cons.iter().enumerate() {
            let at = base + bits * i;
            self.psk.soft(&mut self.code[at..at + bits], c, precision);
        }
    }

    fn finish(&mut self) {
        let Some(mode) = self.mode.take() else { return };
        let mut snr = self.snr.clone();
        snr.sort_by(f32::total_cmp);
        let snr_db = snr.get(snr.len() / 2).map_or(0.0, |p| crate::dsp::decibel(*p));
        self.codeword = Some(Codeword {
            mode,
            call: std::mem::take(&mut self.call),
            cfo_hz: self.cfo_hz(self.cfo_rad),
            soft: std::mem::take(&mut self.code),
            snr_db,
        });
    }
}
