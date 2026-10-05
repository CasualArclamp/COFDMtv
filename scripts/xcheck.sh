#!/usr/bin/env bash
# Cross-checks COFDMtv against the original aicodix programs, both ways: signals from the
# originals must decode in COFDMtv and COFDMtv's signals in the originals. Needs the
# reference programs (tools/cxx-reference/build.sh); runs locally only.
#
# usage: scripts/xcheck.sh
set -uo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
ref="$root/target/cxx-reference"
[ -x "$ref/shredpix_encode" ] || [ -x "$ref/shredpix_encode.exe" ] || { echo "build the reference first: tools/cxx-reference/build.sh"; exit 1; }
cargo build --release -q -p cofdmtv-core --example wavcodec || exit 1
rs="$root/target/release/examples/wavcodec"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
pass=0
fail=0
check() { # name, command producing output, expected substring
	local name="$1" out="$2" want="$3"
	if grep -qF -- "$want" <<<"$out"; then
		pass=$((pass + 1))
	else
		fail=$((fail + 1))
		echo "FAIL $name: wanted '$want', got:"
		sed 's/^/    /' <<<"$out"
	fi
}

head -c 5380 /dev/urandom >"$tmp/p.bin"
for rate in 8000 16000 32000 44100 48000; do
	for mode in 6 9 10 13; do
		carrier=1800
		# original encoder -> COFDMtv decoder
		"$ref/shredpix_encode" "$tmp/a.wav" $rate $mode DL1ABC $carrier 2 1 0 "$tmp/p.bin" 2>/dev/null
		out="$("$rs" decode "$tmp/a.wav" "$tmp/a")"
		check "shredpix->cofdmtv $rate mode $mode" "$out" "DONE flips=0"
		cmp -s "$tmp/a_1.bin" "$tmp/p.bin" || { fail=$((fail + 1)); echo "FAIL payload shredpix->cofdmtv $rate $mode"; }
		# COFDMtv encoder -> original decoder
		"$rs" encode "$tmp/b.wav" $rate $mode DL1ABC $carrier 2 1 0 "$tmp/p.bin" 2>/dev/null
		out="$("$ref/assempix_decode" "$tmp/b.wav" "$tmp/b")"
		check "cofdmtv->assempix $rate mode $mode" "$out" "DONE flips=0"
		cmp -s "$tmp/b_1.bin" "$tmp/p.bin" || { fail=$((fail + 1)); echo "FAIL payload cofdmtv->assempix $rate $mode"; }
	done
	for text in "Hi" "Rattlegram text from COFDMtv, about one hundred bytes long, to use the middle mode of the three...." "$(printf 'x%.0s' $(seq 1 170))"; do
		"$ref/rattlegram_codec" encode "$tmp/t.wav" $rate DL1ABC 1500 2 1 0 "$text" 2>/dev/null
		out="$("$rs" decode "$tmp/t.wav" "$tmp/t")"
		check "rattlegram->cofdmtv $rate ${#text} bytes" "$out" "text=$text"
		"$rs" encode "$tmp/u.wav" $rate text DL1ABC 1500 2 1 0 "$text" 2>/dev/null
		out="$("$ref/rattlegram_codec" decode "$tmp/u.wav")"
		check "cofdmtv->rattlegram $rate ${#text} bytes" "$out" "text=$text"
	done
	"$ref/shredpix_encode" "$tmp/p.wav" $rate 0 DL1ABC 1500 2 1 0 2>/dev/null
	check "shredpix ping->cofdmtv $rate" "$("$rs" decode "$tmp/p.wav" "$tmp/p")" "PING cfo=1500 call=DL1ABC"
	"$rs" encode "$tmp/q.wav" $rate 0 DL1ABC 1500 2 1 0 2>/dev/null
	check "cofdmtv ping->assempix $rate" "$("$ref/assempix_decode" "$tmp/q.wav" "$tmp/q")" "NOPE cfo=1500 mode=0 call=   DL1ABC"
	"$rs" encode "$tmp/r.wav" $rate textping DL1ABC 1500 2 1 0 2>/dev/null
	check "cofdmtv textping->rattlegram $rate" "$("$ref/rattlegram_codec" decode "$tmp/r.wav")" "PING cfo=1500 call=   DL1ABC"
done

# Channel layouts: analytic (I/Q) with a negative carrier, and the second channel.
"$ref/shredpix_encode" "$tmp/iq.wav" 48000 11 DL1ABC -1200 2 1 4 "$tmp/p.bin" 2>/dev/null
check "shredpix I/Q->cofdmtv" "$("$rs" decode "$tmp/iq.wav" "$tmp/iq" 4)" "DONE flips=0"
"$rs" encode "$tmp/iq2.wav" 48000 11 DL1ABC -1200 2 1 4 "$tmp/p.bin" 2>/dev/null
check "cofdmtv I/Q->assempix" "$("$ref/assempix_decode" "$tmp/iq2.wav" "$tmp/iq2" 4)" "DONE flips=0"
"$rs" encode "$tmp/r2.wav" 16000 12 DL1ABC 1700 2 1 2 "$tmp/p.bin" 2>/dev/null
check "cofdmtv right channel->assempix" "$("$ref/assempix_decode" "$tmp/r2.wav" "$tmp/r2" 2)" "DONE flips=0"

echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
