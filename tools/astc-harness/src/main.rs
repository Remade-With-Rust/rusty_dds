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

#[global_allocator]
static GLOBAL_ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;

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

fn astcenc_bin() -> String {
    std::env::var("ASTCENC").unwrap_or_else(|_| "/root/astcws/rel571/inst/bin/astcenc-none".into())
}

fn read_png_rgba(path: &str) -> (u32, u32, Vec<u8>) {
    let dec = png::Decoder::new(std::fs::File::open(path).unwrap_or_else(|e| panic!("{path}: {e}")));
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let px = (info.width * info.height) as usize;
    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf[..px * 4].to_vec(),
        (png::ColorType::Rgb, png::BitDepth::Eight) => buf[..px * 3].chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        other => panic!("{path}: png {other:?}"),
    };
    (info.width, info.height, rgba)
}

fn run_astcenc(args: &[&str]) -> bool {
    std::process::Command::new(astcenc_bin())
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Decode `container` with astcenc (`-dl` or `-ds` to PNG) and compare with
/// our 8-bit decode of `surf`.
fn astcenc_agrees(container: &str, surf: &astc::AstcSurface, srgb: bool, tmp: &str) -> Result<(), String> {
    let png = format!("{tmp}/cmp.png");
    let _ = std::fs::remove_file(&png);
    if !run_astcenc(&[if srgb { "-ds" } else { "-dl" }, container, &png, "-silent"]) {
        return Err(format!("astcenc could not decode {container}"));
    }
    let (w, h, theirs) = read_png_rgba(&png);
    if (w, h) != (surf.width, surf.height) {
        return Err(format!("size {w}x{h} vs ours {}x{}", surf.width, surf.height));
    }
    let prof = if srgb { AstcProfile::LdrSrgb } else { AstcProfile::Ldr };
    let ours = surf.decode_rgba8(prof).map_err(|e| e.to_string())?;
    let ours_slice0 = &ours.pixels[..(w * h * 4) as usize];
    if ours_slice0 != &theirs[..] {
        let flipped: Vec<u8> = theirs.chunks_exact((w * 4) as usize).rev().flatten().copied().collect();
        if ours_slice0 == &flipped[..] {
            return Err("matches only after a vertical flip".into());
        }
        let n = ours_slice0.chunks_exact(4).zip(theirs.chunks_exact(4)).filter(|(a, b)| a != b).count();
        return Err(format!("{n} pixels differ"));
    }
    Ok(())
}

fn cmd_containers(tmp: &str) -> bool {
    let mut ok = true;
    let mut report = |name: &str, r: Result<(), String>| {
        match &r {
            Ok(()) => println!("  ok   {name}"),
            Err(e) => {
                println!("  FAIL {name}: {e}");
                ok = false;
            }
        }
    };
    std::fs::create_dir_all(tmp).unwrap();
    // C1: astcenc's negative .astc files must be errors, not panics.
    let neg = "/root/astcws/rel571/Test/Data";
    for f in ["negative_block_size", "negative_huge", "negative_magic", "negative_overflow", "negative_short"] {
        let p = format!("{neg}/{f}.astc");
        let d = std::fs::read(&p).unwrap();
        report(&format!("C1 {f}.astc rejected"), match astc::AstcFile::parse(&d) {
            Err(_) => Ok(()),
            Ok(_) => Err("parsed".into()),
        });
    }
    // C1: astcenc's own .astc decodes the same through our container, and
    // astcenc reads the .astc we write.
    for f in ["/root/astcws/corpus/s_ldr-rgba-00_6x5_cl_thorough.astc", "/root/astcws/corpus/k_ldr-rgba-base_12x12_cs_medium.astc", "/root/astcws/rel571/Test/Data/LDR-A-1x1.astc"] {
        let d = std::fs::read(f).unwrap();
        let file = astc::AstcFile::parse(&d).unwrap();
        let srgb = f.contains("_cs_");
        report(&format!("C1 read {f}"), astcenc_agrees(f, &file.surface(), srgb, tmp));
        let mut out = Vec::new();
        astc::AstcFile::write(&mut out, file.footprint, file.width, file.height, file.depth, file.data).unwrap();
        let ours = format!("{tmp}/ours.astc");
        std::fs::write(&ours, &out).unwrap();
        report(&format!("C1 write {f}"), if out == d { astcenc_agrees(&ours, &file.surface(), srgb, tmp) } else { Err("bytes differ from input".into()) });
        // Truncated payload: TruncatedData.
        report("C1 truncated", match astc::AstcFile::parse(&d[..d.len() - 1]) {
            Err(rusty_dds::Error::TruncatedData) => Ok(()),
            other => Err(format!("{other:?}")),
        });
    }
    // C2: KTX 1.0 written by astcenc, read by us; written by us, read by astcenc.
    let src_png = "/root/astcws/astc-encoder/Test/Images/Small/LDR-RGBA/ldr-rgba-01.png";
    for (fp, mode) in [("6x5", "-cl"), ("10x8", "-cs"), ("4x4", "-cl")] {
        let k = format!("{tmp}/astcenc_{fp}.ktx");
        let _ = std::fs::remove_file(&k);
        assert!(run_astcenc(&[mode, src_png, &k, fp, "-fast", "-silent"]), "astcenc -c to ktx");
        let d = std::fs::read(&k).unwrap();
        let srgb = mode == "-cs";
        match astc::KtxView::parse(&d) {
            Err(e) => report(&format!("C2 read astcenc {fp}.ktx"), Err(e.to_string())),
            Ok(v) => {
                let s = v.astc_surface(0, 0, 0).unwrap();
                report(&format!("C2 read astcenc {fp}.ktx (srgb tag {})", v.srgb), if v.srgb != srgb { Err("sRGB tag".into()) } else { astcenc_agrees(&k, &s, srgb, tmp) });
                let mut out = Vec::new();
                astc::KtxView::write(&mut out, s.footprint, s.width, s.height, s.depth, srgb, &[s.data]).unwrap();
                let ours = format!("{tmp}/ours_{fp}.ktx");
                std::fs::write(&ours, &out).unwrap();
                let back = astc::KtxView::parse(&out).unwrap();
                report(&format!("C2 write {fp}.ktx, astcenc reads it"), if back.astc_surface(0, 0, 0).unwrap().data != s.data { Err("round trip".into()) } else { astcenc_agrees(&ours, &s, srgb, tmp) });
            }
        }
    }
    // C2/C4a: KTX-Software files. The .ktx and .ktx2 versions of the same
    // array texture must give the same payload per layer.
    let ks = "/root/astcws/thirdparty/KTX-Software/tests/resources";
    let k1d = std::fs::read(format!("{ks}/ktx/astc_8x8_unorm_array_7.ktx")).unwrap();
    let k2d = std::fs::read(format!("{ks}/ktx2/astc_8x8_unorm_array_7.ktx2")).unwrap();
    match (astc::KtxView::parse(&k1d), astc::Ktx2View::parse(&k2d)) {
        (Ok(a), Ok(b)) => {
            println!("    ktx1: {} {}x{}x{} layers {} faces {} levels {}; ktx2: {} {}x{} layers {} levels {}", a.footprint, a.width, a.height, a.depth, a.layers, a.faces, a.levels, b.footprint, b.width, b.height, b.layers, b.levels);
            let mut same = a.layers == b.layers && a.levels == b.levels;
            for l in 0..a.levels.min(b.levels) {
                for y in 0..a.layers.min(b.layers) {
                    same &= a.astc_surface(l, y, 0).unwrap().data == b.astc_surface(l, y, 0).unwrap().data;
                }
            }
            report("C2/C4a astc_8x8_unorm_array_7: .ktx and .ktx2 payloads equal per level and layer", if same { Ok(()) } else { Err("differ".into()) });
            let path = format!("{ks}/ktx/astc_8x8_unorm_array_7.ktx");
            report("C2 astc_8x8_unorm_array_7.ktx layer 0 vs astcenc", astcenc_agrees(&path, &a.astc_surface(0, 0, 0).unwrap(), a.srgb, tmp));
        }
        (a, b) => report("C2/C4a array_7", Err(format!("{:?} / {:?}", a.err(), b.err()))),
    }
    for f in ["ktx_app_astc_8x8.ktx2", "r8g8b8a8_srgb_mip_astc.ktx2", "Desk_astc_hdr6x6.ktx2"] {
        let raw = std::fs::read(format!("{ks}/ktx2/{f}")).unwrap();
        let scheme = u32::from_le_bytes(raw[44..48].try_into().unwrap());
        let d = match astc::ktx::ktx2_inflate(&raw) {
            Ok(v) => v,
            Err(e) => {
                report(&format!("C4a {f} inflate (scheme {scheme})"), Err(e.to_string()));
                continue;
            }
        };
        if scheme != 0 {
            println!("    {f}: supercompression scheme {scheme}, inflated {} -> {} bytes", raw.len(), d.len());
        }
        match astc::Ktx2View::parse(&d) {
            Err(e) => println!("  note {f}: {e} (scheme {})", u32::from_le_bytes(d[44..48].try_into().unwrap())),
            Ok(v) => {
                println!("    {f}: {} {}x{} srgb {} hdr {} layers {} faces {} levels {}", v.footprint, v.width, v.height, v.srgb, v.hdr, v.layers, v.faces, v.levels);
                // Re-wrap level 0 as KTX 1.0 and have astcenc decode it.
                let s = v.astc_surface(0, 0, 0).unwrap();
                let mut out = Vec::new();
                astc::KtxView::write(&mut out, s.footprint, s.width, s.height, s.depth, v.srgb, &[s.data]).unwrap();
                let p = format!("{tmp}/rewrap.ktx");
                std::fs::write(&p, &out).unwrap();
                if v.hdr {
                    // HDR: compare our f32 decode with astcenc's float16 via the oracle path instead.
                    let blocks: Vec<[u8; 16]> = s.data.chunks_exact(16).map(|c| c.try_into().unwrap()).collect();
                    let (_, bad) = cells_check(s.footprint, &blocks[..blocks.len().min(65536)]);
                    report(&format!("C4a {f} payload decodes identically (six cells)"), if bad.iter().all(|&b| b == 0) { Ok(()) } else { Err(format!("{bad:?}")) });
                } else {
                    report(&format!("C4a {f} level 0 vs astcenc"), astcenc_agrees(&p, &s, v.srgb, tmp));
                }
            }
        }
    }
    ok
}

/// C3: ASTC inside DDS. image-dds's 14 files decode like astcenc decoding
/// their payload; DDS files we write are read by the `dds` crate (which
/// decodes ASTC through astc-decode 0.3.1, so its pixels are compared with
/// astc-decode on the same payload, not with ours).
fn cmd_dds(tmp: &str) -> bool {
    use rusty_dds::{AlphaMode, D3D10ResourceDimension, Dds, DdsView, DxgiFormat, NewDxgiParams, SubresourceId};
    let mut ok = true;
    let mut report = |name: &str, r: Result<(), String>| match &r {
        Ok(()) => println!("  ok   {name}"),
        Err(e) => {
            println!("  FAIL {name}: {e}");
            ok = false;
        }
    };
    std::fs::create_dir_all(tmp).unwrap();
    let dir = "/root/astcws/thirdparty/image-dds/test-data/images/astc";
    let mut files: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    files.sort();
    for p in files {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&p).unwrap();
        let r = (|| -> Result<(), String> {
            let dds = DdsView::parse(&bytes).map_err(|e| format!("parse: {e}"))?;
            let c = dds.astc_content().map_err(|e| e.to_string())?;
            let want = name.trim_start_matches("ASTC ").trim_end_matches(".dds");
            if c.footprint.to_string() != want {
                return Err(format!("footprint {} vs name {want}", c.footprint));
            }
            let s = dds.astc_surface(SubresourceId::new(0, 0, 0)).map_err(|e| e.to_string())?;
            if s.data.len() != astc::payload_len(s.width, s.height, 1, s.footprint).unwrap() {
                return Err("payload length".into());
            }
            // The exact mip chain accounts for every byte of the file's payload.
            let stride = dds.get_array_stride().map_err(|e| e.to_string())? as usize;
            if stride * dds.get_num_array_layers() as usize != dds.data.len() {
                return Err(format!("array stride {stride} vs payload {}", dds.data.len()));
            }
            // Write the payload as .astc and let astcenc decode it.
            let mut f = Vec::new();
            astc::AstcFile::write(&mut f, s.footprint, s.width, s.height, 1, s.data).unwrap();
            let path = format!("{tmp}/dds_payload.astc");
            std::fs::write(&path, &f).unwrap();
            astcenc_agrees(&path, &s, c.srgb, tmp)?;
            // The DDS entry point gives the same pixels as the surface decode.
            let img = dds.decode_rgba8(SubresourceId::new(0, 0, 0)).map_err(|e| e.to_string())?;
            let direct = s.decode_rgba8(if c.srgb { AstcProfile::LdrSrgb } else { AstcProfile::Ldr }).unwrap();
            if img.pixels != direct.pixels {
                return Err("decode_rgba8 differs from the surface decode".into());
            }
            println!("    {name}: {} {}x{} srgb {} mips {}", c.footprint, s.width, s.height, c.srgb, dds.get_num_mipmap_levels());
            Ok(())
        })();
        report(&format!("C3 image-dds {name}"), r);
    }
    // Our DDS-ASTC files, read by the dds crate.
    for (w, h, fw, fh, mips) in [(64u32, 48u32, 6u32, 6u32, 1u32), (37, 29, 8, 5, 3), (100, 100, 12, 12, 1)] {
        let fmt = DxgiFormat::from_astc(fw, fh, false).unwrap();
        let mut d = Dds::new_dxgi(NewDxgiParams {
            height: h,
            width: w,
            depth: None,
            format: fmt,
            mipmap_levels: Some(mips),
            array_layers: None,
            caps2: None,
            is_cubemap: false,
            resource_dimension: D3D10ResourceDimension::Texture2D,
            alpha_mode: AlphaMode::Straight,
        })
        .unwrap();
        let fp = AstcFootprint::new_2d(fw, fh).unwrap();
        let lm = gen::legal_modes(fp);
        let mut rng = gen::Rng::new(w as u64 * 31 + h as u64);
        let mut legal_blocks = |n: usize| -> Vec<u8> {
            // Encoder-like content: legal blocks only (astc-decode panics or
            // errors on some illegal encodings, which is not under test here).
            let mut v = Vec::new();
            while v.len() < n * 16 {
                let b = gen::mixed(&mut rng, fp, &lm);
                if ai::parse(&b, fp).kind == ai::BlockKind::Normal {
                    let cems_ldr = ai::parse(&b, fp).cem.iter().all(|c| !matches!(c, 2 | 3 | 7 | 11 | 14 | 15));
                    if cems_ldr {
                        v.extend_from_slice(&b);
                    }
                }
            }
            v
        };
        let total = d.data.len();
        let payload = legal_blocks(total / 16);
        d.data.copy_from_slice(&payload);
        let mut file = Vec::new();
        d.write(&mut file).unwrap();
        let r = (|| -> Result<(), String> {
            let mut dec = dds::Decoder::new(std::io::Cursor::new(&file)).map_err(|e| format!("dds crate: {e:?}"))?;
            let mut img = vec![0u8; (w * h * 4) as usize];
            dec.read_surface(dds::ImageViewMut::new(&mut img[..], dds::Size::new(w, h), dds::ColorFormat::RGBA_U8).unwrap())
                .map_err(|e| format!("dds crate decode: {e:?}"))?;
            // astc-decode on the same level-0 payload.
            let ours = DdsView::parse(&file).unwrap();
            let s = ours.astc_surface(SubresourceId::new(0, 0, 0)).unwrap();
            let mut ad = vec![0u8; (w * h * 4) as usize];
            astc_decode::astc_decode(s.data, w, h, astc_decode::Footprint::new(fw, fh), |x, y, c| {
                let i = ((y * w + x) * 4) as usize;
                ad[i..i + 4].copy_from_slice(&c);
            })
            .map_err(|e| format!("astc-decode: {e:?}"))?;
            if img != ad {
                return Err("dds crate pixels differ from astc-decode on our payload".into());
            }
            // Every mip level's range is exact.
            for m in 0..mips {
                let s = ours.astc_surface(SubresourceId::new(m, 0, 0)).unwrap();
                if s.data.len() != astc::payload_len(s.width, s.height, 1, fp).unwrap() {
                    return Err(format!("mip {m} length"));
                }
            }
            Ok(())
        })();
        report(&format!("C3 our {fw}x{fh} DDS {w}x{h} mips {mips} read by dds 0.2"), r);
    }
    ok
}

