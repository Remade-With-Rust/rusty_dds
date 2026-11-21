//! Differential harness: rusty_dds ASTC stages against the astc-oracle
//! (ARM astcenc 5.7.1). WSL only; see tools/astc-oracle/README.md.
//!
//!   astch modes                      DC3: block-mode table, all 24 footprints
//!   astch partitions                 DC4: partition maps, all 24 footprints
//!   astch ise [n]                    D2: ISE decode, 21 ranges x lengths 1..=64
//!   astch info <profile> <fp> <src>  D3-D6: per-field symbolic/endpoints/weights
//!   astch decode <profile> <u8|f16> <fp> <src>   D7/DC1/DC2: texels
//!   astch sweep <src> [n]            info + decode, every footprint x six cells
//!
//! <src>: `random:N` (uniform 128-bit blocks), `mutate:N:file.astc` (1..=8 bit
//! flips of corpus blocks), `file:path.astc` (the file's blocks),
//! `mix:N` (random blocks biased to legal multi-partition layouts).

mod census;
mod gen;
mod oracle;

use rusty_dds::astc::internals as ai;
use rusty_dds::astc::{self, AstcFootprint, AstcProfile};

fn fp_parse(s: &str) -> AstcFootprint {
    let v: Vec<u32> = s.split('x').map(|t| t.parse().expect("footprint")).collect();
    match v.len() {
        2 => AstcFootprint::new_2d(v[0], v[1]),
        3 => AstcFootprint::new_3d(v[0], v[1], v[2]),
        _ => None,
    }
    .expect("illegal footprint")
}

fn prof_parse(s: &str) -> AstcProfile {
    match s {
        "ldr-srgb" => AstcProfile::LdrSrgb,
        "ldr" => AstcProfile::Ldr,
        "hdr-ldr-a" => AstcProfile::HdrRgbLdrA,
        "hdr" => AstcProfile::Hdr,
        _ => panic!("profile"),
    }
}

fn prof_name(p: AstcProfile) -> &'static str {
    match p {
        AstcProfile::LdrSrgb => "ldr-srgb",
        AstcProfile::Ldr => "ldr",
        AstcProfile::HdrRgbLdrA => "hdr-ldr-a",
        _ => "hdr",
    }
}

fn all_fps() -> Vec<AstcFootprint> {
    AstcFootprint::ALL_2D
        .iter()
        .chain(AstcFootprint::ALL_3D.iter())
        .copied()
        .collect()
}

fn fp_args(fp: AstcFootprint) -> [String; 3] {
    [
        fp.width().to_string(),
        fp.height().to_string(),
        fp.depth().to_string(),
    ]
}

