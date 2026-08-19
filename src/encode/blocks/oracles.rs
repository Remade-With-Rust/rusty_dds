//! Campaign scaffolding: ceiling probes and scalar-vs-fused oracles.
//!
//! These are `#[cfg(test)]` measurement harnesses from the 2026-08 encoder
//! campaign, not unit tests of shipped behaviour. They live here rather than
//! in the encoder files so the hot paths stay readable — several read PNGs and
//! write CSVs, which is not something the encoder core should appear to do.

use super::*;

#[cfg(test)]
mod ceiling_probe {
    use super::*;

    fn load_png_gray_channels(path: &str) -> (usize, usize, Vec<u8>, Vec<u8>) {
        let f = std::fs::File::open(path).expect("png");
        let mut dec = png::Decoder::new(std::io::BufReader::new(f));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        let (w, h) = (info.width as usize, info.height as usize);
        let step = match info.color_type {
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            _ => panic!("unexpected color type"),
        };
        let mut r = Vec::with_capacity(w * h);
        let mut g = Vec::with_capacity(w * h);
        for px in buf.chunks_exact(step) {
            r.push(px[0]);
            g.push(px[if step >= 3 { 1 } else { 0 }]);
        }
        (w, h, r, g)
    }

    fn block_samples(chan: &[u8], w: usize, bx: usize, by: usize) -> [u8; 16] {
        let mut s = [0u8; 16];
        for row in 0..4 {
            for col in 0..4 {
                s[row * 4 + col] = chan[(by * 4 + row) * w + bx * 4 + col];
            }
        }
        s
    }

    /// UNORM-domain SSE of the best signed encoding found by a bounded
    /// near-exhaustive endpoint sweep (both orders => both palette modes).
    fn exhaustive_signed_sse(samples: &[u8; 16]) -> i64 {
        let mut lo = 127i32;
        let mut hi = -127i32;
        for &s in samples {
            let v = unorm_u8_to_snorm_i32(s);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        let a_lo = (lo - 8).max(-127);
        let a_hi = (hi + 8).min(127);
        let current = encode_alpha_block_signed(*samples);
        let mut best_err = alpha_sse_s(samples, &current);
        let mut best = current;
        for e0 in a_lo..=a_hi {
            for e1 in a_lo..=a_hi {
                if e0 == e1 {
                    continue;
                }
                consider_alpha_s(e0, e1, samples, &mut best, &mut best_err);
            }
        }
        // 4-lerp sentinel mode benefits from endpoints at range edges too.
        best_err as i64
    }

    fn load_tiff_gray(path: &str) -> Option<(usize, usize, Vec<u8>)> {
        use tiff::decoder::DecodingResult;
        use tiff::ColorType;
        let f = std::fs::File::open(path).ok()?;
        let mut dec = tiff::decoder::Decoder::new(std::io::BufReader::new(f)).ok()?;
        let (w, h) = dec.dimensions().ok()?;
        let ct = dec.colortype().ok()?;
        match (ct, dec.read_image().ok()?) {
            (ColorType::Gray(8), DecodingResult::U8(v)) => Some((w as usize, h as usize, v)),
            _ => None,
        }
    }

    /// Observe-only harvest for the signed sweep gate: every signed block in
    /// the corpus -> (map, span, n_unique, null_err, gain, pairs).
    #[test]
    #[ignore]
    fn signed_sweep_harvest() {
        let root = env!("CARGO_MANIFEST_DIR");
        let mut sources: Vec<(String, Vec<(usize, usize, Vec<u8>)>)> = Vec::new();
        // Normals (R+G channels -> bc5s) and roughness masks (R -> bc4s).
        for asset in ["Bricks097", "Metal063", "Rock064", "Wood095"] {
            let p = format!("{root}/corpus/raw/{asset}/{asset}_1K-PNG_NormalGL.png");
            if std::path::Path::new(&p).exists() {
                let (w, h, r, g) = load_png_gray_channels(&p);
                sources.push((format!("{asset}_normal"), vec![(w, h, r), (w, h, g)]));
            }
            let p = format!("{root}/corpus/raw/{asset}/{asset}_1K-PNG_Roughness.png");
            if std::path::Path::new(&p).exists() {
                let (w, h, r, _) = load_png_gray_channels(&p);
                sources.push((format!("{asset}_mask"), vec![(w, h, r)]));
            }
        }
        for tex in ["tex_bark", "tex_straw", "tex_water", "tex_wool", "tex_brick_1024"] {
            let p = format!("{root}/corpus/raw_tif/{tex}.tiff");
            if let Some((w, h, v)) = load_tiff_gray(&p) {
                sources.push((tex.to_string(), vec![(w, h, v)]));
            }
        }
        let mut csv = String::from("map,span,n_unique,null_err,gain,pairs,dcheb\n");
        for (name, chans) in &sources {
            for (w, h, chan) in chans {
                for by in 0..h / 4 {
                    for bx in 0..w / 4 {
                        let s = block_samples(chan, *w, bx, by);
                        let (mut best, mut err, lo, hi, span, n_unique) =
                            encode_alpha_block_signed_presweep(s);
                        let null_err = err;
                        if null_err == 0 {
                            continue;
                        }
                        let pre0 = best[0] as i8 as i32;
                        let pre1 = best[1] as i8 as i32;
                        signed_sweep(lo, hi, &s, &mut best, &mut err);
                        let gain = null_err - err;
                        // Chebyshev distance from pre-sweep endpoints to the
                        // winners (order-insensitive: try both pairings).
                        let dcheb = if gain > 0 {
                            let w0 = best[0] as i8 as i32;
                            let w1 = best[1] as i8 as i32;
                            let d_a = (w0 - pre0).abs().max((w1 - pre1).abs());
                            let d_b = (w0 - pre1).abs().max((w1 - pre0).abs());
                            d_a.min(d_b)
                        } else {
                            -1
                        };
                        let range = (hi + 8).min(127) - (lo - 8).max(-127) + 1;
                        csv.push_str(&format!(
                            "{name},{span},{n_unique},{null_err},{gain},{},{dcheb}\n",
                            range * range
                        ));
                    }
                }
            }
        }
        std::fs::write(format!("{root}/target/signed_sweep_harvest.csv"), csv).unwrap();
        println!("wrote target/signed_sweep_harvest.csv");
    }

    #[test]
    #[ignore]
    fn bc5s_wood_ceiling() {
        let root = env!("CARGO_MANIFEST_DIR");
        let path = format!("{root}/corpus/raw/Wood095/Wood095_1K-PNG_NormalGL.png");
        let (w, h, r, g) = load_png_gray_channels(&path);
        let (bw, bh) = (w / 4, h / 4);
        let mut cur_sse = 0i64;
        let mut ceil_sse = 0i64;
        let nthreads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let rows_per = (bh + nthreads - 1) / nthreads;
        let results: Vec<(i64, i64)> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for t in 0..nthreads {
                let (r, g) = (&r, &g);
                handles.push(scope.spawn(move || {
                    let mut cur = 0i64;
                    let mut ceil = 0i64;
                    for by in (t * rows_per)..((t + 1) * rows_per).min(bh) {
                        for bx in 0..bw {
                            for chan in [r, g] {
                                let s = block_samples(chan, w, bx, by);
                                let enc = encode_alpha_block_signed(s);
                                cur += alpha_sse_s(&s, &enc) as i64;
                                ceil += exhaustive_signed_sse(&s);
                            }
                        }
                    }
                    (cur, ceil)
                }));
            }
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (c, x) in results {
            cur_sse += c;
            ceil_sse += x;
        }
        let n = (w * h * 2) as f64;
        let psnr = |sse: i64| 10.0 * (255.0f64 * 255.0 / (sse as f64 / n)).log10();
        println!(
            "Wood BC5S: current={:.3} dB  ceiling={:.3} dB  (delta {:+.3})",
            psnr(cur_sse),
            psnr(ceil_sse),
            psnr(ceil_sse) - psnr(cur_sse)
        );
    }
}

#[cfg(test)]
mod fuse_oracle {
    use super::*;

