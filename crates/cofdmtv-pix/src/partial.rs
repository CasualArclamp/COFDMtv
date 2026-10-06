//! Pictures shown as they arrive: the beginning of a WebP, JPEG or PNG file decoded as far
//! as it goes. v2 pictures come in frames that carry the file in order, so the receiver
//! has its first bytes while the rest is on the way.
//!
//! WebP (lossy and lossless) goes through libwebp's incremental decoder, which gives rows
//! from the top once the headers are in — for a lossy picture that includes the first
//! partition (every macroblock's prediction modes), a tenth to a third of the file. JPEG
//! goes through image's decoder (zune-jpeg), which decodes a file cut short as far as it
//! goes and fills the rest with mid-gray; PNG row by row with the png crate.

use image::{ImageFormat, RgbaImage};
use std::io::Cursor;

/// A picture decoded as far as its file has arrived.
#[derive(Debug, Clone)]
pub struct Partial {
    /// The whole picture, transparent where it has not arrived.
    pub image: RgbaImage,
    /// Rows decoded, from the top.
    pub rows: u32,
}

impl Partial {
    fn empty(width: u32, height: u32) -> Option<Partial> {
        (width > 0 && height > 0).then(|| Partial { image: RgbaImage::new(width, height), rows: 0 })
    }
}

/// Decode the beginning `head` of a picture file: the rows it gives, from the top, or an
/// empty picture of the right size while only the headers are in. `None` before that,
/// and for formats other than WebP, JPEG and PNG.
pub fn decode_partial(head: &[u8]) -> Option<Partial> {
    match image::guess_format(head).ok()? {
        ImageFormat::WebP => webp_rows(head),
        ImageFormat::Jpeg => jpeg_rows(head).or_else(|| size_only(head)),
        ImageFormat::Png => png_rows(head).or_else(|| size_only(head)),
        _ => None,
    }
}

fn size_only(head: &[u8]) -> Option<Partial> {
    let (w, h) = crate::dimensions(head)?;
    Partial::empty(w, h)
}

fn webp_rows(head: &[u8]) -> Option<Partial> {
    use libwebp_sys::{VP8StatusCode, WEBP_CSP_MODE, WebPGetInfo, WebPIDecGetRGB, WebPIDelete, WebPINewRGB, WebPIUpdate};
    // SAFETY: the decoder reads `head`, which outlives it, into an RGBA buffer of its own;
    // the rows it reports decoded are copied out (within its width, height and stride)
    // before it is deleted.
    let decoded = unsafe {
        let idec = WebPINewRGB(WEBP_CSP_MODE::MODE_RGBA, std::ptr::null_mut(), 0, 0);
        if idec.is_null() {
            return None;
        }
        let status = WebPIUpdate(idec, head.as_ptr(), head.len());
        let (mut rows, mut width, mut height, mut stride) = (0, 0, 0, 0);
        let rgba = WebPIDecGetRGB(idec, &mut rows, &mut width, &mut height, &mut stride);
        // Suspended: it needs more data.
        let fine = matches!(status, VP8StatusCode::VP8_STATUS_OK | VP8StatusCode::VP8_STATUS_SUSPENDED);
        let out = (fine && !rgba.is_null() && width > 0 && height > 0 && stride >= 4 * width).then(|| {
            let (w, h) = (width as usize, height as usize);
            let rows = (rows.max(0) as usize).min(h);
            let mut image = RgbaImage::new(w as u32, h as u32);
            for (y, out) in image.chunks_exact_mut(4 * w).take(rows).enumerate() {
                out.copy_from_slice(std::slice::from_raw_parts(rgba.add(y * stride as usize), 4 * w));
            }
            Partial { image, rows: rows as u32 }
        });
        WebPIDelete(idec);
        out
    };
    decoded.or_else(|| {
        // No rows yet: the size, if the headers are in.
        let (mut width, mut height) = (0, 0);
        // SAFETY: reads `head` only.
        let known = unsafe { WebPGetInfo(head.as_ptr(), head.len(), &mut width, &mut height) } != 0;
        if known { Partial::empty(width as u32, height as u32) } else { None }
    })
}

/// Rows of the largest JPEG MCU (vertical sampling factor 2) that may be wrong above the
/// gray: the one being decoded when the bytes ran out.
const MCU_ROWS: u32 = 16;

fn jpeg_rows(head: &[u8]) -> Option<Partial> {
    let mut image = image::load_from_memory_with_format(head, ImageFormat::Jpeg).ok()?.to_rgba8();
    let (w, h) = image.dimensions();
    let gray = image.rows().rev().take_while(|row| row.clone().all(|p| p.0 == [128, 128, 128, 255])).count() as u32;
    let rows = if gray == 0 { h } else { (h - gray).saturating_sub(MCU_ROWS) };
    image.as_mut()[(rows * w * 4) as usize..].fill(0);
    Some(Partial { image, rows })
}

