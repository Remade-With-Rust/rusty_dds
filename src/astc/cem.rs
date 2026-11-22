//! Colour endpoint modes ("Endpoint Interpretation", "LDR Endpoint Decoding",
//! "HDR Endpoint Decoding"), and the per-profile expansion of the decoded
//! endpoints into the 16-bit interpolation domain.

use super::AstcProfile;

/// Endpoints as the endpoint mode decodes them: LDR components are 8-bit
/// (0..=255), HDR components are 12-bit (0..=0xFFF).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEndpoints {
    pub e0: [i32; 4],
    pub e1: [i32; 4],
    /// RGB are HDR (12-bit pseudo-logarithmic).
    pub rgb_hdr: bool,
    /// Alpha is HDR (12-bit). Only mode 15 sets it.
    pub alpha_hdr: bool,
    /// Alpha was not coded (HDR modes 2, 3, 7, 11): the profile picks 1.0.
    pub alpha_default: bool,
}

/// Endpoints expanded to the 16-bit interpolation domain, with the per-lane
/// flag saying which lanes go through the HDR (LNS) output conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoints {
    pub e0: [u16; 4],
    pub e1: [u16; 4],
    pub lns: [bool; 4],
    /// The endpoint mode was HDR and the profile is LDR: the partition
    /// decodes to the error colour (magenta).
    pub hdr_in_ldr: bool,
}

/// Number of endpoint integers a CEM consumes.
pub const fn value_count(cem: u8) -> usize {
    2 * ((cem as usize >> 2) + 1)
}

fn bit_transfer_signed(a: i32, b: i32) -> (i32, i32) {
    let b = (b >> 1) | (a & 0x80);
    let mut a = (a >> 1) & 0x3F;
    if a & 0x20 != 0 {
        a -= 0x40;
    }
    (a, b)
}

fn blue_contract(r: i32, g: i32, b: i32, a: i32) -> [i32; 4] {
    [(r + b) >> 1, (g + b) >> 1, b, a]
}

fn clamp8(c: [i32; 4]) -> [i32; 4] {
    [
        c[0].clamp(0, 255),
        c[1].clamp(0, 255),
        c[2].clamp(0, 255),
        c[3].clamp(0, 255),
    ]
}

fn ldr(e0: [i32; 4], e1: [i32; 4]) -> RawEndpoints {
    RawEndpoints {
        e0,
        e1,
        rgb_hdr: false,
        alpha_hdr: false,
        alpha_default: false,
    }
}

fn hdr_rgb(e0: [i32; 3], e1: [i32; 3]) -> RawEndpoints {
    RawEndpoints {
        e0: [e0[0], e0[1], e0[2], 0x780],
        e1: [e1[0], e1[1], e1[2], 0x780],
        rgb_hdr: true,
        alpha_hdr: false,
        alpha_default: true,
    }
}

/// HDR mode 7: RGB base + scale.
fn hdr_mode7(v: &[i32]) -> ([i32; 3], [i32; 3]) {
    let (majcomp, mode, [mut red, mut green, mut blue, mut scale]) = mode7_fields(v);
    const SHAMTS: [i32; 6] = [1, 1, 2, 3, 4, 5];
    let shamt = SHAMTS[mode as usize];
    red <<= shamt;
    green <<= shamt;
    blue <<= shamt;
    scale <<= shamt;
    if mode != 5 {
        green = red - green;
        blue = red - blue;
    }
    if majcomp == 1 {
        core::mem::swap(&mut red, &mut green);
    }
    if majcomp == 2 {
        core::mem::swap(&mut red, &mut blue);
    }
    let e1 = [
        red.clamp(0, 0xFFF),
        green.clamp(0, 0xFFF),
        blue.clamp(0, 0xFFF),
    ];
    let e0 = [
        (red - scale).clamp(0, 0xFFF),
        (green - scale).clamp(0, 0xFFF),
        (blue - scale).clamp(0, 0xFFF),
    ];
    (e0, e1)
}

