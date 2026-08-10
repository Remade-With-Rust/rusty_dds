# rusty_dds vs Microsoft DirectXTex

Peer: [Microsoft DirectXTex](https://github.com/microsoft/DirectXTex)

Protocol: `Dds::read + decode_rgba8 -> RGBA8` vs `LoadFromDDSMemory + Decompress|Convert -> R8G8B8A8_UNORM`

Summary: 15 cases — **13 ahead**, 1 parity, **1 behind**, 0 dxtex-failed

| Case | Content | Context | rusty_dds (ns) | DirectXTex (ns) | Ratio | Verdict |
|------|---------|---------|----------------|-----------------|-------|---------|
| bc1__X-2D | bc1 | X-2D | 3388 | 17402 | 0.19 | ahead |
| bc2__X-2D | bc2 | X-2D | 4960 | 19204 | 0.26 | ahead |
| bc3__X-2D | bc3 | X-2D | 4602 | 20574 | 0.22 | ahead |
| bc4u__X-2D | bc4u | X-2D | 5498 | 32678 | 0.17 | ahead |
| bc4s__X-2D | bc4s | X-2D | 5786 | 36862 | 0.16 | ahead |
| bc5u__X-2D | bc5u | X-2D | 11904 | 37740 | 0.32 | ahead |
| bc5s__X-2D | bc5s | X-2D | 7514 | 43856 | 0.17 | ahead |
| bc7__X-2D | bc7 | X-2D | 24338 | 63590 | 0.38 | ahead |
| rgba8__X-2D | rgba8 | X-2D | 724 | 472 | 1.53 | behind |
| bgra8__X-2D | bgra8 | X-2D | 3258 | 10822 | 0.30 | ahead |
| bc1__X-MIP-tip | bc1 | X-MIP | 304 | 322 | 0.94 | parity |
| bc3__X-ARRAY | bc3 | X-ARRAY | 1522 | 5664 | 0.27 | ahead |
| bc1__X-CUBE-face | bc1 | X-CUBE | 1210 | 4784 | 0.25 | ahead |
| bc7__X-NPOT | bc7 | X-NPOT | 396 | 538 | 0.74 | ahead |
| bc1__X-VOL | bc1 | X-VOL | 1692 | 5598 | 0.30 | ahead |
