//! The transmitter: a worker thread turns a list of transmissions (a ping, a text, a
//! picture, the frames of a multi-frame picture, modem datagrams) into audio for a sound
//! card or a file, symbol by symbol, publishing its progress like the receiver does.

use crate::spectrum::SpectrumAnalyzer;
use cofdmtv_core::cofdmtv::{Encoder, RATES, TxRequest};
use cofdmtv_core::dsp::Cplx;
use cofdmtv_core::modem::{self, ModemEncoder, ModemRequest};
use cofdmtv_io::{AudioFormat, Container, Encoding, FileWriter, OutputOptions, OutputStream, Resampler, ResamplerQuality};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver as Rx, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Where the signal goes.
#[derive(Debug, Clone, PartialEq)]
pub enum OutputSpec {
    /// A sound-card output by name (or part of it); `None`: the system default.
    Device { name: Option<String> },
    /// A WAV or FLAC file (16 bit).
    File { path: PathBuf },
}

/// How the signal is put on the output's channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TxChannel {
    /// The real signal on every channel.
    #[default]
    Mono,
    /// On the left channel only.
    Left,
    /// On the right channel only.
    Right,
    /// The complex signal: real part left, imaginary part right (Shredpix "analytic").
    Iq,
}

impl TxChannel {
    pub const ALL: [TxChannel; 4] = [Self::Mono, Self::Left, Self::Right, Self::Iq];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mono => "Mono",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Iq => "I/Q",
        }
    }
}

/// The signal of one transmission.
#[derive(Debug, Clone)]
pub enum TxSignal {
    /// COFDMTV: a picture, a text or a ping.
    Cofdmtv(TxRequest),
    /// Modem datagrams (frames in one transmission).
    Modem(ModemRequest),
}

/// One transmission.
#[derive(Debug, Clone)]
pub struct TxJob {
    pub label: String,
    pub signal: TxSignal,
    /// Silence before it, seconds.
    pub gap_s: f32,
}

impl TxJob {
    /// A COFDMTV transmission.
    pub fn cofdmtv(label: impl Into<String>, request: TxRequest, gap_s: f32) -> TxJob {
        TxJob { label: label.into(), signal: TxSignal::Cofdmtv(request), gap_s }
    }

    /// A modem transmission.
    pub fn modem(label: impl Into<String>, request: ModemRequest, gap_s: f32) -> TxJob {
        TxJob { label: label.into(), signal: TxSignal::Modem(request), gap_s }
    }
}

/// What to transmit, and where.
#[derive(Debug, Clone)]
pub struct TxConfig {
    pub output: OutputSpec,
    pub channel: TxChannel,
    /// Sample rate to build the signal at (a COFDMTV rate; the modem needs 44.1 or 48 kHz);
    /// `None`: the sound card's rate if it fits, else 48 kHz.
    pub rate: Option<u32>,
    /// Level relative to the apps' (0 dB: about −12 dBFS RMS).
    pub gain_db: f32,
    pub jobs: Vec<TxJob>,
}

/// Things that happened.
#[derive(Debug, Clone)]
pub enum TxEvent {
    Log(String),
    /// Transmission `index` (of the jobs) starts.
    JobStarted { index: usize, label: String },
    /// Everything has been sent (and played out).
    Finished,
    Error(String),
    /// The worker has stopped (the last event).
    Stopped,
}

/// The transmitter's displays.
#[derive(Debug, Clone, Default)]
pub struct TxSnapshot {
    pub running: bool,
    /// The transmission on the air (0-based) and how many there are.
    pub job: usize,
    pub jobs: usize,
    pub label: String,
    /// Symbols of this transmission sent, of all.
    pub symbol: usize,
    pub symbols: usize,
    /// Signal seconds produced, and the estimated total.
    pub seconds: f64,
    pub total_seconds: f64,
    /// RMS and peak of the last symbol, dBFS.
    pub level: Option<(f32, f32)>,
    pub spectrum: Vec<f32>,
    pub spectrum_axis: (f64, f64),
    pub iq: bool,
    /// The sound card, or the file.
    pub destination: String,
    /// Rate of the signal, and of the output.
    pub rate: u32,
    pub output_rate: u32,
    pub channels: usize,
    pub underruns: u64,
}

