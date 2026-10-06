//! The receiver: a worker thread reads the source, runs the COFDMTV decoder, the aicodix
//! modem decoder (at 44.1 and 48 kHz) and the spectrum analyser over every sample, and
//! publishes what the displays need; a second thread decodes completed payloads (list
//! decoding of up to 65536 bits takes a while), rebuilds multi-frame pictures and files and
//! saves what arrives, so the worker listens on.
//!
//! # How the GUI talks to it
//!
//! Like DecDRM's engine: the worker owns all receiver state and publishes a [`RxSnapshot`]
//! (a complete copy of the displays' data) about 30 times a second into an
//! `Arc<Mutex<…>>`; [`Receiver::snapshot_if_newer`] hands out a clone of each new one, so
//! the GUI never holds a lock while drawing. Discrete happenings (a transmission found, a
//! picture, message or file received, log lines) come as [`RxEvent`]s through a channel,
//! where none are lost between two polls; waterfall rows through a bounded one of their own.

use crate::input::{ChannelSel, InputSpec, Source, SourceInfo};
use crate::payload::{self, Picture, ReceivedFile, TextMessage};
use crate::spectrum::SpectrumAnalyzer;
use chrono::Local;
use cofdmtv_core::coding::psk::Psk;
use cofdmtv_core::coding::qam::Qam;
use cofdmtv_core::cofdmtv::multiframe::{self, Progress, Reassembler};
use cofdmtv_core::cofdmtv::{Codeword, Decoder, Event, Mode, PolarDecoders, decode_codeword};
use cofdmtv_core::modem::{self, ModemCodeword, ModemDecoder, ModemEvent, Modulation, decode_datagram};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver as Rx, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// What to receive.
#[derive(Debug, Clone, PartialEq)]
pub struct RxConfig {
    pub input: InputSpec,
    pub channel: ChannelSel,
    /// Save received pictures and files (and a log of the messages) here.
    pub save_dir: Option<PathBuf>,
}

/// Which kind of signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// COFDMTV: pictures, text, pings.
    Cofdmtv,
    /// The aicodix modem: datagrams.
    Modem,
}

/// Things that happened, in order.
#[derive(Debug, Clone)]
pub enum RxEvent {
    /// A line for the log.
    Log(String),
    /// A transmission (or a modem frame) starts.
    Sync { kind: SignalKind, label: String, call: String, cfo_hz: f32, bandwidth_hz: f32 },
    /// A ping.
    Ping { call: String, cfo_hz: f32 },
    /// A sync symbol whose preamble (meta data) could not be decoded.
    PreambleFailed,
    /// A payload is complete and being decoded.
    Decoding { label: String, call: String },
    /// A payload could not be decoded.
    DecodeFailed { label: String, call: String, snr_db: f32 },
    Picture(Picture),
    Text(TextMessage),
    /// A modem datagram or a file rebuilt from frames (not a picture).
    File(ReceivedFile),
    /// A frame of a multi-frame picture or file arrived.
    MultiFrame { call: String, have: usize, need: usize, size: usize },
    /// The recording ended.
    EndOfInput,
    /// Something went wrong; the receiver stops.
    Error(String),
    /// The worker has stopped (the last event).
    Stopped,
}

/// A transmission being received.
#[derive(Debug, Clone, PartialEq)]
pub struct Receiving {
    pub kind: SignalKind,
    pub label: String,
    pub call: String,
    pub cfo_hz: f32,
    pub bandwidth_hz: f32,
    /// Payload symbols in, of all.
    pub symbol: usize,
    pub symbols: usize,
}

