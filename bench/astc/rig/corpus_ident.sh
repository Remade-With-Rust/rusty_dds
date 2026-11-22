#!/bin/bash
# Byte identity vs 0.11.0 over the whole astcenc corpus: all six decode cells
# (--ir-decode all) and the row seam, per file, both arms. Skips 3D files
# (the bench's loader is 2D only); prints counts and any mismatch.
CP=/root/astcws/cp
n=0; same=0; diff=0; skip=0
for f in /root/astcws/corpus/*.astc /root/astcws/thirdparty/astc-codec/src/decoder/testdata/*.astc; do
  [ -f "$f" ] || continue
  a=$($CP/tgt-base/release/astc_bench --ir-decode all "$f" 2>/dev/null) || { skip=$((skip+1)); continue; }
  b=$($CP/tgt-work/release/astc_bench --ir-decode all "$f" 2>/dev/null)
  ra=$($CP/tgt-base/release/astc_bench --ir-rows "$f" 2>/dev/null); rb=$($CP/tgt-work/release/astc_bench --ir-rows "$f" 2>/dev/null)
  n=$((n+1))
  if [ "${a#*:}" == "${b#*:}" ] && [ "${ra#*:}" == "${rb#*:}" ]; then same=$((same+1)); else diff=$((diff+1)); echo "MISMATCH $f"; fi
done
echo "corpus identity: files $n identical $same differing $diff skipped(3D/unreadable) $skip"
