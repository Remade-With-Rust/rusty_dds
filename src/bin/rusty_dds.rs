//! Unified CLI: `rusty-dds info|decode|encode|retag`
//!
//! ```text
//! cargo run --bin rusty-dds -- info texture.dds
//! cargo run --bin rusty-dds -- decode texture.dds -o out.rgba
//! cargo run --bin rusty-dds -- encode --width 64 --height 64 --format bc7 pixels.rgba -o out.dds
//! cargo run --bin rusty-dds -- retag texture.dds BC7_UNorm_sRGB
//! ```

use rusty_dds::*;
use std::env;
use std::fs::{self, File};
use std::io::{Seek, SeekFrom};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return if args.is_empty() {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        };
    }

    let cmd = args.remove(0);
    match cmd.as_str() {
        "info" => cmd_info(&args),
        "decode" => cmd_decode(&args),
        "encode" => cmd_encode(&args),
        "retag" => cmd_retag(&args),
        other => {
            eprintln!("unknown command: {other}");
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    eprintln!(
        "\
rusty-dds — DDS texture toolkit (Remade With Rust)

Usage:
  rusty-dds info   <file.dds|.astc|.ktx|.ktx2>
  rusty-dds decode <file.dds|.astc|.ktx|.ktx2> [--mip N] [--layer N] [--face N]
                   [--profile srgb|ldr] [-o out.rgba]
  rusty-dds encode --width W --height H --format <name> <pixels.rgba> [-o out.dds]
                   [--mips N] [--depth D] [--array N] [--cubemap]
  rusty-dds encode --width W --height H --format astc --block 6x6
                   [--preset fastest|fast|medium|thorough] [--profile srgb|ldr]
                   <pixels.rgba> -o out.astc|out.ktx|out.ktx2|out.dds
  rusty-dds retag  <file.dds> <DxgiFormat>

Formats: bc1 bc2 bc3 bc4u bc4s bc5u bc5s bc7 rgba8 bgra8 astc
Retag examples: BC7_UNorm  BC7_UNorm_sRGB  BC1_UNorm  R8G8B8A8_UNorm
"
    );
}

fn cmd_info(args: &[String]) -> ExitCode {
    let Some(path) = args.first() else {
        eprintln!("Usage: rusty-dds info <file.dds>");
        return ExitCode::from(2);
    };
    #[cfg(feature = "astc")]
    if let Some(code) = astc_info(path) {
        return code;
    }
    match read_dds(path) {
        Ok(dds) => {
            println!("{dds:?}");
            #[cfg(feature = "astc")]
            if let Ok(c) = dds.astc_content() {
                println!(
                    "  ASTC content: {} {}",
                    c.footprint,
                    if c.srgb { "sRGB" } else { "linear" }
                );
            }
            if let Ok(c) = dds.decode_content() {
                println!("  LDR content: {}", c.name());
            }
            if let Ok(g) = dds.gpu_format() {
                println!("  GPU: wgpu={} vulkan={}", g.wgpu_name, g.vulkan_name);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_decode(args: &[String]) -> ExitCode {
    let mut path = None;
    let mut out = None;
    let mut mip = 0u32;
    let mut layer = 0u32;
    let mut face = 0u32;
    #[allow(unused_mut, unused_variables, unused_assignments)]
    let mut profile: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--profile" => {
                i += 1;
                profile = args.get(i).cloned();
            }
            "--mip" => {
                i += 1;
                mip = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--layer" => {
                i += 1;
                layer = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--face" => {
                i += 1;
                face = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "-o" | "--output" => {
                i += 1;
                out = args.get(i).cloned();
            }
            s if !s.starts_with('-') && path.is_none() => path = Some(s.to_string()),
            s => {
                eprintln!("unknown decode arg: {s}");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    let Some(path) = path else {
        eprintln!("Usage: rusty-dds decode <file.dds> [-o out.rgba]");
        return ExitCode::from(2);
    };

    #[cfg(feature = "astc")]
    {
        let out_path = out.clone().unwrap_or_else(|| {
            let mut p = PathBuf::from(&path);
            p.set_extension("rgba");
            p.display().to_string()
        });
        let prof = match profile.as_deref().map(astc_profile_arg) {
            Some(None) => {
                eprintln!("--profile: srgb|ldr|hdr|hdr-ldr-a");
                return ExitCode::from(2);
            }
            Some(p) => p,
            None => None,
        };
        if let Some(code) = astc_decode(&path, mip, layer, face, prof, &out_path) {
            return code;
        }
    }
    let dds = match read_dds(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let id = if dds.is_cubemap() {
        match CubemapFace::from_index(face) {
            Ok(f) => SubresourceId::cubemap(mip, layer, f),
            Err(_) => {
                eprintln!("invalid --face {face}");
                return ExitCode::from(2);
            }
        }
    } else {
        SubresourceId::mip_layer(mip, layer)
    };

    let img = match dds.decode_rgba8(id) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("decode: {e}");
            return ExitCode::FAILURE;
        }
    };

    let out_path = out.unwrap_or_else(|| {
        let mut p = PathBuf::from(&path);
        p.set_extension("rgba");
        p.display().to_string()
    });
    if let Err(e) = fs::write(&out_path, &img.pixels) {
        eprintln!("write {out_path}: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "wrote {out_path} ({}x{}x{} RGBA8, {} bytes)",
        img.width,
        img.height,
        img.depth,
        img.pixels.len()
    );
    ExitCode::SUCCESS
}

fn cmd_encode(args: &[String]) -> ExitCode {
    let mut width = None;
    let mut height = None;
    let mut depth = 1u32;
    let mut mips = 1u32;
    let mut array = 1u32;
    let mut cubemap = false;
    let mut format = None;
    let mut input = None;
    let mut out = None;
    let mut block = String::from("6x6");
    let mut preset = String::from("medium");
    let mut profile = String::from("srgb");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--block" => {
                i += 1;
                block = args.get(i).cloned().unwrap_or_default();
            }
            "--preset" => {
                i += 1;
                preset = args.get(i).cloned().unwrap_or_default();
            }
            "--profile" => {
                i += 1;
                profile = args.get(i).cloned().unwrap_or_default();
            }
            "--width" => {
                i += 1;
                width = args.get(i).and_then(|s| s.parse().ok());
            }
            "--height" => {
                i += 1;
                height = args.get(i).and_then(|s| s.parse().ok());
            }
            "--depth" => {
                i += 1;
                depth = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1);
            }
            "--mips" => {
                i += 1;
                mips = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1);
            }
            "--array" => {
                i += 1;
                array = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1);
            }
            "--cubemap" => cubemap = true,
            "--format" | "-f" => {
                i += 1;
                format = args.get(i).cloned();
            }
            "-o" | "--output" => {
                i += 1;
                out = args.get(i).cloned();
            }
            s if !s.starts_with('-') && input.is_none() => input = Some(s.to_string()),
            s => {
                eprintln!("unknown encode arg: {s}");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }

    let (Some(w), Some(h), Some(fmt_name), Some(in_path)) = (width, height, format, input) else {
        eprintln!(
            "Usage: rusty-dds encode --width W --height H --format bc7 pixels.rgba [-o out.dds]"
        );
        return ExitCode::from(2);
    };
    if fmt_name.eq_ignore_ascii_case("astc") {
        #[cfg(feature = "astc")]
        {
            let pixels = match fs::read(&in_path) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("read {in_path}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let out_path = out.unwrap_or_else(|| {
                let mut p = PathBuf::from(&in_path);
                p.set_extension("astc");
                p.display().to_string()
            });
            return astc_encode(&pixels, w, h, &block, &preset, &profile, &out_path);
        }
        #[cfg(not(feature = "astc"))]
        {
            let _ = (&block, &preset, &profile);
            eprintln!("this build has no `astc` feature");
            return ExitCode::from(2);
        }
    }
    let Some(content) = DecodeContent::from_name(&fmt_name) else {
        eprintln!("unknown --format {fmt_name} (try bc1, bc7, rgba8, …)");
        return ExitCode::from(2);
    };

    let pixels = match fs::read(&in_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("read {in_path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut layout = EncodeLayout::flat_2d(content, w, h)
        .with_mips(mips)
        .with_array(array)
        .with_depth(depth);
    if cubemap {
        layout = layout.cubemap();
    }

    let dds = match Dds::encode_from_rgba8(&pixels, layout) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("encode: {e}");
            return ExitCode::FAILURE;
        }
    };

    let out_path = out.unwrap_or_else(|| {
        let mut p = PathBuf::from(&in_path);
        p.set_extension("dds");
        p.display().to_string()
    });
    let mut file = match File::create(&out_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("create {out_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = dds.write(&mut file) {
        eprintln!("write: {e}");
        return ExitCode::FAILURE;
    }
    println!("wrote {out_path} ({})", content.name());
    ExitCode::SUCCESS
}

fn cmd_retag(args: &[String]) -> ExitCode {
    let Some(path) = args.first() else {
        eprintln!("Usage: rusty-dds retag <file.dds> <DxgiFormat>");
        return ExitCode::from(2);
    };
    let Some(tag) = args.get(1) else {
        eprintln!("Usage: rusty-dds retag <file.dds> <DxgiFormat>");
        return ExitCode::from(2);
    };
    let format = match parse_dxgi_tag(tag) {
        Some(f) => f,
        None => {
            eprintln!("unknown DXGI tag: {tag}");
            return ExitCode::from(2);
        }
    };

    let mut file = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut dds = match Dds::read(&mut file) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("read: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(ref mut h10) = dds.header10 else {
        eprintln!("DX10 header required for retag");
        return ExitCode::FAILURE;
    };
    h10.dxgi_format = format;
    if file.seek(SeekFrom::Start(0)).is_err() || dds.write(&mut file).is_err() {
        eprintln!("write failed");
        return ExitCode::FAILURE;
    }
    println!("retagged {path} → {tag}");
    ExitCode::SUCCESS
}

fn parse_dxgi_tag(tag: &str) -> Option<DxgiFormat> {
    use DxgiFormat::*;
    Some(match tag {
        "BC1_UNorm" => BC1_UNorm,
        "BC1_UNorm_sRGB" => BC1_UNorm_sRGB,
        "BC2_UNorm" => BC2_UNorm,
        "BC2_UNorm_sRGB" => BC2_UNorm_sRGB,
        "BC3_UNorm" => BC3_UNorm,
        "BC3_UNorm_sRGB" => BC3_UNorm_sRGB,
        "BC4_UNorm" => BC4_UNorm,
        "BC4_SNorm" => BC4_SNorm,
        "BC5_UNorm" => BC5_UNorm,
        "BC5_SNorm" => BC5_SNorm,
        "BC7_UNorm" => BC7_UNorm,
        "BC7_UNorm_sRGB" => BC7_UNorm_sRGB,
        "R8G8B8A8_UNorm" => R8G8B8A8_UNorm,
        "R8G8B8A8_UNorm_sRGB" => R8G8B8A8_UNorm_sRGB,
        "B8G8R8A8_UNorm" => B8G8R8A8_UNorm,
        "B8G8R8A8_UNorm_sRGB" => B8G8R8A8_UNorm_sRGB,
        _ => return None,
    })
}

fn read_dds(path: &str) -> Result<Dds, String> {
    let mut file = File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    Dds::read(&mut file).map_err(|e| format!("read {path}: {e}"))
}

// ------------------------------------------------------------------ ASTC --

/// ASTC containers the CLI reads besides DDS, detected by magic.
#[cfg(feature = "astc")]
enum AstcInput<'a> {
    File(astc::AstcFile<'a>),
    Ktx(astc::KtxView<'a>),
    Ktx2(astc::Ktx2View<'a>),
}

#[cfg(feature = "astc")]
fn astc_input(bytes: &[u8]) -> Option<Result<AstcInput<'_>, Error>> {
    if bytes.starts_with(&[0x13, 0xAB, 0xA1, 0x5C]) {
        return Some(astc::AstcFile::parse(bytes).map(AstcInput::File));
    }
    if bytes.starts_with(&[0xAB, 0x4B, 0x54, 0x58, 0x20, 0x31, 0x31]) {
        return Some(astc::KtxView::parse(bytes).map(AstcInput::Ktx));
    }
    if bytes.starts_with(&[0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30]) {
        return Some(astc::Ktx2View::parse(bytes).map(AstcInput::Ktx2));
    }
    None
}

#[cfg(feature = "astc")]
fn astc_profile_arg(s: &str) -> Option<astc::AstcProfile> {
    Some(match s {
        "srgb" | "ldr-srgb" => astc::AstcProfile::LdrSrgb,
        "ldr" | "linear" => astc::AstcProfile::Ldr,
        "hdr" => astc::AstcProfile::Hdr,
        "hdr-ldr-a" => astc::AstcProfile::HdrRgbLdrA,
        _ => return None,
    })
}

/// `info` for .astc / .ktx / .ktx2. Returns None when the file is not one.
#[cfg(feature = "astc")]
fn astc_info(path: &str) -> Option<ExitCode> {
    let bytes = fs::read(path).ok()?;
    let r = astc_input(&bytes)?;
    Some(match r {
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
        Ok(AstcInput::File(f)) => {
            println!(
                ".astc: {} block, {}x{}x{}, {} bytes of blocks",
                f.footprint,
                f.width,
                f.height,
                f.depth,
                f.data.len()
            );
            ExitCode::SUCCESS
        }
        Ok(AstcInput::Ktx(k)) => {
            println!(
                "KTX 1.0: ASTC {} {}, {}x{}x{}, layers {}, faces {}, levels {}",
                k.footprint,
                if k.srgb { "sRGB" } else { "linear" },
                k.width,
                k.height,
                k.depth,
                k.layers,
                k.faces,
                k.levels
            );
            ExitCode::SUCCESS
        }
        Ok(AstcInput::Ktx2(k)) => {
            println!(
                "KTX 2.0: ASTC {} {}, {}x{}x{}, layers {}, faces {}, levels {}",
                k.footprint,
                if k.hdr {
                    "SFLOAT"
                } else if k.srgb {
                    "sRGB"
                } else {
                    "UNORM"
                },
                k.width,
                k.height,
                k.depth,
                k.layers,
                k.faces,
                k.levels
            );
            ExitCode::SUCCESS
        }
    })
}

/// `decode` for .astc / .ktx / .ktx2 to raw RGBA8.
#[cfg(feature = "astc")]
fn astc_decode(
    path: &str,
    mip: u32,
    layer: u32,
    face: u32,
    profile: Option<astc::AstcProfile>,
    out: &str,
) -> Option<ExitCode> {
    let bytes = fs::read(path).ok()?;
    let r = astc_input(&bytes)?;
    let surf = match r {
        Err(e) => {
            eprintln!("{e}");
            return Some(ExitCode::FAILURE);
        }
        Ok(AstcInput::File(f)) => Ok(f.surface()),
        Ok(AstcInput::Ktx(k)) => k.astc_surface(mip, layer, face),
        Ok(AstcInput::Ktx2(k)) => k.astc_surface(mip, layer, face),
    };
    let surf = match surf {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Some(ExitCode::FAILURE);
        }
    };
    let img = match surf.decode_rgba8(profile.unwrap_or(surf.default_profile())) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("decode: {e}");
            return Some(ExitCode::FAILURE);
        }
    };
    if let Err(e) = fs::write(out, &img.pixels) {
        eprintln!("write {out}: {e}");
        return Some(ExitCode::FAILURE);
    }
    println!(
        "wrote {out} ({}x{}x{} RGBA8, {} bytes)",
        img.width,
        img.height,
        img.depth,
        img.pixels.len()
    );
    Some(ExitCode::SUCCESS)
}

