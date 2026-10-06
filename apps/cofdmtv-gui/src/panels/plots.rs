//! Plot tabs: the input spectrum, its waterfall and the payload constellation.
//!
//! The plots are monitoring displays (DecDRM's): their axes are set from the data every
//! frame and zooming/dragging is disabled; hovering shows the value under the cursor. The
//! constellation builds up the last symbols' points as BinModem's symbol scope does.

use super::{Palette, placeholder};
use crate::receiver::RxSession;
use crate::ring_image::RingImage;
use crate::settings::{PlotTab, Span};
use eframe::egui::{self, Color32, ColorImage, ComboBox, RichText, TextureHandle, TextureId, TextureOptions, Ui};
use std::sync::Arc;
use egui_plot::{HLine, HoverPosition, Line, LineStyle, MarkerShape, Plot, PlotBounds, PlotImage, PlotPoint, PlotPoints, Points as Scatter, Span as PlotSpan, VLine};

/// The waterfall's and the constellation's images on the GPU.
#[derive(Default)]
pub struct PlotTextures {
    waterfall: RingImage,
    constellation: Option<ConstellationImage>,
}

/// The constellation's build-up image, and what it was made from.
struct ConstellationImage {
    points: Arc<Vec<[f32; 2]>>,
    texels: usize,
    colour: Color32,
    few: bool,
    texture: TextureHandle,
}

/// Common settings of all plots.
pub fn base_plot<'a>(id: &str) -> Plot<'a> {
    Plot::new(id).allow_zoom(false).allow_drag(false).allow_scroll(false).allow_boxed_zoom(false).allow_double_click_reset(false).show_crosshair(false)
}

/// Hover label `x unit_x, y unit_y` with the given decimals.
pub fn hover_label(ux: &'static str, dx: usize, uy: &'static str, dy: usize) -> impl Fn(&HoverPosition<'_>) -> Option<String> {
    move |pos: &HoverPosition<'_>| {
        let p = match pos {
            HoverPosition::NearDataPoint { position, .. } | HoverPosition::Elsewhere { position } => position,
        };
        Some(format!("{:.dx$} {ux}\n{:.dy$} {uy}", p.x, p.y))
    }
}

/// A spectrum to plot.
pub struct SpectrumView<'a> {
    /// dB per bin, bins `axis.0 + k·axis.1` Hz.
    pub db: &'a [f32],
    pub axis: (f64, f64),
    /// Frequencies shown, Hz.
    pub span: (f64, f64),
    /// A signal: its band (Hz) and carrier.
    pub band: Option<(f64, f64)>,
    pub carrier: Option<f64>,
}

/// A spectrum with the signal's band shaded and its carrier marked.
pub fn spectrum_plot(ui: &mut Ui, id: &str, name: &str, s: &SpectrumView<'_>, pal: &Palette, height: f32) {
    let (first, bin) = s.axis;
    let points: Vec<[f64; 2]> = s
        .db
        .iter()
        .enumerate()
        .map(|(k, &db)| [first + k as f64 * bin, f64::from(db)])
        .filter(|p| p[0] >= s.span.0 && p[0] <= s.span.1)
        .collect();
    let (mut y0, mut y1) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p[1]), hi.max(p[1])));
    if !y0.is_finite() {
        (y0, y1) = (-110.0, -20.0);
    }
    // From a little below the floor to above the peaks, at least 40 dB.
    let mut sorted: Vec<f64> = points.iter().map(|p| p[1]).collect();
    sorted.sort_by(f64::total_cmp);
    if let Some(&floor) = sorted.get(sorted.len() / 10) {
        y0 = (floor - 10.0).max(y0 - 3.0);
    }
    y0 = y0.max(-140.0);
    y1 = (y1 + 6.0).max(y0 + 40.0);
    base_plot(id)
        .height(height.max(80.0))
        .x_axis_label("frequency (Hz)")
        .y_axis_label("power (dB)")
        .label_formatter(hover_label("Hz", 1, "dB", 1))
        .show(ui, |p| {
            p.set_plot_bounds(PlotBounds::from_min_max([s.span.0, y0], [s.span.1, y1]));
            if let Some((lo, hi)) = s.band {
                p.span(PlotSpan::new("signal", lo..=hi).fill(pal.band).border_width(0.0));
            }
            if let Some(c) = s.carrier {
                p.vline(VLine::new("carrier", c).color(pal.marker).style(LineStyle::dashed_dense()));
            }
            if !points.is_empty() {
                p.line(Line::new(name.to_string(), PlotPoints::new(points)).color(pal.spectrum).width(1.0));
            }
        });
}