fn load_rgba(path: &str) -> (u32, u32, Vec<u8>) {
    let dec = png::Decoder::new(std::fs::File::open(path).unwrap_or_else(|e| panic!("{path}: {e}")));
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let px = (info.width * info.height) as usize;
    let rgba: Vec<u8> = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf[..px * 4].to_vec(),
        (png::ColorType::Rgb, png::BitDepth::Eight) => buf[..px * 3].chunks_exact(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        (png::ColorType::Grayscale, png::BitDepth::Eight) => buf[..px].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => buf[..px * 2].chunks_exact(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        other => panic!("{path}: png {other:?}"),
    };
    (info.width, info.height, rgba)
}

/// A 2048x2048 natural-content test texture: source images tiled row-major
/// (each cropped to its tile), for the claim-grade decode bench (section 2.4
/// asks for 2048x2048 inputs; the Kodak set is 768x512).
///   astch mosaic <out.png> <in.png>...
fn cmd_mosaic(args: &[String]) -> bool {
    const N: usize = 2048;
    let srcs: Vec<(u32, u32, Vec<u8>)> = args[1..].iter().map(|p| load_rgba(p)).collect();
    let mut out = vec![0u8; N * N * 4];
    let (tw, th) = (srcs[0].0 as usize, srcs[0].1 as usize);
    let cols = N.div_ceil(tw);
    for y in 0..N {
        for x in 0..N {
            let (s, sx, sy) = (&srcs[((y / th) * cols + x / tw) % srcs.len()], x % tw, y % th);
            let (sw, sh) = (s.0 as usize, s.1 as usize);
            let i = ((sy % sh) * sw + (sx % sw)) * 4;
            out[(y * N + x) * 4..(y * N + x) * 4 + 4].copy_from_slice(&s.2[i..i + 4]);
        }
    }
    let f = std::fs::File::create(&args[0]).unwrap();
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), N as u32, N as u32);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&out).unwrap();
    true
}

