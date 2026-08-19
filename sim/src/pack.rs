//! Cooking the asset pack, and the manifest that pins it into a run.
//!
//! Sources are procedural so the harness runs on any box with no corpus
//! checkout. They are *not* a substitute for studio maps — the plan's open
//! question 1 stands, and the manifest records `source=procedural` so no board
//! can quietly present these as content numbers.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rusty_dds::{Dds, DecodeContent, EncodeLayout};

use crate::hash::{fnv1a, hash_2d, mix, Rng, FNV_OFFSET};
use crate::provider::{SimError, SimResult};
use crate::scenario::Tier;

// ------------------------------------------------------------------ manifest

#[derive(Clone, Debug)]
pub struct PackTexture {
    pub file: String,
    pub content: &'static str,
    pub width: u32,
    pub height: u32,
    pub mips: u32,
    pub bytes: u64,
    pub hash: u64,
}

#[derive(Clone, Debug)]
pub struct Pack {
    pub dir: PathBuf,
    pub tier: Tier,
    pub size: u32,
    pub rdo_lambda: f32,
    pub textures: Vec<PackTexture>,
    pub total_bytes: u64,
    /// Fold of every payload hash — two runs whose pack hashes differ are not
    /// comparable, and the board says so rather than averaging them.
    pub hash: u64,
}

impl Pack {
    pub fn path(&self, t: &PackTexture) -> PathBuf {
        self.dir.join(&t.file)
    }

    pub fn mips_per_texture(&self) -> Vec<u32> {
        self.textures.iter().map(|t| t.mips).collect()
    }

    fn manifest_path(dir: &Path) -> PathBuf {
        dir.join("pack.txt")
    }

    pub fn save(&self) -> SimResult<()> {
        let mut s = String::new();
        s.push_str("# rusty_dds_sim pack manifest v1\n");
        s.push_str("source procedural\n");
        s.push_str(&format!("tier {}\n", self.tier.name()));
        s.push_str(&format!("size {}\n", self.size));
        s.push_str(&format!("rdo_lambda {}\n", self.rdo_lambda));
        s.push_str(&format!("total_bytes {}\n", self.total_bytes));
        s.push_str(&format!("hash {:016x}\n", self.hash));
        for t in &self.textures {
            s.push_str(&format!(
                "texture {} {} {} {} {} {} {:016x}\n",
                t.file, t.content, t.width, t.height, t.mips, t.bytes, t.hash
            ));
        }
        fs::write(Self::manifest_path(&self.dir), s)?;
        Ok(())
    }

    pub fn load(dir: &Path) -> SimResult<Pack> {
        let text = fs::read_to_string(Self::manifest_path(dir)).map_err(|e| {
            SimError(format!(
                "no pack at {} ({e}) — run `sim cook` first",
                dir.display()
            ))
        })?;
        let mut pack = Pack {
            dir: dir.to_path_buf(),
            tier: Tier::Medium,
            size: 0,
            rdo_lambda: 0.0,
            textures: Vec::new(),
            total_bytes: 0,
            hash: 0,
        };
        for line in text.lines() {
            let mut f = line.split_whitespace();
            match f.next() {
                Some("tier") => {
                    pack.tier = f
                        .next()
                        .and_then(Tier::parse)
                        .ok_or_else(|| SimError("bad tier in manifest".into()))?
                }
                Some("size") => pack.size = parse_field(f.next(), "size")?,
                Some("rdo_lambda") => pack.rdo_lambda = parse_field(f.next(), "rdo_lambda")?,
                Some("total_bytes") => pack.total_bytes = parse_field(f.next(), "total_bytes")?,
                Some("hash") => {
                    pack.hash = u64::from_str_radix(f.next().unwrap_or(""), 16)
                        .map_err(|e| SimError(format!("bad pack hash: {e}")))?
                }
                Some("texture") => {
                    let file = f
                        .next()
                        .ok_or_else(|| SimError("texture line missing file".into()))?
                        .to_string();
                    let content = f
                        .next()
                        .ok_or_else(|| SimError("texture line missing content".into()))?;
                    let content = DecodeContent::ALL_LDR
                        .iter()
                        .find(|c| c.name() == content)
                        .map(|c| c.name())
                        .ok_or_else(|| SimError(format!("unknown content `{content}`")))?;
                    pack.textures.push(PackTexture {
                        file,
                        content,
                        width: parse_field(f.next(), "width")?,
                        height: parse_field(f.next(), "height")?,
                        mips: parse_field(f.next(), "mips")?,
                        bytes: parse_field(f.next(), "bytes")?,
                        hash: u64::from_str_radix(f.next().unwrap_or(""), 16)
                            .map_err(|e| SimError(format!("bad texture hash: {e}")))?,
                    });
                }
                _ => {}
            }
        }
        if pack.textures.is_empty() {
            return Err(SimError("pack manifest lists no textures".into()));
        }
        Ok(pack)
    }
}

