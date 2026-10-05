#!/usr/bin/env bash
# Builds the reference programs from the original aicodix sources, for checking COFDMtv
# against them (see README.md here). Needs g++ (on Windows: MSYS2's mingw64 g++) and
# the aicodix repositories cloned into reference/ (git-ignored):
#
#   git clone https://github.com/aicodix/shredpix   reference/shredpix
#   git clone https://github.com/aicodix/assempix   reference/assempix
#   git clone https://github.com/aicodix/rattlegram reference/rattlegram
#   git clone https://github.com/aicodix/modem      reference/modem
#   git clone https://github.com/aicodix/code       reference/code
#   git clone https://github.com/aicodix/dsp        reference/dsp
#
# Output: target/cxx-reference/{shredpix_encode,assempix_decode,rattlegram_codec,
# crs_chunks,modem_encode,modem_decode}(.exe)
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
ref="$root/reference"
here="$root/tools/cxx-reference"
out="$root/target/cxx-reference"
mkdir -p "$out"
if [ -d /c/msys64/mingw64/bin ]; then
	export PATH="/c/msys64/mingw64/bin:$PATH"
fi
CXX="${CXX:-g++}"
# -msse4.1 picks the same SIMD width (4 floats) as the apps on ARM NEON phones; static
# linking keeps the MinGW runtime DLLs out of the way.
FLAGS=(-std=c++17 -O2 -msse4.1 -static -w)
"$CXX" "${FLAGS[@]}" -I"$here" -I"$ref/shredpix/app/src/main/cpp" "$here/shredpix_encode.cc" -o "$out/shredpix_encode"
"$CXX" "${FLAGS[@]}" -I"$here" -I"$ref/assempix/app/src/main/cpp" "$here/assempix_decode.cc" -o "$out/assempix_decode"
"$CXX" "${FLAGS[@]}" -I"$here" -I"$ref/rattlegram/app/src/main/cpp" "$here/rattlegram_codec.cc" -o "$out/rattlegram_codec"
"$CXX" "${FLAGS[@]}" -I"$here" -I"$ref/code" "$here/crs_chunks.cc" -o "$out/crs_chunks"
"$CXX" "${FLAGS[@]}" -ffast-math -I"$ref/modem" -I"$ref/dsp" -I"$ref/code" "$ref/modem/encode.cc" -o "$out/modem_encode"
"$CXX" "${FLAGS[@]}" -ffast-math -I"$ref/modem" -I"$ref/dsp" -I"$ref/code" "$ref/modem/decode.cc" -o "$out/modem_decode"
echo "built into $out"