fn hex(b: &[u8; 16]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn cmd_modes() -> bool {
    let mut ok = true;
    let mut total = 0;
    let mut good = 0;
    for fp in all_fps() {
        let a = fp_args(fp);
        let o = oracle::run(&["modes", &a[0], &a[1], &a[2]], &[]);
        assert_eq!(o.len(), 2048 * 8);
        let mut bad = 0;
        for m in 0..2048u32 {
            let r = &o[m as usize * 8..m as usize * 8 + 8];
            let ours: [u8; 8] = match ai::classify(m, fp) {
                ai::ModeClass::Grid(g) if ai::check(&g, fp).is_ok() => [
                    1,
                    g.w,
                    g.h,
                    g.d,
                    g.range,
                    g.dual_plane as u8,
                    g.weight_count() as u8,
                    g.weight_bits() as u8,
                ],
                _ => [0; 8],
            };
            total += 1;
            if ours[..] == r[..] {
                good += 1;
            } else {
                bad += 1;
                if bad <= 5 {
                    println!("  {fp} mode {m:#05x}: ours {ours:?} oracle {r:?}");
                }
            }
        }
        if bad > 0 {
            ok = false;
            println!("{fp}: {bad} mismatching modes");
        }
    }
    println!("DC3 block modes: {good} / {total} equal");
    ok
}

fn cmd_partitions() -> bool {
    let mut total = 0;
    let mut good = 0;
    for fp in all_fps() {
        let a = fp_args(fp);
        let o = oracle::run(&["partitions", &a[0], &a[1], &a[2]], &[]);
        let tc = fp.texels() as usize;
        assert_eq!(o.len(), 3 * 1024 * tc);
        let small = tc < 31;
        let mut bad = 0;
        for c in 2..=4u32 {
            for s in 0..1024u32 {
                let base = ((c as usize - 2) * 1024 + s as usize) * tc;
                let mut i = 0;
                let mut same = true;
                for z in 0..fp.depth() {
                    for y in 0..fp.height() {
                        for x in 0..fp.width() {
                            if ai::select_partition(s, x, y, z, c, small) != o[base + i] {
                                same = false;
                            }
                            i += 1;
                        }
                    }
                }
                total += 1;
                if same {
                    good += 1;
                } else {
                    bad += 1;
                    if bad <= 3 {
                        println!("  {fp} count {c} seed {s}: map differs");
                    }
                }
            }
        }
        if bad > 0 {
            println!("{fp}: {bad} mismatching maps");
        }
    }
    println!("DC4 partition maps: {good} / {total} equal");
    good == total
}

fn cmd_ise(n: usize) -> bool {
    let mut rng = gen::Rng::new(0x15E);
    let mut inputs = vec![0u8; n * 72];
    rng.fill(&mut inputs);
    let mut ok = true;
    let mut cases = 0u64;
    for range in 0..21usize {
        for count in 1..=64usize {
            // Only lengths whose sequence fits in the 576-bit input.
            if ai::sequence_bits(count as u32, range) > 72 * 8 - 16 {
                continue;
            }
            let o = oracle::run(&["ise", &range.to_string(), &count.to_string()], &inputs);
            assert_eq!(o.len(), n * count);
            for k in 0..n {
                let inp = &inputs[k * 72..k * 72 + 72];
                // Our decoder reads a u128; split long sequences across words
                // by decoding from a shifted window when they exceed 128 bits.
                let bits = ai::sequence_bits(count as u32, range);
                if bits > 128 {
                    continue;
                }
                let mut w = [0u8; 16];
                w.copy_from_slice(&inp[..16]);
                let src = u128::from_le_bytes(w);
                // Bits beyond the sequence must read as zero for our decoder
                // too: mask the window to the sequence length plus the final
                // block's unread tail, exactly as the oracle sees the stream.
                let mut out = [0u8; 64];
                ai::ise_decode(src, 0, range, &mut out[..count]);
                cases += 1;
                if out[..count] != o[k * count..(k + 1) * count] {
                    if ok {
                        println!(
                            "ISE mismatch range {range} count {count}: ours {:?} oracle {:?}",
                            &out[..count],
                            &o[k * count..(k + 1) * count]
                        );
                    }
                    ok = false;
                }
            }
        }
    }
    println!("D2 ISE: {cases} sequences compared, {}", if ok { "all equal" } else { "MISMATCH" });
    ok
}

/// INFO record layout (tools/astc-oracle/oracle.cpp).
const INFO_SIZE: usize = 1 + 1 + 2 + 2 + 4 + 1 + 1 + 1 + 3 + 1 + 1 + 8 + 32 + 64 + 128 + 8 + 216 * 3;

struct Info<'a>(&'a [u8]);
impl Info<'_> {
    fn u8(&self, o: usize) -> u8 {
        self.0[o]
    }
    fn u16(&self, o: usize) -> u16 {
        u16::from_le_bytes([self.0[o], self.0[o + 1]])
    }
    fn i32(&self, o: usize) -> i32 {
        i32::from_le_bytes([self.0[o], self.0[o + 1], self.0[o + 2], self.0[o + 3]])
    }
}

