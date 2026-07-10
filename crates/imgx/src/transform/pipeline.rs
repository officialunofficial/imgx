//! Transform pipeline: probe -> budget check -> decide -> reload -> extract
//! frame -> trim -> rotate/flip -> resize -> effects -> background ->
//! encode. Ported from src/transform/pipeline.zig. The animated+cover
//! resize workaround (INV-2) and GIF pre-encode safety check (INV-3) are
//! the highest-risk pieces of this entire rewrite — see docs/INVARIANTS.md.

use thiserror::Error;

use imgx_vips::{ThumbnailOptions, VipsError, VipsImage, consts};

use super::negotiate;
use super::params::{
    FitMode, FlipMode, Gravity, MetadataMode, OutputFormat, Rotation, TransformParams,
};

#[derive(Debug, Error)]
pub enum TransformError {
    #[error(transparent)]
    Vips(#[from] VipsError),
    #[error("source image exceeds the configured pixel budget ({0} > {1})")]
    ExceedsMaxPixels(u64, u64),
}

/// Result of a transform pipeline execution.
#[derive(Debug, Clone, PartialEq)]
pub struct TransformResult {
    pub data: Vec<u8>,
    pub format: OutputFormat,
    pub width: u32,
    pub height: u32,
    pub is_animated: bool,
    pub frame_count: Option<u32>,
}

/// Safety limits enforced during transform execution -- a general
/// decompression-bomb guard on any source image (`max_pixels`) plus the
/// animated-specific budget (`max_frames`, `max_animated_pixels`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformLimits {
    pub max_pixels: u64,
    pub max_frames: u32,
    pub max_animated_pixels: u64,
}

impl Default for TransformLimits {
    fn default() -> Self {
        Self {
            max_pixels: 71_000_000,
            max_frames: 100,
            max_animated_pixels: 50_000_000,
        }
    }
}

