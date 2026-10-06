//! Transmitter page: what to send (a picture, a text, a ping, or data over the modem), how
//! (mode, carrier, lead-in, fancy header) and where (sound card or file) on the left; the
//! transmission's state, the Transmit button, the output level and spectrum on the right
//! (DecDRM's layout).
//!
//! A picture is prepared as Shredpix does: scaled to a pixel budget and compressed with
//! the best quality that fits one frame (5380 bytes), or the blocks of a multi-frame
//! picture; a file that fits already is sent unchanged. Preparing runs on a thread of
//! its own whenever the picture or a setting changes, and the page shows the result as
//! the receiver will see it.
//!
//! Data goes over the aicodix modem: a text in one datagram, or a file in frames with
//! the multi-frame header, all in one transmission.

use super::meter::{GOOD, HOT, WARN, level_meter};
use super::plots::{SpectrumView, spectrum_plot};
use super::source::{DeviceLists, device_combo};
use super::{Palette, card, enabled, fmt_time, grid, placeholder, row_label, value};
use crate::settings::{AIR_CHOICES, DataSource, NOISE_CHOICES, PicFormat, Pixels, Settings, TxChannelChoice, TxKind, TxOutputKind};
use crate::transmitter::TxSession;
use cofdmtv_engine::cofdmtv_core::coding::{base37, base40};
use cofdmtv_engine::cofdmtv_core::cofdmtv::multiframe;
use cofdmtv_engine::cofdmtv_core::cofdmtv::{IMAGE_BYTES, Mode, RATES, SYMBOL_SECONDS, TEXT_BYTES, TxRequest};
use cofdmtv_engine::cofdmtv_core::modem::{self, CodeRate, ModemMode, ModemRequest, Modulation, transmission_seconds};
use cofdmtv_engine::receiver::cofdmtv_bandwidth;
use cofdmtv_engine::{OutputSpec, TxConfig, TxJob, v2};
use cofdmtv_pix::{PIXEL_CHOICES, Source};
use eframe::egui::{self, Color32, ColorImage, ComboBox, RichText, TextureHandle, TextureOptions, Ui};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;

/// The inputs of a prepared picture: when they change, it is prepared again.
#[derive(Debug, Clone, PartialEq)]
struct PrepKey {
    generation: u64,
    format: PicFormat,
    pixels: Pixels,
    /// Bytes the picture may have.
    budget: usize,
    as_is: bool,
}

/// A picture ready to send.
struct Prepared {
    file: Vec<u8>,
    /// What was done, for the page.
    info: String,
    preview: Option<TextureHandle>,
}

/// What the preparing thread hands back.
struct PrepResult {
    file: Vec<u8>,
    info: String,
    preview: Option<ColorImage>,
}

/// A file to send over the modem.
struct DataFile {
    name: String,
    bytes: Vec<u8>,
}

/// Largest file read for sending (the multi-frame header has 24 bits for the size).
const MAX_DATA_FILE: u64 = 1 << 24;

fn load_data(path: &Path) -> Result<DataFile, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_DATA_FILE {
        return Err(format!("{len} bytes: at most {MAX_DATA_FILE}"));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    Ok(DataFile { name, bytes })
}

/// Blocks (frames needed) and frames sent for a data file of `len` bytes in `mode`: extra
/// frames go with a file of more than one block.
fn data_frames(len: usize, mode: ModemMode, extra: u8) -> (usize, usize) {
    let blocks = multiframe::blocks_in(len, mode.data_bytes());
    (blocks, blocks + if blocks > 1 { usize::from(extra) } else { 0 })
}

/// State of the transmitter page.
#[derive(Default)]
pub struct TxPage {
    source: Option<Arc<Source>>,
    source_name: String,
    /// Bumped for every new source picture.
    generation: u64,
    source_texture: Option<TextureHandle>,
    prepared: Option<(PrepKey, Prepared)>,
    job: Option<(PrepKey, mpsc::Receiver<Result<PrepResult, String>>)>,
    /// The inputs whose preparation failed (not retried until they change).
    failed: Option<PrepKey>,
    error: Option<String>,
    notice: Option<String>,
    /// The file to send as data, and why the last one could not be opened.
    data: Option<DataFile>,
    data_notice: Option<String>,
}

impl TxPage {
    /// Load the picture in `settings.picture` (at start).
    pub fn new(settings: &mut Settings) -> Self {
        let mut page = Self::default();
        if let Some(path) = settings.picture.clone() {
            if path.is_file() {
                page.open_picture(settings, &path);
            } else {
                settings.picture = None;
            }
        }
        if let Some(path) = settings.data_file.clone() {
            match load_data(&path) {
                Ok(d) => page.data = Some(d),
                Err(_) => settings.data_file = None,
            }
        }
        page
    }

    /// Send the file at `path` as data (over the modem).
    pub fn open_data(&mut self, settings: &mut Settings, path: &Path) {
        settings.tx_kind = TxKind::Data;
        settings.data_source = DataSource::File;
        match load_data(path) {
            Ok(d) => {
                self.data = Some(d);
                self.data_notice = None;
                settings.data_file = Some(path.to_path_buf());
            }
            Err(e) => self.data_notice = Some(format!("cannot open {}: {e}", path.display())),
        }
    }

