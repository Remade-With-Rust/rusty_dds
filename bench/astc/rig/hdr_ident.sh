#!/bin/bash
# hdr_ident.sh: HDR encoded-byte identity of the work arm against the base arm.
# HdrRgbLdrA (`--ir-encode-hdr`): hdr-rgb-00 (256x256) and 256x256 crops of
# arboretum, bellparkpier, canarywharf x {fast, medium, thorough} x
# {4x4, 6x6, 8x8, 12x12} (48 encodes). Base hashes cached in $CP/hdr_ident.base
# (delete it to refresh). Exit 1 on any difference.
CP=${CP:-/root/astcws/av}
H=/root/astcws/astc-encoder/Test/Images
J=${J:-16}
jobs() {
  for img in Small/HDR-RGB/hdr-rgb-00.hdr HDRIHaven/HDR-RGB/hdr-rgb-arboretum.hdr@512,256,256,256 \
             HDRIHaven/HDR-RGB/hdr-rgb-bellparkpier.hdr@256,128,256,256 HDRIHaven/HDR-RGB/hdr-rgb-canarywharf.hdr@768,384,256,256; do
    for p in fast medium thorough; do for fp in 4x4 6x6 8x8 12x12; do echo "$p $fp $img"; done; done
  done
}
run() { # arm -> sorted "preset fp img hash" lines
  jobs | xargs -P $J -L 1 bash -c 'h=$('$CP'/tgt-'$1'/release/astc_bench --ir-encode-hdr $0 $1 '$H'/$2 | sed "s/.*hash //"); echo "$0 $1 $2 $h"' | sort
}
[ -s $CP/hdr_ident.base ] || run base > $CP/hdr_ident.base
run work > $CP/hdr_ident.work
n=$(grep -c ' [0-9a-f]\{16\}$' $CP/hdr_ident.work)
if cmp -s $CP/hdr_ident.base $CP/hdr_ident.work && [ $n -eq 48 ]; then
  echo "hdr-ident: $n/48 HDR encodes byte-identical to base"
else
  echo "hdr-ident: DIFFER ($n hashed lines)"; diff $CP/hdr_ident.base $CP/hdr_ident.work | head; exit 1
fi