/// The displays' data.
#[derive(Debug, Clone, Default)]
pub struct RxSnapshot {
    pub source: SourceInfo,
    /// Position in a recording, or time since the start (sound card), seconds.
    pub position_s: f64,
    /// RMS and peak level of the last 100 ms, dBFS.
    pub level: Option<(f32, f32)>,
    /// Averaged spectrum, dB, and its axis (first bin, bin spacing; Hz).
    pub spectrum: Vec<f32>,
    pub spectrum_axis: (f64, f64),
    pub iq: bool,
    /// Seconds between waterfall rows.
    pub row_s: f64,
    pub receiving: Option<Receiving>,
    /// A payload is being decoded.
    pub decoding: bool,
    /// The payload points of the last symbols, oldest first — about a dozen for every
    /// point of the modulation (see [`PointHistory`]) — the ideal points of the
    /// modulation (none for the large QAMs), its name, and how many symbols the points
    /// span. Shared, so a snapshot copies the pointer.
    pub constellation: Arc<Vec<[f32; 2]>>,
    pub ideal: Vec<[f32; 2]>,
    pub constellation_label: String,
    pub constellation_symbols: usize,
    /// Signal-to-noise ratio of the last payload symbol, dB.
    pub snr_db: Option<f32>,
    /// The modem decoder runs (the input is at 44.1 or 48 kHz).
    pub modem: bool,
    pub running: bool,
}

/// Handle of a running receiver. Dropping it stops the worker.
pub struct Receiver {
    commands: Sender<()>,
    events: Rx<RxEvent>,
    rows: Rx<Vec<f32>>,
    shared: Arc<Mutex<(u64, RxSnapshot)>>,
    seen: u64,
    worker: Option<JoinHandle<()>>,
}

/// Waterfall rows kept for a GUI that does not poll (about 20 s).
const ROW_QUEUE: usize = 256;
/// How often the worker publishes a snapshot.
const PUBLISH: Duration = Duration::from_millis(33);

impl Receiver {
    /// Start receiving. Errors (e.g. an unknown device) arrive as [`RxEvent::Error`].
    pub fn start(cfg: RxConfig) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let (row_tx, row_rx) = mpsc::sync_channel(ROW_QUEUE);
        let shared = Arc::new(Mutex::new((0, RxSnapshot::default())));
        let s = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("cofdmtv-rx".into())
            .spawn(move || {
                run(&cfg, &cmd_rx, &ev_tx, &row_tx, &s);
                let _ = ev_tx.send(RxEvent::Stopped);
            })
            .expect("spawning the receiver thread");
        Self { commands: cmd_tx, events: ev_rx, rows: row_rx, shared, seen: 0, worker: Some(worker) }
    }

    /// Ask the worker to stop (it reports [`RxEvent::Stopped`]).
    pub fn stop(&self) {
        let _ = self.commands.send(());
    }

    /// Events since the last call.
    pub fn poll_events(&self) -> Vec<RxEvent> {
        self.events.try_iter().collect()
    }

    /// Waterfall rows since the last call (dB, on the spectrum's axis).
    pub fn take_rows(&self) -> Vec<Vec<f32>> {
        self.rows.try_iter().collect()
    }

    /// A copy of the latest snapshot, if it is newer than the last one taken.
    pub fn snapshot_if_newer(&mut self) -> Option<RxSnapshot> {
        let guard = self.shared.lock().ok()?;
        if guard.0 == self.seen {
            return None;
        }
        self.seen = guard.0;
        Some(guard.1.clone())
    }

    /// The worker has ended.
    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Wait for the worker to end (after [`Self::stop`] or the end of a recording).
    pub fn join(&mut self) {
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop();
        self.join();
    }
}

/// Time stamp of a log line: the position in a recording, else the time of day.
fn stamp(source: &Source) -> String {
    if source.info().is_file { format!("{:7.2}s", source.position_s()) } else { Local::now().format("%H:%M:%S").to_string() }
}

/// Payloads for the decoding thread.
enum Job {
    Cofdmtv(Codeword),
    Modem(ModemCodeword),
}

/// Bandwidth of a COFDMTV mode's payload, Hz.
pub fn cofdmtv_bandwidth(mode: Mode) -> f32 {
    mode.carriers() as f32 * 6.25
}

fn cofdmtv_label(mode: Mode) -> String {
    match mode {
        Mode::Text(n) => format!("text mode {n}"),
        m => format!("mode {}", m.label()),
    }
}

