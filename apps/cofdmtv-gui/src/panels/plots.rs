//! Plot tabs: the input spectrum, its waterfall and the payload constellation.
//!
//! The plots are monitoring displays (DecDRM's): their axes are set from the data every
//! frame and zooming/dragging is disabled; hovering shows the value under the cursor.

use super::{Palette, placeholder};
use crate::receiver::RxSession;
use crate::ring_image::RingImage;
use crate::settings::{PlotTab, Span};
use cofdmtv_engine::cofdmtv_core::coding::psk::Psk;
use eframe::egui::{Color32, ComboBox, RichText, Ui};
use egui_plot::{HLine, HoverPosition, Line, LineStyle, MarkerShape, Plot, PlotBounds, PlotImage, PlotPoint, PlotPoints, Points as Scatter, Span as PlotSpan, VLine};

/// The waterfall's image on the GPU.
#[derive(Default)]
pub struct PlotTextures {
    waterfall: RingImage,
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
                p.span(PlotSpan::new("COFDMTV signal", lo..=hi).fill(pal.band).border_width(0.0));
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
                constellation(ui, rx, &pal, side);
            });
        }
        PlotTab::Spectrum => spectrum_plot(ui, "spectrum", "input spectrum", &spectrum, &pal, avail.y),
        PlotTab::Waterfall => waterfall_plot(ui, rx, &mut textures.waterfall, band, carrier, &pal, avail.y - 22.0, true),
        PlotTab::Constellation => {
            let side = (avail.x).min(avail.y - 40.0).max(120.0);
            constellation(ui, rx, &pal, side);
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
                    p.vline(VLine::new("COFDMTV signal", x).color(pal.band_edge).width(1.0));
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

/// The last payload symbol's constellation on a square plot, ideal points as crosses.
fn constellation(ui: &mut Ui, rx: &RxSession, pal: &Palette, side: f32) {
    let snap = &rx.snap;
    ui.vertical(|ui| {
        let title = match (&snap.receiving, snap.psk) {
            (Some(r), Some(psk)) => format!("{} payload ({} carriers)", psk_name(psk), r.mode.carriers()),
            (None, Some(psk)) => format!("{} payload (last symbol)", psk_name(psk)),
            _ => "Payload".to_string(),
        };
        ui.label(title);
        let ideal: Vec<[f64; 2]> = snap.psk.map(|p| p.points().into_iter().map(|c| [f64::from(c.re), f64::from(c.im)]).collect()).unwrap_or_default();
        let points: Vec<[f64; 2]> = snap.constellation.iter().map(|p| [f64::from(p[0]), f64::from(p[1])]).collect();
        base_plot("constellation")
            .width(side)
            .height(side)
            .data_aspect(1.0)
            .show_axes(false)
            .label_formatter(hover_label("I", 2, "Q", 2))
            .show(ui, |p| {
                p.set_plot_bounds(PlotBounds::from_min_max([-1.5, -1.5], [1.5, 1.5]));
                p.hline(HLine::new("", 0.0).color(Color32::from_gray(128)).width(0.5));
                p.vline(VLine::new("", 0.0).color(Color32::from_gray(128)).width(0.5));
                if !points.is_empty() {
                    p.points(Scatter::new("carriers", PlotPoints::new(points)).color(pal.points).radius(1.8));
                }
                if !ideal.is_empty() {
                    p.points(Scatter::new("ideal", PlotPoints::new(ideal)).shape(MarkerShape::Plus).color(pal.ideal).radius(4.0));
                }
            });
    });
}

fn psk_name(psk: Psk) -> &'static str {
    match psk {
        Psk::Bpsk => "BPSK",
        Psk::Qpsk => "QPSK",
        Psk::Psk8 => "8PSK",
    }
}
