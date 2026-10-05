//! Where the receiver's signal comes from: a recording (WAV/FLAC, decoded as fast as
//! possible or paced to real time) or a sound-card input, brought to a rate the modems
//! work at and reduced to one real or one complex (I/Q) signal.

use cofdmtv_core::cofdmtv::RATES;
use cofdmtv_core::dsp::Cplx;
use cofdmtv_io::{AudioFormat, FileReader, InputOptions, InputStream, Resampler, ResamplerQuality};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The source of a receiver.
#[derive(Debug, Clone, PartialEq)]
pub enum InputSpec {
    /// A WAV or FLAC recording.
    File { path: PathBuf, realtime: bool },
    /// A sound-card input by name (or part of it); `None`: the system default.
    Device { name: Option<String> },
}

/// Which part of the input carries the signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelSel {
    /// The mean of all channels (also right for mono sources).
    #[default]
    Mix,
    Left,
    Right,
    /// I on the left channel, Q on the right (Shredpix's "analytic" output).
    Iq,
    /// Q on the left, I on the right.
    IqSwapped,
}

impl ChannelSel {
    pub const ALL: [ChannelSel; 5] = [Self::Mix, Self::Left, Self::Right, Self::Iq, Self::IqSwapped];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mix => "L+R",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Iq => "I/Q",
            Self::IqSwapped => "Q/I",
        }
    }

    pub fn is_iq(self) -> bool {
        matches!(self, Self::Iq | Self::IqSwapped)
    }

    /// The signal of one interleaved frame: real (in `re`) or complex.
    #[inline]
    pub fn pick(self, frame: &[f32]) -> Cplx {
        let l = frame[0];
        let r = frame.get(1).copied().unwrap_or(l);
        match self {
            Self::Mix => Cplx::new(frame.iter().sum::<f32>() / frame.len() as f32, 0.0),
            Self::Left => Cplx::new(l, 0.0),
            Self::Right => Cplx::new(r, 0.0),
            Self::Iq => Cplx::new(l, r),
            Self::IqSwapped => Cplx::new(r, l),
        }
    }
}

/// The rate a stream at `rate` is processed at: itself if the modems work at it,
/// otherwise 48 kHz (resampled).
pub fn processing_rate(rate: u32) -> u32 {
    if RATES.contains(&rate) { rate } else { 48_000 }
}

enum Kind {
    File { reader: FileReader, realtime: bool, started: Option<Instant>, read: u64 },
    Device(InputStream),
}

/// An open source.
pub struct Source {
    kind: Kind,
    format: AudioFormat,
    name: String,
    duration_s: Option<f64>,
    resampler: Option<Resampler>,
}

/// About a source, for the displays.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceInfo {
    /// File or device name.
    pub name: String,
    /// The input's own rate and channels.
    pub rate: u32,
    pub channels: usize,
    /// The rate the receiver runs at.
    pub processing_rate: u32,
    /// Length of a recording, seconds.
    pub duration_s: Option<f64>,
    pub is_file: bool,
}

impl Source {
    pub fn open(spec: &InputSpec, channel: ChannelSel) -> Result<Self, String> {
        let (kind, format, name, duration_s) = match spec {
            InputSpec::File { path, realtime } => {
                let reader = FileReader::open(path).map_err(|e| e.to_string())?;
                let format = reader.format();
                let duration = reader.duration().map(|d| d.as_secs_f64());
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                (Kind::File { reader, realtime: *realtime, started: None, read: 0 }, format, name, duration)
            }
            InputSpec::Device { name } => {
                let opts = InputOptions {
                    device: name.clone(),
                    sample_rate: None,
                    channels: if channel.is_iq() { Some(2) } else { None },
                    buffer: Duration::from_secs(4),
                };
                let stream = InputStream::open(&opts).map_err(|e| e.to_string())?;
                let format = stream.format();
                let name = stream.device_name().to_string();
                (Kind::Device(stream), format, name, None)
            }
        };
        if channel.is_iq() && format.channels < 2 {
            return Err(format!("{name} has one channel: I/Q needs two"));
        }
        let rate = processing_rate(format.sample_rate);
        let resampler = if rate == format.sample_rate {
            None
        } else {
            Some(Resampler::new(format.sample_rate, rate, format.channels, ResamplerQuality::High).map_err(|e| e.to_string())?)
        };
        Ok(Self { kind, format, name, duration_s, resampler })
    }

    pub fn info(&self) -> SourceInfo {
        SourceInfo {
            name: self.name.clone(),
            rate: self.format.sample_rate,
            channels: self.format.channels,
            processing_rate: processing_rate(self.format.sample_rate),
            duration_s: self.duration_s,
            is_file: matches!(self.kind, Kind::File { .. }),
        }
    }

    pub fn channels(&self) -> usize {
        self.format.channels
    }

    /// Seconds of input consumed (recordings).
    pub fn position_s(&self) -> f64 {
        match &self.kind {
            Kind::File { reader, .. } => reader.position() as f64 / f64::from(self.format.sample_rate),
            Kind::Device(_) => 0.0,
        }
    }

    /// The next block of interleaved samples at the processing rate (about `ms`
    /// milliseconds of input); `Ok(None)` at the end of a recording. Waits for a sound
    /// card, or to keep a recording in real time.
    pub fn read(&mut self, ms: u32) -> Result<Option<Vec<f32>>, String> {
        let frames = (self.format.sample_rate * ms / 1000).max(1) as usize;
        let block = match &mut self.kind {
            Kind::File { reader, realtime, started, read } => {
                if *realtime {
                    let start = *started.get_or_insert_with(Instant::now);
                    let due = Duration::from_secs_f64(*read as f64 / f64::from(self.format.sample_rate));
                    let elapsed = start.elapsed();
                    if due > elapsed {
                        std::thread::sleep(due - elapsed);
                    }
                }
                match reader.read(frames).map_err(|e| e.to_string())? {
                    Some(b) => {
                        *read += (b.len() / self.format.channels) as u64;
                        b
                    }
                    None => {
                        // The end: what the resampler still holds, then nothing more.
                        let rest = self.resampler.take().map(|mut r| r.flush()).unwrap_or_default();
                        return Ok((!rest.is_empty()).then_some(rest));
                    }
                }
            }
            Kind::Device(stream) => {
                let block = stream.read_blocking(frames, Duration::from_millis(500)).map_err(|e| e.to_string())?;
                if let Some(e) = stream.take_errors().into_iter().next() {
                    return Err(e.to_string());
                }
                block
            }
        };
        Ok(Some(match &mut self.resampler {
            Some(r) => r.process(&block),
            None => block,
        }))
    }
}