/// Tab bar plus the selected tab.
pub fn show(ui: &mut Ui, tab: &mut PlotTab, span: &mut Span, rx: &RxSession, textures: &mut PlotTextures) {
    ui.horizontal(|ui| {
        for t in PlotTab::ALL {
            ui.selectable_value(tab, t, t.label());
        }
        ui.with_layout(eframe::egui::Layout::right_to_left(eframe::egui::Align::Center), |ui| {
            ComboBox::from_id_salt("span")
                .selected_text(span.label())
                .show_ui(ui, |ui| {
                    for s in Span::ALL {
                        ui.selectable_value(span, s, s.label());
                    }
                })
                .response
                .on_hover_text("Frequencies shown by the spectrum and the waterfall");
        });
    });
    ui.separator();
    let pal = Palette::for_ui(ui);
    let avail = ui.available_size();
    let snap = &rx.snap;
    let rate = snap.source.processing_rate;
    if rate == 0 {
        placeholder(ui, "Start the receiver: the spectrum, the waterfall and the constellation appear here.");
        return;
    }
    let view_span = span.range(rate, snap.iq);
    let (band, carrier) = match rx.band {
        Some((c, bw)) => (Some((f64::from(c - bw / 2.0), f64::from(c + bw / 2.0))), Some(f64::from(c))),
        None => (None, None),
    };
    let spectrum = SpectrumView { db: &snap.spectrum, axis: snap.spectrum_axis, span: view_span, band, carrier };
    match tab {
        PlotTab::Overview => {
            let side = (avail.x * 0.3).min(avail.y * 0.55).max(120.0);
            let spectrum_height = (avail.y - side - 40.0).max(120.0);
            spectrum_plot(ui, "spectrum", "input spectrum", &spectrum, &pal, spectrum_height);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width((avail.x - side - 16.0).max(200.0));
                    waterfall_plot(ui, rx, &mut textures.waterfall, band, carrier, &pal, side, false);
                });
                constellation(ui, rx, &pal, side, &mut textures.constellation);
            });
        }
        PlotTab::Spectrum => spectrum_plot(ui, "spectrum", "input spectrum", &spectrum, &pal, avail.y),
        PlotTab::Waterfall => waterfall_plot(ui, rx, &mut textures.waterfall, band, carrier, &pal, avail.y - 22.0, true),
        PlotTab::Constellation => {
            let side = (avail.x).min(avail.y - 40.0).max(120.0);
            constellation(ui, rx, &pal, side, &mut textures.constellation);
        }
    }
}

/// The spectrum's history, newest at the top.
#[allow(clippy::too_many_arguments)]
fn waterfall_plot(ui: &mut Ui, rx: &RxSession, texture: &mut RingImage, band: Option<(f64, f64)>, carrier: Option<f64>, pal: &Palette, height: f32, caption: bool) {
    let waterfall = &rx.waterfall;
    let Some((texture_id, uv)) = texture.update(ui.ctx(), "waterfall", waterfall) else {
        placeholder(ui, "No spectrum yet: the waterfall fills while the receiver runs.");
        return;
    };
    let (x0, x1) = waterfall.span_hz();
    let seconds = waterfall.span_s();
    base_plot("waterfall")
        .height(height.max(80.0))
        .x_axis_label("frequency (Hz)")
        .y_axis_label("time (s)")
        .label_formatter(hover_label("Hz", 1, "s", 1))
        .show(ui, |p| {
            p.set_plot_bounds(PlotBounds::from_min_max([x0, -seconds], [x1, 0.0]));
            p.image(PlotImage::new("waterfall", texture_id, PlotPoint::new((x0 + x1) / 2.0, -seconds / 2.0), [(x1 - x0) as f32, seconds as f32]).uv(uv));
            if let Some((lo, hi)) = band {
                for x in [lo, hi] {
                    p.vline(VLine::new("signal", x).color(pal.band_edge).width(1.0));
                }
            }
            if let Some(c) = carrier {
                p.vline(VLine::new("carrier", c).color(pal.marker).style(LineStyle::dashed_dense()));
            }
        });
    if caption {
        let (lo, hi) = waterfall.levels();
        ui.label(
            RichText::new(format!(
                "{} rows (~{:.0} s), newest at the top; colours {lo:.0} … {hi:.0} dB. The call sign of a transmission's fancy header reads here.",
                waterfall.rows(),
                waterfall.filled_s()
            ))
            .weak()
            .small(),
        );
    }
}

