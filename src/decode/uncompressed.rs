//! Uncompressed surface → RGBA8.

use crate::error::Error;

pub fn decode_rgba8(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    Ok(data[..expected].to_vec())
}

pub fn decode_bgra8(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let mut out = Vec::with_capacity(expected);
    for px in data[..expected].chunks_exact(4) {
        out.extend_from_slice(&[px[2], px[1], px[0], px[3]]); // BGRA → RGBA
    }
    Ok(out)
}
