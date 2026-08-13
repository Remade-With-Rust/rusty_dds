# Third-party notices

`rusty_dds` ships under [MIT](LICENSE-MIT). It also contains, or depends on,
work by others under permissive terms. This file records those obligations so a
copy of `rusty_dds` carries the notices its inputs require.

Everything listed here is MIT or equivalently permissive. There is no
copyleft, no source-availability trigger, and no per-title or per-seat fee
anywhere in the dependency graph.

---

## Code included in this repository

### `ddsfile` — container lineage

The DDS container layer (`Header`, `Header10`, `Dds` read/write, the D3D and
DXGI format tables) began as a fork of
[PistonDevelopers/ddsfile](https://github.com/PistonDevelopers/ddsfile).

> Copyright (c) 2018 Michael Dilger and `ddsfile` contributors
> Licensed MIT — the full text is in [`LICENSE-MIT`](LICENSE-MIT), which is
> this project's license file and preserves the upstream notice.

### `bcdec_rs` — BC7 two-subset partition table

`src/encode/blocks/m1.rs` contains the 64-entry BC7 two-subset partition table
(`P2`) copied verbatim from
[bcdec_rs](https://github.com/ScanMountGoat/bcdec_rs) by ScanMountGoat, itself a
Rust port of [bcdec](https://github.com/iOrange/bcdec) by Sergii Kudlai
(iOrange). The table is used so the encoder's partition indices agree exactly
with the decoder we gate against.

> `bcdec_rs` — Copyright (c) ScanMountGoat — MIT
> `bcdec` — Copyright (c) Sergii Kudlai — MIT
>
> Permission is hereby granted, free of charge, to any person obtaining a copy
> of this software and associated documentation files (the "Software"), to deal
> in the Software without restriction, including without limitation the rights
> to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
> copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all
> copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
> IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
> FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
> LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
> OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
> SOFTWARE.

The BC6H and BC7 bitstream layouts themselves are defined by Microsoft's
published BCn format specifications; format specifications are not copyrightable
subject matter, and no Microsoft code is included here.

---

## Runtime dependencies

| Crate | License | Role |
|---|---|---|
| `bcdec_rs` | MIT | BCn/BC6H decode core (optional, `decode` feature) |
| `bitflags` | MIT OR Apache-2.0 | header flag types |
| `byteorder` | MIT OR Unlicense | container field I/O |
| `enum-primitive-derive` | MIT | format enum conversions |
| `num-traits` | MIT OR Apache-2.0 | numeric traits |

Development and benchmarking only — never linked into a consumer's build:
`criterion`, `ddsfile` (parse A/B peer), `serde_json`, `tiff`, `png`,
`miniz_oxide` (RDO rate metric), `eframe`/`rfd` (demo viewer).

---

## Benchmark corpora — not redistributed

The measurement corpora are **fetched locally and git-ignored**; no third-party
image is committed to this repository. Provenance is recorded so results can be
reproduced:

| Corpus | Source | Terms |
|---|---|---|
| ambientCG PBR materials | [ambientcg.com](https://ambientcg.com/) | CC0 |
| CryTIF UI textures | [CRYTEK/GameSDK](https://github.com/CRYTEK/GameSDK) | Crytek sample-project assets, fetched for local benchmarking only |
| USC-SIPI images | [sipi.usc.edu](https://sipi.usc.edu/database/) | research image database, local benchmarking only |
| HDRIs | [Polyhaven](https://polyhaven.com/) | CC0 |

The one image committed to the tree is `examples/assets/demo_sample.tif`, used
by the viewer demo.

---

## Trademarks

"Remade With Rust", "Mata", and "Mata Network" are marks of Mata Network. The
MIT license grants rights to the **code**; it does not grant rights to these
marks. Fork, ship, and sell the code freely — see
[`docs/commercial-model.md`](docs/commercial-model.md) — and simply use your own
name for a fork.

DDS, DirectDraw, DirectX, and Direct3D are marks of Microsoft. Star Citizen and
StarEngine are marks of Cloud Imperium Games. CryEngine is a mark of Crytek.
This project is not affiliated with, endorsed by, or sponsored by any of them;
they are named only to describe interoperability and measurement peers.