/// Mode 7's bit fields before the shift: (major component, sub-mode,
/// [red, green, blue, scale]). A bit permutation of the four values for a
/// given sub-mode, which is what lets the encoder invert it.
pub(crate) fn mode7_fields(v: &[i32]) -> (i32, i32, [i32; 4]) {
    let modeval = ((v[0] & 0xC0) >> 6) | ((v[1] & 0x80) >> 5) | ((v[2] & 0x80) >> 4);
    let (majcomp, mode) = if modeval & 0xC != 0xC {
        (modeval >> 2, modeval & 3)
    } else if modeval != 0xF {
        (modeval & 3, 4)
    } else {
        (0, 5)
    };
    let mut red = v[0] & 0x3F;
    let mut green = v[1] & 0x1F;
    let mut blue = v[2] & 0x1F;
    let mut scale = v[3] & 0x1F;
    let x0 = (v[1] >> 6) & 1;
    let x1 = (v[1] >> 5) & 1;
    let x2 = (v[2] >> 6) & 1;
    let x3 = (v[2] >> 5) & 1;
    let x4 = (v[3] >> 7) & 1;
    let x5 = (v[3] >> 6) & 1;
    let x6 = (v[3] >> 5) & 1;
    let ohm = 1 << mode;
    if ohm & 0x30 != 0 {
        green |= x0 << 6;
    }
    if ohm & 0x3A != 0 {
        green |= x1 << 5;
    }
    if ohm & 0x30 != 0 {
        blue |= x2 << 6;
    }
    if ohm & 0x3A != 0 {
        blue |= x3 << 5;
    }
    if ohm & 0x3D != 0 {
        scale |= x6 << 5;
    }
    if ohm & 0x2D != 0 {
        scale |= x5 << 6;
    }
    if ohm & 0x04 != 0 {
        scale |= x4 << 7;
    }
    if ohm & 0x3B != 0 {
        red |= x4 << 6;
    }
    if ohm & 0x04 != 0 {
        red |= x3 << 6;
    }
    if ohm & 0x10 != 0 {
        red |= x5 << 7;
    }
    if ohm & 0x0F != 0 {
        red |= x2 << 7;
    }
    if ohm & 0x05 != 0 {
        red |= x1 << 8;
    }
    if ohm & 0x0A != 0 {
        red |= x0 << 8;
    }
    if ohm & 0x05 != 0 {
        red |= x0 << 9;
    }
    if ohm & 0x02 != 0 {
        red |= x6 << 9;
    }
    if ohm & 0x01 != 0 {
        red |= x3 << 10;
    }
    if ohm & 0x02 != 0 {
        red |= x5 << 10;
    }
    (majcomp, mode, [red, green, blue, scale])
}

fn sign_extend(v: i32, bits: i32) -> i32 {
    let shift = 32 - bits;
    (v << shift) >> shift
}