/// Compare one block's fields. Returns the first differing field name.
fn info_diff(blk: &[u8; 16], fp: AstcFootprint, profile: AstcProfile, rec: &[u8]) -> Option<String> {
    let r = Info(rec);
    let s = ai::parse(blk, fp);
    let btype = match s.kind {
        ai::BlockKind::Illegal(_) => 0,
        ai::BlockKind::VoidHdr => 1,
        ai::BlockKind::VoidLdr => 2,
        ai::BlockKind::Normal => 3,
    };
    if btype != r.u8(0) {
        return Some(format!("block_type ours {btype} oracle {} ({:?})", r.u8(0), s.kind));
    }
    if btype == 1 || btype == 2 {
        for c in 0..4 {
            if s.void_color[c] != r.u16(18 + 2 * c) {
                return Some(format!("void colour [{c}]"));
            }
        }
        return None;
    }
    if btype == 0 {
        return None;
    }
    let mut o = 1;
    let check = |name: &str, ours: i64, theirs: i64| -> Option<String> {
        (ours != theirs).then(|| format!("{name}: ours {ours} oracle {theirs}"))
    };
    if let Some(e) = check("partitions", s.partitions as i64, r.u8(o) as i64) {
        return Some(e);
    }
    o += 1;
    let seed = if s.partitions > 1 { s.partition_seed } else { 0 };
    if let Some(e) = check("seed", seed as i64, r.u16(o) as i64) {
        return Some(e);
    }
    o += 2;
    if let Some(e) = check("block_mode", s.mode as i64, r.u16(o) as i64) {
        return Some(e);
    }
    o += 2;
    for p in 0..4 {
        let ours = if p < s.partitions as usize { s.cem[p] } else { 0 };
        if let Some(e) = check(&format!("cem[{p}]"), ours as i64, r.u8(o + p) as i64) {
            return Some(e);
        }
    }
    o += 4;
    if let Some(e) = check("color_range", s.color_range as i64, r.u8(o) as i64) {
        return Some(e);
    }
    o += 1;
    if let Some(e) = check("weight_range", s.weight_range as i64, r.u8(o) as i64) {
        return Some(e);
    }
    o += 1;
    let p2 = if s.dual_plane { s.plane2_component as i8 } else { -1 };
    if let Some(e) = check("plane2", p2 as i64, r.u8(o) as i8 as i64) {
        return Some(e);
    }
    o += 1;
    for a in 0..3 {
        if let Some(e) = check(&format!("grid[{a}]"), s.grid[a] as i64, r.u8(o + a) as i64) {
            return Some(e);
        }
    }
    o += 3;
    if s.partitions > 1 {
        if let Some(e) = check("cem_matched", s.cem_matched as i64, r.u8(o) as i64) {
            return Some(e);
        }
    }
    o += 1;
    let wc = s.plane_weight_count() * if s.dual_plane { 2 } else { 1 };
    if let Some(e) = check("weight_count", wc as i64, r.u8(o) as i64) {
        return Some(e);
    }
    o += 1 + 8;
    // colour values [4][8]
    let mut off = 0;
    for p in 0..s.partitions as usize {
        let n = ai::value_count(s.cem[p]);
        for j in 0..n {
            if let Some(e) = check(
                &format!("color[{p}][{j}]"),
                s.color_values[off + j] as i64,
                r.u8(o + p * 8 + j) as i64,
            ) {
                return Some(e);
            }
        }
        off += n;
    }
    o += 32;
    // weights: astcenc keeps plane 2 at +32.
    let pw = s.plane_weight_count();
    for i in 0..pw {
        let (w1, w2) = if s.dual_plane {
            (s.weights[2 * i], Some(s.weights[2 * i + 1]))
        } else {
            (s.weights[i], None)
        };
        if let Some(e) = check(&format!("weight[{i}]"), w1 as i64, r.u8(o + i) as i64) {
            return Some(e);
        }
        if let Some(w2) = w2 {
            if let Some(e) = check(&format!("weight2[{i}]"), w2 as i64, r.u8(o + 32 + i) as i64) {
                return Some(e);
            }
        }
    }
    o += 64;
    // endpoints
    let mut off = 0;
    let mut eps = Vec::new();
    for p in 0..s.partitions as usize {
        let n = ai::value_count(s.cem[p]);
        let raw = ai::cem_decode(s.cem[p], &s.color_values[off..off + n]);
        off += n;
        let ep = ai::cem_expand(&raw, profile);
        for c in 0..4 {
            let o0 = r.i32(o + (p * 8 + c) * 4);
            let o1 = r.i32(o + (p * 8 + 4 + c) * 4);
            if ep.e0[c] as i32 != o0 || ep.e1[c] as i32 != o1 {
                return Some(format!(
                    "endpoints p{p} cem {} lane {c}: ours ({}, {}) oracle ({o0}, {o1}) raw {:?}",
                    s.cem[p], ep.e0[c], ep.e1[c], raw
                ));
            }
        }
        let rh = r.u8(o + 128 + p) != 0;
        let ah = r.u8(o + 132 + p) != 0;
        if !matches!(profile, AstcProfile::Ldr | AstcProfile::LdrSrgb) && (rh != ep.lns[0] || ah != ep.lns[3]) {
            return Some(format!("lns flags p{p}: ours {:?} oracle rgb {rh} a {ah}", ep.lns));
        }
        eps.push(ep);
    }
    o += 136;
    // per-texel weights and partitions
    let mut g1 = [0u8; ai::GRID_PAD];
    let mut g2 = [0u8; ai::GRID_PAD];
    for i in 0..pw {
        if s.dual_plane {
            g1[i] = s.weights[2 * i];
            g2[i] = s.weights[2 * i + 1];
        } else {
            g1[i] = s.weights[i];
        }
    }
    let (gw, gh, gd) = (s.grid[0] as u32, s.grid[1] as u32, s.grid[2] as u32);
    let mut w1 = [0u8; 216];
    let mut w2 = [0u8; 216];
    ai::infill(fp, gw, gh, gd, &g1, &mut w1);
    if s.dual_plane {
        ai::infill(fp, gw, gh, gd, &g2, &mut w2);
    }
    let tc = fp.texels() as usize;
    for i in 0..tc {
        if w1[i] != r.u8(o + i) {
            return Some(format!("texel weight[{i}] ours {} oracle {} grid {gw}x{gh}x{gd}", w1[i], r.u8(o + i)));
        }
        if s.dual_plane && w2[i] != r.u8(o + 216 + i) {
            return Some(format!("texel weight2[{i}]"));
        }
    }
    let small = tc < 31;
    let mut i = 0;
    for z in 0..fp.depth() {
        for y in 0..fp.height() {
            for x in 0..fp.width() {
                let p = if s.partitions > 1 {
                    ai::select_partition(s.partition_seed as u32, x, y, z, s.partitions as u32, small)
                } else {
                    0
                };
                if p != r.u8(o + 432 + i) {
                    return Some(format!("partition of texel {i}"));
                }
                i += 1;
            }
        }
    }
    let _ = eps;
    None
}