/// Block-mode usage of our encoder: for every 2D footprint, encode each image
/// at `preset` and count the normal blocks per raw block mode. Prints
/// `fp mode count` lines (input to the mode-shortlist generator).
///   astch modehist <preset> <png>...
fn cmd_modehist(args: &[String]) -> bool {
    let preset = match args[0].as_str() {
        "fastest" => astc::AstcPreset::Fastest,
        "fast" => astc::AstcPreset::Fast,
        "medium" => astc::AstcPreset::Medium,
        "thorough" => astc::AstcPreset::Thorough,
        _ => astc::AstcPreset::Exhaustive,
    };
    let imgs: Vec<(u32, u32, Vec<u8>)> = args[1..].iter().map(|p| load_rgba(p)).collect();
    for &fp in AstcFootprint::ALL_2D {
        let mut hist = std::collections::BTreeMap::<u16, u64>::new();
        let opts = astc::AstcEncodeOptions::new(fp, AstcProfile::Ldr).with_preset(preset);
        for (w, h, img) in &imgs {
            let data = astc::encode_rgba8(img, *w, *h, &opts).unwrap();
            for b in data.chunks_exact(16) {
                let s = ai::parse(b.try_into().unwrap(), fp);
                if matches!(s.kind, ai::BlockKind::Normal) {
                    *hist.entry(s.mode).or_default() += 1;
                }
            }
        }
        for (m, c) in hist {
            println!("{fp} {m} {c}");
        }
        eprintln!("{fp} done");
    }
    true
}

