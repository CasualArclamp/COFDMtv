//! The GUI's view of the receiving engine.
//!
//! The engine runs on its own threads and owns all receiver state; the GUI polls a copy
//! of its latest snapshot each frame, plus the events (transmissions, pictures, texts,
//! log lines) and waterfall rows that arrived since (see `cofdmtv_engine::receiver`). What
//! the displays derive from them — the waterfall history, the received pictures with
//! their textures, a picture still arriving, the message and file lists, the indicator
//! states — lives here.

use crate::waterfall::{Waterfall, crop};
use chrono::{DateTime, Local};
use cofdmtv_engine::payload::{ImageKind, Picture, ReceivedFile, TextMessage};
use cofdmtv_engine::{Receiver, RxConfig, RxEvent, RxSnapshot};
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::collections::VecDeque;
use std::time::Instant;

/// Log lines kept.
pub const LOG_CAPACITY: usize = 5000;
/// Pictures kept in memory (the saved files stay on disk).
pub const PICTURES_KEPT: usize = 200;

/// Bounded list of log lines.
#[derive(Debug, Clone, Default)]
pub struct LogBuffer {
    lines: VecDeque<String>,
}

impl LogBuffer {
    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push_back(line.into());
        while self.lines.len() > LOG_CAPACITY {
            self.lines.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn get(&self, i: usize) -> Option<&str> {
        self.lines.get(i).map(String::as_str)
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// All lines joined with newlines (for the clipboard).
    pub fn text(&self) -> String {
        self.lines.iter().map(String::as_str).collect::<Vec<_>>().join("\n")
    }
}

/// A received picture and its texture (made when first shown).
pub struct ShownPicture {
    pub picture: Picture,
    pub size: Option<(u32, u32)>,
    texture: Option<Result<TextureHandle, String>>,
}

impl ShownPicture {
    fn new(picture: Picture) -> Self {
        let size = cofdmtv_pix::dimensions(&picture.data);
        Self { picture, size, texture: None }
    }

    /// The texture, decoding the picture the first time.
    pub fn texture(&mut self, ctx: &egui::Context, id: usize) -> Result<&TextureHandle, &str> {
        if self.texture.is_none() {
            self.texture = Some(match cofdmtv_pix::decode(&self.picture.data) {
                Some(rgba) => {
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    let image = ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                    Ok(ctx.load_texture(format!("picture-{id}"), image, TextureOptions::LINEAR))
                }
                None => Err(match self.picture.kind {
                    Some(k) => format!("{} pictures cannot be shown here (saved as received)", k.name()),
                    None => "not a picture COFDMtv knows".to_string(),
                }),
            });
        }
        match self.texture.as_ref().expect("set above") {
            Ok(t) => Ok(t),
            Err(e) => Err(e.as_str()),
        }
    }
}

/// A line of the message list.
#[derive(Debug, Clone)]
pub enum Message {
    Text(TextMessage),
    Ping { time: DateTime<Local>, call: String, cfo_hz: f32 },
}

/// The frames of a multi-frame picture or file in hand.
#[derive(Debug, Clone)]
pub struct MultiFrame {
    pub call: String,
    /// How it comes ("v2 QAM256 1/2 normal", "modem QPSK 1/2 short", "mode 12 …").
    pub label: String,
    pub have: usize,
    pub need: usize,
    pub size: usize,
}

/// A picture arriving in order (a v2 picture's first frames carry the file itself), shown
/// as far as it has come.
pub struct Arriving {
    pub call: String,
    pub kind: Option<ImageKind>,
    /// The file's first bytes in hand.
    head: Vec<u8>,
    /// `head` came after the texture was made.
    stale: bool,
    /// The picture so far (transparent where it has not arrived), the rows decoded, and
    /// whether it sharpens once they are all in (a progressive JPEG).
    shown: Option<(TextureHandle, u32, bool)>,
}

impl Arriving {
    fn new(call: String, head: Vec<u8>) -> Self {
        Self { call, kind: ImageKind::sniff(&head), head, stale: true, shown: None }
    }

    /// More of the same picture: the texture stays until the new bytes are decoded.
    fn update(&mut self, head: Vec<u8>) {
        self.head = head;
        self.stale = true;
    }

    /// Bytes of it in hand.
    pub fn bytes(&self) -> usize {
        self.head.len()
    }

    /// The picture so far, the rows decoded and whether it sharpens further, decoding what
    /// came since the last call; `None` until its header is in.
    pub fn texture(&mut self, ctx: &egui::Context) -> Option<(&TextureHandle, u32, bool)> {
        if std::mem::take(&mut self.stale)
            && let Some(p) = cofdmtv_pix::decode_partial(&self.head)
            // Less than before (a cut no decoder takes): keep what is shown.
            && self.shown.as_ref().is_none_or(|(_, rows, _)| p.rows >= *rows)
        {
            let size = [p.image.width() as usize, p.image.height() as usize];
            let image = ColorImage::from_rgba_unmultiplied(size, p.image.as_raw());
            match &mut self.shown {
                Some((tex, rows, progressive)) if tex.size() == size => {
                    tex.set(image, TextureOptions::LINEAR);
                    (*rows, *progressive) = (p.rows, p.progressive);
                }
                _ => self.shown = Some((ctx.load_texture("arriving", image, TextureOptions::LINEAR), p.rows, p.progressive)),
            }
        }
        self.shown.as_ref().map(|(t, rows, progressive)| (t, *rows, *progressive))
    }
}

/// How the last transmission ended, for the indicators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Decoded,
    Failed,
}

/// Engine handle plus everything the GUI derives from it.
#[derive(Default)]
pub struct RxSession {
    engine: Option<Receiver>,
    stopping: bool,
    /// What is being received (file or device name), for the title and the log.
    pub label: String,
    pub snap: RxSnapshot,
    pub waterfall: Waterfall,
    pub log: LogBuffer,
    /// Newest last.
    pub pictures: Vec<ShownPicture>,
    /// The picture shown large (index into `pictures`); `None`: the newest.
    pub selected: Option<usize>,
    pub messages: Vec<Message>,
    /// Modem datagrams and files that are neither text nor pictures, newest last.
    pub files: Vec<ReceivedFile>,
    pub multiframe: Option<MultiFrame>,
    /// A picture of that, arriving in order.
    pub arriving: Option<Arriving>,
    /// The band of the transmission being received or last received (carrier and
    /// bandwidth, Hz), and when it ended.
    pub band: Option<(f32, f32)>,
    pub band_ended: Option<Instant>,
    /// A sync whose preamble failed, recently.
    pub preamble_failed: Option<Instant>,
    pub outcome: Option<(Outcome, Instant)>,
    /// Non-ASCII text received since the application last took it (fonts).
    pub text_seen: String,
    /// Pictures and messages that arrived since the application last looked (for a
    /// repaint / attention).
    pub fresh: bool,
    started: Option<Instant>,
}

impl RxSession {
    pub fn is_running(&self) -> bool {
        self.engine.is_some()
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping
    }