fn run(cfg: &RxConfig, commands: &Rx<()>, events: &Sender<RxEvent>, rows: &SyncSender<Vec<f32>>, shared: &Arc<Mutex<(u64, RxSnapshot)>>) {
    let mut source = match Source::open(&cfg.input, cfg.channel) {
        Ok(s) => s,
        Err(e) => {
            let _ = events.send(RxEvent::Error(e));
            return;
        }
    };
    let info = source.info();
    let rate = info.processing_rate;
    let iq = cfg.channel.is_iq();
    let mut dec = Decoder::new(rate).expect("processing rates are COFDMTV rates");
    let mut modem = ModemDecoder::new(rate);
    let mut spectrum = SpectrumAnalyzer::for_rate(rate, iq);
    let resampled = if info.rate == rate { String::new() } else { format!(", resampled to {rate} Hz") };
    let _ = events.send(RxEvent::Log(format!(
        "receiving from {} ({} Hz, {} ch{resampled}, {}); COFDMTV{}",
        info.name,
        info.rate,
        info.channels,
        cfg.channel.label(),
        if modem.is_some() { " and the modem" } else { " (the modem needs 44.1 or 48 kHz)" }
    )));

    // The decoding thread.
    let decoding = Arc::new(AtomicBool::new(false));
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let decoder_thread = {
        let events = events.clone();
        let decoding = Arc::clone(&decoding);
        let save_dir = cfg.save_dir.clone();
        std::thread::Builder::new()
            .name("cofdmtv-decode".into())
            .spawn(move || decode_worker(&job_rx, &events, &decoding, save_dir))
            .expect("spawning the decoding thread")
    };

    let started = Instant::now();
    let mut last_publish = Instant::now() - PUBLISH;
    let channels = source.channels();
    let mut level = LevelMeter::new(rate);
    let mut ended = false;
    // Every payload symbol's points go into the constellation's history, from whichever
    // decoder demodulated it; its SNR is the one shown.
    let mut history = PointHistory::default();
    let (mut cofdmtv_seq, mut modem_seq) = (dec.points_seq(), modem.as_ref().map_or(0, ModemDecoder::points_seq));
    let mut last_points = SignalKind::Cofdmtv;
    loop {
        match commands.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        let block = match source.read(20) {
            Ok(Some(b)) => b,
            Ok(None) if !ended => {
                // The synchronisers look a few symbols into the past: let a transmission
                // right at the end of the recording through with a second of silence.
                ended = true;
                vec![0.0; channels * rate as usize]
            }
            Ok(None) => {
                let _ = events.send(RxEvent::Log(format!("{} end of the recording", stamp(&source))));
                let _ = events.send(RxEvent::EndOfInput);
                break;
            }
            Err(e) => {
                let _ = events.send(RxEvent::Error(e));
                break;
            }
        };
        let mut found = Vec::new();
        let mut modem_found = Vec::new();
        for frame in block.chunks_exact(channels) {
            let z = cfg.channel.pick(frame);
            level.push(if iq { z.norm() } else { z.re });
            spectrum.push(z);
            let ready = if iq { dec.push(z) } else { dec.push_real(z.re) };
            if ready {
                if let Some(ev) = dec.process() {
                    found.push(ev);
                }
                if dec.points_seq() != cofdmtv_seq {
                    cofdmtv_seq = dec.points_seq();
                    let (points, psk) = dec.constellation();
                    let ideal = || psk.points().into_iter().map(|c| [c.re, c.im]).collect();
                    // Erased carriers (no reference) are 0: not points.
                    let shown = points.iter().filter(|c| c.re != 0.0 || c.im != 0.0).map(|c| [c.re, c.im]);
                    history.push(psk_name(psk), psk.points().len(), ideal, shown);
                    last_points = SignalKind::Cofdmtv;
                }
            }
            if let Some(m) = &mut modem {
                let ev = if iq { m.push(z) } else { m.push_real(z.re) };
                if let Some(ev) = ev {
                    modem_found.push(ev);
                }
                if m.points_seq() != modem_seq {
                    modem_seq = m.points_seq();
                    if let Some(md) = m.last_modulation() {
                        history.push(md.name(), 1 << md.bits(), || ideal_points(md), m.points.iter().map(|c| [c.re, c.im]));
                        last_points = SignalKind::Modem;
                    }
                }
            }
        }
        for ev in found {
            handle(ev, &mut dec, &source, events, &job_tx);
        }
        if let Some(m) = &mut modem {
            for ev in modem_found {
                handle_modem(ev, m, &source, events, &job_tx);
            }
        }
        let (new_rows, _) = spectrum.take_rows();
        for row in new_rows {
            // A full queue means nobody is looking: drop the row.
            let _ = rows.try_send(row);
        }
        if last_publish.elapsed() >= PUBLISH {
            last_publish = Instant::now();
            let receiving = dec
                .progress()
                .map(|(mode, symbol, symbols)| {
                    let (call, cfo_hz) = dec.current().map(|(c, f)| (c.to_string(), f)).unwrap_or_default();
                    Receiving { kind: SignalKind::Cofdmtv, label: cofdmtv_label(mode), call, cfo_hz, bandwidth_hz: cofdmtv_bandwidth(mode), symbol, symbols }
                })
                .or_else(|| {
                    let m = modem.as_ref()?;
                    let (mode, symbol, symbols) = m.progress()?;
                    let (call, cfo_hz) = m.current().map(|(c, f)| (c.to_string(), f)).unwrap_or_default();
                    Some(Receiving {
                        kind: SignalKind::Modem,
                        label: format!("modem {}", mode.label()),
                        call,
                        cfo_hz,
                        bandwidth_hz: modem::BANDWIDTH_HZ as f32,
                        symbol,
                        symbols,
                    })
                });
            let snr_db = match (last_points, &modem) {
                (SignalKind::Modem, Some(m)) => m.last_snr_db(),
                _ => dec.last_snr_db(),
            };
            let snap = RxSnapshot {
                source: info.clone(),
                position_s: if info.is_file { source.position_s() } else { started.elapsed().as_secs_f64() },
                level: level.value(),
                spectrum: spectrum.average().to_vec(),
                spectrum_axis: spectrum.axis(),
                iq,
                row_s: spectrum.row_seconds(),
                receiving,
                decoding: decoding.load(Ordering::Relaxed),
                constellation: history.points(),
                ideal: history.ideal.clone(),
                constellation_label: history.label.clone(),
                constellation_symbols: history.symbols(),
                snr_db,
                modem: modem.is_some(),
                running: true,
            };
            if let Ok(mut g) = shared.lock() {
                g.0 += 1;
                g.1 = snap;
            }
        }
    }
    // Let the decoding thread finish what it has.
    drop(job_tx);
    let _ = decoder_thread.join();
    if let Ok(mut g) = shared.lock() {
        g.0 += 1;
        g.1.running = false;
        g.1.receiving = None;
        g.1.decoding = false;
        if info.is_file {
            g.1.position_s = source.position_s();
        }
        // A recording read faster than real time ends between two snapshots: the last
        // symbols are not in the last one.
        g.1.constellation = history.points();
        g.1.ideal = history.ideal.clone();
        g.1.constellation_label = history.label.clone();
        g.1.constellation_symbols = history.symbols();
        g.1.snr_db = match (last_points, &modem) {
            (SignalKind::Modem, Some(m)) => m.last_snr_db(),
            _ => dec.last_snr_db(),
        };
    }
}