/// PSNR over RGB for opaque sources, over RGBA otherwise.
fn psnr_img(src: &[u8], dec: &[u8]) -> f64 {
    let opaque = src.chunks_exact(4).all(|p| p[3] == 255);
    let chans = if opaque { 3 } else { 4 };
    let mut se = 0f64;
    let mut n = 0f64;
    for (a, b) in src.chunks_exact(4).zip(dec.chunks_exact(4)) {
        for c in 0..chans {
            let d = a[c] as f64 - b[c] as f64;
            se += d * d;
            n += 1.0;
        }
    }
    if se == 0.0 {
        return 99.0;
    }
    10.0 * (255.0f64 * 255.0 * n / se).log10()
}

/// Quality: ours vs astcenc 5.7.1 at a preset, per image and footprint, on
/// pixels decoded by our decoder (bit-exact with astcenc's), plus EC1 on
/// every block we emit (astcenc decodes it identically in all six cells).
///   astch quality <preset> <fps> <profile ldr|srgb> "<astcenc extra args>" <png>...
fn cmd_quality(args: &[String]) -> bool {
    let preset = match args[0].as_str() {
        "fastest" => astc::AstcPreset::Fastest,
        "fast" => astc::AstcPreset::Fast,
        "medium" => astc::AstcPreset::Medium,
        "thorough" => astc::AstcPreset::Thorough,
        _ => astc::AstcPreset::Exhaustive,
    };
    let fps: Vec<AstcFootprint> = args[1].split(',').map(fp_parse).collect();
    let srgb = args[2] == "srgb";
    let prof = if srgb { AstcProfile::LdrSrgb } else { AstcProfile::Ldr };
    let extra: Vec<&str> = args[3].split_whitespace().collect();
    let tmp = "/root/astcws/out/qtmp";
    std::fs::create_dir_all(tmp).unwrap();
    let mut ok = true;
    let mut sums: std::collections::BTreeMap<AstcFootprint, (f64, f64, f64, usize, f64, f64)> = Default::default();
    println!("| image | footprint | ours dB | astcenc dB | gap dB | ours MT/s | EC1 bad |");
    println!("|---|---|---|---|---|---|---|");
    for path in &args[4..] {
        let (w, h, img) = load_rgba(path);
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().into_owned();
        for &fp in &fps {
            let opts = astc::AstcEncodeOptions::new(fp, prof).with_preset(preset);
            let t = std::time::Instant::now();
            let ours = astc::encode_rgba8(&img, w, h, &opts).unwrap();
            let secs = t.elapsed().as_secs_f64();
            let mut dec = vec![0u8; img.len()];
            astc::decode_rgba8_into(&ours, w, h, 1, fp, prof, &mut dec).unwrap();
            let p_ours = psnr_img(&img, &dec);
            // EC1: every emitted block, all six cells against the oracle.
            let blocks: Vec<[u8; 16]> = ours.chunks_exact(16).map(|c| c.try_into().unwrap()).collect();
            let (_, bad) = cells_check(fp, &blocks);
            let ec1: u64 = bad.iter().sum();
            let illegal = blocks.iter().filter(|b| matches!(ai::parse(b, fp).kind, ai::BlockKind::Illegal(_))).count();
            // astcenc on the same source.
            let out = format!("{tmp}/{name}_{fp}.astc");
            let _ = std::fs::remove_file(&out);
            let fps_s = fp.to_string();
            let mut a: Vec<&str> = vec![if srgb { "-cs" } else { "-cl" }, path, &out, &fps_s];
            let pre = format!("-{}", args[0]);
            a.push(&pre);
            a.extend(["-j", "8", "-silent"]);
            a.extend(extra.iter().copied());
            let ok_enc = run_astcenc(&a);
            let p_ref = if ok_enc {
                let d = std::fs::read(&out).unwrap();
                let f = astc::AstcFile::parse(&d).unwrap();
                let mut dec2 = vec![0u8; img.len()];
                astc::decode_rgba8_into(f.data, w, h, 1, fp, prof, &mut dec2).unwrap();
                psnr_img(&img, &dec2)
            } else {
                f64::NAN
            };
            let mts = (w * h) as f64 / secs / 1e6;
            println!(
                "| {name} | {fp} | {p_ours:.3} | {p_ref:.3} | {:+.3} | {mts:.2} | {ec1}{} |",
                p_ours - p_ref,
                if illegal > 0 { format!(" ({illegal} illegal)") } else { String::new() }
            );
            ok &= ec1 == 0 && illegal == 0;
            let e = sums.entry(fp).or_insert((0.0, 0.0, 0.0, 0, f64::MAX, 0.0));
            e.0 += p_ours;
            e.1 += p_ref;
            e.2 += mts;
            e.3 += 1;
            e.4 = e.4.min(p_ours - p_ref);
            e.5 += secs;
        }
    }
    println!();
    println!("| footprint | images | ours mean dB | astcenc mean dB | mean gap | worst gap | ours MT/s (mean) |");
    println!("|---|---|---|---|---|---|---|");
    for (fp, (a, b, m, n, worst, _)) in &sums {
        let n = *n as f64;
        println!("| {fp} | {} | {:.3} | {:.3} | {:+.3} | {:+.3} | {:.2} |", n as usize, a / n, b / n, (a - b) / n, worst, m / n);
    }
    ok
}

