//! What arrived: pictures recognised by their file signature and cut free of the payload's
//! zero padding, text messages, and saving them the way Assempix names its pictures
//! (`yyyyMMdd_HHmmss_CALL.ext`).

use chrono::{DateTime, Local};
use std::path::{Path, PathBuf};

/// Picture formats COFDMTV carries (Assempix accepts JPEG, PNG, WebP and AVIF).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageKind {
    Jpeg,
    Png,
    Webp,
    Avif,
}

impl ImageKind {
    /// Recognise a picture by its first bytes.
    pub fn sniff(data: &[u8]) -> Option<ImageKind> {
        if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(ImageKind::Jpeg)
        } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(ImageKind::Png)
        } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            Some(ImageKind::Webp)
        } else if data.len() >= 12 && &data[4..8] == b"ftyp" && (&data[8..12] == b"avif" || &data[8..12] == b"avis" || &data[8..12] == b"mif1") {
            Some(ImageKind::Avif)
        } else {
            None
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ImageKind::Jpeg => "jpg",
            ImageKind::Png => "png",
            ImageKind::Webp => "webp",
            ImageKind::Avif => "avif",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ImageKind::Jpeg => "JPEG",
            ImageKind::Png => "PNG",
            ImageKind::Webp => "WebP",
            ImageKind::Avif => "AVIF",
        }
    }

    /// The picture file within a zero-padded payload.
    pub fn trim(self, data: &[u8]) -> &[u8] {
        let end = match self {
            ImageKind::Webp => {
                let riff = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
                (8 + riff).min(data.len())
            }
            ImageKind::Avif => {
                // Top-level ISO BMFF boxes, one after the other.
                let mut at = 0;
                while at + 8 <= data.len() {
                    let size = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
                    if size < 8 || at + size > data.len() {
                        break;
                    }
                    at += size;
                }
                if at == 0 { trim_zeros(data) } else { at }
            }
            // A JPEG ends with EOI (FF D9), a PNG with IEND's CRC: neither ends with zeros.
            ImageKind::Jpeg | ImageKind::Png => trim_zeros(data),
        };
        &data[..end]
    }
}

fn trim_zeros(data: &[u8]) -> usize {
    data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1)
}

/// A received picture.
#[derive(Debug, Clone)]
pub struct Picture {
    pub time: DateTime<Local>,
    pub call: String,
    /// How it came: "mode 11 (8PSK, 2400 Hz)", "modem QAM16 1/2 short".
    pub label: String,
    /// Frames it took (1, or the blocks of a multi-frame picture).
    pub frames: usize,
    pub kind: Option<ImageKind>,
    /// The file.
    pub data: Vec<u8>,
    /// Bits the decoder corrected (COFDMTV reports them).
    pub flips: Option<u32>,
    pub snr_db: f32,
    pub cfo_hz: f32,
    /// Where it was saved.
    pub saved: Option<PathBuf>,
}

/// A received text message (Rattlegram, or a modem datagram that reads as text).
#[derive(Debug, Clone)]
pub struct TextMessage {
    pub time: DateTime<Local>,
    pub call: String,
    pub label: String,
    pub text: String,
    pub flips: Option<u32>,
    pub snr_db: f32,
    pub cfo_hz: f32,
}

/// A received modem datagram or file that is neither text nor a picture.
#[derive(Debug, Clone)]
pub struct ReceivedFile {
    pub time: DateTime<Local>,
    pub call: String,
    pub label: String,
    /// Frames it took.
    pub frames: usize,
    pub data: Vec<u8>,
    pub snr_db: f32,
    pub cfo_hz: f32,
    pub saved: Option<PathBuf>,
}

/// The picture of a decoded image payload: its file cut from the padding.
pub fn picture_bytes(payload: &[u8]) -> (Option<ImageKind>, Vec<u8>) {
    let kind = ImageKind::sniff(payload);
    let data = match kind {
        Some(k) => k.trim(payload).to_vec(),
        None => payload[..trim_zeros(payload)].to_vec(),
    };
    (kind, data)
}

/// File name for a picture received at `time` from `call`: `20261005_213000_DL1ABC.jpg`.
pub fn file_name(time: &DateTime<Local>, call: &str, kind: Option<ImageKind>) -> String {
    let call: String = call.trim().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let ext = kind.map_or("bin", ImageKind::extension);
    format!("{}_{}.{ext}", time.format("%Y%m%d_%H%M%S"), if call.is_empty() { "UNKNOWN".into() } else { call })
}

/// Save `data` in `dir` as `name`, adding `-2`, `-3`… to the stem if the name is taken.
pub fn save_unique(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    for n in 1.. {
        let candidate = if n == 1 { name.to_string() } else { format!("{stem}-{n}.{ext}") };
        let path = dir.join(candidate);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                std::io::Write::write_all(&mut f, data)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Append a text message to `dir/messages.txt`.
pub fn log_message(dir: &Path, msg: &TextMessage) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("messages.txt"))?;
    let line = format!("{} {}: {}\n", msg.time.format("%Y-%m-%d %H:%M:%S"), msg.call, msg.text.replace('\n', " "));
    std::io::Write::write_all(&mut f, line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_recognised_and_trimmed() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 0, 3, 0xFF, 0xD9];
        jpeg.resize(64, 0);
        let (kind, data) = picture_bytes(&jpeg);
        assert_eq!(kind, Some(ImageKind::Jpeg));
        assert_eq!(data.len(), 10);
        let mut webp = b"RIFF\x0a\x00\x00\x00WEBPVP8 \x00\x00".to_vec();
        webp.resize(40, 0);
        assert_eq!(picture_bytes(&webp), (Some(ImageKind::Webp), webp[..18].to_vec()));
        let (kind, data) = picture_bytes(&[1, 2, 3, 0, 0]);
        assert_eq!((kind, data), (None, vec![1, 2, 3]));
    }

    #[test]
    fn names_are_unique() {
        let dir = tempfile::tempdir().unwrap();
        let t = Local::now();
        let name = file_name(&t, "  DL1ABC", Some(ImageKind::Png));
        assert!(name.ends_with("_DL1ABC.png"));
        let a = save_unique(dir.path(), &name, b"a").unwrap();
        let b = save_unique(dir.path(), &name, b"b").unwrap();
        assert_ne!(a, b);
        assert!(b.to_string_lossy().ends_with("-2.png"));
    }
}