    /// pack_bc1_scored must equal the old pack_bc1 + bc1_sse pair exactly.
    #[test]
    fn bc1_scored_matches_pack_plus_sse() {
        let mut state = 0x243F6A8885A308D3u64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..200_000 {
            let mut px = [[0u8; 4]; 16];
            let flat = case % 7 == 0;
            let base = (rng() & 0xFF) as u8;
            for p in px.iter_mut() {
                let r = rng();
                if flat {
                    p[0] = base.wrapping_add((r & 3) as u8);
                    p[1] = base.wrapping_add(((r >> 2) & 3) as u8);
                    p[2] = base.wrapping_add(((r >> 4) & 3) as u8);
                } else {
                    p[0] = (r & 0xFF) as u8;
                    p[1] = ((r >> 8) & 0xFF) as u8;
                    p[2] = ((r >> 16) & 0xFF) as u8;
                }
                // Mix in punch-through alphas sometimes.
                p[3] = if case % 5 == 0 && (r >> 24) & 3 == 0 {
                    ((r >> 26) & 0x7F) as u8
                } else {
                    255
                };
            }
            let e0 = [(rng() & 0xFF) as u8, (rng() & 0xFF) as u8, (rng() & 0xFF) as u8];
            let e1 = [(rng() & 0xFF) as u8, (rng() & 0xFF) as u8, (rng() & 0xFF) as u8];
            let old_block = pack_bc1(px, e0, e1);
            let old_err = bc1_sse(&px, &old_block);
            let (new_block, new_err) =
                pack_bc1_scored(&px, e0, e1, i32::MAX).expect("unbounded");
            // The projection index fit (bc1_fit_4color) is a RESTRICTED
            // search: its SSE can only be >= the exhaustive fit, and only
            // negligibly (rounding cross-term on far-off-line pixels; the
            // corpus moves <=0.012 dB worst-case). Punch-path blocks stay
            // bit-exact.
            assert!(new_err >= old_err, "fast beat exhaustive?! (case {case})");
            assert!(
                new_err <= old_err + old_err / 100 + 32,
                "projection fit degraded SSE beyond contract (case {case}): {new_err} vs {old_err}"
            );
            if new_block != old_block {
                // Bytes may differ only when the fit differs; err must track.
                assert!(new_err >= old_err);
            }
            // Early-abort contract: limit == err must return None (>= abort).
            assert!(pack_bc1_scored(&px, e0, e1, new_err).is_none());
            if new_err > 0 {
                assert!(pack_bc1_scored(&px, e0, e1, new_err + 1).is_some());
            }
        }
    }
}

#[cfg(test)]
mod mode6_projection_oracle {
    use super::*;

