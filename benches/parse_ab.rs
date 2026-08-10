//! Parse A/B: crates.io `ddsfile` vs local `rusty_dds` on identical fixture bytes.
//!
//! Preloads fixtures into memory so the timer measures container parse, not disk I/O.
//!
//! ```text
//! cargo bench --bench parse_ab
//! ```

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

const FIXTURES: &[&str] = &[
    "dxt1_64x64.dds",
    "dxt5_64x64.dds",
    "dxt1_64x64_mips.dds",
    "bc1_64x64_dx10.dds",
    "bc3_64x64_dx10.dds",
    "rgba8_64x64.dds",
    "rgba8_256x256_mips.dds",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = fixtures_dir();
    FIXTURES
        .iter()
        .map(|name| {
            let path = dir.join(name);
            let bytes = fs::read(&path).unwrap_or_else(|e| {
                panic!(
                    "missing fixture {}: {e}\nRun: cargo run --example gen_fixtures",
                    path.display()
                )
            });
            ((*name).to_string(), bytes)
        })
        .collect()
}

fn bench_parse_ab(c: &mut Criterion) {
    let fixtures = load_fixtures();

    let mut group = c.benchmark_group("dds_parse");
    for (name, bytes) in &fixtures {
        group.throughput(Throughput::Bytes(bytes.len() as u64));

        group.bench_with_input(
            BenchmarkId::new("rusty_dds", name),
            bytes,
            |b, bytes| {
                b.iter(|| {
                    let dds = rusty_dds::Dds::read(Cursor::new(bytes.as_slice()))
                        .expect("rusty_dds parse");
                    black_box(dds.data.len())
                })
            },
        );

        group.bench_with_input(BenchmarkId::new("ddsfile", name), bytes, |b, bytes| {
            b.iter(|| {
                let dds = ddsfile::Dds::read(Cursor::new(bytes.as_slice())).expect("ddsfile parse");
                black_box(dds.data.len())
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_parse_ab);
criterion_main!(benches);
