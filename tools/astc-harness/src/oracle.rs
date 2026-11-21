//! Spawn the astc-oracle with stdin/stdout pipes.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

pub fn path() -> String {
    std::env::var("ASTC_ORACLE").unwrap_or_else(|_| "/root/astcws/rdds/tools/astc-oracle/oracle".into())
}

/// Run the oracle with `args`, feeding `input` on stdin; returns stdout.
pub fn run(args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new(path())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn astc-oracle (set ASTC_ORACLE)");
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    writer.join().unwrap();
    let st = child.wait().unwrap();
    assert!(st.success(), "astc-oracle {args:?} failed");
    out
}
