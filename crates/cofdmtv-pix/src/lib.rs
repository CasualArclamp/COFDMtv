//! Pictures for COFDMTV, as Shredpix prepares them and Assempix shows them.
//!
//! A picture to send is scaled to at most a number of pixels (Shredpix's "1M" … "16K"
//! choices; each side 16…1024 pixels, which Assempix requires) and compressed — JPEG,
//! PNG, or WebP lossy/lossless — with the highest quality that fits the byte budget: one
//! payload (5380 bytes), or the blocks of a multi-frame transmission. A file that already
//! fits is sent unchanged. A picture still arriving is decoded as far as it has come
//! ([`decode_partial`]).

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{DynamicImage, ImageEncoder, ImageReader, RgbImage, RgbaImage};
use std::io::Cursor;
use std::path::Path;

mod partial;
pub use partial::{Partial, decode_partial};

/// Smallest and largest side Assempix accepts.
pub const MIN_SIDE: u32 = 16;
pub const MAX_SIDE: u32 = 1024;

/// Compression of a picture to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    WebpLossy,
    WebpLossless,
    Jpeg,
    Png,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::WebpLossy, Format::Jpeg, Format::WebpLossless, Format::Png];

    pub fn label(self) -> &'static str {
        match self {
            Format::WebpLossy => "WebP",
            Format::WebpLossless => "WebP lossless",
            Format::Jpeg => "JPEG",
            Format::Png => "PNG",
        }
    }

    pub fn is_lossy(self) -> bool {
        matches!(self, Format::WebpLossy | Format::Jpeg)
    }
}

/// Pixel budgets as Shredpix offers them.
pub const PIXEL_CHOICES: [(u32, &str); 7] = [
    (1 << 20, "1M"),
    (1 << 19, "512K"),
    (1 << 18, "256K"),
    (1 << 17, "128K"),
    (1 << 16, "64K"),
    (1 << 15, "32K"),
    (1 << 14, "16K"),
];

/// A picture loaded for sending.
#[derive(Clone)]
pub struct Source {
    /// Upright (EXIF orientation applied), alpha composited onto white.
    pub image: RgbImage,
    /// The file as read, if it is a JPEG, PNG or WebP that could be sent as is.
    pub original: Option<Vec<u8>>,
}

impl Source {
    pub fn open(path: &Path) -> Result<Source, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_bytes(bytes)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Source, String> {
        let reader = ImageReader::new(Cursor::new(&bytes)).with_guessed_format().map_err(|e| e.to_string())?;
        let sendable = matches!(reader.format(), Some(image::ImageFormat::Jpeg | image::ImageFormat::Png | image::ImageFormat::WebP));
        let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
        use image::ImageDecoder;
        let orientation = decoder.orientation().map_err(|e| e.to_string())?;
        let upright = orientation == image::metadata::Orientation::NoTransforms;
        let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
        img.apply_orientation(orientation);
        if img.width() < MIN_SIDE || img.height() < MIN_SIDE {
            return Err(format!("{}×{} pixels: pictures need at least {MIN_SIDE} on each side", img.width(), img.height()));
        }
        Ok(Source { image: flatten(img), original: (sendable && upright).then_some(bytes) })
    }

    /// From an RGBA picture (e.g. the clipboard).
    pub fn from_rgba(img: RgbaImage) -> Result<Source, String> {
        if img.width() < MIN_SIDE || img.height() < MIN_SIDE {
            return Err(format!("{}×{} pixels: pictures need at least {MIN_SIDE} on each side", img.width(), img.height()));
        }
        Ok(Source { image: flatten(DynamicImage::ImageRgba8(img)), original: None })
    }

    /// The original file if it can go as it is within `budget` bytes.
    pub fn original_if_fits(&self, budget: usize) -> Option<&[u8]> {
        let o = self.original.as_deref()?;
        let (w, h) = self.image.dimensions();
        (o.len() <= budget && (MIN_SIDE..=MAX_SIDE).contains(&w) && (MIN_SIDE..=MAX_SIDE).contains(&h)).then_some(o)
    }
}

/// Composite onto white and drop the alpha channel.
fn flatten(img: DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let rgba = img.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = u16::from(p[3]);
        let mix = |c: u8| ((u16::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])])
    })
}

/// The size of `img` scaled to at most `max_pixels`, each side within 16…1024, keeping
/// the aspect ratio (as far as those limits allow).
pub fn fitted_size(width: u32, height: u32, max_pixels: u32) -> (u32, u32) {
    let (w, h) = (f64::from(width), f64::from(height));
    let mut scale = (f64::from(max_pixels) / (w * h)).sqrt().min(1.0);
    scale = scale.min(f64::from(MAX_SIDE) / w).min(f64::from(MAX_SIDE) / h);
    let side = |v: f64| ((v * scale).floor() as u32).clamp(MIN_SIDE, MAX_SIDE);
    (side(w), side(h))
}

/// Scale for sending (Lanczos; never enlarges).
pub fn scale(img: &RgbImage, max_pixels: u32) -> RgbImage {
    let (w, h) = fitted_size(img.width(), img.height(), max_pixels);
    if (w, h) == img.dimensions() {
        return img.clone();
    }
    image::imageops::resize(img, w, h, image::imageops::FilterType::Lanczos3)
}

