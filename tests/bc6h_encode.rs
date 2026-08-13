//! BC6H_UF16 encode round-trip gates: encode → decode (bcdec oracle) must
//! reconstruct HDR content within PSNR floors, exactly reproduce
//! endpoint-representable content, and honor the UF16 clamp rules.

#![cfg(all(feature = "decode", feature = "encode"))]

use rusty_dds::*;

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// PSNR over positive HDR values in log2 space (HDR error is relative, not
/// absolute): 20*log10(peak/rmse) on log2-luminance-ish per-channel values.
fn log_psnr(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut sse = 0f64;
    let mut n = 0usize;
    for (&x, &y) in a.iter().zip(b) {
        let lx = (x.max(1e-6) as f64).log2();
        let ly = (y.max(1e-6) as f64).log2();
        sse += (lx - ly) * (lx - ly);
        n += 1;
    }
    if sse == 0.0 {
        return f64::INFINITY;
    }
    // peak = full half-float log2 range (~[-20, 16] ≈ 36 stops)
    let rmse = (sse / n as f64).sqrt();
    20.0 * (36.0 / rmse).log10()
}

fn roundtrip(pixels: &[f32], w: u32, h: u32) -> Vec<f32> {
    let dds = Dds::encode_bc6h_uf16(pixels, w, h).expect("encode");
    assert_eq!(dds.get_dxgi_format(), Some(DxgiFormat::BC6H_UF16));
    let img = dds
        .decode_rgba_f32(SubresourceId::mip_layer(0, 0))
        .expect("decode");
    assert_eq!((img.width, img.height), (w, h));
    img.pixels
}

#[test]
fn hdr_gradient_roundtrip_floor() {
    // Smooth HDR gradient spanning ~14 stops: the mode-11 sweet spot.
    let (w, h) = (64u32, 64u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = (x as f32 + 1.0) / w as f32;
            let s = (y as f32 + 1.0) / h as f32;
            let base = 2f32.powf(t * 14.0 - 4.0); // 1/16 .. 1024
            px.extend_from_slice(&[base, base * (0.5 + 0.5 * s), base * 0.25, 1.0]);
        }
    }
    let out = roundtrip(&px, w, h);
    let (a, b): (Vec<f32>, Vec<f32>) = (
        px.chunks(4).flat_map(|c| c[..3].to_vec()).collect(),
        out.chunks(4).flat_map(|c| c[..3].to_vec()).collect(),
    );
    let psnr = log_psnr(&a, &b);
    assert!(psnr > 40.0, "gradient log-PSNR too low: {psnr:.2} dB");
}

#[test]
fn hdr_noise_roundtrip_floor() {
    // Per-block random HDR values across ~8 stops: worst-ish case.
    let (w, h) = (32u32, 32u32);
    let mut state = 0xC0FFEE;
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        let r = xorshift(&mut state);
        let e0 = ((r & 0xFF) as f32 / 255.0) * 8.0 - 2.0;
        let e1 = (((r >> 8) & 0xFF) as f32 / 255.0) * 8.0 - 2.0;
        let e2 = (((r >> 16) & 0xFF) as f32 / 255.0) * 8.0 - 2.0;
        px.extend_from_slice(&[2f32.powf(e0), 2f32.powf(e1), 2f32.powf(e2), 1.0]);
    }
    let out = roundtrip(&px, w, h);
    let (a, b): (Vec<f32>, Vec<f32>) = (
        px.chunks(4).flat_map(|c| c[..3].to_vec()).collect(),
        out.chunks(4).flat_map(|c| c[..3].to_vec()).collect(),
    );
    let psnr = log_psnr(&a, &b);
    assert!(psnr > 20.0, "noise log-PSNR too low: {psnr:.2} dB");
}

#[test]
fn flat_block_reconstructs_near_exact() {
    // A constant color is endpoint-representable: error is only the 10-bit
    // endpoint quantization. The quantizer steps by ~31 half-bit units, so
    // the RELATIVE floor loosens at tiny magnitudes (~1.4% at 0.001 — an
    // inherent mode-11 property, matched by DirectXTex).
    // Worst-case rel error = half a quantizer step = ~15.5 half-bit units
    // over a 1024-unit mantissa octave ~= 1.5%; tiny magnitudes add
    // subnormal coarseness.
    for (v, floor) in [
        (0.001f32, 0.02),
        (0.5, 0.016),
        (1.0, 0.016),
        (37.5, 0.016),
        (1000.0, 0.016),
    ] {
        let px: Vec<f32> = (0..16).flat_map(|_| [v, v, v, 1.0]).collect();
        let out = roundtrip(&px, 4, 4);
        for c in out.chunks(4) {
            for ch in 0..3 {
                let rel = (c[ch] - v).abs() / v;
                assert!(rel < floor, "flat {v}: got {} (rel {rel})", c[ch]);
            }
        }
    }
}

#[test]
fn uf16_clamps_negative_nan_and_huge() {
    let px: Vec<f32> = vec![
        -5.0, f32::NAN, 1e30, 0.25, // pixel 0: neg, NaN, huge, -
        0.0, 0.0, 0.0, 1.0,
    ]
    .into_iter()
    .chain(std::iter::repeat(0.0).take(14 * 4))
    .collect();
    let out = roundtrip(&px, 4, 4);
    assert_eq!(out[0], 0.0, "negative must clamp to 0");
    assert_eq!(out[1], 0.0, "NaN must clamp to 0");
    assert!(
        out[2] > 60000.0 && out[2].is_finite(),
        "huge must clamp near 65504, got {}",
        out[2]
    );
}

