//! Transmitter → file → receiver, through the engines' threads.

use cofdmtv_core::cofdmtv::{Mode, TxRequest};
use cofdmtv_engine::payload::ImageKind;
use cofdmtv_engine::{ChannelSel, InputSpec, OutputSpec, Receiver, RxConfig, RxEvent, TxChannel, TxConfig, TxEvent, TxJob, Transmitter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn request(mode: Mode, payload: Vec<u8>) -> TxRequest {
    TxRequest { mode, payload, call_sign: "DL1ABC".into(), carrier_hz: 1700, noise_symbols: 1, fancy_header: false, text_family: matches!(mode, Mode::Text(_)) }
}

fn transmit(path: &Path, rate: u32, channel: TxChannel, jobs: Vec<TxJob>) {
    let mut tx = Transmitter::start(TxConfig { output: OutputSpec::File { path: path.into() }, channel, rate: Some(rate), gain_db: 0.0, jobs });
    tx.join();
    let events = tx.poll_events();
    assert!(events.iter().any(|e| matches!(e, TxEvent::Finished)), "{events:?}");
}

fn receive(path: &Path, channel: ChannelSel, save_dir: Option<PathBuf>) -> Vec<RxEvent> {
    let mut rx = Receiver::start(RxConfig { input: InputSpec::File { path: path.into(), realtime: false }, channel, save_dir });
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut events = Vec::new();
    while !events.iter().any(|e| matches!(e, RxEvent::Stopped)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        events.extend(rx.poll_events());
    }
    rx.join();
    events.extend(rx.poll_events());
    events
}

#[test]
fn picture_text_and_ping_through_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("tx.wav");
    let jpeg = std::fs::read(fixture("testcard.jpg")).unwrap();
    let text = "Grüße from the engine test";
    let jobs = vec![
        TxJob { label: "picture".into(), request: request(Mode::Image(11), jpeg.clone()), gap_s: 0.5 },
        TxJob { label: "text".into(), request: request(Mode::Text(0), text.as_bytes().to_vec()), gap_s: 0.5 },
        TxJob { label: "ping".into(), request: request(Mode::Ping, Vec::new()), gap_s: 0.5 },
    ];
    transmit(&wav, 16000, TxChannel::Mono, jobs);
    let saves = dir.path().join("received");
    let events = receive(&wav, ChannelSel::Mix, Some(saves.clone()));
    let picture = events.iter().find_map(|e| if let RxEvent::Picture(p) = e { Some(p) } else { None }).expect("a picture");
    assert_eq!(picture.kind, Some(ImageKind::Jpeg));
    assert_eq!(picture.data, jpeg);
    assert_eq!(picture.call, "DL1ABC");
    let saved = picture.saved.as_ref().expect("saved");
    assert_eq!(std::fs::read(saved).unwrap(), jpeg);
    assert!(events.iter().any(|e| matches!(e, RxEvent::Text(m) if m.text == text)));
    assert!(events.iter().any(|e| matches!(e, RxEvent::Ping { call, .. } if call == "DL1ABC")));
    assert!(std::fs::read_to_string(saves.join("messages.txt")).unwrap().contains(text));
    assert!(events.iter().any(|e| matches!(e, RxEvent::EndOfInput)));
}

#[test]
fn multiframe_iq_flac() {
    // A two-block picture as three frames (the first one lost), I/Q at 32 kHz in a FLAC.
    let dir = tempfile::tempdir().unwrap();
    let flac = dir.path().join("tx.flac");
    let file = std::fs::read(fixture("testcard_large.jpg")).unwrap();
    let frames = cofdmtv_core::cofdmtv::multiframe::split(&file, 3).unwrap();
    let jobs = frames
        .into_iter()
        .enumerate()
        .skip(1) // the first frame "lost"
        .map(|(i, f)| TxJob { label: format!("frame {i}"), request: request(Mode::Image(10), f), gap_s: 0.2 })
        .collect();
    transmit(&flac, 32000, TxChannel::Iq, jobs);
    let events = receive(&flac, ChannelSel::Iq, None);
    assert!(events.iter().any(|e| matches!(e, RxEvent::MultiFrame { have: 1, need: 2, .. })), "{events:?}");
    let picture = events.iter().find_map(|e| if let RxEvent::Picture(p) = e { Some(p) } else { None }).expect("the picture");
    assert_eq!(picture.frames, 2);
    assert_eq!(picture.data, file);
}