/// HDR mode 11: two RGB endpoints (also the RGB half of modes 14 and 15).
fn hdr_mode11(v: &[i32]) -> ([i32; 3], [i32; 3]) {
    let majcomp = ((v[4] & 0x80) >> 7) | ((v[5] & 0x80) >> 6);
    if majcomp == 3 {
        return (
            [v[0] << 4, v[2] << 4, (v[4] & 0x7F) << 5],
            [v[1] << 4, v[3] << 4, (v[5] & 0x7F) << 5],
        );
    }
    let mode = ((v[1] & 0x80) >> 7) | ((v[2] & 0x80) >> 6) | ((v[3] & 0x80) >> 5);
    let mut va = v[0] | ((v[1] & 0x40) << 2);
    let mut vb0 = v[2] & 0x3F;
    let mut vb1 = v[3] & 0x3F;
    let mut vc = v[1] & 0x3F;
    const DBITS: [i32; 8] = [7, 6, 7, 6, 5, 6, 5, 6];
    let mut vd0 = sign_extend(v[4] & 0x7F, DBITS[mode as usize]);
    let mut vd1 = sign_extend(v[5] & 0x7F, DBITS[mode as usize]);
    let x0 = (v[2] >> 6) & 1;
    let x1 = (v[3] >> 6) & 1;
    let x2 = (v[4] >> 6) & 1;
    let x3 = (v[5] >> 6) & 1;
    let x4 = (v[4] >> 5) & 1;
    let x5 = (v[5] >> 5) & 1;
    let ohm = 1 << mode;
    if ohm & 0xA4 != 0 {
        va |= x0 << 9;
    }
    if ohm & 0x08 != 0 {
        va |= x2 << 9;
    }
    if ohm & 0x50 != 0 {
        va |= x4 << 9;
    }
    if ohm & 0x50 != 0 {
        va |= x5 << 10;
    }
    if ohm & 0xA0 != 0 {
        va |= x1 << 10;
    }
    if ohm & 0xC0 != 0 {
        va |= x2 << 11;
    }
    if ohm & 0x04 != 0 {
        vc |= x1 << 6;
    }
    if ohm & 0xE8 != 0 {
        vc |= x3 << 6;
    }
    if ohm & 0x20 != 0 {
        vc |= x2 << 7;
    }
    if ohm & 0x5B != 0 {
        vb0 |= x0 << 6;
    }
    if ohm & 0x5B != 0 {
        vb1 |= x1 << 6;
    }
    if ohm & 0x12 != 0 {
        vb0 |= x2 << 7;
    }
    if ohm & 0x12 != 0 {
        vb1 |= x3 << 7;
    }
    // The specification's listing writes `modeval` here; the variable in
    // scope is `mode` (the three mode bits).
    let shamt = (mode >> 1) ^ 3;
    va <<= shamt;
    vb0 <<= shamt;
    vb1 <<= shamt;
    vc <<= shamt;
    vd0 <<= shamt;
    vd1 <<= shamt;
    let mut e1 = [
        va.clamp(0, 0xFFF),
        (va - vb0).clamp(0, 0xFFF),
        (va - vb1).clamp(0, 0xFFF),
    ];
    let mut e0 = [
        (va - vc).clamp(0, 0xFFF),
        (va - vb0 - vc - vd0).clamp(0, 0xFFF),
        (va - vb1 - vc - vd1).clamp(0, 0xFFF),
    ];
    if majcomp == 1 {
        e0.swap(0, 1);
        e1.swap(0, 1);
    } else if majcomp == 2 {
        e0.swap(0, 2);
        e1.swap(0, 2);
    }
    (e0, e1)
}

