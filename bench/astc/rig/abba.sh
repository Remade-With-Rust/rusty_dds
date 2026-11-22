#!/bin/bash
# abba.sh <out> <rounds> <binA> <binB> <bench args...>
# Whole processes alternate ABBA (round r even: A then B; odd: B then A),
# pinned to CPU 2 at nice -5; each process times itself on the thread CPU clock.
OUT=$1; R=$2; A=$3; B=$4; shift 4
mkdir -p $OUT; rm -f $OUT/*.txt
for r in $(seq 0 $((R-1))); do
  if [ $((r % 2)) -eq 0 ]; then ord="A B"; else ord="B A"; fi
  for arm in $ord; do
    bin=$A; [ $arm == B ] && bin=$B
    taskset -c 2 nice -n -5 $bin "$@" > $OUT/r${r}_$arm.txt
  done
done
python3 $(dirname "$(readlink -f "$0")")/abba.py $OUT $R
