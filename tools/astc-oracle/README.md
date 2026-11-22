# astc-oracle (not published, never built by cargo)

A batch front end over ARM's astcenc, used as a **black-box reference** for
`rusty_dds::astc`. It reads fixed-size records on stdin and writes fixed-size
binary records on stdout; see the header of `oracle.cpp` for the commands
(`decode`, `info`, `ise`, `modes`, `partitions`).

## Licence posture

`oracle.cpp` is ours (MIT, like the rest of this repository). At build time it
links astcenc's static library and includes its headers; astcenc is
Apache-2.0 and is never vendored, copied or translated into the crate. The
oracle lives outside the crate's dependency graph: `tools/` is excluded from
the published package and nothing in `Cargo.toml` refers to it. The crate's
ASTC code is written from the Khronos Data Format Specification; astcenc's
behaviour is matched by comparing outputs, never by porting its source.

## Pinned reference

| | |
|---|---|
| astcenc release | 5.7.1 (2026-11-03) |
| commit | `0affb51aaf0e0de3a8b038fa32487d59754a1eed` |
| ISA | `none` static library (scalar); the AVX2 build is scored against it in brick 0.4 |

Re-pin when astcenc 6.0.0 is tagged, then re-run DC1 and DC2 (plan
`docs/plans/astc.md`, risk R11).

## Build (WSL / Linux, gcc 15, cmake 4)

```sh
git clone https://github.com/ARM-software/astc-encoder /root/astcws/astc-encoder
git -C /root/astcws/astc-encoder worktree add /root/astcws/rel571 0affb51aaf0e0de3a8b038fa32487d59754a1eed
cd /root/astcws/rel571
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DASTCENC_ISA_AVX2=ON -DASTCENC_ISA_NONE=ON \
      -DCMAKE_INSTALL_PREFIX=/root/astcws/rel571/inst
cmake --build build -j4 && cmake --install build

# then, from a checkout of this repository on ext4 (not /mnt/c or /mnt/f):
ASTCENC=/root/astcws/rel571 sh tools/astc-oracle/build.sh    # -> tools/astc-oracle/oracle
```

`/root/astcws/rel571/inst/bin/astcenc-none -version` must print `astcenc v5.7.1`.

The differential harness that drives it is `tools/astc-harness`
(`cargo build --release` there; it finds the oracle through `ASTC_ORACLE` or
the default path `/root/astcws/rdds/tools/astc-oracle/oracle`).
