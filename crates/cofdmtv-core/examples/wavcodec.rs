//! COFDMTV to and from WAV files, with the same command lines and output as the C++
//! reference programs in `tools/cxx-reference` (for cross-checks, see `scripts/xcheck.sh`).
//!
//! ```text
//! wavcodec encode OUT.wav RATE MODE CALLSIGN CARRIER NOISE FANCY CHANNEL [PAYLOAD...|TEXT]
//!     MODE: 0 (ping), 6..13 (picture payload files, sent one after the other), text
//!     (TEXT argument), textping
//!     CHANNEL: 0 mono, 1 first, 2 second, 4 analytic (I/Q)
//! wavcodec decode IN.wav OUTPREFIX [CHANNEL]
//!     CHANNEL: 0 mono, 1 first, 2 second, 3 sum, 4 analytic (I/Q)
//! wavcodec split INPUT FRAMES OUTPREFIX
//!     multi-frame (CRS) payloads OUTPREFIX_0.bin … of a file
//! wavcodec modem-encode OUT.wav RATE BITS CHANNELS OFFSET CALLSIGN MODULATION CODERATE FRAMESIZE INPUT...
//!     the aicodix modem, arguments as its `encode` takes them
//! wavcodec modem-decode IN.wav OUTPREFIX
//!     the aicodix modem: datagrams to OUTPREFIX_1.bin, …
//! wavcodec compare A.wav B.wav [SYMBOL]
//!     SAME if two 16-bit WAV files have the same format and length and their samples
//!     differ by at most 1 (rounding), else DIFF and where. With SYMBOL (samples of a
//!     symbol, all channels), differences confined to single symbols are "SAME except N
//!     symbols": the modem's encoders pick the scrambling with the lowest PAPR, and where
//!     two candidates (or one and the threshold) are within rounding of each other, the
//!     original (built with -ffast-math) and COFDMtv may pick differently.
//! ```

use cofdmtv_core::cofdmtv::multiframe::{self, Progress, Reassembler};
use cofdmtv_core::cofdmtv::{Decoder, Encoder, Event, Mode, PolarDecoders, TxRequest, decode_codeword};
use cofdmtv_core::dsp::Cplx;
use std::process::ExitCode;

