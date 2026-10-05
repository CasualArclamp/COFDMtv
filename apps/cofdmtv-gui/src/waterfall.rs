//! Waterfall: the history of the input spectrum, a row every 90 ms (half a COFDMTV
//! symbol with its guard interval), newest on top, cropped to the span shown.
//!
//! [`Waterfall`] keeps the last [`WATERFALL_ROWS`] rows and tracks display levels that
//! follow the noise floor and the strongest signals (DecDRM's waterfall). It is plain
//! data, so it can be unit-tested; [`crate::ring_image`] colours the rows through
//! [`palette`] and keeps them on the GPU.

use crate::ring_image::RingSource;
use eframe::egui::Color32;
use std::collections::VecDeque;

/// Rows kept and shown (about 46 s: a whole picture transmission and its header).
pub const WATERFALL_ROWS: usize = 512;
/// Smallest level range shown, dB.
const MIN_RANGE_DB: f32 = 30.0;
/// Time constant of the display levels, seconds.
const LEVELS_TAU_S: f64 = 1.0;

/// History of spectrum rows plus display levels.
#[derive(Debug, Clone, Default)]
pub struct Waterfall {
    /// Oldest first.
    rows: VecDeque<Vec<f32>>,
    /// The frequencies the rows cover (first and last bin, Hz) and the time per row.
    span_hz: (f64, f64),
    row_s: f64,
    pushed: u64,
    epoch: u64,
    floor: Option<f32>,
    peak: Option<f32>,
}

/// The part of `row` (bins `first + k·bin` Hz) within `span` (Hz), and the frequencies of
/// its first and last bin.
pub fn crop(row: &[f32], axis: (f64, f64), span: (f64, f64)) -> (Vec<f32>, (f64, f64)) {
    let (first, bin) = axis;
    if row.is_empty() || bin <= 0.0 {
        return (Vec::new(), span);
    }
    let lo = ((span.0 - first) / bin).ceil().max(0.0) as usize;
    let hi = (((span.1 - first) / bin).floor() as usize).min(row.len() - 1);
    if lo > hi {
        return (Vec::new(), span);
    }
    (row[lo..=hi].to_vec(), (first + lo as f64 * bin, first + hi as f64 * bin))
}

impl Waterfall {
    /// Add a row covering `span_hz`, `row_s` seconds after the previous one. Another
    /// width or span starts afresh.
    pub fn push(&mut self, row: Vec<f32>, span_hz: (f64, f64), row_s: f64) {
        if row.is_empty() {
            return;
        }
        let same = self.rows.back().is_some_and(|r| r.len() == row.len()) && self.span_hz == span_hz && self.row_s == row_s;
        if !same {
            *self = Self { epoch: self.epoch + 1, span_hz, row_s, ..Self::default() };
        }
        self.update_levels(&row);
        self.rows.push_back(row);
        while self.rows.len() > WATERFALL_ROWS {
            self.rows.pop_front();
        }
        self.pushed += 1;
    }

    pub fn clear(&mut self) {
        *self = Self { epoch: self.epoch + 1, ..Self::default() };
    }

    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    /// Frequencies of the first and last column, Hz.
    pub fn span_hz(&self) -> (f64, f64) {
        self.span_hz
    }

    /// Seconds the full image spans (its time axis).
    pub fn span_s(&self) -> f64 {
        WATERFALL_ROWS as f64 * self.row_s
    }

    /// Seconds the rows held cover.
    pub fn filled_s(&self) -> f64 {
        self.rows.len() as f64 * self.row_s
    }

    /// Display range (low, high), dB: from a little below the noise floor to above the
    /// strongest signals, at least [`MIN_RANGE_DB`] wide.
    pub fn levels(&self) -> (f32, f32) {
        let floor = self.floor.unwrap_or(-100.0);
        let peak = self.peak.unwrap_or(floor + MIN_RANGE_DB);
        let lo = floor - 5.0;
        (lo, (peak + 3.0).max(lo + MIN_RANGE_DB))
    }

    fn update_levels(&mut self, row: &[f32]) {
        let mut sorted = row.to_vec();
        sorted.sort_by(f32::total_cmp);
        let pick = |q: f32| sorted[((sorted.len() - 1) as f32 * q).round() as usize];
        let (floor, peak) = (pick(0.2), pick(0.995));
        let a = (self.row_s / LEVELS_TAU_S).min(1.0) as f32;
        let smooth = |old: Option<f32>, new: f32| Some(old.map_or(new, |o| o + a * (new - o)));
        self.floor = smooth(self.floor, floor);
        self.peak = smooth(self.peak, peak);
    }
}

impl RingSource for Waterfall {
    fn rows(&self) -> &VecDeque<Vec<f32>> {
        &self.rows
    }

    fn capacity(&self) -> usize {
        WATERFALL_ROWS
    }

    fn pushed(&self) -> u64 {
        self.pushed
    }

    fn epoch(&self) -> u64 {
        self.epoch
    }

    fn levels(&self) -> (f32, f32) {
        Waterfall::levels(self)
    }
}

/// A 256-entry colour map from black through purple and red to pale yellow, close to
/// matplotlib's "inferno": perceptually ordered, readable on dark and light themes.
pub fn palette() -> Vec<Color32> {
    const STOPS: [(f32, [u8; 3]); 6] =
        [(0.0, [0, 0, 4]), (0.2, [40, 11, 84]), (0.4, [101, 21, 110]), (0.6, [159, 42, 99]), (0.8, [237, 105, 37]), (1.0, [252, 255, 164])];
    (0..256)
        .map(|i| {
            let t = i as f32 / 255.0;
            let k = STOPS.windows(2).position(|w| t <= w[1].0).unwrap_or(STOPS.len() - 2);
            let ((t0, c0), (t1, c1)) = (STOPS[k], STOPS[k + 1]);
            let f = (t - t0) / (t1 - t0);
            let mix = |a: u8, b: u8| (f32::from(a) + f * (f32::from(b) - f32::from(a))).round() as u8;
            Color32::from_rgb(mix(c0[0], c1[0]), mix(c0[1], c1[1]), mix(c0[2], c1[2]))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_to_span() {
        let row: Vec<f32> = (0..640).map(|i| i as f32).collect();
        // 6.25 Hz bins from 0 Hz: 1000–2000 Hz are bins 160…320.
        let (part, (a, b)) = crop(&row, (0.0, 6.25), (1000.0, 2000.0));
        assert_eq!((part.len(), part[0], a, b), (161, 160.0, 1000.0, 2000.0));
        // Beyond the row: clipped.
        let (part, (_, b)) = crop(&row, (0.0, 6.25), (0.0, 8000.0));
        assert_eq!((part.len(), b), (640, 639.0 * 6.25));
        // I/Q rows start at −fs/2.
        let (part, (a, _)) = crop(&row, (-2000.0, 6.25), (0.0, 100.0));
        assert_eq!((part[0], a), (320.0, 0.0));
    }

    #[test]
    fn rows_and_levels() {
        let mut w = Waterfall::default();
        for _ in 0..(WATERFALL_ROWS + 10) {
            let mut row = vec![-100.0; 64];
            row[10] = -40.0;
            w.push(row, (0.0, 400.0), 0.09);
        }
        assert_eq!(w.rows(), WATERFALL_ROWS);
        let (lo, hi) = w.levels();
        assert!((lo - -105.0).abs() < 1.0 && hi >= lo + MIN_RANGE_DB);
        let e = w.epoch();
        w.push(vec![-100.0; 32], (0.0, 200.0), 0.09);
        assert!(w.epoch() > e && w.rows() == 1, "another span starts afresh");
    }
}