/// `encode --format astc`: the container follows the output extension
/// (.astc, .ktx or .dds).
#[cfg(feature = "astc")]
fn astc_encode(
    pixels: &[u8],
    w: u32,
    h: u32,
    block: &str,
    preset: &str,
    profile: &str,
    out: &str,
) -> ExitCode {
    let dims: Vec<u32> = block.split('x').filter_map(|t| t.parse().ok()).collect();
    let Some(fp) = (if dims.len() == 2 {
        astc::AstcFootprint::new_2d(dims[0], dims[1])
    } else {
        None
    }) else {
        eprintln!("--block {block}: not a 2D ASTC footprint (4x4 ... 12x12)");
        return ExitCode::from(2);
    };
    let preset = match preset {
        "fastest" => astc::AstcPreset::Fastest,
        "fast" => astc::AstcPreset::Fast,
        "medium" => astc::AstcPreset::Medium,
        "thorough" => astc::AstcPreset::Thorough,
        "exhaustive" => astc::AstcPreset::Exhaustive,
        other => {
            eprintln!("--preset {other}: fastest|fast|medium|thorough|exhaustive");
            return ExitCode::from(2);
        }
    };
    let Some(prof) = astc_profile_arg(profile) else {
        eprintln!("--profile {profile}: srgb|ldr");
        return ExitCode::from(2);
    };
    let opts = astc::AstcEncodeOptions::new(fp, prof).with_preset(preset);
    let ext = std::path::Path::new(out)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut bytes = Vec::new();
    let r = match ext.as_str() {
        "dds" => Dds::encode_astc_from_rgba8(pixels, w, h, &opts).and_then(|d| d.write(&mut bytes)),
        "ktx2" => astc::encode_rgba8(pixels, w, h, &opts).and_then(|p| {
            let kind = if prof == astc::AstcProfile::LdrSrgb {
                astc::ktx::VkAstcKind::Srgb
            } else {
                astc::ktx::VkAstcKind::Unorm
            };
            astc::Ktx2View::write(&mut bytes, fp, w, h, 1, kind, &[&p])
        }),
        "ktx" => astc::encode_rgba8(pixels, w, h, &opts).and_then(|p| {
            astc::KtxView::write(
                &mut bytes,
                fp,
                w,
                h,
                1,
                prof == astc::AstcProfile::LdrSrgb,
                &[&p],
            )
        }),
        _ => astc::encode_rgba8(pixels, w, h, &opts)
            .and_then(|p| astc::AstcFile::write(&mut bytes, fp, w, h, 1, &p)),
    };
    if let Err(e) = r {
        eprintln!("encode: {e}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = fs::write(out, &bytes) {
        eprintln!("write {out}: {e}");
        return ExitCode::FAILURE;
    }
    println!("wrote {out} (ASTC {fp}, {preset:?}, {prof:?})");
    ExitCode::SUCCESS
}
