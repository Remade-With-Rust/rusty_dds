//! rusty_dds ASTC decode against the two pure-Rust ASTC decoders on crates.io.
//!
//!   astc_bench <file.astc>... [--rounds N] [--profile ldr|srgb]
//!
//! Method (printed with every run):
//! * One binary links all arms, so they share compiler, flags and the global
//!   allocator: rusty_alloc (`rusty_alloc-api`), the house shipping allocator.
//! * Correctness first: each baseline's 8-bit output is compared with ours
//!   (ours is bit-exact with astcenc 5.7.1 `decode_unorm8`, gated in
//!   tools/astc-harness); the differing-pixel count is printed.
//! * Every arm decodes the same payload to 8-bit RGBA into a buffer allocated
//!   once (astc-decode writes through its per-texel callback; texture2ddecoder
//!   writes packed u32). No file I/O in the timed region.
//! * Arms interleave in-process and the order reverses every round (ABBA).
//!   Each sample runs K decodes, K calibrated so a sample is >= 50 ms.
//! * Null arm: ours twice under two names; its spread is the noise floor.
//! * Pin the process yourself (`taskset -c 2` on Linux, affinity on Windows).
//! * Clock: thread CPU time on Unix (`CLOCK_THREAD_CPUTIME_ID`), wall time
//!   elsewhere.

#[global_allocator]
static GLOBAL_ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;

use rusty_dds::astc::{self, AstcFootprint, AstcProfile};
use std::hint::black_box;

/// Seconds of this thread's CPU time on Unix (immune to preemption by other
/// processes on a shared host); wall time elsewhere.
#[cfg(unix)]
fn clock_now() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes one timespec through a valid pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}
#[cfg(not(unix))]
fn clock_now() -> f64 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

struct Input {
    name: String,
    fp: AstcFootprint,
    w: u32,
    h: u32,
    payload: Vec<u8>,
}

fn load(path: &str) -> Input {
    let d = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(&d[..4], &[0x13, 0xAB, 0xA1, 0x5C], "{path}: not an .astc file");
    let fp = AstcFootprint::new_3d(d[4] as u32, d[5] as u32, d[6] as u32).expect("footprint");
    let dim = |o: usize| d[o] as u32 | (d[o + 1] as u32) << 8 | (d[o + 2] as u32) << 16;
    let (w, h, z) = (dim(7), dim(10), dim(13));
    assert!(z == 1 && !fp.is_3d(), "2D only");
    let n = astc::payload_len(w, h, 1, fp).unwrap();
    Input {
        name: std::path::Path::new(path).file_name().unwrap().to_string_lossy().into_owned(),
        fp,
        w,
        h,
        payload: d[16..16 + n].to_vec(),
    }
}

type Arm<'a> = (&'static str, Box<dyn FnMut() + 'a>);

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn load_png(path: &str) -> (u32, u32, Vec<u8>) {
    let dec = png::Decoder::new(std::fs::File::open(path).unwrap_or_else(|e| panic!("{path}: {e}")));
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let px = (info.width * info.height) as usize;
    let rgba: Vec<u8> = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf[..px * 4].to_vec(),
        (png::ColorType::Rgb, png::BitDepth::Eight) => buf[..px * 3].chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        other => panic!("{path}: png {other:?}"),
    };
    (info.width, info.height, rgba)
}