    /// Use the picture at `path`.
    pub fn open_picture(&mut self, settings: &mut Settings, path: &Path) {
        match Source::open(path) {
            Ok(src) => {
                self.set_source(src, path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()));
                settings.picture = Some(path.to_path_buf());
                settings.tx_kind = TxKind::Picture;
            }
            Err(e) => self.notice = Some(format!("cannot open {}: {e}", path.display())),
        }
    }

    fn set_source(&mut self, src: Source, name: String) {
        self.source = Some(Arc::new(src));
        self.source_name = name;
        self.generation += 1;
        self.source_texture = None;
        self.prepared = None;
        self.job = None;
        self.failed = None;
        self.error = None;
        self.notice = None;
    }

    /// Text being edited (for the fallback fonts).
    pub fn text<'a>(&self, settings: &'a Settings) -> &'a str {
        &settings.text
    }

    fn key(&self, settings: &Settings) -> PrepKey {
        PrepKey { generation: self.generation, format: settings.format, pixels: settings.pixels, budget: picture_budget(settings), as_is: settings.send_as_is }
    }

    /// A picture is being prepared (or about to be): Transmit has to wait.
    pub fn busy(&self, settings: &Settings) -> bool {
        settings.tx_kind == TxKind::Picture && self.source.is_some() && (self.job.is_some() || (self.prepared.is_none() && self.failed.is_none()))
    }

    /// Start preparing if the inputs changed; collect a finished preparation (every frame).
    pub fn update(&mut self, ctx: &egui::Context, settings: &Settings) {
        let key = self.key(settings);
        if let Some((k, rx)) = &self.job {
            match rx.try_recv() {
                Ok(result) => {
                    let k = k.clone();
                    self.job = None;
                    match result {
                        Ok(r) => {
                            let preview = r.preview.map(|img| ctx.load_texture("tx-preview", img, TextureOptions::LINEAR));
                            self.prepared = Some((k, Prepared { file: r.file, info: r.info, preview }));
                            self.failed = None;
                            self.error = None;
                        }
                        Err(e) => {
                            self.prepared = None;
                            self.failed = Some(k);
                            self.error = Some(e);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(50)),
                Err(mpsc::TryRecvError::Disconnected) => self.job = None,
            }
        }
        let Some(src) = self.source.clone() else { return };
        let done = self.prepared.as_ref().is_some_and(|(k, _)| *k == key);
        let running = self.job.as_ref().is_some_and(|(k, _)| *k == key);
        if done || running || self.failed.as_ref() == Some(&key) {
            return;
        }
        // A job for older inputs is abandoned: its thread finishes into a closed channel.
        let (tx, rx) = mpsc::channel();
        let k = key.clone();
        std::thread::spawn(move || {
            let _ = tx.send(prepare(&src, &k));
        });
        self.job = Some((key, rx));
    }
}

/// The preparation itself (on its own thread).
fn prepare(src: &Source, key: &PrepKey) -> Result<PrepResult, String> {
    let budget = key.budget;
    if key.as_is
        && let Some(o) = src.original_if_fits(budget)
    {
        let (w, h) = src.image.dimensions();
        let info = format!("sent as it is: {w}×{h}, {} bytes", o.len());
        return Ok(PrepResult { file: o.to_vec(), info, preview: preview_of(o) });
    }
    let format = key.format.format();
    let (pixels, e) = match key.pixels {
        Pixels::Auto => cofdmtv_pix::encode_auto(&src.image, format, budget, 1 << 20, 50)?,
        Pixels::Max(p) => (p, cofdmtv_pix::encode_to_fit(&cofdmtv_pix::scale(&src.image, p), format, budget)?),
    };
    let quality = e.quality.map_or(String::new(), |q| format!(", quality {q}"));
    let budget_name = PIXEL_CHOICES.iter().find(|(p, _)| *p == pixels).map_or(String::new(), |(_, n)| format!(" (≤ {n} pixels)"));
    let info = format!("{}×{} {}{quality}, {} bytes{budget_name}", e.width, e.height, format.label(), e.bytes.len());
    Ok(PrepResult { preview: preview_of(&e.bytes), file: e.bytes, info })
}

fn preview_of(bytes: &[u8]) -> Option<ColorImage> {
    let rgba = cofdmtv_pix::decode(bytes)?;
    Some(ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

/// Bytes the picture may have: one frame, the blocks of a multi-frame picture, or what a
/// v2 picture's air time holds.
fn picture_budget(settings: &Settings) -> usize {
    if settings.picture_v2 {
        v2_plan(settings).budget
    } else if settings.blocks <= 1 {
        IMAGE_BYTES
    } else {
        usize::from(settings.blocks) * multiframe::BLOCK_DATA
    }
}

/// The modem's noise symbols for the lead-in set (v2 pictures).
fn v2_lead_in(settings: &Settings) -> usize {
    v2::lead_in_symbols(v2::lead_in_seconds(settings.noise_symbols))
}

/// How a v2 picture fits the air time set.
fn v2_plan(settings: &Settings) -> v2::Plan {
    v2::plan(settings.v2_mode, f64::from(settings.v2_air_s), v2_lead_in(settings), settings.fancy_header, usize::from(settings.extra_frames))
}

/// Blocks and frames of a v2 picture of `len` bytes.
fn v2_frame_count(len: usize, settings: &Settings) -> (usize, usize) {
    let blocks = multiframe::blocks_in(len, settings.v2_mode.data_bytes());
    (blocks, blocks + v2_plan(settings).extra())
}

/// The rate the signal will be built at, for the carrier limits.
fn signal_rate(settings: &Settings) -> u32 {
    let rates: &[u32] = if settings.modem_signal() { &modem::RATES } else { &RATES };
    settings.tx_rate.filter(|r| rates.contains(r)).unwrap_or(48_000)
}

/// The COFDMTV mode of what is to be sent (`None`: over the modem, data or a v2 picture).
fn mode_of(settings: &Settings) -> Option<Mode> {
    match settings.tx_kind {
        TxKind::Picture if settings.picture_v2 => None,
        TxKind::Picture => Some(Mode::Image(settings.tx_mode)),
        TxKind::Text => Some(Mode::text_for(settings.text.len().max(1)).unwrap_or(Mode::Text(14))),
        TxKind::Ping => Some(Mode::Ping),
        TxKind::Data => None,
    }
}

/// Carrier and width of the band of what is to be sent, Hz.
fn band_of(settings: &Settings) -> (i32, f32) {
    match mode_of(settings) {
        Some(mode) => (settings.carrier_hz, cofdmtv_bandwidth(mode)),
        None => (settings.data_carrier_hz, modem::BANDWIDTH_HZ as f32),
    }
}

/// Carriers the modem may use here: multiples of 300 Hz in the band, up to 3 kHz unless
/// higher ones are allowed.
fn data_carrier_range(settings: &Settings) -> RangeInclusive<i32> {
    let iq = settings.tx_channel == TxChannelChoice::Iq;
    let full = modem::carrier_range(signal_rate(settings), iq);
    let max = if settings.ultrasonic { *full.end() } else { (*full.end()).min(3000) };
    (if iq { -max } else { *full.start() })..=max
}

/// The call sign as it will go out, or why it cannot.
fn call_sign_check(settings: &Settings) -> Result<String, String> {
    if settings.modem_signal() {
        base40::check(&settings.call_sign).map(base40::decode)
    } else {
        base37::check(&settings.call_sign).map(|()| base37::decode(base37::encode(&settings.call_sign)))
    }
}

impl TxPage {
    /// Draw the page.
    pub fn show(&mut self, ui: &mut Ui, settings: &mut Settings, devices: &mut DeviceLists, tx: &mut TxSession, allow_device: bool) {
        egui::Panel::right("tx_side").resizable(true).default_size(420.0).min_size(320.0).show(ui, |ui| self.side(ui, settings, tx, allow_device));
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("tx_form").auto_shrink([false, false]).show(ui, |ui| {
                let locked = tx.is_running();
                enabled(ui, !locked, |ui| {
                    self.station_card(ui, settings);
                    self.content_card(ui, settings);
                    signal_card(ui, settings);
                    output_card(ui, settings, devices);
                });
            });
        });
    }

    fn station_card(&mut self, ui: &mut Ui, settings: &mut Settings) {
        card(ui, "Station", "your call sign goes out with everything you send", |ui| {
            grid(ui, "station", |ui| {
                row_label(ui, "Call sign");
                ui.horizontal(|ui| {
                    let hint = if settings.modem_signal() {
                        "Letters, digits and /, up to nine (the modem's base 40: space, /, 0–9, A–Z)"
                    } else {
                        "Letters and digits, up to nine (base 37: space, 0–9, A–Z)"
                    };
                    let edit = egui::TextEdit::singleline(&mut settings.call_sign).desired_width(140.0).char_limit(base37::MAX_LEN).font(egui::TextStyle::Monospace);
                    ui.add(edit).on_hover_text(hint);
                    match call_sign_check(settings) {
                        Ok(call) => ui.label(RichText::new(call.trim().to_string()).weak()),
                        Err(e) => ui.colored_label(Palette::for_ui(ui).error, e),
                    };
                });
                ui.end_row();
            });
        });
    }

    fn content_card(&mut self, ui: &mut Ui, settings: &mut Settings) {
        card(ui, "Send", "", |ui| {
            ui.horizontal(|ui| {
                for k in TxKind::ALL {
                    ui.selectable_value(&mut settings.tx_kind, k, RichText::new(k.label()).size(14.0));
                }
            });
            ui.add_space(6.0);
            match settings.tx_kind {
                TxKind::Picture => self.picture(ui, settings),
                TxKind::Text => text(ui, settings),
                TxKind::Ping => {
                    placeholder(ui, "A ping sends just your call sign: about half a second (plus the lead-in and the fancy header). Assempix and Rattlegram show it as \"ping\".");
                }
                TxKind::Data => self.data(ui, settings),
            }
        });
    }

    /// Data over the modem: a text in one datagram, or a file.
    fn data(&mut self, ui: &mut Ui, settings: &mut Settings) {
        let pal = Palette::for_ui(ui);
        let mode = settings.data_mode;
        let size = mode.data_bytes();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut settings.data_source, DataSource::Text, "Text").on_hover_text("A message in one datagram");
            ui.selectable_value(&mut settings.data_source, DataSource::File, "File").on_hover_text("Any file, in as many frames as it takes");
        });
        ui.add_space(4.0);
        match settings.data_source {
            DataSource::Text => {
                let hint = format!("Your message (UTF-8, up to {size} bytes in one {} datagram)", mode.label());
                ui.add(egui::TextEdit::multiline(&mut settings.text).desired_rows(4).desired_width(f32::INFINITY).hint_text(hint));
                let len = settings.text.len();
                let color = if len > size { pal.error } else { ui.visuals().weak_text_color() };
                ui.label(RichText::new(format!("{len} / {size} bytes")).color(color).monospace());
            }
            DataSource::File => {
                ui.horizontal(|ui| {
                    if ui.button("Open file…").clicked() {
                        let mut dialog = rfd::FileDialog::new().set_title("Open a file to send");
                        if let Some(dir) = settings.data_file.as_deref().and_then(Path::parent).filter(|d| d.is_dir()) {
                            dialog = dialog.set_directory(dir);
                        }
                        if let Some(path) = dialog.pick_file() {
                            self.open_data(settings, &path);
                        }
                    }
                    match &self.data {
                        Some(d) => {
                            ui.add(egui::Label::new(RichText::new(&d.name).monospace()).truncate());
                        }
                        None => {
                            ui.label(RichText::new("or drop a file on the window").weak().italics());
                        }
                    }
                });
                if let Some(n) = &self.data_notice {
                    ui.colored_label(pal.error, n);
                }
                if let Some(d) = &self.data {
                    let (blocks, frames) = data_frames(d.bytes.len(), mode, settings.extra_frames);
                    ui.horizontal(|ui| {
                        ui.label(format!("{} bytes: {blocks} frame{} of {size} bytes", d.bytes.len(), if blocks == 1 { "" } else { "s" }));
                        if blocks > 1 {
                            ui.label("+");
                            ui.add(egui::DragValue::new(&mut settings.extra_frames).range(0..=64)).on_hover_text("Extra frames: lose up to this many and the file still arrives");
                            ui.label("extra");
                        }
                    });
                    let any = if frames > 1 { format!(" from any {blocks} of the {frames} frames") } else { String::new() };
                    ui.label(
                        RichText::new(format!("With a multi-frame header (size, CRC-32): COFDMtv rebuilds the file{any}. The original modem's decoder writes each frame as it is."))
                            .weak(),
                    );
                }
            }
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("The aicodix modem: 44.1 or 48 kHz, 2400 Hz wide. A text in one datagram arrives as a message, a file as a file (a picture as a picture).")
                .weak()
                .small(),
        );
    }

    fn picture(&mut self, ui: &mut Ui, settings: &mut Settings) {
        ui.horizontal(|ui| {
            if ui.button("Open picture…").clicked() {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Open a picture to send")
                    .add_filter("Pictures", &["jpg", "jpeg", "png", "webp", "bmp", "gif", "tif", "tiff"])
                    .add_filter("All files", &["*"]);
                if let Some(dir) = settings.picture.as_deref().and_then(Path::parent).filter(|d| d.is_dir()) {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(path) = dialog.pick_file() {
                    self.open_picture(settings, &path);
                }
            }
            if self.source.is_some() {
                ui.add(egui::Label::new(RichText::new(&self.source_name).monospace()).truncate());
            } else {
                ui.label(RichText::new("or drop a picture on the window").weak().italics());
            }
        });
        if let Some(n) = &self.notice {
            ui.colored_label(Palette::for_ui(ui).error, n);
        }
        let Some(src) = self.source.clone() else { return };
        ui.add_space(6.0);
        // The original and what will be sent, side by side.
        let ctx = ui.ctx().clone();
        let tex = self.source_texture.get_or_insert_with(|| {
            let img = &src.image;
            let thumb = image::imageops::thumbnail(img, 480.min(img.width()), (480 * img.height() / img.width().max(1)).max(1).min(img.height()));
            let rgba: Vec<u8> = thumb.pixels().flat_map(|p| [p.0[0], p.0[1], p.0[2], 255]).collect();
            ctx.load_texture("tx-source", ColorImage::from_rgba_unmultiplied([thumb.width() as usize, thumb.height() as usize], &rgba), TextureOptions::LINEAR)
        });
        let half = ((ui.available_width() - 20.0) / 2.0).clamp(120.0, 320.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(format!("Original {}×{}", src.image.width(), src.image.height())).weak());
                let size = tex.size_vec2();
                ui.add(egui::Image::new(&*tex).fit_to_exact_size(size * (half / size.x).min(half * 0.75 / size.y)).corner_radius(3.0));
            });
            ui.vertical(|ui| {
                ui.label(RichText::new("As it will arrive").weak());
                match (&self.prepared, &self.job) {
                    (_, Some(_)) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("compressing…");
                        });
                    }
                    (Some((_, p)), None) => match &p.preview {
                        Some(t) => {
                            let size = t.size_vec2();
                            ui.add(egui::Image::new(t).fit_to_exact_size(size * (half / size.x).min(half * 0.75 / size.y)).corner_radius(3.0));
                        }
                        None => placeholder(ui, "(no preview)"),
                    },
                    (None, None) => {
                        if let Some(e) = &self.error {
                            ui.add(egui::Label::new(RichText::new(e).color(Palette::for_ui(ui).error)).wrap());
                        }
                    }
                }
            });
        });
        ui.add_space(6.0);
        grid(ui, "picture_settings", |ui| {
            row_label(ui, "Format");
            ComboBox::from_id_salt("pic_format").selected_text(settings.format.label()).show_ui(ui, |ui| {
                for f in PicFormat::ALL {
                    ui.selectable_value(&mut settings.format, f, f.label());
                }
            });
            ui.end_row();
            row_label(ui, "Size");
            ComboBox::from_id_salt("pic_pixels").selected_text(settings.pixels.label()).show_ui(ui, |ui| {
                ui.selectable_value(&mut settings.pixels, Pixels::Auto, "Auto").on_hover_text("The largest size that keeps a decent quality");
                for (p, name) in PIXEL_CHOICES {
                    ui.selectable_value(&mut settings.pixels, Pixels::Max(p), format!("{name} pixels"));
                }
            });
            ui.end_row();
            if settings.picture_v2 {
                row_label(ui, "Air time");
                ui.horizontal(|ui| {
                    ComboBox::from_id_salt("v2_air")
                        .selected_text(format!("{} s", settings.v2_air_s))
                        .show_ui(ui, |ui| {
                            for t in AIR_CHOICES {
                                ui.selectable_value(&mut settings.v2_air_s, t, format!("{t} s"));
                            }
                        })
                        .response
                        .on_hover_text("How long the picture may take on the air: it is compressed to fill that time in the v2 mode below");
                    ui.label("+");
                    ui.add(egui::DragValue::new(&mut settings.extra_frames).range(0..=12)).on_hover_text("Extra frames: lose up to this many and the picture still arrives");
                    ui.label("extra");
                });
                ui.end_row();
                let plan = v2_plan(settings);
                let extra = if plan.extra() > 0 { format!(" + {} extra", plan.extra()) } else { String::new() };
                row_label(ui, "");
                ui.label(
                    RichText::new(format!(
                        "up to {} bytes: {} frame{} of {} bytes{extra}, {:.0} s",
                        plan.budget,
                        plan.blocks,
                        if plan.blocks == 1 { "" } else { "s" },
                        settings.v2_mode.data_bytes(),
                        plan.seconds
                    ))
                    .weak(),
                );
                ui.end_row();
            } else {
            row_label(ui, "Frames");
            ui.horizontal(|ui| {
                let label = |b: u8| if b <= 1 { "1 frame (5380 bytes)".to_string() } else { format!("{b} blocks ({} bytes)", usize::from(b) * multiframe::BLOCK_DATA) };
                ComboBox::from_id_salt("pic_blocks").selected_text(label(settings.blocks)).show_ui(ui, |ui| {
                    for b in 1..=multiframe::MAX_BLOCKS as u8 {
                        ui.selectable_value(&mut settings.blocks, b, label(b));
                    }
                })
                .response
                .on_hover_text("A bigger picture in several frames, rebuilt by Assempix (or COFDMtv) from any of them as many as there are blocks");
                if settings.blocks > 1 {
                    ui.label("+");
                    ui.add(egui::DragValue::new(&mut settings.extra_frames).range(0..=12)).on_hover_text("Extra frames: lose up to this many and the picture still arrives");
                    ui.label("extra");
                }
            });
            ui.end_row();
            }
            row_label(ui, "");
            ui.checkbox(&mut settings.send_as_is, "Send the file unchanged if it fits");
            ui.end_row();
        });
        if let Some((_, p)) = &self.prepared {
            let budget = picture_budget(settings);
            ui.add_space(4.0);
            let used = p.file.len() as f32 / budget as f32;
            let frames = if settings.picture_v2 {
                let (blocks, frames) = v2_frame_count(p.file.len(), settings);
                format!(" · {frames} frame{}, any {blocks} rebuild it", if frames == 1 { "" } else { "s" })
            } else if settings.blocks > 1 {
                let n = multiframe::blocks_for(p.file.len());
                format!(" · {} frames", n + usize::from(settings.extra_frames))
            } else {
                String::new()
            };
            ui.add(egui::ProgressBar::new(used.min(1.0)).desired_width(ui.available_width()).text(format!("{} of {budget} bytes{frames}", p.file.len())));
            ui.label(RichText::new(&p.info).weak());
        }
    }

    /// The status panel: state and the Transmit button, output, spectrum, history.
    fn side(&mut self, ui: &mut Ui, settings: &mut Settings, tx: &mut TxSession, allow_device: bool) {
        let pal = Palette::for_ui(ui);
        egui::ScrollArea::vertical().id_salt("tx_side_scroll").auto_shrink([false, false]).show(ui, |ui| {
            self.transmission_card(ui, settings, tx, allow_device, &pal);
            output_status_card(ui, tx);
            if !tx.snap.spectrum.is_empty() {
                card(ui, "Output spectrum", "", |ui| {
                    let span = crate::settings::Span::Audio.range(tx.snap.rate.max(8000), tx.snap.iq);
                    let (c, bw) = band_of(settings);
                    let (c, bw) = (f64::from(c), f64::from(bw));
                    let view = SpectrumView { db: &tx.snap.spectrum, axis: tx.snap.spectrum_axis, span, band: Some((c - bw / 2.0, c + bw / 2.0)), carrier: Some(c) };
                    spectrum_plot(ui, "tx_spectrum", "output spectrum", &view, &pal, 160.0);
                });
            }
            card(ui, "Sent", "", |ui| {
                if tx.history.is_empty() {
                    placeholder(ui, "Nothing sent yet.");
                }
                for s in tx.history.iter().rev().take(30) {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(s.time.format("%H:%M:%S").to_string()).monospace().weak());
                        ui.label(&s.what);
                        ui.label(RichText::new(format!("{:.1} s{}", s.seconds, if s.completed { "" } else { ", stopped" })).weak());
                    });
                }
            });
            if !tx.messages.is_empty() {
                egui::CollapsingHeader::new(format!("Messages ({})", tx.messages.len())).id_salt("tx_messages").show(ui, |ui| {
                    for m in &tx.messages {
                        ui.add(egui::Label::new(RichText::new(m).monospace().small()).wrap());
                    }
                });
            }
        });
    }

    fn transmission_card(&mut self, ui: &mut Ui, settings: &mut Settings, tx: &mut TxSession, allow_device: bool, pal: &Palette) {
        let snap = &tx.snap;
        let (state, light) = if tx.is_running() {
            if tx.is_stopping() { ("Stopping", WARN) } else { ("On the air", GOOD) }
        } else if tx.error.is_some() {
            ("Failed", HOT)
        } else if tx.finished() {
            ("Sent", ui.visuals().weak_text_color())
        } else {
            ("Idle", ui.visuals().weak_text_color())
        };
        let mut start = false;
        let mut stop = false;
        card(ui, "Transmission", "", |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 6.0, light);
                ui.label(RichText::new(state).strong().size(17.0));
                if tx.is_running() && !tx.label.is_empty() {
                    ui.label(RichText::new(&tx.label).weak());
                }
            });
            ui.add_space(4.0);
            let width = ui.available_width();
            if tx.is_running() {
                let button = egui::Button::new(RichText::new("\u{25A0}  Stop").strong().size(16.0).color(Color32::WHITE)).fill(HOT).min_size(egui::vec2(width, 34.0));
                if ui.add_enabled(!tx.is_stopping(), button).clicked() {
                    stop = true;
                }
            } else {
                let problem = self.problem(settings, allow_device);
                let button = egui::Button::new(RichText::new("\u{25B6}  Transmit").strong().size(16.0).color(Color32::WHITE))
                    .fill(ui.visuals().selection.bg_fill)
                    .min_size(egui::vec2(width, 34.0));
                let r = ui.add_enabled(problem.is_none(), button);
                let r = match &problem {
                    Some(p) => r.on_disabled_hover_text(p.as_str()),
                    None => r.on_hover_text("Send it"),
                };
                if r.clicked() {
                    start = true;
                }
                if let Some(p) = &problem {
                    ui.label(RichText::new(p).weak().italics());
                }
            }
            if let Some(e) = &tx.error {
                ui.add(egui::Label::new(RichText::new(e).color(pal.error)).wrap());
            }
            if snap.symbols > 0 && (tx.is_running() || tx.finished()) {
                ui.add_space(4.0);
                if snap.jobs > 1 {
                    ui.horizontal(|ui| {
                        value(ui, "Frame", format!("{} of {}", snap.job + 1, snap.jobs));
                    });
                }
                let fraction = (snap.seconds / snap.total_seconds.max(0.1)).clamp(0.0, 1.0) as f32;
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(ui.available_width())
                        .text(format!("{} of {} · symbol {} / {}", fmt_time(snap.seconds), fmt_time(snap.total_seconds), snap.symbol, snap.symbols)),
                );
            } else if !tx.is_running() {
                ui.label(RichText::new(format!("About {:.1} s on the air", self.air_time(settings))).weak());
            }
        });
        if stop {
            tx.stop();
        }
        if start {
            self.transmit(settings, tx, allow_device);
        }
    }

    /// Seconds on the air for what is set up.
    fn air_time(&self, settings: &Settings) -> f64 {
        if settings.tx_kind == TxKind::Picture && settings.picture_v2 {
            let frames = self.prepared.as_ref().map_or(v2_plan(settings).frames, |(_, p)| v2_frame_count(p.file.len(), settings).1);
            return transmission_seconds(settings.v2_mode, frames, v2_lead_in(settings), settings.fancy_header);
        }
        let Some(mode) = mode_of(settings) else {
            let m = settings.data_mode;
            let frames = match (settings.data_source, &self.data) {
                (DataSource::File, Some(d)) => data_frames(d.bytes.len(), m, settings.extra_frames).1,
                _ => 1,
            };
            return m.duration_s() + m.frame_s() * (frames - 1) as f64;
        };
        let extra = settings.noise_symbols + if settings.fancy_header { 11 } else { 0 };
        let per = (mode.air_symbols() + extra) as f64 * SYMBOL_SECONDS;
        let frames = match (&self.prepared, settings.tx_kind) {
            (Some((_, p)), TxKind::Picture) if p.file.len() > IMAGE_BYTES || settings.blocks > 1 => multiframe::blocks_for(p.file.len()) + usize::from(settings.extra_frames),
            _ => 1,
        };
        per * frames as f64
    }

    /// Why the Transmit button is off.
    pub fn problem(&self, settings: &Settings, allow_device: bool) -> Option<String> {
        if let Err(e) = call_sign_check(settings) {
            return Some(format!("Call sign: {e}"));
        }
        match settings.tx_kind {
            TxKind::Picture if self.source.is_none() => return Some("Open a picture first.".into()),
            TxKind::Picture if self.job.is_some() => return Some("Compressing…".into()),
            TxKind::Picture if self.prepared.is_none() => return Some(self.error.clone().unwrap_or_else(|| "The picture is not ready.".into())),
            TxKind::Text if settings.text.is_empty() => return Some("Write a message first.".into()),
            TxKind::Text if settings.text.len() > TEXT_BYTES => return Some(format!("The message is {} bytes: at most {TEXT_BYTES}.", settings.text.len())),
            TxKind::Data => {
                if let Some(p) = self.data_problem(settings) {
                    return Some(p);
                }
            }
            _ => {}
        }
        match mode_of(settings) {
            Some(mode) => {
                let range = mode.carrier_range(signal_rate(settings), settings.tx_channel == TxChannelChoice::Iq, settings.ultrasonic);
                if !range.contains(&settings.carrier_hz) {
                    return Some(format!("Carrier: {} needs {}…{} Hz.", mode.label(), range.start(), range.end()));
                }
            }
            None => {
                if settings.tx_rate.is_some_and(|r| !modem::RATES.contains(&r)) {
                    return Some("Sample rate: the modem needs 44.1 or 48 kHz (or Automatic).".into());
                }
                let range = data_carrier_range(settings);
                if !range.contains(&settings.data_carrier_hz) || settings.data_carrier_hz % modem::CARRIER_STEP_HZ != 0 {
                    return Some(format!("Carrier: the modem needs a multiple of 300 Hz in {}…{} Hz.", range.start(), range.end()));
                }
            }
        }
        match settings.tx_output {
            TxOutputKind::Device if !allow_device => Some("Sound-card output is off in this run (--no-audio).".into()),
            TxOutputKind::File if settings.tx_file.is_none() => Some("Choose an output file.".into()),
            _ => None,
        }
    }

    /// What is wrong with the data to send, if anything.
    fn data_problem(&self, settings: &Settings) -> Option<String> {
        let mode = settings.data_mode;
        let size = mode.data_bytes();
        match settings.data_source {
            DataSource::Text if settings.text.is_empty() => Some("Write a message first.".into()),
            DataSource::Text if settings.text.len() > size => Some(format!(
                "The message is {} bytes: a {} datagram holds {size}. Pick a larger mode, or send it as a file.",
                settings.text.len(),
                mode.label()
            )),
            DataSource::Text => None,
            DataSource::File => match &self.data {
                None => Some("Open a file first.".into()),
                Some(d) if d.bytes.is_empty() => Some("The file is empty.".into()),
                Some(d) => {
                    let blocks = multiframe::blocks_in(d.bytes.len(), size);
                    (blocks > multiframe::MAX_BLOCKS_ANY)
                        .then(|| format!("The file needs {blocks} frames of {}: at most {}. Pick a larger mode.", mode.label(), multiframe::MAX_BLOCKS_ANY))
                }
            },
        }
    }

    /// Send what is set up (if nothing is in the way).
    pub fn transmit(&mut self, settings: &mut Settings, tx: &mut TxSession, allow_device: bool) {
        if self.problem(settings, allow_device).is_some() {
            return;
        }
        let call_sign = settings.call_sign.trim().to_string();
        let request = |mode: Mode, payload: Vec<u8>| TxRequest {
            mode,
            payload,
            call_sign: call_sign.clone(),
            carrier_hz: settings.carrier_hz,
            noise_symbols: settings.noise_symbols,
            fancy_header: settings.fancy_header,
            text_family: matches!(settings.tx_kind, TxKind::Text),
        };
        let (jobs, label) = match settings.tx_kind {
            TxKind::Ping => (vec![TxJob::cofdmtv("ping", request(Mode::Ping, Vec::new()), 0.0)], "ping".to_string()),
            TxKind::Text => {
                let bytes = settings.text.as_bytes().to_vec();
                let label = format!("text, {} bytes", bytes.len());
                (vec![TxJob::cofdmtv(label.clone(), request(Mode::Text(0), bytes), 0.0)], label)
            }
            TxKind::Data => {
                let mode = settings.data_mode;
                let (what, frames) = match (settings.data_source, &self.data) {
                    (DataSource::File, Some(d)) => match multiframe::split_chunks(&d.bytes, data_frames(d.bytes.len(), mode, settings.extra_frames).1, mode.data_bytes()) {
                        Ok(frames) => (format!("{}, {} bytes", d.name, d.bytes.len()), frames),
                        Err(e) => {
                            tx.error = Some(e);
                            return;
                        }
                    },
                    (DataSource::File, None) => return,
                    (DataSource::Text, _) => (format!("modem text, {} bytes", settings.text.len()), vec![settings.text.as_bytes().to_vec()]),
                };
                let n = frames.len();
                let label = format!("{what}{}, {}", if n > 1 { format!(" in {n} frames") } else { String::new() }, mode.label());
                let request = ModemRequest::new(mode, call_sign.clone(), settings.data_carrier_hz, frames);
                (vec![TxJob::modem(label.clone(), request, 0.0)], label)
            }
            TxKind::Picture if settings.picture_v2 => {
                let Some((_, p)) = &self.prepared else { return };
                let mode = settings.v2_mode;
                match v2::frames(&p.file, mode, v2_plan(settings).extra()) {
                    Ok(frames) => {
                        let n = frames.len();
                        let label = format!("v2 picture, {} bytes in {n} frame{}, {}", p.file.len(), if n == 1 { "" } else { "s" }, mode.label());
                        let request = v2::request(mode, call_sign.clone(), settings.data_carrier_hz, frames, v2_lead_in(settings), settings.fancy_header);
                        (vec![TxJob::modem(label.clone(), request, 0.0)], label)
                    }
                    Err(e) => {
                        tx.error = Some(e);
                        return;
                    }
                }
            }
            TxKind::Picture => {
                let Some((_, p)) = &self.prepared else { return };
                let mode = Mode::Image(settings.tx_mode);
                if settings.blocks <= 1 && p.file.len() <= IMAGE_BYTES {
                    let label = format!("picture, {} bytes, mode {}", p.file.len(), settings.tx_mode);
                    (vec![TxJob::cofdmtv(label.clone(), request(mode, p.file.clone()), 0.0)], label)
                } else {
                    let needed = multiframe::blocks_for(p.file.len());
                    match multiframe::split(&p.file, needed + usize::from(settings.extra_frames)) {
                        Ok(frames) => {
                            let n = frames.len();
                            let label = format!("picture, {} bytes in {n} frames, mode {}", p.file.len(), settings.tx_mode);
                            let jobs = frames
                                .into_iter()
                                .enumerate()
                                .map(|(i, f)| TxJob::cofdmtv(format!("frame {} of {n}", i + 1), request(mode, f), if i == 0 { 0.0 } else { 0.3 }))
                                .collect();
                            (jobs, label)
                        }
                        Err(e) => {
                            tx.error = Some(e);
                            return;
                        }
                    }
                }
            }
        };
        let output = match settings.tx_output {
            TxOutputKind::Device => OutputSpec::Device { name: settings.tx_device.clone() },
            TxOutputKind::File => OutputSpec::File { path: settings.tx_file.clone().unwrap_or_else(|| PathBuf::from("cofdmtv.wav")) },
        };
        let cfg = TxConfig { output, channel: settings.tx_channel.channel(), rate: settings.tx_rate, gain_db: settings.tx_gain_db, jobs };
        tx.start(cfg, label);
    }
}

