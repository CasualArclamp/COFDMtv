//! `cofdmtv` — COFDMTV pictures, text and pings from the command line.
//!
//! ```text
//! cofdmtv rx recording.wav --out-dir received
//! cofdmtv rx --device "CABLE-A Output"
//! cofdmtv tx picture photo.jpg --call DL1ABC -o picture.wav
//! cofdmtv tx text "Hello" --call DL1ABC --device "CABLE-A Input"
//! cofdmtv devices
//! ```

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use cofdmtv_engine::cofdmtv_core::cofdmtv::multiframe;
use cofdmtv_engine::cofdmtv_core::cofdmtv::{IMAGE_BYTES, Mode, RATES, TxRequest};
use cofdmtv_engine::cofdmtv_io::{list_input_devices, list_output_devices};
use cofdmtv_engine::{ChannelSel, InputSpec, OutputSpec, Receiver, RxConfig, RxEvent, TxChannel, TxConfig, TxEvent, TxJob, Transmitter};
use cofdmtv_pix::{Format, PIXEL_CHOICES, Source};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "cofdmtv", version, about = "COFDMTV transmitter and receiver: pictures (Shredpix/Assempix), text (Rattlegram), pings")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Receive from a recording or a sound card; pictures are saved, texts printed.
    Rx(RxArgs),
    /// Transmit a picture, a text or a ping to a sound card or a file.
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
    /// Save received pictures (and a log of the messages) here.
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
}

#[derive(Args)]
struct PictureArgs {
    /// The picture file.
    file: PathBuf,
    /// Picture mode: 6, 7 (8PSK), 8, 9 (QPSK), 10, 11 (8PSK), 12, 13 (QPSK).
    #[arg(long, default_value_t = 11, value_parser = clap::value_parser!(u8).range(6..=13))]
    mode: u8,
    /// Compression.
    #[arg(long, value_enum, default_value_t = PicFormat::Webp)]
    format: PicFormat,
    /// Most pixels: 1M, 512K, 256K, 128K, 64K, 32K, 16K, or auto (the largest that keeps a
    /// decent quality).
    #[arg(long, default_value = "auto")]
    pixels: String,
    /// Data blocks (1 = one frame; up to 12 for a multi-frame picture Assempix rebuilds).
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=12))]
    blocks: u8,
    /// Extra frames of a multi-frame picture (any `blocks` of the frames rebuild it).
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
struct TxCommon {
    /// Your call sign (letters and digits, up to nine).
    #[arg(long, global = true, default_value = "ANONYMOUS")]
    call: String,
    /// Carrier (centre) frequency, Hz.
    #[arg(long, global = true, default_value_t = 1700)]
    carrier: i32,
    /// Seconds of noise before each transmission (rounded to 180 ms symbols).
    #[arg(long, global = true, default_value_t = 1.0)]
    noise: f64,
    /// Leave out the call sign drawn on the waterfall after each transmission.
    #[arg(long, global = true)]
    no_fancy: bool,
    /// Write to this file (WAV or FLAC) instead of a sound card.
    #[arg(long, short, global = true)]
    out: Option<PathBuf>,
    /// Sound-card output (name or a unique part of it); the system default if omitted.
    #[arg(long, global = true, conflicts_with = "out")]
    device: Option<String>,
    /// Sample rate of the signal (8000, 16000, 32000, 44100, 48000); default: the sound
    /// card's, or 48000 for files.
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
                    "{} {:<9} picture: {} bytes, {}{}",
                    p.time.format("%H:%M:%S"),
                    p.call,
                    p.data.len(),
                    p.kind.map_or("unknown format", |k| k.name()),
                    p.saved.map_or(String::new(), |s| format!(" -> {}", s.display()))
                ),
                RxEvent::Text(m) => println!("{} {:<9} text: {}", m.time.format("%H:%M:%S"), m.call, m.text),
                RxEvent::Ping { call, cfo_hz } => println!("{:<9} ping at {cfo_hz:.0} Hz", call),
                RxEvent::MultiFrame { call, have, need, size } => println!("{call:<9} frame {have} of {need} ({size} bytes)"),
                RxEvent::DecodeFailed { call, snr_db, .. } => println!("{call:<9} decoding failed (SNR {snr_db:.1} dB)"),
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
    if let Some(r) = c.rate
        && !RATES.contains(&r)
    {
        bail!("rate {r}: use one of {RATES:?}");
    }
    let noise_symbols = (c.noise / 0.18).round().max(0.0) as usize;
    let request = |mode, payload, text_family| TxRequest {
        mode,
        payload,
        call_sign: c.call.clone(),
        carrier_hz: c.carrier,
        noise_symbols,
        fancy_header: !c.no_fancy,
        text_family,
    };
    let jobs = match &a.what {
        TxWhat::Ping => vec![TxJob { label: "ping".into(), request: request(Mode::Ping, Vec::new(), false), gap_s: 0.0 }],
        TxWhat::Text { text } => {
            if text.len() > 170 {
                bail!("{} bytes: a text has at most 170", text.len());
            }
            vec![TxJob { label: "text".into(), request: request(Mode::Text(0), text.as_bytes().to_vec(), true), gap_s: 0.0 }]
        }
        TxWhat::Picture(p) => picture_jobs(p, &request)?,
    };
    let mode = jobs[0].request.mode;
    let rate = c.rate.unwrap_or(48_000);
    let range = mode.carrier_range(rate, matches!(c.channel, OutChannel::Iq), true);
    if !range.contains(&c.carrier) {
        bail!("carrier {} Hz: {} needs {}…{} Hz here", c.carrier, mode.label(), range.start(), range.end());
    }
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

fn picture_jobs(p: &PictureArgs, request: &dyn Fn(Mode, Vec<u8>, bool) -> TxRequest) -> Result<Vec<TxJob>> {
    let src = Source::open(&p.file).map_err(|e| anyhow!(e))?;
    let blocks = usize::from(p.blocks);
    let budget = if blocks == 1 { IMAGE_BYTES } else { blocks * multiframe::BLOCK_DATA };
    let format = match p.format {
        PicFormat::Webp => Format::WebpLossy,
        PicFormat::WebpLossless => Format::WebpLossless,
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
    let mode = Mode::Image(p.mode);
    if blocks == 1 && file.len() <= IMAGE_BYTES {
        return Ok(vec![TxJob { label: "picture".into(), request: request(mode, file, false), gap_s: 0.0 }]);
    }
    let needed = multiframe::blocks_for(file.len());
    let frames = multiframe::split(&file, needed + usize::from(p.extra)).map_err(|e| anyhow!(e))?;
    eprintln!("{} bytes in {} frames (any {needed} rebuild the picture)", file.len(), frames.len());
    let count = frames.len();
    Ok(frames
        .into_iter()
        .enumerate()
        .map(|(i, f)| TxJob { label: format!("frame {} of {count}", i + 1), request: request(mode, f, false), gap_s: if i == 0 { 0.0 } else { 0.3 } })
        .collect())
}