/// Points the constellation display keeps per point of the modulation.
const POINTS_PER_STATE: usize = 12;
/// Points it keeps at least.
const MIN_POINTS: usize = 512;

/// The constellation display's points: the last symbols', newest last, as BinModem's
/// symbol scope keeps them — a dozen for every point of the modulation, at least 512, so
/// that a large constellation shows clusters rather than a speckled square (one QAM4096
/// symbol lands 256 points on 4096 places; this keeps the last 192 symbols). A new
/// modulation starts afresh.
#[derive(Default)]
struct PointHistory {
    label: String,
    ideal: Vec<[f32; 2]>,
    depth: usize,
    points: VecDeque<[f32; 2]>,
    /// Points of the last symbol (to count the symbols kept).
    per_symbol: usize,
    /// What the snapshots share, made again when points came in since.
    shared: Arc<Vec<[f32; 2]>>,
    changed: bool,
}

impl PointHistory {
    /// Points kept for a modulation of `states` points.
    fn depth(states: usize) -> usize {
        (POINTS_PER_STATE * states).max(MIN_POINTS)
    }

    /// Add a symbol's `points` in a modulation `label` of `states` points.
    fn push(&mut self, label: &str, states: usize, ideal: impl FnOnce() -> Vec<[f32; 2]>, points: impl Iterator<Item = [f32; 2]>) {
        if self.label != label {
            self.label = label.to_string();
            self.depth = Self::depth(states);
            self.ideal = if states <= 64 { ideal() } else { Vec::new() };
            self.points.clear();
        }
        let before = self.points.len();
        self.points.extend(points);
        self.per_symbol = self.points.len() - before;
        let excess = self.points.len().saturating_sub(self.depth);
        self.points.drain(..excess);
        self.changed = true;
    }