fn text(ui: &mut Ui, settings: &mut Settings) {
    let pal = Palette::for_ui(ui);
    ui.add(egui::TextEdit::multiline(&mut settings.text).desired_rows(4).desired_width(f32::INFINITY).hint_text("Your message (UTF-8, up to 170 bytes)"));
    let len = settings.text.len();
    ui.horizontal(|ui| {
        let color = if len > TEXT_BYTES { pal.error } else { ui.visuals().weak_text_color() };
        ui.label(RichText::new(format!("{len} / {TEXT_BYTES} bytes")).color(color).monospace());
        if let Some(Mode::Text(n)) = Mode::text_for(len.max(1)) {
            let m = Mode::Text(n);
            ui.label(RichText::new(format!("→ mode {n}: up to {} bytes, {} symbols", m.payload_bytes(), m.symbols())).weak());
        }
    });
}

fn signal_card(ui: &mut Ui, settings: &mut Settings) {
    card(ui, "Signal", "how it sounds on the air", |ui| {
        grid(ui, "signal", |ui| {
            if settings.tx_kind == TxKind::Picture {
                row_label(ui, "Modes");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut settings.picture_v2, false, "COFDMTV 6–13").on_hover_text("Shredpix's picture modes, which Assempix receives");
                    ui.selectable_value(&mut settings.picture_v2, true, "v2").on_hover_text(
                        "The picture in aicodix modem frames, in any modulation and code rate, with the lead-in and the fancy header (COFDMtv receives it)",
                    );
                });
                ui.end_row();
            }
            match mode_of(settings) {
                Some(mode) => cofdmtv_signal(ui, settings, mode),
                None => modem_signal(ui, settings),
            }
            row_label(ui, "");
            ui.checkbox(&mut settings.ultrasonic, "Carriers above 3 kHz").on_hover_text(
                "Allow carriers up to the top of the band (Shredpix's \"ultrasonic\" option). At 44.1 and 48 kHz this reaches frequencies you may not hear: prolonged exposure to loud high-pitched sound can damage hearing.",
            );
            ui.end_row();
        });
    });
}

