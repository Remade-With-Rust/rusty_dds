#!/bin/bash
# Build the natural ASTC corpus (plan section 6.1 a) with astcenc 5.7.1.
# Output: $OUT/*.astc (not committed; regenerate with this script).
#   ASTCENC=/root/astcws/rel571/inst/bin/astcenc-avx2 IMG=/root/astcws/astc-encoder/Test/Images \
#   OUT=/root/astcws/corpus ./corpus.sh
set -u
A=${ASTCENC:-/root/astcws/rel571/inst/bin/astcenc-avx2}
IMG=${IMG:-/root/astcws/astc-encoder/Test/Images}
OUT=${OUT:-/root/astcws/corpus}
J=${J:-8}
mkdir -p "$OUT" "$OUT/slices"
FP2="4x4 5x4 5x5 6x5 6x6 8x5 8x6 8x8 10x5 10x6 10x8 10x10 12x10 12x12"
FP3="3x3x3 4x3x3 4x4x3 4x4x4 5x4x4 5x5x4 5x5x5 6x5x5 6x6x5 6x6x6"

enc() { # mode in out fp preset [extra]
  local m=$1 in=$2 out=$3 fp=$4 pre=$5; shift 5
  [ -s "$out" ] && return 0
  "$A" "$m" "$in" "$out" "$fp" "$pre" -j "$J" -silent "$@" >/dev/null 2>&1 || echo "FAIL $m $in $fp $pre"
}

# Small LDR set: every 2D footprint, both LDR profiles, thorough + fastest.
for f in "$IMG"/Small/LDR-*/*.png "$IMG"/Small/LDRS-*/*.png; do
  b=$(basename "$f" .png)
  for fp in $FP2; do
    enc -cl "$f" "$OUT/s_${b}_${fp}_cl_thorough.astc" "$fp" -thorough
    enc -cs "$f" "$OUT/s_${b}_${fp}_cs_thorough.astc" "$fp" -thorough
    enc -cl "$f" "$OUT/s_${b}_${fp}_cl_fastest.astc" "$fp" -fastest
  done
done
# Small HDR set: HDR profiles.
for f in "$IMG"/Small/HDR-*/*.hdr; do
  b=$(basename "$f" .hdr)
  for fp in $FP2; do
    enc -ch "$f" "$OUT/s_${b}_${fp}_ch_thorough.astc" "$fp" -thorough
    enc -cH "$f" "$OUT/s_${b}_${fp}_cH_thorough.astc" "$fp" -thorough
  done
done
# LDR images through the HDR encoders (HDR endpoint modes on LDR content).
for f in "$IMG"/Small/LDR-RGBA/ldr-rgba-00.png "$IMG"/Small/LDR-RGB/ldr-rgb-00.png; do
  b=$(basename "$f" .png)
  for fp in 4x4 6x5 8x8 12x10; do
    enc -cH "$f" "$OUT/s_${b}_${fp}_cH_medium.astc" "$fp" -medium
  done
done
# 3D: 6-slice volumes built from Small images (all 256x256).
i=0
for f in "$IMG"/Small/LDR-RGB/ldr-rgb-0[0-5].png; do cp -f "$f" "$OUT/slices/ldr_$i.png"; i=$((i+1)); done
i=0
for f in "$IMG"/Small/LDR-RGBA/ldr-rgba-0[0-2].png "$IMG"/Small/LDRS-RGBA/ldrs-rgba-0[0-2].png; do cp -f "$f" "$OUT/slices/rgba_$i.png"; i=$((i+1)); done
for fp in $FP3; do
  enc -cl "$OUT/slices/ldr.png" "$OUT/v_ldr_${fp}_cl_medium.astc" "$fp" -medium -zdim 6
  enc -cs "$OUT/slices/rgba.png" "$OUT/v_rgba_${fp}_cs_medium.astc" "$fp" -medium -zdim 6
  enc -cH "$OUT/slices/rgba.png" "$OUT/v_rgba_${fp}_cH_fast.astc" "$fp" -fast -zdim 6
done
# Large natural textures: four footprints, medium.
for f in "$IMG"/Khronos/*/*.png; do
  b=$(basename "$f" .png)
  for fp in 4x4 6x6 8x8 12x12; do
    enc -cs "$f" "$OUT/k_${b}_${fp}_cs_medium.astc" "$fp" -medium
  done
done
for f in "$IMG"/HDRIHaven/HDR-RGB/*.hdr; do
  b=$(basename "$f" .hdr)
  for fp in 6x6 8x8; do
    enc -ch "$f" "$OUT/h_${b}_${fp}_ch_fast.astc" "$fp" -fast
  done
done
ls "$OUT"/*.astc | wc -l
du -sh "$OUT"
