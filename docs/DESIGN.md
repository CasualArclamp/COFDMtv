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
  repository CasualArclamp/COFDMtv, CI on GitHub Actions (Linux and Windows). Made
  public at the user's request on 2026-10-06, with v0.1.2.

## Decisions on v2 picture modes (user, 2026-10-06)

"Putting the data modes, where you choose the code rate and the modcod, into the SSTV
modes — maybe we will call these v2 modes." Asked how, the user chose:

- **Signal**: the picture in aicodix modem frames (not a new COFDMTV signal), so every
  modulation, code rate and frame size of the modem is there, at 44.1 and 48 kHz,
  2400 Hz wide;
- **Modcod**: a free choice of modulation, code rate and frame size, as for data;
- **Extras**: COFDMTV's fancy header, the noise lead-in, and extra frames;
- **Size**: set by the air time the picture may take.

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
- **The modem** is aicodix/modem's `encode.cc`/`decode.cc`, defined at 44.1 and 48 kHz
  only: Schmidl–Cox on two back-to-back sync symbols, the base-40 meta symbol (call sign,
  mode, CRC-16, polar 256/72), payloads in non-systematic polar codes of 2^11…2^16 bits,
  BPSK…QAM4096, Theil–Sen tracking on the pilots, the PAPR scrambling number in a
  Hadamard code. Payload list size 32. Same sample positions and Es/N0 as the original.
- **One receiver for both systems**: at 44.1 and 48 kHz the receiver runs the modem's
  decoder beside COFDMTV's on the same samples (their sync symbols differ in length, so
  neither mistakes the other's); at the other rates only COFDMTV.
- **What goes over the modem**: a text in one datagram, zero padded, as the original
  encoder sends a file; a file in frames carrying COFDMTV's multi-frame (CRS) header, the
  chunk being the mode's datagram size (up to 1024 blocks) — a COFDMtv extension that
  gives the receiver the exact size and a CRC-32 and lets extra frames make up for lost
  ones (the original decoder writes each frame as it is). A received datagram is a part
  of such a file, else a picture if it is one (trimmed), else text if it is printable
  UTF-8, else a file (the whole datagram, as the original writes it).
- **Modem carriers** are multiples of 300 Hz from 1200 Hz (real signals) as the original
  encoder allows; the GUI keeps them up to 3 kHz unless "Carriers above 3 kHz" is on, as
  for COFDMTV. Modem call signs are base 40 (with `/`), COFDMTV's base 37.
- **License**: GPL-2.0-or-later, as DecDRM (whose GUI and audio code COFDMtv reuses).

## Verification

- `tools/cxx-reference/build.sh` builds the original programs (Shredpix encoder, Assempix
  decoder with its CRS bookkeeping, Rattlegram codec, the CRS chunk tool, the modem) with
  MSYS2's g++ from clones in `reference/` (git-ignored).
- `scripts/xcheck.sh`: 347 checks. Decoding both directions (originals → COFDMtv,
  COFDMtv → originals): COFDMTV at five rates, picture modes 6/9/10/13, three text
  lengths, pings, I/Q and stereo; the modem's 64 modes (eight modulations, four code
  rates, short and normal frames) at 44.1 and 48 kHz, with identical Es/N0 to the
  original decoder. Waveforms sample by sample: COFDMtv's COFDMTV signals equal the apps'
  within 1 LSB (rounding) for every picture mode, the three text modes and both pings at
  every rate, and the I/Q and one-channel layouts; the modem's too, except now and then
  one symbol whose PAPR scrambling differs (the encoders keep the first of 128 candidates
  below a PAPR of 5, else the lowest; a candidate within rounding of the threshold or of
  another goes either way, and the C++ is built with `-ffast-math`) — the scrambling's
  number travels on the pilots, so both decode alike, which the payload comparisons check.
- The apps' noise lead-in draws each carrier as `cmplx(nrz(noise_seq()), nrz(noise_seq()))`;
  C++ leaves the order of the two calls open, and the apps' compiler (Clang, Android NDK)
  draws I first, g++ Q first. COFDMtv draws I first; `build.sh` also builds copies of the
  two encoders that do (`*_ltr`) for the waveform comparison.
- Found by the waveform comparison (2026-10-05) and fixed: text was clipped like
  Shredpix clips (the larger of |re| and |im|) instead of like Rattlegram (the
  magnitude). Decoding had not minded either way. Multi-frame checked both ways by hand (2026-10-05): COFDMtv frames rebuild in the
  Assempix logic and vice versa, with a frame missing; CRS chunks are byte-identical to
  aicodix/crs.
- `scripts/sensitivity.sh`: identical impaired signals (aicodix/disorders: 23.5 Hz CFO,
  30 ppm SFO, optional multipath, AWGN) decoded by both. COFDMtv decodes at least as well
  everywhere; near threshold about 0.3–0.5 dB better (larger list).
