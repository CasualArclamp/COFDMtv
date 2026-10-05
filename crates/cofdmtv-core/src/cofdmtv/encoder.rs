//! The COFDMTV transmitter: Shredpix `encoder.hh` (pictures, ping) and Rattlegram
//! `encoder.hh` (text, ping), one symbol at a time.
//!
//! The two apps build the same signal with small differences, kept here per family:
//!
//! | | pictures (Shredpix) | text (Rattlegram) |
//! |---|---|---|
//! | reference for the payload | a pilot symbol | the preamble |
//! | PAPR reduction | preamble, pilot and payload, at 8 and 16 kHz only (4× oversampled) | every symbol; 4×, 2× or 1× oversampled by rate |
//! | guard crossfade | the whole guard interval | half of it for sync, preamble and payload |
//! | fancy header carriers | a fresh pilot sequence per line | the running noise sequence |
//!
//! A ping is sent the picture way unless it comes from the text side.

use super::meta::Meta;
use super::{Layout, Mode};
use crate::coding::bch::BchEncoder;
use crate::coding::mls::Mls;
use crate::coding::tables::BASE37_BITMAP;
use crate::coding::xorshift::Xorshift32;
use crate::coding::{base37, nrz};
use crate::dsp::{Cplx, Fft, improve_papr};

/// What to send.
#[derive(Debug, Clone)]
pub struct TxRequest {
    pub mode: Mode,
    /// The payload: for pictures up to 5380 bytes (zero padded), for text up to 170 bytes
    /// (the mode follows from the length when sent as [`Mode::Text`]); ignored for a ping.
    pub payload: Vec<u8>,
    pub call_sign: String,
    /// Centre frequency, Hz (negative values only make sense for I/Q output).
    pub carrier_hz: i32,
    /// Symbols of noise before the signal (180 ms each).
    pub noise_symbols: usize,
    /// Draw the call sign into the waterfall after the signal.
    pub fancy_header: bool,
    /// Build a ping the Rattlegram way (text family) rather than the Shredpix way.
    pub text_family: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Noise,
    Sync,
    Preamble,
    Pilot,
    Payload,
    Fancy,
    Silence,
    Done,
}

pub struct Encoder {
    layout: Layout,
    fft: Fft,
    /// Oversampling factor and FFT for PAPR reduction (`None`: not used).
    papr: Option<(usize, Fft)>,
    text: bool,
    carrier_offset: i32,
    freq: Vec<Cplx>,
    temp: Vec<Cplx>,
    guard: Vec<Cplx>,
    prev: Vec<Cplx>,
    out: Vec<Cplx>,
    /// Payload constellation points, carrier by carrier, symbol after symbol.
    cons: Vec<Cplx>,
    meta_bits: [u8; 255],
    call: [u8; 9],
    noise_seq: Mls,
    step: Step,
    noise_count: usize,
    fancy_line: usize,
    symbol_number: usize,
    symbol_count: usize,
    pay_car_cnt: usize,
    pay_car_off: i32,
    has_pilot: bool,
    total: usize,
    produced: usize,
}

