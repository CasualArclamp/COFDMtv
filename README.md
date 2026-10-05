# COFDMtv

A desktop transceiver for the aicodix audio modems, in Rust, with the look of
[DecDRM](https://github.com/CasualArclamp/DecDRM):

- **COFDMTV pictures** — send and receive images like the Android apps
  [Shredpix](https://github.com/aicodix/shredpix) and
  [Assempix](https://github.com/aicodix/assempix): modes 6–13 (8PSK/QPSK, 1.6–3.2 kHz,
  9–24 s), your call sign, the "fancy header" on the waterfall, and pictures larger than
  one frame (Cauchy Reed–Solomon multi-frame, as Assempix accepts it);
- **COFDMTV text** — UTF-8 messages of up to 170 bytes in about a second, like
  [Rattlegram](https://github.com/aicodix/rattlegram), and pings;
- **modem datagrams** — texts and files over the
  [aicodix modem](https://github.com/aicodix/modem): BPSK to QAM4096, code rates 1/2 to
  5/6, short or normal frames, at 44.1 or 48 kHz; files in as many frames as they take
  (up to 1024: 250 kB in the smallest frames, 7 MB in the largest), extra frames making
  up for lost ones.

![COFDMtv receiving a recording: the spectrum, the waterfall with the call signs of two transmissions' fancy headers, the QPSK constellation, the picture received, and messages from Rattlegram and the modem](docs/images/receiver.png)

![The transmitter after sending a picture in three frames: the original and the WebP as it will arrive, mode, carrier and lead-in, and the status panel with the output spectrum](docs/images/transmitter.png)

COFDMTV is OFDM with 160 ms symbols (6.25 Hz carrier spacing) and a 1/8 guard interval,
differentially PSK modulated, protected by systematic polar codes with a CRC — see
[aicodix.de/cofdmtv](https://www.aicodix.de/cofdmtv/). The modem is coherent OFDM with
7.5 Hz spacing, pilots and polar codes of up to 65536 bits.

The modems are ports of the original C++, checked against it in both directions: every
picture and text mode, every sample rate, multi-frame pictures, and all 64 modem modes
decode in COFDMtv from the originals' signals and in the originals from COFDMtv's, and
COFDMtv's signals equal the originals' sample by sample.

## The program

`cofdmtv-gui` has two pages:

- **Receiver** — from a sound card or a recording (WAV, FLAC; mono, stereo or I/Q):
  input, sync and decode LEDs, the transmission being received (mode, call sign,
  carrier, SNR, progress), spectrum, waterfall (where the fancy header's call sign
  reads) and the payload's constellation; received pictures large and in a gallery,
  messages and pings, and files. Pictures and files are saved as Assempix names them
  (`20261005_213000_DL1ABC.jpg`), messages appended to `messages.txt`. COFDMTV and the
  modem are received at the same time (the modem at 44.1 and 48 kHz).
- **Transmitter** — a picture (scaled and compressed to fit one frame or several, with a
  preview of how it will arrive), a text, a ping, or data over the modem (a text or any
  file); mode, carrier, lead-in and fancy header; to a sound card or a WAV/FLAC file,
  mono, one channel or I/Q.

Drop a recording on the window to receive it, a picture to send it, any other file to
send it as data. Settings are kept in `%APPDATA%\cofdmtv\gui.toml` (Windows) or
`~/.config/cofdmtv/gui.toml`.

The command line does the same without a window:

```
cofdmtv rx recording.wav --out-dir received
cofdmtv rx --device "Line In" --duration 600
cofdmtv tx picture photo.jpg --call DL1ABC --mode 11 -o picture.wav
cofdmtv tx picture large.jpg --call DL1ABC --blocks 3 --extra 1 --device "Speakers"
cofdmtv tx text "Hello" --call DL1ABC --device "Speakers"
cofdmtv tx data --text "Hello over the modem" --call DL1ABC/P -o text.wav
cofdmtv tx data report.pdf --modulation qam64 --code-rate 2/3 --frame normal -o data.wav
cofdmtv devices
```

`cofdmtv --help` and `cofdmtv tx data --help` list the options.

## Building

With a current Rust toolchain (1.88 or later):

```
cargo build --release
```

gives `target/release/cofdmtv-gui` and `target/release/cofdmtv`. On Linux, cpal needs the
ALSA headers (`libasound2-dev` and `pkg-config` on Debian and Ubuntu).

Single-file Windows executables that need nothing installed (static C runtime):
`powershell -ExecutionPolicy Bypass -File scripts\build-portable.ps1` writes them to
`exe\`. Pushing a tag `vX.Y.Z` has GitHub Actions build them the same way, smoke-test
them and attach them to a draft release (`.github/workflows/release.yml`).

Tests: `cargo test --release --workspace` (loopback of every mode at every rate, signals
made by the originals in `tests/fixtures`, the engines end to end), and
`scripts/smoke-test.sh target/release OUT_DIR` (the CLI and the GUI end to end, as CI runs
it). `scripts/xcheck.sh` cross-checks against the original programs, built from clones of
the aicodix repositories by `tools/cxx-reference/build.sh` (see `docs/DESIGN.md`).

## Credits and license

The modems are ports of Ahmet Inan's aicodix C++ code (BSD Zero Clause License): DSP and
coding from [aicodix/dsp](https://github.com/aicodix/dsp) and
[aicodix/code](https://github.com/aicodix/code), the signals from the apps and the modem
above. The fancy header font is from Terminus (SIL Open Font License). The GUI and audio
code come from DecDRM.

COFDMtv is licensed under the GNU General Public License, version 2 or later.
