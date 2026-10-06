//! Persistent GUI settings: the last source, the transmitter's form, view choices, stored
//! as TOML in the per-user configuration directory: `%APPDATA%\cofdmtv\gui.toml` on
//! Windows, `$XDG_CONFIG_HOME/cofdmtv/gui.toml` (or `~/.config/cofdmtv/gui.toml`)
//! elsewhere.
//!
//! Rust notes: `#[derive(Serialize, Deserialize)]` makes serde generate the TOML
//! (de)serialisation code at compile time. `#[serde(default)]` on the struct fills every
//! field missing from the file with its `Default` value, so settings files written by
//! older versions still load after new fields are added (and unknown fields from newer
//! versions are ignored).

use cofdmtv_engine::cofdmtv_core::modem::{self, CodeRate, ModemMode, Modulation};
use cofdmtv_engine::{ChannelSel, TxChannel};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Top-level page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Page {
    #[default]
    Receiver,
    Transmitter,
}

/// Colour theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeChoice {
    /// Follow the operating system.
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [Self::System, Self::Dark, Self::Light];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System theme",
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }

    pub fn preference(self) -> eframe::egui::ThemePreference {
        use eframe::egui::ThemePreference;
        match self {
            Self::System => ThemePreference::System,
            Self::Dark => ThemePreference::Dark,
            Self::Light => ThemePreference::Light,
        }
    }
}

/// Where the receiver's signal comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    #[default]
    Device,
    File,
}

/// The receiver's channel choice (maps to [`ChannelSel`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RxChannel {
    #[default]
    Mix,
    Left,
    Right,
    Iq,
    Qi,
}

impl RxChannel {
    pub const ALL: [RxChannel; 5] = [Self::Mix, Self::Left, Self::Right, Self::Iq, Self::Qi];

    pub fn sel(self) -> ChannelSel {
        match self {
            Self::Mix => ChannelSel::Mix,
            Self::Left => ChannelSel::Left,
            Self::Right => ChannelSel::Right,
            Self::Iq => ChannelSel::Iq,
            Self::Qi => ChannelSel::IqSwapped,
        }
    }

    pub fn label(self) -> &'static str {
        self.sel().label()
    }
}

/// Tab of the plot area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlotTab {
    #[default]
    Overview,
    Spectrum,
    Waterfall,
    Constellation,
}

impl PlotTab {
    pub const ALL: [PlotTab; 4] = [Self::Overview, Self::Spectrum, Self::Waterfall, Self::Constellation];

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Spectrum => "Spectrum",
            Self::Waterfall => "Waterfall",
            Self::Constellation => "Constellation",
        }
    }
}

/// Frequency span of the spectrum and the waterfall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Span {
    /// 0–4 kHz (±4 kHz for I/Q): where the apps put their signals.
    #[default]
    Audio,
    /// 0–8 kHz (±8 kHz).
    Wide,
    /// The whole band of the input.
    Full,
}

impl Span {
    pub const ALL: [Span; 3] = [Self::Audio, Self::Wide, Self::Full];

    pub fn label(self) -> &'static str {
        match self {
            Self::Audio => "4 kHz",
            Self::Wide => "8 kHz",
            Self::Full => "Full band",
        }
    }

    /// The span (Hz) for an input at `rate`, real or I/Q.
    pub fn range(self, rate: u32, iq: bool) -> (f64, f64) {
        let nyquist = f64::from(rate) / 2.0;
        let top = match self {
            Self::Audio => 4000.0_f64.min(nyquist),
            Self::Wide => 8000.0_f64.min(nyquist),
            Self::Full => nyquist,
        };
        if iq { (-top, top) } else { (0.0, top) }
    }
}

/// What the side panel shows below the latest picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SideTab {
    #[default]
    Pictures,
    Messages,
    Files,
}

/// What the transmitter sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TxKind {
    #[default]
    Picture,
    Text,
    Ping,
    /// Modem datagrams: a text or a file.
    Data,
}

impl TxKind {
    pub const ALL: [TxKind; 4] = [Self::Picture, Self::Text, Self::Ping, Self::Data];

