//! Drawing code, one module per screen area (DecDRM's layout). Panels read the GUI state
//! and return what the user asked for (or change settings directly); they hold no
//! receiver logic of their own.

pub mod log;
pub mod meter;
pub mod plots;
pub mod received;
pub mod source;
pub mod status_strip;
pub mod tx_page;

use eframe::egui::{self, Color32, RichText, Sense, Stroke, Ui, vec2};

/// Colours chosen to read well on both the dark and the light theme (DecDRM's).
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub spectrum: Color32,
    pub band: Color32,
    /// Edges of the signal band where a fill would hide the data (waterfall).
    pub band_edge: Color32,
    pub marker: Color32,
    /// Constellation points.
    pub points: Color32,
    /// Ideal constellation points.
    pub ideal: Color32,
    pub error: Color32,
    /// Accent for text messages.
    pub text: Color32,
}

impl Palette {
    pub fn for_ui(ui: &Ui) -> Self {
        if ui.visuals().dark_mode {
            Self {
                spectrum: Color32::from_rgb(110, 170, 255),
                band: Color32::from_rgba_unmultiplied(70, 200, 110, 36),
                band_edge: Color32::from_rgb(90, 220, 130),
                marker: Color32::from_rgb(240, 150, 50),
                points: Color32::from_rgb(110, 170, 255),
                ideal: Color32::from_gray(235),
                error: Color32::from_rgb(255, 110, 100),
                text: Color32::from_rgb(240, 180, 60),
            }
        } else {
            Self {
                spectrum: Color32::from_rgb(20, 90, 200),
                band: Color32::from_rgba_unmultiplied(30, 160, 70, 40),
                band_edge: Color32::from_rgb(20, 150, 60),
                marker: Color32::from_rgb(200, 100, 0),
                points: Color32::from_rgb(20, 90, 200),
                ideal: Color32::from_gray(20),
                error: Color32::from_rgb(200, 30, 20),
                text: Color32::from_rgb(170, 100, 0),
            }
        }
    }
}

/// State of one indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Led {
    #[default]
    Off,
    Red,
    Yellow,
    Green,
}

impl Led {
    pub fn word(self) -> &'static str {
        match self {
            Self::Off => "inactive",
            Self::Red => "bad",
            Self::Yellow => "partly OK",
            Self::Green => "OK",
        }
    }
}

/// Fill colour of an indicator.
pub fn led_color(led: Led, dark_mode: bool) -> Color32 {
    match led {
        Led::Green => Color32::from_rgb(40, 200, 70),
        Led::Yellow => Color32::from_rgb(250, 190, 20),
        Led::Red => Color32::from_rgb(230, 55, 50),
        Led::Off if dark_mode => Color32::from_gray(75),
        Led::Off => Color32::from_gray(185),
    }
}

/// An indicator: a coloured dot followed by its name, with an explanation on hover.
pub fn led(ui: &mut Ui, state: Led, name: &str, help: &str) {
    let dark = ui.visuals().dark_mode;
    let response = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
            let painter = ui.painter();
            painter.circle_filled(rect.center(), 5.5, led_color(state, dark));
            let rim = if dark { Color32::from_gray(20) } else { Color32::from_gray(110) };
            painter.circle_stroke(rect.center(), 5.5, Stroke::new(1.0, rim));
            ui.label(name);
        })
        .response;
    response.on_hover_text(format!("{name}: {} — {help}", state.word()));
}

/// A label/value pair of the status strip; the value is monospaced so numbers do not
/// make the layout jitter. Returns the value label's response (for a tooltip).
pub fn value(ui: &mut Ui, name: &str, value: impl Into<String>) -> egui::Response {
    ui.label(RichText::new(name).weak());
    ui.label(RichText::new(value.into()).monospace())
}

/// Bold section heading.
pub fn heading(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).strong());
}

/// A dimmed placeholder line for empty views.
pub fn placeholder(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).weak().italics()).wrap());
}

/// A framed card with a title and a weak subtitle (DecDRM's transmitter cards).
pub fn card(ui: &mut Ui, title: &str, subtitle: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(10)).corner_radius(egui::CornerRadius::same(6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(title).strong().size(15.0));
            ui.label(RichText::new(subtitle).weak());
        });
        ui.add_space(4.0);
        add(ui);
    });
    ui.add_space(8.0);
}

/// A two-column grid (labels left, controls right).
pub fn grid(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([18.0, 6.0]).min_col_width(110.0).show(ui, add);
}

/// A grid row's label.
pub fn row_label(ui: &mut Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).weak())
}

/// `m:ss.s`, or `h:mm:ss` for an hour or more.
pub fn fmt_time(seconds: f64) -> String {
    let s = seconds.max(0.0);
    if s >= 3600.0 {
        let t = s as u64;
        format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
    } else {
        format!("{}:{:04.1}", (s / 60.0) as u64, s % 60.0)
    }
}

/// Run `add` greyed out and inert unless `on`, keeping the controls in the caller's row.
/// (Rust note: a closure `FnOnce(&mut Ui) -> Response` is itself a widget, so
/// `add_enabled` can run it; `out` carries its result out.)
pub fn enabled<R>(ui: &mut Ui, on: bool, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut out = None;
    ui.add_enabled(on, |ui: &mut Ui| {
        out = Some(add(ui));
        ui.response()
    });
    out.expect("add_enabled runs the closure")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(fmt_time(5.25), "0:05.2");
        assert_eq!(fmt_time(83.0), "1:23.0");
        assert_eq!(fmt_time(3723.0), "1:02:03");
    }
}