fn cmd_info(profile: AstcProfile, fp: AstcFootprint, blocks: &[[u8; 16]], quiet: bool) -> (u64, u64) {
    let a = fp_args(fp);
    let input: Vec<u8> = blocks.iter().flatten().copied().collect();
    let o = oracle::run(&["info", prof_name(profile), &a[0], &a[1], &a[2]], &input);
    assert_eq!(o.len(), blocks.len() * INFO_SIZE);
    let mut bad = 0u64;
    for (k, b) in blocks.iter().enumerate() {
        if let Some(e) = info_diff(b, fp, profile, &o[k * INFO_SIZE..(k + 1) * INFO_SIZE]) {
            bad += 1;
            if bad <= 3 && !quiet {
                println!("  info {fp} {}: block {} {e}", prof_name(profile), hex(b));
            }
        }
    }
    (blocks.len() as u64, bad)
}

fn cmd_decode(profile: AstcProfile, f16: bool, fp: AstcFootprint, blocks: &[[u8; 16]], quiet: bool) -> (u64, u64) {
    let a = fp_args(fp);
    let input: Vec<u8> = blocks.iter().flatten().copied().collect();
    let o = oracle::run(
        &["decode", prof_name(profile), if f16 { "f16" } else { "u8" }, &a[0], &a[1], &a[2]],
        &input,
    );
    let tc = fp.texels() as usize;
    let per = tc * if f16 { 8 } else { 4 };
    assert_eq!(o.len(), blocks.len() * per);
    let mut bad = 0u64;
    let mut t8 = vec![[0u8; 4]; tc];
    let mut t16 = vec![[0u16; 4]; tc];
    for (k, b) in blocks.iter().enumerate() {
        let theirs = &o[k * per..(k + 1) * per];
        let mut ours = Vec::with_capacity(per);
        if f16 {
            astc::decode_block_f16(b, fp, profile, &mut t16);
            for t in &t16 {
                for c in t {
                    ours.extend_from_slice(&c.to_le_bytes());
                }
            }
        } else {
            astc::decode_block_rgba8(b, fp, profile, &mut t8);
            for t in &t8 {
                ours.extend_from_slice(t);
            }
        }
        if ours != theirs {
            bad += 1;
            if bad <= 3 && !quiet {
                let i = (0..per).find(|&i| ours[i] != theirs[i]).unwrap();
                let s = ai::parse(b, fp);
                println!(
                    "  decode {fp} {} {}: block {} first diff byte {i} (texel {}) ours {:02x} oracle {:02x}; kind {:?} cem {:?} parts {} dual {}",
                    prof_name(profile),
                    if f16 { "f16" } else { "u8" },
                    hex(b),
                    i / if f16 { 8 } else { 4 },
                    ours[i],
                    theirs[i],
                    s.kind,
                    &s.cem[..s.partitions.max(1) as usize],
                    s.partitions,
                    s.dual_plane
                );
            }
        }
    }
    (blocks.len() as u64, bad)
}