/// Execute the full transform pipeline: decode -> resize -> effects -> encode.
///
/// `input_data` is the raw bytes of the source image. `tp` controls the
/// resize/effect/encode behavior. `accept_header` is used for format
/// negotiation when `tp.format` is `None`/`Auto`.
pub fn transform(
    input_data: &[u8],
    tp: &TransformParams,
    accept_header: Option<&str>,
    limits: Option<TransformLimits>,
) -> Result<TransformResult, TransformError> {
    let limits = limits.unwrap_or_default();

    // -- PROBE --
    // Load first frame only (cheap) to detect animation metadata.
    let mut current = VipsImage::from_buffer(input_data)?;

    // Decompression-bomb guard: reject before any resize/effect/encode work
    // touches full pixel data if the *first frame's* pixel count alone
    // already exceeds the configured budget. This is intentionally
    // independent of the animated total-frames budget below (a single
    // frame can be dangerous on its own even if max_animated_pixels is
    // never reached because the source turns out not to be animated).
    let first_frame_pixels = current.width() as u64 * current.height() as u64;
    if first_frame_pixels > limits.max_pixels {
        return Err(TransformError::ExceedsMaxPixels(
            first_frame_pixels,
            limits.max_pixels,
        ));
    }

    let n_pages = n_pages_of(&current);
    let is_animated = n_pages.is_some_and(|n| n > 1);

    // -- BUDGET CHECK --
    // Enforce animated pixel budget; fall back to static first frame
    // (like Cloudflare) if total pixels across all frames exceeds it.
    let over_budget = if is_animated {
        let frame_w = current.width() as u64;
        let page_h = page_height_of(&current).unwrap_or_else(|| current.height()) as u64;
        let frame_count = n_pages.unwrap_or(1) as u64;
        (frame_w * page_h * frame_count) > limits.max_animated_pixels
    } else {
        false
    };

    // Effective frame count after clamping to max_frames.
    let effective_pages: Option<i32> = if is_animated && !over_budget {
        Some(
            n_pages
                .expect("invariant: is_animated is only true when n_pages_of() returned Some")
                .min(limits.max_frames as i32),
        )
    } else {
        n_pages
    };

    // -- DECIDE --
    let animated_format: Option<OutputFormat> = if is_animated
        && !over_budget
        && tp.anim != super::params::AnimMode::Static
        && tp.frame.is_none()
    {
        negotiate::negotiate_animated_format(accept_header, tp.format)
    } else {
        None
    };
    let animated_output = animated_format.is_some();

    // -- RELOAD --
    // If producing animated output, reload with all frames stacked,
    // clamped to max_frames if the source exceeds it.
    if animated_output {
        // invariant: animated_format (and thus animated_output) is only
        // Some when is_animated && !over_budget, which is exactly the
        // condition under which effective_pages/n_pages above are Some.
        let effective_pages = effective_pages.expect("invariant: animated_output implies Some");
        let n_pages = n_pages.expect("invariant: animated_output implies Some");
        current = if effective_pages < n_pages {
            VipsImage::from_buffer_animated(input_data, effective_pages)?
        } else {
            VipsImage::from_buffer_animated(input_data, -1)?
        };
    }

    // -- EXTRACT FRAME --
    // A specific frame requested on an animated source: extract it and
    // proceed as static from here on (animated_output is already false
    // in this case, since DECIDE required tp.frame.is_none()).
    if let (Some(frame_idx), true) = (tp.frame, is_animated) {
        if !animated_output {
            current = VipsImage::from_buffer_animated(input_data, -1)?;
        }
        let page_height = page_height_of(&current).unwrap_or_else(|| current.height());
        let actual_pages = n_pages.unwrap_or(1);
        let frame_idx = frame_idx as i32;
        let safe_frame = if frame_idx >= actual_pages {
            actual_pages - 1
        } else {
            frame_idx
        };
        let img_width = current.width();
        current = current.crop(0, safe_frame * page_height, img_width, page_height)?;
    }

    // -- TRIM -- (skipped for animated output: operates on the whole stack)
    if let Some(threshold) = tp.trim
        && !animated_output
    {
        let (left, top, width, height) = current.find_trim(threshold as f64)?;
        if width > 0 && height > 0 {
            current = current.crop(left, top, width, height)?;
        }
    }

    // -- ROTATE / FLIP --
    if let Some(rotation) = tp.rotate {
        let angle = match rotation {
            Rotation::Deg0 => consts::VIPS_ANGLE_D0,
            Rotation::Deg90 => consts::VIPS_ANGLE_D90,
            Rotation::Deg180 => consts::VIPS_ANGLE_D180,
            Rotation::Deg270 => consts::VIPS_ANGLE_D270,
        };
        if angle != consts::VIPS_ANGLE_D0 {
            current = current.rot(angle)?;
        }
    }
    if let Some(flip_mode) = tp.flip {
        if matches!(flip_mode, FlipMode::H | FlipMode::Hv) {
            current = current.flip(consts::VIPS_DIRECTION_HORIZONTAL)?;
        }
        if matches!(flip_mode, FlipMode::V | FlipMode::Hv) {
            current = current.flip(consts::VIPS_DIRECTION_VERTICAL)?;
        }
    }

    // -- RESIZE --
    let eff_w = tp.effective_width().map(|w| w as i32);
    let eff_h = tp.effective_height().map(|h| h as i32);

    if eff_w.is_some() || eff_h.is_some() {
        let source_w = current.width();
        let source_h = current.height();

        let effective_fit = if tp.fit == FitMode::Pad {
            FitMode::Contain
        } else {
            tp.fit
        };

        let thumb_width: i32 = match eff_w {
            Some(w) => w,
            None => {
                // invariant: the outer `if eff_w.is_some() || eff_h.is_some()`
                // guarantees eff_h is Some whenever eff_w is None.
                let h = eff_h.expect("invariant: eff_w.is_none() implies eff_h.is_some()");
                let ratio = source_w as f64 / source_h as f64;
                let derived = h as f64 * ratio;
                (derived.min(8192.0) as i32).max(1)
            }
        };

        // Animated + cover: vips_thumbnail_image's crop corrupts frame
        // boundaries on stacked animated buffers (libvips upstream bug
        // #2668). Two-step workaround instead — see docs/INVARIANTS.md
        // INV-2. DO NOT "simplify" this back to a single thumbnail call.
        if let (true, FitMode::Cover, Some(tw), Some(th)) =
            (animated_output, effective_fit, eff_w, eff_h)
        {
            let pages = effective_pages.or(n_pages).unwrap_or(1);
            let page_h = page_height_of(&current).unwrap_or(source_h / pages);

            // Scale so each frame covers the target rectangle.
            let hscale = tw as f64 / source_w as f64;
            let vscale = th as f64 / page_h as f64;
            let scale = hscale.max(vscale);
            let resize_w = ((source_w as f64 * scale).ceil() as i32).max(1);
            let resize_page_h = ((page_h as f64 * scale).ceil() as i32).max(1);
            let resize_stack_h = resize_page_h * pages;

            // Step 1: resize without crop — pass stack height, no crop option.
            current = current.thumbnail(
                resize_w,
                ThumbnailOptions {
                    height: Some(resize_stack_h),
                    ..Default::default()
                },
            )?;

            let resized_page_h = current.height() / pages;

            // Step 2: crop per-frame if needed.
            if resized_page_h > th || current.width() > tw {
                let cur_w = current.width();
                let crop_left = (cur_w - tw) / 2;
                let crop_top = (resized_page_h - th) / 2;

                if crop_top == 0 {
                    // Horizontal-only crop: single extract_area on the full stack.
                    current = current.crop(crop_left, 0, tw, current.height())?;
                } else {
                    // Vertical crop needed: extract each frame, crop, reassemble.
                    let frame_count = pages.min(256);
                    let mut frames: Vec<VipsImage> = Vec::with_capacity(frame_count as usize);
                    for fi in 0..frame_count {
                        let y_off = fi * resized_page_h;
                        frames.push(current.crop(crop_left, y_off + crop_top, tw, th)?);
                    }
                    current = imgx_vips::arrayjoin_vertical(&frames)?;
                }
            }

            current.set_int("page-height", th);
        } else {
            let opts = build_thumbnail_options(effective_fit, tp.gravity, eff_h);
            current = current.thumbnail(thumb_width, opts)?;

            // After resize, refresh page-height metadata for animated
            // images so the GIF/WebP encoder splits frames correctly.
            if animated_output {
                let new_height = current.height();
                let pages = effective_pages.or(n_pages).unwrap_or(1);
                let new_page_height = new_height / pages;
                if new_page_height > 0 {
                    current.set_int("page-height", new_page_height);
                }
            }
        }

        // fit=pad: embed the resized image centered on a target canvas.
        // Skipped for animated output (would pad the full stack height).
        if tp.fit == FitMode::Pad && !animated_output {
            let target_w = eff_w.unwrap_or_else(|| current.width());
            let target_h = eff_h.unwrap_or_else(|| current.height());
            let cur_w = current.width();
            let cur_h = current.height();

            if cur_w < target_w || cur_h < target_h {
                let off_x = (target_w - cur_w) / 2;
                let off_y = (target_h - cur_h) / 2;
                let bg = bg_color_from_params(tp.background);
                current = current.embed(off_x, off_y, target_w, target_h, bg)?;
            }
        }
    }

    // -- EFFECTS -- (sharpen -> blur -> brightness/contrast -> gamma -> saturation)
    if let Some(sigma) = tp.sharpen {
        current = current.sharpen(sigma as f64)?;
    }
    if let Some(sigma) = tp.blur {
        current = current.gaussblur(sigma as f64)?;
    }
    if tp.brightness.is_some() || tp.contrast.is_some() {
        let contrast_val = tp.contrast.map(|c| c as f64).unwrap_or(1.0);
        let brightness_offset = tp
            .brightness
            .map(|b| (b as f64 - 1.0) * 128.0)
            .unwrap_or(0.0);
        current = current.linear1(contrast_val, brightness_offset)?;
    }
    if let Some(g) = tp.gamma {
        current = current.gamma(g as f64)?;
    }
    if let Some(sat) = tp.saturation {
        let sat_f64 = sat as f64;
        current = current.colourspace(consts::VIPS_INTERPRETATION_LCH)?;

        let l_band = current.extract_band(0, 1)?;
        let c_band = current.extract_band(1, 1)?;
        let h_band = current.extract_band(2, 1)?;

        let c_scaled = c_band.linear1(sat_f64, 0.0)?;
        let lc = VipsImage::bandjoin2(&l_band, &c_scaled)?;
        let lch_result = VipsImage::bandjoin2(&lc, &h_band)?;

        current = lch_result.colourspace(consts::VIPS_INTERPRETATION_sRGB)?;
    }

    // -- BACKGROUND -- (flatten alpha onto background color)
    if tp.background.is_some() && tp.fit != FitMode::Pad && current.has_alpha() {
        let bg = bg_color_from_params(tp.background);
        current = current.flatten(bg)?;
    }

    // -- ENCODE --
    let output_format = animated_format.unwrap_or_else(|| {
        negotiate::negotiate_format(accept_header, current.has_alpha(), tp.format)
    });

    let out_width = current.width() as u32;
    let out_height = current.height() as u32;

    let data = encode_image(&current, output_format, tp.quality, tp.metadata)?;

    Ok(TransformResult {
        data,
        format: output_format,
        width: out_width,
        height: out_height,
        is_animated: animated_output,
        frame_count: if animated_output {
            effective_pages.map(|p| p as u32)
        } else {
            None
        },
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// `n-pages` metadata, guarding against the < 1 sentinel the same way
/// bindings.zig's `getNPages` does.
fn n_pages_of(img: &VipsImage) -> Option<i32> {
    img.n_pages().filter(|&n| n >= 1)
}

/// `page-height` metadata, guarding against the < 1 sentinel the same way
/// bindings.zig's `getPageHeight` does.
fn page_height_of(img: &VipsImage) -> Option<i32> {
    img.page_height().filter(|&n| n >= 1)
}

/// Convert an optional RGB byte triplet to an f64 background array.
/// Defaults to white when no color is specified.
fn bg_color_from_params(background: Option<[u8; 3]>) -> [f64; 3] {
    match background {
        Some(rgb) => [rgb[0] as f64, rgb[1] as f64, rgb[2] as f64],
        None => [255.0, 255.0, 255.0],
    }
}

/// Map FitMode + Gravity to vips ThumbnailOptions.
fn build_thumbnail_options(
    fit: FitMode,
    gravity: Gravity,
    height: Option<i32>,
) -> ThumbnailOptions {
    let mut opts = ThumbnailOptions {
        height,
        ..Default::default()
    };
    match fit {
        FitMode::Contain | FitMode::Pad | FitMode::Inside => {
            opts.size = Some(consts::VIPS_SIZE_DOWN)
        }
        FitMode::Cover => opts.crop = Some(map_gravity_to_crop(gravity)),
        FitMode::Fill => opts.size = Some(consts::VIPS_SIZE_FORCE),
        FitMode::Outside => opts.size = Some(consts::VIPS_SIZE_UP),
    }
    opts
}

/// Map a Gravity value to the corresponding VIPS_INTERESTING_* constant.
/// Directional gravities (north, south, ...) aren't directly supported by
/// vips_thumbnail_image's crop parameter and fall back to center cropping.
fn map_gravity_to_crop(gravity: Gravity) -> i32 {
    match gravity {
        Gravity::Center => consts::VIPS_INTERESTING_CENTRE,
        Gravity::Smart => consts::VIPS_INTERESTING_ENTROPY,
        Gravity::Attention => consts::VIPS_INTERESTING_ATTENTION,
        _ => consts::VIPS_INTERESTING_CENTRE,
    }
}

/// Encode a VipsImage into a buffer using the specified output format.
fn encode_image(
    image: &VipsImage,
    format: OutputFormat,
    quality: u8,
    metadata: MetadataMode,
) -> Result<Vec<u8>, VipsError> {
    let q = quality as i32;
    // Strip -> strip all metadata; Keep/Copyright -> preserve metadata
    // (libvips has no "copyright-only" mode, so Copyright is treated the
    // same as Keep for now, matching the Zig implementation's known
    // future-enhancement note).
    let do_strip = metadata == MetadataMode::Strip;
    match format {
        OutputFormat::Jpeg | OutputFormat::Auto => image.save_jpeg(q, do_strip),
        OutputFormat::Png => image.save_png(6, do_strip),
        OutputFormat::Webp => image.save_webp(q, do_strip),
        OutputFormat::Avif => image.save_avif(q, do_strip),
        OutputFormat::Gif => encode_gif(image),
    }
}

/// Encode as GIF. Before encoding, validates that page-height metadata
/// evenly divides the total image height — stale metadata (left over
/// from resize or effects) causes a SIGSEGV in the GIF encoder, so reset
/// to single-frame if the invariant doesn't hold. See docs/INVARIANTS.md
/// INV-3 — this reproduces a real prior crash, do not remove.
fn encode_gif(image: &VipsImage) -> Result<Vec<u8>, VipsError> {
    if let Some(ph) = image.page_height() {
        let h = image.height();
        if ph > h || h % ph != 0 {
            image.set_int("page-height", h);
            image.set_int("n-pages", 1);
        }
    }
    image.save_gif()
}

#[cfg(test)]
mod tests {
    use super::super::params::{AnimMode, parse};
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::Once;

    static VIPS_INIT: Once = Once::new();

    fn init() {
        VIPS_INIT.call_once(|| imgx_vips::init().expect("vips init"));
    }

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test/fixtures")
            .join(name);
        fs::read(&path).unwrap_or_else(|e| panic!("reading fixture {path:?}: {e}"))
    }

    fn static_fixture() -> Vec<u8> {
        fixture("test_4x4.png")
    }

    fn animated_fixture() -> Option<Vec<u8>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/fixtures/loading.gif");
        fs::read(&path).ok()
    }

    #[test]
    fn transform_with_default_params_preserves_image() {
        init();
        let data = static_fixture();
        let result = transform(&data, &TransformParams::default(), None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 4);
        assert_eq!(result.height, 4);
    }

    #[test]
    fn transform_rejects_source_exceeding_max_pixels() {
        init();
        let data = static_fixture(); // test_4x4.png = 16 pixels
        let limits = TransformLimits {
            max_pixels: 10,
            ..Default::default()
        };
        let err = transform(&data, &TransformParams::default(), None, Some(limits))
            .expect_err("16-pixel source must be rejected under a 10-pixel budget");
        assert!(matches!(err, TransformError::ExceedsMaxPixels(16, 10)));
    }

    #[test]
    fn transform_accepts_source_within_max_pixels() {
        init();
        let data = static_fixture(); // test_4x4.png = 16 pixels
        let limits = TransformLimits {
            max_pixels: 16,
            ..Default::default()
        };
        assert!(transform(&data, &TransformParams::default(), None, Some(limits)).is_ok());
    }

    #[test]
    fn transform_resize_to_specific_width() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            width: Some(2),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 2);
    }

    #[test]
    fn transform_to_jpeg_format() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            format: Some(OutputFormat::Jpeg),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.format, OutputFormat::Jpeg);
    }

    #[test]
    fn transform_to_webp_format() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            format: Some(OutputFormat::Webp),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.format, OutputFormat::Webp);
    }

    #[test]
    fn transform_to_png_format() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            format: Some(OutputFormat::Png),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.format, OutputFormat::Png);
    }

    #[test]
    fn transform_with_auto_format_negotiation() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            format: Some(OutputFormat::Auto),
            ..Default::default()
        };
        let result = transform(&data, &p, Some("image/webp"), None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.format, OutputFormat::Webp);
    }

    #[test]
    fn transform_with_sharpen() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            sharpen: Some(1.5),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 4);
        assert_eq!(result.height, 4);
    }

    #[test]
    fn transform_with_blur() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            blur: Some(2.0),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 4);
        assert_eq!(result.height, 4);
    }

    #[test]
    fn transform_with_fit_cover() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            width: Some(2),
            height: Some(2),
            fit: FitMode::Cover,
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 2);
        assert_eq!(result.height, 2);
    }

    #[test]
    fn transform_with_fit_fill() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            width: Some(2),
            height: Some(3),
            fit: FitMode::Fill,
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 2);
        assert_eq!(result.height, 3);
    }

    #[test]
    fn transform_with_rotate_90() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            rotate: Some(Rotation::Deg90),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 4);
        assert_eq!(result.height, 4);
    }

    #[test]
    fn transform_with_flip_horizontal() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            flip: Some(FlipMode::H),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 4);
        assert_eq!(result.height, 4);
    }

    #[test]
    fn transform_with_brightness() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            brightness: Some(1.5),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 4);
    }

    #[test]
    fn transform_with_contrast() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            contrast: Some(0.8),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
    }

    #[test]
    fn transform_with_gamma() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            gamma: Some(2.2),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
    }

    #[test]
    fn transform_with_saturation() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            saturation: Some(0.5),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 4);
        assert_eq!(result.height, 4);
    }

    #[test]
    fn transform_with_fit_pad() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            width: Some(8),
            height: Some(8),
            fit: FitMode::Pad,
            background: Some([255, 0, 0]),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.width, 8);
        assert_eq!(result.height, 8);
    }

    #[test]
    fn transform_with_metadata_keep() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            metadata: MetadataMode::Keep,
            format: Some(OutputFormat::Png),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
    }

    #[test]
    fn animated_gif_passthrough_produces_output() {
        init();
        let Some(data) = animated_fixture() else {
            return;
        };
        let p = TransformParams {
            format: Some(OutputFormat::Gif),
            ..Default::default()
        };
        let result = transform(&data, &p, Some("image/gif"), None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.format, OutputFormat::Gif);
        assert!(result.is_animated);
    }

    #[test]
    fn animated_gif_anim_static_produces_single_frame() {
        init();
        let Some(data) = animated_fixture() else {
            return;
        };
        let p = TransformParams {
            anim: AnimMode::Static,
            format: Some(OutputFormat::Png),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert!(!result.is_animated);
        // Single frame: height should be 128 (one frame), not 1536 (stacked).
        assert_eq!(result.height, 128);
    }

    #[test]
    fn animated_gif_frame_1_extracts_second_frame() {
        init();
        let Some(data) = animated_fixture() else {
            return;
        };
        let p = TransformParams {
            frame: Some(1),
            format: Some(OutputFormat::Png),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert!(!result.data.is_empty());
        assert!(!result.is_animated);
        assert_eq!(result.width, 128);
        assert_eq!(result.height, 128);
    }

    #[test]
    fn animated_gif_f_webp_produces_animated_webp() {
        init();
        let Some(data) = animated_fixture() else {
            return;
        };
        let p = TransformParams {
            format: Some(OutputFormat::Webp),
            ..Default::default()
        };
        let result = transform(&data, &p, Some("image/webp"), None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.format, OutputFormat::Webp);
        assert!(result.is_animated);
    }

    #[test]
    fn animated_gif_resize_produces_animated_output() {
        init();
        let Some(data) = animated_fixture() else {
            return;
        };
        let p = TransformParams {
            width: Some(64),
            format: Some(OutputFormat::Gif),
            ..Default::default()
        };
        let result = transform(&data, &p, Some("image/gif"), None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.format, OutputFormat::Gif);
        assert!(result.is_animated);
        assert_eq!(result.width, 64);
    }

    /// Regression test for the page-height SIGSEGV this pipeline's GIF
    /// safety check (INV-3) exists to prevent — this exact resize path
    /// caused a crash before the fix.
    #[test]
    fn animated_gif_resize_preserves_correct_page_height_for_encoding() {
        init();
        let Some(data) = animated_fixture() else {
            return;
        };
        let p = TransformParams {
            width: Some(32),
            height: Some(32),
            format: Some(OutputFormat::Gif),
            ..Default::default()
        };
        let result = transform(&data, &p, Some("image/gif"), None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.format, OutputFormat::Gif);
        assert!(result.is_animated);
        assert_eq!(result.width, 32);
    }

    #[test]
    fn animated_gif_with_effects_encodes_without_segfault() {
        init();
        let Some(data) = animated_fixture() else {
            return;
        };
        let p = TransformParams {
            width: Some(64),
            sharpen: Some(1.5),
            format: Some(OutputFormat::Gif),
            ..Default::default()
        };
        let result = transform(&data, &p, Some("image/gif"), None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.format, OutputFormat::Gif);
    }

    #[test]
    fn animated_gif_resize_and_blur_encodes_correctly() {
        init();
        let Some(data) = animated_fixture() else {
            return;
        };
        let p = TransformParams {
            width: Some(48),
            blur: Some(1.0),
            format: Some(OutputFormat::Gif),
            ..Default::default()
        };
        let result = transform(&data, &p, Some("image/gif"), None).unwrap();
        assert!(!result.data.is_empty());
        assert_eq!(result.format, OutputFormat::Gif);
        assert!(result.is_animated);
    }

    #[test]
    fn static_image_is_not_marked_as_animated() {
        init();
        let data = static_fixture();
        let result = transform(&data, &TransformParams::default(), None, None).unwrap();
        assert!(!result.is_animated);
        assert_eq!(result.frame_count, None);
    }

    #[test]
    fn animated_gif_over_pixel_budget_falls_back_to_static() {
        init();
        let Some(data) = animated_fixture() else {
            return;
        };
        let p = TransformParams {
            format: Some(OutputFormat::Gif),
            ..Default::default()
        };
        let cfg = TransformLimits {
            max_animated_pixels: 1000,
            max_frames: 100,
            ..Default::default()
        };
        let result = transform(&data, &p, Some("image/gif"), Some(cfg)).unwrap();
        assert!(!result.data.is_empty());
        assert!(!result.is_animated);
        assert_eq!(result.height, 128);
    }

    #[test]
    fn animated_gif_with_max_frames_clamping() {
        init();
        let Some(data) = animated_fixture() else {
            return;
        };
        let p = TransformParams {
            format: Some(OutputFormat::Gif),
            ..Default::default()
        };
        let cfg = TransformLimits {
            max_frames: 3,
            max_animated_pixels: 50_000_000,
            ..Default::default()
        };
        let result = transform(&data, &p, Some("image/gif"), Some(cfg)).unwrap();
        assert!(!result.data.is_empty());
        assert!(result.is_animated);
        assert_eq!(result.width, 128);
    }

    /// Sanity-check that TransformParams::default() through `parse("")`
    /// behaves identically for the pipeline (params.rs and pipeline.rs
    /// must stay in lockstep on what "default" means).
    #[test]
    fn parsed_empty_params_transform_same_as_default() {
        init();
        let data = static_fixture();
        let parsed = parse("").unwrap();
        let result = transform(&data, &parsed, None, None).unwrap();
        assert_eq!(result.width, 4);
        assert_eq!(result.height, 4);
    }
}
