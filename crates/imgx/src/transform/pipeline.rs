//! Transform pipeline: probe -> budget check -> decide -> reload -> extract
//! frame -> trim -> rotate/flip -> resize -> effects -> background ->
//! encode. Ported from src/transform/pipeline.zig. The animated+cover
//! resize workaround (INV-2) and GIF pre-encode safety check (INV-3) are
//! the highest-risk pieces of this entire rewrite — see docs/INVARIANTS.md.

use thiserror::Error;

use imgx_vips::{DEFAULT_AVIF_EFFORT, ThumbnailOptions, VipsError, VipsImage, consts};

use super::negotiate;
use super::params::{
    CompressionMode, DrawOverlay, DrawRepeat, FitMode, FlipMode, Gravity, MetadataMode,
    OutputFormat, Rotation, TransformParams,
};
use super::thumbhash::{self, ThumbhashError};

/// Why `transform` failed.
#[derive(Debug, Error)]
pub enum TransformError {
    #[error(transparent)]
    Vips(#[from] VipsError),
    #[error("source image exceeds the configured pixel budget ({0} > {1})")]
    ExceedsMaxPixels(u64, u64),
    #[error(transparent)]
    Thumbhash(#[from] ThumbhashError),
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
    /// True when the resize ran the reduced decode of the source bytes.
    /// Tests read it. No response header or body carries it. See INV-21.
    pub decoded_at_reduced_size: bool,
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

/// Server-wide encoder settings. They apply to every request and no
/// request parameter changes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderSettings {
    pub avif_effort: u8,
}

impl Default for EncoderSettings {
    fn default() -> Self {
        Self {
            avif_effort: DEFAULT_AVIF_EFFORT,
        }
    }
}

/// The server-wide settings for one transform. They hold the safety
/// `limits` and the `encoder` settings.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TransformSettings {
    pub limits: TransformLimits,
    pub encoder: EncoderSettings,
}

/// The settings that `encode_image` uses for one image. They hold the
/// request `quality` and `metadata` mode, and the server-wide `avif_effort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeOptions {
    pub quality: u8,
    pub avif_effort: u8,
    pub metadata: MetadataMode,
}

impl EncodeOptions {
    /// Build the options for one request. `quality` and `metadata` come from
    /// the request. `avif_effort` comes from the server-wide `encoder`.
    pub fn new(tp: &TransformParams, encoder: EncoderSettings) -> Self {
        Self {
            quality: tp.quality,
            avif_effort: encoder.avif_effort,
            metadata: tp.metadata,
        }
    }
}

