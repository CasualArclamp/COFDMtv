//! Status strip: indicators, what is being received, and the input, in two rows.

use super::{Led, Palette, fmt_time, led, value};
use crate::receiver::{Outcome, RxSession};
use cofdmtv_engine::SignalKind;
use cofdmtv_engine::cofdmtv_core::cofdmtv::SYMBOL_SECONDS;
use eframe::egui::{self, RichText, Ui};
use std::time::Duration;

/// RMS input levels (dBFS) for the input indicator. COFDMTV's peaks are clipped to
/// about 12 dB above its RMS level, so above −10 dBFS RMS the sound card clips.
pub const LEVEL_SILENT_DBFS: f32 = -90.0;
pub const LEVEL_WEAK_DBFS: f32 = -60.0;
pub const LEVEL_HOT_DBFS: f32 = -10.0;
pub const LEVEL_CLIP_DBFS: f32 = -3.0;

/// How long an outcome keeps its indicator lit.
const SHOW_OUTCOME: Duration = Duration::from_secs(15);

/// Input: level within a sensible range.
pub fn input_led(running: bool, level: Option<(f32, f32)>) -> Led {
    let Some((l, _)) = level.filter(|_| running) else { return Led::Off };
    if !l.is_finite() || l <= LEVEL_SILENT_DBFS || l >= LEVEL_CLIP_DBFS {
        Led::Red
    } else if l <= LEVEL_WEAK_DBFS || l >= LEVEL_HOT_DBFS {
        Led::Yellow
    } else {
        Led::Green
    }
}

pub fn show(ui: &mut Ui, rx: &RxSession) {
    let snap = &rx.snap;
    let running = rx.is_running();
    let recent = |t: Option<std::time::Instant>| t.is_some_and(|t| t.elapsed() < SHOW_OUTCOME);
    // Row 1: indicators and the transmission.
    ui.horizontal_wrapped(|ui| {
        led(ui, input_led(running, snap.level), "Input", "RMS input level in a usable range (not silent, not clipping)");
        let sync = if !running {
            Led::Off
        } else if snap.receiving.is_some() {
            Led::Green
        } else if recent(rx.preamble_failed) {
            Led::Yellow
        } else {
            Led::Off
        };
        led(ui, sync, "Sync", "a transmission found and its preamble or meta data (call sign, mode) decoded; yellow: a COFDMTV sync symbol whose preamble was damaged");
        let decode = if snap.decoding {
            Led::Yellow
        } else {
            match rx.outcome {
                Some((Outcome::Decoded, t)) if t.elapsed() < SHOW_OUTCOME => Led::Green,
                Some((Outcome::Failed, t)) if t.elapsed() < SHOW_OUTCOME => Led::Red,
                _ => Led::Off,
            }
        };
        led(ui, decode, "Decode", "the last payload's polar code and CRC: green decoded, red failed, yellow decoding");
        ui.separator();
        let state = if !running {
            "Idle"
        } else if rx.is_stopping() {
            "Stopping…"
        } else if snap.decoding {
            "Decoding"
        } else if snap.receiving.is_some() {
            "Receiving"
        } else {
            "Listening"
        };
        ui.label(RichText::new(state).strong());
        if let Some(r) = &snap.receiving {
            value(ui, "Mode", r.label.clone());
            value(ui, "From", r.call.clone());
            value(ui, "Carrier", format!("{:.1} Hz", r.cfo_hz)).on_hover_text("Centre frequency of the signal, as the synchroniser measured it");
            value(ui, "SNR", snap.snr_db.map_or_else(|| "–".into(), |s| format!("{s:.1} dB")))
                .on_hover_text("Signal-to-noise ratio of the last payload symbol (from its decision errors)");
            let fraction = r.symbol as f32 / r.symbols.max(1) as f32;
            let symbol_s = match r.kind {
                SignalKind::Cofdmtv => SYMBOL_SECONDS,
                SignalKind::Modem => 41.0 / 300.0,
            };
            ui.add(egui::ProgressBar::new(fraction).desired_width(150.0).text(format!("{} / {} symbols", r.symbol, r.symbols)))
                .on_hover_text(format!("{:.1} s to go", r.symbols.saturating_sub(r.symbol) as f64 * symbol_s));
        } else if let Some(m) = &rx.multiframe {
            value(ui, "Multi-frame", format!("{} of {} frames from {}", m.have, m.need, m.call))
                .on_hover_text(format!("A {}-byte file in {} blocks: any {} frames rebuild it", m.size, m.need, m.need));
        }
    });

    // Row 2: the input.
    ui.horizontal_wrapped(|ui| {
        level_meter(ui, snap.level.filter(|_| running));
        let info = &snap.source;
        if !info.name.is_empty() {
            match info.duration_s.filter(|d| *d > 0.0 && info.is_file) {
                Some(total) => {
                    ui.label(RichText::new("File").weak());
                    let pos = snap.position_s;
                    ui.add(egui::ProgressBar::new((pos / total).clamp(0.0, 1.0) as f32).desired_width(150.0).text(format!("{} / {}", fmt_time(pos), fmt_time(total))))
                        .on_hover_text(format!("{} — {} Hz, {} ch", info.name, info.rate, info.channels));
                }
                None => {
                    value(ui, "Elapsed", fmt_time(snap.position_s));
                    let text = format!("{} ({} Hz, {} ch)", info.name, info.rate, info.channels);
                    ui.add(egui::Label::new(RichText::new(&text).weak()).truncate()).on_hover_text(text);
                }
            }
        }
        ui.separator();
        let texts = rx.messages.iter().filter(|m| matches!(m, crate::receiver::Message::Text(_))).count();
        let pings = rx.messages.len() - texts;
        value(ui, "Pictures", rx.pictures.len().to_string());
        value(ui, "Texts", texts.to_string());
        value(ui, "Pings", pings.to_string());
        value(ui, "Files", rx.files.len().to_string());
    });
}

/// Input level bar (−90 … 0 dBFS).
fn level_meter(ui: &mut Ui, level: Option<(f32, f32)>) {
    ui.label(RichText::new("Level").weak());
    let (fraction, text) = match level {
        Some((rms, _)) => (((rms - LEVEL_SILENT_DBFS) / -LEVEL_SILENT_DBFS).clamp(0.0, 1.0), format!("{rms:.1} dBFS")),
        None => (0.0, "–".to_string()),
    };
    let pal = Palette::for_ui(ui);
    let color = if level.is_some_and(|(rms, peak)| rms >= LEVEL_CLIP_DBFS || peak >= -0.1) { pal.error } else { ui.visuals().selection.bg_fill };
    ui.add(egui::ProgressBar::new(fraction).desired_width(110.0).fill(color).text(text))
        .on_hover_text(level.map_or_else(|| "RMS input level".to_string(), |(r, p)| format!("RMS {r:.1} dBFS, peak {p:.1} dBFS (last 100 ms)")));
}
