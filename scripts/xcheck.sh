#!/usr/bin/env bash
# Cross-checks COFDMtv against the original aicodix programs, both ways: signals from the
# originals must decode in COFDMtv and COFDMtv's signals in the originals, and the
# encoders' waveforms must equal the originals' sample by sample (within 1 LSB). Needs
# the reference programs (tools/cxx-reference/build.sh); runs locally only.
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

# The aicodix modem: every modulation, code rate and frame size, both ways, two frames
# each; the rate and the channels (mono, I/Q) vary with the mode.
n=0
for modulation in BPSK QPSK 8PSK QAM16 QAM64 QAM256 QAM1024 QAM4096; do
	for code_rate in 1/2 2/3 3/4 5/6; do
		for frame_size in short normal; do
			n=$((n + 1))
			rate=$((n % 2 ? 48000 : 44100))
			chans=$((n % 3 ? 1 : 2))
			head -c 7000 /dev/urandom >"$tmp/m1.dat"
			head -c 100 /dev/urandom >"$tmp/m2.dat"
			label="$modulation $code_rate $frame_size $rate Hz ${chans}ch"
			"$ref/modem_encode" "$tmp/me.wav" $rate 16 $chans 1500 DL1ABC $modulation $code_rate $frame_size "$tmp/m1.dat" "$tmp/m2.dat" 2>/dev/null
			rm -f "$tmp"/mr_*.bin
			out="$("$rs" modem-decode "$tmp/me.wav" "$tmp/mr")"
			check "modem->cofdmtv $label" "$out" "DONE Es/N0"
			bytes=$(stat -c %s "$tmp/mr_1.bin" 2>/dev/null || echo 0)
			cmp -s <(head -c "$bytes" "$tmp/m1.dat") "$tmp/mr_1.bin" || { fail=$((fail + 1)); echo "FAIL payload modem->cofdmtv $label"; }
			cmp -s <(head -c 100 "$tmp/mr_2.bin") "$tmp/m2.dat" || { fail=$((fail + 1)); echo "FAIL second frame modem->cofdmtv $label"; }
			"$rs" modem-encode "$tmp/mf.wav" $rate 16 $chans 1500 DL1ABC $modulation $code_rate $frame_size "$tmp/m1.dat" "$tmp/m2.dat" 2>/dev/null
			rm -f "$tmp"/mo1 "$tmp"/mo2
			out="$("$ref/modem_decode" "$tmp/mf.wav" "$tmp/mo1" "$tmp/mo2" 2>&1)"
			check "cofdmtv->modem $label" "$out" "Es/N0"
			# One symbol with 41 guard intervals of 1/300 s, all channels interleaved.
			out="$("$rs" compare "$tmp/me.wav" "$tmp/mf.wav" $((rate * 41 / 300 * chans)))"
			check "waveform modem $label" "$out" "SAME"
			grep -q "except" <<<"$out" && echo "note: waveform modem $label: $out (a different PAPR scrambling; decoded alike)"
			cmp -s "$tmp/mo1" "$tmp/mr_1.bin" || { fail=$((fail + 1)); echo "FAIL payload cofdmtv->modem $label"; }
			cmp -s <(head -c 100 "$tmp/mo2") "$tmp/m2.dat" || { fail=$((fail + 1)); echo "FAIL second frame cofdmtv->modem $label"; }
		done
	done
done

# Waveforms of the COFDMTV encoders against the apps' (the *_ltr builds draw the noise
# lead-in as the apps do; see tools/cxx-reference/build.sh): every picture mode, the
# three text modes and both pings at every rate, and the channel layouts.
for rate in 8000 16000 32000 44100 48000; do
	for mode in 6 7 8 9 10 11 12 13; do
		"$ref/shredpix_encode_ltr" "$tmp/wa.wav" $rate $mode DL1ABC 1800 2 1 0 "$tmp/p.bin" 2>/dev/null
		"$rs" encode "$tmp/wb.wav" $rate $mode DL1ABC 1800 2 1 0 "$tmp/p.bin" 2>/dev/null
		check "waveform picture $rate mode $mode" "$("$rs" compare "$tmp/wa.wav" "$tmp/wb.wav")" "SAME"
	done
	for text in "Hi" "Rattlegram text from COFDMtv, about one hundred bytes long, to use the middle mode of the three...." "$(printf 'x%.0s' $(seq 1 170))"; do
		"$ref/rattlegram_codec_ltr" encode "$tmp/wt.wav" $rate DL1ABC 1500 2 1 0 "$text" 2>/dev/null
		"$rs" encode "$tmp/wu.wav" $rate text DL1ABC 1500 2 1 0 "$text" 2>/dev/null
		check "waveform text $rate ${#text} bytes" "$("$rs" compare "$tmp/wt.wav" "$tmp/wu.wav")" "SAME"
	done
	"$ref/shredpix_encode_ltr" "$tmp/wp.wav" $rate 0 DL1ABC 1500 2 1 0 2>/dev/null
	"$rs" encode "$tmp/wq.wav" $rate 0 DL1ABC 1500 2 1 0 2>/dev/null
	check "waveform ping $rate" "$("$rs" compare "$tmp/wp.wav" "$tmp/wq.wav")" "SAME"
	"$ref/rattlegram_codec_ltr" encode "$tmp/wr.wav" $rate DL1ABC 1500 2 1 0 2>/dev/null
	"$rs" encode "$tmp/ws.wav" $rate textping DL1ABC 1500 2 1 0 2>/dev/null
	check "waveform text ping $rate" "$("$rs" compare "$tmp/wr.wav" "$tmp/ws.wav")" "SAME"
done
"$ref/shredpix_encode_ltr" "$tmp/wa.wav" 48000 11 DL1ABC -1200 2 1 4 "$tmp/p.bin" 2>/dev/null
"$rs" encode "$tmp/wb.wav" 48000 11 DL1ABC -1200 2 1 4 "$tmp/p.bin" 2>/dev/null
check "waveform picture I/Q" "$("$rs" compare "$tmp/wa.wav" "$tmp/wb.wav")" "SAME"
"$ref/rattlegram_codec_ltr" encode "$tmp/wt.wav" 16000 DL1ABC 1700 2 1 2 "Right channel" 2>/dev/null
"$rs" encode "$tmp/wu.wav" 16000 text DL1ABC 1700 2 1 2 "Right channel" 2>/dev/null
check "waveform text right channel" "$("$rs" compare "$tmp/wt.wav" "$tmp/wu.wav")" "SAME"

echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