/// Decision parity: for the worst blocks of our encode, print our choice and
/// astcenc's (from its .astc of the same image), with per-block SSE.
///   astch blockdbg <png> <fp> <preset> <astcenc .astc> [n]
fn cmd_blockdbg(args: &[String]) -> bool {
    let (w, h, img) = load_rgba(&args[0]);
    let fp = fp_parse(&args[1]);
    let preset = match args[2].as_str() {
        "fastest" => astc::AstcPreset::Fastest,
        "fast" => astc::AstcPreset::Fast,
        _ => astc::AstcPreset::Medium,
    };
    let theirs = std::fs::read(&args[3]).unwrap();
    let theirs = astc::AstcFile::parse(&theirs).unwrap().data.to_vec();
    let n: usize = args.get(4).map(|s| s.parse().unwrap()).unwrap_or(8);
    let opts = astc::AstcEncodeOptions::new(fp, AstcProfile::Ldr).with_preset(preset);
    let ours = astc::encode_rgba8(&img, w, h, &opts).unwrap();
    let (nbx, nby, _) = fp.block_counts(w, h, 1);
    let tc = fp.texels() as usize;
    let mut rows = Vec::new();
    let mut t8 = vec![[0u8; 4]; tc];
    for by in 0..nby {
        for bx in 0..nbx {
            let k = (by * nbx + bx) as usize;
            let mut se = [0f64; 2];
            for (j, data) in [&ours, &theirs].iter().enumerate() {
                let b: [u8; 16] = data[k * 16..k * 16 + 16].try_into().unwrap();
                astc::decode_block_rgba8(&b, fp, AstcProfile::Ldr, &mut t8);
                for ty in 0..fp.height() {
                    for tx in 0..fp.width() {
                        let x = (bx * fp.width() + tx).min(w - 1);
                        let y = (by * fp.height() + ty).min(h - 1);
                        let s = ((y * w + x) * 4) as usize;
                        let p = t8[(ty * fp.width() + tx) as usize];
                        for c in 0..3 {
                            let d = p[c] as f64 - img[s + c] as f64;
                            se[j] += d * d;
                        }
                    }
                }
            }
            rows.push((se[0] - se[1], k, se));
        }
    }
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let desc = |b: &[u8; 16]| -> String {
        let s = ai::parse(b, fp);
        format!(
            "{:?} grid {}x{} wr {} cr {} parts {} cem {:?} dual {} p2 {}",
            s.kind, s.grid[0], s.grid[1], s.weight_range, s.color_range, s.partitions,
            &s.cem[..s.partitions.max(1) as usize], s.dual_plane, s.plane2_component
        )
    };
    let tot: (f64, f64) = rows.iter().fold((0.0, 0.0), |a, r| (a.0 + r.2[0], a.1 + r.2[1]));
    println!("total SSE ours {:.0} astcenc {:.0}", tot.0, tot.1);
    for r in rows.iter().take(n) {
        let k = r.1;
        let ob: [u8; 16] = ours[k * 16..k * 16 + 16].try_into().unwrap();
        let tb: [u8; 16] = theirs[k * 16..k * 16 + 16].try_into().unwrap();
        println!("block {k}: SSE ours {:.0} astcenc {:.0}", r.2[0], r.2[1]);
        println!("   ours    {}", desc(&ob));
        println!("   astcenc {}", desc(&tb));
        let s = ai::parse(&ob, fp);
        println!("   ours colour values {:?}", &s.color_values[..s.color_count as usize]);
        let s = ai::parse(&tb, fp);
        println!("   astc colour values {:?}", &s.color_values[..s.color_count as usize]);
    }
    true
}

