//! Pure ThumbHash encoder for the `format=thumbhash` output mode.
//!
//! This module ports `rgbaToThumbHash` from `thumbhash@0.1.1` (MIT, evanw/thumbhash).
//! It has no I/O and no dependency on libvips.
//! It uses f64 in the reference operation order and calls `libm::cos`,
//! so the hash bytes do not depend on the platform math library (INV-16).

use std::f64::consts::PI;

use thiserror::Error;

/// Largest width or height that the encoder accepts.
pub const MAX_SIDE: u32 = 100;

/// Why the ThumbHash path rejected its input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ThumbhashError {
    #[error("thumbhash input {width}x{height} is outside 1..={MAX_SIDE} on a side")]
    InvalidDimensions { width: u32, height: u32 },
    #[error("thumbhash buffer has {actual} bytes, expected {expected}")]
    BufferLengthMismatch { expected: usize, actual: usize },
    #[error("thumbhash input has {0} bands, expected 1 to 4")]
    UnsupportedBands(u32),
}

/// Encodes 8-bit RGBA pixels (not premultiplied) into a ThumbHash of 17 to 25 bytes.
///
/// # Errors
///
/// Returns an error unless `1 <= width <= 100`, `1 <= height <= 100`,
/// and `rgba.len() == 4 * width * height`.
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, ThumbhashError> {
    if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
        return Err(ThumbhashError::InvalidDimensions { width, height });
    }
    let (w, h) = (width as usize, height as usize);
    let pixel_count = w * h;
    if rgba.len() != 4 * pixel_count {
        return Err(ThumbhashError::BufferLengthMismatch {
            expected: 4 * pixel_count,
            actual: rgba.len(),
        });
    }

    let mut avg_r = 0.0_f64;
    let mut avg_g = 0.0_f64;
    let mut avg_b = 0.0_f64;
    let mut avg_a = 0.0_f64;
    for px in rgba.as_chunks::<4>().0 {
        let alpha = f64::from(px[3]) / 255.0;
        avg_r += alpha / 255.0 * f64::from(px[0]);
        avg_g += alpha / 255.0 * f64::from(px[1]);
        avg_b += alpha / 255.0 * f64::from(px[2]);
        avg_a += alpha;
    }
    if avg_a != 0.0 {
        avg_r /= avg_a;
        avg_g /= avg_a;
        avg_b /= avg_a;
    }

    let has_alpha = avg_a < pixel_count as f64;
    let l_limit = if has_alpha { 5.0 } else { 7.0 };
    let longest = f64::from(width.max(height));
    let lx = round_to_u32(l_limit * f64::from(width) / longest).max(1);
    let ly = round_to_u32(l_limit * f64::from(height) / longest).max(1);

    let mut l = Vec::with_capacity(pixel_count);
    let mut p = Vec::with_capacity(pixel_count);
    let mut q = Vec::with_capacity(pixel_count);
    let mut a = Vec::with_capacity(pixel_count);
    for px in rgba.as_chunks::<4>().0 {
        let alpha = f64::from(px[3]) / 255.0;
        let r = avg_r * (1.0 - alpha) + alpha / 255.0 * f64::from(px[0]);
        let g = avg_g * (1.0 - alpha) + alpha / 255.0 * f64::from(px[1]);
        let b = avg_b * (1.0 - alpha) + alpha / 255.0 * f64::from(px[2]);
        l.push((r + g + b) / 3.0);
        p.push((r + g) / 2.0 - b);
        q.push(r - g);
        a.push(alpha);
    }

    let l_channel = encode_channel(&l, w, h, lx.max(3), ly.max(3));
    let p_channel = encode_channel(&p, w, h, 3, 3);
    let q_channel = encode_channel(&q, w, h, 3, 3);
    let a_channel = has_alpha.then(|| encode_channel(&a, w, h, 5, 5));

    let is_landscape = width > height;
    let header24 = round_to_u32(63.0 * l_channel.dc)
        | (round_to_u32(31.5 + 31.5 * p_channel.dc) << 6)
        | (round_to_u32(31.5 + 31.5 * q_channel.dc) << 12)
        | (round_to_u32(31.0 * l_channel.scale) << 18)
        | (u32::from(has_alpha) << 23);
    let header16 = (if is_landscape { ly } else { lx })
        | (round_to_u32(63.0 * p_channel.scale) << 3)
        | (round_to_u32(63.0 * q_channel.scale) << 9)
        | (u32::from(is_landscape) << 15);

    let mut hash = Vec::with_capacity(25);
    hash.extend_from_slice(&[
        (header24 & 255) as u8,
        ((header24 >> 8) & 255) as u8,
        (header24 >> 16) as u8,
        (header16 & 255) as u8,
        (header16 >> 8) as u8,
    ]);
    if let Some(alpha_channel) = &a_channel {
        hash.push(
            round_to_u32(15.0 * alpha_channel.dc) as u8
                | ((round_to_u32(15.0 * alpha_channel.scale) as u8) << 4),
        );
    }

    let ac_start = hash.len();
    let mut ac_index = 0;
    let channels = [&l_channel, &p_channel, &q_channel]
        .into_iter()
        .chain(a_channel.as_ref());
    for channel in channels {
        for &f in &channel.ac {
            let position = ac_start + (ac_index >> 1);
            if position >= hash.len() {
                hash.push(0);
            }
            hash[position] |= (round_to_u32(15.0 * f) as u8) << ((ac_index & 1) << 2);
            ac_index += 1;
        }
    }
    Ok(hash)
}