/// Decode endpoint mode `cem` from its unquantised values (0..=255 each;
/// `v.len() >= value_count(cem)`).
pub fn decode(cem: u8, v: &[u8]) -> RawEndpoints {
    let mut w = [0i32; 8];
    for (d, s) in w.iter_mut().zip(v.iter()) {
        *d = *s as i32;
    }
    let v = &w;
    match cem & 15 {
        0 => ldr([v[0], v[0], v[0], 0xFF], [v[1], v[1], v[1], 0xFF]),
        1 => {
            let l0 = (v[0] >> 2) | (v[1] & 0xC0);
            let l1 = (l0 + (v[1] & 0x3F)).min(0xFF);
            ldr([l0, l0, l0, 0xFF], [l1, l1, l1, 0xFF])
        }
        2 => {
            let (y0, y1) = if v[1] >= v[0] {
                (v[0] << 4, v[1] << 4)
            } else {
                ((v[1] << 4) + 8, (v[0] << 4) - 8)
            };
            hdr_rgb([y0; 3], [y1; 3])
        }
        3 => {
            let (y0, d) = if v[0] & 0x80 != 0 {
                (
                    ((v[1] & 0xE0) << 4) | ((v[0] & 0x7F) << 2),
                    (v[1] & 0x1F) << 2,
                )
            } else {
                (
                    ((v[1] & 0xF0) << 4) | ((v[0] & 0x7F) << 1),
                    (v[1] & 0x0F) << 1,
                )
            };
            let y1 = (y0 + d).min(0xFFF);
            hdr_rgb([y0; 3], [y1; 3])
        }
        4 => ldr([v[0], v[0], v[0], v[2]], [v[1], v[1], v[1], v[3]]),
        5 => {
            let (a1, b0) = bit_transfer_signed(v[1], v[0]);
            let (a3, b2) = bit_transfer_signed(v[3], v[2]);
            ldr(
                clamp8([b0, b0, b0, b2]),
                clamp8([b0 + a1, b0 + a1, b0 + a1, b2 + a3]),
            )
        }
        6 => ldr(
            [
                (v[0] * v[3]) >> 8,
                (v[1] * v[3]) >> 8,
                (v[2] * v[3]) >> 8,
                0xFF,
            ],
            [v[0], v[1], v[2], 0xFF],
        ),
        7 => {
            let (e0, e1) = hdr_mode7(v);
            hdr_rgb(e0, e1)
        }
        8 => {
            let s0 = v[0] + v[2] + v[4];
            let s1 = v[1] + v[3] + v[5];
            if s1 >= s0 {
                ldr([v[0], v[2], v[4], 0xFF], [v[1], v[3], v[5], 0xFF])
            } else {
                ldr(
                    blue_contract(v[1], v[3], v[5], 0xFF),
                    blue_contract(v[0], v[2], v[4], 0xFF),
                )
            }
        }
        9 => {
            let (a1, b0) = bit_transfer_signed(v[1], v[0]);
            let (a3, b2) = bit_transfer_signed(v[3], v[2]);
            let (a5, b4) = bit_transfer_signed(v[5], v[4]);
            if a1 + a3 + a5 >= 0 {
                ldr(
                    clamp8([b0, b2, b4, 0xFF]),
                    clamp8([b0 + a1, b2 + a3, b4 + a5, 0xFF]),
                )
            } else {
                ldr(
                    clamp8(blue_contract(b0 + a1, b2 + a3, b4 + a5, 0xFF)),
                    clamp8(blue_contract(b0, b2, b4, 0xFF)),
                )
            }
        }
        10 => ldr(
            [
                (v[0] * v[3]) >> 8,
                (v[1] * v[3]) >> 8,
                (v[2] * v[3]) >> 8,
                v[4],
            ],
            [v[0], v[1], v[2], v[5]],
        ),
        11 => {
            let (e0, e1) = hdr_mode11(v);
            hdr_rgb(e0, e1)
        }
        12 => {
            let s0 = v[0] + v[2] + v[4];
            let s1 = v[1] + v[3] + v[5];
            if s1 >= s0 {
                ldr([v[0], v[2], v[4], v[6]], [v[1], v[3], v[5], v[7]])
            } else {
                ldr(
                    blue_contract(v[1], v[3], v[5], v[7]),
                    blue_contract(v[0], v[2], v[4], v[6]),
                )
            }
        }
        13 => {
            let (a1, b0) = bit_transfer_signed(v[1], v[0]);
            let (a3, b2) = bit_transfer_signed(v[3], v[2]);
            let (a5, b4) = bit_transfer_signed(v[5], v[4]);
            let (a7, b6) = bit_transfer_signed(v[7], v[6]);
            if a1 + a3 + a5 >= 0 {
                ldr(
                    clamp8([b0, b2, b4, b6]),
                    clamp8([b0 + a1, b2 + a3, b4 + a5, b6 + a7]),
                )
            } else {
                ldr(
                    clamp8(blue_contract(b0 + a1, b2 + a3, b4 + a5, b6 + a7)),
                    clamp8(blue_contract(b0, b2, b4, b6)),
                )
            }
        }
        14 => {
            let (e0, e1) = hdr_mode11(v);
            RawEndpoints {
                e0: [e0[0], e0[1], e0[2], v[6]],
                e1: [e1[0], e1[1], e1[2], v[7]],
                rgb_hdr: true,
                alpha_hdr: false,
                alpha_default: false,
            }
        }
        _ => {
            let (e0, e1) = hdr_mode11(v);
            let mode = ((v[6] >> 7) & 1) | ((v[7] >> 6) & 2);
            let mut a0 = v[6] & 0x7F;
            let mut a1 = v[7] & 0x7F;
            if mode == 3 {
                a0 <<= 5;
                a1 <<= 5;
            } else {
                a0 |= (a1 << (mode + 1)) & 0x780;
                a1 &= 0x3F >> mode;
                a1 ^= 0x20 >> mode;
                a1 -= 0x20 >> mode;
                a0 <<= 4 - mode;
                a1 <<= 4 - mode;
                a1 = (a1 + a0).clamp(0, 0xFFF);
            }
            RawEndpoints {
                e0: [e0[0], e0[1], e0[2], a0],
                e1: [e1[0], e1[1], e1[2], a1],
                rgb_hdr: true,
                alpha_hdr: true,
                alpha_default: false,
            }
        }
    }
}

