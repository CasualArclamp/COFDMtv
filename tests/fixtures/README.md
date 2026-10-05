# Test fixtures

Signals made by the **original** aicodix programs (built by
`tools/cxx-reference/build.sh`), so that the tests check COFDMtv against Shredpix,
Assempix and Rattlegram without needing a C++ compiler. All 8 kHz, mono, 16-bit, with
the reference programs' second of silence on either side cut to a quarter (text: 0.25 s).

| File | Made with |
|---|---|
| `testcard.jpg` | 320×240 JPEG, 5345 bytes (fits one picture payload); drawn with Pillow |
| `testcard_large.jpg` | 640×480 JPEG, 10594 bytes (two blocks of a multi-frame picture) |
| `shredpix_mode12.wav` | `shredpix_encode … 8000 12 DL1ABC 1700 1 1 0 testcard.jpg` (one noise symbol, fancy header) |
| `shredpix_ping.wav` | `shredpix_encode … 8000 0 DL1ABC 1500 1 0 0` |
| `rattlegram_text.txt` | the text of the next file (UTF-8) |
| `rattlegram_text.wav` | `rattlegram_codec encode … 8000 DL1ABC 1600 1 0 0 @rattlegram_text.txt` (mode 16) |
| `shredpix_multiframe.wav` | `crs_chunks testcard_large.jpg 5380 c0 c1 c2`, then frames `c2` and `c1` by `shredpix_encode … 8000 10 DL1ABC 1700 1 0 0` (frame `c0` "lost") |

The pictures are COFDMtv's own and may be used freely.
