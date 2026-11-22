#!/bin/bash
# enc_ident.sh: encoded-byte identity of the work arm against the base arm.
# Ldr: Kodak 01 03 05 08 13 15 19 23 x {fastest, fast, medium, thorough} x
# {4x4, 6x6, 8x8, 12x12} (128 encodes). LdrSrgb (the decode_unorm8 error rule):
# Kodak 01 08 15 23 x {fast, medium} x the same footprints (32 encodes).
# One --ir-encode job per encode, run in parallel. The base arm's hashes are
# cached in $CP/enc_ident.base (delete it to refresh). Exit 1 on any difference.
CP=${CP:-/root/astcws/se}
K=/root/astcws/kodak
J=${J:-20}
jobs() {
  for img in 01 03 05 08 13 15 19 23; do for p in fastest fast medium thorough; do for fp in 4x4 6x6 8x8 12x12; do
    echo "0 $p $fp $img"; done; done; done
  for img in 01 08 15 23; do for p in fast medium; do for fp in 4x4 6x6 8x8 12x12; do
    echo "1 $p $fp $img"; done; done; done
}
run() { # arm -> sorted "srgb preset fp img hash" lines
  jobs | xargs -P $J -L 1 bash -c 'h=$(ASTC_SRGB=$0 '$CP'/tgt-'$1'/release/astc_bench --ir-encode $1 $2 '$K'/kodim$3.png | sed "s/.*hash //"); echo "$0 $1 $2 $3 $h"' | sort
}
[ -s $CP/enc_ident.base ] || run base > $CP/enc_ident.base
run work > $CP/enc_ident.work
n=$(grep -c ' [0-9a-f]\{16\}$' $CP/enc_ident.work)
if cmp -s $CP/enc_ident.base $CP/enc_ident.work && [ $n -eq 160 ]; then
  echo "enc-ident: $n/160 encodes byte-identical to base (128 Ldr, 32 LdrSrgb)"
else
  echo "enc-ident: DIFFER ($n hashed lines)"; diff $CP/enc_ident.base $CP/enc_ident.work | head; exit 1
fi