/// Exact payload size of one mip, from the manifest alone.
///
/// The pool needs this *before* it opens a file, to fill a per-frame upload
/// budget deterministically. Deriving it from the manifest keeps budgeting a
/// pure function of the pack rather than of whatever happens to be open.
pub fn sub_bytes(content: &str, size: u32, mip: u32) -> u64 {
    let block_bytes: u64 = match content {
        "bc1" | "bc4u" | "bc4s" => 8,
        "rgba8" | "bgra8" => 0, // uncompressed: handled below
        _ => 16,
    };
    let w = (size >> mip).max(1) as u64;
    let h = w;
    if block_bytes == 0 {
        return w * h * 4;
    }
    w.div_ceil(4) * h.div_ceil(4) * block_bytes
}

fn parse_field<T: std::str::FromStr>(v: Option<&str>, what: &str) -> SimResult<T> {
    v.ok_or_else(|| SimError(format!("manifest missing {what}")))?
        .parse()
        .map_err(|_| SimError(format!("manifest field {what} is not a number")))
}

// -------------------------------------------------------------------- cooking

pub struct CookOptions {
    pub tier: Tier,
    pub textures: u32,
    pub size: Option<u32>,
    pub out: PathBuf,
    pub threads: usize,
}

pub fn cook(opts: &CookOptions) -> SimResult<Pack> {
    let size = opts.size.unwrap_or_else(|| opts.tier.default_size());
    if !size.is_power_of_two() || size < 4 {
        return Err(SimError("pack size must be a power of two >= 4".into()));
    }
    if opts.textures == 0 {
        return Err(SimError("a pack needs at least one texture".into()));
    }
    fs::create_dir_all(&opts.out)?;

    let mips = (size.trailing_zeros()) + 1;
    let jobs: Vec<u32> = (0..opts.textures).collect();
    let threads = opts.threads.max(1).min(jobs.len().max(1));

    let mut cooked: Vec<Option<PackTexture>> = vec![None; jobs.len()];
    let tier = opts.tier;
    let out = opts.out.clone();

    std::thread::scope(|scope| -> SimResult<()> {
        let chunk = jobs.len().div_ceil(threads);
        let mut handles = Vec::new();
        for (slot, ids) in cooked.chunks_mut(chunk).zip(jobs.chunks(chunk)) {
            let out = out.clone();
            handles.push(scope.spawn(move || -> SimResult<()> {
                for (dst, &id) in slot.iter_mut().zip(ids) {
                    *dst = Some(cook_one(tier, id, size, mips, &out)?);
                }
                Ok(())
            }));
        }
        for h in handles {
            h.join().map_err(|_| SimError("cook worker panicked".into()))??;
        }
        Ok(())
    })?;

    let textures: Vec<PackTexture> = cooked.into_iter().flatten().collect();
    let total_bytes = textures.iter().map(|t| t.bytes).sum();
    let mut hash = FNV_OFFSET;
    for t in &textures {
        hash = mix(hash, t.hash);
    }

    let pack = Pack {
        dir: opts.out.clone(),
        tier,
        size,
        rdo_lambda: tier.rdo().strength(),
        textures,
        total_bytes,
        hash,
    };
    pack.save()?;
    Ok(pack)
}

fn cook_one(
    tier: Tier,
    id: u32,
    size: u32,
    mips: u32,
    out: &Path,
) -> SimResult<PackTexture> {
    let content = tier.content_for(id);
    let pixels = source_rgba8(content, id, size);
    let layout = EncodeLayout::flat_2d(content, size, size)
        .with_mips(mips)
        .with_rdo(tier.rdo());
    let dds = Dds::encode_from_rgba8(&pixels, layout)?;

    let mut bytes = Vec::new();
    dds.write(&mut bytes)?;
    let file = format!("t{id:04}_{}.dds", content.name());
    let mut f = fs::File::create(out.join(&file))?;
    f.write_all(&bytes)?;
    f.flush()?;

    Ok(PackTexture {
        file,
        content: content.name(),
        width: size,
        height: size,
        mips,
        bytes: bytes.len() as u64,
        hash: fnv1a(&bytes),
    })
}

// ------------------------------------------------------- procedural sources