/// Expand decoded endpoints to the 16-bit interpolation domain for `profile`.
///
/// - LDR profiles: an HDR endpoint mode becomes the magenta error endpoints
///   for this partition only. Linear LDR expands by replication
///   (`c * 257`); sRGB expands every component, alpha included, as
///   `(c << 8) | 0x80`.
/// - HDR profiles: HDR components shift left 4 (12 to 16 bits), LDR ones
///   replicate. An HDR mode without coded alpha gets 1.0: as HDR `0x780` in
///   the `Hdr` profile, as LDR `0xFF` in `HdrRgbLdrA` (astcenc's choice;
///   both decode to 1.0).
pub fn expand(raw: &RawEndpoints, profile: AstcProfile) -> Endpoints {
    match profile {
        AstcProfile::Ldr | AstcProfile::LdrSrgb => {
            let hdr = raw.rgb_hdr || raw.alpha_hdr;
            let (e0, e1) = if hdr {
                ([0xFF, 0, 0xFF, 0xFF], [0xFF, 0, 0xFF, 0xFF])
            } else {
                (raw.e0, raw.e1)
            };
            let x = |c: i32| -> u16 {
                let c = c as u16 & 0xFF;
                if profile == AstcProfile::LdrSrgb {
                    (c << 8) | 0x80
                } else {
                    c * 257
                }
            };
            Endpoints {
                e0: [x(e0[0]), x(e0[1]), x(e0[2]), x(e0[3])],
                e1: [x(e1[0]), x(e1[1]), x(e1[2]), x(e1[3])],
                lns: [false; 4],
                hdr_in_ldr: hdr,
            }
        }
        _ => {
            let mut e0 = raw.e0;
            let mut e1 = raw.e1;
            let mut alpha_hdr = raw.alpha_hdr;
            if raw.alpha_default {
                if profile == AstcProfile::Hdr {
                    e0[3] = 0x780;
                    e1[3] = 0x780;
                    alpha_hdr = true;
                } else {
                    e0[3] = 0xFF;
                    e1[3] = 0xFF;
                    alpha_hdr = false;
                }
            }
            let lns = [raw.rgb_hdr, raw.rgb_hdr, raw.rgb_hdr, alpha_hdr];
            let mut o0 = [0u16; 4];
            let mut o1 = [0u16; 4];
            for c in 0..4 {
                if lns[c] {
                    o0[c] = ((e0[c] & 0xFFF) << 4) as u16;
                    o1[c] = ((e1[c] & 0xFFF) << 4) as u16;
                } else {
                    o0[c] = (e0[c] & 0xFF) as u16 * 257;
                    o1[c] = (e1[c] & 0xFF) as u16 * 257;
                }
            }
            Endpoints {
                e0: o0,
                e1: o1,
                lns,
                hdr_in_ldr: false,
            }
        }
    }
}
