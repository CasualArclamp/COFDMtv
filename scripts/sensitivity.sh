#!/usr/bin/env bash
# Decoding performance of COFDMtv against the original decoders on identical impaired
# signals: the original encoders' signals with a frequency offset (23.5 Hz), a sample-rate
# offset (30 ppm), optionally multipath, and white noise at several levels (dBFS, as
# aicodix/disorders `awgn` takes it). Needs the reference programs and the disorders tools
# in target/cxx-reference (tools/cxx-reference/build.sh, then build aicodix/disorders).
#
# usage: scripts/sensitivity.sh [TRIALS] [multipath]
set -uo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
ref="$root/target/cxx-reference"
trials="${1:-4}"
multipath="${2:-}"
cargo build --release -q -p cofdmtv-core --example wavcodec || exit 1
rs="$root/target/release/examples/wavcodec"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
head -c 5380 /dev/urandom >"$tmp/p.bin"
text="Sensitivity check of COFDMtv against Rattlegram"

impair() { # in, out, level
	"$ref/cfo" "$tmp/c.wav" "$1" 23.5 2>/dev/null
	"$ref/sfo" "$tmp/s.wav" "$tmp/c.wav" 30 2>/dev/null
	local src="$tmp/s.wav"
	if [ -n "$multipath" ]; then
		"$ref/multipath" "$tmp/m.wav" "$tmp/s.wav" "$root/reference/disorders/multipath.txt" 10 2>/dev/null
		src="$tmp/m.wav"
	fi
	"$ref/awgn" "$2" "$src" "$3" 2>/dev/null
}

printf "%-10s %6s %10s %10s\n" signal level original cofdmtv
for what in ${SIGNALS:-6 11 13 text}; do
	if [ "$what" = text ]; then
		"$ref/rattlegram_codec" encode "$tmp/clean.wav" 8000 DL1ABC 1500 2 0 0 "$text" 2>/dev/null
	else
		"$ref/shredpix_encode" "$tmp/clean.wav" 8000 "$what" DL1ABC 1700 2 0 0 "$tmp/p.bin" 2>/dev/null
	fi
	for level in ${LEVELS:--24 -22 -20 -18 -16 -14}; do
		orig=0
		ours=0
		for _ in $(seq "$trials"); do
			impair "$tmp/clean.wav" "$tmp/n.wav" "$level"
			if [ "$what" = text ]; then
				"$ref/rattlegram_codec" decode "$tmp/n.wav" | grep -qF "text=$text" && orig=$((orig + 1))
				"$rs" decode "$tmp/n.wav" "$tmp/r" | grep -qF "text=$text" && ours=$((ours + 1))
			else
				rm -f "$tmp"/o_1.bin "$tmp"/r_1.bin
				"$ref/assempix_decode" "$tmp/n.wav" "$tmp/o" >/dev/null
				cmp -s "$tmp/o_1.bin" "$tmp/p.bin" && orig=$((orig + 1))
				"$rs" decode "$tmp/n.wav" "$tmp/r" >/dev/null
				cmp -s "$tmp/r_1.bin" "$tmp/p.bin" && ours=$((ours + 1))
			fi
		done
		printf "%-10s %6s %7s/%s %7s/%s\n" "mode $what" "$level" "$orig" "$trials" "$ours" "$trials"
	done
done
