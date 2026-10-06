//! `cofdmtv` — COFDMTV pictures, Rattlegram text, pings and aicodix modem datagrams from the
//! command line.
//!
//! ```text
//! cofdmtv rx recording.wav --out-dir received
//! cofdmtv rx --device "CABLE-A Output"
//! cofdmtv tx picture photo.jpg --call DL1ABC -o picture.wav
//! cofdmtv tx text "Hello" --call DL1ABC --device "CABLE-A Input"
//! cofdmtv tx data report.pdf --modulation qam16 --frame normal --call DL1ABC -o data.wav
//! cofdmtv tx picture photo.jpg --v2 --modulation qam64 --frame normal --air-time 30 -o v2.wav
//! cofdmtv devices
//! ```

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use cofdmtv_engine::cofdmtv_core::cofdmtv::multiframe;
use cofdmtv_engine::cofdmtv_core::cofdmtv::{IMAGE_BYTES, Mode, RATES, TEXT_BYTES, TxRequest};
use cofdmtv_engine::cofdmtv_core::modem::{self, CodeRate, ModemMode, ModemRequest, Modulation};
use cofdmtv_engine::cofdmtv_io::{list_input_devices, list_output_devices};
use cofdmtv_engine::{ChannelSel, InputSpec, OutputSpec, Receiver, RxConfig, RxEvent, TxChannel, TxConfig, TxEvent, TxJob, Transmitter, v2};
use cofdmtv_pix::{Format, PIXEL_CHOICES, Source};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "cofdmtv",
    version,
    about = "COFDMTV transmitter and receiver: pictures (Shredpix/Assempix), text (Rattlegram), pings, and aicodix modem datagrams"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Receive from a recording or a sound card; pictures and files are saved, texts printed.
    Rx(RxArgs),
    /// Transmit a picture, a text, a ping or modem datagrams to a sound card or a file.
    Tx(TxArgs),
    /// List the sound cards.
    Devices,
}

#[derive(Args)]
struct RxArgs {
    /// Recording to decode (WAV or FLAC); without it, the sound card.
    file: Option<PathBuf>,
    /// Sound-card input (name or a unique part of it); the system default if omitted.
    #[arg(long, conflicts_with = "file")]
    device: Option<String>,
    /// Channel carrying the signal.
    #[arg(long, value_enum, default_value_t = RxChannel::Mix)]
    channel: RxChannel,
    /// Save received pictures and files (and a log of the messages) here.
    #[arg(long, default_value = "received")]
    out_dir: PathBuf,
    /// Pace a recording to real time.
    #[arg(long)]
    realtime: bool,
    /// Stop after this many seconds (sound card).
    #[arg(long)]
    duration: Option<f64>,
    /// Print the decoder's log.
    #[arg(long, short)]
    verbose: bool,
}

#[derive(Copy, Clone, ValueEnum)]
enum RxChannel {
    /// The mean of the channels.
    Mix,
    Left,
    Right,
    /// I left, Q right.
    Iq,
    /// Q left, I right.
    Qi,
}

#[derive(Args)]
struct TxArgs {
    #[command(subcommand)]
    what: TxWhat,
    #[command(flatten)]
    common: TxCommon,
}

#[derive(Subcommand)]
enum TxWhat {
    /// A picture (any common format; compressed to fit unless it already does).
    Picture(PictureArgs),
    /// A UTF-8 text of up to 170 bytes (Rattlegram).
    Text {
        /// The message.
        text: String,
    },
    /// Just the call sign.
    Ping,
    /// Modem datagrams (aicodix modem, 44.1 or 48 kHz): a text in one datagram, or a file in
    /// frames with a multi-frame header.
    Data(DataArgs),
}

