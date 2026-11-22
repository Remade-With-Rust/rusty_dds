#!/bin/bash
# ir.sh <tag> [inst...]: every instrument x both arms under callgrind, in parallel,
# identical `env -i` environment. Prints Ir per (instrument, arm) and the anchors.
CP=${CP:-/root/astcws/cp}
A=/root/astcws
TAG=$1; shift
INSTS=${@:-dec-u8 dec-f16 dec-rows enc-fastest enc-fast enc-medium enc-thorough cont}
OUT=$CP/cg/$TAG; mkdir -p $OUT
D="$A/scout/d_4x4.astc@512 $A/scout/d_6x6.astc@512 $A/scout/d_8x8.astc@512 $A/scout/d_12x12.astc@512 $A/corpus/s_ldr-rgb-06_5x5_cs_thorough.astc $A/corpus/h_hdr-rgb-arboretum_6x6_ch_fast.astc"
E="$A/kodak/kodim23.png@256,128,256,256 $A/astc-encoder/Test/Images/Small/LDR-RGBA/ldr-rgba-00.png"
K=$A/thirdparty/KTX-Software/tests/resources/ktx2
C="$A/scout/d_8x8.astc $CP/fix/app8x8_zstd.ktx2 $K/Desk_astc_hdr6x6.ktx2 $K/ktx_app_astc_8x8.ktx2 $CP/fix/m6x6.ktx $CP/fix/ASTC_6x5.dds"
args() {
  case $1 in
    dec-u8) echo "--ir-decode u8 $D";;
    dec-f16) echo "--ir-decode f16 $D";;
    enc-thorough) echo "--ir-encode thorough 4x4,8x8 $E";;
    enc-*) echo "--ir-encode ${1#enc-} 4x4,6x6,8x8,12x12 $E";;
    cont) echo "--ir-container $C";;
    dec-rows) echo "--ir-rows $A/scout/d_4x4.astc@256 $A/scout/d_8x8.astc@512 $A/scout/d_12x12.astc@512";;
  esac
}
for arm in base work; do
  for i in $INSTS; do
    ( env -i PATH=/usr/bin:/bin HOME=/root valgrind --tool=callgrind --callgrind-out-file=$OUT/$i.$arm.out \
        $CP/tgt-$arm/release/astc_bench $(args $i) > $OUT/$i.$arm.txt 2> $OUT/$i.$arm.err ) &
  done
done
wait
printf "%-13s %16s %16s %10s  %s\n" inst base work delta anchors
for i in $INSTS; do
  b=$(grep -m1 '^totals:' $OUT/$i.base.out | awk '{print $2}')
  w=$(grep -m1 '^totals:' $OUT/$i.work.out | awk '{print $2}')
  ab=$(cat $OUT/$i.base.txt); aw=$(cat $OUT/$i.work.txt)
  same=$([ "${ab#*:}" == "${aw#*:}" ] && echo "anchors-equal" || echo "ANCHORS DIFFER: base[$ab] work[$aw]")
  printf "%-13s %16s %16s %+9.3f%%  %s | %s\n" $i $b $w $(echo "scale=6; ($w-$b)*100/$b" | bc) "$same" "${aw#*:}"
done
