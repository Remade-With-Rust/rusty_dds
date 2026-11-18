//! rusty_dds vs `dds` 0.2 — decode a DDS main surface (or a whole mip chain) to
//! RGBA, the way MaterialCraft does. Requested on Remade-With-Rust/rusty_dds#2.
//!
//! Method (printed with every run):
//! * ONE binary links both crates, so both arms share the compiler, the flags
//!   and the global allocator by construction (no per-arm allocator guard to
//!   forget). The allocator is a cargo feature and is printed.
//! * Correctness gate BEFORE timing: both crates decode the same bytes to f32
//!   and the max abs difference / differing-channel count is reported.
//! * Every arm decodes into a buffer allocated once and reused, so no arm
//!   measures the allocator (the f32 output is 16 B/px).
//! * Arms are interleaved in-process; the order reverses every round (ABBA),
//!   so neither arm is always "second and warmer".
//! * Each sample runs the same K decodes in every arm (K calibrated so a sample
//!   is >= ~60 ms), timed by `Instant` (QPC on Windows, sub-microsecond). The
//!   smallest gap between two distinct samples is printed as the clock quantum.
//! * NULL arm: the rusty_dds f32 arm run twice under two names; its spread is
//!   the resolution of this harness on this machine, this session.
//! * Work parity: each arm reports the pixels it produced per sample.

#[cfg(feature = "rusty-alloc")]
#[global_allocator]
static ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;

const ALLOCATOR: &str = if cfg!(feature = "rusty-alloc") {
    "rusty_alloc"
} else {
    "system (Windows HeapAlloc)"
};

use rusty_dds::{
    D3D10ResourceDimension, AlphaMode, DdsView, DecodeContent, DecodeOptions, Dds, DxgiFormat,
    EncodeLayout, NewDxgiParams, SingleChannel, SubresourceId,
};
use std::hint::black_box;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ---------------------------------------------------------------- inputs ----

fn load_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let dec = png::Decoder::new(std::fs::File::open(path).unwrap_or_else(|e| panic!("{path:?}: {e}")));
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let (w, h) = (info.width, info.height);
    let px = (w * h) as usize;
    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf[..px * 4].to_vec(),
        (png::ColorType::Rgb, png::BitDepth::Eight) => {
            buf[..px * 3].chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect()
        }
        (png::ColorType::Grayscale, png::BitDepth::Eight) => {
            buf[..px].iter().flat_map(|&g| [g, g, g, 255]).collect()
        }
        (png::ColorType::Grayscale, png::BitDepth::Sixteen) => {
            buf[..px * 2].chunks_exact(2).flat_map(|c| [c[0], c[0], c[0], 255]).collect()
        }
        (png::ColorType::Rgb, png::BitDepth::Sixteen) => {
            buf[..px * 6].chunks_exact(6).flat_map(|c| [c[0], c[2], c[4], 255]).collect()
        }
        (png::ColorType::Rgba, png::BitDepth::Sixteen) => {
            buf[..px * 8].chunks_exact(8).flat_map(|c| [c[0], c[2], c[4], c[6]]).collect()
        }
        other => panic!("{path:?}: unsupported png layout {other:?}"),
    };
    (w, h, rgba)
}

/// Mirror-tile a square photo up to `side` so 4096^2 is real photo content
/// (no hard seams that would make blocks unusually flat or busy).
fn tile_to(w: u32, h: u32, src: &[u8], side: u32) -> Vec<u8> {
    let mut out = vec![0u8; (side * side * 4) as usize];
    for y in 0..side {
        let ty = y / h;
        let sy = if ty % 2 == 0 { y % h } else { h - 1 - y % h };
        for x in 0..side {
            let tx = x / w;
            let sx = if tx % 2 == 0 { x % w } else { w - 1 - x % w };
            let s = ((sy * w + sx) * 4) as usize;
            let d = ((y * side + x) * 4) as usize;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    out
}

fn downsample(side: u32, src: &[u8]) -> Vec<u8> {
    let n = (side / 2).max(1);
    let mut out = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            for c in 0..4 {
                let at = |xx: u32, yy: u32| src[(((yy.min(side - 1)) * side + xx.min(side - 1)) * 4 + c) as usize] as u32;
                let v = at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1);
                out[((y * n + x) * 4 + c) as usize] = ((v + 2) / 4) as u8;
            }
        }
    }
    out
}