/// I and Q the constellation plot spans: ± this.
const SPAN: f32 = 1.5;
/// Most texels across the constellation's image (a bigger plot shows them larger).
const MAX_TEXELS: usize = 1024;

/// The payload's constellation on a square plot: the last symbols' points building up
/// where they land (see [`build_up`]), the ideal points as crosses.
fn constellation(ui: &mut Ui, rx: &RxSession, pal: &Palette, side: f32, image: &mut Option<ConstellationImage>) {
    let snap = &rx.snap;
    ui.vertical(|ui| {
        let n = snap.constellation_symbols;
        if snap.constellation.is_empty() {
            ui.label("Payload");
        } else {
            ui.label(format!("{} payload, last {n} symbol{}", snap.constellation_label, if n == 1 { "" } else { "s" })).on_hover_text(format!(
                "{} points, each drawn faintly: where symbols keep landing, on the constellation's points, they build up",
                snap.constellation.len()
            ));
        }
        let texels = ((side * ui.ctx().pixels_per_point()).round() as usize).clamp(64, MAX_TEXELS);
        // The modulations with ideal points to show have few points: larger squares.
        let few = !snap.ideal.is_empty();
        let texture = (!snap.constellation.is_empty()).then(|| refresh(ui.ctx(), image, &snap.constellation, texels, pal.points, few));
        let ideal: Vec<[f64; 2]> = snap.ideal.iter().map(|p| [f64::from(p[0]), f64::from(p[1])]).collect();
        let span = f64::from(SPAN);
        base_plot("constellation")
            .width(side)
            .height(side)
            .data_aspect(1.0)
            .show_axes(false)
            .label_formatter(hover_label("I", 2, "Q", 2))
            .show(ui, |p| {
                p.set_plot_bounds(PlotBounds::from_min_max([-span, -span], [span, span]));
                p.hline(HLine::new("", 0.0).color(Color32::from_gray(128)).width(0.5));
                p.vline(VLine::new("", 0.0).color(Color32::from_gray(128)).width(0.5));
                // (egui_plot paints images first, under everything else.)
                if let Some(id) = texture {
                    p.image(PlotImage::new("points", id, PlotPoint::new(0.0, 0.0), [2.0 * SPAN, 2.0 * SPAN]));
                }
                if !ideal.is_empty() {
                    p.points(Scatter::new("ideal", PlotPoints::new(ideal)).shape(MarkerShape::Plus).color(pal.ideal.gamma_multiply(0.8)).radius(4.0));
                }
            });
    });
}

/// The constellation's texture, made again when the points, the size or the colour
/// changed (new points come with each payload symbol, a few times a second).
fn refresh(ctx: &egui::Context, image: &mut Option<ConstellationImage>, points: &Arc<Vec<[f32; 2]>>, texels: usize, colour: Color32, few: bool) -> TextureId {
    let current = image.as_ref().is_some_and(|i| Arc::ptr_eq(&i.points, points) && i.texels == texels && i.colour == colour && i.few == few);
    if !current {
        let pixels = build_up(points, texels, colour, few);
        match image {
            Some(i) => {
                i.texture.set(pixels, TextureOptions::NEAREST);
                (i.points, i.texels, i.colour, i.few) = (Arc::clone(points), texels, colour, few);
            }
            None => {
                let texture = ctx.load_texture("constellation", pixels, TextureOptions::NEAREST);
                *image = Some(ConstellationImage { points: Arc::clone(points), texels, colour, few, texture });
            }
        }
    }
    image.as_ref().map_or(TextureId::default(), |i| i.texture.id())
}

