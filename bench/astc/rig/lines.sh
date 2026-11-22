#!/bin/bash
# lines.sh <tag> <inst> <min_ir> <file-substr>...: per-line Ir in the work arm.
CP=/root/astcws/cp
F=$CP/cg/$1/$2.work.out; MIN=$3; shift 3
callgrind_annotate --auto=yes --threshold=100 $F 2>/dev/null > /tmp/ann.$$
for s in "$@"; do
  awk -v m=$MIN -v f="$s" '
    /^-- Auto-annotated source:/ {show=(index($0,f)>0); next}

    show && /^ *[0-9][0-9,]* / { v=$1; gsub(",","",v); if (v+0>=m) print f": "$0 }' /tmp/ann.$$ | cut -c1-175
done
rm -f /tmp/ann.$$
