#!/bin/bash
# Wall check for the vein round: branch vs 0.11.2, same bench source, rusty_alloc in both
# arms, whole processes ABBA-alternated (abba.sh), pinned to CPU 2 at nice -5, thread CPU
# clock, with a null arm (0.11.2 vs a byte copy of itself). Arms frozen in $CP/wall/arms.
S=$(dirname "$(readlink -f "$0")")
CP=${CP:-/root/astcws/av}; W=$CP/wall
R=${R:-10}
md5sum $W/arms/* | awk '{print $1, $2}'
K=/root/astcws/kodak
IMG="$K/kodim01.png $K/kodim03.png $K/kodim08.png $K/kodim15.png"
HDR=/root/astcws/astc-encoder/Test/Images/Small/HDR-RGB/hdr-rgb-00.hdr
echo "### encode medium: 0.11.2 (A) vs branch (B)"; date; uptime
bash $S/abba.sh $W/encm $R $W/arms/base $W/arms/work --time-encode medium 4x4,6x6,8x8,12x12 3 $IMG
echo "### encode medium null: 0.11.2 (A) vs byte copy (B)"; date; uptime
bash $S/abba.sh $W/encmnull $R $W/arms/base $W/arms/null --time-encode medium 4x4,6x6,8x8,12x12 3 $IMG
echo "### encode fast: 0.11.2 (A) vs branch (B)"; date; uptime
bash $S/abba.sh $W/encf $R $W/arms/base $W/arms/work --time-encode fast 4x4,6x6,8x8,12x12 3 $IMG
echo "### encode thorough 8x8: 0.11.2 (A) vs branch (B)"; date; uptime
bash $S/abba.sh $W/enct $R $W/arms/base $W/arms/work --time-encode thorough 8x8 1 $K/kodim01.png $K/kodim15.png
echo "### HDR medium: 0.11.2 (A) vs branch (B)"; date; uptime
bash $S/abba.sh $W/hdrm $R $W/arms/base $W/arms/work --time-encode-hdr medium 4x4,6x6,8x8,12x12 3 $HDR
date