#[derive(Args)]
struct PictureArgs {
    /// The picture file.
    file: PathBuf,
    /// Picture mode: 6, 7 (8PSK), 8, 9 (QPSK), 10, 11 (8PSK), 12, 13 (QPSK).
    #[arg(long, default_value_t = 11, value_parser = clap::value_parser!(u8).range(6..=13), conflicts_with = "v2")]
    mode: u8,
    /// A v2 mode: the picture in aicodix modem frames (--modulation, --code-rate,
    /// --frame) after the lead-in (--noise) and before the fancy header, compressed to
    /// fill --air-time, with --extra frames.
    #[arg(long)]
    v2: bool,
    #[command(flatten)]
    modcod: ModcodArgs,
    /// Seconds a v2 picture may take on the air.
    #[arg(long, default_value_t = 30.0, requires = "v2")]
    air_time: f64,
    /// Compression (with --v2, jpeg is a progressive JPEG: it arrives whole and coarse
    /// first, then sharper; a WebP arrives from the top down).
    #[arg(long, value_enum, default_value_t = PicFormat::Webp)]
    format: PicFormat,
    /// Most pixels: 1M, 512K, 256K, 128K, 64K, 32K, 16K, or auto (the largest that keeps a
    /// decent quality).
    #[arg(long, default_value = "auto")]
    pixels: String,
    /// Data blocks (1 = one frame; up to 12 for a multi-frame picture Assempix rebuilds).
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=12), conflicts_with = "v2")]
    blocks: u8,
    /// Extra frames of a multi-frame or v2 picture (any `blocks` of the frames rebuild it).
    #[arg(long, default_value_t = 1)]
    extra: u8,
    /// Re-compress even if the file already fits.
    #[arg(long)]
    recompress: bool,
}

#[derive(Copy, Clone, ValueEnum)]
enum PicFormat {
    Webp,
    WebpLossless,
    Jpeg,
    Png,
}

#[derive(Args)]
struct DataArgs {
    /// The file to send (in as many frames as it takes; any `blocks` of them rebuild it).
    #[arg(required_unless_present = "text", conflicts_with = "text")]
    file: Option<PathBuf>,
    /// Send this text (one datagram) instead of a file.
    #[arg(long)]
    text: Option<String>,
    #[command(flatten)]
    modcod: ModcodArgs,
    /// Extra frames for a file (any `blocks` of the frames rebuild it); default: 1 if the
    /// file takes more than one frame, else 0.
    #[arg(long)]
    extra: Option<u16>,
}

/// A modem mode: modulation, code rate, frame size.
#[derive(Args)]
struct ModcodArgs {
    /// Modulation (modem data and v2 pictures).
    #[arg(long, value_enum, default_value_t = ModArg::Qam16)]
    modulation: ModArg,
    /// Code rate (modem data and v2 pictures).
    #[arg(long, value_enum, default_value_t = RateArg::Half)]
    code_rate: RateArg,
    /// Frame size (normal frames carry two to four times as much).
    #[arg(long, value_enum, default_value_t = FrameArg::Short)]
    frame: FrameArg,
}

impl ModcodArgs {
    fn mode(&self) -> ModemMode {
        let modulation = match self.modulation {
            ModArg::Bpsk => Modulation::Bpsk,
            ModArg::Qpsk => Modulation::Qpsk,
            ModArg::Psk8 => Modulation::Psk8,
            ModArg::Qam16 => Modulation::Qam16,
            ModArg::Qam64 => Modulation::Qam64,
            ModArg::Qam256 => Modulation::Qam256,
            ModArg::Qam1024 => Modulation::Qam1024,
            ModArg::Qam4096 => Modulation::Qam4096,
        };
        let rate = match self.code_rate {
            RateArg::Half => CodeRate::Half,
            RateArg::TwoThirds => CodeRate::TwoThirds,
            RateArg::ThreeQuarters => CodeRate::ThreeQuarters,
            RateArg::FiveSixths => CodeRate::FiveSixths,
        };
        ModemMode { modulation, rate, normal: matches!(self.frame, FrameArg::Normal) }
    }
}

#[derive(Copy, Clone, ValueEnum)]
enum ModArg {
    Bpsk,
    Qpsk,
    #[value(name = "8psk")]
    Psk8,
    Qam16,
    Qam64,
    Qam256,
    Qam1024,
    Qam4096,
}