/// Mode, carrier, lead-in and fancy header of a COFDMTV transmission: rows of the signal grid.
fn cofdmtv_signal(ui: &mut Ui, settings: &mut Settings, mode: Mode) {
    row_label(ui, "Mode");
    if settings.tx_kind == TxKind::Picture {
        ComboBox::from_id_salt("tx_mode").width(260.0).selected_text(mode_line(Mode::Image(settings.tx_mode))).show_ui(ui, |ui| {
            for n in Mode::IMAGE_MODES {
                ui.selectable_value(&mut settings.tx_mode, n, mode_line(Mode::Image(n)));
            }
        });
    } else {
        ui.label(RichText::new(match mode {
            Mode::Ping => "ping".to_string(),
            m => format!("{} (from the length of the text)", m.label()),
        }).weak());
    }
    ui.end_row();
    row_label(ui, "Carrier");
    let range = mode.carrier_range(signal_rate(settings), settings.tx_channel == TxChannelChoice::Iq, settings.ultrasonic);
    ui.horizontal(|ui| {
        ui.add(egui::DragValue::new(&mut settings.carrier_hz).range(range.clone()).speed(10.0).suffix(" Hz"))
            .on_hover_text("Centre frequency of the signal (the apps use 50 Hz steps)");
        settings.carrier_hz = (settings.carrier_hz / 50 * 50).clamp(*range.start(), *range.end());
        let half = mode.carriers() as f32 * 3.125;
        ui.label(RichText::new(format!("{:.0}…{:.0} Hz", settings.carrier_hz as f32 - half, settings.carrier_hz as f32 + half)).weak());
    });
    ui.end_row();
    lead_in_and_header(ui, settings);
}