/// Expands 8-bit samples with 1 to 4 bands into RGBA.
///
/// One band becomes gray with opaque alpha. Two bands become gray with alpha.
/// Three bands get an opaque alpha. Four bands pass through unchanged.
///
/// # Errors
///
/// Returns an error unless `bands` is in 1..=4 and `bytes.len() == width * height * bands`.
/// A 16-bit or float buffer therefore fails closed.
pub fn rgba_from_bands(
    bytes: &[u8],
    width: u32,
    height: u32,
    bands: u32,
) -> Result<Vec<u8>, ThumbhashError> {
    if !(1..=4).contains(&bands) {
        return Err(ThumbhashError::UnsupportedBands(bands));
    }
    let pixel_count = u64::from(width) * u64::from(height);
    let expected = pixel_count
        .checked_mul(u64::from(bands))
        .and_then(|len| usize::try_from(len).ok());
    let Some(expected) = expected.filter(|&len| len == bytes.len()) else {
        return Err(ThumbhashError::BufferLengthMismatch {
            expected: expected.unwrap_or(usize::MAX),
            actual: bytes.len(),
        });
    };

    let bands = bands as usize;
    if bands == 4 {
        return Ok(bytes.to_vec());
    }
    let mut rgba = Vec::with_capacity(expected / bands * 4);
    for sample in bytes.chunks_exact(bands) {
        match *sample {
            [gray] => rgba.extend_from_slice(&[gray, gray, gray, 255]),
            [gray, alpha] => rgba.extend_from_slice(&[gray, gray, gray, alpha]),
            [r, g, b] => rgba.extend_from_slice(&[r, g, b, 255]),
            _ => unreachable!("bands is 1..=3 here"),
        }
    }
    Ok(rgba)
}

/// Encodes bytes as standard-alphabet base64 (RFC 4648) with `=` padding.
pub fn to_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let group = (b0 << 16) | (b1 << 8) | b2;
        out.push(char::from(ALPHABET[(group >> 18) as usize & 63]));
        out.push(char::from(ALPHABET[(group >> 12) as usize & 63]));
        out.push(if chunk.len() > 1 {
            char::from(ALPHABET[(group >> 6) as usize & 63])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(ALPHABET[group as usize & 63])
        } else {
            '='
        });
    }
    out
}

/// One encoded channel: the DC term, the AC terms, and the AC scale.
struct Channel {
    dc: f64,
    ac: Vec<f64>,
    scale: f64,
}

// Inputs are non-negative up to float error, so `round` equals JS `Math.round`.
fn round_to_u32(value: f64) -> u32 {
    value.round() as u32
}

fn encode_channel(channel: &[f64], w: usize, h: usize, nx: u32, ny: u32) -> Channel {
    let mut dc = 0.0_f64;
    let mut ac = Vec::new();
    let mut scale = 0.0_f64;
    let mut fx = vec![0.0_f64; w];
    for cy in 0..ny {
        let mut cx = 0;
        while cx * ny < nx * (ny - cy) {
            let mut f = 0.0_f64;
            for (x, slot) in fx.iter_mut().enumerate() {
                *slot = libm::cos(PI / w as f64 * f64::from(cx) * (x as f64 + 0.5));
            }
            for y in 0..h {
                let fy = libm::cos(PI / h as f64 * f64::from(cy) * (y as f64 + 0.5));
                for (x, &weight) in fx.iter().enumerate() {
                    f += channel[x + y * w] * weight * fy;
                }
            }
            f /= (w * h) as f64;
            if cx > 0 || cy > 0 {
                ac.push(f);
                scale = scale.max(f.abs());
            } else {
                dc = f;
            }
            cx += 1;
        }
    }
    if scale != 0.0 {
        for value in &mut ac {
            *value = 0.5 + 0.5 / scale * *value;
        }
    }
    Channel { dc, ac, scale }
}