#[derive(Copy, Clone, ValueEnum)]
enum RateArg {
    #[value(name = "1/2")]
    Half,
    #[value(name = "2/3")]
    TwoThirds,
    #[value(name = "3/4")]
    ThreeQuarters,
    #[value(name = "5/6")]
    FiveSixths,
}

#[derive(Copy, Clone, ValueEnum)]
enum FrameArg {
    Short,
    Normal,
}

#[derive(Args)]
struct TxCommon {
    /// Your call sign (letters and digits, up to nine).
    #[arg(long, global = true, default_value = "ANONYMOUS")]
    call: String,
    /// Carrier (centre) frequency, Hz [default: 1700; data and v2: 1500, a multiple of 300].
    #[arg(long, global = true, allow_hyphen_values = true)]
    carrier: Option<i32>,
    /// Seconds of noise before each COFDMTV transmission or v2 picture (in whole symbols).
    #[arg(long, global = true, default_value_t = 1.0)]
    noise: f64,
    /// Leave out the call sign drawn on the waterfall after each COFDMTV transmission or
    /// v2 picture.
    #[arg(long, global = true)]
    no_fancy: bool,
    /// Write to this file (WAV or FLAC) instead of a sound card.
    #[arg(long, short, global = true)]
    out: Option<PathBuf>,
    /// Sound-card output (name or a unique part of it); the system default if omitted.
    #[arg(long, global = true, conflicts_with = "out")]
    device: Option<String>,
    /// Sample rate of the signal (8000, 16000, 32000, 44100, 48000; data and v2: 44100, 48000);
    /// default: the sound card's, or 48000 for files.
    #[arg(long, global = true)]
    rate: Option<u32>,
    /// Output channels.
    #[arg(long, global = true, value_enum, default_value_t = OutChannel::Mono)]
    channel: OutChannel,
    /// Level relative to the apps', dB.
    #[arg(long, global = true, default_value_t = 0.0, allow_hyphen_values = true)]
    gain: f32,
}

#[derive(Copy, Clone, ValueEnum)]
enum OutChannel {
    Mono,
    Left,
    Right,
    Iq,
}

/// COFDMTV's carrier unless one is given, Hz.
const COFDMTV_CARRIER_HZ: i32 = 1700;

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Devices => devices(),
        Command::Rx(a) => rx(a),
        Command::Tx(a) => tx(a),
    }
}

fn devices() -> Result<()> {
    for (title, list) in [("Inputs", list_input_devices()?), ("Outputs", list_output_devices()?)] {
        println!("{title}:");
        for d in list {
            let fmt = d.default_format.map_or_else(String::new, |f| format!("  ({f})"));
            println!("  {}{}{fmt}", if d.is_default { "* " } else { "  " }, d.name);
        }
    }
    Ok(())
}

/// A flag set by Ctrl+C.
fn interrupted() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&flag);
    let _ = ctrlc::set_handler(move || f.store(true, Ordering::Relaxed));
    flag
}