/// Handle of a running transmitter. Dropping it stops the worker.
pub struct Transmitter {
    commands: Sender<()>,
    events: Rx<TxEvent>,
    shared: Arc<Mutex<(u64, TxSnapshot)>>,
    seen: u64,
    worker: Option<JoinHandle<()>>,
}

impl Transmitter {
    pub fn start(cfg: TxConfig) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let shared = Arc::new(Mutex::new((0, TxSnapshot::default())));
        let s = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("cofdmtv-tx".into())
            .spawn(move || {
                if let Err(e) = run(&cfg, &cmd_rx, &ev_tx, &s) {
                    let _ = ev_tx.send(TxEvent::Error(e));
                }
                if let Ok(mut g) = s.lock() {
                    g.0 += 1;
                    g.1.running = false;
                }
                let _ = ev_tx.send(TxEvent::Stopped);
            })
            .expect("spawning the transmitter thread");
        Self { commands: cmd_tx, events: ev_rx, shared, seen: 0, worker: Some(worker) }
    }

    pub fn stop(&self) {
        let _ = self.commands.send(());
    }

    pub fn poll_events(&self) -> Vec<TxEvent> {
        self.events.try_iter().collect()
    }

    pub fn snapshot_if_newer(&mut self) -> Option<TxSnapshot> {
        let guard = self.shared.lock().ok()?;
        if guard.0 == self.seen {
            return None;
        }
        self.seen = guard.0;
        Some(guard.1.clone())
    }

    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn join(&mut self) {
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

impl Drop for Transmitter {
    fn drop(&mut self) {
        self.stop();
        self.join();
    }
}

enum Sink {
    Device { stream: OutputStream, resampler: Option<Resampler> },
    File(FileWriter),
}

impl Sink {
    fn write(&mut self, frames: &[f32]) -> Result<(), String> {
        match self {
            Sink::Device { stream, resampler } => {
                let block = match resampler {
                    Some(r) => r.process(frames),
                    None => frames.to_vec(),
                };
                stream.write_blocking(&block, Duration::from_secs(5)).map_err(|e| e.to_string())?;
                if let Some(e) = stream.take_errors().into_iter().next() {
                    return Err(e.to_string());
                }
                Ok(())
            }
            Sink::File(w) => w.write(frames).map_err(|e| e.to_string()),
        }
    }

    fn underruns(&self) -> u64 {
        match self {
            Sink::Device { stream, .. } => stream.stats().underruns,
            Sink::File(_) => 0,
        }
    }
}

/// The rate to build the signal at for an output running at `output_rate`: as requested if
/// that is a COFDMTV rate, else the output's own if it is one, else 48 kHz; with modem
/// transmissions 44.1 or 48 kHz.
pub fn signal_rate(requested: Option<u32>, output_rate: u32, modem: bool) -> u32 {
    let ok = |r: &u32| if modem { modem::RATES.contains(r) } else { RATES.contains(r) };
    requested.filter(ok).unwrap_or(if ok(&output_rate) { output_rate } else { 48_000 })
}

/// The encoders of a session, one per kind.
struct Encoders {
    cofdmtv: Encoder,
    modem: Option<ModemEncoder>,
}

impl Encoders {
    /// Set up `signal`; returns the transmission's chunks and seconds.
    fn configure(&mut self, signal: &TxSignal) -> Result<(usize, f64), String> {
        match signal {
            TxSignal::Cofdmtv(r) => {
                self.cofdmtv.configure(r)?;
                let l = self.cofdmtv.layout();
                let n = self.cofdmtv.total_symbols();
                Ok((n, (n * l.extended_len) as f64 / f64::from(l.rate)))
            }
            TxSignal::Modem(r) => {
                let m = self.modem.as_mut().ok_or("the modem needs 44.1 or 48 kHz")?;
                m.configure(r)?;
                Ok((m.total_chunks(), m.total_seconds()))
            }
        }
    }

    fn next(&mut self, signal: &TxSignal) -> Option<&[Cplx]> {
        match signal {
            TxSignal::Cofdmtv(_) => self.cofdmtv.next_symbol(),
            TxSignal::Modem(_) => self.modem.as_mut()?.next_chunk(),
        }
    }

    fn produced(&self, signal: &TxSignal) -> usize {
        match signal {
            TxSignal::Cofdmtv(_) => self.cofdmtv.produced_symbols(),
            TxSignal::Modem(_) => self.modem.as_ref().map_or(0, ModemEncoder::produced_chunks),
        }
    }
}

fn run(cfg: &TxConfig, commands: &Rx<()>, events: &Sender<TxEvent>, shared: &Arc<Mutex<(u64, TxSnapshot)>>) -> Result<(), String> {
    let two = cfg.channel != TxChannel::Mono;
    let any_modem = cfg.jobs.iter().any(|j| matches!(j.signal, TxSignal::Modem(_)));
    let (mut sink, rate, out_format, destination) = match &cfg.output {
        OutputSpec::Device { name } => {
            let opts = OutputOptions {
                device: name.clone(),
                sample_rate: cfg.rate,
                channels: two.then_some(2),
                buffer: Duration::from_secs(1),
                start_threshold: Duration::from_millis(200),
            };
            let stream = OutputStream::open(&opts).map_err(|e| e.to_string())?;
            let format = stream.format();
            if two && format.channels < 2 {
                return Err(format!("{} has one channel: {} needs two", stream.device_name(), cfg.channel.label()));
            }
            let rate = signal_rate(cfg.rate, format.sample_rate, any_modem);
            let resampler = if rate == format.sample_rate {
                None
            } else {
                Some(Resampler::new(rate, format.sample_rate, format.channels, ResamplerQuality::High).map_err(|e| e.to_string())?)
            };
            let name = stream.device_name().to_string();
            (Sink::Device { stream, resampler }, rate, format, name)
        }
        OutputSpec::File { path } => {
            let rate = signal_rate(cfg.rate, 48_000, any_modem);
            let format = AudioFormat::new(rate, if two { 2 } else { 1 });
            let container = Container::from_path(path).unwrap_or(Container::Wav);
            let w = FileWriter::create(path, format, container, Encoding::Int16).map_err(|e| e.to_string())?;
            (Sink::File(w), rate, format, path.display().to_string())
        }
    };
    let channels = out_format.channels;
    let mut enc = Encoders { cofdmtv: Encoder::new(rate).ok_or(format!("{rate} Hz is not a COFDMTV rate"))?, modem: ModemEncoder::new(rate) };
    // Check every transmission first, and add up the time on the air.
    let mut total = 0.0;
    for job in &cfg.jobs {
        let (_, seconds) = enc.configure(&job.signal).map_err(|e| format!("{}: {e}", job.label))?;
        total += f64::from(job.gap_s) + seconds;
    }
    let _ = events.send(TxEvent::Log(format!(
        "transmitting {} transmission{} ({total:.1} s) to {destination} at {rate} Hz{}",
        cfg.jobs.len(),
        if cfg.jobs.len() == 1 { "" } else { "s" },
        if out_format.sample_rate == rate { String::new() } else { format!(" (resampled to {} Hz)", out_format.sample_rate) }
    )));
    let gain = 10f32.powf(cfg.gain_db / 20.0);
    let iq = cfg.channel == TxChannel::Iq;
    let mut spectrum = SpectrumAnalyzer::for_rate(rate, iq);
    let mut snap = TxSnapshot {
        running: true,
        jobs: cfg.jobs.len(),
        total_seconds: total,
        destination: destination.clone(),
        rate,
        output_rate: out_format.sample_rate,
        channels,
        iq,
        ..TxSnapshot::default()
    };
    let publish = |snap: &TxSnapshot| {
        if let Ok(mut g) = shared.lock() {
            g.0 += 1;
            g.1 = snap.clone();
        }
    };
    let stopped = || !matches!(commands.try_recv(), Err(TryRecvError::Empty));
    let mut frames = Vec::new();
    let mut seconds = 0.0;
    let silence_len = (rate / 10) as usize;
    'jobs: for (index, job) in cfg.jobs.iter().enumerate() {
        let _ = events.send(TxEvent::JobStarted { index, label: job.label.clone() });
        let silence = vec![0.0f32; silence_len * channels];
        let mut left = (job.gap_s.max(0.0) * rate as f32) as usize;
        while left > 0 {
            if stopped() {
                break 'jobs;
            }
            let n = left.min(silence_len);
            sink.write(&silence[..n * channels])?;
            left -= n;
            seconds += n as f64 / f64::from(rate);
        }
        let (chunks, _) = enc.configure(&job.signal)?;
        snap.job = index;
        snap.label = job.label.clone();
        snap.symbols = chunks;
        let started = Instant::now();
        let mut sent_s = 0.0;
        while let Some(chunk) = enc.next(&job.signal) {
            if stopped() {
                let _ = events.send(TxEvent::Log("stopped".into()));
                break 'jobs;
            }
            frames.clear();
            let (mut sum, mut peak) = (0.0f32, 0.0f32);
            for &z in chunk {
                let z = z * gain;
                let (re, im) = (z.re.clamp(-1.0, 1.0), z.im.clamp(-1.0, 1.0));
                spectrum.push(if iq { Cplx::new(re, im) } else { Cplx::new(re, 0.0) });
                let a = if iq { (re * re + im * im).sqrt() } else { re.abs() };
                sum += a * a;
                peak = peak.max(a);
                match cfg.channel {
                    TxChannel::Mono => frames.extend(std::iter::repeat_n(re, channels)),
                    TxChannel::Left | TxChannel::Right | TxChannel::Iq => {
                        let (l, r) = match cfg.channel {
                            TxChannel::Left => (re, 0.0),
                            TxChannel::Right => (0.0, re),
                            _ => (re, im),
                        };
                        frames.push(l);
                        frames.push(r);
                        frames.extend(std::iter::repeat_n(0.0, channels - 2));
                    }
                }
            }
            let len = chunk.len();
            sink.write(&frames)?;
            let chunk_s = len as f64 / f64::from(rate);
            seconds += chunk_s;
            sent_s += chunk_s;
            let db = |p: f32| (10.0 * p.max(1e-12).log10()).max(-120.0);
            snap.level = Some((db(sum / len.max(1) as f32), db(peak * peak)));
            snap.symbol = enc.produced(&job.signal);
            snap.seconds = seconds;
            snap.spectrum = spectrum.average().to_vec();
            snap.spectrum_axis = spectrum.axis();
            snap.underruns = sink.underruns();
            let _ = spectrum.take_rows();
            publish(&snap);
        }
        let _ = events.send(TxEvent::Log(format!(
            "{} sent: {sent_s:.1} s{}",
            job.label,
            if matches!(sink, Sink::File(_)) { format!(" (written in {:.2} s)", started.elapsed().as_secs_f64()) } else { String::new() }
        )));
    }
    match sink {
        Sink::Device { mut stream, mut resampler } => {
            if let Some(r) = &mut resampler {
                let rest = r.flush();
                let _ = stream.write_blocking(&rest, Duration::from_secs(2));
            }
            // Play out what is queued (up to a second), unless stopped meanwhile.
            let deadline = Instant::now() + Duration::from_secs(3);
            while stream.buffered_frames() > 0 && Instant::now() < deadline {
                if stopped() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            stream.drain(Duration::from_millis(200));
        }
        Sink::File(w) => w.finalize().map_err(|e| e.to_string())?,
    }
    let _ = events.send(TxEvent::Finished);
    Ok(())
}