/// Execute the full transform pipeline: decode -> resize -> effects -> encode.
///
/// `input_data` is the raw bytes of the source image. `tp` controls the
/// resize/effect/encode behavior. `accept_header` is used for format
/// negotiation when `tp.format` is `None`/`Auto`. `None` for `settings`
/// selects the default limits and encoder settings.
pub fn transform(
    input_data: &[u8],
    tp: &TransformParams,
    accept_header: Option<&str>,
    settings: Option<TransformSettings>,
) -> Result<TransformResult, TransformError> {
    let settings = settings.unwrap_or_default();
    let encode_options = EncodeOptions::new(tp, settings.encoder);
    let compression_fast = tp.compression == Some(CompressionMode::Fast);
    // Only `format=thumbhash` loads the source strictly (INV-19): a hash of
    // the blank pixels of a partly decoded source is wrong. Every other
    // format keeps the default load.
    let strict = tp.format == Some(OutputFormat::Thumbhash);

    // -- PROBE --
    // Load first frame only (cheap) to detect animation metadata.
    let mut current = load_first_frame(input_data, strict)?;

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
    if first_frame_pixels > settings.limits.max_pixels {
        return Err(TransformError::ExceedsMaxPixels(
            first_frame_pixels,
            settings.limits.max_pixels,
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
        (frame_w * page_h * frame_count) > settings.limits.max_animated_pixels
    } else {
        false
    };

    // Effective frame count after clamping to max_frames.
    let effective_pages: Option<i32> = if is_animated && !over_budget {
        Some(
            n_pages
                .expect("invariant: is_animated is only true when n_pages_of() returned Some")
                .min(settings.limits.max_frames as i32),
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
            load_pages(input_data, effective_pages, strict)?
        } else {
            load_pages(input_data, -1, strict)?
        };
    }

    // -- EXTRACT FRAME --
    // A specific frame requested on an animated source: extract it and
    // proceed as static from here on (animated_output is already false
    // in this case, since DECIDE required tp.frame.is_none()).
    if let (Some(frame_idx), true) = (tp.frame, is_animated) {
        if !animated_output {
            current = load_pages(input_data, -1, strict)?;
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

    // A JPEG or WebP source decodes at reduced size when the resize reads
    // the source bytes directly. That works only if no earlier stage changed
    // `current`. See `resize_reads_source`.
    let resize_source = (resize_reads_source(tp, animated_output) && current.shrinks_on_load())
        .then_some(input_data);

    // -- RESIZE --
    let mut decoded_at_reduced_size = false;
    let eff_w = tp.effective_width().map(|w| w as i32);
    let eff_h = tp.effective_height().map(|h| h as i32);

    if eff_w.is_some() || eff_h.is_some() {
        let source_w = current.width();
        let source_h = current.height();
        // The thumbnail box applies to the image after the EXIF orientation,
        // so a derived side needs the oriented aspect ratio.
        let (oriented_w, oriented_h) = oriented_size(&current);

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
                let ratio = oriented_w as f64 / oriented_h as f64;
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
        // That ratio uses the oriented dimensions (see `oriented_size`).
        let derived_height = if eff_h.is_none()
            && !matches!(
                effective_fit,
                FitMode::Cover | FitMode::Fill | FitMode::Crop | FitMode::AspectCrop
            )
            && oriented_w > 0
        {
            let ratio = oriented_h as f64 / oriented_w as f64;
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
            //
            // The scale and the crop use the oriented dimensions, because
            // `vips_thumbnail_*` applies the EXIF orientation first. The
            // direct crop turns the pixels upright first, so the crop runs on
            // the displayed image and the output carries no orientation tag.
            let hscale = tw as f64 / oriented_w as f64;
            let vscale = th as f64 / oriented_h as f64;
            let scale = hscale.max(vscale);
            if scale > 1.0 {
                current = current.autorot()?;
                let target_ratio = tw as f64 / th as f64;
                let source_ratio = oriented_w as f64 / oriented_h as f64;
                let (crop_w, crop_h) = if source_ratio > target_ratio {
                    let new_w = ((oriented_h as f64 * target_ratio).round() as i32)
                        .min(oriented_w)
                        .max(1);
                    (new_w, oriented_h)
                } else {
                    let new_h = ((oriented_w as f64 / target_ratio).round() as i32)
                        .min(oriented_h)
                        .max(1);
                    (oriented_w, new_h)
                };
                let crop_left = (oriented_w - crop_w) / 2;
                let crop_top = (oriented_h - crop_h) / 2;
                current = current.crop(crop_left, crop_top, crop_w, crop_h)?;
            } else {
                let opts = ThumbnailOptions {
                    height: Some(th),
                    crop: Some(map_gravity_to_crop(tp.gravity)),
                    size: Some(consts::VIPS_SIZE_DOWN),
                    ..Default::default()
                };
                (current, decoded_at_reduced_size) =
                    resize(&current, resize_source, None, tw, opts)?;
            }
        } else {
            let opts = build_thumbnail_options(effective_fit, tp.gravity, derived_height);
            (current, decoded_at_reduced_size) = resize(
                &current,
                resize_source,
                Some((oriented_w, oriented_h)),
                thumb_width,
                opts,
            )?;

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
        let encoded = encode_image(&current, negotiated, &encode_options)?;
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
            decoded_at_reduced_size,
        });
    }

    // -- THUMBHASH -- (imgx extension: the body is the base64 ThumbHash of
    // the transformed image. INV-11 and the animated budget check ran
    // above, so a source over budget never reaches this block. `validate()`
    // rejects `draw` for this format, so no overlay changes the pixels
    // after this point.)
    if tp.format == Some(OutputFormat::Thumbhash) {
        let (hash_width, hash_height, rgba) = thumbhash_rgba(&current)?;
        let hash = thumbhash::encode(hash_width, hash_height, &rgba)?;
        return Ok(TransformResult {
            data: thumbhash::to_base64(&hash).into_bytes(),
            format: OutputFormat::Thumbhash,
            width: out_width,
            height: out_height,
            is_animated: false,
            frame_count: None,
            decoded_at_reduced_size,
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

    let data = encode_image(&current, output_format, &encode_options)?;

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
        decoded_at_reduced_size,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Render `image` into an 8-bit RGBA raster of at most 100x100 pixels for
/// the ThumbHash encoder. Returns the raster width, height, and bytes.
/// `thumbnail` keeps the aspect ratio, never upscales, and converts CMYK,
/// 16-bit, and float sources to 8-bit sRGB. It ignores the EXIF orientation
/// tag, so the raster has the orientation of `image`, the pixels that every
/// other output format encodes.
fn thumbhash_rgba(image: &VipsImage) -> Result<(u32, u32, Vec<u8>), TransformError> {
    let side = thumbhash::MAX_SIDE as i32;
    let small = image.thumbnail(
        side,
        ThumbnailOptions {
            height: Some(side),
            crop: None,
            size: Some(consts::VIPS_SIZE_DOWN),
            no_rotate: true,
        },
    )?;
    let bytes = small.write_to_memory()?;
    let (width, height) = (small.width() as u32, small.height() as u32);
    let rgba = thumbhash::rgba_from_bands(&bytes, width, height, small.bands() as u32)?;
    Ok((width, height, rgba))
}

/// Load the first frame of `data`. With `strict`, libvips stops at the
/// first decode error instead of returning blank pixels for the part of the
/// source that it cannot decode. The pixels are read later from this same
/// image, so the strict setting covers them.
fn load_first_frame(data: &[u8], strict: bool) -> Result<VipsImage, VipsError> {
    if strict {
        VipsImage::from_buffer_strict(data)
    } else {
        VipsImage::from_buffer(data)
    }
}

/// Load `n` pages of `data` (`n = -1` loads all), with the same `strict`
/// meaning as `load_first_frame`.
fn load_pages(data: &[u8], n: i32, strict: bool) -> Result<VipsImage, VipsError> {
    if strict {
        VipsImage::from_buffer_animated_strict(data, n)
    } else {
        VipsImage::from_buffer_animated(data, n)
    }
}

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

/// Return the width and height of `image` after libvips applies the EXIF
/// orientation. `vips_thumbnail_*` applies it first, so a resize box fits the
/// turned image. Orientations 5 to 8 turn the image by 90 degrees, so they
/// swap the sides. The resize stage always applies the tag: the request has
/// no `no_rotate` option. The `aspect-crop` direct crop calls `autorot` to
/// match.
fn oriented_size(image: &VipsImage) -> (i32, i32) {
    let (width, height) = (image.width(), image.height());
    match image.get_int("orientation") {
        Some(5..=8) => (height, width),
        _ => (width, height),
    }
}

/// Convert an optional RGB byte triplet to an f64 background array.
/// Defaults to white when no color is specified.
fn bg_color_from_params(background: Option<[u8; 3]>) -> [f64; 3] {
    match background {
        Some(rgb) => [rgb[0] as f64, rgb[1] as f64, rgb[2] as f64],
        None => [255.0, 255.0, 255.0],
    }
}

/// Return true when the resize can decode the source bytes again, at reduced
/// size. That is only right when no stage before the resize changes
/// `current`, and when the crop window does not depend on the pixels that the
/// reduced decode changes. Keep this list in step with the stages of
/// `transform` that run before the resize.
///
/// `format=thumbhash` never runs the reduced decode. It decodes the source at
/// full size, with the strict load of INV-19. A strict reduced decode is not
/// reliable: on libvips 8.15.1, `fail_on=error` in `vips_thumbnail_source` can
/// let a truncated JPEG through. A hash of blank pixels then follows.
///
/// A parameter whose value leaves the image as it is does not stop the
/// reduced decode. These are `rotate=0` and a per-side trim of exactly 0. A
/// numeric `trim`, a `flip`, and a `frame` stay blockers for every value:
/// `trim` removes a uniform border at any threshold, a `flip` always turns
/// the image, and `frame` is a no-op only for a source with one frame,
/// which this function cannot tell.
///
/// `gravity=smart` and `gravity=attention` pick the crop window from the
/// image content. A reduced decode of a WebP source changes that content,
/// and the window can move by tens of pixels. These requests decode at full
/// size. The gravity matters only for the fits that crop.
fn resize_reads_source(tp: &TransformParams, animated_output: bool) -> bool {
    tp.format != Some(OutputFormat::Thumbhash)
        && !animated_output
        && tp.frame.is_none()
        && tp.trim.is_none()
        && !trims_a_side(tp)
        && tp.rotate.is_none_or(|rotation| rotation == Rotation::Deg0)
        && tp.flip.is_none()
        && !crops_by_content(tp)
}

/// Return true when a per-side trim removes at least one pixel row or
/// column. A value of exactly 0 removes nothing, whatever the image size.
fn trims_a_side(tp: &TransformParams) -> bool {
    [tp.trim_top, tp.trim_right, tp.trim_bottom, tp.trim_left]
        .into_iter()
        .flatten()
        .any(|value| value != 0.0)
}

/// Return true when the fit crops to the box and the gravity picks the crop
/// window from the image content.
fn crops_by_content(tp: &TransformParams) -> bool {
    matches!(tp.fit, FitMode::Cover | FitMode::Crop | FitMode::AspectCrop)
        && matches!(tp.gravity, Gravity::Smart | Gravity::Attention)
}

/// Return the size that `vips_thumbnail_image` gives a `contain` box on a
/// full-size image. `source` is the size of the image after the EXIF
/// orientation. `target` is the box, and it sets the sides of the box in
/// the same space. This is the size of the full decode for `contain`,
/// `inside`, and `pad`.
///
/// libvips picks one shrink factor for both sides: the larger of the two
/// ratios, and at least 1. It clamps the factor to each side. Then it gives
/// each side to its reduce step as the reciprocal of a reciprocal, and rounds
/// the result to the nearest integer. The steps below follow that maths,
/// including the two divisions, so the rounding matches bit for bit.
fn contain_size(source: (i32, i32), target: (i32, i32)) -> (i32, i32) {
    let shrink = (source.0 as f64 / target.0 as f64)
        .max(source.1 as f64 / target.1 as f64)
        .max(1.0);
    (
        reduced_side(source.0, shrink),
        reduced_side(source.1, shrink),
    )
}

fn reduced_side(length: i32, shrink: f64) -> i32 {
    let length = length as f64;
    let reduce = 1.0 / (1.0 / shrink.min(length));
    (length / reduce + 0.5) as i32
}

/// Return the size that the full decode gives when the fit is `contain`,
/// `inside`, or `pad`, and `None` for every other request. `cover` and `crop`
/// crop to the box, and `fill` and `outside` give the same size on both
/// decode paths. A request without a box height has no box to check, so it
/// also gives `None`. `oriented` is the size of the source after the EXIF
/// orientation. The pipeline never sets `no_rotate`, so the box holds
/// oriented dimensions.
fn full_decode_size(
    oriented: (i32, i32),
    width: i32,
    opts: ThumbnailOptions,
) -> Option<(i32, i32)> {
    if opts.crop.is_some() || opts.size != Some(consts::VIPS_SIZE_DOWN) {
        return None;
    }
    Some(contain_size(oriented, (width, opts.height?)))
}

/// Resize `current` to fit `width`. With `source` set, run the reduced
/// decode of those bytes (`VipsImage::thumbnail_buffer`). Otherwise shrink
/// the full-size `current`. The flag in the result is true when the resize
/// decoded `source`. `oriented` is the size of `current` after the EXIF
/// orientation. A caller that needs no size check passes `None`.
///
/// A reduced decode measures its shrink factor on the smaller image that the
/// decoder returns, so a `contain` request can land 1 px away from the full
/// decode. The size of a lazy image is known before libvips reads any pixel.
/// When it differs from `full_decode_size`, the resize runs again with `size`
/// set to `force` and the exact size. A request that already has the right
/// size keeps its bytes.
fn resize(
    current: &VipsImage,
    source: Option<&[u8]>,
    oriented: Option<(i32, i32)>,
    width: i32,
    opts: ThumbnailOptions,
) -> Result<(VipsImage, bool), VipsError> {
    let Some(bytes) = source else {
        return current.thumbnail(width, opts).map(|image| (image, false));
    };
    let image = VipsImage::thumbnail_buffer(bytes, width, opts)?;
    let image = match oriented.and_then(|size| full_decode_size(size, width, opts)) {
        Some((box_width, box_height))
            if (image.width(), image.height()) != (box_width, box_height) =>
        {
            let exact = ThumbnailOptions {
                height: Some(box_height),
                size: Some(consts::VIPS_SIZE_FORCE),
                ..opts
            };
            VipsImage::thumbnail_buffer(bytes, box_width, exact)?
        }
        _ => image,
    };
    Ok((image, true))
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
/// proves the libvips compositing math against local image buffers,
/// independent of how those bytes were obtained. The real remote-URL
/// fetch that supplies `overlay_bytes` for a live request goes through
/// the SSRF-safe `origin::RemoteFetcher` (gap 2), wired up in
/// `server.rs`'s `handle_image_request` and `apply_draw_overlays` below.
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
/// `pub(crate)` (rather than private) so `server.rs` can re-encode a base
/// image after compositing draw overlays fetched post-transform -- see
/// the draw-overlay wiring in `server.rs`'s `handle_image_request`
/// (Cloudflare parity gap 11).
pub(crate) fn encode_image(
    image: &VipsImage,
    format: OutputFormat,
    options: &EncodeOptions,
) -> Result<Vec<u8>, VipsError> {
    let q = options.quality as i32;
    // Strip -> strip all metadata; Keep/Copyright -> preserve metadata
    // (libvips has no "copyright-only" mode, so Copyright is treated the
    // same as Keep for now, matching the Zig implementation's known
    // future-enhancement note).
    let do_strip = options.metadata == MetadataMode::Strip;
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
        OutputFormat::Avif => image.save_avif(q, options.avif_effort, do_strip),
        OutputFormat::Gif => encode_gif(image),
        // Never reached: `transform()` intercepts `format == Json` and
        // `format == Thumbhash` before calling `encode_image` and builds a
        // text payload instead (see the `-- JSON --` and `-- THUMBHASH --`
        // blocks above).
        OutputFormat::Json | OutputFormat::Thumbhash => image.save_jpeg(q, do_strip),
    }
}

/// Composite already-fetched `draw` overlay bytes onto an already-
/// transformed `TransformResult`, then re-encode with the same
/// format/quality/metadata the base transform used. Called from
/// `server.rs`'s `handle_image_request` after `transform()` and after
/// every overlay URL has been fetched through the SSRF-safe
/// `origin::RemoteFetcher` (gap 2), gated on `IMGX_ALLOW_DRAW_OVERLAYS`
/// (gap 11).
///
/// A no-op (returns `result` unchanged) when there is nothing to
/// composite, when the output is animated (compositing onto a
/// vertically-stacked animated frame buffer would corrupt frame
/// boundaries -- the same INV-2 concern the BORDER stage already
/// documents), or when the output is a `format=json` metadata response
/// or a `format=thumbhash` text response (no image bytes to composite
/// onto).
pub fn apply_draw_overlays(
    result: TransformResult,
    draw: &[DrawOverlay],
    overlay_bytes: &[Vec<u8>],
    options: &EncodeOptions,
) -> Result<TransformResult, TransformError> {
    if draw.is_empty() || overlay_bytes.is_empty() || result.is_animated {
        return Ok(result);
    }
    if result.format.produces_text_body() {
        return Ok(result);
    }

    let mut base = VipsImage::from_buffer(&result.data)?;
    let mut bytes_iter = overlay_bytes.iter();
    for entry in draw {
        if entry.url.is_none() {
            continue;
        }
        let Some(bytes) = bytes_iter.next() else {
            break;
        };
        base = composite_draw_overlay(&base, bytes, entry)?;
    }

    let data = encode_image(&base, result.format, options)?;
    Ok(TransformResult { data, ..result })
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
    use super::super::thumbhash::{
        self,
        test_support::{BODY_CHARS, HASH_BYTES, base64_decode, is_standard_base64},
    };
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::Once;

    static VIPS_INIT: Once = Once::new();

    fn init() {
        VIPS_INIT.call_once(|| imgx_vips::init().expect("vips init"));
    }

    fn test_encode_options() -> EncodeOptions {
        EncodeOptions {
            quality: 80,
            avif_effort: DEFAULT_AVIF_EFFORT,
            metadata: MetadataMode::Strip,
        }
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
        let err = transform(
            &data,
            &TransformParams::default(),
            None,
            Some(TransformSettings {
                limits,
                ..Default::default()
            }),
        )
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
        assert!(
            transform(
                &data,
                &TransformParams::default(),
                None,
                Some(TransformSettings {
                    limits,
                    ..Default::default()
                })
            )
            .is_ok()
        );
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
        let result = transform(
            &data,
            &p,
            Some("image/gif"),
            Some(TransformSettings {
                limits: cfg,
                ..Default::default()
            }),
        )
        .unwrap();
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
        let result = transform(
            &data,
            &p,
            Some("image/gif"),
            Some(TransformSettings {
                limits: cfg,
                ..Default::default()
            }),
        )
        .unwrap();
        assert!(!result.data.is_empty());
        assert!(result.is_animated);
        assert_eq!(result.frame_count, Some(3));
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
    // math proof against local, already-fetched image buffers. The real
    // remote-URL fetch that supplies `overlay_bytes` in production goes
    // through origin::RemoteFetcher (gap 2) -- see apply_draw_overlays'
    // own tests further down for the post-transform integration point.
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

    // ------------------------------------------------------------------
    // apply_draw_overlays -- the post-transform integration point called
    // from server.rs after every overlay URL has been fetched through
    // the SSRF-safe origin::RemoteFetcher (gap 2). These tests operate
    // on a real TransformResult from transform() itself, proving the
    // composite-then-re-encode round trip end to end.
    // ------------------------------------------------------------------

    #[test]
    fn apply_draw_overlays_composites_and_reencodes_preserving_dimensions() {
        init();
        let data = nonsquare_fixture();
        let base_result = transform(&data, &TransformParams::default(), None, None).unwrap();
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            ..Default::default()
        };
        let composited = apply_draw_overlays(
            base_result.clone(),
            std::slice::from_ref(&entry),
            &[overlay_fixture()],
            &test_encode_options(),
        )
        .unwrap();
        assert_eq!(composited.width, base_result.width);
        assert_eq!(composited.height, base_result.height);
        assert_ne!(composited.data, base_result.data);
        // Re-decodable -- proves it's a real, valid re-encoded image, not
        // just the base bytes passed through unchanged.
        let decoded = VipsImage::from_buffer(&composited.data).unwrap();
        assert_eq!(decoded.width(), composited.width as i32);
    }

    #[test]
    fn apply_draw_overlays_is_a_no_op_with_no_overlays() {
        init();
        let data = nonsquare_fixture();
        let base_result = transform(&data, &TransformParams::default(), None, None).unwrap();
        let result =
            apply_draw_overlays(base_result.clone(), &[], &[], &test_encode_options()).unwrap();
        assert_eq!(result.data, base_result.data);
    }

    #[test]
    fn apply_draw_overlays_skips_animated_output() {
        init();
        let data = fixture("loading.gif");
        let p = TransformParams {
            width: Some(50),
            ..Default::default()
        };
        let base_result = transform(&data, &p, Some("image/gif"), None).unwrap();
        assert!(base_result.is_animated);
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            ..Default::default()
        };
        let result = apply_draw_overlays(
            base_result.clone(),
            std::slice::from_ref(&entry),
            &[overlay_fixture()],
            &test_encode_options(),
        )
        .unwrap();
        assert_eq!(result.data, base_result.data);
    }

    #[test]
    fn apply_draw_overlays_skips_json_output() {
        init();
        let data = nonsquare_fixture();
        let p = TransformParams {
            format: Some(OutputFormat::Json),
            ..Default::default()
        };
        let base_result = transform(&data, &p, None, None).unwrap();
        assert_eq!(base_result.format, OutputFormat::Json);
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            ..Default::default()
        };
        let result = apply_draw_overlays(
            base_result.clone(),
            std::slice::from_ref(&entry),
            &[overlay_fixture()],
            &test_encode_options(),
        )
        .unwrap();
        assert_eq!(result.data, base_result.data);
    }

    fn thumbhash_result(data: &[u8], params: &str, accept: Option<&str>) -> TransformResult {
        let p = parse(params).unwrap();
        transform(data, &p, accept, None).unwrap()
    }

    fn thumbhash_bytes(result: &TransformResult) -> Vec<u8> {
        base64_decode(std::str::from_utf8(&result.data).expect("thumbhash body is ascii"))
    }

    fn hash_is_landscape(hash: &[u8]) -> bool {
        hash[4] & 0x80 != 0
    }

    fn hash_has_alpha(hash: &[u8]) -> bool {
        hash[2] & 0x80 != 0
    }

    #[test]
    fn transform_with_format_thumbhash_returns_base64_text_not_image_bytes() {
        init();
        let data = nonsquare_fixture();
        let result = thumbhash_result(&data, "format=thumbhash", None);
        assert_eq!(result.format, OutputFormat::Thumbhash);
        let text = std::str::from_utf8(&result.data).expect("body must be ascii text");
        assert!(BODY_CHARS.contains(&text.len()), "length {}", text.len());
        assert!(is_standard_base64(text), "{text}");
        let hash = base64_decode(text);
        assert!(
            HASH_BYTES.contains(&hash.len()),
            "hash length {}",
            hash.len()
        );
        assert_eq!(thumbhash::to_base64(&hash), text);
        assert!(!result.is_animated);
        assert_eq!(result.frame_count, None);
        assert_eq!((result.width, result.height), (2000, 1500));
    }

    #[test]
    fn transform_thumbhash_reflects_output_aspect_ratio_of_resize() {
        init();
        let data = nonsquare_fixture();
        let landscape = thumbhash_result(&data, "format=thumbhash", None);
        assert!(hash_is_landscape(&thumbhash_bytes(&landscape)));

        let portrait = thumbhash_result(&data, "format=thumbhash,w=100,h=300,fit=cover", None);
        assert_eq!((portrait.width, portrait.height), (100, 300));
        let hash = thumbhash_bytes(&portrait);
        assert!(!hash_is_landscape(&hash));
        assert_eq!(hash[3] & 7, 2, "lx of a 1:3 image");

        let rotated = thumbhash_result(&data, "format=thumbhash,rotate=90", None);
        assert_eq!((rotated.width, rotated.height), (1500, 2000));
        assert!(!hash_is_landscape(&thumbhash_bytes(&rotated)));
    }

    #[test]
    fn transform_thumbhash_rejects_source_exceeding_max_pixels() {
        init();
        let data = static_fixture();
        let limits = TransformLimits {
            max_pixels: 10,
            ..Default::default()
        };
        let p = parse("format=thumbhash").unwrap();
        let err = transform(
            &data,
            &p,
            None,
            Some(TransformSettings {
                limits,
                ..Default::default()
            }),
        )
        .expect_err("16-pixel source must be rejected under a 10-pixel budget");
        assert!(matches!(err, TransformError::ExceedsMaxPixels(16, 10)));
    }

    #[test]
    fn transform_thumbhash_accepts_source_within_max_pixels() {
        init();
        let data = static_fixture();
        let limits = TransformLimits {
            max_pixels: 16,
            ..Default::default()
        };
        let p = parse("format=thumbhash").unwrap();
        let result = transform(
            &data,
            &p,
            None,
            Some(TransformSettings {
                limits,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(result.format, OutputFormat::Thumbhash);
        assert!(HASH_BYTES.contains(&thumbhash_bytes(&result).len()));
    }

    #[test]
    fn transform_thumbhash_reads_at_most_100_pixels_per_side() {
        init();
        let data = nonsquare_fixture();
        let big = VipsImage::from_buffer(&data)
            .unwrap()
            .thumbnail(
                4000,
                ThumbnailOptions {
                    height: Some(3000),
                    crop: None,
                    size: Some(consts::VIPS_SIZE_FORCE),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!((big.width(), big.height()), (4000, 3000));
        let (w, h, rgba) = thumbhash_rgba(&big).unwrap();
        assert_eq!((w, h), (100, 75));
        assert_eq!(rgba.len(), 4 * 100 * 75);
        assert!(rgba.len() <= 40_000);

        let result = thumbhash_result(&data, "format=thumbhash,w=4000,h=3000,fit=fill", None);
        assert_eq!((result.width, result.height), (4000, 3000));
        assert!(result.data.len() <= 36);
    }

    #[test]
    fn transform_thumbhash_is_independent_of_accept_quality_metadata_and_onerror() {
        init();
        let data = nonsquare_fixture();
        let baseline = thumbhash_result(&data, "format=thumbhash,w=300", None).data;
        for (params, accept) in [
            (
                "format=thumbhash,w=300,q=5,metadata=keep",
                Some("image/avif"),
            ),
            (
                "format=thumbhash,w=300,q=100,metadata=strip,compression=fast",
                Some("image/webp,image/png"),
            ),
            ("format=thumbhash,w=300,anim=static", Some("*/*")),
            ("format=thumbhash,w=300,onerror=redirect", None),
            (
                "fmt=thumbhash,w=300,q=1,metadata=copyright",
                Some("image/jpeg"),
            ),
        ] {
            let other = thumbhash_result(&data, params, accept).data;
            assert_eq!(other, baseline, "{params} with accept {accept:?}");
        }
    }

    #[test]
    fn transform_thumbhash_of_animated_gif_uses_first_frame_and_is_not_animated() {
        init();
        let data = fixture("loading.gif");
        let first_frame = VipsImage::from_buffer(&data).unwrap();
        assert!(n_pages_of(&first_frame).is_some_and(|n| n > 1));
        let (frame_w, frame_h) = (first_frame.width() as u32, first_frame.height() as u32);

        let result = thumbhash_result(&data, "format=thumbhash", Some("image/webp,image/gif"));
        assert_eq!(result.format, OutputFormat::Thumbhash);
        assert!(!result.is_animated);
        assert_eq!(result.frame_count, None);
        assert_eq!((result.width, result.height), (frame_w, frame_h));
        assert!(HASH_BYTES.contains(&thumbhash_bytes(&result).len()));

        let explicit = thumbhash_result(&data, "format=thumbhash,frame=0", None);
        assert_eq!(explicit.data, result.data);
    }

    #[test]
    fn transform_thumbhash_of_four_band_opaque_source_clears_alpha_flag() {
        init();
        let data = static_fixture();
        assert_eq!(VipsImage::from_buffer(&data).unwrap().bands(), 4);
        let result = thumbhash_result(&data, "format=thumbhash", None);
        assert!(!hash_has_alpha(&thumbhash_bytes(&result)));
    }

    #[test]
    fn transform_thumbhash_of_transparent_source_sets_alpha_flag_and_matches_reference_vector() {
        init();
        let data = fixture("alpha_4x4.png");
        let source = VipsImage::from_buffer(&data).unwrap();
        assert_eq!(source.bands(), 4);
        let pixels = source.write_to_memory().unwrap();
        assert!(
            pixels.as_chunks::<4>().0.iter().any(|px| px[3] < 255),
            "the fixture must hold pixels with alpha below 255"
        );
        let result = thumbhash_result(&data, "format=thumbhash", None);
        assert!(hash_has_alpha(&thumbhash_bytes(&result)));
        // `rgbaToThumbHash` of thumbhash@0.1.1 on the same 4x4 RGBA pixels.
        assert_eq!(
            std::str::from_utf8(&result.data).unwrap(),
            "JOmFLQ44l3eweHx3d7DGCOpJYnhwiJeIdw=="
        );
    }

    #[test]
    fn transform_thumbhash_of_cmyk_source_succeeds() {
        init();
        let data = fixture("cmyk.jpg");
        let result = thumbhash_result(&data, "format=thumbhash", None);
        assert_eq!(result.format, OutputFormat::Thumbhash);
        let hash = thumbhash_bytes(&result);
        assert!(HASH_BYTES.contains(&hash.len()));
        assert!(
            !hash_has_alpha(&hash),
            "the K channel must not be read as alpha"
        );
    }

    #[test]
    fn transform_thumbhash_landscape_bit_matches_result_dimensions_for_exif_source() {
        init();
        let data = fixture("exif_orientation.jpg");
        for params in [
            "format=thumbhash",
            "format=thumbhash,rotate=90",
            "format=thumbhash,rotate=180",
            "format=thumbhash,border=1",
        ] {
            let result = thumbhash_result(&data, params, None);
            let hash = thumbhash_bytes(&result);
            assert_eq!(
                hash_is_landscape(&hash),
                result.width > result.height,
                "{params}: result is {}x{}",
                result.width,
                result.height
            );
            let png_params = params.replace("format=thumbhash", "format=png");
            let png = thumbhash_result(&data, &png_params, None);
            assert_eq!(
                (result.width, result.height),
                (png.width, png.height),
                "{params}: hash and png results must report the same size"
            );
        }
    }

    fn hash_of_pixels_in(encoded: &[u8]) -> Vec<u8> {
        let image = VipsImage::from_buffer(encoded).unwrap();
        let (width, height) = (image.width() as u32, image.height() as u32);
        let bytes = image.write_to_memory().unwrap();
        let rgba = thumbhash::rgba_from_bands(&bytes, width, height, image.bands() as u32).unwrap();
        thumbhash::encode(width, height, &rgba).unwrap()
    }

    #[test]
    fn transform_thumbhash_landscape_bit_follows_the_hashed_thumbnail_for_near_square_output() {
        init();
        let data = nonsquare_fixture();
        let landscape = |params: &str| {
            let result = thumbhash_result(&data, params, None);
            (
                hash_is_landscape(&thumbhash_bytes(&result)),
                result.width > result.height,
            )
        };
        // The 100x100 thumbnail of a 4000x3999 output has equal sides, so
        // the bit is clear although the result is wider than it is tall.
        assert_eq!(
            landscape("format=thumbhash,w=4000,h=3999,fit=fill"),
            (false, true)
        );
        // The bit is never set for a result that is not wider than it is tall.
        assert_eq!(
            landscape("format=thumbhash,w=3999,h=4000,fit=fill"),
            (false, false)
        );
        assert_eq!(
            landscape("format=thumbhash,w=4000,h=4000,fit=fill"),
            (false, false)
        );
        // The bit is set once the thumbnail keeps the order of the sides.
        assert_eq!(
            landscape("format=thumbhash,w=4000,h=3900,fit=fill"),
            (true, true)
        );
    }

    #[test]
    fn transform_thumbhash_of_exif_source_hashes_the_pixels_other_formats_output() {
        init();
        let data = fixture("exif_orientation.jpg");
        let png = thumbhash_result(&data, "format=png", None);
        let result = thumbhash_result(&data, "format=thumbhash", None);
        assert_eq!(thumbhash_bytes(&result), hash_of_pixels_in(&png.data));
    }

    #[test]
    fn transform_thumbhash_of_image_under_100px_hashes_native_pixels() {
        init();
        let data = static_fixture();
        let result = thumbhash_result(&data, "format=thumbhash", None);
        assert_eq!(
            thumbhash_bytes(&result),
            hash_of_pixels_in(&data),
            "a 4x4 image must hash its own pixels, not an upscaled copy"
        );

        let data = nonsquare_fixture();
        let small = thumbhash_result(&data, "format=thumbhash,w=50,h=20,fit=fill", None);
        assert_eq!((small.width, small.height), (50, 20));
        let png = thumbhash_result(&data, "format=png,w=50,h=20,fit=fill", None);
        assert_eq!(
            thumbhash_bytes(&small),
            hash_of_pixels_in(&png.data),
            "a 50x20 output must hash its own pixels, not an upscaled copy"
        );
    }

    #[test]
    fn apply_draw_overlays_is_a_noop_for_thumbhash_result() {
        init();
        let data = nonsquare_fixture();
        let base_result = thumbhash_result(&data, "format=thumbhash", None);
        assert_eq!(base_result.format, OutputFormat::Thumbhash);
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            ..Default::default()
        };
        let result = apply_draw_overlays(
            base_result.clone(),
            std::slice::from_ref(&entry),
            &[overlay_fixture()],
            &test_encode_options(),
        )
        .unwrap();
        assert_eq!(result, base_result);
    }

    /// A 2000x1500 JPEG. Unlike the PNG fixture, a JPEG source can decode
    /// at reduced size.
    fn nonsquare_jpeg() -> Vec<u8> {
        let png_bytes = nonsquare_fixture();
        let png = VipsImage::from_buffer(&png_bytes).unwrap();
        png.save_jpeg(85, true).unwrap()
    }

    #[test]
    fn resize_reads_source_only_when_no_earlier_stage_changes_the_image() {
        let base = TransformParams::default();
        assert!(resize_reads_source(&base, false));
        assert!(!resize_reads_source(&base, true), "animated output");
        let blockers = [
            (
                "frame",
                TransformParams {
                    frame: Some(1),
                    ..base.clone()
                },
            ),
            (
                "trim",
                TransformParams {
                    trim: Some(10.0),
                    ..base.clone()
                },
            ),
            (
                "trim_top",
                TransformParams {
                    trim_top: Some(1.0),
                    ..base.clone()
                },
            ),
            (
                "trim_right",
                TransformParams {
                    trim_right: Some(1.0),
                    ..base.clone()
                },
            ),
            (
                "trim_bottom",
                TransformParams {
                    trim_bottom: Some(1.0),
                    ..base.clone()
                },
            ),
            (
                "trim_left",
                TransformParams {
                    trim_left: Some(1.0),
                    ..base.clone()
                },
            ),
            (
                "rotate",
                TransformParams {
                    rotate: Some(Rotation::Deg90),
                    ..base.clone()
                },
            ),
            (
                "flip",
                TransformParams {
                    flip: Some(FlipMode::H),
                    ..base.clone()
                },
            ),
            // The reduced decode is lenient. A truncated source then gives
            // blank pixels (INV-19).
            (
                "format=thumbhash",
                TransformParams {
                    format: Some(OutputFormat::Thumbhash),
                    ..base.clone()
                },
            ),
            // The tests below use `frame=0` as their full-decode control, so
            // it must keep disabling the reduced decode.
            (
                "frame=0",
                TransformParams {
                    frame: Some(0),
                    ..base.clone()
                },
            ),
        ];
        for (name, tp) in blockers {
            assert!(!resize_reads_source(&tp, false), "{name}");
        }
    }

    #[test]
    fn jpeg_source_rotate_is_applied_before_resize() {
        init();
        let p = TransformParams {
            width: Some(200),
            rotate: Some(Rotation::Deg90),
            ..Default::default()
        };
        let result = transform(&nonsquare_jpeg(), &p, None, None).unwrap();
        assert_eq!(result.width, 200);
        assert!(
            result.height > result.width,
            "got {}x{}",
            result.width,
            result.height
        );
    }

    #[test]
    fn jpeg_source_trim_is_applied_before_resize() {
        init();
        // Trimming half of 2000 px leaves 1000x1500. A resize that skips
        // the trim gives 100x75.
        let p = TransformParams {
            width: Some(100),
            trim_left: Some(0.5),
            ..Default::default()
        };
        let result = transform(&nonsquare_jpeg(), &p, None, None).unwrap();
        assert_eq!((result.width, result.height), (100, 150));
    }

    /// `frame=0` is a no-op on a static source that still disables the
    /// reduced decode, so it gives the full decode to compare against.
    #[test]
    fn shrink_on_load_resize_matches_full_decode_dimensions() {
        init();
        let jpeg = nonsquare_jpeg();
        for (fit, height) in [
            (FitMode::Contain, None),
            (FitMode::Inside, Some(80)),
            (FitMode::Cover, Some(80)),
            (FitMode::Contain, Some(80)),
            (FitMode::AspectCrop, Some(80)),
        ] {
            let fast = TransformParams {
                width: Some(120),
                height,
                fit,
                ..Default::default()
            };
            let full = TransformParams {
                frame: Some(0),
                ..fast.clone()
            };
            let a = transform(&jpeg, &fast, None, None).unwrap();
            let b = transform(&jpeg, &full, None, None).unwrap();
            assert_eq!((a.width, a.height), (b.width, b.height), "fit={fit:?}");
        }
    }

    #[test]
    fn shrink_on_load_resize_matches_full_decode_dimensions_for_exif_source() {
        init();
        let data = fixture("exif_orientation.jpg");
        let fast = TransformParams {
            width: Some(2),
            ..Default::default()
        };
        let full = TransformParams {
            frame: Some(0),
            ..fast.clone()
        };
        let a = transform(&data, &fast, None, None).unwrap();
        let b = transform(&data, &full, None, None).unwrap();
        assert_eq!((a.width, a.height), (b.width, b.height));
    }

    /// A JPEG with the given EXIF orientation tag. It stores 2000x1500
    /// pixels, or 1500x2000 pixels when `stored_portrait` is set.
    fn oriented_jpeg(orientation: i32, stored_portrait: bool) -> Vec<u8> {
        let mut image = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
        if stored_portrait {
            image = image.rot(consts::VIPS_ANGLE_D90).unwrap();
        }
        image.set_int("orientation", orientation);
        let jpeg = image.save_jpeg(85, false).unwrap();
        let reloaded = VipsImage::from_buffer(&jpeg).unwrap();
        assert_eq!(reloaded.get_int("orientation"), Some(orientation));
        let stored = if stored_portrait {
            (1500, 2000)
        } else {
            (2000, 1500)
        };
        assert_eq!((reloaded.width(), reloaded.height()), stored);
        jpeg
    }

    /// Run a width-only or height-only request with the reduced decode (JPEG
    /// source) and with the full decode. `frame=0` is a no-op that forces the
    /// full decode. Both decodes must agree.
    fn resize_dimensions_on_both_paths(
        data: &[u8],
        width: Option<u32>,
        height: Option<u32>,
    ) -> (u32, u32) {
        let fast = TransformParams {
            width,
            height,
            ..Default::default()
        };
        let full = TransformParams {
            frame: Some(0),
            ..fast.clone()
        };
        let a = transform(data, &fast, None, None).unwrap();
        let b = transform(data, &full, None, None).unwrap();
        assert_eq!(
            (a.width, a.height),
            (b.width, b.height),
            "the reduced decode and the full decode must agree"
        );
        (a.width, a.height)
    }

    /// Each case pairs a stored shape with the expected output size. The
    /// flag is true for a stored portrait image. A stored 2000x1500 image
    /// with a quarter-turn tag displays as 1500x2000. A stored 1500x2000
    /// image displays as 2000x1500.
    const QUARTER_TURN_CASES: [(bool, (u32, u32)); 2] = [(false, (300, 400)), (true, (300, 225))];

    #[test]
    fn resize_width_only_derives_height_from_oriented_aspect_for_quarter_turn_exif() {
        init();
        for (stored_portrait, expected) in QUARTER_TURN_CASES {
            for orientation in [5, 6, 7, 8] {
                let data = oriented_jpeg(orientation, stored_portrait);
                assert_eq!(
                    resize_dimensions_on_both_paths(&data, Some(300), None),
                    expected,
                    "orientation {orientation}, stored portrait {stored_portrait}"
                );
            }
        }
    }

    #[test]
    fn resize_height_only_derives_width_from_oriented_aspect_for_quarter_turn_exif() {
        init();
        for (stored_portrait, expected) in QUARTER_TURN_CASES {
            for orientation in [5, 6, 7, 8] {
                let data = oriented_jpeg(orientation, stored_portrait);
                assert_eq!(
                    resize_dimensions_on_both_paths(&data, None, Some(expected.1)),
                    expected,
                    "orientation {orientation}, stored portrait {stored_portrait}"
                );
            }
        }
    }

    #[test]
    fn resize_derives_missing_side_from_stored_aspect_when_exif_is_not_a_quarter_turn() {
        init();
        let untagged = {
            let png = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
            png.save_jpeg(85, false).unwrap()
        };
        let mut sources = vec![("none".to_string(), untagged)];
        for orientation in [1, 2, 3, 4] {
            sources.push((orientation.to_string(), oriented_jpeg(orientation, false)));
        }
        for (label, data) in sources {
            assert_eq!(
                resize_dimensions_on_both_paths(&data, Some(300), None),
                (300, 225),
                "orientation {label}, width only"
            );
            assert_eq!(
                resize_dimensions_on_both_paths(&data, None, Some(225)),
                (300, 225),
                "orientation {label}, height only"
            );
        }
    }

    #[test]
    fn resize_derives_missing_side_from_oriented_aspect_for_other_fit_modes() {
        init();
        for fit in [
            FitMode::Contain,
            FitMode::Inside,
            FitMode::Pad,
            FitMode::Crop,
        ] {
            for (stored_portrait, expected) in QUARTER_TURN_CASES {
                let data = oriented_jpeg(6, stored_portrait);
                for (width, height) in [(Some(expected.0), None), (None, Some(expected.1))] {
                    // `crop` needs a height to define its box.
                    if fit == FitMode::Crop && height.is_none() {
                        continue;
                    }
                    let p = TransformParams {
                        width,
                        height,
                        fit,
                        ..Default::default()
                    };
                    let result = transform(&data, &p, None, None).unwrap();
                    assert_eq!(
                        (result.width, result.height),
                        expected,
                        "fit={fit:?}, stored portrait {stored_portrait}, w={width:?}, h={height:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn resize_applies_exif_orientation_after_an_explicit_rotate() {
        init();
        let p = TransformParams {
            width: Some(300),
            rotate: Some(Rotation::Deg90),
            ..Default::default()
        };
        // rotate=90 turns the stored 1500x2000 into 2000x1500. The tag then
        // turns it back to 1500x2000, so w=300 gives 300x400. The stored
        // ratio would give 300x225.
        let data = oriented_jpeg(6, true);
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!((result.width, result.height), (300, 400));
        // rotate=90 turns the stored 2000x1500 into 1500x2000. The tag then
        // turns it back to 2000x1500, so w=300 gives 300x225.
        let data = oriented_jpeg(6, false);
        let result = transform(&data, &p, None, None).unwrap();
        assert_eq!((result.width, result.height), (300, 225));
    }

    /// An untagged JPEG that is 1500x2000 when `portrait` is set and
    /// 2000x1500 otherwise. It has the displayed shape of a tagged source.
    fn untagged_jpeg(portrait: bool) -> Vec<u8> {
        let mut image = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
        if portrait {
            image = image.rot(consts::VIPS_ANGLE_D90).unwrap();
        }
        image.save_jpeg(85, false).unwrap()
    }

    fn aspect_crop_size(data: &[u8], width: u32, height: u32, fast_path: bool) -> (u32, u32) {
        let p = TransformParams {
            width: Some(width),
            height: Some(height),
            fit: FitMode::AspectCrop,
            frame: (!fast_path).then_some(0),
            ..Default::default()
        };
        let result = transform(data, &p, None, None).unwrap();
        (result.width, result.height)
    }

    /// The tagged source displays with the sides swapped, so `aspect-crop`
    /// must give the size of an untagged image with that displayed shape.
    /// The requests cover the downscale branch and the direct-crop branch.
    #[test]
    fn aspect_crop_uses_oriented_dimensions_for_quarter_turn_exif() {
        init();
        let requests = [(1000, 1600), (3000, 2500), (1600, 1000), (300, 400)];
        for stored_portrait in [false, true] {
            let untagged = untagged_jpeg(!stored_portrait);
            for orientation in [5, 6, 7, 8] {
                let tagged = oriented_jpeg(orientation, stored_portrait);
                for (width, height) in requests {
                    for fast_path in [true, false] {
                        assert_eq!(
                            aspect_crop_size(&tagged, width, height, fast_path),
                            aspect_crop_size(&untagged, width, height, true),
                            "orientation {orientation}, stored portrait {stored_portrait}, \
                             {width}x{height}, fast path {fast_path}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn aspect_crop_direct_crop_gives_the_documented_size_for_quarter_turn_exif() {
        init();
        // Displayed 1500x2000. The request 3000x2500 needs an upscale, so the
        // image crops to the 6:5 ratio at the original size.
        let data = oriented_jpeg(6, false);
        assert_eq!(aspect_crop_size(&data, 3000, 2500, true), (1500, 1250));
        // The request 1000x1600 fits without an upscale.
        assert_eq!(aspect_crop_size(&data, 1000, 1600, true), (1000, 1600));
    }

    #[test]
    fn aspect_crop_direct_crop_clears_the_orientation_tag_with_metadata_keep() {
        init();
        for orientation in 1..=8 {
            let data = oriented_jpeg(orientation, false);
            let p = TransformParams {
                width: Some(3000),
                height: Some(2500),
                fit: FitMode::AspectCrop,
                format: Some(OutputFormat::Jpeg),
                metadata: MetadataMode::Keep,
                ..Default::default()
            };
            let result = transform(&data, &p, None, None).unwrap();
            let reloaded = VipsImage::from_buffer(&result.data).unwrap();
            assert!(
                matches!(reloaded.get_int("orientation"), None | Some(1)),
                "orientation {orientation} left a turn tag on the output"
            );
            assert_eq!(
                (reloaded.width() as u32, reloaded.height() as u32),
                (result.width, result.height),
                "orientation {orientation}"
            );
        }
    }

    fn encoded_len(data: &[u8], params: &str, settings: Option<TransformSettings>) -> usize {
        let tp = parse(params).unwrap();
        transform(data, &tp, None, settings).unwrap().data.len()
    }

    #[test]
    fn transform_settings_default_matches_the_parts_defaults() {
        let settings = TransformSettings::default();
        assert_eq!(settings.limits, TransformLimits::default());
        assert_eq!(settings.encoder, EncoderSettings::default());
    }

    #[test]
    fn transform_settings_encoder_reaches_the_avif_encoder() {
        init();
        let data = nonsquare_fixture();
        let params = "format=avif,w=300";
        let with_effort = |avif_effort| {
            Some(TransformSettings {
                encoder: EncoderSettings { avif_effort },
                ..Default::default()
            })
        };
        let slow = encoded_len(&data, params, with_effort(9));
        let fast = encoded_len(&data, params, with_effort(0));
        assert!(
            fast > slow,
            "effort 0 must give larger AVIF output than effort 9 ({fast} vs {slow})"
        );
        assert_eq!(
            encoded_len(&data, params, None),
            encoded_len(&data, params, Some(TransformSettings::default())),
            "None must select the default transform settings"
        );
    }

    #[test]
    fn transform_quality_reaches_the_jpeg_encoder() {
        init();
        let data = nonsquare_fixture();
        let low = encoded_len(&data, "format=jpeg,w=300,quality=10", None);
        let high = encoded_len(&data, "format=jpeg,w=300,quality=90", None);
        assert!(
            low < high,
            "quality=10 must give fewer bytes ({low} vs {high})"
        );
    }

    #[test]
    fn transform_metadata_keep_preserves_exif_and_strip_removes_it() {
        init();
        let data = oriented_jpeg(6, false);
        let orientation_of = |metadata: &str| {
            let tp = parse(&format!("format=jpeg,metadata={metadata}")).unwrap();
            let result = transform(&data, &tp, None, None).unwrap();
            VipsImage::from_buffer(&result.data)
                .unwrap()
                .get_int("orientation")
        };
        assert_eq!(orientation_of("keep"), Some(6));
        assert_eq!(orientation_of("strip"), None);
    }

    #[test]
    fn apply_draw_overlays_honours_encode_options_quality() {
        init();
        let data = nonsquare_fixture();
        let tp = parse("format=jpeg,w=600").unwrap();
        let base_result = transform(&data, &tp, None, None).unwrap();
        let entry = DrawOverlay {
            url: Some("https://example.com/logo.png".to_string()),
            ..Default::default()
        };
        let len_at = |quality: u8| {
            let options = EncodeOptions {
                quality,
                ..test_encode_options()
            };
            apply_draw_overlays(
                base_result.clone(),
                std::slice::from_ref(&entry),
                &[overlay_fixture()],
                &options,
            )
            .unwrap()
            .data
            .len()
        };
        let (low, high) = (len_at(10), len_at(90));
        assert!(
            low < high,
            "quality 10 must give fewer bytes ({low} vs {high})"
        );
    }

    #[test]
    fn encode_options_new_copies_request_fields_and_encoder_effort() {
        let tp = parse("quality=33,metadata=keep").unwrap();
        let options = EncodeOptions::new(&tp, EncoderSettings { avif_effort: 3 });
        assert_eq!(
            options,
            EncodeOptions {
                quality: 33,
                avif_effort: 3,
                metadata: MetadataMode::Keep,
            }
        );
    }

    #[test]
    fn oriented_size_swaps_the_sides_only_for_quarter_turn_orientations() {
        init();
        for orientation in 1..=8 {
            let png = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
            png.set_int("orientation", orientation);
            let expected = if (5..=8).contains(&orientation) {
                (1500, 2000)
            } else {
                (2000, 1500)
            };
            assert_eq!(oriented_size(&png), expected, "orientation {orientation}");
        }
        let untagged = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
        assert_eq!(oriented_size(&untagged), (2000, 1500));
    }

    /// Sources that a strict libvips load rejects but the default load
    /// accepts. Each is a real fixture, cut inside its first frame or its
    /// headers. The JPEG and WebP sources are re-encoded copies of the large
    /// PNG fixture, so the cut lands inside real entropy-coded data.
    fn truncated_sources() -> Vec<(String, Vec<u8>)> {
        let png = fixture("bench_2000x1500.png");
        let image = VipsImage::from_buffer(&png).unwrap();
        let jpeg = image.save_jpeg(85, true).unwrap();
        let webp = image.save_webp(80, true).unwrap();
        let cmyk = fixture("cmyk.jpg");
        let exif = fixture("exif_orientation.jpg");
        let gif = fixture("loading.gif");
        let small_webp = fixture("static.webp");
        let cuts: [(&str, &Vec<u8>, usize); 14] = [
            ("png", &png, png.len() / 4),
            ("png", &png, png.len() / 2),
            ("png", &png, png.len() * 3 / 4),
            ("jpeg", &jpeg, jpeg.len() / 4),
            ("jpeg", &jpeg, jpeg.len() / 2),
            ("cmyk jpeg", &cmyk, 281),
            ("exif jpeg", &exif, 657),
            ("gif", &gif, 101),
            ("gif", &gif, 355),
            ("gif", &gif, 800),
            ("webp", &webp, webp.len() / 2),
            ("webp", &webp, webp.len() - 100),
            ("small webp", &small_webp, 32),
            ("small webp", &small_webp, 63),
        ];
        cuts.into_iter()
            .map(|(label, data, at)| (format!("{label} cut at {at}"), data[..at].to_vec()))
            .collect()
    }

    /// Sources whose tail is overwritten, so the length stays valid.
    fn corrupted_tail_sources() -> Vec<(String, Vec<u8>)> {
        let png = fixture("bench_2000x1500.png");
        let jpeg = VipsImage::from_buffer(&png)
            .unwrap()
            .save_jpeg(85, true)
            .unwrap();
        [("png", png), ("jpeg", jpeg)]
            .into_iter()
            .map(|(label, mut bytes)| {
                let from = bytes.len() / 2;
                bytes[from..].fill(0xAA);
                (format!("{label} tail overwritten from {from}"), bytes)
            })
            .collect()
    }

    /// The label of every case for which `transform` did not fail with a
    /// libvips error.
    fn cases_without_vips_error(sources: Vec<(String, Vec<u8>)>, params: &[&str]) -> Vec<String> {
        let mut offenders = Vec::new();
        for (label, data) in &sources {
            for params in params {
                let p = parse(params).unwrap();
                match transform(data, &p, None, None) {
                    Err(TransformError::Vips(_)) => {}
                    Ok(result) => offenders.push(format!(
                        "{label} [{params}]: got {} hash {:?}",
                        result.format.as_str(),
                        String::from_utf8_lossy(&result.data)
                    )),
                    Err(other) => offenders.push(format!("{label} [{params}]: {other:?}")),
                }
            }
        }
        offenders
    }

    #[test]
    fn transform_thumbhash_of_truncated_source_returns_error_not_a_hash() {
        init();
        let offenders = cases_without_vips_error(
            truncated_sources(),
            &[
                "format=thumbhash",
                "format=thumbhash,w=50",
                "format=thumbhash,w=300,h=200,fit=cover",
                "format=thumbhash,rotate=90,frame=1",
            ],
        );
        assert!(offenders.is_empty(), "no error for: {offenders:#?}");
    }

    #[test]
    fn transform_thumbhash_of_source_with_corrupted_tail_returns_error_not_a_hash() {
        init();
        let offenders = cases_without_vips_error(
            corrupted_tail_sources(),
            &["format=thumbhash", "format=thumbhash,w=50"],
        );
        assert!(offenders.is_empty(), "no error for: {offenders:#?}");
    }

    #[test]
    fn transform_thumbhash_of_truncated_source_over_budget_returns_budget_error_first() {
        init();
        // INV-20: the pixel budget check runs before any pixel decode, so
        // an over-budget source that is also truncated reports the budget.
        let settings = TransformSettings {
            limits: TransformLimits {
                max_pixels: 1_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let png = fixture("bench_2000x1500.png");
        let jpeg = VipsImage::from_buffer(&png)
            .unwrap()
            .save_jpeg(85, true)
            .unwrap();
        let p = parse("format=thumbhash").unwrap();
        for data in [&png[..png.len() / 2], &jpeg[..jpeg.len() / 2]] {
            let err = transform(data, &p, None, Some(settings)).expect_err("over budget");
            assert!(
                matches!(err, TransformError::ExceedsMaxPixels(3_000_000, 1_000)),
                "{err:?}"
            );
        }
    }

    #[test]
    fn transform_of_truncated_source_still_succeeds_for_every_other_format() {
        init();
        // Control: only `format=thumbhash` loads strictly. Every other
        // output format keeps the default load, which accepts a partly
        // decodable source and encodes the pixels it got.
        let png = fixture("bench_2000x1500.png");
        let jpeg = VipsImage::from_buffer(&png)
            .unwrap()
            .save_jpeg(85, true)
            .unwrap();
        let sources = [
            ("png", png[..png.len() / 2].to_vec()),
            ("jpeg", jpeg[..jpeg.len() / 2].to_vec()),
            ("gif", fixture("loading.gif")[..355].to_vec()),
        ];
        let mut failures = Vec::new();
        for (label, data) in &sources {
            for params in [
                "",
                "format=png",
                "format=jpeg,w=100",
                "format=webp,w=64,h=64,fit=cover",
                "format=json",
                "format=gif",
            ] {
                let p = parse(params).unwrap();
                if let Err(err) = transform(data, &p, None, None) {
                    failures.push(format!("{label} [{params}]: {err:?}"));
                }
            }
        }
        assert!(failures.is_empty(), "unexpected errors: {failures:#?}");
    }

    #[test]
    fn transform_thumbhash_of_complete_source_is_unchanged_by_the_strict_load() {
        init();
        // The strict load accepts every valid fixture, and it hashes the
        // same pixels as the default load: `format=png` output of the same
        // request decodes to the same hash.
        for name in [
            "bench_2000x1500.png",
            "cmyk.jpg",
            "exif_orientation.jpg",
            "loading.gif",
            "static.webp",
            "alpha_4x4.png",
        ] {
            let data = fixture(name);
            let thumb = thumbhash_result(&data, "format=thumbhash,w=50", None);
            let png = thumbhash_result(&data, "format=png,w=50", None);
            assert_eq!(
                thumbhash_bytes(&thumb),
                hash_of_pixels_in(&png.data),
                "{name}"
            );
        }
    }

    // ----------------------------------------------------------------
    // Decode path: which requests run the reduced decode (INV-21 to INV-26)
    // ----------------------------------------------------------------

    fn nonsquare_webp() -> Vec<u8> {
        VipsImage::from_buffer(&nonsquare_fixture())
            .unwrap()
            .save_webp(80, true)
            .unwrap()
    }

    fn transform_params(data: &[u8], params: &str) -> TransformResult {
        let p = parse(params).unwrap_or_else(|e| panic!("parse [{params}]: {e:?}"));
        transform(data, &p, None, None).unwrap_or_else(|e| panic!("transform [{params}]: {e:?}"))
    }

    /// Run `params` as given, then again with `frame=0`. `frame=0` is a
    /// no-op on a static source that forces the full decode. Each run must take the path that
    /// its name says.
    fn both_paths(data: &[u8], params: &str) -> (TransformResult, TransformResult) {
        let reduced = transform_params(data, params);
        let full = transform_params(data, &format!("{params},frame=0"));
        assert!(
            reduced.decoded_at_reduced_size,
            "[{params}] must decode at reduced size"
        );
        assert!(
            !full.decoded_at_reduced_size,
            "[{params},frame=0] must decode at full size"
        );
        (reduced, full)
    }

    /// An animated WebP of the 12 frames of `loading.gif`.
    fn animated_webp() -> Vec<u8> {
        let gif = fixture("loading.gif");
        let frames = VipsImage::from_buffer_animated(&gif, -1).unwrap();
        let webp = frames.save_webp(80, true).unwrap();
        let probe = VipsImage::from_buffer(&webp).unwrap();
        assert_eq!(probe.n_pages(), Some(12), "the encoder keeps every frame");
        webp
    }

    struct Raster {
        width: i32,
        height: i32,
        bands: i32,
        pixels: Vec<u8>,
    }

    fn raster(encoded: &[u8]) -> Raster {
        let image = VipsImage::from_buffer(encoded).unwrap();
        Raster {
            width: image.width(),
            height: image.height(),
            bands: image.bands(),
            pixels: image.write_to_memory().unwrap(),
        }
    }

    fn mean_abs_diff(a: &Raster, b: &Raster) -> f64 {
        assert_eq!(
            (a.width, a.height, a.bands),
            (b.width, b.height, b.bands),
            "rasters must have the same shape"
        );
        let total: u64 = a
            .pixels
            .iter()
            .zip(&b.pixels)
            .map(|(x, y)| x.abs_diff(*y) as u64)
            .sum();
        total as f64 / a.pixels.len() as f64
    }

    fn flipped(image: &Raster, horizontal: bool, vertical: bool) -> Raster {
        let (w, h, bands) = (
            image.width as usize,
            image.height as usize,
            image.bands as usize,
        );
        let mut pixels = Vec::with_capacity(image.pixels.len());
        for y in 0..h {
            let src_y = if vertical { h - 1 - y } else { y };
            for x in 0..w {
                let src_x = if horizontal { w - 1 - x } else { x };
                let at = (src_y * w + src_x) * bands;
                pixels.extend_from_slice(&image.pixels[at..at + bands]);
            }
        }
        Raster {
            pixels,
            width: image.width,
            height: image.height,
            bands: image.bands,
        }
    }

    #[test]
    fn transform_reports_reduced_decode_for_jpeg_and_webp_resize() {
        init();
        let sources = [("jpeg", nonsquare_jpeg()), ("webp", nonsquare_webp())];
        let requests = [
            "w=100",
            "h=100",
            "w=100,h=80",
            "w=100,h=80,fit=contain",
            "w=100,h=80,fit=inside",
            "w=100,h=80,fit=cover",
            "w=100,h=80,fit=fill",
            "w=100,h=80,fit=outside",
            "w=100,h=80,fit=pad",
            "w=100,h=80,fit=crop",
            "w=100,h=80,fit=aspect-crop",
            "w=100,h=80,fit=cover,g=center",
            "w=100,h=80,fit=cover,g=north",
            "w=100,h=80,fit=contain,g=smart",
            "w=100,h=80,fit=pad,g=attention",
            "w=100,f=jpeg",
            "w=100,f=png",
            "w=100,f=webp",
            "w=100,f=json",
            "w=50,dpr=2",
            "w=100,sharpen=1,blur=1,brightness=1.1,gamma=1.2,saturation=0.8",
            "w=100,bg=ff0000,border=2,q=50,metadata=keep,compression=fast",
        ];
        let mut misses = Vec::new();
        for (label, data) in &sources {
            for params in requests {
                if !transform_params(data, params).decoded_at_reduced_size {
                    misses.push(format!("{label} [{params}]"));
                }
            }
        }
        assert!(
            misses.is_empty(),
            "these requests must decode at reduced size: {misses:#?}"
        );
    }

    #[test]
    fn transform_reports_full_decode_when_the_resize_cannot_read_the_source() {
        init();
        let png = nonsquare_fixture();
        let gif = fixture("loading.gif");
        let small_png = VipsImage::from_buffer(&png)
            .unwrap()
            .thumbnail(64, ThumbnailOptions::default())
            .unwrap();
        // The libheif of some builds, such as Alpine, has no AVIF encoder.
        let avif = small_png.save_avif(50, 0, true).ok();
        let jpeg = nonsquare_jpeg();
        let webp = nonsquare_webp();
        let mut cases: Vec<(&str, &[u8], &str)> = vec![
            // PNG, GIF, and AVIF have no reduced decode.
            ("png", &png, "w=100"),
            ("png", &png, "w=100,h=80,fit=cover"),
            ("png", &png, "w=100,h=80,fit=aspect-crop"),
            ("gif", &gif, "w=64,anim=static"),
            ("gif", &gif, "w=64,frame=1"),
            ("animated gif", &gif, "w=64"),
            ("animated gif", &gif, "w=64,h=64,fit=cover"),
            // No resize runs, so no reduced decode runs.
            ("jpeg", &jpeg, "f=png"),
            ("jpeg", &jpeg, "sharpen=1"),
            ("jpeg", &jpeg, "f=json"),
            ("webp", &webp, "f=png"),
            ("webp", &webp, "f=json"),
            // An `aspect-crop` that upscales crops the full image.
            ("jpeg", &jpeg, "w=3000,h=2500,fit=aspect-crop"),
            ("webp", &webp, "w=3000,h=2500,fit=aspect-crop"),
            // `format=thumbhash` keeps the strict full decode (INV-19).
            ("jpeg", &jpeg, "w=100,f=thumbhash"),
            ("jpeg", &jpeg, "w=100,h=80,fit=cover,f=thumbhash"),
            ("webp", &webp, "w=100,f=thumbhash"),
            ("webp", &webp, "w=100,h=80,fit=cover,f=thumbhash"),
            // Content-aware gravity picks the crop window from the full image.
            ("jpeg", &jpeg, "w=100,h=80,fit=cover,g=smart"),
            ("jpeg", &jpeg, "w=100,h=80,fit=crop,g=attention"),
            ("jpeg", &jpeg, "w=100,h=80,fit=aspect-crop,g=smart"),
            ("webp", &webp, "w=100,h=80,fit=cover,g=smart"),
            ("webp", &webp, "w=100,h=80,fit=cover,g=attention"),
            ("webp", &webp, "w=100,h=80,fit=crop,g=smart"),
            ("webp", &webp, "w=100,h=80,fit=aspect-crop,g=attention"),
        ];
        if let Some(avif) = &avif {
            cases.push(("avif", avif, "w=32"));
        }
        let mut wrong = Vec::new();
        for (label, data, params) in cases {
            if transform_params(data, params).decoded_at_reduced_size {
                wrong.push(format!("{label} [{params}]"));
            }
        }
        assert!(
            wrong.is_empty(),
            "these requests must decode at full size: {wrong:#?}"
        );
    }

    #[test]
    fn rotate_takes_effect_and_disables_reduced_decode() {
        init();
        for (label, data) in [("jpeg", nonsquare_jpeg()), ("webp", nonsquare_webp())] {
            for (rotate, expected) in [(90, (100, 133)), (180, (100, 75)), (270, (100, 133))] {
                let params = format!("w=100,rotate={rotate}");
                let result = transform_params(&data, &params);
                assert!(!result.decoded_at_reduced_size, "{label} [{params}]");
                assert_eq!(
                    (result.width, result.height),
                    expected,
                    "{label} [{params}]"
                );
            }
        }
    }

    #[test]
    fn flip_takes_effect_and_disables_reduced_decode() {
        init();
        for (label, data) in [("jpeg", nonsquare_jpeg()), ("webp", nonsquare_webp())] {
            let (_, control) = both_paths(&data, "w=100,f=png");
            let control = raster(&control.data);
            for (mode, horizontal, vertical) in
                [("h", true, false), ("v", false, true), ("hv", true, true)]
            {
                let params = format!("w=100,f=png,flip={mode}");
                let result = transform_params(&data, &params);
                assert!(!result.decoded_at_reduced_size, "{label} [{params}]");
                let out = raster(&result.data);
                let expected = flipped(&control, horizontal, vertical);
                assert!(
                    mean_abs_diff(&out, &expected) < 4.0,
                    "{label} [{params}] must equal the flipped control"
                );
                assert!(
                    mean_abs_diff(&out, &control) > 20.0,
                    "{label} [{params}] must differ from the unflipped control"
                );
            }
        }
    }

    #[test]
    fn per_side_trim_takes_effect_and_disables_reduced_decode() {
        init();
        // The source is 2000x1500. Each request keeps the size in the range
        // below. A resize that skips the trim gives 100x75.
        let cases = [
            ("trim.top=750", (100, 37..=38)),
            ("trim.bottom=750", (100, 37..=38)),
            ("trim.right=1000", (100, 150..=150)),
            ("trim.left=0.5", (100, 150..=150)),
        ];
        for (label, data) in [("jpeg", nonsquare_jpeg()), ("webp", nonsquare_webp())] {
            for (trim, (width, heights)) in &cases {
                let params = format!("w=100,{trim}");
                let result = transform_params(&data, &params);
                assert!(!result.decoded_at_reduced_size, "{label} [{params}]");
                assert_eq!(result.width, *width, "{label} [{params}]");
                assert!(
                    heights.contains(&result.height),
                    "{label} [{params}]: height {}",
                    result.height
                );
            }
        }
    }

    #[test]
    fn numeric_trim_takes_effect_and_disables_reduced_decode() {
        init();
        // A white border 500 px wide on the left of the 2000x1500 image.
        let bordered = VipsImage::from_buffer(&nonsquare_fixture())
            .unwrap()
            .embed(500, 0, 2500, 1500, [255.0, 255.0, 255.0])
            .unwrap();
        let sources = [
            ("jpeg", bordered.save_jpeg(90, true).unwrap()),
            ("webp", bordered.save_webp(90, true).unwrap()),
        ];
        for (label, data) in sources {
            let untrimmed = transform_params(&data, "w=100");
            assert!(untrimmed.decoded_at_reduced_size, "{label}");
            assert_eq!(untrimmed.height, 60, "{label}: 2500x1500 gives 100x60");
            let trimmed = transform_params(&data, "w=100,trim=10");
            assert!(!trimmed.decoded_at_reduced_size, "{label}");
            assert!(
                (74..=76).contains(&trimmed.height),
                "{label}: the trim leaves about 2000x1500, got height {}",
                trimmed.height
            );
        }
    }

    #[test]
    fn frame_takes_effect_and_disables_reduced_decode_on_animated_webp() {
        init();
        let data = animated_webp();
        let first = transform_params(&data, "w=64,f=png,anim=static");
        assert!(
            first.decoded_at_reduced_size,
            "a static request reads the first frame from the source"
        );
        let second = transform_params(&data, "w=64,f=png,frame=1");
        assert!(!second.decoded_at_reduced_size);
        assert_eq!((second.width, second.height), (64, 64));
        assert_ne!(first.data, second.data, "frame 1 differs from frame 0");
    }

    #[test]
    fn animated_output_disables_reduced_decode_and_keeps_every_frame() {
        init();
        let result = transform_params(&animated_webp(), "w=64,f=webp");
        assert!(!result.decoded_at_reduced_size);
        assert!(result.is_animated);
        assert_eq!(result.frame_count, Some(12));
        assert_eq!(result.width, 64);
    }

    #[test]
    fn frame_zero_is_the_full_decode_control() {
        init();
        for (label, data) in [("jpeg", nonsquare_jpeg()), ("webp", nonsquare_webp())] {
            let result = transform_params(&data, "w=100,frame=0");
            assert!(!result.decoded_at_reduced_size, "{label}");
            assert_eq!((result.width, result.height), (100, 75), "{label}");
        }
    }

    fn max_pixels_settings(max_pixels: u64) -> Option<TransformSettings> {
        Some(TransformSettings {
            limits: TransformLimits {
                max_pixels,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    #[test]
    fn transform_rejects_jpeg_and_webp_over_budget_for_every_resize_request() {
        init();
        let settings = max_pixels_settings(2_999_999);
        for (label, data) in [("jpeg", nonsquare_jpeg()), ("webp", nonsquare_webp())] {
            for params in [
                "w=100",
                "w=100,f=jpeg",
                "w=100,f=webp",
                "w=100,f=png",
                "w=100,f=json",
                "w=100,h=80,fit=cover,f=jpeg",
                "w=100,h=80,fit=aspect-crop,f=jpeg",
            ] {
                let p = parse(params).unwrap();
                let err = transform(&data, &p, None, settings)
                    .expect_err(&format!("{label} [{params}] is over the budget"));
                assert!(
                    matches!(err, TransformError::ExceedsMaxPixels(3_000_000, 2_999_999)),
                    "{label} [{params}]: {err:?}"
                );
            }
        }
    }

    #[test]
    fn transform_accepts_jpeg_and_webp_at_exactly_max_pixels() {
        init();
        let settings = max_pixels_settings(3_000_000);
        for (label, data) in [("jpeg", nonsquare_jpeg()), ("webp", nonsquare_webp())] {
            let p = parse("w=100,f=jpeg").unwrap();
            let result = transform(&data, &p, None, settings).unwrap();
            assert!(result.decoded_at_reduced_size, "{label}");
            assert_eq!((result.width, result.height), (100, 75), "{label}");
        }
    }

    #[test]
    fn transform_checks_the_budget_before_the_reduced_decode_of_a_damaged_source() {
        init();
        // The reduced decode is lenient, so it accepts these sources.
        // The budget error must come first, for every format. A WebP cut
        // short fails at the probe, so its tail is overwritten instead.
        let settings = max_pixels_settings(1_000);
        let jpeg = nonsquare_jpeg();
        let mut webp = nonsquare_webp();
        let from = webp.len() / 2;
        webp[from..].fill(0xAA);
        let sources = [
            ("jpeg cut", jpeg[..jpeg.len() / 2].to_vec()),
            ("webp", webp),
        ];
        for (label, data) in &sources {
            for params in ["w=100,f=jpeg", "w=100,f=png", "w=100,f=json", "w=100"] {
                let p = parse(params).unwrap();
                let err = transform(data, &p, None, settings)
                    .expect_err(&format!("{label} [{params}] is over the budget"));
                assert!(
                    matches!(err, TransformError::ExceedsMaxPixels(3_000_000, 1_000)),
                    "{label} [{params}]: {err:?}"
                );
            }
        }
    }

    /// Damaged copies of `data`: cut short, tail overwritten, and a window
    /// of bytes inverted. Every copy keeps the file header.
    fn mutated_sources(label: &str, data: &[u8]) -> Vec<(String, Vec<u8>)> {
        let len = data.len();
        let mut out = Vec::new();
        for k in 1..16 {
            out.push((
                format!("{label} cut at {k}/16"),
                data[..len * k / 16].to_vec(),
            ));
        }
        for k in 1..8 {
            let mut bytes = data.to_vec();
            bytes[len * k / 8..].fill(0xAA);
            out.push((format!("{label} tail from {k}/8 overwritten"), bytes));
        }
        for k in 1..10 {
            let mut bytes = data.to_vec();
            let at = len * k / 10;
            for byte in &mut bytes[at..at + 16] {
                *byte ^= 0xFF;
            }
            out.push((format!("{label} bytes at {k}/10 inverted"), bytes));
        }
        out
    }

    fn outcome(result: &Result<TransformResult, TransformError>) -> &'static str {
        match result {
            Ok(_) => "ok",
            Err(TransformError::Vips(_)) => "libvips error",
            Err(TransformError::ExceedsMaxPixels(..)) => "budget error",
            Err(TransformError::Thumbhash(_)) => "thumbhash error",
        }
    }

    /// Run `params` with the reduced decode and with the full decode
    /// (`frame=0`).
    fn transform_both_paths(
        data: &[u8],
        params: &str,
    ) -> (
        Result<TransformResult, TransformError>,
        Result<TransformResult, TransformError>,
    ) {
        let reduced = transform(data, &parse(params).unwrap(), None, None);
        let full = transform(
            data,
            &parse(&format!("{params},frame=0")).unwrap(),
            None,
            None,
        );
        (reduced, full)
    }

    const DAMAGED_SOURCE_REQUESTS: [&str; 2] = ["w=100", "w=300,h=200,fit=cover"];

    #[test]
    fn reduced_and_full_decode_agree_on_success_or_failure_for_damaged_jpeg_sources() {
        init();
        // A WebP cut short fails at the probe on both paths, so it joins the
        // JPEG sources here. The other damaged WebP sources have their own test.
        let mut sources = mutated_sources("jpeg", &nonsquare_jpeg());
        let webp = nonsquare_webp();
        sources.extend(
            mutated_sources("webp", &webp)
                .into_iter()
                .filter(|(label, _)| label.contains("cut at")),
        );
        let mut mismatches = Vec::new();
        let (mut both_ok, mut both_err, mut reduced_ok) = (0, 0, 0);
        for (label, data) in &sources {
            for params in DAMAGED_SOURCE_REQUESTS {
                let (reduced, full) = transform_both_paths(data, params);
                let (a, b) = (outcome(&reduced), outcome(&full));
                if a != b {
                    mismatches.push(format!("{label} [{params}]: reduced {a}, full {b}"));
                } else if a == "ok" {
                    both_ok += 1;
                    if reduced.as_ref().is_ok_and(|r| r.decoded_at_reduced_size) {
                        reduced_ok += 1;
                    }
                } else {
                    both_err += 1;
                }
            }
        }
        assert!(mismatches.is_empty(), "paths disagree: {mismatches:#?}");
        assert!(
            both_ok > 0 && both_err > 0,
            "ok {both_ok}, error {both_err}"
        );
        assert!(reduced_ok > 0, "no damaged source took the reduced decode");
    }

    /// On libvips 8.15.1, a WebP with a damaged bitstream can succeed with the
    /// reduced decode while the full decode fails. The libwebp scaler accepts
    /// data that the plain decode rejects. libvips 8.18.4 gives the same
    /// class for both decodes. The reduced decode must never fail where the
    /// full decode succeeds.
    #[test]
    fn reduced_decode_never_fails_where_the_full_decode_succeeds_for_damaged_webp_sources() {
        init();
        let sources: Vec<_> = mutated_sources("webp", &nonsquare_webp())
            .into_iter()
            .filter(|(label, _)| !label.contains("cut at"))
            .collect();
        let mut turned_to_failure = Vec::new();
        let mut full_failures = 0;
        for (label, data) in &sources {
            for params in DAMAGED_SOURCE_REQUESTS {
                let (reduced, full) = transform_both_paths(data, params);
                if reduced.is_err() && full.is_ok() {
                    turned_to_failure.push(format!("{label} [{params}]"));
                }
                if full.is_err() {
                    full_failures += 1;
                }
            }
        }
        assert!(
            turned_to_failure.is_empty(),
            "the reduced decode fails where the full decode succeeds: {turned_to_failure:#?}"
        );
        assert!(full_failures > 0, "the damage never reached the decoder");
    }

    #[test]
    fn truncated_jpeg_fills_the_missing_rows_with_gray_on_both_paths() {
        init();
        let jpeg = nonsquare_jpeg();
        let cut = &jpeg[..jpeg.len() / 2];
        for params in ["w=200,f=png", "w=200,f=png,frame=0"] {
            let result = transform(cut, &parse(params).unwrap(), None, None).unwrap();
            let image = raster(&result.data);
            let row = image.width as usize * image.bands as usize;
            let last = &image.pixels[image.pixels.len() - row..];
            assert!(
                last.iter().all(|v| (120..=136).contains(v)),
                "[{params}] the last row must be gray, got {:?}",
                &last[..12]
            );
        }
    }

    /// The container-level metadata of an encoded image: every JPEG APPn
    /// segment, every WebP chunk except the pixel data, and every PNG chunk
    /// except the header and the pixel data.
    fn container_metadata(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        if bytes.starts_with(&[0xFF, 0xD8]) {
            let mut at = 2;
            while at + 4 <= bytes.len() && bytes[at] == 0xFF && bytes[at + 1] != 0xDA {
                let marker = bytes[at + 1];
                let len = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
                if (0xE1..=0xEF).contains(&marker) {
                    out.push((
                        format!("app{}", marker - 0xE0),
                        bytes[at + 4..at + 2 + len].to_vec(),
                    ));
                }
                at += 2 + len;
            }
        } else if bytes.starts_with(b"RIFF") {
            let mut at = 12;
            while at + 8 <= bytes.len() {
                let name = String::from_utf8_lossy(&bytes[at..at + 4]).to_string();
                let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
                if !["VP8 ", "VP8L", "ALPH"].contains(&name.as_str()) {
                    out.push((name, bytes[at + 8..at + 8 + len].to_vec()));
                }
                at += 8 + len + (len & 1);
            }
        } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            let mut at = 8;
            while at + 8 <= bytes.len() {
                let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
                let name = String::from_utf8_lossy(&bytes[at + 4..at + 8]).to_string();
                if !["IHDR", "IDAT", "IEND"].contains(&name.as_str()) {
                    out.push((name, bytes[at + 8..at + 8 + len].to_vec()));
                }
                at += 12 + len;
            }
        }
        out
    }

    fn has_chunk(chunks: &[(String, Vec<u8>)], name: &str) -> bool {
        chunks.iter().any(|(n, _)| n == name)
    }

    fn webp_with_orientation(orientation: i32) -> Vec<u8> {
        let image = VipsImage::from_buffer(&nonsquare_fixture()).unwrap();
        image.set_int("orientation", orientation);
        image.save_webp(80, false).unwrap()
    }

    #[test]
    fn metadata_is_equal_on_both_decode_paths_for_every_output_format() {
        init();
        let sources: Vec<(&str, Vec<u8>, &str)> = vec![
            ("icc jpeg", fixture("icc_srgb_640x480.jpg"), "w=80"),
            ("icc webp", fixture("icc_srgb_640x480.webp"), "w=80"),
            ("exif jpeg", fixture("exif_orientation.jpg"), "w=2"),
            ("oriented jpeg", oriented_jpeg(6, false), "w=100"),
            ("oriented webp", webp_with_orientation(6), "w=100"),
        ];
        let mut mismatches = Vec::new();
        for (label, data, size) in &sources {
            for format in ["jpeg", "webp", "png"] {
                for metadata in ["keep", "copyright", "strip"] {
                    let params = format!("{size},f={format},metadata={metadata}");
                    let (reduced, full) = both_paths(data, &params);
                    let a = container_metadata(&reduced.data);
                    let b = container_metadata(&full.data);
                    if a != b {
                        mismatches.push(format!("{label} [{params}]"));
                    }
                }
            }
        }
        assert!(mismatches.is_empty(), "metadata differs: {mismatches:#?}");
    }

    #[test]
    fn embedded_icc_profile_is_kept_byte_equal_on_both_decode_paths() {
        init();
        let jpeg = fixture("icc_srgb_640x480.jpg");
        let webp = fixture("icc_srgb_640x480.webp");
        let source_jpeg_icc = container_metadata(&jpeg)
            .into_iter()
            .find(|(name, _)| name == "app2")
            .expect("the JPEG fixture has an ICC profile")
            .1;
        let source_webp_icc = container_metadata(&webp)
            .into_iter()
            .find(|(name, _)| name == "ICCP")
            .expect("the WebP fixture has an ICC profile")
            .1;
        for (data, format, chunk, source_icc) in [
            (&jpeg, "jpeg", "app2", &source_jpeg_icc),
            (&webp, "webp", "ICCP", &source_webp_icc),
        ] {
            let params = format!("w=80,f={format},metadata=keep");
            let (reduced, full) = both_paths(data, &params);
            for (path, result) in [("reduced", reduced), ("full", full)] {
                let chunks = container_metadata(&result.data);
                assert!(has_chunk(&chunks, chunk), "{format} {path}: no ICC profile");
                let icc = &chunks.iter().find(|(n, _)| n == chunk).unwrap().1;
                assert_eq!(icc, source_icc, "{format} {path}: the profile changed");
            }
        }
    }

    #[test]
    fn exif_orientation_is_applied_once_and_reset_on_both_decode_paths() {
        init();
        for orientation in 1..=8 {
            let expected = if orientation >= 5 {
                (300, 400)
            } else {
                (300, 225)
            };
            let sources = [
                ("jpeg", oriented_jpeg(orientation, false)),
                ("webp", webp_with_orientation(orientation)),
            ];
            for (label, data) in sources {
                let params = "w=300,f=jpeg,metadata=keep";
                let (reduced, full) = both_paths(&data, params);
                for (path, result) in [("reduced", &reduced), ("full", &full)] {
                    assert_eq!(
                        (result.width, result.height),
                        expected,
                        "{label} orientation {orientation} {path}"
                    );
                    let out = VipsImage::from_buffer(&result.data).unwrap();
                    assert!(
                        matches!(out.get_int("orientation"), None | Some(1)),
                        "{label} orientation {orientation} {path}: the tag is {:?}",
                        out.get_int("orientation")
                    );
                }
                let (reduced, full) = both_paths(&data, "w=300,f=png");
                assert!(
                    mean_abs_diff(&raster(&reduced.data), &raster(&full.data)) < 4.0,
                    "{label} orientation {orientation}: the paths turn the pixels differently"
                );
            }
        }
    }

    #[test]
    fn cmyk_jpeg_output_is_three_band_srgb_on_both_decode_paths() {
        init();
        let data = fixture("cmyk.jpg");
        let (reduced, full) = both_paths(&data, "w=4,f=png");
        let (a, b) = (raster(&reduced.data), raster(&full.data));
        assert_eq!((a.bands, b.bands), (3, 3));
        assert!(mean_abs_diff(&a, &b) < 6.0);
    }

    #[test]
    fn webp_alpha_is_kept_on_both_decode_paths() {
        init();
        let scaled = VipsImage::from_buffer(&fixture("alpha_4x4.png"))
            .unwrap()
            .thumbnail(
                800,
                ThumbnailOptions {
                    height: Some(600),
                    size: Some(consts::VIPS_SIZE_FORCE),
                    ..Default::default()
                },
            )
            .unwrap();
        let data = scaled.save_webp(80, true).unwrap();
        let (reduced, full) = both_paths(&data, "w=100,f=png");
        let (a, b) = (raster(&reduced.data), raster(&full.data));
        assert_eq!((a.bands, b.bands), (4, 4));
        let alpha = |r: &Raster| Raster {
            width: r.width,
            height: r.height,
            bands: 1,
            pixels: r.pixels.chunks(4).map(|px| px[3]).collect(),
        };
        assert!(mean_abs_diff(&alpha(&a), &alpha(&b)) < 4.0);
    }
    // ----------------------------------------------------------------
    // Size parity of the two decode paths (INV-23)
    // ----------------------------------------------------------------

    /// A JPEG or WebP of `width` x `height` pixels, scaled from the large
    /// PNG fixture.
    fn sized_source(webp: bool, width: i32, height: i32) -> Vec<u8> {
        let scaled = VipsImage::from_buffer(&nonsquare_fixture())
            .unwrap()
            .thumbnail(
                width,
                ThumbnailOptions {
                    height: Some(height),
                    size: Some(consts::VIPS_SIZE_FORCE),
                    ..Default::default()
                },
            )
            .unwrap();
        if webp {
            scaled.save_webp(80, true).unwrap()
        } else {
            scaled.save_jpeg(85, true).unwrap()
        }
    }

    /// The size that `vips_thumbnail_image` gives a `fit=contain` box on a
    /// lazy image of `width` x `height` pixels. The image holds no decoded
    /// pixels, so this costs nothing per size.
    fn libvips_contain_size(width: i32, height: i32, target: (i32, i32)) -> (i32, i32) {
        let lazy = VipsImage::from_buffer(&static_fixture())
            .unwrap()
            .embed(0, 0, width, height, [0.0, 0.0, 0.0])
            .unwrap();
        let out = lazy
            .thumbnail(
                target.0,
                ThumbnailOptions {
                    height: Some(target.1),
                    size: Some(consts::VIPS_SIZE_DOWN),
                    ..Default::default()
                },
            )
            .unwrap();
        (out.width(), out.height())
    }

    #[test]
    fn contain_size_gives_the_sizes_of_the_full_decode_for_the_audit_examples() {
        // (source, box, size of the full decode)
        let cases = [
            ((300, 225), (50, 38), (50, 38)),
            ((2000, 1307), (200, 131), (200, 131)),
            ((300, 200), (2, 1), (2, 1)),
            ((1600, 1000), (100, 63), (100, 63)),
            ((300, 200), (500, 500), (300, 200)),
            ((2000, 1500), (100, 100), (100, 75)),
        ];
        for (source, target, expected) in cases {
            assert_eq!(
                contain_size(source, target),
                expected,
                "{source:?} {target:?}"
            );
            assert_eq!(
                libvips_contain_size(source.0, source.1, target),
                expected,
                "libvips {source:?} {target:?}"
            );
        }
    }

    #[test]
    fn contain_size_equals_the_libvips_size_for_many_sources_and_boxes() {
        init();
        let sources = [
            (1, 1),
            (2, 3),
            (16, 16),
            // Tall and narrow sources. The reciprocal of the reciprocal of the
            // shrink factor changes the rounded size for these.
            (28, 189),
            (28, 1834),
            (40, 1270),
            (40, 1740),
            (73, 1000),
            (1000, 73),
            (100, 2000),
            (2000, 40),
            (299, 201),
            (300, 200),
            (300, 225),
            (500, 333),
            (641, 479),
            (800, 600),
            (1000, 750),
            (1024, 1024),
            (1600, 1000),
            (1601, 1067),
            (2000, 1307),
            (2000, 1500),
            (2400, 3200),
            (3201, 2399),
            (4000, 3000),
            (4032, 3024),
            (5000, 4999),
            (6000, 4000),
            (8000, 6000),
            (8400, 8400),
        ];
        let widths = [
            1, 2, 3, 4, 5, 7, 8, 9, 13, 16, 17, 18, 26, 31, 33, 50, 64, 99, 100, 127, 128, 199,
            200, 254, 255, 333, 400, 641, 999, 1000, 1200, 1920, 4000,
        ];
        let mut checked = 0;
        for (source_w, source_h) in sources {
            for width in widths {
                let derived = ((width as f64 * source_h as f64 / source_w as f64).round() as i32)
                    .clamp(1, 8192);
                for height in [derived, 1, 2, 7, 50, 133, 300, 700, 5000] {
                    let target = (width, height);
                    assert_eq!(
                        contain_size((source_w, source_h), target),
                        libvips_contain_size(source_w, source_h, target),
                        "source {source_w}x{source_h}, box {width}x{height}"
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, sources.len() * widths.len() * 9);
    }

    /// The fit modes and the box sizes of the table tests below. A box has a
    /// width and an optional height.
    const PARITY_FITS: [&str; 8] = [
        "contain",
        "inside",
        "pad",
        "cover",
        "crop",
        "fill",
        "outside",
        "aspect-crop",
    ];

    /// Every request whose two paths give another size, as text.
    fn size_mismatches(webp: bool, source_w: i32, source_h: i32, params: &[String]) -> Vec<String> {
        let data = sized_source(webp, source_w, source_h);
        let label = format!(
            "{} {source_w}x{source_h}",
            if webp { "webp" } else { "jpeg" }
        );
        let mut mismatches = Vec::new();
        for params in params {
            let reduced = transform_params(&data, params);
            let full = transform_params(&data, &format!("{params},frame=0"));
            assert!(!full.decoded_at_reduced_size, "{label} [{params}] control");
            // An `aspect-crop` that upscales crops the full image.
            assert!(
                reduced.decoded_at_reduced_size || params.contains("aspect-crop"),
                "{label} [{params}] must decode at reduced size"
            );
            if (reduced.width, reduced.height) != (full.width, full.height) {
                mismatches.push(format!(
                    "{label} [{params}]: reduced {}x{}, full {}x{}",
                    reduced.width, reduced.height, full.width, full.height
                ));
            }
        }
        mismatches
    }

    fn parity_requests(widths: &[u32], heights: &[Option<u32>]) -> Vec<String> {
        let mut requests = Vec::new();
        for fit in PARITY_FITS {
            for width in widths {
                for height in heights {
                    requests.push(match height {
                        Some(h) => format!("w={width},h={h},fit={fit},f=jpeg"),
                        None => format!("w={width},fit={fit},f=jpeg"),
                    });
                }
            }
        }
        requests
    }

    #[test]
    fn reduced_decode_gives_the_full_decode_size_for_every_fit_mode() {
        init();
        let requests = parity_requests(
            &[2, 5, 17, 33, 100, 199, 254],
            &[None, Some(1), Some(50), Some(133)],
        );
        let sources = [
            (300, 200),
            (300, 225),
            (641, 479),
            (1000, 750),
            (73, 1000),
            (1000, 73),
        ];
        let mismatches: Vec<String> = std::thread::scope(|scope| {
            let mut jobs = Vec::new();
            for webp in [false, true] {
                for (source_w, source_h) in sources {
                    let requests = &requests;
                    jobs.push(
                        scope.spawn(move || size_mismatches(webp, source_w, source_h, requests)),
                    );
                }
            }
            jobs.into_iter()
                .flat_map(|job| job.join().unwrap())
                .collect()
        });
        assert!(
            mismatches.is_empty(),
            "{} requests changed size with the reduced decode: {:#?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(40)]
        );
    }

    #[test]
    fn reduced_decode_gives_the_full_decode_size_for_large_sources() {
        init();
        let requests = parity_requests(&[200, 254, 400], &[None, Some(133)]);
        let mismatches: Vec<String> = std::thread::scope(|scope| {
            let jobs: Vec<_> = [(false, 2000, 1307), (true, 2000, 1307), (false, 3201, 2399)]
                .into_iter()
                .map(|(webp, w, h)| {
                    let requests = &requests;
                    scope.spawn(move || size_mismatches(webp, w, h, requests))
                })
                .collect();
            jobs.into_iter()
                .flat_map(|job| job.join().unwrap())
                .collect()
        });
        assert!(
            mismatches.is_empty(),
            "{} requests changed size with the reduced decode: {:#?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(40)]
        );
    }

    #[test]
    fn reduced_decode_gives_the_full_decode_size_for_exif_orientations() {
        init();
        let requests = [
            "w=50",
            "w=200,h=50",
            "h=40",
            "w=33,h=200,fit=pad",
            "w=100,h=100,fit=inside",
        ];
        let mut mismatches = Vec::new();
        for (label, stored_portrait) in [("landscape", false), ("portrait", true)] {
            for orientation in 1..=8 {
                let data = oriented_jpeg(orientation, stored_portrait);
                for params in requests {
                    let reduced = transform_params(&data, params);
                    let full = transform_params(&data, &format!("{params},frame=0"));
                    if (reduced.width, reduced.height) != (full.width, full.height) {
                        mismatches.push(format!(
                            "{label} orientation {orientation} [{params}]: reduced {}x{}, full {}x{}",
                            reduced.width, reduced.height, full.width, full.height
                        ));
                    }
                }
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    #[test]
    fn audit_examples_keep_their_size_with_the_reduced_decode() {
        init();
        let cases = [
            (false, (300, 225), "w=50", (50, 38)),
            (false, (2000, 1307), "w=200", (200, 131)),
            (false, (300, 200), "w=2", (2, 1)),
            (true, (1600, 1000), "w=100", (100, 63)),
        ];
        for (webp, (source_w, source_h), params, expected) in cases {
            let data = sized_source(webp, source_w, source_h);
            let result = transform_params(&data, params);
            assert!(
                result.decoded_at_reduced_size,
                "{source_w}x{source_h} [{params}]"
            );
            assert_eq!(
                (result.width, result.height),
                expected,
                "{source_w}x{source_h} [{params}]"
            );
        }
    }

    #[test]
    fn contain_inside_and_pad_never_exceed_the_box_and_cover_fill_crop_give_the_box() {
        init();
        let data = sized_source(false, 1000, 750);
        for (width, height) in [(50, 33), (199, 133), (333, 7), (5, 300)] {
            for fit in ["contain", "inside", "pad"] {
                let params = format!("w={width},h={height},fit={fit}");
                let result = transform_params(&data, &params);
                assert!(
                    result.width <= width && result.height <= height,
                    "[{params}] gave {}x{}",
                    result.width,
                    result.height
                );
            }
            for fit in ["cover", "fill", "crop"] {
                let params = format!("w={width},h={height},fit={fit}");
                let result = transform_params(&data, &params);
                assert_eq!((result.width, result.height), (width, height), "[{params}]");
            }
        }
    }

    // ----------------------------------------------------------------
    // Content-aware gravity reads the full image (INV-21)
    // ----------------------------------------------------------------

    /// Requests that crop to the box, so the gravity picks the window.
    const CROP_FITS: [&str; 3] = ["cover", "crop", "aspect-crop"];

    #[test]
    fn smart_and_attention_crops_decode_at_full_size_and_keep_the_full_decode_window() {
        init();
        // On these sources the reduced decode moved the window: the mean
        // difference to the full decode was 5 to 9 levels, against less than 1
        // for `gravity=center`.
        let sources = [("webp", nonsquare_webp()), ("jpeg", nonsquare_jpeg())];
        for (label, data) in sources {
            for gravity in ["smart", "attention"] {
                for fit in CROP_FITS {
                    for (width, height) in [(400, 100), (60, 60), (300, 200)] {
                        let params = format!("w={width},h={height},fit={fit},g={gravity},f=png");
                        let result = transform_params(&data, &params);
                        let control = transform_params(&data, &format!("{params},frame=0"));
                        assert!(
                            !result.decoded_at_reduced_size,
                            "{label} [{params}] must decode at full size"
                        );
                        assert_eq!(
                            result.data, control.data,
                            "{label} [{params}] must give the window of the full decode"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn centre_and_directional_crops_still_decode_at_reduced_size() {
        init();
        for (label, data) in [("webp", nonsquare_webp()), ("jpeg", nonsquare_jpeg())] {
            for gravity in ["center", "north", "southeast"] {
                for fit in CROP_FITS {
                    let params = format!("w=400,h=100,fit={fit},g={gravity},f=png");
                    let (reduced, full) = both_paths(&data, &params);
                    assert_eq!(
                        (reduced.width, reduced.height),
                        (full.width, full.height),
                        "{label} [{params}]"
                    );
                    assert!(
                        mean_abs_diff(&raster(&reduced.data), &raster(&full.data)) < 1.0,
                        "{label} [{params}] must keep the window of the full decode"
                    );
                }
            }
        }
    }

    #[test]
    fn smart_and_attention_gravity_do_not_stop_the_reduced_decode_when_the_fit_does_not_crop() {
        init();
        for (label, data) in [("webp", nonsquare_webp()), ("jpeg", nonsquare_jpeg())] {
            for gravity in ["smart", "attention"] {
                for fit in ["contain", "inside", "pad", "fill", "outside"] {
                    let params = format!("w=400,h=100,fit={fit},g={gravity},f=png");
                    assert!(
                        transform_params(&data, &params).decoded_at_reduced_size,
                        "{label} [{params}]: the gravity has no effect on this fit"
                    );
                }
            }
        }
    }

    // ----------------------------------------------------------------
    // Parameters that do not change the image keep the reduced decode
    // ----------------------------------------------------------------

    #[test]
    fn resize_reads_source_ignores_parameters_that_provably_change_nothing() {
        let base = TransformParams::default();
        let no_ops = [
            (
                "rotate=0",
                TransformParams {
                    rotate: Some(Rotation::Deg0),
                    ..base.clone()
                },
            ),
            (
                "trim.top=0",
                TransformParams {
                    trim_top: Some(0.0),
                    ..base.clone()
                },
            ),
            (
                "all four sides at 0",
                TransformParams {
                    trim_top: Some(0.0),
                    trim_right: Some(0.0),
                    trim_bottom: Some(0.0),
                    trim_left: Some(0.0),
                    ..base.clone()
                },
            ),
            (
                "rotate=0 and trim.left=0",
                TransformParams {
                    rotate: Some(Rotation::Deg0),
                    trim_left: Some(0.0),
                    ..base.clone()
                },
            ),
        ];
        for (name, tp) in no_ops {
            assert!(resize_reads_source(&tp, false), "{name}");
            assert!(
                !resize_reads_source(&tp, true),
                "{name} with animated output"
            );
        }
    }

    #[test]
    fn resize_reads_source_still_stops_for_parameters_that_change_the_image() {
        let base = TransformParams::default();
        let changes = [
            (
                "rotate=90",
                TransformParams {
                    rotate: Some(Rotation::Deg90),
                    ..base.clone()
                },
            ),
            (
                "rotate=180",
                TransformParams {
                    rotate: Some(Rotation::Deg180),
                    ..base.clone()
                },
            ),
            (
                "rotate=270",
                TransformParams {
                    rotate: Some(Rotation::Deg270),
                    ..base.clone()
                },
            ),
            (
                "flip=h",
                TransformParams {
                    flip: Some(FlipMode::H),
                    ..base.clone()
                },
            ),
            (
                "flip=v",
                TransformParams {
                    flip: Some(FlipMode::V),
                    ..base.clone()
                },
            ),
            (
                "flip=hv",
                TransformParams {
                    flip: Some(FlipMode::Hv),
                    ..base.clone()
                },
            ),
            (
                "trim=10",
                TransformParams {
                    trim: Some(10.0),
                    ..base.clone()
                },
            ),
            (
                "trim of 0",
                TransformParams {
                    trim: Some(0.0),
                    ..base.clone()
                },
            ),
            (
                "trim.top=0.25",
                TransformParams {
                    trim_top: Some(0.25),
                    ..base.clone()
                },
            ),
            (
                "trim.bottom=0.001",
                TransformParams {
                    trim_bottom: Some(0.001),
                    ..base.clone()
                },
            ),
            (
                "trim.top=0 and trim.left=1",
                TransformParams {
                    trim_top: Some(0.0),
                    trim_left: Some(1.0),
                    ..base.clone()
                },
            ),
            (
                "rotate=0 and trim.right=5",
                TransformParams {
                    rotate: Some(Rotation::Deg0),
                    trim_right: Some(5.0),
                    ..base.clone()
                },
            ),
            (
                "frame=0",
                TransformParams {
                    frame: Some(0),
                    ..base.clone()
                },
            ),
            (
                "rotate=0 and flip=h",
                TransformParams {
                    rotate: Some(Rotation::Deg0),
                    flip: Some(FlipMode::H),
                    ..base.clone()
                },
            ),
        ];
        for (name, tp) in changes {
            assert!(!resize_reads_source(&tp, false), "{name}");
        }
    }

    #[test]
    fn resize_reads_source_stops_only_for_the_fits_that_crop_by_content() {
        let base = TransformParams::default();
        for gravity in [Gravity::Smart, Gravity::Attention] {
            for (fit, stops) in [
                (FitMode::Cover, true),
                (FitMode::Crop, true),
                (FitMode::AspectCrop, true),
                (FitMode::Contain, false),
                (FitMode::Inside, false),
                (FitMode::Pad, false),
                (FitMode::Fill, false),
                (FitMode::Outside, false),
            ] {
                let tp = TransformParams {
                    fit,
                    gravity,
                    ..base.clone()
                };
                assert_eq!(
                    resize_reads_source(&tp, false),
                    !stops,
                    "fit={fit:?} gravity={gravity:?}"
                );
            }
        }
        for gravity in [Gravity::Center, Gravity::North, Gravity::Southwest] {
            let tp = TransformParams {
                fit: FitMode::Cover,
                gravity,
                ..base.clone()
            };
            assert!(resize_reads_source(&tp, false), "gravity={gravity:?}");
        }
    }

    #[test]
    fn requests_with_parameters_that_change_nothing_decode_at_reduced_size_with_the_same_bytes() {
        init();
        let plain_requests = ["w=100,f=png", "w=100,h=80,fit=cover,f=png", "w=100,f=jpeg"];
        let no_ops = [
            "rotate=0",
            "trim.top=0",
            "trim.right=0.0,trim.bottom=0",
            "rotate=0,trim.left=0",
        ];
        for (label, data) in [("jpeg", nonsquare_jpeg()), ("webp", nonsquare_webp())] {
            for plain in plain_requests {
                let expected = transform_params(&data, plain);
                assert!(expected.decoded_at_reduced_size, "{label} [{plain}]");
                for no_op in no_ops {
                    let params = format!("{plain},{no_op}");
                    let result = transform_params(&data, &params);
                    assert!(
                        result.decoded_at_reduced_size,
                        "{label} [{params}] must decode at reduced size"
                    );
                    assert_eq!(
                        result.data, expected.data,
                        "{label} [{params}] must give the bytes of [{plain}]"
                    );
                }
            }
        }
    }

    #[test]
    fn a_side_trim_next_to_a_zero_side_trim_still_takes_effect() {
        init();
        // Trimming half of 2000 px from the left leaves 1000x1500.
        for (label, data) in [("jpeg", nonsquare_jpeg()), ("webp", nonsquare_webp())] {
            let result = transform_params(&data, "w=100,trim.top=0,trim.left=0.5,rotate=0");
            assert!(!result.decoded_at_reduced_size, "{label}");
            assert_eq!((result.width, result.height), (100, 150), "{label}");
        }
    }

    // ----------------------------------------------------------------
    // format=thumbhash keeps the strict full decode (INV-19)
    // ----------------------------------------------------------------

    /// JPEG sources cut inside the pixel data, or with an overwritten tail.
    /// Their header is intact, so the probe accepts them.
    fn damaged_jpeg_sources() -> Vec<(String, Vec<u8>)> {
        truncated_sources()
            .into_iter()
            .chain(corrupted_tail_sources())
            .filter(|(label, _)| label.starts_with("jpeg"))
            .collect()
    }

    #[test]
    fn transform_thumbhash_of_damaged_jpeg_fails_where_the_lenient_reduced_decode_succeeds() {
        init();
        let sources = damaged_jpeg_sources();
        assert_eq!(sources.len(), 3, "two cuts and one overwritten tail");
        for (label, data) in sources {
            for resize in ["w=50", "w=300,h=200,fit=cover", "w=100,h=100,fit=pad"] {
                // Control: the same resize on the default load reads the source
                // at reduced size and returns the blank pixels as an image.
                let lenient = transform_params(&data, &format!("{resize},f=png"));
                assert!(
                    lenient.decoded_at_reduced_size,
                    "{label} [{resize}] control must decode at reduced size"
                );
                // `format=thumbhash` decodes at full size with the strict load.
                // A strict reduced decode fails only some of the time on
                // libvips 8.15.1, so this request must never take that path.
                let params = format!("format=thumbhash,{resize}");
                let p = parse(&params).unwrap();
                assert!(
                    matches!(
                        transform(&data, &p, None, None),
                        Err(TransformError::Vips(_))
                    ),
                    "{label} [{params}] must fail, not give a hash"
                );
            }
        }
    }

    #[test]
    fn full_decode_size_needs_a_box_height() {
        let opts = ThumbnailOptions {
            size: Some(consts::VIPS_SIZE_DOWN),
            ..Default::default()
        };
        assert_eq!(full_decode_size((300, 225), 50, opts), None);
    }

    #[test]
    fn full_decode_size_gives_the_contain_size_for_a_box_with_a_height() {
        let opts = ThumbnailOptions {
            height: Some(38),
            size: Some(consts::VIPS_SIZE_DOWN),
            ..Default::default()
        };
        assert_eq!(full_decode_size((300, 225), 50, opts), Some((50, 38)));
    }

    #[test]
    fn full_decode_size_is_none_for_the_fits_that_need_no_correction() {
        let cases = [
            ("cover", FitMode::Cover),
            ("crop", FitMode::Crop),
            ("fill", FitMode::Fill),
            ("outside", FitMode::Outside),
        ];
        for (label, fit) in cases {
            let opts = build_thumbnail_options(fit, Gravity::Center, Some(38));
            assert_eq!(full_decode_size((300, 225), 50, opts), None, "{label}");
        }
        for (label, fit) in [
            ("contain", FitMode::Contain),
            ("inside", FitMode::Inside),
            ("pad", FitMode::Pad),
        ] {
            let opts = build_thumbnail_options(fit, Gravity::Center, Some(38));
            assert_eq!(
                full_decode_size((300, 225), 50, opts),
                Some((50, 38)),
                "{label}"
            );
        }
    }

    #[test]
    fn resize_without_an_oriented_size_keeps_the_plain_reduced_decode() {
        init();
        let data = sized_source(false, 1000, 750);
        let current = VipsImage::from_buffer(&data).unwrap();
        let opts = build_thumbnail_options(FitMode::Contain, Gravity::Center, Some(38));
        let plain = VipsImage::thumbnail_buffer(&data, 50, opts).unwrap();
        assert_ne!(
            (plain.width(), plain.height()),
            (50, 38),
            "the plain decode needs a wrong size for this check"
        );
        let (image, reduced) = resize(&current, Some(&data), None, 50, opts).unwrap();
        assert!(reduced);
        assert_eq!(
            (image.width(), image.height()),
            (plain.width(), plain.height())
        );
        assert_eq!(
            image.write_to_memory().unwrap(),
            plain.write_to_memory().unwrap()
        );
    }

    #[test]
    fn resize_keeps_a_plain_reduced_decode_when_its_size_is_right_and_fixes_it_when_not() {
        init();
        // (webp, source size, box, the plain reduced decode has the box size)
        let cases = [
            (false, (1000, 750), (533, 400), true),
            (false, (1000, 750), (50, 38), false),
            (true, (1000, 750), (50, 38), true),
            (true, (1600, 1000), (100, 63), false),
        ];
        for (webp, source_size, (width, height), plain_is_right) in cases {
            let label = format!("webp={webp} {source_size:?} box {width}x{height}");
            let data = sized_source(webp, source_size.0, source_size.1);
            let current = VipsImage::from_buffer(&data).unwrap();
            let opts = build_thumbnail_options(FitMode::Contain, Gravity::Center, Some(height));
            let plain = VipsImage::thumbnail_buffer(&data, width, opts).unwrap();
            assert_eq!(
                (plain.width(), plain.height()) == (width, height),
                plain_is_right,
                "{label}: plain decode"
            );
            let (image, reduced) =
                resize(&current, Some(&data), Some(source_size), width, opts).unwrap();
            assert!(reduced, "{label}");
            assert_eq!((image.width(), image.height()), (width, height), "{label}");
            if plain_is_right {
                assert_eq!(
                    image.write_to_memory().unwrap(),
                    plain.write_to_memory().unwrap(),
                    "{label}: the pixels of the plain decode"
                );
            }
        }
    }
}
