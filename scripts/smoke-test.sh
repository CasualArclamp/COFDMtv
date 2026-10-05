#!/usr/bin/env bash
# Smoke test of the COFDMtv executables, for CI (Linux, or Git Bash on Windows):
#
#   scripts/smoke-test.sh BIN_DIR OUT_DIR
#
# BIN_DIR holds cofdmtv and cofdmtv-gui. The CLI transmits a picture, a text and a
# multi-frame picture into files and receives them; the GUI receives the picture file
# and saves a screenshot (headless under xvfb-run on Linux; on Windows it needs an OpenGL
# driver, Mesa on GitHub's runners). OUT_DIR receives the receiver output and the
# screenshot.
set -euo pipefail

if [ $# -lt 2 ]; then
    echo "usage: $0 BIN_DIR OUT_DIR" >&2
    exit 2
fi
bin=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
repo=$(cd "$(dirname "$0")/.." && pwd)
fixtures="$repo/tests/fixtures"
work=$(mktemp -d)
cd "$work"

"$bin/cofdmtv" --version
"$bin/cofdmtv-gui" --version
"$bin/cofdmtv" devices || echo "(no sound cards here)"

echo "== picture: transmit, receive"
"$bin/cofdmtv" tx picture "$fixtures/testcard.jpg" --call CI1TEST --mode 12 --rate 8000 -o picture.wav
"$bin/cofdmtv" rx picture.wav --out-dir "$out/received" | tee "$out/rx_picture.txt"
grep -E "CI1TEST +picture: 5345 bytes, JPEG" "$out/rx_picture.txt"

echo "== text: transmit, receive"
"$bin/cofdmtv" tx text "Smoke test: Grüße ✓" --call CI1TEST --rate 16000 -o text.wav
"$bin/cofdmtv" rx text.wav --out-dir "$out/received" | tee "$out/rx_text.txt"
grep -F "text: Smoke test: Grüße ✓" "$out/rx_text.txt"

echo "== multi-frame picture (WebP, two blocks and an extra frame, I/Q at 48 kHz)"
"$bin/cofdmtv" tx picture "$fixtures/testcard_large.jpg" --call CI1TEST --mode 10 --blocks 2 --extra 1 --recompress --channel iq -o multi.wav
"$bin/cofdmtv" rx multi.wav --channel iq --out-dir "$out/received" | tee "$out/rx_multi.txt"
grep -E "CI1TEST +picture: [0-9]+ bytes, WebP" "$out/rx_multi.txt"

echo "== GUI: receive the picture file, screenshot"
cat > gui.toml <<'TOML'
save_dir = "gui-received"
realtime = false
TOML
gui=("$bin/cofdmtv-gui")
if [ "$(uname -s)" = Linux ]; then
    gui=(xvfb-run -a -s "-screen 0 1280x800x24" "$bin/cofdmtv-gui")
fi
"${gui[@]}" picture.wav --start --no-audio --config gui.toml --exit-after 8 --screenshot "$out/gui.png"
test -s "$out/gui.png"
ls gui-received/*.jpg
echo "smoke test passed"