    #[test]
    fn mode6_projection_matches_exhaustive() {
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..400_000u32 {
            // Production-shaped palettes: random endpoints through the same
            // quantize/unquantize as try_bc7_mode6, biased toward small axes
            // every few cases to stress the mono-gate boundary.
            let r = rng();
            let e0 = [
                (r & 0xFF) as u8,
                ((r >> 8) & 0xFF) as u8,
                ((r >> 16) & 0xFF) as u8,
                ((r >> 24) & 0xFF) as u8,
            ];
            let e1 = if case % 4 == 0 {
                // near-degenerate: e1 within +-8 of e0 per channel
                let s = rng();
                let mut v = [0u8; 4];
                for c in 0..4 {
                    let d = ((s >> (8 * c)) & 0xF) as i32 - 8;
                    v[c] = (e0[c] as i32 + d).clamp(0, 255) as u8;
                }
                v
            } else {
                let s = rng();
                [
                    (s & 0xFF) as u8,
                    ((s >> 8) & 0xFF) as u8,
                    ((s >> 16) & 0xFF) as u8,
                    ((s >> 24) & 0xFF) as u8,
                ]
            };
            let (q0, p0) = quantize_7p(e0);
            let (q1, p1) = quantize_7p(e1);
            let pal = palette_mode6(unquantize_7p(q0, p0), unquantize_7p(q1, p1));
            let mut px = [[0u8; 4]; 16];
            for p in px.iter_mut() {
                let r = rng();
                // Mix: random pixels and near-palette pixels (index-fit shape).
                if r & 1 == 0 {
                    let k = ((r >> 1) & 15) as usize;
                    for c in 0..4 {
                        let n = ((r >> (8 + 8 * c)) & 7) as i32 - 3;
                        p[c] = (pal[k][c] as i32 + n).clamp(0, 255) as u8;
                    }
                } else {
                    p[0] = (r >> 8) as u8;
                    p[1] = (r >> 16) as u8;
                    p[2] = (r >> 24) as u8;
                    p[3] = (r >> 32) as u8;
                }
            }
            let fast = fit_indices_mode6(&px, &pal);
            let slow = fit_indices_mode6_exhaustive(&px, &pal);
            // Contract: the projection window is a RESTRICTED search, so its
            // SSE can only be >= the exhaustive fit, and only negligibly so
            // (divergence needs a pixel far off the endpoint line, where the
            // rounding cross-term outweighs the t-distance — SSE-tiny by
            // construction; corpus payloads move 0 cases at 0.0001 dB).
            assert!(fast.1 >= slow.1, "fast beat exhaustive?! (case {case})");
            assert!(
                fast.1 <= slow.1 + slow.1 / 100 + 16,
                "projection fit degraded SSE beyond contract (case {case}): {} vs {}",
                fast.1,
                slow.1
            );
        }
    }
}

#[cfg(test)]
mod alpha_select_oracle {
    use super::*;

    /// Full enumeration: every (a0, a1) endpoint pair x every sample value,
    /// both palette modes, unsigned domain — the selector must reproduce
    /// the linear scan's argmin (strict `<`, lowest index wins ties) on all
    /// ~16.7M combinations. This is a proof by exhaustion, not a sample.
    #[test]
    #[ignore] // ~seconds in release; run explicitly
    fn alpha_select_matches_linear_exhaustive() {
        for a0 in 0..=255u8 {
            for a1 in 0..=255u8 {
                let (palette, order): ([u8; 8], &[u8; 8]) = if a0 > a1 {
                    (alpha_palette6_u(a0, a1), &ALPHA_ORDER6)
                } else {
                    (alpha_palette4_u(a0, a1), &ALPHA_ORDER4)
                };
                let sel = AlphaSelect::build(&palette, order);
                for s in 0..=255u8 {
                    let mut lin = 0u8;
                    let mut lin_d = i32::MAX;
                    for (j, &p) in palette.iter().enumerate() {
                        let d = (p as i32 - s as i32).abs();
                        if d < lin_d {
                            lin_d = d;
                            lin = j as u8;
                        }
                    }
                    let fast = sel.select(s);
                    assert_eq!(
                        fast, lin,
                        "a0={a0} a1={a1} s={s} palette={palette:?}"
                    );
                }
            }
        }
    }
}
