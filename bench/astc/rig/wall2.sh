#!/bin/bash
# Decode A/B re-run at 20 rounds (the 10-round set was disturbed by load), plus its null arm.
S=$(dirname "$(readlink -f "$0")")
W=${CP:-/root/astcws/cp}/wall
D=/root/astcws/out/dbench
DEC=""
for m in m1 m2 m3 m4; do for fp in 4x4 6x6 8x8 12x12; do DEC="$DEC $D/${m}_$fp.astc"; done; done
echo "### decode (20 rounds): base (A) vs branch (B)"; date; uptime
bash $S/abba.sh $W/dec20 20 $W/arms/base $W/arms/work --time-decode 7 $DEC
echo "### decode null (20 rounds): base (A) vs base copy (B)"; date; uptime
bash $S/abba.sh $W/decnull20 20 $W/arms/base $W/arms/null --time-decode 7 $DEC
date
