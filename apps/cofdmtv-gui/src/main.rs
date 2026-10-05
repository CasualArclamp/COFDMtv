//! `cofdmtv-gui` — desktop front end of COFDMtv: COFDMTV pictures (Shredpix/Assempix),
//! text (Rattlegram) and pings, received and transmitted, with DecDRM's look.
//!
//! Layout of the crate:
//! * [`app`] — the eframe application: pages, panels, repaint policy;
//! * [`receiver`], [`transmitter`] — the engines' handles and what the GUI derives
//!   from them;
//! * [`waterfall`], [`ring_image`] — the waterfall history and its texture;
//! * [`panels`] — drawing code, one module per screen area;
//! * [`settings`] — the settings remembered between runs; [`fonts`] — fallback fonts.

// Release builds on Windows are GUI-subsystem programs, so no console window opens when
// the program is started from Explorer. Debug builds keep the console for messages.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod fonts;
mod panels;
mod receiver;
mod ring_image;
mod settings;
mod transmitter;
mod waterfall;

use clap::Parser;
use eframe::egui;
use std::path::PathBuf;

/// Command-line options. All are optional: without them the GUI restores the last
/// session's settings.
#[derive(Parser, Debug, Clone, Default)]
#[command(name = "cofdmtv-gui", version, about = "COFDMtv — COFDMTV pictures, text and pings over audio")]
pub struct Args {
    /// A recording to receive (WAV/FLAC) or a picture to send.
    pub file: Option<PathBuf>,
    /// Start receiving right away.
    #[arg(long)]
    pub start: bool,
    /// Open this page.
    #[arg(long, value_enum)]
    pub page: Option<settings::Page>,
    /// Transmit right away with the transmitter page's settings.
    #[arg(long)]
    pub transmit: bool,
    /// Do not transmit to a sound card in this run (files only).
    #[arg(long)]
    pub no_audio: bool,
    /// Settings file to use instead of the per-user default.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Quit after this many seconds (at most a day).
    #[arg(long, value_name = "SECONDS")]
    pub exit_after: Option<f64>,
    /// Save a PNG screenshot of the window just before quitting (after `--exit-after`
    /// seconds, default 10).
    #[arg(long, value_name = "PNG")]
    pub screenshot: Option<PathBuf>,
    /// Initial window size in points, e.g. `1280x800` (for documentation screenshots).
    #[arg(long, value_name = "WxH", value_parser = parse_size)]
    pub window_size: Option<(f32, f32)>,
}

/// `1280x800` → (1280, 800).
fn parse_size(s: &str) -> Result<(f32, f32), String> {
    let (w, h) = s.split_once(['x', 'X']).ok_or("expected WIDTHxHEIGHT, e.g. 1280x800")?;
    let num = |v: &str| v.trim().parse::<f32>().map_err(|e| e.to_string()).and_then(|n| if n >= 200.0 { Ok(n) } else { Err("too small".into()) });
    Ok((num(w)?, num(h)?))
}

fn main() -> eframe::Result {
    let args = Args::parse();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("COFDMtv")
            .with_app_id("cofdmtv-gui")
            .with_inner_size(args.window_size.map_or([1280.0, 800.0], |(w, h)| [w, h]))
            .with_min_inner_size([900.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    // `Box::new(|cc| ..)`: eframe takes the app constructor as a boxed closure and calls it
    // once the window and the rendering context exist.
    eframe::run_native("COFDMtv", options, Box::new(move |cc| Ok(Box::new(app::CofdmtvApp::new(cc, args)))))
}
