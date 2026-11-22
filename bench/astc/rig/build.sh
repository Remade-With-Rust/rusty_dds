#!/bin/bash
# Build the Ir bench in an arm: build.sh base|work [features]. Fails loudly.
set -e
CP=/root/astcws/cp
ARM=$1
FEAT=${2:-zstd}
cd $CP/$ARM/bench/astc
[ -f $CP/bench.lock ] && cp $CP/bench.lock Cargo.lock
df -h /root | tail -1
CARGO_TARGET_DIR=$CP/tgt-$ARM cargo build --release --features "$FEAT" 2>&1 | grep -E "^(error|warning: unused)|Finished|Compiling rusty_(dds|alloc)" || true
test ${PIPESTATUS[0]} -eq 0 || { echo BUILD FAILED; exit 1; }
[ -f $CP/bench.lock ] || cp Cargo.lock $CP/bench.lock
ls -la --time-style=full-iso $CP/tgt-$ARM/release/astc_bench | awk '{print $6, $7, $9}'
