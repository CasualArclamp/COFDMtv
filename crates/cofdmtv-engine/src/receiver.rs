//! The receiver: a worker thread reads the source, runs the COFDMTV decoder and the
//! spectrum analyser over every sample, and publishes what the displays need; a second
//! thread decodes completed payloads (list decoding of up to 65536 bits takes a while),
//! rebuilds multi-frame pictures and saves what arrives, so the worker listens on.
//!
//! # How the GUI talks to it
//!
//! Like DecDRM's engine: the worker owns all receiver state and publishes a [`RxSnapshot`]
//! (a complete copy of the displays' data) about 30 times a second into an
//! `Arc<Mutex<…>>`; [`Receiver::snapshot_if_newer`] hands out a clone of each new one, so
//! the GUI never holds a lock while drawing. Discrete happenings (a transmission found, a
//! picture or message received, log lines) come as [`RxEvent`]s through a channel, where
//! none are lost between two polls; waterfall rows through a bounded one of their own.

use crate::input::{ChannelSel, InputSpec, Source, SourceInfo};
use crate::payload::{self, Picture, TextMessage};
use crate::spectrum::SpectrumAnalyzer;
use chrono::Local;
use cofdmtv_core::coding::psk::Psk;
use cofdmtv_core::cofdmtv::multiframe::{Progress, Reassembler};
use cofdmtv_core::cofdmtv::{Codeword, Decoder, Event, Mode, PolarDecoders, decode_codeword};
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
    /// Save received pictures (and a log of the messages) here.
    pub save_dir: Option<PathBuf>,
}

/// Things that happened, in order.
#[derive(Debug, Clone)]
pub enum RxEvent {
    /// A line for the log.
    Log(String),
    /// A transmission starts.
    Sync { mode: Mode, call: String, cfo_hz: f32 },
    /// A ping.
    Ping { call: String, cfo_hz: f32 },
    /// A sync symbol whose preamble could not be decoded.
    PreambleFailed,
    /// A payload is complete and being decoded.
    Decoding { mode: Mode, call: String },
    /// A payload could not be decoded.
    DecodeFailed { mode: Mode, call: String, snr_db: f32 },
    Picture(Picture),
    Text(TextMessage),
    /// A frame of a multi-frame picture arrived.
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
    pub mode: Mode,
    pub call: String,
    pub cfo_hz: f32,
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
    /// The last payload symbol's points and its modulation.
    pub constellation: Vec<[f32; 2]>,
    pub psk: Option<Psk>,
    /// Signal-to-noise ratio of the last payload symbol, dB.
    pub snr_db: Option<f32>,
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
    let mut spectrum = SpectrumAnalyzer::for_rate(rate, iq);
    let resampled = if info.rate == rate { String::new() } else { format!(", resampled to {rate} Hz") };
    let _ = events.send(RxEvent::Log(format!(
        "receiving from {} ({} Hz, {} ch{resampled}, {})",
        info.name,
        info.rate,
        info.channels,
        cfg.channel.label()
    )));

    // The decoding thread.
    let decoding = Arc::new(AtomicBool::new(false));
    let (cw_tx, cw_rx) = mpsc::channel::<Codeword>();
    let decoder_thread = {
        let events = events.clone();
        let decoding = Arc::clone(&decoding);
        let save_dir = cfg.save_dir.clone();
        std::thread::Builder::new()
            .name("cofdmtv-decode".into())
            .spawn(move || decode_worker(&cw_rx, &events, &decoding, save_dir))
            .expect("spawning the decoding thread")
    };

    let started = Instant::now();
    let mut last_publish = Instant::now() - PUBLISH;
    let channels = source.channels();
    let mut level = LevelMeter::new(rate);
    let mut ended = false;
    loop {
        match commands.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        let block = match source.read(20) {
            Ok(Some(b)) => b,
            Ok(None) if !ended => {
                // The synchroniser looks about two symbols into the past: let a
                // transmission right at the end of the recording through with silence.
                ended = true;
                vec![0.0; channels * 4 * dec.layout().extended_len]
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
        for frame in block.chunks_exact(channels) {
            let z = cfg.channel.pick(frame);
            level.push(if iq { z.norm() } else { z.re });
            spectrum.push(z);
            let ready = if iq { dec.push(z) } else { dec.push_real(z.re) };
            if ready && let Some(ev) = dec.process() {
                found.push(ev);
            }
        }
        for ev in found {
            handle(ev, &mut dec, &source, events, &cw_tx);
        }
        let (new_rows, _) = spectrum.take_rows();
        for row in new_rows {
            // A full queue means nobody is looking: drop the row.
            let _ = rows.try_send(row);
        }
        if last_publish.elapsed() >= PUBLISH {
            last_publish = Instant::now();
            let (points, psk) = dec.constellation();
            let snap = RxSnapshot {
                source: info.clone(),
                position_s: if info.is_file { source.position_s() } else { started.elapsed().as_secs_f64() },
                level: level.value(),
                spectrum: spectrum.average().to_vec(),
                spectrum_axis: spectrum.axis(),
                iq,
                row_s: spectrum.row_seconds(),
                receiving: dec.progress().map(|(mode, symbol, symbols)| {
                    let (call, cfo_hz) = dec.current().map(|(c, f)| (c.to_string(), f)).unwrap_or_default();
                    Receiving { mode, call, cfo_hz, symbol, symbols }
                }),
                decoding: decoding.load(Ordering::Relaxed),
                constellation: points.iter().filter(|c| c.re != 0.0 || c.im != 0.0).map(|c| [c.re, c.im]).collect(),
                psk: (!points.is_empty()).then_some(psk),
                snr_db: dec.last_snr_db(),
                running: true,
            };
            if let Ok(mut g) = shared.lock() {
                g.0 += 1;
                g.1 = snap;
            }
        }
    }
    // Let the decoding thread finish what it has.
    drop(cw_tx);
    let _ = decoder_thread.join();
    if let Ok(mut g) = shared.lock() {
        g.0 += 1;
        g.1.running = false;
        g.1.receiving = None;
        g.1.decoding = false;
    }
}

fn handle(ev: Event, dec: &mut Decoder, source: &Source, events: &Sender<RxEvent>, codewords: &Sender<Codeword>) {
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
            let _ = events.send(RxEvent::Sync { mode, call, cfo_hz });
        }
        Event::Done => {
            if let Some(cw) = dec.take_codeword() {
                log(format!("{:>9}  {} symbols in, SNR {:.1} dB, decoding", cw.call, cw.mode.symbols(), cw.snr_db));
                let _ = events.send(RxEvent::Decoding { mode: cw.mode, call: cw.call.clone() });
                let _ = codewords.send(cw);
            }
        }
    }
}

