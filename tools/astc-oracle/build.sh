#!/bin/sh
# Build the astc-oracle against an astcenc 5.7.1 build tree. WSL/Linux only.
#   ASTCENC=/root/astcws/rel571 ./build.sh   -> ./oracle
set -eu
ASTCENC=${ASTCENC:-/root/astcws/rel571}
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${OUT:-$HERE/oracle}
# Same ISA defines as the astcenc-none static library (scalar reference path).
g++ -O2 -std=c++14 -flto=auto -ffp-contract=off -fno-math-errno \
    -DASTCENC_AVX=0 -DASTCENC_F16C=0 -DASTCENC_NEON=0 -DASTCENC_POPCNT=0 -DASTCENC_SSE=0 -DASTCENC_SVE=0 \
    -I"$ASTCENC/Source" "$HERE/oracle.cpp" "$ASTCENC/build/Source/libastcenc-none-static.a" \
    -pthread -o "$OUT"
echo "built $OUT"