fn encode(args: &[String]) -> Result<(), String> {
    let [out, rate, mode, call, carrier, noise, fancy, channel, rest @ ..] = args else {
        return Err("encode OUT.wav RATE MODE CALLSIGN CARRIER NOISE FANCY CHANNEL [PAYLOAD|TEXT]".into());
    };
    let num = |s: &String| s.parse::<i64>().map_err(|e| format!("{s}: {e}"));
    let rate = num(rate)? as u32;
    let channel = num(channel)?;
    let (mode, payloads, text_family) = match mode.as_str() {
        "text" => (Mode::Text(0), vec![rest.first().cloned().unwrap_or_default().into_bytes()], true),
        "textping" => (Mode::Ping, vec![Vec::new()], true),
        "0" => (Mode::Ping, vec![Vec::new()], false),
        m => {
            let n = m.parse::<u8>().map_err(|e| format!("{m}: {e}"))?;
            if rest.is_empty() {
                return Err("picture modes need PAYLOAD files".into());
            }
            let files = rest.iter().map(|f| std::fs::read(f).map_err(|e| format!("{f}: {e}"))).collect::<Result<_, _>>()?;
            (Mode::Image(n), files, false)
        }
    };
    let mut enc = Encoder::new(rate).ok_or(format!("unsupported rate {rate}"))?;
    let channels = if channel == 0 { 1 } else { 2 };
    let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(out, spec).map_err(|e| e.to_string())?;
    let q = |x: f32| (32767.0 * x).round_ties_even().clamp(-32768.0, 32767.0) as i16;
    let mut write = |z: Cplx| -> Result<(), String> {
        let frame: &[i16] = match channel {
            0 => &[q(z.re)],
            1 => &[q(z.re), 0],
            2 => &[0, q(z.re)],
            _ => &[q(z.re), q(z.im)],
        };
        for &s in frame {
            w.write_sample(s).map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    let mut symbols = 0;
    // A second of silence before, between and after the transmissions.
    for payload in payloads {
        for _ in 0..rate {
            write(Cplx::new(0.0, 0.0))?;
        }
        let req = TxRequest {
            mode,
            payload,
            call_sign: call.clone(),
            carrier_hz: num(carrier)? as i32,
            noise_symbols: num(noise)? as usize,
            fancy_header: num(fancy)? != 0,
            text_family,
        };
        enc.configure(&req)?;
        while let Some(sym) = enc.next_symbol() {
            for &z in sym.to_vec().iter() {
                write(z)?;
            }
            symbols += 1;
        }
    }
    for _ in 0..rate {
        write(Cplx::new(0.0, 0.0))?;
    }
    w.finalize().map_err(|e| e.to_string())?;
    eprintln!("wrote {symbols} symbols to {out}");
    Ok(())
}

fn decode(args: &[String]) -> Result<(), String> {
    let [input, prefix, rest @ ..] = args else {
        return Err("decode IN.wav OUTPREFIX [CHANNEL]".into());
    };
    let mut r = hound::WavReader::open(input).map_err(|e| format!("{input}: {e}"))?;
    let spec = r.spec();
    let channels = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => r.samples::<i32>().map(|s| s.map(|v| v as f32 / (1u32 << (spec.bits_per_sample - 1)) as f32)).collect::<Result<_, _>>(),
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>(),
    }
    .map_err(|e| e.to_string())?;
    let channel: i64 = match rest.first() {
        Some(c) => c.parse().map_err(|e| format!("{c}: {e}"))?,
        None if channels > 1 => 1,
        None => 0,
    };
    let mut dec = Decoder::new(spec.sample_rate).ok_or(format!("unsupported rate {}", spec.sample_rate))?;
    let mut polar = PolarDecoders::default();
    let mut frames = Reassembler::default();
    let mut decoded = 0;
    for (n, frame) in samples.chunks_exact(channels).enumerate() {
        let second = frame.get(1).copied().unwrap_or(frame[0]);
        let ready = match channel {
            1 => dec.push_real(frame[0]),
            2 => dec.push_real(second),
            3 => dec.push_real((frame[0] + second) / 2.0),
            4 => dec.push(Cplx::new(frame[0], second)),
            _ => dec.push_real(frame[0]),
        };
        if !ready {
            continue;
        }
        match dec.process() {
            Some(Event::PreambleFailed) => println!("FAIL at={n}"),
            Some(Event::Unsupported { mode, call, cfo_hz }) => println!("NOPE cfo={cfo_hz} mode={mode} call={call}"),
            Some(Event::Ping { call, cfo_hz }) => println!("PING cfo={cfo_hz} call={call}"),
            Some(Event::Sync { mode, call, cfo_hz }) => println!("SYNC at={n} cfo={cfo_hz} mode={} call={call}", mode.number()),
            Some(Event::Done) => {
                let cw = dec.take_codeword().expect("codeword after Done");
                match decode_codeword(&cw, &mut polar) {
                    None => println!("DONE failed"),
                    Some(p) if matches!(p.mode, Mode::Text(_)) => {
                        println!("DONE flips={} text={}", p.flips, String::from_utf8_lossy(&p.data));
                    }
                    Some(p) => {
                        decoded += 1;
                        let name = format!("{prefix}_{decoded}.bin");
                        std::fs::write(&name, &p.data).map_err(|e| format!("{name}: {e}"))?;
                        println!("DONE flips={} snr={:.1}dB -> {name}", p.flips, p.snr_db);
                        match frames.push(&p.data) {
                            Progress::NotMultiFrame => {}
                            Progress::Partial { have, need } => println!("CRS chunk have={have}/{need}"),
                            Progress::Complete(file) => {
                                let name = format!("{prefix}_crs.bin");
                                std::fs::write(&name, &file).map_err(|e| format!("{name}: {e}"))?;
                                println!("CRS complete bytes={} -> {name}", file.len());
                            }
                            other => println!("CRS {other:?}"),
                        }
                    }
                }
            }
            None => {}
        }
    }
    Ok(())
}

fn split(args: &[String]) -> Result<(), String> {
    let [input, frames, prefix] = args else {
        return Err("split INPUT FRAMES OUTPREFIX".into());
    };
    let file = std::fs::read(input).map_err(|e| format!("{input}: {e}"))?;
    let frames: usize = frames.parse().map_err(|e| format!("{frames}: {e}"))?;
    for (i, f) in multiframe::split(&file, frames)?.iter().enumerate() {
        let name = format!("{prefix}_{i}.bin");
        std::fs::write(&name, f).map_err(|e| format!("{name}: {e}"))?;
    }
    eprintln!("{} blocks in {frames} frames", multiframe::blocks_for(file.len()));
    Ok(())
}

fn modem_encode(args: &[String]) -> Result<(), String> {
    use cofdmtv_core::modem::{CodeRate, ModemEncoder, ModemMode, ModemRequest, Modulation};
    let [out, rate, bits, channels, offset, call, modulation, code_rate, frame_size, inputs @ ..] = args else {
        return Err("modem-encode OUT.wav RATE BITS CHANNELS OFFSET CALLSIGN MODULATION CODERATE FRAMESIZE INPUT...".into());
    };
    let num = |s: &String| s.parse::<i64>().map_err(|e| format!("{s}: {e}"));
    let rate = num(rate)? as u32;
    let channels = num(channels)? as u16;
    let modulation = Modulation::ALL.into_iter().find(|m| m.name() == modulation).ok_or(format!("modulation {modulation}"))?;
    let code_rate = CodeRate::ALL.into_iter().find(|r| r.name() == code_rate).ok_or(format!("code rate {code_rate}"))?;
    let normal = match frame_size.as_str() {
        "short" => false,
        "normal" => true,
        f => return Err(format!("frame size {f}")),
    };
    let mode = ModemMode { modulation, rate: code_rate, normal };
    let frames = inputs
        .iter()
        .map(|f| std::fs::read(f).map(|mut d| {
            d.resize(mode.data_bytes(), 0);
            d
        }).map_err(|e| format!("{f}: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    let mut enc = ModemEncoder::new(rate).ok_or(format!("unsupported rate {rate}"))?;
    enc.configure(&ModemRequest { mode, call_sign: call.clone(), carrier_hz: num(offset)? as i32, frames })?;
    let float = num(bits)? == 32;
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: if float { 32 } else { 16 },
        sample_format: if float { hound::SampleFormat::Float } else { hound::SampleFormat::Int },
    };
    let mut w = hound::WavWriter::create(out, spec).map_err(|e| e.to_string())?;
    let mut write = |z: Cplx| -> Result<(), String> {
        for (i, v) in [z.re, z.im].into_iter().enumerate().take(usize::from(channels)) {
            let _ = i;
            if float {
                w.write_sample(v).map_err(|e| e.to_string())?;
            } else {
                w.write_sample((32767.0 * v).round_ties_even().clamp(-32768.0, 32767.0) as i16).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    };
    for _ in 0..rate {
        write(Cplx::new(0.0, 0.0))?;
    }
    while let Some(chunk) = enc.next_chunk() {
        for &z in chunk.to_vec().iter() {
            write(z)?;
        }
    }
    for _ in 0..rate {
        write(Cplx::new(0.0, 0.0))?;
    }
    w.finalize().map_err(|e| e.to_string())?;
    let papr = &enc.papr_db;
    if !papr.is_empty() {
        let mut p = papr.clone();
        p.sort_by(f32::total_cmp);
        eprintln!("{}: PAPR {:.1} .. {:.1} .. {:.1} dB", mode.label(), p[0], p[p.len() / 2], p[p.len() - 1]);
    }
    Ok(())
}

fn modem_decode(args: &[String]) -> Result<(), String> {
    use cofdmtv_core::modem::{ModemDecoder, ModemEvent, decode_datagram};
    let [input, prefix] = args else {
        return Err("modem-decode IN.wav OUTPREFIX".into());
    };
    let mut r = hound::WavReader::open(input).map_err(|e| format!("{input}: {e}"))?;
    let spec = r.spec();
    let channels = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => r.samples::<i32>().map(|s| s.map(|v| v as f32 / (1u32 << (spec.bits_per_sample - 1)) as f32)).collect::<Result<_, _>>(),
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>(),
    }
    .map_err(|e| e.to_string())?;
    let mut dec = ModemDecoder::new(spec.sample_rate).ok_or(format!("unsupported rate {}", spec.sample_rate))?;
    let mut decoder = None;
    let mut n = 0;
    for frame in samples.chunks_exact(channels) {
        let ev = if channels == 1 { dec.push_real(frame[0]) } else { dec.push(Cplx::new(frame[0], frame[1])) };
        match ev {
            Some(ModemEvent::Sync { mode, call, cfo_hz }) => println!("SYNC cfo={cfo_hz:.1} mode={} call={call}", mode.label()),
            Some(ModemEvent::MetaFailed) => println!("META failed"),
            Some(ModemEvent::Lost) => println!("LOST"),
            Some(ModemEvent::Done) => {
                let cw = dec.take_codeword().expect("codeword");
                match decode_datagram(&cw, &mut decoder) {
                    Some(d) => {
                        n += 1;
                        let name = format!("{prefix}_{n}.bin");
                        std::fs::write(&name, &d.data).map_err(|e| format!("{name}: {e}"))?;
                        println!("DONE Es/N0={:.1}..{:.1}..{:.1}dB -> {name}", d.snr_db.0, d.snr_db.1, d.snr_db.2);
                    }
                    None => println!("DONE failed"),
                }
            }
            None => {}
        }
    }
    Ok(())
}

fn compare(args: &[String]) -> Result<(), String> {
    let (a, b, symbol) = match args {
        [a, b] => (a, b, None),
        [a, b, s] => (a, b, Some(s.parse::<usize>().map_err(|e| format!("{s}: {e}"))?)),
        _ => return Err("compare A.wav B.wav [SYMBOL]".into()),
    };
    let read = |path: &String| -> Result<(hound::WavSpec, Vec<i32>), String> {
        let mut r = hound::WavReader::open(path).map_err(|e| format!("{path}: {e}"))?;
        let spec = r.spec();
        let samples = r.samples::<i32>().collect::<Result<Vec<_>, _>>().map_err(|e| format!("{path}: {e}"))?;
        Ok((spec, samples))
    };
    let ((sa, a), (sb, b)) = (read(a)?, read(b)?);
    if sa != sb || a.len() != b.len() {
        println!("DIFF format or length: {sa:?} {} samples, {sb:?} {} samples", a.len(), b.len());
        return Ok(());
    }
    let (at, max) = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).enumerate().max_by_key(|&(_, d)| d).unwrap_or((0, 0));
    let off = a.iter().zip(&b).filter(|(x, y)| x != y).count();
    if max <= 1 {
        println!("SAME max_diff={max} differing={off} of {}", a.len());
        return Ok(());
    }
    if let Some(symbol) = symbol {
        // Runs of differences beyond rounding, split where a quarter symbol is alike.
        let big: Vec<usize> = a.iter().zip(&b).enumerate().filter(|(_, (x, y))| (*x - *y).abs() > 1).map(|(i, _)| i).collect();
        let mut runs = vec![(big[0], big[0])];
        for &i in &big[1..] {
            let last = runs.last_mut().expect("one run at least");
            if i - last.1 > symbol / 4 {
                runs.push((i, i));
            } else {
                last.1 = i;
            }
        }
        if runs.iter().all(|(s, e)| e - s < symbol + symbol / 8) {
            let at: Vec<String> = runs.iter().map(|(s, _)| s.to_string()).collect();
            println!("SAME except {} symbol{} (at sample {})", runs.len(), if runs.len() == 1 { "" } else { "s" }, at.join(", "));
            return Ok(());
        }
    }
    println!("DIFF max_diff={max} at sample {at} (of {}), {off} differing", a.len());
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("encode") => encode(&args[1..]),
        Some("decode") => decode(&args[1..]),
        Some("split") => split(&args[1..]),
        Some("modem-encode") => modem_encode(&args[1..]),
        Some("modem-decode") => modem_decode(&args[1..]),
        Some("compare") => compare(&args[1..]),
        _ => Err("usage: wavcodec encode … | wavcodec decode …".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