fn rx(a: RxArgs) -> Result<()> {
    let input = match a.file {
        Some(path) => InputSpec::File { path, realtime: a.realtime },
        None => InputSpec::Device { name: a.device },
    };
    let channel = match a.channel {
        RxChannel::Mix => ChannelSel::Mix,
        RxChannel::Left => ChannelSel::Left,
        RxChannel::Right => ChannelSel::Right,
        RxChannel::Iq => ChannelSel::Iq,
        RxChannel::Qi => ChannelSel::IqSwapped,
    };
    let stop = interrupted();
    let started = Instant::now();
    let mut rx = Receiver::start(RxConfig { input, channel, save_dir: Some(a.out_dir) });
    let mut stopping = false;
    let mut error = None;
    let frames = |n: usize| if n > 1 { format!(" in {n} frames") } else { String::new() };
    let saved = |s: Option<PathBuf>| s.map_or(String::new(), |s| format!(" -> {}", s.display()));
    'run: loop {
        std::thread::sleep(Duration::from_millis(50));
        let _ = rx.take_rows();
        if !stopping && (stop.load(Ordering::Relaxed) || a.duration.is_some_and(|d| started.elapsed().as_secs_f64() >= d)) {
            rx.stop();
            stopping = true;
        }
        for ev in rx.poll_events() {
            match ev {
                RxEvent::Log(line) if a.verbose => eprintln!("{line}"),
                RxEvent::Picture(p) => println!(
                    "{} {:<9} picture, {}: {} bytes, {}{}{}",
                    p.time.format("%H:%M:%S"),
                    p.call,
                    p.label,
                    p.data.len(),
                    p.kind.map_or("unknown format", |k| k.name()),
                    frames(p.frames),
                    saved(p.saved)
                ),
                RxEvent::File(f) => println!(
                    "{} {:<9} file, {}: {} bytes{}{}",
                    f.time.format("%H:%M:%S"),
                    f.call,
                    f.label,
                    f.data.len(),
                    frames(f.frames),
                    saved(f.saved)
                ),
                RxEvent::Text(m) => println!("{} {:<9} text, {}: {}", m.time.format("%H:%M:%S"), m.call, m.label, m.text),
                RxEvent::Ping { call, cfo_hz } => println!("{:<9} ping at {cfo_hz:.0} Hz", call),
                RxEvent::MultiFrame { call, have, need, size, head, .. } => {
                    // A picture arriving in order (v2): how much of it is in.
                    let shown = cofdmtv_pix::decode_partial(&head).map_or(String::new(), |p| {
                        if p.progressive && p.rows == p.image.height() {
                            format!(", the whole picture, {}% in", head.len() * 100 / size.max(1))
                        } else {
                            format!(", {} of {} rows in", p.rows, p.image.height())
                        }
                    });
                    println!("{call:<9} frame {have} of {need} ({size} bytes{shown})");
                }
                RxEvent::DecodeFailed { label, call, snr_db } => println!("{call:<9} {label}: decoding failed (SNR {snr_db:.1} dB)"),
                RxEvent::Error(e) => error = Some(e),
                RxEvent::Stopped => break 'run,
                _ => {}
            }
        }
    }
    rx.join();
    match error {
        Some(e) => Err(anyhow!(e)),
        None => Ok(()),
    }
}

fn tx(a: TxArgs) -> Result<()> {
    let c = &a.common;
    let modem_signal = match &a.what {
        TxWhat::Data(_) => true,
        TxWhat::Picture(p) => p.v2,
        _ => false,
    };
    let rates: &[u32] = if modem_signal { &modem::RATES } else { &RATES };
    if let Some(r) = c.rate
        && !rates.contains(&r)
    {
        bail!("rate {r}: use one of {rates:?}");
    }
    let iq = matches!(c.channel, OutChannel::Iq);
    let jobs = match &a.what {
        TxWhat::Ping => vec![TxJob::cofdmtv("ping", cofdmtv_request(c, Mode::Ping, iq)?, 0.0)],
        TxWhat::Text { text } => {
            if text.len() > TEXT_BYTES {
                bail!("{} bytes: a text has at most {TEXT_BYTES}", text.len());
            }
            let request = TxRequest { payload: text.as_bytes().to_vec(), ..cofdmtv_request(c, Mode::Text(0), iq)? };
            vec![TxJob::cofdmtv("text", request, 0.0)]
        }
        TxWhat::Picture(p) if p.v2 => vec![picture_v2_job(p, c, iq)?],
        TxWhat::Picture(p) => picture_jobs(p, c, iq)?,
        TxWhat::Data(d) => vec![data_job(d, c, iq)?],
    };
    let output = match &c.out {
        Some(path) => OutputSpec::File { path: path.clone() },
        None => OutputSpec::Device { name: c.device.clone() },
    };
    let channel = match c.channel {
        OutChannel::Mono => TxChannel::Mono,
        OutChannel::Left => TxChannel::Left,
        OutChannel::Right => TxChannel::Right,
        OutChannel::Iq => TxChannel::Iq,
    };
    let stop = interrupted();
    let mut tx = Transmitter::start(TxConfig { output, channel, rate: c.rate, gain_db: c.gain, jobs });
    let mut error = None;
    'run: loop {
        std::thread::sleep(Duration::from_millis(50));
        if stop.load(Ordering::Relaxed) {
            tx.stop();
        }
        for ev in tx.poll_events() {
            match ev {
                TxEvent::Log(line) => eprintln!("{line}"),
                TxEvent::Error(e) => error = Some(e),
                TxEvent::Stopped => break 'run,
                _ => {}
            }
        }
    }
    tx.join();
    match error {
        Some(e) => Err(anyhow!(e)),
        None => Ok(()),
    }
}

