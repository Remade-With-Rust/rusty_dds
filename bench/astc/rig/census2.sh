#!/bin/bash
# compact: callee primitives and callers >= 0.005% of total
CP=${CP:-/root/astcws/cp}
for inst in "$@"; do
F=$CP/cg/$TAG/$inst.work.out
echo "######## $inst  total $(grep -m1 '^totals:' $F | awk '{print $2}')"
callgrind_annotate --tree=caller --threshold=100 $F 2>/dev/null | python3 -c '
import sys,re
pat=re.compile(r"memcpy|memmove|memset|__rust_alloc\b|__rust_dealloc|__rust_realloc|__rust_alloc_zeroed")
blk=[]
def flush(b):
    c=[l for l in b if " * " in l]
    if not c or not pat.search(c[0]): return
    m=re.search(r"\(\s*([\d.]+)%\)",c[0])
    if m and float(m.group(1))<0.005: return
    print(re.sub(r"\s+\S*/(?=[^/]*:)"," ",c[0])[:150])
    for l in b:
        if " < " in l:
            m=re.search(r"\(\s*([\d.]+)%\)",l)
            if m and float(m.group(1))>=0.005:
                l=re.sub(r"/root/astcws/[a-z]+/work/src/astc/|/rustc/\w+/library/|\[/root\S*\]","",l.strip())
                print("     ",l[:190])
for l in sys.stdin:
    l=l.rstrip("\n")
    if not l.strip(): flush(blk); blk=[]
    else: blk.append(l)
flush(blk)
'
done