    pub fn label(self) -> &'static str {
        match self {
            Self::Picture => "Picture",
            Self::Text => "Text",
            Self::Ping => "Ping",
            Self::Data => "Data",
        }
    }
}

/// What the modem sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DataSource {
    /// The message (one datagram).
    #[default]
    Text,
    /// A file, in as many frames as it takes.
    File,
}

/// A [`ModemMode`] in the settings file by its name, "QAM16 1/2 short".
mod modem_mode_name {
    use cofdmtv_engine::cofdmtv_core::modem::ModemMode;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(mode: &ModemMode, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&mode.label())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<ModemMode, D::Error> {
        let name = String::deserialize(d)?;
        ModemMode::all().find(|m| m.label().eq_ignore_ascii_case(name.trim())).ok_or_else(|| D::Error::custom(format!("unknown modem mode \"{name}\"")))
    }
}

/// Compression of the picture to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PicFormat {
    #[default]
    Webp,
    Jpeg,
    WebpLossless,
    Png,
}

impl PicFormat {
    pub const ALL: [PicFormat; 4] = [Self::Webp, Self::Jpeg, Self::WebpLossless, Self::Png];

    pub fn format(self) -> cofdmtv_pix::Format {
        match self {
            Self::Webp => cofdmtv_pix::Format::WebpLossy,
            Self::Jpeg => cofdmtv_pix::Format::Jpeg,
            Self::WebpLossless => cofdmtv_pix::Format::WebpLossless,
            Self::Png => cofdmtv_pix::Format::Png,
        }
    }

    pub fn label(self) -> &'static str {
        self.format().label()
    }
}

/// Pixel budget of the picture to send: automatic, or one of Shredpix's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pixels {
    /// The largest that keeps a decent quality.
    #[default]
    Auto,
    Max(u32),
}

impl Pixels {
    pub fn label(self) -> String {
        match self {
            Self::Auto => "Auto".into(),
            Self::Max(n) => cofdmtv_pix::PIXEL_CHOICES.iter().find(|(p, _)| *p == n).map_or_else(|| format!("{n}"), |(_, l)| (*l).to_string()),
        }
    }
}

/// Where the transmitter's signal goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TxOutputKind {
    #[default]
    Device,
    File,
}

/// The transmitter's channel choice (maps to [`TxChannel`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TxChannelChoice {
    #[default]
    Mono,
    Left,
    Right,
    Iq,
}

impl TxChannelChoice {
    pub const ALL: [TxChannelChoice; 4] = [Self::Mono, Self::Left, Self::Right, Self::Iq];

    pub fn channel(self) -> TxChannel {
        match self {
            Self::Mono => TxChannel::Mono,
            Self::Left => TxChannel::Left,
            Self::Right => TxChannel::Right,
            Self::Iq => TxChannel::Iq,
        }
    }

    pub fn label(self) -> &'static str {
        self.channel().label()
    }
}

/// Lead-in noise choices as Shredpix offers them (symbols of 180 ms).
pub const NOISE_CHOICES: [(usize, &str); 6] = [(0, "none"), (1, "¼ s"), (3, "½ s"), (6, "1 s"), (11, "2 s"), (22, "4 s")];