fn f32_to_half(f: f32) -> u16 {
    let b = f.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let m = b & 0x7f_ffff;
    if e >= 31 {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (m | 0x80_0000) >> (1 - e);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    let h = ((e as u32) << 10) | (m >> 13);
    let round = (m >> 12) & 1;
    sign | (h + round) as u16
}

/// One uncompressed level of `fmt` from RGBA8 photo pixels.
fn pack_level(fmt: DxgiFormat, rgba: &[u8]) -> Vec<u8> {
    use DxgiFormat::*;
    let mut out = Vec::new();
    for p in rgba.chunks_exact(4) {
        let [r, g, b, a] = [p[0], p[1], p[2], p[3]];
        let lum = (r as f32 * 0.2126 + g as f32 * 0.7152 + b as f32 * 0.0722) / 255.0;
        // HDR: an exposure curve that puts highlights above 1.0.
        let hdr = |c: u8| (c as f32 / 255.0).powf(2.2) * 6.0;
        match fmt {
            R16G16B16A16_Float => {
                for v in [hdr(r), hdr(g), hdr(b), a as f32 / 255.0] {
                    out.extend_from_slice(&f32_to_half(v).to_le_bytes());
                }
            }
            R32G32B32A32_Float => {
                for v in [hdr(r), hdr(g), hdr(b), a as f32 / 255.0] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
            R32_Float => out.extend_from_slice(&(lum * 3.0).to_le_bytes()),
            R16_UNorm => {
                // A height map: luma widened with low-order detail so all 16
                // bits carry information.
                let v = (lum * 65535.0) as u32 ^ ((r as u32 * 31 + b as u32) & 0xff);
                out.extend_from_slice(&(v.min(65535) as u16).to_le_bytes());
            }
            R16G16_UNorm => {
                out.extend_from_slice(&((r as u16) * 257).to_le_bytes());
                out.extend_from_slice(&((g as u16) * 257).to_le_bytes());
            }
            R10G10B10A2_UNorm => {
                let q = |c: u8| (c as u32 * 1023 + 127) / 255;
                let v = q(r) | q(g) << 10 | q(b) << 20 | ((a as u32 * 3 + 127) / 255) << 30;
                out.extend_from_slice(&v.to_le_bytes());
            }
            B5G6R5_UNorm => {
                let v = ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3);
                out.extend_from_slice(&v.to_le_bytes());
            }
            R8_UNorm => out.push((lum * 255.0).round() as u8),
            _ => unreachable!("{fmt:?}"),
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Src {
    Color,
    Rough,
    Normal,
}

struct Case {
    name: String,
    bytes: Vec<u8>,
    mips: u32,
    /// rusty_dds has an RGBA8 path for this format.
    has_u8: bool,
    /// Compare/expand one-channel formats as grey, the `dds` convention.
    grey: bool,
    bc7: bool,
}

fn cache_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cache");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Builds ONE case at a time and hands it to `run`, which drops it before the
/// next is built. Holding every 4096^2 input at once peaked at ~5 GB of commit.
fn for_each_case(sizes: &[(u32, u32)], filter: &Option<String>, mut run: impl FnMut(Case)) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/raw/Rock064");
    let src = |s: Src| {
        let f = match s {
            Src::Color => "Rock064_1K-PNG_Color.png",
            Src::Rough => "Rock064_1K-PNG_Roughness.png",
            Src::Normal => "Rock064_1K-PNG_NormalGL.png",
        };
        load_png(&root.join(f))
    };
    let photos = [Src::Color, Src::Rough, Src::Normal].map(|s| (s, src(s)));
    let photo = |s: Src, side: u32| {
        let (_, (w, h, px)) = photos.iter().find(|(k, _)| *k == s).unwrap();
        tile_to(*w, *h, px, side)
    };

    let block: &[(DecodeContent, &str, Src)] = &[
        (DecodeContent::Bc1, "BC1", Src::Color),
        (DecodeContent::Bc3, "BC3", Src::Color),
        (DecodeContent::Bc4UNorm, "BC4", Src::Rough),
        (DecodeContent::Bc5UNorm, "BC5", Src::Normal),
        (DecodeContent::Bc7, "BC7", Src::Color),
        (DecodeContent::Rgba8, "RGBA8", Src::Color),
    ];
    let plain: &[(DxgiFormat, &str, Src, bool)] = &[
        (DxgiFormat::R16G16B16A16_Float, "R16G16B16A16_FLOAT", Src::Color, false),
        (DxgiFormat::R32G32B32A32_Float, "R32G32B32A32_FLOAT", Src::Color, false),
        (DxgiFormat::R32_Float, "R32_FLOAT", Src::Rough, true),
        (DxgiFormat::R16_UNorm, "R16_UNORM", Src::Rough, true),
        (DxgiFormat::R16G16_UNorm, "R16G16_UNORM", Src::Normal, false),
        (DxgiFormat::R10G10B10A2_UNorm, "R10G10B10A2_UNORM", Src::Color, false),
        (DxgiFormat::B5G6R5_UNorm, "B5G6R5_UNORM", Src::Color, false),
        (DxgiFormat::R8_UNorm, "R8_UNORM", Src::Rough, true),
    ];

    for &(side, mips) in sizes {
        let label = if mips > 1 {
            format!("{side}^2 +mips")
        } else {
            format!("{side}^2")
        };
        for &(content, name, s) in block {
            let full = format!("{name} {label}");
            if filter.as_ref().is_some_and(|f| !full.contains(f.as_str())) {
                continue;
            }
            let path = cache_dir().join(format!("{name}_{side}_{mips}.dds"));
            let bytes = if let Ok(b) = std::fs::read(&path) {
                b
            } else {
                eprintln!("  encoding {full} with rusty_dds (cached afterwards)...");
                let mut layout = EncodeLayout::flat_2d(content, side, side);
                layout.mipmap_levels = mips;
                let dds = Dds::encode_from_rgba8(&photo(s, side), layout).expect("encode");
                let mut b = Vec::new();
                dds.write(&mut b).unwrap();
                std::fs::write(&path, &b).unwrap();
                b
            };
            run(Case {
                name: full,
                bytes,
                mips,
                has_u8: true,
                grey: content == DecodeContent::Bc4UNorm,
                bc7: content == DecodeContent::Bc7,
            });
        }
        for &(fmt, name, s, grey) in plain {
            let full = format!("{name} {label}");
            if filter.as_ref().is_some_and(|f| !full.contains(f.as_str())) {
                continue;
            }
            let mut dds = Dds::new_dxgi(NewDxgiParams {
                height: side,
                width: side,
                depth: None,
                format: fmt,
                mipmap_levels: if mips > 1 { Some(mips) } else { None },
                array_layers: None,
                caps2: None,
                is_cubemap: false,
                resource_dimension: D3D10ResourceDimension::Texture2D,
                alpha_mode: AlphaMode::Straight,
            })
            .unwrap();
            let mut level = photo(s, side);
            let mut lside = side;
            let mut data = Vec::new();
            for _ in 0..mips {
                data.extend(pack_level(fmt, &level));
                level = downsample(lside, &level);
                lside = (lside / 2).max(1);
            }
            assert_eq!(data.len(), dds.data.len(), "{full}: payload size");
            dds.data = data;
            let mut b = Vec::new();
            dds.write(&mut b).unwrap();
            run(Case {
                name: full,
                bytes: b,
                mips,
                has_u8: false,
                grey,
                bc7: false,
            });
        }
    }
}

// ------------------------------------------------------------------ arms ----

/// Per-level reusable output buffers.
struct Bufs {
    dims: Vec<(u32, u32)>,
    f: Vec<Vec<f32>>,
    b: Vec<Vec<u8>>,
}

impl Bufs {
    fn new(bytes: &[u8], mips: u32) -> Bufs {
        let v = DdsView::parse(bytes).unwrap();
        let (w, h) = (v.get_width(), v.get_height());
        let dims: Vec<_> = (0..mips).map(|m| ((w >> m).max(1), (h >> m).max(1))).collect();
        let f = dims.iter().map(|&(w, h)| vec![0f32; (w * h * 4) as usize]).collect();
        let b = dims.iter().map(|&(w, h)| vec![0u8; (w * h * 4) as usize]).collect();
        Bufs { dims, f, b }
    }
    fn pixels(&self) -> u64 {
        self.dims.iter().map(|&(w, h)| w as u64 * h as u64).sum()
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Arm {
    Dds02F32,
    RdF32,
    RdF32Null,
    RdF32Serial,
    RdU8,
}

impl Arm {
    fn label(self) -> &'static str {
        match self {
            Arm::Dds02F32 => "dds 0.2  f32",
            Arm::RdF32 => "rusty_dds f32",
            Arm::RdF32Null => "rusty_dds f32 (null)",
            Arm::RdF32Serial => "rusty_dds f32 1-thread",
            Arm::RdU8 => "rusty_dds u8",
        }
    }
}

fn run_arm(arm: Arm, case: &Case, bufs: &mut Bufs) {
    let opts = if case.grey {
        DecodeOptions::new().single_channel(SingleChannel::Grey)
    } else {
        DecodeOptions::new()
    };
    match arm {
        Arm::Dds02F32 => {
            let mut dec = dds::Decoder::new(Cursor::new(&case.bytes[..])).unwrap();
            for (m, &(w, h)) in bufs.dims.iter().enumerate() {
                let view = dds::ImageViewMut::new(
                    bytemuck::cast_slice_mut(&mut bufs.f[m]),
                    dds::Size::new(w, h),
                    dds::ColorFormat::RGBA_F32,
                )
                .unwrap();
                dec.read_surface(view).unwrap();
            }
        }
        Arm::RdF32 | Arm::RdF32Null => {
            let v = DdsView::parse(&case.bytes[..]).unwrap();
            for m in 0..bufs.dims.len() {
                v.decode_rgba_f32_into_with(SubresourceId::mip_layer(m as u32, 0), &mut bufs.f[m], opts)
                    .unwrap();
            }
        }
        Arm::RdF32Serial => {
            // The caller-split seam over the whole surface: no internal threads.
            let v = DdsView::parse(&case.bytes[..]).unwrap();
            for m in 0..bufs.dims.len() {
                let id = SubresourceId::mip_layer(m as u32, 0);
                let rows = v.block_rows_f32(id).unwrap();
                v.decode_block_rows_f32_into(id, 0..rows, &mut bufs.f[m]).unwrap();
            }
        }
        Arm::RdU8 => {
            let v = DdsView::parse(&case.bytes[..]).unwrap();
            for m in 0..bufs.dims.len() {
                v.decode_rgba8_into_with(SubresourceId::mip_layer(m as u32, 0), &mut bufs.b[m], opts)
                    .unwrap();
            }
        }
    }
    black_box(&bufs.f);
    black_box(&bufs.b);
}

// --------------------------------------------------------------- gate -------

struct Gate {
    max_abs: f32,
    differing: u64,
    total: u64,
    note: &'static str,
}

/// Two buffer sets in all: `ours` is the timing buffer set (reused after),
/// `other` holds dds 0.2's output, then the serial and u8 cross-checks.
fn gate(case: &Case, ours: &mut Bufs) -> Gate {
    let mut other = Bufs::new(&case.bytes, case.mips);
    run_arm(Arm::Dds02F32, case, &mut other);
    run_arm(Arm::RdF32, case, ours);
    let mut g = Gate { max_abs: 0.0, differing: 0, total: 0, note: "" };
    for (x, y) in other.f.iter().flatten().zip(ours.f.iter().flatten()) {
        g.total += 1;
        let d = (x - y).abs();
        if !(d <= 1e-6 || (x.is_nan() && y.is_nan()) || x == y) {
            g.differing += 1;
            if d > g.max_abs || d.is_nan() {
                g.max_abs = d;
            }
        }
    }
    // The serial seam must equal the threaded path bit for bit.
    if case.bc7 {
        run_arm(Arm::RdF32Serial, case, &mut other);
        assert!(
            other.f.iter().flatten().zip(ours.f.iter().flatten()).all(|(p, q)| p.to_bits() == q.to_bits()),
            "{}: serial and threaded f32 differ",
            case.name
        );
    }
    // The u8 arm must be the f32 arm rounded from the 8-bit-defined formats.
    if case.has_u8 && !case.name.starts_with("BC4") && !case.name.starts_with("BC5") {
        run_arm(Arm::RdU8, case, &mut other);
        assert!(
            other.b.iter().flatten().zip(ours.f.iter().flatten()).all(|(&p, &q)| p as f32 / 255.0 == q),
            "{}: u8 and f32 paths disagree",
            case.name
        );
    }
    g.note = if case.name.starts_with("BC3") && g.differing > 0 {
        "dds 0.2 BC3 colour uses BC1 3-colour mode"
    } else if case.name.starts_with("BC5") && g.differing > 0 {
        "f32 interpolation rounding"
    } else {
        ""
    };
    g
}

// ------------------------------------------------------------- timing -------

struct Stats {
    best: f64,
    median: f64,
    p10: f64,
    p90: f64,
}

fn stats(v: &mut [f64]) -> Stats {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Stats { best: v[0], median: q(0.5), p10: q(0.1), p90: q(0.9) }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let pairs: usize = get("--pairs").map(|s| s.parse().unwrap()).unwrap_or(21);
    let min_ms: f64 = get("--min-ms").map(|s| s.parse().unwrap()).unwrap_or(60.0);
    let filter = get("--filter");
    let sizes = [(1024, 1), (4096, 1), (4096, 13)];

    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    println!("# rusty_dds (working tree, path dependency) vs dds 0.2.0 — decode to RGBA");
    println!(
        "method: one binary, both crates; allocator = {ALLOCATOR}; {threads} logical CPUs; \
         release; arms interleaved, order reversed each round; {pairs} rounds; \
         wall clock (Instant/QPC); buffers allocated once and reused in every arm; \
         K decodes per sample, K equal across arms, sample >= {min_ms} ms; \
         ms below = per decode (all mips for +mips rows)"
    );
    println!("content: ambientCG Rock064 (CC0) colour / roughness / normal, mirror-tiled to 4096^2; block formats encoded by rusty_dds");
    println!();

    let mut quantum = f64::INFINITY;

    println!("| case | arm | best ms | median ms | p10-p90 ms | vs dds 0.2 (median) |");
    println!("|---|---|---:|---:|---:|---:|");
    let mut gates = Vec::new();
    for_each_case(&sizes, &filter, |case| {
        let case = &case;
        let mut bufs = Bufs::new(&case.bytes, case.mips);
        let g = gate(case, &mut bufs);
        let mut arms = vec![Arm::Dds02F32, Arm::RdF32, Arm::RdF32Null];
        if case.bc7 {
            arms.push(Arm::RdF32Serial);
        }
        if case.has_u8 {
            arms.push(Arm::RdU8);
        }

        // Warm (page-fault every buffer, fault in code) and calibrate K on the
        // slowest arm so every arm's sample clears `min_ms`.
        let mut worst = 0f64;
        for &a in &arms {
            run_arm(a, case, &mut bufs);
            let t = Instant::now();
            run_arm(a, case, &mut bufs);
            worst = worst.max(t.elapsed().as_secs_f64() * 1e3);
        }
        let fastest_guess = worst / 8.0; // the fastest arm is rarely >8x quicker
        let k = ((min_ms / fastest_guess.max(1e-3)).ceil() as usize).clamp(1, 100_000);

        let mut samples: Vec<Vec<f64>> = vec![Vec::with_capacity(pairs); arms.len()];
        for round in 0..pairs {
            let order: Vec<usize> = if round % 2 == 0 {
                (0..arms.len()).collect()
            } else {
                (0..arms.len()).rev().collect()
            };
            for i in order {
                let t = Instant::now();
                for _ in 0..k {
                    run_arm(arms[i], case, &mut bufs);
                }
                samples[i].push(t.elapsed().as_secs_f64() * 1e3 / k as f64);
            }
        }
        for s in &samples {
            let mut v: Vec<f64> = s.iter().map(|x| x * k as f64).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            for w in v.windows(2) {
                if w[1] > w[0] {
                    quantum = quantum.min(w[1] - w[0]);
                }
            }
        }
        let st: Vec<Stats> = samples.iter_mut().map(|s| stats(s)).collect();
        let base = st[0].median;
        for (i, a) in arms.iter().enumerate() {
            let s = &st[i];
            let ratio = if i == 0 {
                "1.00x".to_string()
            } else {
                format!("{:.2}x faster", base / s.median)
            };
            println!(
                "| {} | {} | {:.3} | {:.3} | {:.3}-{:.3} | {} |",
                case.name,
                a.label(),
                s.best,
                s.median,
                s.p10,
                s.p90,
                ratio
            );
        }
        // Null-arm floor: median of the paired |ratio - 1| between the two
        // identical rusty_dds arms.
        let mut pr: Vec<f64> = samples[1]
            .iter()
            .zip(&samples[2])
            .map(|(a, b)| (a / b - 1.0).abs())
            .collect();
        pr.sort_by(|a, b| a.partial_cmp(b).unwrap());
        gates.push((case.name.clone(), g, bufs.pixels(), k, pr[pr.len() / 2], pr[pr.len() - 1]));
    });

    println!();
    println!("clock quantum (smallest gap between distinct raw samples): {:.4} ms", quantum);
    println!();
    println!("| case | pixels/decode | K | null arm median / worst pair | f32 gate vs dds 0.2: differing channels | max abs diff | note |");
    println!("|---|---:|---:|---:|---:|---:|---|");
    for (name, g, px, k, nmed, nworst) in gates {
        println!(
            "| {name} | {px} | {k} | {:.1}% / {:.1}% | {} of {} | {:.2e} | {} |",
            nmed * 100.0,
            nworst * 100.0,
            g.differing,
            g.total,
            g.max_abs,
            g.note
        );
    }
}
