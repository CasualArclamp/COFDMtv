# COFDMtv

A desktop transceiver for the aicodix audio modems, in Rust:

- **COFDMTV pictures** — send and receive images like the Android apps
  [Shredpix](https://github.com/aicodix/shredpix) and
  [Assempix](https://github.com/aicodix/assempix): modes 6–13 (8PSK/QPSK, 1.6–3.2 kHz,
  9–24 s), your call sign, the "fancy header" on the waterfall, and pictures larger than
  one frame (Cauchy Reed–Solomon multi-frame, as Assempix accepts it);
- **COFDMTV text** — UTF-8 messages of up to 170 bytes in about a second, like
  [Rattlegram](https://github.com/aicodix/rattlegram), and pings;
- **modem datagrams** — files over the [aicodix modem](https://github.com/aicodix/modem)
  (in progress).

COFDMTV is OFDM with 160 ms symbols (6.25 Hz carrier spacing) and a 1/8 guard interval,
differentially PSK modulated, protected by systematic polar codes with a CRC — see
[aicodix.de/cofdmtv](https://www.aicodix.de/cofdmtv/).

**Status:** the modem core is complete and checked against the original C++ in both
directions (every picture and text mode, every sample rate, multi-frame); the desktop
application is being built.

## Building

```
cargo test --release -p cofdmtv-core
cargo run --release -p cofdmtv-core --example wavcodec -- decode recording.wav out
```

## Credits and license

The modems are ports of Ahmet Inan's aicodix C++ code (BSD Zero Clause License): DSP and
coding from [aicodix/dsp](https://github.com/aicodix/dsp) and
[aicodix/code](https://github.com/aicodix/code), the signal from the apps above. The
fancy header font is from Terminus (SIL Open Font License).

COFDMtv is licensed under the GNU General Public License, version 2 or later.
