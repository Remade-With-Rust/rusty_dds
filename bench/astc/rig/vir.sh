#!/bin/bash
# vir.sh <tag> [arm] : the encoder Ir instruments for one arm under callgrind,
# all in parallel, identical `env -i` environment every time. Writes
# $CP/cg/<tag>/<inst>.{out,txt}. With REF=<tag> also prints the delta against
# that tag's numbers and checks the anchors (blocks, bytes, FNV of the
# encoded bytes) are unchanged.
#   enc-fastest/fast/medium : kodim23 256x256 crop + ldr-rgba-00, 4x4 6x6 8x8 12x12
#   enc-medk                : Medium, kodim05 + kodim19 192x192 crops, LdrSrgb
#                             (the decode_unorm8 rule), 4x4 6x6 8x8 12x12
#   enc-thorough            : kodim23 128x128 crop + ldr-rgba-00, 4x4 8x8
#   enc-hdr                 : Medium HDR (HdrRgbLdrA), hdr-rgb-00 + arboretum
#                             128x128 crop, 4x4 6x6 8x8
CP=${CP:-/root/astcws/av}
A=/root/astcws
TAG=$1
ARM=${2:-work}
INSTS=${INSTS:-enc-fastest enc-fast enc-medium enc-medk enc-thorough enc-hdr}
OUT=$CP/cg/$TAG; mkdir -p $OUT
E="$A/kodak/kodim23.png@256,128,256,256 $A/astc-encoder/Test/Images/Small/LDR-RGBA/ldr-rgba-00.png"
K="$A/kodak/kodim05.png@256,128,192,192 $A/kodak/kodim19.png@128,256,192,192"
T="$A/kodak/kodim23.png@256,128,128,128 $A/astc-encoder/Test/Images/Small/LDR-RGBA/ldr-rgba-00.png"
H="$A/astc-encoder/Test/Images/Small/HDR-RGB/hdr-rgb-00.hdr $A/astc-encoder/Test/Images/HDRIHaven/HDR-RGB/hdr-rgb-arboretum.hdr@512,256,128,128"
args() {
  case $1 in
    enc-medk) echo "--ir-encode medium 4x4,6x6,8x8,12x12 $K";;
    enc-thorough) echo "--ir-encode thorough 4x4,8x8 $T";;
    enc-hdr) echo "--ir-encode-hdr medium 4x4,6x6,8x8 $H";;
    enc-*) echo "--ir-encode ${1#enc-} 4x4,6x6,8x8,12x12 $E";;
  esac
}
envs() { [ "$1" == enc-medk ] && echo "ASTC_SRGB=1"; }
BIN=$CP/tgt-$ARM/release/astc_bench
ls -la --time-style=full-iso $BIN | awk '{print "binary", $6, $7}'
for i in $INSTS; do
  ( env -i PATH=/usr/bin:/bin HOME=/root $(envs $i) valgrind --tool=callgrind --callgrind-out-file=$OUT/$i.out \
      $BIN $(args $i) > $OUT/$i.txt 2> $OUT/$i.err ) &
done
wait
printf "%-13s %16s %16s %10s  %s\n" inst "${REF:-}" $TAG delta anchors
for i in $INSTS; do
  w=$(grep -m1 '^totals:' $OUT/$i.out | awk '{print $2}')
  aw=$(cat $OUT/$i.txt)
  if [ -n "$REF" ] && [ -f $CP/cg/$REF/$i.out ]; then
    b=$(grep -m1 '^totals:' $CP/cg/$REF/$i.out | awk '{print $2}')
    ab=$(cat $CP/cg/$REF/$i.txt)
    same=$([ "${ab#*:}" == "${aw#*:}" ] && echo "anchors-equal" || echo "ANCHORS DIFFER: ref[$ab]")
    printf "%-13s %16s %16s %+9.3f%%  %s |%s\n" $i $b $w $(echo "scale=6; ($w-$b)*100/$b" | bc) "$same" "${aw#*:}"
  else
    printf "%-13s %16s %16s %10s  |%s\n" $i - $w - "${aw#*:}"
  fi
done