    /// Symbols the points span.
    fn symbols(&self) -> usize {
        self.points.len().div_ceil(self.per_symbol.max(1))
    }

    /// The points for a snapshot.
    fn points(&mut self) -> Arc<Vec<[f32; 2]>> {
        if self.changed {
            self.shared = Arc::new(self.points.iter().copied().collect());
            self.changed = false;
        }
        Arc::clone(&self.shared)
    }
}

fn psk_name(psk: Psk) -> &'static str {
    match psk {
        Psk::Bpsk => "BPSK",
        Psk::Qpsk => "QPSK",
        Psk::Psk8 => "8PSK",
    }
}

/// The ideal points of a modem modulation for the plot (none beyond 64 points).
fn ideal_points(m: Modulation) -> Vec<[f32; 2]> {
    let bits = m.bits();
    if bits > 6 {
        return Vec::new();
    }
    (0..1u32 << bits)
        .map(|v| {
            let b: Vec<f32> = (0..bits).map(|i| if (v >> i) & 1 != 0 { -1.0 } else { 1.0 }).collect();
            let c = match bits {
                1 => Psk::Bpsk.map(&b),
                2 => Psk::Qpsk.map(&b),
                3 => Psk::Psk8.map(&b),
                n => Qam::new(n).map(&b),
            };
            [c.re, c.im]
        })
        .collect()
}

fn handle(ev: Event, dec: &mut Decoder, source: &Source, events: &Sender<RxEvent>, jobs: &Sender<Job>) {
    let t = stamp(source);
    let log = |s: String| {
        let _ = events.send(RxEvent::Log(format!("{t} {s}")));
    };
    match ev {
        Event::PreambleFailed => {
            log("sync found, preamble not decodable".into());
            let _ = events.send(RxEvent::PreambleFailed);
        }
        Event::Unsupported { mode, call, cfo_hz } => {
            log(format!("{call:>9}  mode {mode} not supported ({cfo_hz:.1} Hz)"));
        }
        Event::Ping { call, cfo_hz } => {
            log(format!("{call:>9}  ping at {cfo_hz:.1} Hz"));
            let _ = events.send(RxEvent::Ping { call, cfo_hz });
        }
        Event::Sync { mode, call, cfo_hz } => {
            log(format!("{call:>9}  {} at {cfo_hz:.1} Hz, {} symbols", mode.label(), mode.symbols()));
            let _ = events.send(RxEvent::Sync { kind: SignalKind::Cofdmtv, label: cofdmtv_label(mode), call, cfo_hz, bandwidth_hz: cofdmtv_bandwidth(mode) });
        }
        Event::Done => {
            if let Some(cw) = dec.take_codeword() {
                log(format!("{:>9}  {} symbols in, SNR {:.1} dB, decoding", cw.call, cw.mode.symbols(), cw.snr_db));
                let _ = events.send(RxEvent::Decoding { label: cofdmtv_label(cw.mode), call: cw.call.clone() });
                let _ = jobs.send(Job::Cofdmtv(cw));
            }
        }
    }
}

