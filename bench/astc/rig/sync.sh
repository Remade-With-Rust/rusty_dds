#!/bin/bash
# Sync the worktree into the work tree and the bench into both trees.
set -e
CP=${CP:-/root/astcws/cp}
WT=${WT:-$(readlink -f "$(dirname "$(readlink -f "$0")")/../../..")}
rsync -a --delete --exclude target --exclude .git --exclude .agent-scratch --exclude bench/astc/rig --exclude 'bench/astc/target' --exclude 'bench/astc/Cargo.lock' --exclude sim --exclude corpus --exclude third_party --exclude fuzz $WT/ $CP/work/
rsync -a --delete --exclude target --exclude Cargo.lock $WT/bench/astc/ $CP/base/bench/astc/
