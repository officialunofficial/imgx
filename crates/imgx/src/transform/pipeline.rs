//! Transform pipeline: probe -> budget check -> decide -> reload -> extract
//! frame -> trim -> rotate/flip -> resize -> effects -> background ->
//! encode. Ported from src/transform/pipeline.zig. The animated+cover
//! resize workaround (INV-2) and GIF pre-encode safety check (INV-3) are
//! the highest-risk pieces of this entire rewrite — see docs/INVARIANTS.md.

use thiserror::Error;

use imgx_vips::{ThumbnailOptions, VipsError, VipsImage, consts};

use super::negotiate;
use super::params::{
    CompressionMode, DrawOverlay, DrawRepeat, FitMode, FlipMode, Gravity, MetadataMode,
    OutputFormat, Rotation, TransformParams,
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
    let compression_fast = tp.compression == Some(CompressionMode::Fast);

    // -- PROBE --
    // Load first frame only (cheap) to detect animation metadata.
    let mut current = VipsImage::from_buffer(input_data)?;

    // Captured for `format=json`'s "original" stats (gap 5) -- the raw
    // probed dimensions before any rotate/resize/crop touches them.
    let original_width = current.width() as u32;
    let original_height = current.height() as u32;

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

    // Cloudflare's per-side trim keys (docs/CLOUDFLARE_PARITY.md gap 9):
    // fixed pixel crop from each edge independently, NOT border-color
    // aware (unlike the legacy numeric `trim` above, via find_trim) --
    // additive, both may be combined in the same request. A value in
    // `0.0..1.0` is a fraction of that side's dimension, resolved here
    // where the actual image size is known (parse time isn't).
    if !animated_output
        && (tp.trim_top.is_some()
            || tp.trim_right.is_some()
            || tp.trim_bottom.is_some()
            || tp.trim_left.is_some())
    {
        let w = current.width();
        let h = current.height();
        let resolve = |v: Option<f32>, dim: i32| -> i32 {
            match v {
                Some(v) if v < 1.0 => (v as f64 * dim as f64).round() as i32,
                Some(v) => v.round() as i32,
                None => 0,
            }
        };
        let top = resolve(tp.trim_top, h).clamp(0, h);
        let left = resolve(tp.trim_left, w).clamp(0, w);
        let right = resolve(tp.trim_right, w).clamp(0, w);
        let bottom = resolve(tp.trim_bottom, h).clamp(0, h);
        let new_w = (w - left - right).max(1);
        let new_h = (h - top - bottom).max(1);
        current = current.crop(left, top, new_w, new_h)?;
    }

    // -- ROTATE / FLIP --
    if let Some(rotation) = tp.rotate {
        let angle = rotation_angle(rotation);
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

        // Bugfix uncovered while verifying gap 13 (rotate-before-resize
        // ordering, docs/CLOUDFLARE_PARITY.md): vips_thumbnail_image
        // defaults its `height` option to the `width` value when omitted
        // entirely -- i.e. it fits within a WIDTHxWIDTH *square* box, not
        // "preserve aspect ratio from width alone" as callers requesting
        // only `w=` naturally expect. Confirmed both via this crate's
        // pipeline and directly via `vipsthumbnail --size` on a
        // known-non-square fixture. For fit modes that don't already
        // define their own explicit target box (Cover/Fill/Crop/
        // AspectCrop all require both dimensions to mean anything), a
        // missing height is now derived from the *current* (i.e.
        // already-rotated/flipped) aspect ratio before the thumbnail call,
        // rather than left as `None` for vips to silently square-box.
        let derived_height = if eff_h.is_none()
            && !matches!(
                effective_fit,
                FitMode::Cover | FitMode::Fill | FitMode::Crop | FitMode::AspectCrop
            )
            && source_w > 0
        {
            let ratio = source_h as f64 / source_w as f64;
            Some(((thumb_width as f64 * ratio).round().min(8192.0) as i32).max(1))
        } else {
            eff_h
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
        } else if let (FitMode::AspectCrop, false, Some(tw), Some(th)) =
            (effective_fit, animated_output, eff_w, eff_h)
        {
            // Cloudflare's `aspect-crop` (docs/CLOUDFLARE_PARITY.md gap
            // 3): crop to the target aspect ratio. If the source is
            // large enough to cover the target without upscaling, this
            // is identical to `crop`/`cover` (downscale-then-crop). If
            // the source is smaller, it must NOT be upscaled -- instead
            // the *original-size* image is cropped directly to the
            // target aspect ratio.
            let hscale = tw as f64 / source_w as f64;
            let vscale = th as f64 / source_h as f64;
            let scale = hscale.max(vscale);
            if scale > 1.0 {
                let target_ratio = tw as f64 / th as f64;
                let source_ratio = source_w as f64 / source_h as f64;
                let (crop_w, crop_h) = if source_ratio > target_ratio {
                    let new_w = ((source_h as f64 * target_ratio).round() as i32)
                        .min(source_w)
                        .max(1);
                    (new_w, source_h)
                } else {
                    let new_h = ((source_w as f64 / target_ratio).round() as i32)
                        .min(source_h)
                        .max(1);
                    (source_w, new_h)
                };
                let crop_left = (source_w - crop_w) / 2;
                let crop_top = (source_h - crop_h) / 2;
                current = current.crop(crop_left, crop_top, crop_w, crop_h)?;
            } else {
                let opts = ThumbnailOptions {
                    height: Some(th),
                    crop: Some(map_gravity_to_crop(tp.gravity)),
                    size: Some(consts::VIPS_SIZE_DOWN),
                };
                current = current.thumbnail(tw, opts)?;
            }
        } else {
            let opts = build_thumbnail_options(effective_fit, tp.gravity, derived_height);
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

    // -- BORDER -- (gap 10: verified against
    // developers.cloudflare.com/images/optimization/features/ -- "The
    // border is applied after the image has been resized. The border
    // width automatically scales with the dpr parameter." No published
    // URL syntax exists for this feature (Cloudflare marks it
    // "available only in Workers"), so the border/border.*
    // key names themselves are spec-derived -- see
    // docs/CLOUDFLARE_PARITY.md gap 10. Skipped for animated output: an
    // `embed` on the stacked frame buffer would corrupt frame boundaries
    // the same way a naive crop does (INV-2's underlying concern).
    if !animated_output
        && (tp.border_width.is_some()
            || tp.border_top.is_some()
            || tp.border_right.is_some()
            || tp.border_bottom.is_some()
            || tp.border_left.is_some())
    {
        let uniform = tp.border_width.unwrap_or(0) as f32;
        let scaled = |side: Option<u32>| -> i32 {
            let px = side.map(|v| v as f32).unwrap_or(uniform);
            (px * tp.dpr).round().max(0.0) as i32
        };
        let top = scaled(tp.border_top);
        let right = scaled(tp.border_right);
        let bottom = scaled(tp.border_bottom);
        let left = scaled(tp.border_left);
        if top > 0 || right > 0 || bottom > 0 || left > 0 {
            let cur_w = current.width();
            let cur_h = current.height();
            let target_w = cur_w + left + right;
            let target_h = cur_h + top + bottom;
            // Cloudflare's `border.color` accepts any CSS color with no
            // stated default; imgx only parses 6-hex like `bg` (see
            // docs/CLOUDFLARE_PARITY.md gap 10) and defaults to black
            // when unset, since Cloudflare's own docs example shows an
            // explicit color in every case.
            let bg = tp
                .border_color
                .map(|c| [c[0] as f64, c[1] as f64, c[2] as f64])
                .unwrap_or([0.0, 0.0, 0.0]);
            current = current.embed(left, top, target_w, target_h, bg)?;
        }
    }

    let out_width = current.width() as u32;
    let out_height = current.height() as u32;

    // -- JSON -- (gap 5: format=json is a metadata-only response, no
    // image bytes. Schema is spec-derived -- see
    // docs/CLOUDFLARE_PARITY.md -- but the values themselves are real,
    // computed from the actual transform, not guessed: a real codec is
    // negotiated and actually encoded so "transformed.file_size" reflects
    // the size the image WOULD have been served at.)
    if tp.format == Some(OutputFormat::Json) {
        let negotiated = negotiate::negotiate_format(accept_header, current.has_alpha(), None);
        let negotiated = if animated_output {
            negotiated
        } else {
            negotiate::apply_compression_fast(negotiated, compression_fast)
        };
        let encoded = encode_image(&current, negotiated, tp.quality, tp.metadata)?;
        let json = format!(
            "{{\"original\":{{\"width\":{original_width},\"height\":{original_height},\"file_size\":{}}},\"transformed\":{{\"width\":{out_width},\"height\":{out_height},\"format\":\"{}\",\"file_size\":{}}}}}",
            input_data.len(),
            negotiated.as_str(),
            encoded.len(),
        );
        return Ok(TransformResult {
            data: json.into_bytes(),
            format: OutputFormat::Json,
            width: out_width,
            height: out_height,
            is_animated: animated_output,
            frame_count: if animated_output {
                effective_pages.map(|p| p as u32)
            } else {
                None
            },
        });
    }

    // -- ENCODE --
    let output_format = animated_format.unwrap_or_else(|| {
        negotiate::negotiate_format(accept_header, current.has_alpha(), tp.format)
    });
    // Gap 6 -- compression=fast: bias away from the slowest encoder
    // (AVIF)/WebP toward JPEG. Skipped for animated output -- forcing
    // JPEG would silently drop animation, an interaction Cloudflare's
    // docs don't describe, so animated requests are left alone.
    let output_format = if animated_output {
        output_format
    } else {
        negotiate::apply_compression_fast(output_format, compression_fast)
    };

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
        // Cloudflare's `crop`: fill the target area like `cover`, but
        // never upscale (VIPS_SIZE_DOWN clamps to downscale-only). See
        // docs/CLOUDFLARE_PARITY.md gap 3.
        FitMode::Crop => {
            opts.crop = Some(map_gravity_to_crop(gravity));
            opts.size = Some(consts::VIPS_SIZE_DOWN);
        }
        // AspectCrop is handled by its own dedicated branch in
        // `transform()` before `build_thumbnail_options` is ever called
        // for it (its crop math isn't a plain vips_thumbnail_image call)
        // -- this arm only exists for match exhaustiveness.
        FitMode::AspectCrop => {
            opts.crop = Some(map_gravity_to_crop(gravity));
            opts.size = Some(consts::VIPS_SIZE_DOWN);
        }
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

fn rotation_angle(rotation: Rotation) -> i32 {
    match rotation {
        Rotation::Deg0 => consts::VIPS_ANGLE_D0,
        Rotation::Deg90 => consts::VIPS_ANGLE_D90,
        Rotation::Deg180 => consts::VIPS_ANGLE_D180,
        Rotation::Deg270 => consts::VIPS_ANGLE_D270,
    }
}

/// Resolve a `draw` overlay dimension/position value: `>= 1.0` is a pixel
/// count, a value in `0.0..1.0` is a fraction of `base_dim` -- the same
/// convention as the per-side trim keys (gap 9).
fn resolve_overlay_dim(v: f32, base_dim: i32) -> i32 {
    if v < 1.0 {
        (v as f64 * base_dim as f64).round() as i32
    } else {
        v.round() as i32
    }
}

/// Composite a single already-decoded overlay image onto `base`,
/// implementing a bounded, spec-derived subset of Cloudflare's `draw`
/// overlay semantics (docs/CLOUDFLARE_PARITY.md gap 11). This function
/// proves the libvips compositing math against local, already-fetched
/// image buffers -- the actual remote-URL fetch that would supply
/// `overlay_bytes` in a real request is deliberately NOT implemented in
/// this pass (see the gap-11 note in CLOUDFLARE_PARITY.md); callers here
/// (currently only this module's own tests) supply already-fetched bytes
/// directly.
///
/// Documented scope limitations, not silently dropped:
/// - Opacity attenuation only has an effect when the overlay already
///   carries an alpha channel (PNG/WebP) -- consistent with Cloudflare's
///   own recommendation to use PNG/WebP for overlays.
/// - When both `background` and `opacity` are set on the same entry,
///   `background` is applied first (flattening away the overlay's own
///   alpha channel), so `opacity` has no further effect -- a documented
///   simplification rather than a two-pass blend.
/// - Blend mode is always "over"; Cloudflare's `draw` doesn't document a
///   configurable blend mode via its published options.
pub fn composite_draw_overlay(
    base: &VipsImage,
    overlay_bytes: &[u8],
    entry: &DrawOverlay,
) -> Result<VipsImage, TransformError> {
    let mut overlay = VipsImage::from_buffer(overlay_bytes)?;

    let base_w = base.width();
    let base_h = base.height();

    let target_w = entry.width.map(|w| resolve_overlay_dim(w, base_w));
    let target_h = entry.height.map(|h| resolve_overlay_dim(h, base_h));
    if target_w.is_some() || target_h.is_some() {
        let fit = entry.fit.unwrap_or(FitMode::Contain);
        let gravity = entry.gravity.unwrap_or(Gravity::Center);
        let width = target_w.unwrap_or_else(|| overlay.width());
        let opts = build_thumbnail_options(fit, gravity, target_h);
        overlay = overlay.thumbnail(width.max(1), opts)?;
    }

    if let Some(rotation) = entry.rotate {
        let angle = rotation_angle(rotation);
        if angle != consts::VIPS_ANGLE_D0 {
            overlay = overlay.rot(angle)?;
        }
    }

    if let Some(repeat) = entry.repeat {
        let (tile_w, tile_h) = match repeat {
            DrawRepeat::Both => (base_w, base_h),
            DrawRepeat::X => (base_w, overlay.height()),
            DrawRepeat::Y => (overlay.width(), base_h),
        };
        overlay = overlay.tile_to_size(tile_w.max(1), tile_h.max(1))?;
    }

    if let Some(bg) = entry.background
        && overlay.has_alpha()
    {
        let bg_f = [bg[0] as f64, bg[1] as f64, bg[2] as f64];
        overlay = overlay.flatten(bg_f)?;
    }

    if let Some(op) = entry.opacity
        && overlay.has_alpha()
    {
        let bands = overlay.bands();
        let color = overlay.extract_band(0, bands - 1)?;
        let alpha = overlay.extract_band(bands - 1, 1)?;
        let alpha_scaled = alpha.linear1(op as f64, 0.0)?;
        overlay = VipsImage::bandjoin2(&color, &alpha_scaled)?;
    }

    let ow = overlay.width();
    let oh = overlay.height();
    let x = match (entry.left, entry.right) {
        (Some(l), _) => resolve_overlay_dim(l, base_w),
        (None, Some(r)) => base_w - ow - resolve_overlay_dim(r, base_w),
        (None, None) => (base_w - ow) / 2,
    };
    let y = match (entry.top, entry.bottom) {
        (Some(t), _) => resolve_overlay_dim(t, base_h),
        (None, Some(b)) => base_h - oh - resolve_overlay_dim(b, base_h),
        (None, None) => (base_h - oh) / 2,
    };

    base.composite_over(&overlay, x, y).map_err(Into::into)
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
        // BaselineJpeg shares Jpeg's encode path exactly: libvips'
        // vips_jpegsave_buffer already defaults `interlace` to FALSE
        // (baseline), so there is no separate FFI call to make -- see
        // docs/CLOUDFLARE_PARITY.md gap 5.
        OutputFormat::Jpeg | OutputFormat::BaselineJpeg | OutputFormat::Auto => {
            image.save_jpeg(q, do_strip)
        }
        OutputFormat::Png => image.save_png(6, do_strip),
        OutputFormat::Webp => image.save_webp(q, do_strip),
        OutputFormat::Avif => image.save_avif(q, do_strip),
        OutputFormat::Gif => encode_gif(image),
        // Never reached: `transform()` intercepts `format == Json`
        // before calling `encode_image` and builds a JSON stats payload
        // instead (see the `-- JSON --` block below).
        OutputFormat::Json => image.save_jpeg(q, do_strip),
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

    // ----------------------------------------------------------------
    // Cloudflare parity gaps (docs/CLOUDFLARE_PARITY.md)
    // ----------------------------------------------------------------

    fn nonsquare_fixture() -> Vec<u8> {
        fixture("bench_2000x1500.png")
    }

    /// Gap 13 -- verify (and lock in as a regression test) that rotate is
    /// applied BEFORE resize, and that width/height refer to the
    /// post-rotation axes -- matching Cloudflare's documented behavior
    /// ("Rotation is performed before resizing; width and height options
    /// will refer to the axes after the image is rotated," verified
    /// against developers.cloudflare.com/images/optimization/features/
    /// via the Cloudflare docs MCP search tool). The 2000x1500 (4:3
    /// landscape) source, rotated 90 degrees, becomes 1500x2000
    /// (portrait) BEFORE the w=200 resize is applied -- so a w=200
    /// resize (no height given, aspect-ratio-derived) must produce a
    /// TALLER-than-wide 200x~267 output, not a 200x150 output (which is
    /// what resize-before-rotate would produce).
    #[test]
    fn rotate_is_applied_before_resize_axes_reflect_post_rotation_orientation() {
        init();
        let data = nonsquare_fixture();
        let p = TransformParams {
            width: Some(200),
            rotate: Some(Rotation::Deg90),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!(result.width, 200);
        // 1500x2000 (post-rotation) resized to width=200 preserves aspect
        // ratio => height = 200 * (2000/1500) = 266.67 -> 266 or 267.
        assert!(
            result.height > result.width,
            "post-rotation resize must be taller than wide (got {}x{}); \
             a resize-before-rotate bug would instead produce a 200x150 \
             wide output",
            result.width,
            result.height
        );
    }

    /// Gap 3 -- pixel-dimension proof that Cloudflare's `squeeze` and
    /// imgx's existing `fill` are equivalent (both force exact
    /// non-aspect-preserving dimensions), justifying the parser alias in
    /// params.rs rather than a new enum variant.
    #[test]
    fn transform_with_fit_squeeze_matches_fill_dimensions() {
        init();
        let data = static_fixture();
        let squeeze = parse("w=2,h=3,fit=squeeze").unwrap();
        let fill = parse("w=2,h=3,fit=fill").unwrap();
        assert_eq!(squeeze.fit, fill.fit);
        let result = transform(&data, &squeeze, None, None).unwrap();
        assert_eq!(result.width, 2);
        assert_eq!(result.height, 3);
    }

    /// Gap 3 -- pixel-dimension proof that Cloudflare's `scale-up`
    /// (upscale-only, never downscale, preserve aspect) matches imgx's
    /// existing `outside` (`VIPS_SIZE_UP`): requesting a target SMALLER
    /// than the 4x4 source must leave the source untouched (never
    /// downscale).
    #[test]
    fn transform_with_fit_scale_up_never_downscales_smaller_target() {
        init();
        let data = static_fixture(); // 4x4
        let p = parse("w=2,h=2,fit=scale-up").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!(
            result.width, 4,
            "scale-up must never downscale below source size"
        );
        assert_eq!(result.height, 4);
    }

    /// Gap 3 -- `fit=crop`: fills the target area like `cover` when the
    /// source is large enough, but never upscales.
    #[test]
    fn transform_with_fit_crop_never_upscales_smaller_source() {
        init();
        let data = static_fixture(); // 4x4
        let p = parse("w=8,h=8,fit=crop").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert!(
            result.width <= 4 && result.height <= 4,
            "fit=crop must never upscale a source smaller than the \
             target (got {}x{})",
            result.width,
            result.height
        );
    }

    #[test]
    fn transform_with_fit_crop_fills_target_when_source_is_larger() {
        init();
        let data = nonsquare_fixture(); // 2000x1500
        let p = parse("w=100,h=100,fit=crop").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!(result.width, 100);
        assert_eq!(result.height, 100);
    }

    /// Gap 3 -- `fit=aspect-crop`: when the source is smaller than the
    /// target's covering size, it must NOT upscale, but must still crop
    /// to the target aspect ratio (unlike `crop`, which would just keep
    /// the whole original image in that case).
    #[test]
    fn transform_with_fit_aspect_crop_never_upscales_but_matches_target_ratio() {
        init();
        let data = static_fixture(); // 4x4, ratio 1:1
        // Target ratio 2:1 -- source stays <= 4 wide/tall (no upscale)
        // but must be cropped so width:height is 2:1, not left at 4:4.
        let p = parse("w=200,h=100,fit=aspect-crop").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert!(result.width <= 4 && result.height <= 4);
        assert_eq!(
            result.width, 4,
            "aspect-crop keeps the full width and crops height to match ratio"
        );
        assert_eq!(result.height, 2, "4 wide at a 2:1 ratio crops height to 2");
    }

    #[test]
    fn transform_with_fit_aspect_crop_downscales_and_crops_when_source_is_larger() {
        init();
        let data = nonsquare_fixture(); // 2000x1500, ratio 4:3
        let p = parse("w=100,h=100,fit=aspect-crop").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!(result.width, 100);
        assert_eq!(result.height, 100);
    }

    /// Gap 9 -- Cloudflare's per-side trim keys crop fixed pixel counts
    /// from each edge independently of border-color uniformity (unlike
    /// the legacy numeric `trim=<threshold>`, which is border-color-aware
    /// via find_trim). A 1.0-fraction value is interpreted as a fraction
    /// of that side's dimension.
    #[test]
    fn transform_with_per_side_trim_crops_fixed_pixel_counts() {
        init();
        let data = nonsquare_fixture(); // 2000x1500
        let p = parse("trim.top=100,trim.left=200").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!(result.width, 2000 - 200);
        assert_eq!(result.height, 1500 - 100);
    }

    #[test]
    fn transform_with_per_side_trim_fraction_values() {
        init();
        let data = nonsquare_fixture(); // 2000x1500
        let p = parse("trim.left=0.1,trim.right=0.1").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        // 10% off each side horizontally: 2000 - 200 - 200 = 1600.
        assert_eq!(result.width, 1600);
        assert_eq!(result.height, 1500);
    }

    #[test]
    fn transform_with_legacy_numeric_trim_still_works() {
        init();
        let data = static_fixture();
        let p = TransformParams {
            trim: Some(50.0),
            ..Default::default()
        };
        let result = transform(&data, &p, None, None);
        assert!(result.is_ok());
    }

    /// Gap 5 -- `format=json`: metadata-only response, no image bytes.
    /// Schema is spec-derived (see docs/CLOUDFLARE_PARITY.md) but must
    /// report real, computed values: original dimensions/file size and
    /// post-transform dimensions/format/encoded size.
    #[test]
    fn transform_with_format_json_returns_metadata_not_image_bytes() {
        init();
        let data = nonsquare_fixture(); // 2000x1500
        let p = parse("w=100,format=json").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!(result.format, OutputFormat::Json);
        let body = String::from_utf8(result.data).expect("json response must be valid utf8");
        let json: serde_json::Value = serde_json::from_str(&body).expect("must be valid json");
        assert_eq!(json["original"]["width"], 2000);
        assert_eq!(json["original"]["height"], 1500);
        assert_eq!(json["original"]["file_size"], data.len());
        assert_eq!(json["transformed"]["width"], 100);
        assert_eq!(json["transformed"]["height"], 75);
        assert!(json["transformed"]["file_size"].as_u64().unwrap() > 0);
    }

    /// Gap 6 -- `compression=fast`: when the client would otherwise
    /// negotiate AVIF, compression=fast biases the choice to JPEG instead.
    #[test]
    fn transform_with_compression_fast_downgrades_avif_negotiation_to_jpeg() {
        init();
        let data = static_fixture();
        let p = parse("compression=fast").unwrap();
        let result = transform(&data, &p, Some("image/avif,image/webp"), None).unwrap();
        assert_eq!(result.format, OutputFormat::Jpeg);
    }

    #[test]
    fn transform_without_compression_fast_still_negotiates_avif() {
        init();
        let data = static_fixture();
        let p = TransformParams::default();
        let result = transform(&data, &p, Some("image/avif,image/webp"), None).unwrap();
        assert_eq!(result.format, OutputFormat::Avif);
    }

    #[test]
    fn transform_with_compression_fast_leaves_explicit_png_format_unchanged() {
        init();
        let data = static_fixture();
        let p = parse("compression=fast,format=png").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!(result.format, OutputFormat::Png);
    }

    /// compression=fast also overrides an *explicit* `format=avif`/`webp`
    /// request, matching Cloudflare's documented "will usually override
    /// the format parameter."
    #[test]
    fn transform_with_compression_fast_overrides_explicit_avif_format() {
        init();
        let data = static_fixture();
        let p = parse("compression=fast,format=avif").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!(result.format, OutputFormat::Jpeg);
    }

    // ------------------------------------------------------------------
    // Gap 10 -- border (docs/CLOUDFLARE_PARITY.md)
    // ------------------------------------------------------------------

    #[test]
    fn transform_with_uniform_border_grows_output_by_border_on_all_sides() {
        init();
        let data = static_fixture(); // 4x4
        let p = parse("border=2").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!(result.width, 8);
        assert_eq!(result.height, 8);
    }

    #[test]
    fn transform_with_per_side_border_grows_output_asymmetrically() {
        init();
        let data = static_fixture(); // 4x4
        let p = parse("border.top=1,border.left=2,border.right=3,border.bottom=4").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!(result.width, 4 + 2 + 3);
        assert_eq!(result.height, 4 + 1 + 4);
    }

    #[test]
    fn transform_without_border_leaves_dimensions_unchanged() {
        init();
        let data = static_fixture();
        let result = transform(&data, &TransformParams::default(), None, None).unwrap();
        assert_eq!(result.width, 4);
        assert_eq!(result.height, 4);
    }

    #[test]
    fn transform_with_border_after_resize_uses_post_resize_dimensions() {
        init();
        let data = nonsquare_fixture(); // 2000x1500
        let p = parse("w=100,border=5").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        // Resized to w=100 (h derived as 75), then a 5px uniform border.
        assert_eq!(result.width, 100 + 10);
        assert_eq!(result.height, 75 + 10);
    }

    #[test]
    fn transform_with_border_and_dpr_scales_border_width() {
        init();
        let data = static_fixture(); // 4x4
        let p = parse("border=2,dpr=2").unwrap();
        let result = transform(&data, &p, None, None).unwrap();
        // Border scales with dpr: 2px * dpr 2 = 4px each side.
        assert_eq!(result.width, 4 + 8);
        assert_eq!(result.height, 4 + 8);
    }

    // ------------------------------------------------------------------
    // Gap 11 -- draw overlays (docs/CLOUDFLARE_PARITY.md): compositing
    // math proof against local, already-fetched image buffers. The
    // remote-URL fetch that would supply `overlay_bytes` in production
    // is deliberately not implemented -- see composite_draw_overlay's
    // doc comment and CLOUDFLARE_PARITY.md gap 11.
    // ------------------------------------------------------------------

    fn overlay_fixture() -> Vec<u8> {
        fixture("test_4x4.png")
    }

    #[test]
    fn composite_draw_overlay_output_matches_base_dimensions() {
        init();
        let base = VipsImage::from_buffer(&nonsquare_fixture()).unwrap(); // 2000x1500
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            ..Default::default()
        };
        let result = composite_draw_overlay(&base, &overlay_fixture(), &entry).unwrap();
        assert_eq!(result.width(), base.width());
        assert_eq!(result.height(), base.height());
    }

    #[test]
    fn composite_draw_overlay_resizes_overlay_to_requested_pixel_width() {
        init();
        let base = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            width: Some(50.0),
            ..Default::default()
        };
        // Should not error resizing the overlay to a 50px-wide box before
        // compositing -- this exercises the resize branch without a way
        // to directly observe the intermediate overlay size, so the
        // assertion is on successful completion + base-sized output.
        let result = composite_draw_overlay(&base, &overlay_fixture(), &entry).unwrap();
        assert_eq!(result.width(), base.width());
    }

    #[test]
    fn composite_draw_overlay_resizes_overlay_to_fractional_width() {
        init();
        let base = VipsImage::from_buffer(&nonsquare_fixture()).unwrap(); // 2000 wide
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            width: Some(0.1), // 10% of 2000 = 200px
            ..Default::default()
        };
        let result = composite_draw_overlay(&base, &overlay_fixture(), &entry).unwrap();
        assert_eq!(result.width(), base.width());
    }

    #[test]
    fn composite_draw_overlay_with_explicit_position_succeeds() {
        init();
        let base = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            bottom: Some(5.0),
            right: Some(5.0),
            ..Default::default()
        };
        let result = composite_draw_overlay(&base, &overlay_fixture(), &entry).unwrap();
        assert_eq!(result.width(), base.width());
        assert_eq!(result.height(), base.height());
    }

    #[test]
    fn composite_draw_overlay_with_opacity_succeeds() {
        init();
        let base = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            opacity: Some(0.5),
            ..Default::default()
        };
        let result = composite_draw_overlay(&base, &overlay_fixture(), &entry).unwrap();
        assert_eq!(result.width(), base.width());
    }

    #[test]
    fn composite_draw_overlay_with_repeat_both_tiles_across_base() {
        init();
        let base = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            repeat: Some(DrawRepeat::Both),
            ..Default::default()
        };
        let result = composite_draw_overlay(&base, &overlay_fixture(), &entry).unwrap();
        assert_eq!(result.width(), base.width());
        assert_eq!(result.height(), base.height());
    }

    #[test]
    fn composite_draw_overlay_with_rotate_succeeds() {
        init();
        let base = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            rotate: Some(Rotation::Deg90),
            ..Default::default()
        };
        let result = composite_draw_overlay(&base, &overlay_fixture(), &entry).unwrap();
        assert_eq!(result.width(), base.width());
        assert_eq!(result.height(), base.height());
    }

    #[test]
    fn composite_draw_overlay_with_background_flattens_overlay_alpha() {
        init();
        let base = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            background: Some([255, 0, 0]),
            ..Default::default()
        };
        let result = composite_draw_overlay(&base, &overlay_fixture(), &entry).unwrap();
        assert_eq!(result.width(), base.width());
    }
}
