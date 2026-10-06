//! Modem encoder → decoder: every mode, both rates, several frames in one transmission.

use cofdmtv_core::coding::xorshift::Xorshift32;
use cofdmtv_core::dsp::Cplx;
use cofdmtv_core::modem::{CodeRate, Datagram, ModemDecoder, ModemEncoder, ModemEvent, ModemMode, ModemRequest, Modulation, decode_datagram};

fn transmit(rate: u32, req: &ModemRequest) -> Vec<Cplx> {
    let mut enc = ModemEncoder::new(rate).unwrap();
    enc.configure(req).unwrap();
    let mut out = vec![Cplx::new(0.0, 0.0); rate as usize / 2];
    let mut chunks = 0;
    while let Some(c) = enc.next_chunk() {
        out.extend_from_slice(c);
        chunks += 1;
    }
    assert_eq!(chunks, enc.total_chunks());
    // The decoder reads symbols from the start of a five-symbol buffer: a second of
    // silence lets the last one through.
    out.extend(std::iter::repeat_n(Cplx::new(0.0, 0.0), rate as usize));
    out
}

fn receive(rate: u32, signal: &[Cplx], real: bool) -> (Vec<ModemEvent>, Vec<Datagram>) {
    let mut dec = ModemDecoder::new(rate).unwrap();
    let mut decoder = None;
    let (mut events, mut got) = (Vec::new(), Vec::new());
    for &z in signal {
        let ev = if real { dec.push_real(z.re) } else { dec.push(z) };
        if let Some(ev) = ev {
            if ev == ModemEvent::Done {
                got.extend(decode_datagram(&dec.take_codeword().unwrap(), &mut decoder));
            }
            events.push(ev);
        }
    }
    (events, got)
}

fn data(n: usize, seed: u32) -> Vec<u8> {
    let mut rng = Xorshift32::default();
    for _ in 0..seed {
        rng.next();
    }
    (0..n).map(|_| rng.next() as u8).collect()
}

#[test]
fn every_mode_round_trips() {
    for (n, modulation) in Modulation::ALL.into_iter().enumerate() {
        for (r, rate) in CodeRate::ALL.into_iter().enumerate() {
            for normal in [false, true] {
                // Normal frames of the big modes take a while: one rate each.
                if normal && r != n % 4 {
                    continue;
                }
                let mode = ModemMode { modulation, rate, normal };
                let fs = if (n + r) % 2 == 0 { 48000 } else { 44100 };
                let frames = vec![data(mode.data_bytes(), 1), data(mode.data_bytes() / 2, 2)];
                let req = ModemRequest::new(mode, "DL1ABC/P", 1500, frames.clone());
                let signal = transmit(fs, &req);
                let (events, got) = receive(fs, &signal, (n + r) % 3 != 0);
                assert_eq!(got.len(), 2, "{} at {fs}: {events:?}", mode.label());
                assert_eq!(got[0].data, frames[0], "{}", mode.label());
                let mut second = frames[1].clone();
                second.resize(mode.data_bytes(), 0);
                assert_eq!(got[1].data, second, "{}", mode.label());
                assert_eq!(got[0].call, "DL1ABC/P");
                assert!(matches!(&events[0], ModemEvent::Sync { cfo_hz, .. } if (cfo_hz - 1500.0).abs() < 1.0), "{events:?}");
            }
        }
    }
}

/// A v2 picture's transmission: a lead-in of noise symbols, two frames, the fancy header.
/// The frames decode as ever, the header follows the last one, and the timing is exact.
#[test]
fn lead_in_and_fancy_header() {
    use cofdmtv_core::modem::transmission_seconds;
    let mode = ModemMode { modulation: Modulation::Qam64, rate: CodeRate::TwoThirds, normal: false };
    let frames = vec![data(mode.data_bytes(), 3), data(mode.data_bytes(), 4)];
    for fs in [44100, 48000] {
        let plain = ModemRequest::new(mode, "DL1ABC/P", 1800, frames.clone());
        let req = ModemRequest { noise_symbols: 8, fancy_header: true, ..plain.clone() };
        let mut enc = ModemEncoder::new(fs).unwrap();
        enc.configure(&req).unwrap();
        let mut samples = 0;
        let mut chunks = 0;
        while let Some(c) = enc.next_chunk() {
            samples += c.len();
            chunks += 1;
        }
        assert_eq!(chunks, enc.total_chunks());
        let seconds = transmission_seconds(mode, 2, 8, true);
        assert!((samples as f64 / f64::from(fs) - seconds).abs() < 1e-9, "{samples} samples, {seconds} s");
        assert!((enc.total_seconds() - seconds).abs() < 1e-9);
        enc.configure(&plain).unwrap();
        assert!((enc.total_seconds() - transmission_seconds(mode, 2, 1, false)).abs() < 1e-9);
        assert!((seconds - transmission_seconds(mode, 2, 1, false) - 7.0 * 41.0 / 300.0 - 2.16).abs() < 1e-9);
        let signal = transmit(fs, &req);
        let (events, got) = receive(fs, &signal, true);
        assert_eq!(got.len(), 2, "{events:?}");
        assert_eq!((got[0].data.clone(), got[1].data.clone()), (frames[0].clone(), frames[1].clone()));
        assert_eq!(events.iter().filter(|e| matches!(e, ModemEvent::Sync { .. })).count(), 2, "no false syncs in the lead-in or the header: {events:?}");
    }
}
