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

## Milestones

- [x] M0 — project, C++ reference harness, cross-check scripts.
- [x] M1 — COFDMTV core: pictures, text, ping, multi-frame; cross-checked, fixtures.
- [ ] M2 — audio I/O (sound cards, WAV), engine (worker threads), CLI.
- [ ] M3 — GUI receiver page (DecDRM look): sources, LED strip, spectrum, waterfall,
      constellation, received pictures/text, log.
- [ ] M4 — GUI transmitter page: picture preparation (resize, JPEG/PNG/WebP fitted to the
      payload), multi-frame, text, ping; sound card or WAV.
- [ ] M5 — aicodix modem datagrams: core, CLI, GUI.
- [ ] M6 — GitHub repository, CI, README with screenshots, portable executables.