/// The lead-in and the fancy header: rows of the signal grid (COFDMTV and v2 pictures).
fn lead_in_and_header(ui: &mut Ui, settings: &mut Settings) {
    row_label(ui, "Lead-in");
    let label = NOISE_CHOICES.iter().find(|(n, _)| *n == settings.noise_symbols).map_or("custom", |(_, l)| l);
    ComboBox::from_id_salt("noise").selected_text(label).show_ui(ui, |ui| {
        for (n, l) in NOISE_CHOICES {
            ui.selectable_value(&mut settings.noise_symbols, n, l);
        }
    })
    .response
    .on_hover_text("Noise before the signal, so that a radio's VOX and AGC settle first");
    ui.end_row();
    row_label(ui, "");
    ui.checkbox(&mut settings.fancy_header, "Fancy header").on_hover_text("Draw your call sign into the waterfall after the signal (2 s)");
    ui.end_row();
}

/// Mode (modulation, code rate, frame size) and carrier of a modem transmission, data or a
/// v2 picture (with its lead-in and fancy header): rows of the signal grid.
fn modem_signal(ui: &mut Ui, settings: &mut Settings) {
    let picture = settings.tx_kind == TxKind::Picture;
    row_label(ui, "Mode");
    ui.horizontal(|ui| {
        let m = if picture { &mut settings.v2_mode } else { &mut settings.data_mode };
        ComboBox::from_id_salt("data_modulation").width(80.0).selected_text(m.modulation.name()).show_ui(ui, |ui| {
            for x in Modulation::ALL {
                ui.selectable_value(&mut m.modulation, x, x.name());
            }
        })
        .response
        .on_hover_text("Modulation: more bits per carrier need a cleaner channel");
        ComboBox::from_id_salt("data_rate").width(50.0).selected_text(m.rate.name()).show_ui(ui, |ui| {
            for r in CodeRate::ALL {
                ui.selectable_value(&mut m.rate, r, r.name());
            }
        })
        .response
        .on_hover_text("Code rate: the share of the coded bits that is data; lower rates correct more errors");
        ComboBox::from_id_salt("data_frame").width(70.0).selected_text(if m.normal { "normal" } else { "short" }).show_ui(ui, |ui| {
            ui.selectable_value(&mut m.normal, false, "short");
            ui.selectable_value(&mut m.normal, true, "normal");
        })
        .response
        .on_hover_text("Frame size: normal frames carry two to four times as much as short ones");
    });
    ui.end_row();
    let m = if picture { settings.v2_mode } else { settings.data_mode };
    row_label(ui, "");
    ui.label(RichText::new(format!("{} bytes a frame · {:.1} s · {:.1} kbit/s", m.data_bytes(), m.duration_s(), m.bitrate() / 1000.0)).weak());
    ui.end_row();
    row_label(ui, "Carrier");
    let range = data_carrier_range(settings);
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut settings.data_carrier_hz, range.clone()).step_by(f64::from(modem::CARRIER_STEP_HZ)).suffix(" Hz"))
            .on_hover_text("Centre frequency of the signal (the modem uses multiples of 300 Hz)");
        let step = modem::CARRIER_STEP_HZ;
        settings.data_carrier_hz = ((settings.data_carrier_hz as f32 / step as f32).round() as i32 * step).clamp(*range.start(), *range.end());
        let half = modem::BANDWIDTH_HZ / 2;
        ui.label(RichText::new(format!("{}…{} Hz", settings.data_carrier_hz - half, settings.data_carrier_hz + half)).weak());
    });
    ui.end_row();
    if picture {
        lead_in_and_header(ui, settings);
    } else {
        row_label(ui, "");
        ui.label(RichText::new("The modem starts with a noise symbol of its own: no lead-in or fancy header.").weak().small());
        ui.end_row();
    }
}

