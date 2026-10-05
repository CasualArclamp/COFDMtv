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
                let req = ModemRequest { mode, call_sign: "DL1ABC/P".into(), carrier_hz: 1500, frames: frames.clone() };
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