/// A COFDMTV transmission in `mode` without its payload, the carrier checked.
fn cofdmtv_request(c: &TxCommon, mode: Mode, iq: bool) -> Result<TxRequest> {
    let carrier = c.carrier.unwrap_or(COFDMTV_CARRIER_HZ);
    let range = mode.carrier_range(c.rate.unwrap_or(48_000), iq, true);
    if !range.contains(&carrier) {
        bail!("carrier {carrier} Hz: {} needs {}…{} Hz here", mode.label(), range.start(), range.end());
    }
    Ok(TxRequest {
        mode,
        payload: Vec::new(),
        call_sign: c.call.clone(),
        carrier_hz: carrier,
        noise_symbols: (c.noise / 0.18).round().max(0.0) as usize,
        fancy_header: !c.no_fancy,
        text_family: matches!(mode, Mode::Text(_)),
    })
}

fn picture_jobs(p: &PictureArgs, c: &TxCommon, iq: bool) -> Result<Vec<TxJob>> {
    let mode = Mode::Image(p.mode);
    let template = cofdmtv_request(c, mode, iq)?;
    let request = |payload| TxRequest { payload, ..template.clone() };
    let blocks = usize::from(p.blocks);
    let budget = if blocks == 1 { IMAGE_BYTES } else { blocks * multiframe::BLOCK_DATA };
    let file = picture_file(p, budget)?;
    if blocks == 1 && file.len() <= IMAGE_BYTES {
        return Ok(vec![TxJob::cofdmtv("picture", request(file), 0.0)]);
    }
    let needed = multiframe::blocks_for(file.len());
    let frames = multiframe::split(&file, needed + usize::from(p.extra)).map_err(|e| anyhow!(e))?;
    eprintln!("{} bytes in {} frames (any {needed} rebuild the picture)", file.len(), frames.len());
    let count = frames.len();
    Ok(frames
        .into_iter()
        .enumerate()
        .map(|(i, f)| TxJob::cofdmtv(format!("frame {} of {count}", i + 1), request(f), if i == 0 { 0.0 } else { 0.3 }))
        .collect())
}

/// A picture in a v2 mode: modem frames after the lead-in and before the fancy header, the
/// picture compressed to fill the air time (see `cofdmtv_engine::v2`).
fn picture_v2_job(p: &PictureArgs, c: &TxCommon, iq: bool) -> Result<TxJob> {
    let mode = p.modcod.mode();
    let carrier = modem_carrier(c, iq)?;
    let lead = v2::lead_in_symbols(c.noise);
    let fancy = !c.no_fancy;
    let plan = v2::plan(mode, p.air_time, lead, fancy, usize::from(p.extra));
    let file = picture_file(p, plan.budget)?;
    let frames = v2::frames(&file, mode, plan.extra()).map_err(|e| anyhow!(e))?;
    let blocks = frames.len() - plan.extra();
    eprintln!(
        "{} bytes in {} {} frame{} (any {blocks} rebuild the picture), {:.1} s",
        file.len(),
        frames.len(),
        mode.label(),
        if frames.len() == 1 { "" } else { "s" },
        modem::transmission_seconds(mode, frames.len(), lead, fancy)
    );
    Ok(TxJob::modem(format!("v2 picture, {}", mode.label()), v2::request(mode, c.call.clone(), carrier, frames, lead, fancy), 0.0))
}