    pub fn start(&mut self, cfg: RxConfig, label: String) {
        self.stop_now();
        self.log.push(format!("— start: {label}"));
        self.label = label;
        self.snap = RxSnapshot::default();
        self.waterfall.clear();
        self.multiframe = None;
        self.arriving = None;
        self.band = None;
        self.outcome = None;
        self.preamble_failed = None;
        self.started = Some(Instant::now());
        self.engine = Some(Receiver::start(cfg));
    }

    /// Ask the engine to stop; it reports when it has.
    pub fn stop(&mut self) {
        if let Some(e) = &self.engine {
            e.stop();
            self.stopping = true;
        }
    }

    /// Stop and wait (at exit).
    pub fn stop_now(&mut self) {
        if let Some(mut e) = self.engine.take() {
            e.stop();
            e.join();
            for ev in e.poll_events() {
                self.handle(ev);
            }
        }
        self.stopping = false;
    }

    /// Take what the engine has published; `span` is the frequency range shown (Hz).
    pub fn poll(&mut self, span_of: impl Fn(u32, bool) -> (f64, f64)) {
        let Some(engine) = &mut self.engine else { return };
        if let Some(s) = engine.snapshot_if_newer() {
            self.snap = s;
        }
        let rows = engine.take_rows();
        let events = engine.poll_events();
        let rate = self.snap.source.processing_rate;
        if rate > 0 {
            let span = span_of(rate, self.snap.iq);
            for row in rows {
                let (part, covered) = crop(&row, self.snap.spectrum_axis, span);
                self.waterfall.push(part, covered, self.snap.row_s);
            }
        }
        for ev in events {
            self.handle(ev);
        }
    }