impl Encoder {
    /// An encoder for one of the [`super::RATES`].
    pub fn new(rate: u32) -> Option<Self> {
        let layout = Layout::new(rate)?;
        let n = layout.symbol_len;
        Some(Self {
            layout,
            fft: Fft::new(n),
            papr: None,
            text: false,
            carrier_offset: 0,
            freq: vec![Cplx::new(0.0, 0.0); n],
            temp: vec![Cplx::new(0.0, 0.0); n],
            guard: vec![Cplx::new(0.0, 0.0); layout.guard_len],
            prev: Vec::new(),
            out: vec![Cplx::new(0.0, 0.0); layout.extended_len],
            cons: Vec::new(),
            meta_bits: [0; 255],
            call: [0; 9],
            noise_seq: Mls::new(super::NOISE_POLY),
            step: Step::Done,
            noise_count: 0,
            fancy_line: 0,
            symbol_number: 0,
            symbol_count: 0,
            pay_car_cnt: 0,
            pay_car_off: 0,
            has_pilot: false,
            total: 0,
            produced: 0,
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Prepare a transmission; the symbols follow from [`Self::next_symbol`].
    pub fn configure(&mut self, req: &TxRequest) -> Result<(), String> {
        base37::check(&req.call_sign)?;
        let mut mode = req.mode;
        let mut payload = req.payload.clone();
        if let Mode::Text(_) = mode {
            // The text mode follows from the length, as in Rattlegram.
            mode = Mode::text_for(payload.len()).ok_or_else(|| format!("text of {} bytes: at most {}", payload.len(), super::TEXT_BYTES))?;
        }
        match mode {
            Mode::Image(_) if payload.len() > super::IMAGE_BYTES => {
                return Err(format!("payload of {} bytes: at most {}", payload.len(), super::IMAGE_BYTES));
            }
            Mode::Text(n) | Mode::Image(n) if !(6..=16).contains(&n) => return Err(format!("unknown mode {n}")),
            _ => {}
        }
        let rate = self.layout.rate as i32;
        let half_bw = mode.carriers() as i32 * 25 / 8; // carriers × 6.25 Hz / 2
        if req.carrier_hz.abs() + half_bw > rate / 2 {
            return Err(format!("carrier {} Hz: the signal must stay within ±{} Hz", req.carrier_hz, rate / 2));
        }
        self.text = matches!(mode, Mode::Text(_)) || (mode == Mode::Ping && req.text_family);
        let fact = if self.text {
            Some(((32000 + self.layout.rate / 2) / self.layout.rate) as usize)
        } else if self.layout.rate <= 16000 {
            Some(4)
        } else {
            None
        };
        if self.papr.as_ref().map(|(f, _)| *f) != fact {
            self.papr = fact.map(|f| (f, Fft::new(f * self.layout.symbol_len)));
        }
        self.carrier_offset = (i64::from(req.carrier_hz) * self.layout.symbol_len as i64 / i64::from(self.layout.rate)) as i32;
        let meta = Meta { mode: mode.number(), call: base37::encode(&req.call_sign) };
        self.meta_bits = meta.codeword(&BchEncoder::default());
        self.call = [0; 9];
        for (c, b) in self.call.iter_mut().zip(req.call_sign.bytes()) {
            *c = base37::digit(b);
        }
        self.noise_seq = Mls::new(super::NOISE_POLY);
        self.guard.fill(Cplx::new(0.0, 0.0));
        self.noise_count = req.noise_symbols;
        self.fancy_line = if req.fancy_header { 11 } else { 0 };
        self.symbol_number = 0;
        self.symbol_count = mode.symbols();
        self.pay_car_cnt = mode.carriers();
        self.pay_car_off = -(self.pay_car_cnt as i32) / 2;
        self.has_pilot = matches!(mode, Mode::Image(_));
        self.prev = vec![Cplx::new(0.0, 0.0); self.pay_car_cnt];
        self.cons.clear();
        if let Some(code) = mode.code() {
            payload.resize(code.data_bits / 8, 0);
            Xorshift32::scramble(&mut payload);
            let bits = code.encode(&payload);
            let psk = mode.modulation();
            let k = psk.bits();
            self.cons = bits
                .chunks_exact(k)
                .map(|b| {
                    let v: Vec<f32> = b.iter().map(|&x| nrz(x != 0)).collect();
                    psk.map(&v)
                })
                .collect();
        }
        self.step = Step::Noise;
        self.produced = 0;
        self.total = req.noise_symbols + 2 + usize::from(self.has_pilot) + self.symbol_count + self.fancy_line + 1;
        Ok(())
    }

    /// Symbols of the whole transmission (each [`Layout::extended_len`] samples).
    pub fn total_symbols(&self) -> usize {
        self.total
    }

    /// Symbols produced so far.
    pub fn produced_symbols(&self) -> usize {
        self.produced
    }

    /// The next symbol with its guard interval (complex baseband shifted to the carrier;
    /// the real part is the audio signal), or `None` at the end.
    pub fn next_symbol(&mut self) -> Option<&[Cplx]> {
        let mut data_symbol = false;
        loop {
            match self.step {
                Step::Noise => {
                    if self.noise_count > 0 {
                        self.noise_count -= 1;
                        self.noise_symbol();
                        break;
                    }
                    self.step = Step::Sync;
                }
                Step::Sync => {
                    self.schmidl_cox();
                    self.step = Step::Preamble;
                    data_symbol = true;
                    break;
                }
                Step::Preamble => {
                    self.preamble();
                    data_symbol = true;
                    self.step = if self.symbol_count == 0 {
                        Step::Fancy
                    } else if self.has_pilot {
                        Step::Pilot
                    } else {
                        Step::Payload
                    };
                    break;
                }
                Step::Pilot => {
                    self.pilot_block();
                    self.step = Step::Payload;
                    break;
                }
                Step::Payload => {
                    self.payload_symbol();
                    data_symbol = true;
                    self.symbol_number += 1;
                    if self.symbol_number == self.symbol_count {
                        self.step = Step::Fancy;
                    }
                    break;
                }
                Step::Fancy => {
                    if self.fancy_line > 0 {
                        self.fancy_line -= 1;
                        self.fancy_symbol();
                        break;
                    }
                    self.step = Step::Silence;
                }
                Step::Silence => {
                    self.temp.fill(Cplx::new(0.0, 0.0));
                    self.step = Step::Done;
                    break;
                }
                Step::Done => return None,
            }
        }
        // The guard interval cross-fades from the previous symbol's continuation into this
        // symbol's cyclic prefix.
        let (n, g) = (self.layout.symbol_len, self.layout.guard_len);
        let crossfade_half = self.text && data_symbol;
        for i in 0..g {
            let mut x = i as f32 / (g - 1) as f32;
            if crossfade_half {
                x = x.min(0.5) / 0.5;
            }
            let y = 0.5 * (1.0 - (std::f32::consts::PI * x).cos());
            self.out[i] = (1.0 - y) * self.guard[i] + y * self.temp[i + n - g];
        }
        self.guard.copy_from_slice(&self.temp[..g]);
        self.out[g..].copy_from_slice(&self.temp);
        self.produced += 1;
        Some(&self.out)
    }

    #[inline]
    fn bin(&self, carrier: i32) -> usize {
        self.layout.bin(carrier + self.carrier_offset)
    }

    fn clear(&mut self) {
        self.freq.fill(Cplx::new(0.0, 0.0));
    }

    /// To the time domain, with PAPR reduction if `papr` and the family uses it here.
    fn transform(&mut self, papr: bool) {
        if let Some((fact, fft)) = &mut self.papr
            && (papr || self.text)
        {
            improve_papr(&mut self.freq, *fact, fft);
        }
        self.temp.copy_from_slice(&self.freq);
        self.fft.backward(&mut self.temp);
        let scale = 1.0 / (8.0 * self.layout.symbol_len as f32).sqrt();
        for t in &mut self.temp {
            *t *= scale;
        }
    }

    fn schmidl_cox(&mut self) {
        let mut seq = Mls::new(super::COR_SEQ_POLY);
        let factor = (2.0 * self.layout.symbol_len as f32 / super::COR_SEQ_LEN as f32).sqrt();
        self.clear();
        let b = self.bin(super::COR_SEQ_OFF - 2);
        self.freq[b] = Cplx::new(factor, 0.0);
        for i in 0..super::COR_SEQ_LEN {
            let b = self.bin(2 * i + super::COR_SEQ_OFF);
            self.freq[b] = Cplx::new(nrz(seq.next()), 0.0);
        }
        for i in 0..super::COR_SEQ_LEN {
            let (b, p) = (self.bin(2 * i + super::COR_SEQ_OFF), self.bin(2 * (i - 1) + super::COR_SEQ_OFF));
            let prev = self.freq[p];
            self.freq[b] *= prev;
        }
        self.transform(false);
    }

    fn preamble(&mut self) {
        let mut seq = Mls::new(super::PRE_SEQ_POLY);
        let factor = (self.layout.symbol_len as f32 / super::PRE_SEQ_LEN as f32).sqrt();
        self.clear();
        let b = self.bin(super::PRE_SEQ_OFF - 1);
        self.freq[b] = Cplx::new(factor, 0.0);
        for i in 0..super::PRE_SEQ_LEN {
            let b = self.bin(i + super::PRE_SEQ_OFF);
            self.freq[b] = Cplx::new(nrz(self.meta_bits[i as usize] != 0), 0.0);
        }
        for i in 0..super::PRE_SEQ_LEN {
            let (b, p) = (self.bin(i + super::PRE_SEQ_OFF), self.bin(i - 1 + super::PRE_SEQ_OFF));
            let prev = self.freq[p];
            self.freq[b] *= prev;
        }
        for i in 0..super::PRE_SEQ_LEN {
            let b = self.bin(i + super::PRE_SEQ_OFF);
            self.freq[b] *= nrz(seq.next());
        }
        if self.text {
            // The preamble's carriers are the text payload's reference.
            for i in 0..self.pay_car_cnt {
                self.prev[i] = self.freq[self.bin(i as i32 + super::TEXT_CAR_OFF)];
            }
        }
        self.transform(true);
    }

    fn pilot_block(&mut self) {
        let mut seq = Mls::new(super::PILOT_POLY);
        let factor = (self.layout.symbol_len as f32 / self.pay_car_cnt as f32).sqrt();
        self.clear();
        for i in 0..self.pay_car_cnt {
            let v = Cplx::new(factor * nrz(seq.next()), 0.0);
            let b = self.bin(i as i32 + self.pay_car_off);
            self.freq[b] = v;
            self.prev[i] = v;
        }
        self.transform(true);
    }

    fn payload_symbol(&mut self) {
        self.clear();
        let base = self.pay_car_cnt * self.symbol_number;
        for i in 0..self.pay_car_cnt {
            self.prev[i] *= self.cons[base + i];
            let b = self.bin(i as i32 + self.pay_car_off);
            self.freq[b] = self.prev[i];
        }
        self.transform(true);
    }

    fn fancy_symbol(&mut self) {
        let row = |j: usize, line: usize, call: &[u8; 9]| BASE37_BITMAP[call[j] as usize + 37 * line];
        let active = 1 + (0..9).map(|j| row(j, self.fancy_line, &self.call).count_ones() as usize).sum::<usize>();
        let factor = (self.layout.symbol_len as f32 / active as f32).sqrt();
        let mut pilot = Mls::new(super::PILOT_POLY);
        self.clear();
        for j in 0..9 {
            let pixels = row(j, self.fancy_line, &self.call);
            for i in 0..8 {
                if pixels & (1 << (7 - i)) != 0 {
                    let bit = if self.text { self.noise_seq.next() } else { pilot.next() };
                    let b = self.bin(((8 * j + i) * 3) as i32 + super::FANCY_OFF);
                    self.freq[b] = Cplx::new(factor * nrz(bit), 0.0);
                }
            }
        }
        self.transform(false);
    }

    fn noise_symbol(&mut self) {
        let factor = (self.layout.symbol_len as f32 / self.pay_car_cnt as f32).sqrt();
        self.clear();
        for i in 0..self.pay_car_cnt {
            let re = nrz(self.noise_seq.next());
            let im = nrz(self.noise_seq.next());
            let b = self.bin(i as i32 + self.pay_car_off);
            self.freq[b] = factor * Cplx::new(re, im);
        }
        self.transform(false);
    }
}