/// Run astcenc on every image at a float quality, single-threaded; returns
/// (mean PSNR on our decode, total seconds of astcenc's own coding time).
/// CPU time of this thread, seconds.
fn thread_cpu() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes one timespec through a valid pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

/// User + system CPU time of all waited-for children, seconds.
fn children_cpu() -> f64 {
    // SAFETY: getrusage fills one rusage through a valid pointer.
    let mut ru: libc::rusage = unsafe { core::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut ru) };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 * 1e-6;
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

/// astcenc at `-quality q` over the images: (mean PSNR, coding seconds).
/// Coding seconds are CPU time: the child's CPU time times astcenc's own
/// coding share of its wall time (its "Coding time" over "Total time"), which
/// removes PNG I/O and is immune to preemption by other processes.
fn astcenc_at(bin: &str, q: f32, fp: AstcFootprint, imgs: &[(String, u32, u32, Vec<u8>)], tmp: &str) -> (f64, f64) {
    let mut psnr = 0f64;
    let mut secs = 0f64;
    for (path, w, h, img) in imgs {
        let out = format!("{tmp}/es1.astc");
        let _ = std::fs::remove_file(&out);
        let qs = format!("{q}");
        let fps = fp.to_string();
        let c0 = children_cpu();
        let o = std::process::Command::new(bin)
            .args(["-cl", path, &out, &fps, &qs, "-j", "1"])
            .output()
            .expect("astcenc");
        let cpu = children_cpu() - c0;
        // astcenc prints "Total time: X s" and "Coding time: X s" (wall).
        let text = String::from_utf8_lossy(&o.stdout);
        let field = |name: &str| {
            text.lines()
                .find(|l| l.trim_start().starts_with(name))
                .and_then(|l| l.split_whitespace().nth(2))
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(f64::NAN)
        };
        let (total, coding) = (field("Total time"), field("Coding time"));
        secs += cpu * (coding / total);
        let d = std::fs::read(&out).unwrap();
        let f = astc::AstcFile::parse(&d).unwrap();
        let mut dec = vec![0u8; img.len()];
        astc::decode_rgba8_into(f.data, *w, *h, 1, fp, AstcProfile::Ldr, &mut dec).unwrap();
        psnr += psnr_img(img, &dec);
    }
    (psnr / imgs.len() as f64, secs)
}

/// ES1: equal-quality encode speed. For each footprint, our preset's mean
/// PSNR and single-thread rate; astcenc's float -quality bisected until its
/// mean PSNR matches ours within 0.05 dB; then astcenc-none and astcenc-avx2
/// single-thread rates at that quality.
///   astch es1 <preset> <fps> <png>...
const ES1_ROUNDS: usize = 4;

fn cmd_es1(args: &[String]) -> bool {
    let preset = match args[0].as_str() {
        "fastest" => astc::AstcPreset::Fastest,
        "fast" => astc::AstcPreset::Fast,
        "medium" => astc::AstcPreset::Medium,
        "thorough" => astc::AstcPreset::Thorough,
        _ => astc::AstcPreset::Exhaustive,
    };
    let fps: Vec<AstcFootprint> = args[1].split(',').map(fp_parse).collect();
    let imgs: Vec<(String, u32, u32, Vec<u8>)> = args[2..]
        .iter()
        .map(|p| {
            let (w, h, img) = load_rgba(p);
            (p.clone(), w, h, img)
        })
        .collect();
    let tmp = "/root/astcws/out/es1tmp";
    std::fs::create_dir_all(tmp).unwrap();
    let none = "/root/astcws/rel571/inst/bin/astcenc-none";
    let avx2 = "/root/astcws/rel571/inst/bin/astcenc-avx2";
    let px: f64 = imgs.iter().map(|i| (i.1 * i.2) as f64).sum();
    println!("preset {:?}; {} images, {:.2} Mpx; single thread; CPU time (ours: thread clock; astcenc: child CPU x its coding share of wall); {ES1_ROUNDS} ABBA rounds, minimum per arm", preset, imgs.len(), px / 1e6);
    println!("| footprint | ours dB | ours MT/s | astcenc -quality | astcenc dB | none MT/s | avx2 MT/s | ours / none | ours / avx2 |");
    println!("|---|---|---|---|---|---|---|---|---|");
    for &fp in &fps {
        let opts = astc::AstcEncodeOptions::new(fp, AstcProfile::Ldr).with_preset(preset);
        let mut p_ours = 0f64;
        for (_, w, h, img) in &imgs {
            let data = astc::encode_rgba8(img, *w, *h, &opts).unwrap();
            let mut dec = vec![0u8; img.len()];
            astc::decode_rgba8_into(&data, *w, *h, 1, fp, AstcProfile::Ldr, &mut dec).unwrap();
            p_ours += psnr_img(img, &dec);
        }
        p_ours /= imgs.len() as f64;
        // Bisect astcenc quality (PSNR is monotone enough in quality for this).
        let (mut lo, mut hi) = (0f32, 100f32);
        let mut q = 50f32;
        let mut p_ref = 0f64;
        for _ in 0..12 {
            q = (lo + hi) / 2.0;
            p_ref = astcenc_at(none, q, fp, &imgs, tmp).0;
            if (p_ref - p_ours).abs() <= 0.05 {
                break;
            }
            if p_ref < p_ours {
                lo = q;
            } else {
                hi = q;
            }
        }
        // Timing: ABBA rounds (order reversed every round), minimum per arm,
        // so a burst of background load hits one sample, not one arm.
        let ours_once = || {
            let t = thread_cpu();
            for (_, w, h, img) in &imgs {
                std::hint::black_box(astc::encode_rgba8(img, *w, *h, &opts).unwrap());
            }
            thread_cpu() - t
        };
        let (mut t_ours, mut t_none, mut t_avx2) = (f64::MAX, f64::MAX, f64::MAX);
        for r in 0..ES1_ROUNDS {
            for arm in 0..3 {
                let a = if r % 2 == 0 { arm } else { 2 - arm };
                match a {
                    0 => t_ours = t_ours.min(ours_once()),
                    1 => t_none = t_none.min(astcenc_at(none, q, fp, &imgs, tmp).1),
                    _ => t_avx2 = t_avx2.min(astcenc_at(avx2, q, fp, &imgs, tmp).1),
                }
            }
        }
        let (r_ours, r_none, r_avx2) = (px / t_ours / 1e6, px / t_none / 1e6, px / t_avx2 / 1e6);
        println!(
            "| {fp} | {p_ours:.3} | {r_ours:.3} | {q:.2} | {p_ref:.3} | {r_none:.3} | {r_avx2:.3} | {:.2} | {:.2} |",
            r_ours / r_none,
            r_ours / r_avx2
        );
    }
    true
}