/// `--encode <preset> <fp> <png>...`: single-threaded encode rate and PSNR.
fn encode_mode(args: &[String]) {
    let preset = match args[0].as_str() {
        "fastest" => astc::AstcPreset::Fastest,
        "fast" => astc::AstcPreset::Fast,
        "medium" => astc::AstcPreset::Medium,
        "thorough" => astc::AstcPreset::Thorough,
        _ => astc::AstcPreset::Exhaustive,
    };
    let v: Vec<u32> = args[1].split('x').map(|t| t.parse().unwrap()).collect();
    let fp = AstcFootprint::new_2d(v[0], v[1]).unwrap();
    println!("allocator: rusty_alloc; encode {preset:?} {fp}; best of 3");
    let mut tot_px = 0f64;
    let mut tot_s = 0f64;
    for f in &args[2..] {
        let (w, h, img) = load_png(f);
        let opts = astc::AstcEncodeOptions::new(fp, AstcProfile::Ldr).with_preset(preset);
        let mut best = f64::MAX;
        let mut data = Vec::new();
        for _ in 0..3 {
            let t = clock_now();
            data = astc::encode_rgba8(&img, w, h, &opts).unwrap();
            best = best.min((clock_now() - t));
        }
        let mut dec = vec![0u8; img.len()];
        astc::decode_rgba8_into(&data, w, h, 1, fp, AstcProfile::Ldr, &mut dec).unwrap();
        let se: f64 = img.chunks_exact(4).zip(dec.chunks_exact(4)).map(|(a, b)| (0..3).map(|c| (a[c] as f64 - b[c] as f64).powi(2)).sum::<f64>()).sum();
        let psnr = 10.0 * (255.0f64 * 255.0 * 3.0 * (w * h) as f64 / se).log10();
        let mut hsh: u64 = 0xCBF2_9CE4_8422_2325;
        for &b in &data {
            hsh ^= b as u64;
            hsh = hsh.wrapping_mul(0x100_0000_01B3);
        }
        println!("{f}: {:.3} MT/s  PSNR {psnr:.3} dB  hash {hsh:016x}", (w * h) as f64 / best / 1e6);
        tot_px += (w * h) as f64;
        tot_s += best;
    }
    println!("total: {:.3} MT/s", tot_px / tot_s / 1e6);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("--encode") {
        return encode_mode(&args[1..]);
    }
    #[cfg(feature = "internals")]
    if args.first().map(|s| s.as_str()) == Some("--parse") {
        // Parse-only time vs full fast decode, same blocks.
        for f in &args[1..] {
            let inp = load(f);
            let blocks: Vec<[u8; 16]> = inp.payload.chunks_exact(16).map(|c| c.try_into().unwrap()).collect();
            let mut best_p = f64::MAX;
            let mut best_d = f64::MAX;
            let mut out = vec![0u8; (inp.w * inp.h * 4) as usize];
            for _ in 0..5 {
                let t = clock_now();
                let mut acc = 0u32;
                for b in &blocks {
                    acc = acc.wrapping_add(rusty_dds::astc::internals::parse(b, inp.fp).color_count as u32);
                }
                black_box(acc);
                best_p = best_p.min((clock_now() - t));
                let t = clock_now();
                astc::decode_rgba8_into(&inp.payload, inp.w, inp.h, 1, inp.fp, AstcProfile::Ldr, &mut out).unwrap();
                best_d = best_d.min((clock_now() - t));
            }
            println!("{} {}: parse {:.1} ms, full decode {:.1} ms ({:.0}% parse)", inp.name, inp.fp, best_p * 1e3, best_d * 1e3, 100.0 * best_p / best_d);
        }
        return;
    }
    let mut rounds = 15usize;
    let mut profile = AstcProfile::Ldr;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--rounds" => {
                rounds = args[i + 1].parse().unwrap();
                i += 1;
            }
            "--profile" => {
                profile = if args[i + 1] == "srgb" { AstcProfile::LdrSrgb } else { AstcProfile::Ldr };
                i += 1;
            }
            f => files.push(f.to_string()),
        }
        i += 1;
    }
    println!("allocator: rusty_alloc (rusty_alloc-api 2.5.0); profile {profile:?}; {rounds} ABBA rounds; median MT/s; clock: {}", if cfg!(unix) { "thread CPU time" } else { "wall" });
    println!("| input | footprint | rusty_dds | null arm | astc-decode 0.3.1 | texture2ddecoder 0.1.2 | ratio vs faster baseline | baseline pixels differing (astc-decode / t2d) |");
    println!("|---|---|---|---|---|---|---|---|");
    for f in &files {
        let inp = load(f);
        let (w, h) = (inp.w, inp.h);
        let px = (w * h) as usize;
        let mut ours = vec![0u8; px * 4];
        astc::decode_rgba8_into(&inp.payload, w, h, 1, inp.fp, profile, &mut ours).unwrap();
        // Correctness: baselines against ours.
        let mut ad = vec![0u8; px * 4];
        astc_decode::astc_decode(
            &inp.payload[..],
            w,
            h,
            astc_decode::Footprint::new(inp.fp.width(), inp.fp.height()),
            |x, y, c| {
                let i = ((y * w + x) * 4) as usize;
                ad[i..i + 4].copy_from_slice(&c);
            },
        )
        .unwrap();
        let mut t2 = vec![0u32; px];
        texture2ddecoder::decode_astc(
            &inp.payload,
            w as usize,
            h as usize,
            inp.fp.width() as usize,
            inp.fp.height() as usize,
            &mut t2,
        )
        .unwrap();
        let diff_ad = ours.chunks_exact(4).zip(ad.chunks_exact(4)).filter(|(a, b)| a != b).count();
        let diff_t2 = ours
            .chunks_exact(4)
            .zip(t2.iter())
            .filter(|(a, p)| {
                let b = p.to_le_bytes();
                a[..] != [b[2], b[1], b[0], b[3]]
            })
            .count();

        let payload = &inp.payload;
        let fp = inp.fp;
        let mut buf_a = vec![0u8; px * 4];
        let mut buf_b = vec![0u8; px * 4];
        let mut buf_c = vec![0u8; px * 4];
        let mut buf_d = vec![0u32; px];
        let mut arms: Vec<Arm> = vec![
            (
                "rusty_dds",
                Box::new(|| {
                    astc::decode_rgba8_into(payload, w, h, 1, fp, profile, &mut buf_a).unwrap();
                    black_box(&buf_a);
                }),
            ),
            (
                "null",
                Box::new(|| {
                    astc::decode_rgba8_into(payload, w, h, 1, fp, profile, &mut buf_b).unwrap();
                    black_box(&buf_b);
                }),
            ),
            (
                "astc-decode",
                Box::new(|| {
                    astc_decode::astc_decode(
                        &payload[..],
                        w,
                        h,
                        astc_decode::Footprint::new(fp.width(), fp.height()),
                        |x, y, c| {
                            let i = ((y * w + x) * 4) as usize;
                            buf_c[i..i + 4].copy_from_slice(&c);
                        },
                    )
                    .unwrap();
                    black_box(&buf_c);
                }),
            ),
            (
                "texture2ddecoder",
                Box::new(|| {
                    texture2ddecoder::decode_astc(
                        payload,
                        w as usize,
                        h as usize,
                        fp.width() as usize,
                        fp.height() as usize,
                        &mut buf_d,
                    )
                    .unwrap();
                    black_box(&buf_d);
                }),
            ),
        ];
        // One K for all arms (work parity), set by the fastest arm so every sample is >= 50 ms.
        let mut k = 1usize;
        for (_, arm) in arms.iter_mut() {
            let t = clock_now();
            arm();
            let s = (clock_now() - t);
            k = k.max((0.05 / s).ceil() as usize);
        }
        let mut samples: Vec<Vec<f64>> = vec![Vec::new(); arms.len()];
        for r in 0..rounds {
            let order: Vec<usize> = if r % 2 == 0 {
                (0..arms.len()).collect()
            } else {
                (0..arms.len()).rev().collect()
            };
            for a in order {
                let t = clock_now();
                for _ in 0..k {
                    (arms[a].1)();
                }
                let s = (clock_now() - t);
                samples[a].push(px as f64 * k as f64 / s / 1e6);
            }
        }
        let med: Vec<f64> = samples.iter_mut().map(|s| median(s)).collect();
        let best_base = med[2].max(med[3]);
        println!(
            "| {} {}x{} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.2}x | {} / {} of {} |",
            inp.name,
            w,
            h,
            fp,
            med[0],
            med[1],
            med[2],
            med[3],
            med[0] / best_base,
            diff_ad,
            diff_t2,
            px
        );
    }
}
