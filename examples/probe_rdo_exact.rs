//! Probe: which bumpSign BC7 blocks change under RDO, and what was their
//! baseline error according to the same oracle the guard uses?

use rusty_dds::*;
use std::io::BufReader;

fn main() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let p = root.join("corpus/raw_crytif/bumpSign.tif");
    let f = std::fs::File::open(&p).expect("tif");
    let mut dec = tiff::decoder::Decoder::new(BufReader::new(f)).unwrap();
    let (w, h) = dec.dimensions().unwrap();
    let img = dec.read_image().unwrap();
    let rgba: Vec<u8> = match img {
        tiff::decoder::DecodingResult::U8(v) => v,
        _ => panic!("expected u8"),
    };
    assert_eq!(rgba.len(), (w * h * 4) as usize);

    let base = Dds::encode_from_rgba8(
        &rgba,
        EncodeLayout::flat_2d(DecodeContent::Bc7, w, h).with_rdo(Rdo::Off),
    )
    .unwrap();
    let rdo = Dds::encode_from_rgba8(
        &rgba,
        EncodeLayout::flat_2d(DecodeContent::Bc7, w, h).with_rdo(Rdo::lambda(4.0)),
    )
    .unwrap();

    let bw = (w as usize + 3) / 4;
    let mut diff = 0;
    let mut shown = 0;
    for bi in 0..base.data.len() / 16 {
        let a = &base.data[bi * 16..bi * 16 + 16];
        let b = &rdo.data[bi * 16..bi * 16 + 16];
        if a != b {
            diff += 1;
            if shown < 5 {
                // baseline error via the decode oracle, exactly as the guard sees it
                let mut deca = [0u8; 64];
                let mut decb = [0u8; 64];
                bcdec_rs::bc7(a, &mut deca, 16);
                bcdec_rs::bc7(b, &mut decb, 16);
                let (bx, by) = (bi % bw, bi / bw);
                let mut ea = 0i64;
                let mut eb = 0i64;
                for row in 0..4 {
                    for col in 0..4 {
                        let x = (bx * 4 + col).min(w as usize - 1);
                        let y = (by * 4 + row).min(h as usize - 1);
                        let s = (y * w as usize + x) * 4;
                        for c in 0..4 {
                            let src = rgba[s + c] as i64;
                            let da = deca[(row * 4 + col) * 4 + c] as i64 - src;
                            let db = decb[(row * 4 + col) * 4 + c] as i64 - src;
                            ea += da * da;
                            eb += db * db;
                        }
                    }
                }
                println!(
                    "block {bi} ({bx},{by}): base mode-byte {:#04x} err={ea}  rdo mode-byte {:#04x} err={eb}",
                    a[0], b[0]
                );
                shown += 1;
            }
        }
    }
    println!("blocks differing: {diff} / {}", base.data.len() / 16);
}