const CELLS: [(AstcProfile, bool); 6] = [
    (AstcProfile::LdrSrgb, false),
    (AstcProfile::Ldr, false),
    (AstcProfile::LdrSrgb, true),
    (AstcProfile::Ldr, true),
    (AstcProfile::HdrRgbLdrA, true),
    (AstcProfile::Hdr, true),
];

fn cmd_sweep(src: &str, fps: &[AstcFootprint]) -> bool {
    let mut all_ok = true;
    for &fp in fps {
        let blocks = gen::source(src, fp);
        let mut line = format!("{fp:>7} n={:<8}", blocks.len());
        for prof in [AstcProfile::Ldr, AstcProfile::Hdr] {
            let (_, bad) = cmd_info(prof, fp, &blocks, false);
            line += &format!(" info[{}]={bad}", prof_name(prof));
            all_ok &= bad == 0;
        }
        for (prof, f16) in CELLS {
            let (_, bad) = cmd_decode(prof, f16, fp, &blocks, false);
            line += &format!(" {}x{}={bad}", prof_name(prof), if f16 { "f16" } else { "u8" });
            all_ok &= bad == 0;
        }
        println!("{line}");
    }
    all_ok
}

/// Run the six cells on `blocks` (oracle and ours, cells in parallel);
/// returns per-block pass flags and per-cell differing-block counts.
fn cells_check(fp: AstcFootprint, blocks: &[[u8; 16]]) -> (Vec<bool>, [u64; 6]) {
    let a = fp_args(fp);
    let input: Vec<u8> = blocks.iter().flatten().copied().collect();
    let tc = fp.texels() as usize;
    let results: Vec<Vec<bool>> = std::thread::scope(|sc| {
        let hs: Vec<_> = CELLS
            .iter()
            .map(|&(prof, f16)| {
                let input = &input;
                let a = &a;
                sc.spawn(move || {
                    let o = oracle::run(
                        &["decode", prof_name(prof), if f16 { "f16" } else { "u8" }, &a[0], &a[1], &a[2]],
                        input,
                    );
                    let per = tc * if f16 { 8 } else { 4 };
                    assert_eq!(o.len(), blocks.len() * per);
                    let mut t8 = vec![[0u8; 4]; tc];
                    let mut t16 = vec![[0u16; 4]; tc];
                    let mut ours = Vec::with_capacity(per);
                    blocks
                        .iter()
                        .enumerate()
                        .map(|(k, b)| {
                            ours.clear();
                            if f16 {
                                astc::decode_block_f16(b, fp, prof, &mut t16);
                                for t in &t16 {
                                    for c in t {
                                        ours.extend_from_slice(&c.to_le_bytes());
                                    }
                                }
                            } else {
                                astc::decode_block_rgba8(b, fp, prof, &mut t8);
                                for t in &t8 {
                                    ours.extend_from_slice(t);
                                }
                            }
                            ours[..] == o[k * per..(k + 1) * per]
                        })
                        .collect()
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut pass = vec![true; blocks.len()];
    let mut bad = [0u64; 6];
    for (c, r) in results.iter().enumerate() {
        for (k, &ok) in r.iter().enumerate() {
            if !ok {
                bad[c] += 1;
                if pass[k] {
                    println!("  DIFF {fp} cell {c}: block {}", hex(&blocks[k]));
                }
                pass[k] = false;
            }
        }
    }
    (pass, bad)
}

fn corpus_pool(dir: &str) -> std::collections::BTreeMap<AstcFootprint, Vec<[u8; 16]>> {
    let mut pool: std::collections::BTreeMap<AstcFootprint, Vec<[u8; 16]>> = Default::default();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    for p in files {
        if p.extension().and_then(|e| e.to_str()) != Some("astc") {
            continue;
        }
        let d = std::fs::read(&p).unwrap();
        if d.len() < 16 || d[..4] != [0x13, 0xAB, 0xA1, 0x5C] {
            continue;
        }
        let Some(fp) = AstcFootprint::new_3d(d[4] as u32, d[5] as u32, d[6] as u32) else {
            continue;
        };
        let v = pool.entry(fp).or_default();
        for c in d[16..].chunks_exact(16) {
            let mut b = [0u8; 16];
            b.copy_from_slice(c);
            v.push(b);
        }
    }
    pool
}

/// DC1 (natural corpus) + DC2 (random, mutated, mixed) + census, chunked.
fn cmd_dc2(n: usize, corpus: &str, fps: &[AstcFootprint], report: &str) -> bool {
    let pool = corpus_pool(corpus);
    let mut cen = census::Census::default();
    let mut all_ok = true;
    let chunk = 16384usize;
    let mut grand = 0u64;
    for &fp in fps {
        let mut rng = gen::Rng::new(0xDC2 ^ ((fp.texels() as u64) << 20) ^ ((fp.width() as u64) << 8) ^ fp.depth() as u64);
        let lm = gen::legal_modes(fp);
        let natural = pool.get(&fp).cloned().unwrap_or_default();
        let mut line = format!("{fp:>7}");
        let mut nat = natural.clone();
        nat.sort_unstable();
        nat.dedup();
        let mut tot_bad = [0u64; 6];
        for ch in nat.chunks(chunk) {
            let (pass, bad) = cells_check(fp, ch);
            for c in 0..6 {
                tot_bad[c] += bad[c];
            }
            for (b, ok) in ch.iter().zip(pass) {
                if ok {
                    cen.add(b, fp);
                }
            }
        }
        line += &format!(" natural={} bad={:?}", nat.len(), tot_bad);
        all_ok &= tot_bad.iter().all(|&x| x == 0);
        grand += nat.len() as u64;
        for kind in ["random", "mutated", "mix"] {
            if kind == "mutated" && natural.is_empty() {
                line += " mutated=skipped(no corpus)";
                continue;
            }
            let n = if kind == "mix" { (n / 10).max(1) } else { n };
            let mut done = 0usize;
            let mut tot_bad = [0u64; 6];
            while done < n {
                let m = chunk.min(n - done);
                let blocks: Vec<[u8; 16]> = (0..m)
                    .map(|_| match kind {
                        "random" => rng.block(),
                        "mutated" => {
                            let mut b = natural[rng.below(natural.len() as u64) as usize];
                            for _ in 0..1 + rng.below(8) {
                                let bit = rng.below(128) as usize;
                                b[bit / 8] ^= 1 << (bit % 8);
                            }
                            b
                        }
                        _ => gen::mixed(&mut rng, fp, &lm),
                    })
                    .collect();
                let (pass, bad) = cells_check(fp, &blocks);
                for c in 0..6 {
                    tot_bad[c] += bad[c];
                }
                for (b, ok) in blocks.iter().zip(pass) {
                    if ok {
                        cen.add(b, fp);
                    }
                }
                done += m;
            }
            grand += n as u64;
            line += &format!(" {kind}={n} bad={:?}", tot_bad);
            all_ok &= tot_bad.iter().all(|&x| x == 0);
        }
        println!("{line}");
    }
    let mut md = String::new();
    let (cells, full) = cen.report(32, &mut md);
    let md = format!(
        "Blocks checked: {grand} (each in all six cells). Census cells: {cells}, with >= 32 exact blocks: {full}.\n\n{md}"
    );
    std::fs::write(report, &md).unwrap();
    println!("census: {full} / {cells} cells at >= 32 exact blocks; report {report}");
    all_ok
}

/// Write the golden-vector fixture from the ORACLE's output (never ours):
/// per footprint (ALL_2D then ALL_3D), per 16-block chunk, per cell, FNV-1a 64
/// of astcenc's output bytes. Also reports whether our decoder agrees.
fn cmd_golden(path: &str) -> bool {
    use ai::testgen;
    let mut out = b"ASTCGLD1".to_vec();
    let mut ok = true;
    for fp in all_fps() {
        let blocks = testgen::golden_blocks(fp);
        let a = fp_args(fp);
        let input: Vec<u8> = blocks.iter().flatten().copied().collect();
        let tc = fp.texels() as usize;
        let outs: Vec<Vec<u8>> = CELLS
            .iter()
            .map(|&(prof, f16)| {
                oracle::run(&["decode", prof_name(prof), if f16 { "f16" } else { "u8" }, &a[0], &a[1], &a[2]], &input)
            })
            .collect();
        out.extend_from_slice(&(fp.width() << 16 | fp.height() << 8 | fp.depth()).to_le_bytes());
        let chunks = blocks.len() / testgen::GOLDEN_CHUNK;
        out.extend_from_slice(&(chunks as u32).to_le_bytes());
        for ci in 0..chunks {
            for (cell, &(_, f16)) in CELLS.iter().enumerate() {
                let per = tc * if f16 { 8 } else { 4 };
                let lo = ci * testgen::GOLDEN_CHUNK * per;
                let hi = lo + testgen::GOLDEN_CHUNK * per;
                let h = testgen::fnv1a(testgen::FNV_OFFSET, &outs[cell][lo..hi]);
                out.extend_from_slice(&h.to_le_bytes());
            }
        }
        let (_, bad) = cells_check(fp, &blocks);
        ok &= bad.iter().all(|&b| b == 0);
        println!("{fp}: {} blocks, ours vs oracle bad per cell {bad:?}", blocks.len());
    }
    std::fs::write(path, &out).unwrap();
    println!("wrote {path}: {} bytes", out.len());
    ok
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ok = match args.get(1).map(|s| s.as_str()) {
        Some("modes") => cmd_modes(),
        Some("partitions") => cmd_partitions(),
        Some("ise") => cmd_ise(args.get(2).map(|s| s.parse().unwrap()).unwrap_or(2000)),
        Some("info") => {
            let prof = prof_parse(&args[2]);
            let fp = fp_parse(&args[3]);
            let blocks = gen::source(&args[4], fp);
            let (n, bad) = cmd_info(prof, fp, &blocks, false);
            println!("info {fp} {}: {bad} / {n} differ", args[2]);
            bad == 0
        }
        Some("decode") => {
            let prof = prof_parse(&args[2]);
            let f16 = args[3] == "f16";
            let fp = fp_parse(&args[4]);
            let blocks = gen::source(&args[5], fp);
            let (n, bad) = cmd_decode(prof, f16, fp, &blocks, false);
            println!("decode {fp} {} {}: {bad} / {n} differ", args[2], args[3]);
            bad == 0
        }
        Some("golden") => cmd_golden(&args[2]),
        Some("dc2") => {
            let n: usize = args[2].parse().unwrap();
            let fps: Vec<AstcFootprint> = match args.get(5) {
                Some(f) if f != "all" => f.split(',').map(fp_parse).collect(),
                _ => all_fps(),
            };
            cmd_dc2(n, &args[3], &fps, &args[4])
        }
        Some("sweep") => {
            let fps: Vec<AstcFootprint> = match args.get(3) {
                Some(f) => f.split(',').map(fp_parse).collect(),
                None => all_fps(),
            };
            cmd_sweep(&args[2], &fps)
        }
        _ => {
            eprintln!("usage: see the header of tools/astc-harness/src/main.rs");
            false
        }
    };
    std::process::exit(if ok { 0 } else { 1 });
}
