//! Compatibility shim — prefer `rusty-dds info`.

use rusty_dds::*;
use std::env;
use std::fs::File;
use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!("note: `ddsinfo` is deprecated; use `rusty-dds info`");
    let Some(filename) = env::args().nth(1) else {
        eprintln!("Usage: ddsinfo <filename>");
        return ExitCode::from(2);
    };
    let mut file = match File::open(&filename) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("open {filename}: {e}");
            return ExitCode::FAILURE;
        }
    };
    match Dds::read(&mut file) {
        Ok(dds) => {
            println!("{dds:?}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("read {filename}: {e}");
            ExitCode::FAILURE
        }
    }
}
