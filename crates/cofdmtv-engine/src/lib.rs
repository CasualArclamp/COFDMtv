//! COFDMtv's engines: the receiver and the transmitter on worker threads, with their
//! sources and sinks (recordings, sound cards), shared by the command line and the GUI.
//!
//! * [`receiver`] — [`Receiver`]: source → COFDMTV decoder (+ spectrum, level) → events
//!   (transmissions, pictures, texts) and snapshots for the displays.
//! * [`transmitter`] — [`Transmitter`]: transmissions → encoder → sound card or file.
//! * [`input`] — sources, channel selection, resampling to a modem rate.
//! * [`payload`] — recognising, trimming and saving received pictures and messages.
//! * [`spectrum`] — the spectrum and waterfall rows.

pub mod input;
pub mod payload;
pub mod receiver;
pub mod spectrum;
pub mod transmitter;

pub use cofdmtv_core;
pub use cofdmtv_io;
pub use input::{ChannelSel, InputSpec, SourceInfo};
pub use receiver::{Receiver, Receiving, RxConfig, RxEvent, RxSnapshot};
pub use transmitter::{OutputSpec, TxChannel, TxConfig, TxEvent, TxJob, TxSnapshot, Transmitter};
