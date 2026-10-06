//! v2 picture modes: a picture in aicodix modem frames, in any of the modem's modulations,
//! code rates and frame sizes, with COFDMTV's lead-in and fancy header, its size set by
//! the air time it may take.
//!
//! On the air: a lead-in of noise symbols, the picture as a multi-frame file (the CRS
//! header of COFDMTV's multi-frame pictures; any `blocks` of the frames rebuild it, so
//! extra frames make up for lost ones), then the fancy header at the modem's carrier. The
//! receiver needs nothing new: modem frames that rebuild into a picture are shown as one.

use cofdmtv_core::cofdmtv::{SYMBOL_SECONDS, multiframe};
use cofdmtv_core::modem::{ModemMode, ModemRequest, transmission_seconds};

/// A modem symbol with its guard interval, seconds.
const MODEM_SYMBOL_SECONDS: f64 = 41.0 / 300.0;

/// How a v2 picture fits its air time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plan {
    /// Frames sent: `blocks` carry the picture, the rest are extra.
    pub frames: usize,
    pub blocks: usize,
    /// Bytes the picture may have.
    pub budget: usize,
    /// Seconds on the air.
    pub seconds: f64,
}

impl Plan {
    pub fn extra(&self) -> usize {
        self.frames - self.blocks
    }
}

/// The modem's noise symbols for a lead-in of `seconds`, at least the one every
/// transmission starts with.
pub fn lead_in_symbols(seconds: f64) -> usize {
    ((seconds.max(0.0) / MODEM_SYMBOL_SECONDS).round() as usize).max(1)
}

/// The lead-in, seconds, of COFDMTV's `noise_symbols` (the transmitter's setting).
pub fn lead_in_seconds(noise_symbols: usize) -> f64 {
    noise_symbols as f64 * SYMBOL_SECONDS
}

/// The biggest v2 picture in `mode` that stays within `air_s` seconds with
/// `noise_symbols` of lead-in, the fancy header if `fancy`, and up to `extra` extra frames
/// (there being frames to spare): one frame at least, whatever the air time.
pub fn plan(mode: ModemMode, air_s: f64, noise_symbols: usize, fancy: bool, extra: usize) -> Plan {
    let most = multiframe::MAX_BLOCKS_ANY + extra;
    let mut frames = 1;
    while frames < most && transmission_seconds(mode, frames + 1, noise_symbols, fancy) <= air_s {
        frames += 1;
    }
    let extra = extra.min(frames - 1);
    let blocks = (frames - extra).min(multiframe::MAX_BLOCKS_ANY);
    let frames = blocks + extra;
    Plan {
        frames,
        blocks,
        budget: blocks * multiframe::block_data(mode.data_bytes()),
        seconds: transmission_seconds(mode, frames, noise_symbols, fancy),
    }
}

/// The frames of a v2 picture `file`: its blocks and `extra` more.
pub fn frames(file: &[u8], mode: ModemMode, extra: usize) -> Result<Vec<Vec<u8>>, String> {
    let blocks = multiframe::blocks_in(file.len(), mode.data_bytes());
    multiframe::split_chunks(file, blocks + extra, mode.data_bytes())
}

/// The modem transmission of a v2 picture's `frames`.
pub fn request(mode: ModemMode, call_sign: impl Into<String>, carrier_hz: i32, frames: Vec<Vec<u8>>, noise_symbols: usize, fancy: bool) -> ModemRequest {
    ModemRequest { noise_symbols: noise_symbols.max(1), fancy_header: fancy, ..ModemRequest::new(mode, call_sign, carrier_hz, frames) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cofdmtv_core::modem::{CodeRate, Modulation};

    const QAM64: ModemMode = ModemMode { modulation: Modulation::Qam64, rate: CodeRate::Half, normal: true };

    #[test]
    fn the_air_time_sets_the_frames() {
        let lead = lead_in_symbols(1.08);
        assert_eq!(lead, 8);
        let p = plan(QAM64, 30.0, lead, true, 1);
        assert!(p.seconds <= 30.0, "{p:?}");
        assert!(transmission_seconds(QAM64, p.frames + 1, lead, true) > 30.0, "one more frame would not fit: {p:?}");
        assert_eq!((p.extra(), p.budget), (1, p.blocks * multiframe::block_data(QAM64.data_bytes())));
        // Twice the time, more than twice the picture: the lead-in, the header and the
        // extra frame cost the same.
        let q = plan(QAM64, 60.0, lead, true, 1);
        assert!(q.seconds <= 60.0 && transmission_seconds(QAM64, q.frames + 1, lead, true) > 60.0, "{q:?}");
        assert!(q.blocks >= 2 * p.blocks, "{p:?} {q:?}");
    }

    #[test]
    fn one_frame_at_least_and_extra_only_to_spare() {
        let p = plan(QAM64, 0.5, 1, false, 3);
        assert_eq!((p.frames, p.blocks), (1, 1));
        let two = transmission_seconds(QAM64, 2, 1, false);
        let p = plan(QAM64, two, 1, false, 3);
        assert_eq!((p.frames, p.blocks, p.extra()), (2, 1, 1));
        assert_eq!(lead_in_symbols(0.0), 1);
    }

    #[test]
    fn a_picture_goes_in_its_blocks_and_the_extra_frames() {
        let file: Vec<u8> = (0..5000u32).map(|i| (i * 13) as u8).collect();
        let frames = frames(&file, QAM64, 2).unwrap();
        let blocks = multiframe::blocks_in(file.len(), QAM64.data_bytes());
        assert_eq!(frames.len(), blocks + 2);
        assert!(frames.iter().all(|f| f.len() == QAM64.data_bytes()));
        let req = request(QAM64, "DL1ABC/P", 1500, frames, 8, true);
        assert_eq!((req.noise_symbols, req.fancy_header), (8, true));
    }
}
