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
        TxJob::cofdmtv("picture", request(Mode::Image(11), jpeg.clone()), 0.5),
        TxJob::cofdmtv("text", request(Mode::Text(0), text.as_bytes().to_vec()), 0.5),
        TxJob::cofdmtv("ping", request(Mode::Ping, Vec::new()), 0.5),
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
        .map(|(i, f)| TxJob::cofdmtv(format!("frame {i}"), request(Mode::Image(10), f), 0.2))
        .collect();
    transmit(&flac, 32000, TxChannel::Iq, jobs);
    let events = receive(&flac, ChannelSel::Iq, None);
    assert!(events.iter().any(|e| matches!(e, RxEvent::MultiFrame { have: 1, need: 2, .. })), "{events:?}");
    let picture = events.iter().find_map(|e| if let RxEvent::Picture(p) = e { Some(p) } else { None }).expect("the picture");
    assert_eq!(picture.frames, 2);
    assert_eq!(picture.data, file);
}

#[test]
fn modem_text_and_file() {
    use cofdmtv_core::cofdmtv::multiframe;
    use cofdmtv_core::modem::{CodeRate, ModemMode, ModemRequest, Modulation};
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("modem.wav");
    let text = "Modem datagram: hello";
    let mode = ModemMode { modulation: Modulation::Qam16, rate: CodeRate::Half, normal: false };
    // A file of four blocks in five frames (one extra), the first frame lost.
    let file: Vec<u8> = (0..900u32).map(|i| (i * 7 + 3) as u8).collect();
    let blocks = multiframe::blocks_in(file.len(), mode.data_bytes());
    let frames = multiframe::split_chunks(&file, blocks + 1, mode.data_bytes()).unwrap();
    let jobs = vec![
        TxJob::modem("text", ModemRequest::new(mode, "DL1ABC", 1500, vec![text.as_bytes().to_vec()]), 0.3),
        TxJob::modem("file", ModemRequest::new(mode, "DL1ABC", 1800, frames[1..].to_vec()), 0.3),
    ];
    transmit(&wav, 48000, TxChannel::Mono, jobs);
    let saves = dir.path().join("received");
    let events = receive(&wav, ChannelSel::Mix, Some(saves));
    assert!(events.iter().any(|e| matches!(e, RxEvent::Text(m) if m.text == text && m.label.contains("QAM16"))), "{events:?}");
    let got = events.iter().find_map(|e| if let RxEvent::File(f) = e { Some(f) } else { None }).expect("the file");
    assert_eq!(got.data, file);
    assert_eq!(got.frames, blocks);
    assert_eq!(std::fs::read(got.saved.as_ref().unwrap()).unwrap(), file);
}

/// A v2 picture: a lead-in, the picture in QAM256 frames sized by air time with an extra
/// frame, the fancy header; the third frame lost. Its beginning comes frame by frame until
/// the gap, then the extra frame makes up for it: it arrives whole, labelled v2.
#[test]
fn v2_picture() {
    use cofdmtv_core::modem::{CodeRate, ModemMode, Modulation};
    use cofdmtv_engine::v2;
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("v2.wav");
    let jpeg = std::fs::read(fixture("testcard_large.jpg")).unwrap();
    let mode = ModemMode { modulation: Modulation::Qam256, rate: CodeRate::Half, normal: false };
    let lead = v2::lead_in_symbols(1.0);
    let plan = v2::plan(mode, 30.0, lead, true, 1);
    assert!(plan.budget >= jpeg.len(), "{plan:?}");
    let frames = v2::frames(&jpeg, mode, plan.extra()).unwrap();
    let blocks = frames.len() - plan.extra();
    assert!(blocks >= 4 && frames.len() == blocks + 1, "{plan:?}");
    let sent: Vec<Vec<u8>> = frames.iter().enumerate().filter(|&(i, _)| i != 2).map(|(_, f)| f.clone()).collect();
    let request = v2::request(mode, "DL1ABC/P", 1500, sent, lead, true);
    transmit(&wav, 48000, TxChannel::Mono, vec![TxJob::modem("v2 picture", request, 0.0)]);
    let events = receive(&wav, ChannelSel::Mix, None);
    let heads: Vec<(usize, &[u8], &str)> =
        events.iter().filter_map(|e| if let RxEvent::MultiFrame { have, head, label, .. } = e { Some((*have, head.as_slice(), label.as_str())) } else { None }).collect();
    assert_eq!(heads.len(), blocks - 1, "every frame but the last that completes it");
    let copy = jpeg.len().div_ceil(blocks);
    for &(have, head, label) in &heads {
        assert_eq!(head, &jpeg[..have.min(2) * copy], "frame {have}");
        assert_eq!(label, "v2 QAM256 1/2 short");
    }
    let picture = events.iter().find_map(|e| if let RxEvent::Picture(p) = e { Some(p) } else { None }).expect("the picture");
    assert_eq!(picture.data, jpeg);
    assert_eq!(picture.kind, Some(ImageKind::Jpeg));
    assert_eq!(picture.label, "v2 QAM256 1/2 short");
    assert_eq!(picture.call, "DL1ABC/P");
}
