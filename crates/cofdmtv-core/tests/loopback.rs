//! Encoder → decoder through the whole signal chain: every rate, every kind of payload,
//! with and without noise and frequency offsets.

use cofdmtv_core::coding::xorshift::Xorshift32;
use cofdmtv_core::cofdmtv::{Decoder, Encoder, Event, Mode, Payload, PolarDecoders, RATES, TxRequest, decode_codeword};

fn request(mode: Mode, payload: Vec<u8>, carrier_hz: i32) -> TxRequest {
    TxRequest { mode, payload, call_sign: "DL1ABC".into(), carrier_hz, noise_symbols: 2, fancy_header: true, text_family: false }
}

/// The transmission's real signal with a second of silence around it.
fn transmit(rate: u32, req: &TxRequest) -> Vec<f32> {
    let mut enc = Encoder::new(rate).unwrap();
    enc.configure(req).unwrap();
    let mut out = vec![0.0; rate as usize];
    while let Some(sym) = enc.next_symbol() {
        out.extend(sym.iter().map(|c| c.re));
    }
    out.extend(std::iter::repeat_n(0.0, rate as usize));
    out
}

/// Everything the receiver reports for `signal`.
fn receive(rate: u32, signal: &[f32]) -> (Vec<Event>, Vec<Payload>) {
    let mut dec = Decoder::new(rate).unwrap();
    let mut polar = PolarDecoders::default();
    let (mut events, mut payloads) = (Vec::new(), Vec::new());
    for &x in signal {
        if dec.push_real(x)
            && let Some(ev) = dec.process()
        {
            if ev == Event::Done {
                let cw = dec.take_codeword().unwrap();
                if let Some(p) = decode_codeword(&cw, &mut polar) {
                    payloads.push(p);
                }
            }
            events.push(ev);
        }
    }
    (events, payloads)
}

fn random(n: usize, seed: u32) -> Vec<u8> {
    let mut rng = Xorshift32::default();
    for _ in 0..seed {
        rng.next();
    }
    (0..n).map(|_| rng.next() as u8).collect()
}

/// Gaussian noise of standard deviation `sigma`.
fn add_noise(signal: &mut [f32], sigma: f32, seed: u32) {
    let mut rng = Xorshift32::default();
    for _ in 0..seed {
        rng.next();
    }
    for x in signal {
        let u1 = (rng.next() as f32 + 1.0) / (u32::MAX as f32 + 2.0);
        let u2 = rng.next() as f32 / u32::MAX as f32;
        *x += sigma * (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos();
    }
}

#[test]
fn pictures_at_every_rate() {
    for (k, &rate) in RATES.iter().enumerate() {
        let mode = Mode::Image(Mode::IMAGE_MODES[k * 3 % 8]);
        let data = random(5380, k as u32);
        let signal = transmit(rate, &request(mode, data.clone(), 1700));
        let (events, payloads) = receive(rate, &signal);
        assert!(
            matches!(&events[0], Event::Sync { mode: m, call, cfo_hz } if *m == mode && call == "DL1ABC" && (cfo_hz - 1700.0).abs() < 1.0),
            "{rate} Hz: {events:?}"
        );
        assert_eq!(payloads.len(), 1, "{rate} Hz {mode:?}: {events:?}");
        assert_eq!(payloads[0].data, data, "{rate} Hz {mode:?}");
        assert_eq!(payloads[0].flips, 0);
    }
}

#[test]
fn every_picture_mode_at_8_khz() {
    for mode in Mode::IMAGE_MODES.map(Mode::Image) {
        let data = random(5380, u32::from(mode.number()));
        let signal = transmit(8000, &request(mode, data.clone(), 1700));
        let (events, payloads) = receive(8000, &signal);
        assert_eq!(payloads.len(), 1, "{mode:?}: {events:?}");
        assert_eq!(payloads[0].data, data, "{mode:?}");
    }
}

#[test]
fn text_and_ping() {
    for (rate, text) in [(8000, "Hello from COFDMtv"), (48000, &"x".repeat(100) as &str), (16000, &"ä".repeat(85) as &str)] {
        let req = TxRequest { text_family: true, ..request(Mode::Text(0), text.as_bytes().to_vec(), 1500) };
        let signal = transmit(rate, &req);
        let (events, payloads) = receive(rate, &signal);
        assert_eq!(payloads.len(), 1, "{rate}: {events:?}");
        assert_eq!(payloads[0].data, text.as_bytes());
        assert_eq!(payloads[0].mode, Mode::text_for(text.len()).unwrap());
    }
    for text_family in [false, true] {
        let req = TxRequest { text_family, ..request(Mode::Ping, Vec::new(), 2000) };
        let (events, _) = receive(8000, &transmit(8000, &req));
        assert!(matches!(&events[..], [Event::Ping { call, .. }] if call == "DL1ABC"), "{events:?}");
    }
}

#[test]
fn pictures_through_noise() {
    // The signal's RMS is about 0.25: noise at 0.05 RMS in the whole 4 kHz band is
    // about 7 dB SNR in the signal's 2.4 kHz.
    let mode = Mode::Image(11);
    let data = random(5380, 99);
    let mut signal = transmit(8000, &request(mode, data.clone(), 1700));
    add_noise(&mut signal, 0.05, 7);
    let (events, payloads) = receive(8000, &signal);
    assert_eq!(payloads.len(), 1, "{events:?}");
    assert_eq!(payloads[0].data, data);
    assert!(payloads[0].flips > 0, "noise flips bits");
}