#[test]
fn npot_and_truncation_fail_closed() {
    // NPOT encodes fine (edge clamp) …
    let px: Vec<f32> = (0..10 * 6).flat_map(|i| [i as f32, 1.0, 2.0, 1.0]).collect();
    let out = roundtrip(&px, 10, 6);
    assert_eq!(out.len(), 10 * 6 * 4);
    // … while short buffers are refused.
    assert!(Dds::encode_bc6h_uf16(&px[..10], 10, 6).is_err());
    assert!(Dds::encode_bc6h_uf16(&px, 0, 6).is_err());
}

// ---------------------------------------------------------------------------
// Real-content gate: Polyhaven CC0 HDRIs (corpus/raw_hdr, gitignored).
// Skips gracefully when the corpus has not been fetched.
// ---------------------------------------------------------------------------

/// Minimal Radiance RGBE (.hdr) reader: new-style RLE scanlines.
fn read_radiance_hdr(path: &std::path::Path) -> Option<(usize, usize, Vec<f32>)> {
    let data = std::fs::read(path).ok()?;
    let mut pos = 0usize;
    let mut line = || {
        let start = pos;
        while pos < data.len() && data[pos] != b'\n' {
            pos += 1;
        }
        let s = std::str::from_utf8(&data[start..pos]).ok().map(|s| s.to_string());
        pos += 1;
        s
    };
    if !line()?.starts_with("#?") {
        return None;
    }
    loop {
        let l = line()?;
        if l.is_empty() {
            break;
        }
    }
    let res = line()?;
    let mut it = res.split_whitespace();
    let (dy, h, dx, w) = (it.next()?, it.next()?, it.next()?, it.next()?);
    if dy != "-Y" || dx != "+X" {
        return None;
    }
    let h: usize = h.parse().ok()?;
    let w: usize = w.parse().ok()?;
    let mut out = vec![0f32; w * h * 4];
    let mut rgbe_row = vec![0u8; w * 4];
    for y in 0..h {
        if pos + 4 > data.len() {
            return None;
        }
        if data[pos] == 2
            && data[pos + 1] == 2
            && ((data[pos + 2] as usize) << 8 | data[pos + 3] as usize) == w
        {
            pos += 4;
            for c in 0..4 {
                let mut x = 0usize;
                while x < w {
                    let n = data.get(pos).copied()? as usize;
                    pos += 1;
                    if n > 128 {
                        let v = data.get(pos).copied()?;
                        pos += 1;
                        for _ in 0..n - 128 {
                            rgbe_row[x * 4 + c] = v;
                            x += 1;
                        }
                    } else {
                        for _ in 0..n {
                            rgbe_row[x * 4 + c] = data.get(pos).copied()?;
                            pos += 1;
                            x += 1;
                        }
                    }
                }
            }
        } else {
            for x in 0..w {
                rgbe_row[x * 4..x * 4 + 4].copy_from_slice(data.get(pos..pos + 4)?);
                pos += 4;
            }
        }
        for x in 0..w {
            let e = rgbe_row[x * 4 + 3] as i32;
            let scale = if e == 0 { 0.0 } else { (2f32).powi(e - 136) };
            let d = (y * w + x) * 4;
            out[d] = rgbe_row[x * 4] as f32 * scale;
            out[d + 1] = rgbe_row[x * 4 + 1] as f32 * scale;
            out[d + 2] = rgbe_row[x * 4 + 2] as f32 * scale;
            out[d + 3] = 1.0;
        }
    }
    Some((w, h, out))
}

#[test]
fn real_hdri_corpus_roundtrip_floor() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpus/raw_hdr");
    if !root.exists() {
        eprintln!("corpus/raw_hdr not fetched; skipping");
        return;
    }
    let mut tested = 0;
    for entry in std::fs::read_dir(&root).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("hdr") {
            continue;
        }
        let Some((w, h, px)) = read_radiance_hdr(&path) else {
            panic!("failed to parse {}", path.display());
        };
        let out = roundtrip(&px, w as u32, h as u32);
        let (a, b): (Vec<f32>, Vec<f32>) = (
            px.chunks(4).flat_map(|c| c[..3].to_vec()).collect(),
            out.chunks(4).flat_map(|c| c[..3].to_vec()).collect(),
        );
        let psnr = log_psnr(&a, &b);
        eprintln!(
            "{}: {}x{} log-PSNR {:.2} dB",
            path.file_name().unwrap().to_string_lossy(),
            w,
            h,
            psnr
        );
        assert!(
            psnr > 35.0,
            "{}: real-HDRI log-PSNR too low: {psnr:.2} dB",
            path.display()
        );
        tested += 1;
    }
    assert!(tested >= 4, "expected the 4 fetched HDRIs, found {tested}");
}

/// Speed probe for the report (run explicitly, pinned externally).
#[test]
#[ignore]
fn bc6h_encode_speed_probe() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpus/raw_hdr");
    if !root.exists() {
        return;
    }
    for entry in std::fs::read_dir(&root).unwrap().flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("hdr") {
            continue;
        }
        let (w, h, px) = read_radiance_hdr(&path).unwrap();
        let mut best = u128::MAX;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let dds = Dds::encode_bc6h_uf16(&px, w as u32, h as u32).unwrap();
            std::hint::black_box(&dds.data);
            best = best.min(t.elapsed().as_nanos());
        }
        let mpxs = (w * h) as f64 / (best as f64 / 1e9) / 1e6;
        eprintln!(
            "{}: {}x{} encode best {} ms  ({:.1} Mpx/s)",
            path.file_name().unwrap().to_string_lossy(),
            w, h, best / 1_000_000, mpxs
        );
    }
}
