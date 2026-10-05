# COFDMtv — design notes

A desktop program, written in Rust, for the aicodix audio modems, looking and working
like DecDRM (`F:\DRM`, github.com/CasualArclamp/DecDRM).

## Decisions (user, 2026-10-05)

- **Scope**, transmit and receive:
  - **COFDMTV pictures**, compatible with the Android apps Shredpix (sender) and Assempix
    (receiver): modes 6–13, call sign, fancy header, noise lead-in, and multi-frame
    pictures (Cauchy Reed–Solomon, "CRS") as Assempix accepts them;
  - **COFDMTV text**, compatible with Rattlegram: UTF-8 messages up to 170 bytes (modes
    14–16) and pings;
  - **aicodix modem datagrams** (github.com/aicodix/modem): files in frames of BPSK…QAM4096
    at code rates 1/2…5/6, compatible with its `encode`/`decode` tools.
- **Look**: the same as DecDRM — eframe/egui, dark/light theme, top bar with page tabs,
  LED status strip, egui_plot spectrum and constellations, waterfall, cards on the
  transmitter page, log panel.
- **Version control**: git with a commit per milestone, pushed to a private GitHub
  repository CasualArclamp/COFDMtv, CI on GitHub Actions (Linux and Windows).

## Choices made while building

- **Pure Rust port** of the C++ (aicodix, BSD Zero Clause License), as DecDRM ported Dream:
  one static executable, no C++ toolchain to build it. Each Rust module names the C++ file
  it follows.
- **`f32` arithmetic** like the originals (the signals are a few kHz wide and coded;
  staying with the original's arithmetic keeps the two comparable).
- **One COFDMTV receiver** for pings, pictures and text: all share the sync symbol and the
  BCH-coded preamble. It works block by block like Rattlegram's `feed`/`process`; for
  pictures the block after the preamble is the pilot (Assempix), for text the preamble is
  the reference itself (Rattlegram). Equivalent sample positions to both apps.
- **Polar list sizes** 16 (pictures, N = 65536) and 32 (text, N = 2048); the apps use 4/8
  and 16/32 by the phone's SIMD width. Rate-0 shortcuts are taken at exactly the tree nodes
  where the original takes them (the shortcut's metric differs from bit-by-bit decoding,
  so this keeps the decoders equivalent).
- **Soft bits** are not quantised to `i8` for text (Rattlegram does); the SNR estimate that
  scales them is capped at 40 dB so a noiseless loopback cannot produce infinities.
- **Sample rates**: COFDMTV at 8, 16, 32, 44.1 and 48 kHz natively (the symbol is 160 ms
  at every rate); other device rates get resampled.
- **License**: GPL-2.0-or-later, as DecDRM (whose GUI and audio code COFDMtv reuses).

## Verification

- `tools/cxx-reference/build.sh` builds the original programs (Shredpix encoder, Assempix
  decoder with its CRS bookkeeping, Rattlegram codec, the CRS chunk tool, the modem) with
  MSYS2's g++ from clones in `reference/` (git-ignored).
- `scripts/xcheck.sh`: 88 checks, both directions (originals → COFDMtv, COFDMtv →
  originals), five rates, picture modes 6/9/10/13, three text lengths, pings, I/Q and
  stereo. Multi-frame checked both ways by hand (2026-10-05): COFDMtv frames rebuild in the
  Assempix logic and vice versa, with a frame missing; CRS chunks are byte-identical to
  aicodix/crs.
- `scripts/sensitivity.sh`: identical impaired signals (aicodix/disorders: 23.5 Hz CFO,
  30 ppm SFO, optional multipath, AWGN) decoded by both. COFDMtv decodes at least as well
  everywhere; near threshold about 0.3–0.5 dB better (larger list).
- `tests/fixtures`: signals from the originals, decoded in CI (`tests/compat.rs`).

## Layout

- `crates/cofdmtv-core` — pure-Rust modems: `coding/` (CRC, MLS, xorshift, BCH + OSD,
  polar encoder and CA-SCL decoder, PSK, CRS over GF(2^16), base-37 call signs,
  generated tables), `dsp/` (FFT, DC blocker, Hilbert, NCO, sliding buffers/sums,
  triggers, Theil–Sen, PAPR clipping), `cofdmtv/` (modes, sync, preamble, encoder,
  decoder, multi-frame).
- `crates/cofdmtv-io` — DecDRM's `decdrm-io` without the audio player: WAV/FLAC reading
  (symphonia) and writing (hound, flacenc), rubato resampling, cpal sound cards behind
  lock-free ring buffers.
- `crates/cofdmtv-engine` — worker threads: `Receiver` (source → decoder, spectrum,
  level → snapshots and events; a second thread list-decodes payloads, rebuilds
  multi-frame pictures and saves them as Assempix names them) and `Transmitter` (jobs →
  encoder → sound card or file).
- `crates/cofdmtv-pix` — pictures to send: EXIF orientation, scaling to Shredpix's pixel
  budgets within Assempix's 16…1024 sides, JPEG/PNG/WebP (libwebp, vendored) at the
  highest quality that fits; decoding received ones for display.
- `apps/cofdmtv-cli` — `cofdmtv rx|tx picture|text|ping|devices`.

## More choices

- Sources at a rate the modems do not support are resampled to 48 kHz; the transmitter
  builds the signal at the sound card's rate when it is a COFDMTV rate (else 48 kHz and
  resamples), files at 48 kHz unless told otherwise.