/// Everything the GUI remembers between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub page: Page,
    pub theme: ThemeChoice,
    pub show_log: bool,
    // Receiver.
    pub source: SourceKind,
    pub file: Option<PathBuf>,
    /// Sound-card input by name (`None` = system default).
    pub input_device: Option<String>,
    pub channel: RxChannel,
    /// Pace recordings to real time.
    pub realtime: bool,
    /// Save received pictures (and the messages) here; `None`: not saved.
    pub save_dir: Option<PathBuf>,
    pub plot_tab: PlotTab,
    pub span: Span,
    pub side_tab: SideTab,
    // Transmitter.
    pub call_sign: String,
    pub tx_kind: TxKind,
    /// Picture mode, 6…13.
    pub tx_mode: u8,
    pub carrier_hz: i32,
    /// Lead-in noise, symbols.
    pub noise_symbols: usize,
    pub fancy_header: bool,
    pub picture: Option<PathBuf>,
    pub format: PicFormat,
    pub pixels: Pixels,
    /// Data blocks of a picture: 1 = one frame; more = multi-frame.
    pub blocks: u8,
    /// Extra frames of a multi-frame picture.
    pub extra_frames: u8,
    /// Send a picture file unchanged when it fits.
    pub send_as_is: bool,
    /// The message (Rattlegram, or a modem datagram).
    pub text: String,
    /// Modem: mode, carrier (a multiple of 300 Hz), a text or a file.
    #[serde(with = "modem_mode_name")]
    pub data_mode: ModemMode,
    pub data_carrier_hz: i32,
    pub data_source: DataSource,
    pub data_file: Option<PathBuf>,
    /// Pictures in a v2 mode: modem frames in `v2_mode` at the modem's carrier
    /// (`data_carrier_hz`), compressed to fill `v2_air_s` seconds on the air.
    pub picture_v2: bool,
    #[serde(with = "modem_mode_name")]
    pub v2_mode: ModemMode,
    pub v2_air_s: u32,
    pub tx_output: TxOutputKind,
    pub tx_device: Option<String>,
    pub tx_file: Option<PathBuf>,
    /// Sample rate to build the signal at (`None`: automatic).
    pub tx_rate: Option<u32>,
    pub tx_channel: TxChannelChoice,
    pub tx_gain_db: f32,
    /// Allow carriers above 3 kHz (Shredpix's "ultrasonic" option).
    pub ultrasonic: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            page: Page::Receiver,
            theme: ThemeChoice::System,
            show_log: true,
            source: SourceKind::Device,
            file: None,
            input_device: None,
            channel: RxChannel::Mix,
            realtime: true,
            save_dir: default_save_dir(),
            plot_tab: PlotTab::Overview,
            span: Span::Audio,
            side_tab: SideTab::Pictures,
            call_sign: "ANONYMOUS".into(),
            tx_kind: TxKind::Picture,
            tx_mode: 11,
            carrier_hz: 1700,
            noise_symbols: 6,
            fancy_header: true,
            picture: None,
            format: PicFormat::Webp,
            pixels: Pixels::Auto,
            blocks: 1,
            extra_frames: 1,
            send_as_is: true,
            text: String::new(),
            data_mode: ModemMode { modulation: Modulation::Qam16, rate: CodeRate::Half, normal: false },
            data_carrier_hz: modem::DEFAULT_CARRIER_HZ,
            data_source: DataSource::Text,
            data_file: None,
            picture_v2: false,
            v2_mode: ModemMode { modulation: Modulation::Qam16, rate: CodeRate::Half, normal: true },
            v2_air_s: 30,
            tx_output: TxOutputKind::Device,
            tx_device: None,
            tx_file: None,
            tx_rate: None,
            tx_channel: TxChannelChoice::Mono,
            tx_gain_db: 0.0,
            ultrasonic: false,
        }
    }
}

impl Settings {
    /// What is set up goes over the aicodix modem (data, or a picture in a v2 mode) rather
    /// than COFDMTV.
    pub fn modem_signal(&self) -> bool {
        self.tx_kind == TxKind::Data || (self.tx_kind == TxKind::Picture && self.picture_v2)
    }
}

/// Air times offered for v2 pictures, seconds.
pub const AIR_CHOICES: [u32; 10] = [10, 15, 20, 30, 45, 60, 90, 120, 180, 300];

/// `Pictures\COFDMtv` in the user's home (as Assempix saves to Pictures).
pub fn default_save_dir() -> Option<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join("Pictures").join("COFDMtv"))
}

/// Default settings-file location (see the module docs).
pub fn default_config_path() -> Option<PathBuf> {
    config_path_for(cfg!(windows), |key| std::env::var_os(key))
}