fn mode_line(mode: Mode) -> String {
    let seconds = mode.air_symbols() as f64 * SYMBOL_SECONDS;
    format!("{} · {:.0} s · {:.1} kbit/s", mode.label(), seconds, mode.bitrate() / 1000.0)
}

fn output_card(ui: &mut Ui, settings: &mut Settings, devices: &mut DeviceLists) {
    card(ui, "Output", "where the signal goes", |ui| {
        grid(ui, "output", |ui| {
            row_label(ui, "To");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut settings.tx_output, TxOutputKind::Device, "Sound card");
                ui.selectable_value(&mut settings.tx_output, TxOutputKind::File, "File");
            });
            ui.end_row();
            match settings.tx_output {
                TxOutputKind::Device => {
                    row_label(ui, "Sound card");
                    ui.horizontal(|ui| {
                        let names = devices.outputs().to_vec();
                        device_combo(ui, "output_device", &mut settings.tx_device, &names, "System default", 260.0);
                        if ui.small_button("⟳").on_hover_text("Refresh the device list").clicked() {
                            devices.refresh();
                        }
                    });
                    ui.end_row();
                }
                TxOutputKind::File => {
                    row_label(ui, "File");
                    ui.horizontal(|ui| {
                        let name = settings.tx_file.as_deref().map_or_else(|| "none".to_string(), |p| p.display().to_string());
                        ui.add(egui::Label::new(RichText::new(name).monospace()).truncate());
                        if ui.button("Save as…").clicked()
                            && let Some(p) = rfd::FileDialog::new().set_title("Write the signal to").add_filter("WAV", &["wav"]).add_filter("FLAC", &["flac"]).set_file_name("cofdmtv.wav").save_file()
                        {
                            settings.tx_file = Some(p);
                        }
                    });
                    ui.end_row();
                }
            }
            row_label(ui, "Sample rate");
            let label = |r: Option<u32>| r.map_or_else(|| "Automatic".to_string(), |r| format!("{} Hz", r));
            let rates: &[u32] = if settings.modem_signal() { &modem::RATES } else { &RATES };
            ComboBox::from_id_salt("tx_rate").selected_text(label(settings.tx_rate)).show_ui(ui, |ui| {
                ui.selectable_value(&mut settings.tx_rate, None, "Automatic").on_hover_text("The sound card's rate (files: 48 kHz)");
                for &r in rates {
                    ui.selectable_value(&mut settings.tx_rate, Some(r), label(Some(r)));
                }
            })
            .response
            .on_hover_text("The rate the signal is built at. 8 and 16 kHz add Shredpix's oversampled peak limiting.");
            ui.end_row();
            row_label(ui, "Channels");
            ComboBox::from_id_salt("tx_channel").selected_text(settings.tx_channel.label()).show_ui(ui, |ui| {
                for c in TxChannelChoice::ALL {
                    ui.selectable_value(&mut settings.tx_channel, c, c.label());
                }
            })
            .response
            .on_hover_text("Mono: on every channel; Left/Right: on one; I/Q: the complex signal (Shredpix's analytic output)");
            ui.end_row();
            row_label(ui, "Level");
            ui.add(egui::Slider::new(&mut settings.tx_gain_db, -30.0..=6.0).suffix(" dB").step_by(0.5))
                .on_hover_text("Relative to the apps' level (about −12 dBFS RMS); above 0 dB the peaks clip");
            ui.end_row();
        });
    });
}