- `tests/fixtures`: signals from the originals, decoded in CI (`tests/compat.rs`).

## Layout

- `crates/cofdmtv-core` — pure-Rust modems: `coding/` (CRC, MLS, xorshift, BCH + OSD,
  polar encoder and CA-SCL decoder, PSK, QAM, Hadamard, CRS over GF(2^16), base-37 and
  base-40 call signs, generated tables), `dsp/` (FFT, DC blocker, Hilbert, NCO, sliding
  buffers/sums, triggers, Theil–Sen, PAPR clipping, Schmidl–Cox), `cofdmtv/` (modes,
  sync, preamble, encoder, decoder, multi-frame), `modem/` (modes, encoder, decoder).
- `crates/cofdmtv-io` — DecDRM's `decdrm-io` without the audio player: WAV/FLAC reading
  (symphonia) and writing (hound, flacenc), rubato resampling, cpal sound cards behind
  lock-free ring buffers.
- `crates/cofdmtv-engine` — worker threads: `Receiver` (source → COFDMTV and modem
  decoders, spectrum, level → snapshots and events; a second thread list-decodes
  payloads, rebuilds multi-frame pictures and files and saves them as Assempix names
  them) and `Transmitter` (jobs, COFDMTV or modem → encoder → sound card or file).
- `crates/cofdmtv-pix` — pictures to send: EXIF orientation, scaling to Shredpix's pixel
  budgets within Assempix's 16…1024 sides, JPEG/PNG/WebP (libwebp, vendored) at the
  highest quality that fits; decoding received ones for display.
- `apps/cofdmtv-cli` — `cofdmtv rx|tx picture|text|ping|data|devices` (`tx picture --v2`
  for v2 picture modes).

## More choices

- The GUI is the workspace's default member, so `cargo run --release` starts it (a user
  on Arch got "could not determine which binary to run", 2026-10-06); CI, the release
  workflow and the scripts name their packages or pass `--workspace`.

- Sources at a rate the modems do not support are resampled to 48 kHz; the transmitter
  builds the signal at the sound card's rate when it is a rate of the system sent
  (COFDMTV's five, the modem's two; else 48 kHz and resamples), files at 48 kHz unless
  told otherwise.
- At the end of a recording the receiver feeds a second of silence, so that a
  transmission at the very end is still found (COFDMTV's synchroniser looks two symbols
  back, the modem's decoder runs about five symbols behind its input).
- Multi-frame state is kept after a picture is complete (further frames of it are
  "redundant", as in Assempix); a frame of another file starts afresh.
- Received pictures are trimmed of the payload's zero padding by format (WebP by its RIFF
  size, AVIF by its boxes, JPEG/PNG by the trailing zeros) before saving.
- Rattlegram clips every text symbol at about its RMS level in a time domain oversampled
  to about 32 kHz: more power on the air for a radio's amplifier, at the price of in-band
  distortion. The SNR a perfect text signal shows is therefore about 30 dB at 8 kHz,
  17 dB at 16 kHz and 11 dB at 32–48 kHz — in Rattlegram as in COFDMtv (measured with an
  SNR-printing build of Rattlegram's decoder); the polar code makes up for it. COFDMtv
  sends what Rattlegram sends.
- Live test (2026-10-05): CLI transmitter into VB-Audio cable A, CLI receiver on its
  output: text and picture received intact. Modem (same day): a QAM16 text, a 10594-byte JPEG in
  five QAM64 3/4 normal frames built at 44.1 kHz, then a Rattlegram text, into one
  receiver: all intact, the picture rebuilt from its first four frames.

## GUI (`apps/cofdmtv-gui`)

DecDRM's structure and look, reusing its pieces (panel helpers, LEDs, meter, log, ring
image, waterfall model, font fallbacks, settings store, screenshot automation):

- Top bar: COFDMtv · Receiver ▶ · Transmitter ▶ (a marker while running) · theme · Log.
- Receiver: source bar (sound card or recording, channel, real time, Start/Stop, save
  folder) → status strip (LEDs Input / Sync / Decode, state, mode, sender, carrier, SNR,
  symbol progress; level, position, counts) → plot tabs Overview / Spectrum / Waterfall /
  Constellation with a span choice (4 kHz, 8 kHz, full band; the waterfall keeps 6.25 Hz
  bins so the fancy header's call sign reads; the constellation shows the payload's
  modulation, PSK or QAM, on the I and Q axes alone (no grid), built up as BinModem's
  symbol scope does: the engine keeps
  the last symbols' points, a dozen per point of the modulation and at least 512
  (QAM4096: 49 152 points, the last 192 symbols; a new modulation starts afresh), and
  each is drawn as a faint square, so clusters grow where symbols keep landing; larger
  squares for modulations of up to 64 points, under their ideal points' crosses;
  scrolling zooms around the cursor up to 64×, dragging moves the view, a double-click
  or "All" shows it whole, and the points are drawn anew for the part shown, so a
  zoomed QAM4096 shows its clusters) →
  side panel: the latest or chosen picture, its facts and Open/Folder, a multi-frame
  progress card, the gallery, the messages, or the files.