fn handle_modem(ev: ModemEvent, dec: &mut ModemDecoder, source: &Source, events: &Sender<RxEvent>, jobs: &Sender<Job>) {
    let t = stamp(source);
    let log = |s: String| {
        let _ = events.send(RxEvent::Log(format!("{t} {s}")));
    };
    match ev {
        ModemEvent::MetaFailed => log("modem sync found, meta data not decodable".into()),
        ModemEvent::Lost => log("modem frame lost (pilots damaged)".into()),
        ModemEvent::Sync { mode, call, cfo_hz } => {
            log(format!("{call:>9}  modem {} at {cfo_hz:.1} Hz, {} bytes", mode.label(), mode.data_bytes()));
            let _ = events.send(RxEvent::Sync {
                kind: SignalKind::Modem,
                label: format!("modem {}", mode.label()),
                call,
                cfo_hz,
                bandwidth_hz: modem::BANDWIDTH_HZ as f32,
            });
        }
        ModemEvent::Done => {
            if let Some(cw) = dec.take_codeword() {
                log(format!("{:>9}  {} symbols in, Es/N0 {:.1} dB, decoding", cw.call, cw.mode.symbols(), cw.snr_db.1));
                let _ = events.send(RxEvent::Decoding { label: format!("modem {}", cw.mode.label()), call: cw.call.clone() });
                let _ = jobs.send(Job::Modem(cw));
            }
        }
    }
}

fn decode_worker(jobs: &Rx<Job>, events: &Sender<RxEvent>, decoding: &AtomicBool, save_dir: Option<PathBuf>) {
    let mut polar = PolarDecoders::default();
    let mut modem_polar = None;
    let mut frames = Reassembler::default();
    let mut modem_frames = Reassembler::new(128, multiframe::MAX_BLOCKS_ANY);
    let log = |s: String| {
        let _ = events.send(RxEvent::Log(s));
    };
    for job in jobs {
        decoding.store(true, Ordering::Relaxed);
        let t0 = Instant::now();
        match job {
            Job::Cofdmtv(cw) => {
                let result = decode_codeword(&cw, &mut polar);
                let ms = t0.elapsed().as_millis();
                let Some(p) = result else {
                    log(format!("{:>9}  decoding failed ({:.1} dB)", cw.call, cw.snr_db));
                    let _ = events.send(RxEvent::DecodeFailed { label: cofdmtv_label(cw.mode), call: cw.call, snr_db: cw.snr_db });
                    decoding.store(false, Ordering::Relaxed);
                    continue;
                };
                let label = cofdmtv_label(p.mode);
                if let Mode::Text(_) = p.mode {
                    let msg = TextMessage {
                        time: Local::now(),
                        call: p.call.clone(),
                        label,
                        text: String::from_utf8_lossy(&p.data).into_owned(),
                        flips: Some(p.flips),
                        snr_db: p.snr_db,
                        cfo_hz: p.cfo_hz,
                    };
                    log(format!("{:>9}  text, {} bytes, {} bits corrected ({ms} ms): {}", p.call, p.data.len(), p.flips, msg.text));
                    emit_text(events, &save_dir, msg);
                } else {
                    let arrived = Arrived { call: p.call, label, picture_label: None, flips: Some(p.flips), snr_db: p.snr_db, cfo_hz: p.cfo_hz, ms };
                    match frames.push(&p.data) {
                        Progress::NotMultiFrame => {
                            let (_, file) = payload::picture_bytes(&p.data);
                            emit_file(events, &save_dir, &arrived, file, 1);
                        }
                        progress => multi(events, &save_dir, &arrived, progress, &frames),
                    }
                }
            }
            Job::Modem(cw) => {
                let result = decode_datagram(&cw, &mut modem_polar);
                let ms = t0.elapsed().as_millis();
                let label = format!("modem {}", cw.mode.label());
                let Some(d) = result else {
                    log(format!("{:>9}  modem decoding failed (Es/N0 {:.1} dB)", cw.call, cw.snr_db.1));
                    let _ = events.send(RxEvent::DecodeFailed { label, call: cw.call, snr_db: cw.snr_db.1 });
                    decoding.store(false, Ordering::Relaxed);
                    continue;
                };
                let picture_label = Some(format!("v2 {}", cw.mode.label()));
                let arrived = Arrived { call: d.call.clone(), label, picture_label, flips: None, snr_db: d.snr_db.1, cfo_hz: d.cfo_hz, ms };
                if modem_frames.chunk() != d.data.len() {
                    modem_frames = Reassembler::new(d.data.len(), multiframe::MAX_BLOCKS_ANY);
                }
                match modem_frames.push(&d.data) {
                    Progress::NotMultiFrame => {
                        // A datagram: text if it reads as such, else a file.
                        let end = d.data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
                        let text = std::str::from_utf8(&d.data[..end]).ok().filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control() || c.is_whitespace()));
                        match text {
                            _ if payload::ImageKind::sniff(&d.data).is_some() => {
                                let (_, file) = payload::picture_bytes(&d.data);
                                emit_file(events, &save_dir, &arrived, file, 1);
                            }
                            Some(text) => {
                                log(format!("{:>9}  modem text, {end} bytes ({ms} ms): {text}", d.call));
                                let msg = TextMessage {
                                    time: Local::now(),
                                    call: d.call.clone(),
                                    label: arrived.label.clone(),
                                    text: text.to_string(),
                                    flips: None,
                                    snr_db: d.snr_db.1,
                                    cfo_hz: d.cfo_hz,
                                };
                                emit_text(events, &save_dir, msg);
                            }
                            None => emit_file(events, &save_dir, &arrived, d.data, 1),
                        }
                    }
                    progress => multi(events, &save_dir, &arrived, progress, &modem_frames),
                }
            }
        }
        decoding.store(false, Ordering::Relaxed);
    }
}

