#!/bin/bash
# fndiff2.sh <tagA> <tagB> <inst>: self Ir per function+file line group, work arms of two tags.
CP=/root/astcws/cp
i=0
for t in $1 $2; do
  callgrind_annotate --threshold=100 $CP/cg/$t/$3.work.out 2>/dev/null | awk '/^ *[0-9][0-9,]* / && /:/ {v=$1; gsub(",","",v); $1=""; sub(/^ +\([^)]*\) +/,""); sub(/\[.*\]/,""); gsub("/root/astcws/cp/(base|work)/",""); k[$0]+=v} END {for (x in k) print k[x] "\t" x}' > /tmp/fn.$i; i=$((i+1))
done
python3 - <<'PY'
b={};w={}
for f,d in (('/tmp/fn.0',b),('/tmp/fn.1',w)):
    for l in open(f):
        v,k=l.rstrip('\n').split('\t',1); d[k]=d.get(k,0)+int(v)
r=sorted(set(b)|set(w),key=lambda k:-abs(w.get(k,0)-b.get(k,0)))
for k in r[:16]:
    print(f"{w.get(k,0)-b.get(k,0):+14,}  {b.get(k,0):>14,} -> {w.get(k,0):>14,}  {k[:120]}")
PY
