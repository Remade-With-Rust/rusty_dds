//! Compatibility shim — prefer `rusty-dds retag`.

use rusty_dds::*;
use std::env;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom};
use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!("note: `retag` is deprecated; use `rusty-dds retag`");
    let Some(filename) = env::args().nth(1) else {
        eprintln!("Usage: retag <ddsfile> <DxgiFormat>");
        return ExitCode::from(2);
    };
    let Some(tag) = env::args().nth(2) else {
        eprintln!("Usage: retag <ddsfile> <DxgiFormat>");
        return ExitCode::from(2);
    };

    let format = match tag.as_str() {
        "BC1_UNorm" => DxgiFormat::BC1_UNorm,
        "BC7_UNorm" => DxgiFormat::BC7_UNorm,
        "BC7_UNorm_sRGB" => DxgiFormat::BC7_UNorm_sRGB,
        "R8G8B8A8_UNorm" => DxgiFormat::R8G8B8A8_UNorm,
        _ => {
            eprintln!("format not in shim list: {tag} (use rusty-dds retag for full set)");
            return ExitCode::from(2);
        }
    };

    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(&filename)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open {filename}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut dds = match Dds::read(&mut file) {
        Ok(dds) => dds,
        Err(e) => {
            eprintln!("read: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(ref mut h10) = dds.header10 else {
        eprintln!("DX10 header required");
        return ExitCode::FAILURE;
    };
    h10.dxgi_format = format;
    if file.seek(SeekFrom::Start(0)).is_err() || dds.write(&mut file).is_err() {
        eprintln!("write failed");
        return ExitCode::FAILURE;
    }
    println!("Done.");
    ExitCode::SUCCESS
}