/// Output level, destination, rates, underruns.
fn output_status_card(ui: &mut Ui, tx: &TxSession) {
    let snap = &tx.snap;
    card(ui, "Output", "", |ui| {
        if snap.destination.is_empty() {
            placeholder(ui, "The output level and destination appear while transmitting.");
            return;
        }
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 150.0).max(120.0);
            level_meter(ui, "tx_out_meter", snap.level.filter(|_| tx.is_running()), width);
            if let Some((rms, peak)) = snap.level {
                ui.label(RichText::new(format!("{rms:.1} dBFS RMS\npeak {peak:.1}")).monospace().small());
            }
        });
        ui.add_space(2.0);
        egui::Grid::new("tx_output_status").num_columns(2).spacing([12.0, 3.0]).show(ui, |ui| {
            value(ui, "To", snap.destination.clone());
            ui.end_row();
            let rates = if snap.rate == snap.output_rate { format!("{} Hz", snap.rate) } else { format!("{} Hz → {} Hz", snap.rate, snap.output_rate) };
            value(ui, "Rate", format!("{rates}, {} ch", snap.channels));
            ui.end_row();
            if snap.underruns > 0 {
                value(ui, "Underruns", snap.underruns.to_string());
                ui.end_row();
            }
            if let Some(t) = tx.elapsed() {
                value(ui, "Elapsed", fmt_time(t));
                ui.end_row();
            }
        });
    });
}