/// Fractal value noise in `[0, 1]`.
fn noise(seed: u64, x: f32, y: f32, octaves: u32) -> f32 {
    let mut sum = 0.0;
    let mut amp = 1.0;
    let mut total = 0.0;
    let mut freq = 1.0;
    for o in 0..octaves {
        sum += amp * value_noise(seed ^ (o as u64) << 32, x * freq, y * freq);
        total += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / total
}

fn value_noise(seed: u64, x: f32, y: f32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (fx, fy) = (x - xi, y - yi);
    // Smoothstep so the encoders see gradients, not a blocky lattice.
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (ix, iy) = (xi as i32, yi as i32);
    let a = hash_2d(seed, ix, iy);
    let b = hash_2d(seed, ix + 1, iy);
    let c = hash_2d(seed, ix, iy + 1);
    let d = hash_2d(seed, ix + 1, iy + 1);
    let top = a + (b - a) * sx;
    let bot = c + (d - c) * sx;
    top + (bot - top) * sy
}

/// One procedural source, shaped for the format it will be encoded to.
///
/// Flat sources would make every encoder look identical and every RDO pass
/// look free, so each map carries gradients, high-frequency detail and a few
/// hard edges — the structure BCn actually has to spend bits on.
fn source_rgba8(content: DecodeContent, id: u32, size: u32) -> Vec<u8> {
    let seed = crate::hash::fnv1a_seed(0x5A17_C0DE, &id.to_le_bytes());
    let mut rng = Rng::new(seed);
    let scale = rng.range_f32(3.0, 9.0) / size as f32;
    let tint = [
        rng.range_f32(0.45, 1.0),
        rng.range_f32(0.45, 1.0),
        rng.range_f32(0.45, 1.0),
    ];
    // A few hard-edged features per map: BCn spends its error budget on edges,
    // and RDO's match-finding behaves differently around them.
    let feature_count = 6;
    let features: Vec<(f32, f32, f32)> = (0..feature_count)
        .map(|_| {
            (
                rng.range_f32(0.0, size as f32),
                rng.range_f32(0.0, size as f32),
                rng.range_f32(size as f32 * 0.03, size as f32 * 0.12),
            )
        })
        .collect();

    let mut out = vec![0u8; (size as usize) * (size as usize) * 4];
    let h = |x: f32, y: f32| noise(seed, x * scale, y * scale, 5);

    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32, y as f32);
            let mut n = h(fx, fy);
            for &(cx, cy, cr) in &features {
                let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                if d < cr {
                    n = (n * 0.35 + 0.65).min(1.0);
                }
            }
            let o = ((y as usize) * size as usize + x as usize) * 4;
            let px = &mut out[o..o + 4];
            match content {
                DecodeContent::Bc5UNorm | DecodeContent::Bc5SNorm => {
                    // Normal map from the heightfield's gradient.
                    let dx = h(fx + 1.0, fy) - h(fx - 1.0, fy);
                    let dy = h(fx, fy + 1.0) - h(fx, fy - 1.0);
                    let (nx, ny, nz) = normalize(-dx * 8.0, -dy * 8.0, 1.0);
                    px[0] = enc_unit(nx);
                    px[1] = enc_unit(ny);
                    px[2] = enc_unit(nz);
                    px[3] = 255;
                }
                DecodeContent::Bc4UNorm | DecodeContent::Bc4SNorm => {
                    let v = (n * 255.0) as u8;
                    px[0] = v;
                    px[1] = v;
                    px[2] = v;
                    px[3] = 255;
                }
                DecodeContent::Bc3 | DecodeContent::Bc2 => {
                    px[0] = (n * tint[0] * 255.0) as u8;
                    px[1] = (n * tint[1] * 255.0) as u8;
                    px[2] = (n * tint[2] * 255.0) as u8;
                    // Independent alpha channel so BC3's alpha search has work.
                    px[3] = (h(fx * 1.7 + 91.0, fy * 1.7 - 37.0) * 255.0) as u8;
                }
                _ => {
                    px[0] = (n * tint[0] * 255.0) as u8;
                    px[1] = (n * tint[1] * 255.0) as u8;
                    px[2] = (n * tint[2] * 255.0) as u8;
                    px[3] = 255;
                }
            }
        }
    }
    out
}

fn normalize(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let len = (x * x + y * y + z * z).sqrt().max(1e-6);
    (x / len, y / len, z / len)
}

fn enc_unit(v: f32) -> u8 {
    (((v * 0.5 + 0.5).clamp(0.0, 1.0)) * 255.0).round() as u8
}