/// What every decoded payload has.
struct Arrived {
    call: String,
    label: String,
    /// How a picture that came this way is labelled (pictures over the modem: v2).
    picture_label: Option<String>,
    flips: Option<u32>,
    snr_db: f32,
    cfo_hz: f32,
    ms: u128,
}

fn emit_text(events: &Sender<RxEvent>, save_dir: &Option<PathBuf>, msg: TextMessage) {
    if let Some(dir) = save_dir
        && let Err(e) = payload::log_message(dir, &msg)
    {
        let _ = events.send(RxEvent::Log(format!("cannot save the message: {e}")));
    }
    let _ = events.send(RxEvent::Text(msg));
}

/// A frame of a multi-frame picture or file.
fn multi(events: &Sender<RxEvent>, save_dir: &Option<PathBuf>, a: &Arrived, progress: Progress, frames: &Reassembler) {
    let log = |s: String| {
        let _ = events.send(RxEvent::Log(s));
    };
    match progress {
        Progress::Partial { have, need } => {
            let size = frames.state().map_or(0, |(h, _)| h.size);
            log(format!("{:>9}  frame {have} of {need} of a {size}-byte file", a.call));
            let _ = events.send(RxEvent::MultiFrame { call: a.call.clone(), have, need, size });
        }
        Progress::Complete(file) => {
            let n = frames.state().map_or(1, |(h, _)| h.blocks);
            emit_file(events, save_dir, a, file, n);
        }
        Progress::Duplicate => log(format!("{:>9}  a frame received before", a.call)),
        Progress::Redundant => log(format!("{:>9}  a further frame of a file already complete", a.call)),
        Progress::Corrupted => log(format!("{:>9}  the frames do not make a file (CRC failed)", a.call)),
        _ => log(format!("{:>9}  a multi-frame header COFDMtv cannot use", a.call)),
    }
}