fn decode_worker(codewords: &Rx<Codeword>, events: &Sender<RxEvent>, decoding: &AtomicBool, save_dir: Option<PathBuf>) {
    let mut polar = PolarDecoders::default();
    let mut frames = Reassembler::default();
    let log = |s: String| {
        let _ = events.send(RxEvent::Log(s));
    };
    for cw in codewords {
        decoding.store(true, Ordering::Relaxed);
        let t0 = Instant::now();
        let result = decode_codeword(&cw, &mut polar);
        let ms = t0.elapsed().as_millis();
        let now = Local::now();
        let Some(p) = result else {
            log(format!("{:>9}  decoding failed ({:.1} dB)", cw.call, cw.snr_db));
            let _ = events.send(RxEvent::DecodeFailed { mode: cw.mode, call: cw.call, snr_db: cw.snr_db });
            decoding.store(false, Ordering::Relaxed);
            continue;
        };
        match p.mode {
            Mode::Text(_) => {
                let msg = TextMessage {
                    time: now,
                    call: p.call.clone(),
                    mode: p.mode.number(),
                    text: String::from_utf8_lossy(&p.data).into_owned(),
                    flips: p.flips,
                    snr_db: p.snr_db,
                    cfo_hz: p.cfo_hz,
                };
                log(format!("{:>9}  text, {} bytes, {} bits corrected ({ms} ms): {}", p.call, p.data.len(), p.flips, msg.text));
                if let Some(dir) = &save_dir
                    && let Err(e) = payload::log_message(dir, &msg)
                {
                    log(format!("cannot save the message: {e}"));
                }
                let _ = events.send(RxEvent::Text(msg));
            }
            _ => {
                let (file, frames_used) = match frames.push(&p.data) {
                    Progress::NotMultiFrame => (Some(p.data.clone()), 1),
                    Progress::Partial { have, need } => {
                        let size = frames.state().map_or(0, |(h, _)| h.size);
                        log(format!("{:>9}  frame {have} of {need} of a {size}-byte picture ({} bits corrected)", p.call, p.flips));
                        let _ = events.send(RxEvent::MultiFrame { call: p.call.clone(), have, need, size });
                        (None, 0)
                    }
                    Progress::Complete(file) => {
                        let n = cofdmtv_core::cofdmtv::multiframe::blocks_for(file.len());
                        (Some(file), n)
                    }
                    other => {
                        let what = match other {
                            Progress::Duplicate => "a frame received before",
                            Progress::Redundant => "a further frame of a picture already complete",
                            Progress::Corrupted => "the frames do not make a picture (CRC failed)",
                            _ => "a multi-frame header COFDMtv cannot use",
                        };
                        log(format!("{:>9}  {what}", p.call));
                        (None, 0)
                    }
                };
                if let Some(file) = file {
                    let (kind, data) = if frames_used > 1 {
                        (payload::ImageKind::sniff(&file), file)
                    } else {
                        payload::picture_bytes(&file)
                    };
                    let mut saved = None;
                    if let Some(dir) = &save_dir {
                        match payload::save_unique(dir, &payload::file_name(&now, &p.call, kind), &data) {
                            Ok(path) => saved = Some(path),
                            Err(e) => log(format!("cannot save the picture: {e}")),
                        }
                    }
                    let what = kind.map_or("unknown data", payload::ImageKind::name);
                    log(format!(
                        "{:>9}  {what}, {} bytes{}, {} bits corrected ({ms} ms){}",
                        p.call,
                        data.len(),
                        if frames_used > 1 { format!(" in {frames_used} frames") } else { String::new() },
                        p.flips,
                        saved.as_ref().map_or(String::new(), |s| format!(" -> {}", s.display()))
                    ));
                    let _ = events.send(RxEvent::Picture(Picture {
                        time: now,
                        call: p.call,
                        mode: p.mode.number(),
                        frames: frames_used,
                        kind,
                        data,
                        flips: p.flips,
                        snr_db: p.snr_db,
                        cfo_hz: p.cfo_hz,
                        saved,
                    }));
                }
            }
        }
        decoding.store(false, Ordering::Relaxed);
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
