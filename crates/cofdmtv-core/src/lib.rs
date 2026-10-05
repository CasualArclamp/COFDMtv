//! COFDMtv core: pure-Rust ports of the aicodix audio modems.
//!
//! * [`cofdmtv`] — COFDMTV: pictures (Shredpix/Assempix), text (Rattlegram) and pings.
//! * [`coding`] — CRCs, sequences, BCH and ordered-statistics decoding, polar codes with
//!   CRC-aided list decoding, PSK, Cauchy Reed–Solomon erasure coding, call signs.
//! * [`dsp`] — FFTs, filters, oscillator, sliding buffers, Theil–Sen, PAPR reduction.
//!
//! The originals (C++, BSD Zero Clause License) are at github.com/aicodix; each module
//! names the files it follows. `tools/cxx-reference` builds them for cross-checks.

pub mod coding;
pub mod cofdmtv;
pub mod dsp;