    fn handle(&mut self, ev: RxEvent) {
        match ev {
            RxEvent::Log(line) => self.log.push(line),
            RxEvent::Sync { cfo_hz, bandwidth_hz, .. } => {
                self.band = Some((cfo_hz, bandwidth_hz));
                self.band_ended = None;
            }
            RxEvent::Ping { call, cfo_hz } => {
                self.note(&call);
                self.messages.push(Message::Ping { time: Local::now(), call, cfo_hz });
                self.fresh = true;
            }
            RxEvent::PreambleFailed => self.preamble_failed = Some(Instant::now()),
            RxEvent::Decoding { .. } => self.band_ended = Some(Instant::now()),
            RxEvent::DecodeFailed { .. } => self.outcome = Some((Outcome::Failed, Instant::now())),
            RxEvent::Picture(p) => {
                self.outcome = Some((Outcome::Decoded, Instant::now()));
                self.note(&p.call);
                if p.frames > 1 {
                    self.multiframe = None;
                    self.arriving = None;
                }
                self.pictures.push(ShownPicture::new(p));
                if self.pictures.len() > PICTURES_KEPT {
                    self.pictures.remove(0);
                    self.selected = self.selected.and_then(|s| s.checked_sub(1));
                }
                self.selected = None;
                self.fresh = true;
            }
            RxEvent::Text(m) => {
                self.outcome = Some((Outcome::Decoded, Instant::now()));
                self.note(&m.text);
                self.note(&m.call);
                self.messages.push(Message::Text(m));
                self.fresh = true;
            }
            RxEvent::File(f) => {
                self.outcome = Some((Outcome::Decoded, Instant::now()));
                self.note(&f.call);
                if f.frames > 1 {
                    self.multiframe = None;
                    self.arriving = None;
                }
                self.files.push(f);
                if self.files.len() > PICTURES_KEPT {
                    self.files.remove(0);
                }
                self.fresh = true;
            }
            RxEvent::MultiFrame { call, label, have, need, size, head } => {
                self.outcome = Some((Outcome::Decoded, Instant::now()));
                // The same file goes on (frames in hand count up, its head grows or stays).
                let same = self.multiframe.as_ref().is_some_and(|m| m.call == call && m.size == size && m.need == need && m.have < have);
                self.arriving = match self.arriving.take() {
                    _ if head.is_empty() => None,
                    Some(mut a) if same => {
                        a.update(head);
                        Some(a)
                    }
                    _ => Some(Arriving::new(call.clone(), head)),
                };
                self.multiframe = Some(MultiFrame { call, label, have, need, size });
            }
            RxEvent::EndOfInput => {}
            RxEvent::Error(e) => self.log.push(format!("error: {e}")),
            RxEvent::Stopped => {
                if let Some(mut e) = self.engine.take() {
                    e.join();
                }
                self.stopping = false;
                self.snap.running = false;
                self.snap.receiving = None;
                self.snap.decoding = false;
                self.log.push("— stopped");
            }
        }
    }

    fn note(&mut self, text: &str) {
        if !text.is_ascii() {
            self.text_seen.push_str(text);
        }
    }
}

