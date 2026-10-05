//! Signals made by the original aicodix programs (see `tests/fixtures/README.md`) must
//! decode: COFDMtv stays compatible with Shredpix, Assempix and Rattlegram.

use cofdmtv_core::cofdmtv::multiframe::{Progress, Reassembler};
use cofdmtv_core::cofdmtv::{Decoder, Event, Mode, Payload, PolarDecoders, decode_codeword};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn receive(name: &str) -> (Vec<Event>, Vec<Payload>) {
    let mut r = hound::WavReader::open(fixture(name)).unwrap();
    let rate = r.spec().sample_rate;
    let samples: Vec<f32> = r.samples::<i16>().map(|s| f32::from(s.unwrap()) / 32768.0).collect();
    let mut dec = Decoder::new(rate).unwrap();
    let mut polar = PolarDecoders::default();
    let (mut events, mut payloads) = (Vec::new(), Vec::new());
    for x in samples {
        if dec.push_real(x)
            && let Some(ev) = dec.process()
        {
            if ev == Event::Done {
                payloads.extend(decode_codeword(&dec.take_codeword().unwrap(), &mut polar));
            }
            events.push(ev);
        }
    }
    (events, payloads)
}

#[test]
fn shredpix_picture() {
    let (events, payloads) = receive("shredpix_mode12.wav");
    assert!(matches!(&events[0], Event::Sync { mode: Mode::Image(12), call, cfo_hz } if call == "DL1ABC" && (cfo_hz - 1700.0).abs() < 0.5));
    let jpeg = std::fs::read(fixture("testcard.jpg")).unwrap();
    let p = &payloads[0];
    assert_eq!(p.flips, 0);
    assert_eq!(&p.data[..jpeg.len()], &jpeg[..]);
    assert!(p.data[jpeg.len()..].iter().all(|&b| b == 0));
}

#[test]
fn shredpix_ping() {
    let (events, _) = receive("shredpix_ping.wav");
    assert!(matches!(&events[..], [Event::Ping { call, cfo_hz }] if call == "DL1ABC" && (cfo_hz - 1500.0).abs() < 0.5), "{events:?}");
}

#[test]
fn rattlegram_text() {
    let (_, payloads) = receive("rattlegram_text.wav");
    let text = std::fs::read(fixture("rattlegram_text.txt")).unwrap();
    assert_eq!(payloads[0].data, text);
    assert_eq!(payloads[0].mode, Mode::Text(16));
}

#[test]
fn shredpix_multiframe() {
    let (_, payloads) = receive("shredpix_multiframe.wav");
    assert_eq!(payloads.len(), 2);
    let mut r = Reassembler::default();
    assert_eq!(r.push(&payloads[0].data), Progress::Partial { have: 1, need: 2 });
    let Progress::Complete(file) = r.push(&payloads[1].data) else { panic!("not rebuilt") };
    assert_eq!(file, std::fs::read(fixture("testcard_large.jpg")).unwrap());
}
