//! Per-mode cost of the general BC7 decoder.
//!
//! §21's rule: measure in isolation before writing anything. A specialised path
//! can only recover what the general decoder spends, so the first question for
//! modes 1 and 3 is what mode-1 and mode-3 blocks actually cost — not what
//! share they hold. Mode 6 is included as the calibration point: its fast path
//! is a known ~20%, so it shows what "there is headroom here" looks like.

use std::time::Instant;

use rusty_dds::{
    AlphaMode, D3D10ResourceDimension, Dds, DdsView, DxgiFormat, NewDxgiParams, SubresourceId,
};

/// Force a block to `mode`, leaving every other bit as supplied. BC7's mode is
/// unary: `mode` zero bits, then a one, in the low bits of byte 0.
fn force_mode(blk: &mut [u8], mode: u32) {
    let keep = !((1u16 << (mode + 1)) - 1) as u8;
    blk[0] = (blk[0] & keep) | (1u8 << mode);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Pin before measuring. This box runs 70%+ busy from other processes, and
    // an unpinned probe competes with them for cores — which is why the same
    // change has read +34.5% and +3.5% on consecutive runs. Mask 0x3c is four
    // physical cores; HIGH_PRIORITY_CLASS keeps the scheduler off us.
    let pinned = rusty_dds_sim::os::pin_process(0x3c, true);
    eprintln!("[probe] pinned={pinned} mask=0x3c high_priority=true");

    let side = 256u32; // serial, cache-resident: the decoder is the limit here
    println!("all-mode-N BC7, {side}x{side}, serial, into a recycled buffer\n");
    println!("{:<8} {:>12} {:>10}  {}", "mode", "ms/call", "Mpx/s", "shape");
    let shape = |m: u32| match m {
        0 => "3 subsets, RGB 4.4.4, 3-bit idx, 6 p-bits",
        1 => "2 subsets, RGB 6.6.6, 3-bit idx, 2 shared p-bits",
        2 => "3 subsets, RGB 5.5.5, 2-bit idx, no p-bits",
        3 => "2 subsets, RGB 7.7.7, 2-bit idx, 4 p-bits",
        4 => "1 subset, rotation + index select, RGB 5.5.5 A6",
        5 => "1 subset, rotation, RGB 7.7.7 A8, two index sets",
        6 => "1 subset, RGBA 7.7.7.7, 4-bit idx  <-- has fast path",
        _ => "2 subsets, RGBA 5.5.5.5, 2-bit idx",
    };

    for mode in [0u32, 1, 2, 3, 4, 5, 6, 7] {
        let mut dds = Dds::new_dxgi(NewDxgiParams {
            height: side,
            width: side,
            depth: None,
            format: DxgiFormat::BC7_UNorm,
            mipmap_levels: Some(1),
            array_layers: None,
            caps2: None,
            is_cubemap: false,
            resource_dimension: D3D10ResourceDimension::Texture2D,
            alpha_mode: AlphaMode::Straight,
        })?;
        let mut state = 0x1234_5678_9abc_def0u64 ^ (mode as u64) << 32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for blk in dds.data.chunks_exact_mut(16) {
            blk[..8].copy_from_slice(&next().to_le_bytes());
            blk[8..].copy_from_slice(&next().to_le_bytes());
            force_mode(blk, mode);
        }

        let mut bytes = Vec::new();
        dds.write(&mut bytes)?;
        let view = DdsView::parse(&bytes)?;
        let id = SubresourceId::mip_layer(0, 0);
        let mut buf = Vec::new();
        view.decode_rgba8_into(id, &mut buf)?;

        let iters = 400;
        let t0 = Instant::now();
        for _ in 0..iters {
            view.decode_rgba8_into(id, &mut buf)?;
        }
        let ms = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;
        let px = (side as f64) * (side as f64);
        println!(
            "{mode:<8} {ms:>12.4} {:>10.1}  {}",
            (px / 1e6) / (ms / 1e3),
            shape(mode)
        );
    }
    Ok(())
}