/// A complete file: a picture if it is one, else a file; saved, logged, announced.
fn emit_file(events: &Sender<RxEvent>, save_dir: &Option<PathBuf>, a: &Arrived, data: Vec<u8>, frames: usize) {
    let now = Local::now();
    let kind = payload::ImageKind::sniff(&data);
    let mut saved = None;
    if let Some(dir) = save_dir {
        match payload::save_unique(dir, &payload::file_name(&now, &a.call, kind), &data) {
            Ok(path) => saved = Some(path),
            Err(e) => {
                let _ = events.send(RxEvent::Log(format!("cannot save: {e}")));
            }
        }
    }
    let what = kind.map_or("data", payload::ImageKind::name);
    let _ = events.send(RxEvent::Log(format!(
        "{:>9}  {what}, {} bytes{}{} ({} ms){}",
        a.call,
        data.len(),
        if frames > 1 { format!(" in {frames} frames") } else { String::new() },
        a.flips.map_or(String::new(), |f| format!(", {f} bits corrected")),
        a.ms,
        saved.as_ref().map_or(String::new(), |s| format!(" -> {}", s.display()))
    )));
    if kind.is_some() {
        let _ = events.send(RxEvent::Picture(Picture {
            time: now,
            call: a.call.clone(),
            label: a.picture_label.clone().unwrap_or_else(|| a.label.clone()),
            frames,
            kind,
            data,
            flips: a.flips,
            snr_db: a.snr_db,
            cfo_hz: a.cfo_hz,
            saved,
        }));
    } else {
        let _ = events.send(RxEvent::File(ReceivedFile {
            time: now,
            call: a.call.clone(),
            label: a.label.clone(),
            frames,
            data,
            snr_db: a.snr_db,
            cfo_hz: a.cfo_hz,
            saved,
        }));
    }
}

/// RMS and peak of the last 100 ms.
struct LevelMeter {
    len: usize,
    count: usize,
    sum: f32,
    peak: f32,
    value: Option<(f32, f32)>,
}

impl LevelMeter {
    fn new(rate: u32) -> Self {
        Self { len: (rate / 10) as usize, count: 0, sum: 0.0, peak: 0.0, value: None }
    }

    #[inline]
    fn push(&mut self, x: f32) {
        self.sum += x * x;
        self.peak = self.peak.max(x.abs());
        self.count += 1;
        if self.count == self.len {
            let db = |p: f32| (10.0 * p.max(1e-12).log10()).max(-120.0);
            self.value = Some((db(self.sum / self.len as f32), db(self.peak * self.peak)));
            self.count = 0;
            self.sum = 0.0;
            self.peak = 0.0;
        }
    }

    fn value(&self) -> Option<(f32, f32)> {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(n: usize, at: f32) -> impl Iterator<Item = [f32; 2]> {
        (0..n).map(move |i| [at, i as f32])
    }

    #[test]
    fn a_dozen_per_point_and_512_at_least() {
        assert_eq!(PointHistory::depth(8), 512);
        assert_eq!(PointHistory::depth(64), 768);
        assert_eq!(PointHistory::depth(4096), 49_152);
    }

    #[test]
    fn the_last_symbols_are_kept_and_a_new_modulation_starts_afresh() {
        let mut h = PointHistory::default();
        h.push("QAM4096", 4096, Vec::new, symbol(256, 1.0));
        let first = h.points();
        assert_eq!((first.len(), h.symbols()), (256, 1));
        assert!(Arc::ptr_eq(&first, &h.points()), "no new points, no new copy");
        for _ in 0..199 {
            h.push("QAM4096", 4096, Vec::new, symbol(256, 2.0));
        }
        let kept = h.points();
        assert_eq!((kept.len(), h.symbols()), (49_152, 192));
        assert!(kept.iter().all(|p| p[0] == 2.0), "the oldest symbols went first");
        assert!(h.ideal.is_empty());
        h.push("QPSK", 4, || vec![[0.7, 0.7]; 4], symbol(128, 3.0));
        assert_eq!((h.points().len(), h.symbols(), h.ideal.len()), (128, 1, 4));
    }
}
