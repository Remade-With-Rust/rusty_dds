#!/bin/bash
S=$(dirname "$(readlink -f "$0")")
bash $S/sync.sh
bash $S/build.sh work "$1" || exit 1
bash $S/build.sh base "$1" || exit 1
