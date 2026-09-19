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
    for px in rgba.chunks_exact(4) {
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
    for px in rgba.chunks_exact(4) {
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

/// Helpers that the tests of `pipeline.rs` and `server.rs` share with this module.
#[cfg(test)]
pub(crate) mod test_support {
    use std::ops::RangeInclusive;

    /// Byte lengths that `encode` returns (INV-18).
    pub const HASH_BYTES: RangeInclusive<usize> = 17..=25;
    /// Base64 body lengths that follow from `HASH_BYTES` (INV-18).
    pub const BODY_CHARS: RangeInclusive<usize> = 24..=36;

    fn sextet(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    /// True for standard-alphabet base64 (RFC 4648) with correct `=` padding.
    pub fn is_standard_base64(text: &str) -> bool {
        let body = text.trim_end_matches('=');
        text.len().is_multiple_of(4)
            && text.len() - body.len() <= 2
            && body.bytes().all(|c| sextet(c).is_some())
    }

    /// Decodes standard-alphabet base64 and panics on any other byte.
    pub fn base64_decode(text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut accumulator = 0u32;
        let mut bits = 0;
        for c in text.bytes().take_while(|&c| c != b'=') {
            let value = sextet(c).unwrap_or_else(|| panic!("byte {c} is not base64"));
            accumulator = (accumulator << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((accumulator >> bits) as u8);
                accumulator &= (1 << bits) - 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{HASH_BYTES, base64_decode, is_standard_base64};
    use super::*;

    const GOLDEN_SIZES: [(u32, u32); 16] = [
        (1, 1),
        (1, 100),
        (100, 1),
        (2, 7),
        (3, 3),
        (7, 2),
        (10, 10),
        (16, 9),
        (9, 16),
        (33, 17),
        (64, 64),
        (99, 100),
        (100, 99),
        (100, 100),
        (100, 56),
        (56, 100),
    ];

    const GOLDEN_MODES: [&str; 3] = ["opaque", "alpha", "smooth"];

    /// Reference hashes from `thumbhash@0.1.1`, identical under Bun and Node.
    /// Order follows the generator: sizes outer, modes inner.
    const GOLDEN_HASHES: [(&str, &str); 48] = [
        (
            "opaque_1x1",
            "1edc3e0f3708f708888788708f7088f80888808008088800",
        ),
        (
            "alpha_1x1",
            "65ecc925074408f78887707ff80888707ff808f78887707ff8",
        ),
        (
            "smooth_1x1",
            "0df419ff0208f708888788708f7088f80888707ff8f7870f",
        ),
        ("opaque_1x100", "21084219020887887888877738898ff658"),
        (
            "alpha_1x100",
            "a1e7c11906880887887878f788779f3808f78788888788",
        ),
        ("smooth_1x100", "9a0535a14008868978878877f78577af78"),
        ("opaque_100x1", "20f8411186877878888808780d88648f88"),
        (
            "alpha_100x1",
            "60f8c1118288788788887026883075f8788788887078f8",
        ),
        ("smooth_100x1", "5af536a1be7687878888088985874f8708"),
        ("opaque_2x7", "e6180642127e864888f638e78f9a88b809"),
        (
            "alpha_2x7",
            "9d198621087776c588a4fb798da0f88888077788797878",
        ),
        ("smooth_2x7", "64f70d522a70838888788777708207d787"),
        (
            "opaque_3x3",
            "e108422f1077880878878878888788787887807fdd508b04",
        ),
        (
            "alpha_3x3",
            "dd3582150825de28ec753f8e07f37af398bba758e98feb8548",
        ),
        (
            "smooth_3x3",
            "a2f745472276880986877887888788887779001977707f08",
        ),
        ("opaque_7x2", "a4d70932923c7fda5ac377777cb0676f8c"),
        (
            "alpha_7x2",
            "e57785198c77fc568864876061879f88788a8877887807",
        ),
        ("smooth_7x2", "64070e52aa73778780887788817027f887"),
        (
            "opaque_10x10",
            "5ef8051f04b883456675a99796866df488f96af852fb8709",
        ),
        (
            "alpha_10x10",
            "e0f7810d0218615021b87397896e434f30aac8a47e99c80c83",
        ),
        (
            "smooth_10x10",
            "64070a3f1c70777880887787787877777887070777708f08",
        ),
        ("opaque_16x9", "60e801148270548e864b717c8d8340896cf507"),
        (
            "alpha_16x9",
            "60e88113821780665788b8f49a928207fba884a8839948",
        ),
        ("smooth_16x9", "64070a3c9c807778708878878887718008f787"),
        ("opaque_9x16", "df17061c02beb5f2da7da51c4796655f69087a"),
        (
            "alpha_9x16",
            "9df8810b04070502b39bba77a90fb548b3647f83996a71",
        ),
        ("smooth_9x16", "64f7093c1c700787778887787878708108f778"),
        ("opaque_33x17", "dff7011482cf55a8938a899769b80676f814df"),
        (
            "alpha_33x17",
            "2008820b82085f5263758bd666cf709b0ac77b57ad59b3",
        ),
        ("smooth_33x17", "64070a3c9a707777707787777877708007f788"),
        (
            "opaque_64x64",
            "1ff801070085a9864c9777746f9899a84959760395482f0a",
        ),
        (
            "alpha_64x64",
            "1ff88105000802b97d98513391e20109ab7879c36ad78fb586",
        ),
        (
            "smooth_64x64",
            "65070a371a70777780888887787878787777070777707f08",
        ),
        (
            "opaque_99x100",
            "1ff80107005859577588a95853f989ba76c9ea6705088409",
        ),
        (
            "alpha_99x100",
            "dff781050007a8a904745fa789fc2e9f50648b0c47b18a7169",
        ),
        (
            "smooth_99x100",
            "65f709371a70777780887787877887788787070777707f08",
        ),
        (
            "opaque_100x99",
            "e0f7010782939837714a78bc474ea7fa849a820f7a808707",
        ),
        (
            "alpha_100x99",
            "20088205800842f275597a2a87b04a6122a06458a164a0c274",
        ),
        (
            "smooth_100x99",
            "65f709379a70777780778887787888788877070777708f08",
        ),
        (
            "opaque_100x100",
            "1f08020700509595b5b699ab749a68b9869b560ba61f810e",
        ),
        (
            "alpha_100x100",
            "e007820500078c8e80036d642a04c1528f8b969fa6c96ddaa6",
        ),
        (
            "smooth_100x100",
            "65f709371a70777780877887887777787787070777707f08",
        ),
        ("opaque_100x56", "e0070204805583827f58c9569756cb600b979b"),
        (
            "alpha_100x56",
            "1ff88103800777a57097b22dfdfb98c44f9ed9a98c3c9a",
        ),
        ("smooth_100x56", "65f709349a707777808787778787707007f788"),
        ("opaque_56x100", "dff701040284725b47b999ca38fb757063f668"),
        (
            "alpha_56x100",
            "1ff881030008ffa7d576569d445fad20b7b675870fd85e",
        ),
        ("smooth_56x100", "65f709341a700787778887787777707007f787"),
    ];

    struct Lcg(u32);

    impl Lcg {
        fn next_byte(&mut self) -> u8 {
            self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12345);
            ((self.0 >> 16) & 0xff) as u8
        }
    }

    fn clamp_to_byte(value: f64) -> u8 {
        (value as i64).clamp(0, 255) as u8
    }

    fn golden_image(rng: &mut Lcg, width: u32, height: u32, mode: &str) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                match mode {
                    "smooth" => {
                        let r = f64::from(x) * 255.0 / f64::from((width - 1).max(1))
                            + f64::from(rng.next_byte()) / 8.0;
                        let g = f64::from(y) * 255.0 / f64::from((height - 1).max(1))
                            + f64::from(rng.next_byte()) / 8.0;
                        let b = 128.0 + f64::from(rng.next_byte()) / 4.0;
                        pixels.extend_from_slice(&[
                            clamp_to_byte(r),
                            clamp_to_byte(g),
                            clamp_to_byte(b),
                            255,
                        ]);
                    }
                    _ => {
                        let r = rng.next_byte();
                        let g = rng.next_byte();
                        let b = rng.next_byte();
                        let a = if mode == "alpha" {
                            rng.next_byte()
                        } else {
                            255
                        };
                        pixels.extend_from_slice(&[r, g, b, a]);
                    }
                }
            }
        }
        pixels
    }

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn flat_image(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
        pixel.repeat((width * height) as usize)
    }

    #[test]
    fn encode_matches_reference_vectors_for_seeded_noise_alpha_and_smooth_images() {
        let mut rng = Lcg(2024);
        let mut index = 0;
        for (width, height) in GOLDEN_SIZES {
            for mode in GOLDEN_MODES {
                let pixels = golden_image(&mut rng, width, height, mode);
                let (name, expected) = GOLDEN_HASHES[index];
                assert_eq!(name, format!("{mode}_{width}x{height}"));
                let hash = encode(width, height, &pixels).expect("golden image encodes");
                assert_eq!(to_hex(&hash), expected, "{name}");
                index += 1;
            }
        }
        assert_eq!(index, GOLDEN_HASHES.len());
    }

    #[test]
    fn encode_is_deterministic_across_repeated_calls() {
        let mut rng = Lcg(7);
        let pixels = golden_image(&mut rng, 33, 17, "alpha");
        let first = encode(33, 17, &pixels).unwrap();
        for _ in 0..3 {
            assert_eq!(encode(33, 17, &pixels).unwrap(), first);
        }
    }

    #[test]
    fn encode_rejects_zero_width() {
        assert_eq!(
            encode(0, 10, &[]),
            Err(ThumbhashError::InvalidDimensions {
                width: 0,
                height: 10
            })
        );
    }

    #[test]
    fn encode_rejects_zero_height() {
        assert_eq!(
            encode(10, 0, &[]),
            Err(ThumbhashError::InvalidDimensions {
                width: 10,
                height: 0
            })
        );
    }

    #[test]
    fn encode_rejects_width_over_100() {
        let pixels = vec![0; 101 * 4];
        assert_eq!(
            encode(101, 1, &pixels),
            Err(ThumbhashError::InvalidDimensions {
                width: 101,
                height: 1
            })
        );
    }

    #[test]
    fn encode_rejects_height_over_100() {
        let pixels = vec![0; 101 * 4];
        assert_eq!(
            encode(1, 101, &pixels),
            Err(ThumbhashError::InvalidDimensions {
                width: 1,
                height: 101
            })
        );
    }

    #[test]
    fn encode_rejects_buffer_length_mismatch() {
        assert_eq!(
            encode(2, 2, &[0; 15]),
            Err(ThumbhashError::BufferLengthMismatch {
                expected: 16,
                actual: 15
            })
        );
        assert_eq!(
            encode(2, 2, &[0; 17]),
            Err(ThumbhashError::BufferLengthMismatch {
                expected: 16,
                actual: 17
            })
        );
    }

    #[test]
    fn encode_solid_opaque_white_sets_full_dc_and_clears_alpha_flag() {
        let hash = encode(8, 8, &flat_image(8, 8, [255; 4])).unwrap();
        assert_eq!(hash[0] & 0x3f, 63, "luminance DC is full");
        assert_eq!(hash[2] & 0x80, 0, "alpha flag is clear");
        assert_eq!((hash[2] >> 2) & 0x1f, 0, "luminance scale is zero");
        assert_eq!(hash.len(), 5 + 19);
    }

    #[test]
    fn encode_fully_transparent_image_matches_reference_vector() {
        // Reference: `rgbaToThumbHash` of thumbhash@0.1.1. With `avg_a == 0`
        // the encoder must skip the division, so the average colour stays
        // 0 and the P and Q DC terms round to 32, not to 0.
        let expected = "00088205000000000000000000000000000000000000000000";
        for (width, height, pixel) in [(8, 8, [0; 4]), (8, 8, [255, 255, 0, 0]), (1, 1, [0; 4])] {
            let hash = encode(width, height, &flat_image(width, height, pixel)).unwrap();
            assert_eq!(to_hex(&hash), expected, "{width}x{height} {pixel:?}");
        }
    }

    #[test]
    fn encode_output_stays_within_17_to_25_bytes_for_any_shape() {
        let sides = [1, 2, 3, 5, 7, 8, 13, 21, 34, 55, 89, 99, 100];
        let mut rng = Lcg(99);
        for width in sides {
            for height in sides {
                for mode in ["opaque", "alpha"] {
                    let pixels = golden_image(&mut rng, width, height, mode);
                    let hash = encode(width, height, &pixels).unwrap();
                    assert!(
                        HASH_BYTES.contains(&hash.len()),
                        "{mode}_{width}x{height}: {}",
                        hash.len()
                    );
                }
            }
        }
    }

    #[test]
    fn encode_output_reaches_both_length_bounds() {
        let mut rng = Lcg(5);
        let mut lengths = |width, height, mode| {
            let pixels = golden_image(&mut rng, width, height, mode);
            encode(width, height, &pixels).unwrap().len()
        };
        // A long thin opaque image has the fewest coefficients: 5 header
        // bytes and 12 bytes of AC terms.
        assert_eq!(lengths(1, 100, "opaque"), *HASH_BYTES.start());
        assert_eq!(lengths(100, 1, "opaque"), *HASH_BYTES.start());
        // A square image with alpha has the most: 5 header bytes, 1 alpha
        // byte, and 19 bytes of AC terms.
        assert_eq!(lengths(100, 100, "alpha"), *HASH_BYTES.end());
        // Each other shape falls between the two.
        assert_eq!(lengths(100, 100, "opaque"), 24);
        assert_eq!(lengths(1, 100, "alpha"), 23);
    }

    #[test]
    fn rgba_from_bands_expands_one_band_to_gray_with_opaque_alpha() {
        assert_eq!(
            rgba_from_bands(&[10, 20], 2, 1, 1).unwrap(),
            vec![10, 10, 10, 255, 20, 20, 20, 255]
        );
    }

    #[test]
    fn rgba_from_bands_expands_two_bands_to_gray_with_alpha() {
        assert_eq!(
            rgba_from_bands(&[10, 200, 20, 100], 2, 1, 2).unwrap(),
            vec![10, 10, 10, 200, 20, 20, 20, 100]
        );
    }

    #[test]
    fn rgba_from_bands_appends_opaque_alpha_to_three_bands() {
        assert_eq!(
            rgba_from_bands(&[1, 2, 3, 4, 5, 6], 2, 1, 3).unwrap(),
            vec![1, 2, 3, 255, 4, 5, 6, 255]
        );
    }

    #[test]
    fn rgba_from_bands_passes_four_bands_through() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(rgba_from_bands(&bytes, 2, 1, 4).unwrap(), bytes.to_vec());
    }

    #[test]
    fn rgba_from_bands_rejects_zero_and_five_bands() {
        assert_eq!(
            rgba_from_bands(&[], 1, 1, 0),
            Err(ThumbhashError::UnsupportedBands(0))
        );
        assert_eq!(
            rgba_from_bands(&[0; 5], 1, 1, 5),
            Err(ThumbhashError::UnsupportedBands(5))
        );
    }

    #[test]
    fn rgba_from_bands_rejects_length_that_implies_wider_samples() {
        assert_eq!(
            rgba_from_bands(&[0; 24], 2, 2, 3),
            Err(ThumbhashError::BufferLengthMismatch {
                expected: 12,
                actual: 24
            })
        );
        assert_eq!(
            rgba_from_bands(&[0; 64], 2, 2, 4),
            Err(ThumbhashError::BufferLengthMismatch {
                expected: 16,
                actual: 64
            })
        );
        assert_eq!(
            rgba_from_bands(&[0; 3], 2, 2, 1),
            Err(ThumbhashError::BufferLengthMismatch {
                expected: 4,
                actual: 3
            })
        );
    }

    #[test]
    fn rgba_from_bands_rejects_dimension_product_overflow_without_panic() {
        let result = rgba_from_bands(&[], u32::MAX, u32::MAX, 4);
        assert!(matches!(
            result,
            Err(ThumbhashError::BufferLengthMismatch { actual: 0, .. })
        ));
    }

    #[test]
    fn base64_encode_matches_rfc4648_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in vectors {
            assert_eq!(to_base64(input.as_bytes()), expected, "input {input:?}");
        }
    }

    #[test]
    fn base64_encode_of_25_bytes_is_36_chars_with_padding() {
        let encoded = to_base64(&[0xff; 25]);
        assert_eq!(encoded.len(), 36);
        assert!(encoded.ends_with("=="), "25 bytes leave one byte over");
        assert_eq!(encoded.matches('=').count(), 2);
    }

    #[test]
    fn base64_encode_of_17_bytes_is_24_chars_with_one_padding_char() {
        let encoded = to_base64(&[0xff; 17]);
        assert_eq!(encoded.len(), 24);
        assert_eq!(encoded.matches('=').count(), 1, "17 bytes leave two over");
    }

    #[test]
    fn base64_encode_uses_only_standard_alphabet_for_all_byte_values() {
        let bytes: Vec<u8> = (0..=255).collect();
        let encoded = to_base64(&bytes);
        assert_eq!(encoded.len(), 4 * bytes.len().div_ceil(3));
        assert!(is_standard_base64(&encoded));
        let body = encoded.trim_end_matches('=');
        let alphabet = ('A'..='Z')
            .chain('a'..='z')
            .chain('0'..='9')
            .chain(['+', '/']);
        for c in alphabet {
            assert!(body.contains(c), "alphabet character {c} never appears");
        }
        assert_eq!(base64_decode(&encoded), bytes);
    }

    #[test]
    fn base64_helpers_reject_text_outside_the_standard_alphabet() {
        assert!(is_standard_base64("Zm9vYg=="));
        assert!(!is_standard_base64("Zm9vYg="));
        assert!(!is_standard_base64("Zm9v-g=="));
        assert!(!is_standard_base64("Zm9vY==="));
        assert!(!is_standard_base64("Zm9v Yg=="));
    }
}
