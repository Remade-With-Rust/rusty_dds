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

pub fn decode_rgba8_into(
    data: &[u8],
    width: u32,
    height: u32,
    out: &mut [u8],
) -> Result<(), Error> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    if out.len() != expected {
        return Err(Error::OutOfBounds);
    }
    out.copy_from_slice(&data[..expected]);
    Ok(())
}

pub fn decode_bgra8(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, Error> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    let mut out = vec![0u8; expected];
    decode_bgra8_into(data, width, height, &mut out)?;
    Ok(out)
}

pub fn decode_bgra8_into(
    data: &[u8],
    width: u32,
    height: u32,
    out: &mut [u8],
) -> Result<(), Error> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::OutOfBounds)?;
    if data.len() < expected {
        return Err(Error::TruncatedData);
    }
    if out.len() != expected {
        return Err(Error::OutOfBounds);
    }
    for (src, dst) in data[..expected].chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]); // BGRA -> RGBA
    }
    Ok(())
}
