#!/bin/bash
# Final wall check: branch vs 0.11.0, same bench source, rusty_alloc, ABBA,
# pinned, thread CPU clock, with null arms (0.11.0 vs a byte copy of itself).
S=$(dirname "$(readlink -f "$0")")
CP=/root/astcws/cp; W=$CP/wall; mkdir -p $W/arms
cp $CP/tgt-base/release/astc_bench $W/arms/base
cp $CP/tgt-work/release/astc_bench $W/arms/work
cp $CP/tgt-base/release/astc_bench $W/arms/null
md5sum $W/arms/* | awk '{print $1, $2}'
D=/root/astcws/out/dbench
DEC=""
for m in m1 m2 m3 m4; do for fp in 4x4 6x6 8x8 12x12; do DEC="$DEC $D/${m}_$fp.astc"; done; done
K=/root/astcws/kodak
IMG="$K/kodim01.png $K/kodim03.png $K/kodim08.png $K/kodim15.png"
echo "### decode: base (A) vs branch (B)"; date
bash $S/abba.sh $W/dec 10 $W/arms/base $W/arms/work --time-decode 7 $DEC
echo "### decode null: base (A) vs base copy (B)"; date
bash $S/abba.sh $W/decnull 10 $W/arms/base $W/arms/null --time-decode 7 $DEC
echo "### encode fast: base vs branch"; date
bash $S/abba.sh $W/encf 6 $W/arms/base $W/arms/work --time-encode fast 4x4,6x6,8x8,12x12 3 $IMG
echo "### encode medium: base vs branch"; date
bash $S/abba.sh $W/encm 6 $W/arms/base $W/arms/work --time-encode medium 4x4,6x6,8x8,12x12 3 $IMG
echo "### encode fast null: base vs base copy"; date
bash $S/abba.sh $W/encfnull 6 $W/arms/base $W/arms/null --time-encode fast 4x4,6x6,8x8,12x12 3 $IMG
date