/// The points as BinModem's symbol scope draws them: each a small, faint square of one
/// colour, so that where symbols keep landing — on the points of the constellation — they
/// build up, `n` of them as opaque as `n` translucent layers, 1 − (1 − a)ⁿ, while noise
/// stays faint. A modulation of `few` points (up to 64: PSK, QAM16, QAM64) gets larger,
/// less faint squares, as BinModem's smaller constellations do (there the choice goes by
/// the points kept; here every symbol brings hundreds, so it goes by the modulation), large
/// enough to show around the ideal points' crosses. The image is `texels` square over
/// ±[`SPAN`].
fn build_up(points: &[[f32; 2]], texels: usize, colour: Color32, few: bool) -> ColorImage {
    let alpha = if few { 150.0 / 255.0 } else { 110.0 / 255.0 };
    let square = ((texels as f32 / 360.0).clamp(1.0, 2.5) * if few { 2.5 } else { 1.0 }).round().max(1.0) as i64;
    let scale = texels as f32 / (2.0 * SPAN);
    let n = texels as i64;
    let mut hits = vec![0u16; texels * texels];
    for &[x, y] in points {
        if !(x.is_finite() && y.is_finite()) {
            continue;
        }
        // The square's first texel, rows from the top.
        let x0 = ((x + SPAN) * scale - square as f32 / 2.0).round() as i64;
        let y0 = ((SPAN - y) * scale - square as f32 / 2.0).round() as i64;
        for row in y0.max(0)..(y0 + square).min(n) {
            for col in x0.max(0)..(x0 + square).min(n) {
                let h = &mut hits[(row * n + col) as usize];
                *h = h.saturating_add(1);
            }
        }
    }
    // Opacity of 0, 1, 2 … layers; beyond the table they are opaque.
    let opacity: Vec<u8> = (0..64).map(|k| ((1.0 - (1.0f32 - alpha).powi(k)) * 255.0).round() as u8).collect();
    let pixels = hits
        .iter()
        .map(|&k| match k {
            0 => Color32::TRANSPARENT,
            k => Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), opacity.get(usize::from(k)).copied().unwrap_or(255)),
        })
        .collect();
    ColorImage::new([texels, texels], pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_build_up_where_they_land() {
        let colour = Color32::from_rgb(90, 160, 255);
        // A dozen symbols on one point of a crowded constellation, one stray point.
        let mut points = vec![[0.5f32, 0.5]; 12];
        points.extend(std::iter::repeat_n([-1.4, -1.4], 300));
        points.push([-0.5, 0.25]);
        points.push([f32::NAN, 0.0]);
        points.push([9.0, 9.0]);
        let img = build_up(&points, 300, colour, false);
        let at = |x: f32, y: f32| img.pixels[((SPAN - y) * 100.0) as usize * 300 + ((x + SPAN) * 100.0) as usize];
        let (one, twelve, many) = (at(-0.5, 0.25).a(), at(0.5, 0.5).a(), at(-1.4, -1.4).a());
        assert!(one > 0 && one < 120, "a single point is faint: {one}");
        assert!(twelve > 240, "a dozen build up: {twelve}");
        assert_eq!(many, 255);
        assert_eq!(at(1.0, -1.0), Color32::TRANSPARENT);
        assert_eq!(img.pixels.iter().filter(|p| p.a() > 0).count(), 3, "one texel per point at this size, nothing for NaN or off the plot");
        // The points of a small constellation are squares of three texels here.
        let img = build_up(&[[0.5, 0.5]], 300, colour, true);
        assert_eq!(img.pixels.iter().filter(|p| p.a() > 0).count(), 9);
    }
}