/// Settings-file location for a platform, with the environment passed in as a lookup
/// function so tests do not depend on the real environment.
fn config_path_for(windows: bool, env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let non_empty = |key: &str| env(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if windows {
        non_empty("APPDATA")?
    } else {
        non_empty("XDG_CONFIG_HOME").or_else(|| non_empty("HOME").map(|h| h.join(".config")))?
    };
    Some(base.join("cofdmtv").join("gui.toml"))
}

/// Loads and saves [`Settings`], writing only when they changed.
pub struct SettingsStore {
    path: Option<PathBuf>,
    saved: Option<Settings>,
}

impl SettingsStore {
    /// Load from `path` (or the default location). A missing file gives the defaults;
    /// an unreadable one gives the defaults plus a warning for the log.
    pub fn load(path: Option<PathBuf>) -> (Self, Settings, Option<String>) {
        let path = path.or_else(default_config_path);
        let Some(p) = path.clone() else {
            return (Self { path, saved: None }, Settings::default(), Some("no settings directory found".into()));
        };
        match std::fs::read_to_string(&p) {
            Ok(text) => match toml::from_str::<Settings>(&text) {
                Ok(s) => (Self { path, saved: Some(s.clone()) }, s, None),
                Err(e) => {
                    let warn = format!("ignoring unreadable settings file {}: {e}", p.display());
                    (Self { path, saved: None }, Settings::default(), Some(warn))
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self { path, saved: None }, Settings::default(), None),
            Err(e) => {
                let warn = format!("cannot read settings file {}: {e}", p.display());
                (Self { path, saved: None }, Settings::default(), Some(warn))
            }
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// `true` if `settings` differ from what was last loaded or saved.
    pub fn is_dirty(&self, settings: &Settings) -> bool {
        self.saved.as_ref() != Some(settings)
    }

    /// Write `settings` if they changed, through a temporary file and a rename so a
    /// crash cannot leave a truncated file.
    pub fn save_if_changed(&mut self, settings: &Settings) -> Result<(), String> {
        if !self.is_dirty(settings) {
            return Ok(());
        }
        let Some(path) = self.path.clone() else { return Ok(()) };
        // Remember the attempt even if it fails, so a read-only directory does not cause
        // a retry (and an error message) on every frame.
        self.saved = Some(settings.clone());
        let text = toml::to_string_pretty(settings).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("replacing {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_round_trip() {
        let s = Settings {
            source: SourceKind::File,
            file: Some(PathBuf::from("recording.wav")),
            channel: RxChannel::Iq,
            pixels: Pixels::Max(1 << 16),
            tx_rate: Some(8000),
            text: "Grüße".into(),
            tx_kind: TxKind::Data,
            data_mode: ModemMode { modulation: Modulation::Psk8, rate: CodeRate::FiveSixths, normal: true },
            data_source: DataSource::File,
            picture_v2: true,
            v2_mode: ModemMode { modulation: Modulation::Qam256, rate: CodeRate::TwoThirds, normal: false },
            v2_air_s: 60,
            ..Settings::default()
        };
        assert!(toml::to_string_pretty(&s).unwrap().contains("data_mode = \"8PSK 5/6 normal\""));
        let text = toml::to_string_pretty(&s).unwrap();
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), s);
        // Older files lack fields: defaults fill them.
        let old: Settings = toml::from_str("call_sign = \"DL1ABC\"\n").unwrap();
        assert_eq!(old.call_sign, "DL1ABC");
        assert_eq!(old.tx_mode, 11);
        assert_eq!(old.data_mode.label(), "QAM16 1/2 short");
        assert!(toml::from_str::<Settings>("data_mode = \"QAM17 1/2 short\"\n").is_err());
    }

    #[test]
    fn config_locations() {
        let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(*v));
        assert_eq!(
            config_path_for(true, env(&[("APPDATA", r"C:\Users\me\AppData\Roaming")])),
            Some(PathBuf::from(r"C:\Users\me\AppData\Roaming").join("cofdmtv").join("gui.toml"))
        );
        assert_eq!(config_path_for(false, env(&[("HOME", "/home/me")])), Some(PathBuf::from("/home/me/.config/cofdmtv/gui.toml")));
        assert_eq!(config_path_for(true, env(&[])), None);
    }

    #[test]
    fn spans() {
        assert_eq!(Span::Audio.range(48000, false), (0.0, 4000.0));
        assert_eq!(Span::Wide.range(8000, true), (-4000.0, 4000.0));
        assert_eq!(Span::Full.range(44100, false), (0.0, 22050.0));
    }
}