- At the end of a recording the receiver feeds half a second of silence, so that a
  transmission at the very end is still found (the synchroniser looks two symbols back).
- Multi-frame state is kept after a picture is complete (further frames of it are
  "redundant", as in Assempix); a frame of another file starts afresh.
- Received pictures are trimmed of the payload's zero padding by format (WebP by its RIFF
  size, AVIF by its boxes, JPEG/PNG by the trailing zeros) before saving.
- Live test (2026-10-05): CLI transmitter into VB-Audio cable A, CLI receiver on its
  output: text and picture received intact.

## GUI (`apps/cofdmtv-gui`)

DecDRM's structure and look, reusing its pieces (panel helpers, LEDs, meter, log, ring
image, waterfall model, font fallbacks, settings store, screenshot automation):

- Top bar: COFDMtv · Receiver ▶ · Transmitter ▶ (a marker while running) · theme · Log.
- Receiver: source bar (sound card or recording, channel, real time, Start/Stop, save
  folder) → status strip (LEDs Input / Sync / Decode, state, mode, sender, carrier, SNR,
  symbol progress; level, position, counts) → plot tabs Overview / Spectrum / Waterfall /
  Constellation with a span choice (4 kHz, 8 kHz, full band; the waterfall keeps 6.25 Hz
  bins so the fancy header's call sign reads) → side panel: the latest or chosen picture,
  its facts and Open/Folder, a multi-frame progress card, the gallery or the messages.
- Transmitter: cards Station (call sign, checked live), Send (Picture with original and
  "as it will arrive" previews, format, size, frames + extra, send-as-is; Text with a
  byte counter and the mode it takes; Ping), Signal (mode, carrier within the range the
  mode allows, lead-in, fancy header, carriers above 3 kHz), Output (sound card or file,
  rate, channels, level); status side: Transmission (state, the big Transmit/Stop button
  with the reason it is off, frame k of n, progress), Output (meter, destination, rates),
  Output spectrum, Sent.
- Pictures are prepared on a thread whenever an input changes; drag and drop: recordings
  to the receiver, pictures to the transmitter. `--start`, `--transmit`, `--page`,
  `--no-audio`, `--config`, `--screenshot`, `--exit-after`, `--window-size` as in DecDRM.
- Default save folder `Pictures\COFDMtv` (Assempix saves to Pictures). Test runs must use
  `--config` with a `save_dir` under `out/` so nothing lands in the user's Pictures.
- Live GUI test (2026-10-05): GUI receiving from VB-Audio cable A while the CLI sent a
  text and a picture: both received, fancy headers legible; GUI `--transmit` of a
  three-frame picture to a file rebuilt by COFDMtv and by the Assempix logic.

## Milestones

- [x] M0 — project, C++ reference harness, cross-check scripts.
- [x] M1 — COFDMTV core: pictures, text, ping, multi-frame; cross-checked, fixtures.
- [x] M2 — audio I/O (sound cards, WAV), engine (worker threads), pictures, CLI.
- [x] M3 — GUI receiver page (DecDRM look): sources, LED strip, spectrum, waterfall,
      constellation, received pictures/text, log.
- [x] M4 — GUI transmitter page: picture preparation (resize, JPEG/PNG/WebP fitted to the
      payload), multi-frame, text, ping; sound card or WAV.
- [ ] M5 — aicodix modem datagrams: core, CLI, GUI.
- [ ] M6 — GitHub repository, CI, README with screenshots, portable executables.