/// C4b: write KTX 2.0 variants with our writer for `ktx validate`.
///   astch ktx2out <png> <outdir>
fn cmd_ktx2out(args: &[String]) -> bool {
    use rusty_dds::astc::ktx::VkAstcKind;
    let (w, h, img) = load_rgba(&args[0]);
    let dir = &args[1];
    std::fs::create_dir_all(dir).unwrap();
    // Box-filtered mips of the source for a real chain.
    let mut mips = vec![(w, h, img.clone())];
    while mips.last().unwrap().0 > 1 || mips.last().unwrap().1 > 1 {
        let (pw, ph, ref p) = *mips.last().unwrap();
        let (nw, nh) = ((pw / 2).max(1), (ph / 2).max(1));
        let mut n = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let mut s = 0u32;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (2 * x + dx).min(pw - 1);
                        let sy = (2 * y + dy).min(ph - 1);
                        s += p[((sy * pw + sx) * 4 + c) as usize] as u32;
                    }
                    n[((y * nw + x) * 4 + c) as usize] = ((s + 2) / 4) as u8;
                }
            }
        }
        mips.push((nw, nh, n));
    }
    let mut ok = true;
    for (fp, kind, name, nlev) in [
        (AstcFootprint::B6X6, VkAstcKind::Unorm, "unorm_6x6_mips", mips.len()),
        (AstcFootprint::B8X5, VkAstcKind::Srgb, "srgb_8x5", 1),
        (AstcFootprint::B4X4, VkAstcKind::Sfloat, "sfloat_4x4", 1),
        (AstcFootprint::B12X12, VkAstcKind::Srgb, "srgb_12x12_mips", mips.len()),
    ] {
        let prof = if kind == VkAstcKind::Srgb { AstcProfile::LdrSrgb } else { AstcProfile::Ldr };
        let opts = astc::AstcEncodeOptions::new(fp, prof).with_preset(astc::AstcPreset::Fastest);
        let levels: Vec<Vec<u8>> = mips[..nlev].iter().map(|(mw, mh, p)| astc::encode_rgba8(p, *mw, *mh, &opts).unwrap()).collect();
        let refs: Vec<&[u8]> = levels.iter().map(|v| &v[..]).collect();
        let mut out = Vec::new();
        astc::Ktx2View::write(&mut out, fp, w, h, 1, kind, &refs).unwrap();
        let path = format!("{dir}/{name}.ktx2");
        std::fs::write(&path, &out).unwrap();
        ok &= astc::Ktx2View::parse(&out).is_ok();
        println!("wrote {path}");
    }
    // A 3D texture: 6 slices of the same image region, 4x4x4 footprint.
    let fp = AstcFootprint::B4X4X4;
    let (sw, sh, sd) = (32u32, 32u32, 6u32);
    let mut payload = Vec::new();
    for bz in 0..2u32 {
        for by in 0..8u32 {
            for bx in 0..8u32 {
                // Void-extent blocks carrying a colour from the image: legal 3D content.
                let p = &img[(((by * 4 * w) + bx * 4) * 4) as usize..][..4];
                let mut b = [0u8; 16];
                b[0] = 0xFC;
                b[1] = 0xFD;
                for v in &mut b[2..8] {
                    *v = 0xFF;
                }
                for c in 0..4 {
                    let u = p[c] as u16 * 257 ^ (bz as u16 * 0x101);
                    b[8 + 2 * c] = u as u8;
                    b[9 + 2 * c] = (u >> 8) as u8;
                }
                payload.extend_from_slice(&b);
            }
        }
    }
    let mut out = Vec::new();
    astc::Ktx2View::write(&mut out, fp, sw, sh, sd, VkAstcKind::Unorm, &[&payload]).unwrap();
    std::fs::write(format!("{dir}/unorm_4x4x4_3d.ktx2"), &out).unwrap();
    println!("wrote {dir}/unorm_4x4x4_3d.ktx2");
    ok
}

/// Radiance .hdr (RGBE, new-style RLE or flat) to RGBA f32.
fn load_hdr(path: &str) -> (u32, u32, Vec<f32>) {
    let d = std::fs::read(path).unwrap();
    let mut pos = 0;
    let mut line = || {
        let s = pos;
        while d[pos] != b'\n' {
            pos += 1;
        }
        pos += 1;
        String::from_utf8_lossy(&d[s..pos - 1]).into_owned()
    };
    loop {
        if line().is_empty() {
            break;
        }
    }
    let res = line();
    let parts: Vec<&str> = res.split_whitespace().collect();
    let (h, w): (u32, u32) = (parts[1].parse().unwrap(), parts[3].parse().unwrap());
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    let mut scan = vec![[0u8; 4]; w as usize];
    for _ in 0..h {
        if d[pos] == 2 && d[pos + 1] == 2 && w >= 8 && w < 32768 {
            pos += 4;
            for c in 0..4 {
                let mut x = 0usize;
                while x < w as usize {
                    let n = d[pos] as usize;
                    pos += 1;
                    if n > 128 {
                        let v = d[pos];
                        pos += 1;
                        for _ in 0..n - 128 {
                            scan[x][c] = v;
                            x += 1;
                        }
                    } else {
                        for _ in 0..n {
                            scan[x][c] = d[pos];
                            pos += 1;
                            x += 1;
                        }
                    }
                }
            }
        } else {
            for px in scan.iter_mut() {
                px.copy_from_slice(&d[pos..pos + 4]);
                pos += 4;
            }
        }
        for px in &scan {
            let f = if px[3] == 0 { 0.0 } else { 2f32.powi(px[3] as i32 - 136) };
            out.extend_from_slice(&[px[0] as f32 * f, px[1] as f32 * f, px[2] as f32 * f, 1.0]);
        }
    }
    (w, h, out)
}