- Transmitter: cards Station (call sign, checked live), Send (Picture with original and
  "as it will arrive" previews, format, size, frames + extra — for a v2 picture the air
  time + extra and what fits —, send-as-is; Text with a
  byte counter and the mode it takes; Ping; Data: a text or a file over the modem, with
  the frames it takes and extra frames), Signal (COFDMTV: mode, carrier within the range
  the mode allows, lead-in, fancy header; modem: modulation, code rate, frame size with
  bytes, duration and bit rate, carrier in 300 Hz steps; pictures choose between
  COFDMTV 6–13 and v2, which has the modem's rows plus lead-in and fancy header; all:
  carriers above 3 kHz),
  Output (sound card or file, rate, channels, level); status side: Transmission (state,
  the big Transmit/Stop button with the reason it is off, frame k of n, progress), Output
  (meter, destination, rates), Output spectrum, Sent.
- Pictures are prepared on a thread whenever an input changes; drag and drop: recordings
  to the receiver, pictures to the transmitter, other files to the transmitter as data. `--start`, `--transmit`, `--page`,
  `--no-audio`, `--config`, `--screenshot`, `--exit-after`, `--window-size` as in DecDRM.
- Default save folder `Pictures\COFDMtv` (Assempix saves to Pictures). Test runs must use
  `--config` with a `save_dir` under `out/` so nothing lands in the user's Pictures.
- Live GUI test (2026-10-05): GUI receiving from VB-Audio cable A while the CLI sent a
  text and a picture: both received, fancy headers legible; GUI `--transmit` of a
  three-frame picture to a file rebuilt by COFDMtv and by the Assempix logic.

## v2 picture modes

A picture in aicodix modem frames (`cofdmtv_engine::v2`):

- On the air: the lead-in's noise symbols (the transmitter's lead-in, converted to
  modem symbols of 41/300 s; the modem's own noise symbol is the last of them), the
  picture as a multi-frame file with the CRS header in frames of the chosen mode (any
  `blocks` of the frames rebuild it), then COFDMTV's fancy header — 11 lines and a
  silent symbol, drawn as after a Shredpix picture — at the modem's carrier, 3 dB up
  so that it is as loud as the frames. Extra lead-in symbols come from a running noise
  sequence: repeated symbols could look like a sync.
- The air time sets the frames: as many as fit in the time with the lead-in, the header
  and the extra frames (one at least; extra frames only when there are frames to
  spare), and the picture is compressed to fill their blocks.
- The receiver needs nothing new: modem frames rebuild the file, a picture is shown as
  one, labelled "v2 QAM64 1/2 normal" and so on.
- `ModemRequest` has the lead-in (`noise_symbols`) and the header (`fancy_header`);
  `ModemRequest::new` is the original's transmission (one noise symbol, no header), so
  the waveform cross-checks still compare with the original encoder.
- COFDMtv receives v2 pictures; Assempix does not (it hears no COFDMTV), and the
  original modem's decoder writes the frames as they come (CRS-coded).
- Checked: core round trips (lead-in and header leave the frames decodable, no false
  syncs, timing exact), an engine round trip with the first frame lost, CLI and GUI end
  to end (`tx picture --v2`, the GUI's v2 picture through a file into its receiver), the
  smoke test.

## Milestones

- [x] M0 — project, C++ reference harness, cross-check scripts.
- [x] M1 — COFDMTV core: pictures, text, ping, multi-frame; cross-checked, fixtures.
- [x] M2 — audio I/O (sound cards, WAV), engine (worker threads), pictures, CLI.
- [x] M3 — GUI receiver page (DecDRM look): sources, LED strip, spectrum, waterfall,
      constellation, received pictures/text, log.
- [x] M4 — GUI transmitter page: picture preparation (resize, JPEG/PNG/WebP fitted to the
      payload), multi-frame, text, ping; sound card or WAV.
- [x] M5 — aicodix modem datagrams: core (all 64 modes cross-checked both ways), engine
      (both decoders on one input), CLI `tx data`, GUI Data kind and Files tab.
- [x] M6 — GitHub repository and CI, README with screenshots, portable executables
      (`scripts/build-portable.ps1`; `release.yml` drafts a release from a pushed tag).
      v0.1.0 released 2026-10-06 (14e97f2; built, checked and smoke-tested by
      `release.yml`, hashes verified, published). v0.1.1 the same day (55e9142): the
      constellation builds up and zooms. v0.1.2 the same day (813ab31): the
      constellation without a grid, the README with pictures; the repository public.
- [x] M7 — v2 picture modes: pictures in aicodix modem frames with a free modcod, lead-in,
      fancy header and extra frames, sized by air time (core, engine, CLI, GUI).
      Released as v0.1.3 on 2026-10-06 (34e4f72).
