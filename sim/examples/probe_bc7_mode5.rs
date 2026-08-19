//! Is the mode-5 fast path actually faster per block?
//!
//! In a real pack mode 5 is ~9% of blocks, so even a large per-block win lands
//! under the noise of a whole-surface measurement. That is an argument about
//! *share*, not about the code. This builds a surface where every block is
//! mode 5 and measures the path directly, so the two questions stay separate.

use std::time::Instant;

use rusty_dds::{AlphaMode, D3D10ResourceDimension, Dds, DxgiFormat, DdsView, NewDxgiParams, SubresourceId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for &side in &[128u32, 256, 512] {
        let blocks = ((side / 4) * (side / 4)) as usize;
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

        // Every block mode 5, rotations cycled so no branch predictor gets a
        // free ride the real content would not give it.
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for (i, blk) in dds.data.chunks_exact_mut(16).enumerate() {
            blk[..8].copy_from_slice(&next().to_le_bytes());
            blk[8..].copy_from_slice(&next().to_le_bytes());
            blk[0] = 0x20 | (((i % 4) as u8) << 6);
        }

        let mut bytes = Vec::new();
        dds.write(&mut bytes)?;
        let view = DdsView::parse(&bytes)?;
        let id = SubresourceId::mip_layer(0, 0);
        let mut buf = Vec::new();
        view.decode_rgba8_into(id, &mut buf)?;

        let iters = if side >= 512 { 100 } else { 600 };
        let t0 = Instant::now();
        for _ in 0..iters {
            view.decode_rgba8_into(id, &mut buf)?;
        }
        let ms = t0.elapsed().as_secs_f64() * 1e3 / iters as f64;
        let px = (side as f64) * (side as f64);
        println!(
            "all-mode-5 {side}x{side}  {blocks:>6} blocks  {ms:8.4} ms  {:7.1} Mpx/s",
            (px / 1e6) / (ms / 1e3)
        );
    }
    Ok(())
}