/// A compressed picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub format: Format,
    /// Quality used (lossy formats), 1…100.
    pub quality: Option<u8>,
    pub width: u32,
    pub height: u32,
}

/// Compress at one quality.
pub fn encode_at(img: &RgbImage, format: Format, quality: u8) -> Result<Vec<u8>, String> {
    let (w, h) = img.dimensions();
    match format {
        Format::Jpeg => {
            let mut out = Vec::new();
            JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100)).encode_image(img).map_err(|e| e.to_string())?;
            Ok(out)
        }
        Format::Png => {
            let mut out = Vec::new();
            PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Best, image::codecs::png::FilterType::Adaptive)
                .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
                .map_err(|e| e.to_string())?;
            Ok(out)
        }
        Format::WebpLossy => Ok(webp::Encoder::from_rgb(img.as_raw(), w, h).encode(f32::from(quality.clamp(0, 100))).to_vec()),
        Format::WebpLossless => Ok(webp::Encoder::from_rgb(img.as_raw(), w, h).encode_lossless().to_vec()),
    }
}

/// Compress with the highest quality whose file fits `budget` bytes (bisection over
/// 1…100, as Shredpix does).
pub fn encode_to_fit(img: &RgbImage, format: Format, budget: usize) -> Result<Encoded, String> {
    let (width, height) = img.dimensions();
    let done = |bytes: Vec<u8>, quality| Encoded { bytes, format, quality, width, height };
    if !format.is_lossy() {
        let bytes = encode_at(img, format, 100)?;
        return if bytes.len() <= budget {
            Ok(done(bytes, None))
        } else {
            Err(format!("{} as {}: {} bytes, more than {budget}", dims(img), format.label(), bytes.len()))
        };
    }
    let best = encode_at(img, format, 100)?;
    if best.len() <= budget {
        return Ok(done(best, Some(100)));
    }
    let worst = encode_at(img, format, 1)?;
    if worst.len() > budget {
        return Err(format!("{} as {}: {} bytes even at the lowest quality, more than {budget}", dims(img), format.label(), worst.len()));
    }
    let (mut lo, mut hi, mut found) = (1u8, 100u8, worst);
    let mut q_found = 1;
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let bytes = encode_at(img, format, mid)?;
        if bytes.len() <= budget {
            lo = mid;
            found = bytes;
            q_found = mid;
        } else {
            hi = mid;
        }
    }
    Ok(done(found, Some(q_found)))
}

fn dims(img: &RgbImage) -> String {
    format!("{}×{}", img.width(), img.height())
}

/// The largest of [`PIXEL_CHOICES`] (from `max_pixels` down) whose compressed picture fits
/// `budget` at a quality of at least `min_quality` (lossy) — or the smallest that fits at
/// all. Returns the pixel budget used and the result.
pub fn encode_auto(src: &RgbImage, format: Format, budget: usize, max_pixels: u32, min_quality: u8) -> Result<(u32, Encoded), String> {
    let mut fallback = None;
    let mut last_err = String::new();
    for (pixels, _) in PIXEL_CHOICES.iter().copied().filter(|(p, _)| *p <= max_pixels) {
        match encode_to_fit(&scale(src, pixels), format, budget) {
            Ok(e) if !format.is_lossy() || e.quality.unwrap_or(100) >= min_quality => return Ok((pixels, e)),
            Ok(e) => fallback = Some((pixels, e)),
            Err(e) => last_err = e,
        }
    }
    fallback.ok_or(last_err)
}

/// Decode a received picture for display (JPEG, PNG, WebP; `None` for others, e.g. AVIF).
pub fn decode(bytes: &[u8]) -> Option<RgbaImage> {
    image::load_from_memory(bytes).ok().map(|i| i.to_rgba8())
}

/// Size of a received picture without decoding it all.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_image(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, ((x ^ y) & 0xff) as u8]))
    }

    #[test]
    fn sizes_respect_the_limits() {
        assert_eq!(fitted_size(4000, 3000, 1 << 20), (1024, 768));
        assert_eq!(fitted_size(4000, 3000, 1 << 16), (295, 221));
        assert_eq!(fitted_size(100, 50, 1 << 20), (100, 50));
        assert_eq!(fitted_size(5000, 20, 1 << 14), (1024, 16));
    }

    #[test]
    fn lossy_formats_fit_the_budget() {
        let img = test_image(320, 240);
        for format in [Format::Jpeg, Format::WebpLossy] {
            let e = encode_to_fit(&img, format, 5380).unwrap();
            assert!(e.bytes.len() <= 5380, "{format:?}: {}", e.bytes.len());
            let q = e.quality.unwrap();
            if q < 100 {
                assert!(encode_at(&img, format, q + 1).unwrap().len() > 5380, "{format:?}: q {q} is the highest that fits");
            }
            let back = decode(&e.bytes).expect("decodes");
            assert_eq!(back.dimensions(), (320, 240));
        }
        assert!(encode_to_fit(&test_image(1024, 1024), Format::Png, 5380).is_err());
    }

    #[test]
    fn auto_picks_the_largest_good_size() {
        let img = test_image(1600, 1200);
        let (pixels, e) = encode_auto(&img, Format::WebpLossy, 5380, 1 << 20, 50).unwrap();
        assert!(e.quality.unwrap() >= 50 && e.bytes.len() <= 5380, "{pixels} {:?}", e.quality);
        assert!(pixels < 1 << 20);
    }
}