/// The picture to send, as it is if it fits in `budget` bytes, else compressed to fit.
fn picture_file(p: &PictureArgs, budget: usize) -> Result<Vec<u8>> {
    let src = Source::open(&p.file).map_err(|e| anyhow!(e))?;
    let format = match p.format {
        PicFormat::Webp => Format::WebpLossy,
        PicFormat::WebpLossless => Format::WebpLossless,
        // v2 frames carry the picture in order: it arrives whole and coarse first.
        PicFormat::Jpeg if p.v2 => Format::JpegProgressive,
        PicFormat::Jpeg => Format::Jpeg,
        PicFormat::Png => Format::Png,
    };
    let file = match src.original_if_fits(budget).filter(|_| !p.recompress) {
        Some(o) => {
            eprintln!("{} fits as it is: {} bytes", p.file.display(), o.len());
            o.to_vec()
        }
        None => {
            let (pixels, e) = if p.pixels.eq_ignore_ascii_case("auto") {
                cofdmtv_pix::encode_auto(&src.image, format, budget, 1 << 20, 50).map_err(|e| anyhow!(e))?
            } else {
                let pixels = PIXEL_CHOICES
                    .iter()
                    .find(|(_, name)| name.eq_ignore_ascii_case(&p.pixels))
                    .map(|(n, _)| *n)
                    .with_context(|| format!("--pixels {}: use 1M, 512K, 256K, 128K, 64K, 32K, 16K or auto", p.pixels))?;
                (pixels, cofdmtv_pix::encode_to_fit(&cofdmtv_pix::scale(&src.image, pixels), format, budget).map_err(|e| anyhow!(e))?)
            };
            let quality = e.quality.map_or(String::new(), |q| format!(", quality {q}"));
            eprintln!("{}: {}×{} {}{quality}, {} bytes (at most {pixels} pixels)", p.file.display(), e.width, e.height, format.label(), e.bytes.len());
            e.bytes
        }
    };
    Ok(file)
}

/// The modem's carrier: the one given, or its usual 1500 Hz; a multiple of 300 Hz within
/// the band.
fn modem_carrier(c: &TxCommon, iq: bool) -> Result<i32> {
    let carrier = c.carrier.unwrap_or(modem::DEFAULT_CARRIER_HZ);
    let range = modem::carrier_range(c.rate.unwrap_or(48_000), iq);
    if carrier % modem::CARRIER_STEP_HZ != 0 || !range.contains(&carrier) {
        bail!("carrier {carrier} Hz: the modem needs a multiple of {} Hz in {}…{} Hz here", modem::CARRIER_STEP_HZ, range.start(), range.end());
    }
    Ok(carrier)
}

/// A modem transmission: a text in one datagram, or a file in frames with a multi-frame
/// header (its exact size and CRC go with it, and extra frames make up for lost ones).
fn data_job(d: &DataArgs, c: &TxCommon, iq: bool) -> Result<TxJob> {
    let mode = d.modcod.mode();
    let carrier = modem_carrier(c, iq)?;
    let size = mode.data_bytes();
    let request = |frames| ModemRequest::new(mode, c.call.clone(), carrier, frames);
    if let Some(text) = &d.text {
        if text.is_empty() || text.len() > size {
            bail!("{} bytes: a datagram of {} holds 1…{size}; pick a larger mode, or send a file", text.len(), mode.label());
        }
        return Ok(TxJob::modem("text", request(vec![text.as_bytes().to_vec()]), 0.0));
    }
    let path = d.file.as_ref().context("a file or --text")?;
    let file = std::fs::read(path).with_context(|| format!("{}", path.display()))?;
    let blocks = multiframe::blocks_in(file.len(), size);
    let extra = d.extra.map_or(usize::from(blocks > 1), usize::from);
    let frames = multiframe::split_chunks(&file, blocks + extra, size).map_err(|e| anyhow!(e))?;
    eprintln!(
        "{}: {} bytes in {} {} frame{} of {size} bytes (any {blocks} rebuild it), {:.1} s",
        path.display(),
        file.len(),
        frames.len(),
        mode.label(),
        if frames.len() == 1 { "" } else { "s" },
        mode.duration_s() + mode.frame_s() * (frames.len() - 1) as f64
    );
    let name = path.file_name().map_or_else(|| "file".to_string(), |n| n.to_string_lossy().into_owned());
    Ok(TxJob::modem(name, request(frames), 0.0))
}