fn log_rmse(src: &[f32], dec: &[f32]) -> f64 {
    let mut se = 0f64;
    let mut n = 0f64;
    for (a, b) in src.chunks_exact(4).zip(dec.chunks_exact(4)) {
        for c in 0..3 {
            let x = (a[c].max(1e-4) as f64).log2() - (b[c].max(1e-4) as f64).log2();
            se += x * x;
            n += 1.0;
        }
    }
    (se / n).sqrt()
}

/// EC1 for HDR and 3D encodes: every emitted block decodes identically in
/// astcenc (six cells), and HDR quality against astcenc -ch / -cH (log2 RMSE
/// over RGB, on our decode, which is bit-exact with astcenc's).
///   astch ec1x <hdr files...>
fn cmd_ec1x(args: &[String]) -> bool {
    let tmp = "/root/astcws/out/ec1x";
    std::fs::create_dir_all(tmp).unwrap();
    let mut ok = true;
    println!("| image | footprint | profile | preset | blocks | EC1 bad | ours log2 RMSE | astcenc log2 RMSE |");
    println!("|---|---|---|---|---|---|---|---|");
    for path in args {
        let (w, h, px) = load_hdr(path);
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().into_owned();
        for fp in [AstcFootprint::B4X4, AstcFootprint::B6X6, AstcFootprint::B8X8] {
            for (prof, flag) in [(AstcProfile::HdrRgbLdrA, "-ch"), (AstcProfile::Hdr, "-cH")] {
                let opts = astc::AstcEncodeOptions::new(fp, prof).with_preset(astc::AstcPreset::Medium);
                let data = astc::encode_rgba_f32(&px, w, h, &opts).unwrap();
                let blocks: Vec<[u8; 16]> = data.chunks_exact(16).map(|c| c.try_into().unwrap()).collect();
                let (_, bad) = cells_check(fp, &blocks);
                let ec1: u64 = bad.iter().sum();
                ok &= ec1 == 0;
                let mut dec = vec![0f32; px.len()];
                astc::decode_rgba_f32_into(&data, w, h, 1, fp, prof, &mut dec).unwrap();
                let ours = log_rmse(&px, &dec);
                let out = format!("{tmp}/{name}_{fp}.astc");
                let _ = std::fs::remove_file(&out);
                let fps = fp.to_string();
                let theirs = if run_astcenc(&[flag, path, &out, &fps, "-medium", "-j", "8", "-silent"]) {
                    let d = std::fs::read(&out).unwrap();
                    let f = astc::AstcFile::parse(&d).unwrap();
                    let mut dec2 = vec![0f32; px.len()];
                    astc::decode_rgba_f32_into(f.data, w, h, 1, fp, prof, &mut dec2).unwrap();
                    log_rmse(&px, &dec2)
                } else {
                    f64::NAN
                };
                println!("| {name} | {fp} | {prof:?} | Medium | {} | {ec1} | {ours:.4} | {theirs:.4} |", blocks.len());
            }
        }
    }
    // 3D: six 256x256 slices, every 3D footprint, EC1.
    let slices: Vec<(u32, u32, Vec<u8>)> = (0..6).map(|i| load_rgba(&format!("/root/astcws/corpus/slices/ldr_{i}.png"))).collect();
    let (w, h) = (slices[0].0, slices[0].1);
    let vol: Vec<u8> = slices.iter().flat_map(|s| s.2.iter().copied()).collect();
    for &fp in AstcFootprint::ALL_3D {
        let opts = astc::AstcEncodeOptions::new(fp, AstcProfile::Ldr).with_preset(astc::AstcPreset::Fast);
        let data = astc::encode_volume_rgba8(&vol, w, h, 6, &opts).unwrap();
        let blocks: Vec<[u8; 16]> = data.chunks_exact(16).map(|c| c.try_into().unwrap()).collect();
        let (_, bad) = cells_check(fp, &blocks);
        let ec1: u64 = bad.iter().sum();
        ok &= ec1 == 0;
        let mut dec = vec![0u8; vol.len()];
        astc::decode_rgba8_into(&data, w, h, 6, fp, AstcProfile::Ldr, &mut dec).unwrap();
        println!("| volume 256x256x6 | {fp} | Ldr | Fast | {} | {ec1} | PSNR {:.3} dB | |", blocks.len(), psnr_img(&vol, &dec));
    }
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
        Some("containers") => cmd_containers(args.get(2).map(|s| s.as_str()).unwrap_or("/root/astcws/out/ctmp")),
        Some("dds") => cmd_dds(args.get(2).map(|s| s.as_str()).unwrap_or("/root/astcws/out/ctmp")),
        Some("quality") => cmd_quality(&args[2..]),
        Some("blockdbg") => cmd_blockdbg(&args[2..]),
        Some("es1") => cmd_es1(&args[2..]),
        Some("mosaic") => cmd_mosaic(&args[2..]),
        Some("modehist") => cmd_modehist(&args[2..]),
        Some("ktx2out") => cmd_ktx2out(&args[2..]),
        Some("ec1x") => cmd_ec1x(&args[2..]),
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