fn png_rows(head: &[u8]) -> Option<Partial> {
    let mut decoder = png::Decoder::new(Cursor::new(head));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (w, h) = (reader.info().width, reader.info().height);
    let mut partial = Partial::empty(w, h)?;
    if reader.info().interlaced {
        // Adam7 passes: shown when complete.
        return Some(partial);
    }
    let (color, _) = reader.output_color_type();
    let width = w as usize;
    for out in partial.image.chunks_exact_mut(4 * width) {
        let Ok(Some(row)) = reader.next_row() else { break };
        let data = row.data();
        match color {
            png::ColorType::Rgba => out.copy_from_slice(&data[..4 * width]),
            // (Rust note: `as_chunks::<N>()` views a slice as arrays of N, plus a remainder.)
            png::ColorType::Rgb => {
                for (o, &[r, g, b]) in out.as_chunks_mut::<4>().0.iter_mut().zip(data.as_chunks::<3>().0) {
                    *o = [r, g, b, 255];
                }
            }
            png::ColorType::GrayscaleAlpha => {
                for (o, &[g, a]) in out.as_chunks_mut::<4>().0.iter_mut().zip(data.as_chunks::<2>().0) {
                    *o = [g, g, g, a];
                }
            }
            png::ColorType::Grayscale => {
                for (o, &g) in out.as_chunks_mut::<4>().0.iter_mut().zip(data) {
                    *o = [g, g, g, 255];
                }
            }
            // Palettes are expanded.
            png::ColorType::Indexed => break,
        }
        partial.rows += 1;
    }
    Some(partial)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Format, encode_at};
    use image::RgbImage;

    fn test_image(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 255 / w) as u8, (y * 255 / h) as u8, ((x ^ y) & 0xff) as u8]))
    }

    /// Rows decoded from growing beginnings of `file`, which decode as the whole file
    /// does where they have arrived (within `tolerance`) and are transparent below.
    fn grows(file: &[u8], tolerance: u8) -> Vec<u32> {
        let whole = decode_partial(file).unwrap();
        assert_eq!(whole.image.dimensions(), crate::decode(file).unwrap().dimensions());
        let full = whole.image;
        let mut seen = Vec::new();
        for tenth in 1..=10 {
            let Some(p) = decode_partial(&file[..file.len() * tenth / 10]) else {
                seen.push(0);
                continue;
            };
            assert_eq!(p.image.dimensions(), full.dimensions());
            for (y, (a, b)) in p.image.rows().zip(full.rows()).enumerate() {
                for (pa, pb) in a.zip(b) {
                    if (y as u32) < p.rows {
                        assert!(pa.0.iter().zip(pb.0).all(|(&x, y)| x.abs_diff(y) <= tolerance), "row {y}: {pa:?} vs {pb:?}");
                    } else {
                        assert_eq!(pa.0[3], 0, "row {y} has not arrived");
                    }
                }
            }
            seen.push(p.rows);
        }
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
        assert_eq!(*seen.last().unwrap(), full.height(), "{seen:?}");
        seen
    }

    #[test]
    fn webp_arrives_from_the_top() {
        let img = test_image(320, 240);
        for format in [Format::WebpLossy, Format::WebpLossless] {
            let file = encode_at(&img, format, 90).unwrap();
            let rows = grows(&file, 0);
            assert!(rows[5] > 0 && rows[5] < 240, "{format:?}: {rows:?}");
        }
    }

    #[test]
    fn jpeg_and_png_arrive_from_the_top() {
        let img = test_image(320, 240);
        let jpeg = encode_at(&img, Format::Jpeg, 90).unwrap();
        // Chroma upsampling reaches a row into what has not arrived.
        let rows = grows(&jpeg, 2);
        assert!(rows[5] > 0 && rows[5] < 240, "JPEG: {rows:?}");
        let png = encode_at(&img, Format::Png, 100).unwrap();
        let rows = grows(&png, 0);
        assert!(rows[5] > 0 && rows[5] < 240, "PNG: {rows:?}");
    }

    #[test]
    fn the_size_comes_with_the_headers() {
        let img = test_image(320, 240);
        let file = encode_at(&img, Format::WebpLossy, 90).unwrap();
        let p = decode_partial(&file[..64]).expect("the headers are in");
        assert_eq!((p.image.dimensions(), p.rows), ((320, 240), 0));
        assert!(decode_partial(&file[..16]).is_none());
        assert!(decode_partial(b"not a picture at all").is_none());
    }
}
