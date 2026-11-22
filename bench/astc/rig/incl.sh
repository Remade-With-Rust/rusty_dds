#!/bin/bash
# incl.sh <tag> <inst> <pattern>: inclusive Ir of matching functions (work arm).
callgrind_annotate --inclusive=yes --threshold=100 ${CP:-/root/astcws/cp}/cg/$1/$2.work.out 2>/dev/null | grep -E "$3" | head -8 | cut -c1-170
