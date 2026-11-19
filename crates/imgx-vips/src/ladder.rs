//! Bounded single-frame image rendering over libvips.

use std::collections::HashMap;
use std::ffi::CStr;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::{
    DEFAULT_WEBP_EFFORT, MAX_WEBP_EFFORT, ThumbnailOptions, VipsError, VipsImage, consts, init,
    thumbhash,
};

/// Why rendering returned no batch.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum RenderError {
    #[error("invalid render request: {0}")]
    InvalidRequest(&'static str),
    #[error("unsupported image format or unavailable decoder")]
    UnsupportedInput,
    #[error("requested a still image, but the source is animated")]
    AnimatedInput,
    #[error("corrupt image: {0}")]
    CorruptInput(VipsError),
    #[error("input is {actual} bytes, limit is {limit}")]
    InputLimit { actual: usize, limit: usize },
    #[error("raster has {actual} pixels, limit is {limit}")]
    PixelLimit { actual: u64, limit: u64 },
    #[error("encoded output exceeds its byte or dimension limit")]
    OutputLimit,
    #[error("resource unavailable: {0}")]
    Resource(String),
    #[error("render backend: {0}")]
    Backend(#[source] VipsError),
    #[error("could not encode ThumbHash: {0}")]
    Thumbhash(#[from] thumbhash::ThumbhashError),
}

/// Requested renditions. When present, the baseline raster feeds the renditions and ThumbHash.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct LadderRequest<'a> {
    pub baseline: Option<OutputSpec>,
    pub outputs: &'a [OutputSpec],
    pub thumbhash: bool,
    pub decode: DecodePolicy,
}

impl<'a> LadderRequest<'a> {
    /// Render the requested outputs directly from the source, without ThumbHash.
    pub const fn new(outputs: &'a [OutputSpec], decode: DecodePolicy) -> Self {
        Self {
            baseline: None,
            outputs,
            thumbhash: false,
            decode,
        }
    }
}

/// How to open the source. Every policy reads a single frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodePolicy {
    /// Fail on decoder errors, including during pixel evaluation.
    Strict,
    /// Allow libvips' partial-image recovery. Decode the full source once.
    Lenient,
    /// Allow libvips' partial-image recovery and shrink on load when rendering
    /// just one JPEG or WebP derivative without a baseline or ThumbHash.
    LenientReduced,
    /// Like `LenientReduced`, with a header check that refuses animated sources.
    LenientReducedStill,
}

/// Recipe for one output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct OutputSpec {
    pub geometry: Geometry,
    pub codec: Codec,
    pub quality: u8,
    pub effort: WebpEffort,
    pub lossless: LosslessPolicy,
    pub metadata: MetadataPolicy,
}

impl OutputSpec {
    /// A lossy output with metadata stripped and WebP effort chosen by width.
    pub const fn new(geometry: Geometry, codec: Codec, quality: u8) -> Self {
        Self {
            geometry,
            codec,
            quality,
            effort: WebpEffort::ByWidth,
            lossless: LosslessPolicy::Lossy,
            metadata: MetadataPolicy::Strip,
        }
    }
}

/// Output geometry in the displayed orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Geometry {
    /// Downscale to a square bounding box, preserving aspect ratio.
    MaxEdge(u32),
    /// Downscale to a maximum width and the configured edge limit, preserving aspect ratio.
    ScaleDownWidth(u32),
    /// Scale to cover the box and crop, upscaling smaller sources as needed.
    Cover {
        width: u32,
        height: u32,
        gravity: Gravity,
    },
}

/// Position or content heuristic used when cropping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Gravity {
    Center,
    Attention,
    Entropy,
}

/// Encoders available in the shared native bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Codec {
    Webp,
    Jpeg,
    Png,
}

/// WebP encoder effort. JPEG and PNG use their native defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WebpEffort {
    Fixed(u8),
    /// Use imgx's width threshold on the encoded width.
    ByWidth,
}

/// WebP encoding mode. JPEG is always lossy and PNG is always lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LosslessPolicy {
    Lossy,
    Lossless,
    /// Preserve PNG and GIF sources losslessly. Use smart subsampling for other sources.
    Source,
}

/// How to handle input metadata and colour profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MetadataPolicy {
    Keep,
    Strip,
    /// Convert embedded colour profiles to sRGB and attach only the standard sRGB profile.
    Srgb,
}

/// Hard request and response bounds.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub struct RenderLimits {
    pub max_input_bytes: usize,
    /// Bounds decoded sources, outputs and the full raster scanned by smart crops.
    pub max_pixels: u64,
    pub max_edge: u32,
    /// Number of requested encoded outputs, including a baseline and repeated requests.
    pub max_outputs: usize,
    pub max_output_bytes: usize,
    /// Total bytes retained in the result, including the baseline and any ThumbHash.
    pub max_total_output_bytes: usize,
}

impl RenderLimits {
    /// Set all byte, raster and output-count limits for a batch.
    pub const fn new(
        max_input_bytes: usize,
        max_pixels: u64,
        max_edge: u32,
        max_outputs: usize,
        max_output_bytes: usize,
        max_total_output_bytes: usize,
    ) -> Self {
        Self {
            max_input_bytes,
            max_pixels,
            max_edge,
            max_outputs,
            max_output_bytes,
            max_total_output_bytes,
        }
    }
}

/// One complete batch of encoded renditions.
#[derive(Debug)]
#[non_exhaustive]
pub struct Ladder {
    pub baseline: Option<Encoded>,
    pub outputs: Vec<Encoded>,
    /// One entry per requested output, indexing the distinct renditions in `outputs`.
    pub output_indices: Vec<usize>,
    pub thumbhash: Option<Vec<u8>>,
    pub timings: Timings,
}

/// Encoded bytes and their actual dimensions and format.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
}

/// Timings for fully evaluated phases.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Timings {
    /// Header probe and full decode. For a fused render, this is just the probe.
    pub decode: Duration,
    pub baseline: Option<OutputTimings>,
    /// Entries correspond to distinct encoded outputs.
    pub outputs: Vec<OutputTimings>,
    pub thumbhash: Option<Duration>,
}

/// Times for one distinct encoded output.
#[derive(Debug)]
#[non_exhaustive]
pub struct OutputTimings {
    pub resize: Duration,
    /// Decode and resize together for shrink on load. In that case, `resize` is zero.
    pub decode_resize: Option<Duration>,
    pub encode: Duration,
}

/// WebP effort used by the released imgx renderer.
pub fn released_webp_effort(encoded_width: u32) -> u8 {
    if encoded_width >= 1280 {
        2
    } else {
        DEFAULT_WEBP_EFFORT
    }
}

/// Decode the first frame and render a batch. On failure no encoded bytes are returned.
///
/// Resource failures are reported when the allocator or codec exposes a
/// recoverable status. libvips and GLib also use aborting allocators, so an
/// allocation failure in native code can terminate the process.
pub fn render_ladder(
    input: &[u8],
    request: &LadderRequest<'_>,
    limits: &RenderLimits,
) -> Result<Ladder, RenderError> {
    if input.len() > limits.max_input_bytes {
        return Err(RenderError::InputLimit {
            actual: input.len(),
            limit: limits.max_input_bytes,
        });
    }
    validate_request(request, limits)?;
    if input.is_empty() {
        return Err(RenderError::UnsupportedInput);
    }
    init().map_err(backend_error)?;

    let started = Instant::now();
    let mut source_is_lossless = false;
    let first_frame =
        VipsImage::from_allowed_buffer(input, request.decode == DecodePolicy::Strict, |loader| {
            source_is_lossless = matches!(
                loader,
                b"VipsForeignLoadPngSource" | b"VipsForeignLoadNsgifSource"
            );
            source_is_lossless
                || matches!(
                    loader,
                    b"VipsForeignLoadJpegSource"
                        | b"VipsForeignLoadWebpSource"
                        | b"VipsForeignLoadHeifSource"
                )
        })
        .map_err(decode_error)?
        .ok_or(RenderError::UnsupportedInput)?;
    check_pixels(
        (first_frame.width() as u64, first_frame.height() as u64),
        limits.max_pixels,
    )?;

    if request.decode == DecodePolicy::LenientReducedStill
        && first_frame.n_pages().is_some_and(|pages| pages > 1)
    {
        return Err(RenderError::AnimatedInput);
    }
    let fused = request.baseline.is_none()
        && request.outputs.len() == 1
        && !request.thumbhash
        && matches!(
            request.decode,
            DecodePolicy::LenientReduced | DecodePolicy::LenientReducedStill
        )
        && first_frame.shrinks_on_load()
        && request.outputs[0].metadata != MetadataPolicy::Srgb
        && !matches!(
            request.outputs[0].geometry,
            Geometry::Cover {
                gravity: Gravity::Attention | Gravity::Entropy,
                ..
            }
        );
    let source = if fused {
        first_frame
    } else {
        let decoded = first_frame.materialize().map_err(decode_error)?;
        drop(first_frame);
        decoded
    };
    let mut timings = Timings {
        decode: started.elapsed(),
        ..Default::default()
    };
    let mut total_bytes = 0;
    let mut baseline = None;
    let mut baseline_raster = None;
    if let Some(spec) = request.baseline {
        let (raster, time) = resize(&source, None, spec, limits)?;
        let (encoded, timing) = encode(&raster, spec, time, false, source_is_lossless)?;
        account_bytes(encoded.bytes.len(), &mut total_bytes, limits)?;
        baseline = Some(encoded);
        baseline_raster = Some(raster);
        timings.baseline = Some(timing);
    }

    let source = baseline_raster.as_ref().unwrap_or(&source);
    let mut outputs = Vec::new();
    let mut output_indices = Vec::new();
    let mut indices = HashMap::new();
    let count = request.outputs.len();
    outputs.try_reserve_exact(count).map_err(resource_error)?;
    output_indices
        .try_reserve_exact(count)
        .map_err(resource_error)?;
    timings
        .outputs
        .try_reserve_exact(count)
        .map_err(resource_error)?;
    indices.try_reserve(count).map_err(resource_error)?;
    for &spec in request.outputs {
        let spec = canonical_spec(spec, oriented_size(source));
        if let Some(&index) = indices.get(&spec) {
            output_indices.push(index);
            continue;
        }
        let (raster, time) = resize(source, fused.then_some(input), spec, limits)?;
        let (encoded, timing) = encode(&raster, spec, time, fused, source_is_lossless)?;
        account_bytes(encoded.bytes.len(), &mut total_bytes, limits)?;
        let index = outputs.len();
        indices.insert(spec, index);
        output_indices.push(index);
        outputs.push(encoded);
        timings.outputs.push(timing);
    }

    let thumbhash = if request.thumbhash {
        let started = Instant::now();
        let hash = hash_image(source)?;
        account_bytes(hash.len(), &mut total_bytes, limits)?;
        timings.thumbhash = Some(started.elapsed());
        Some(hash)
    } else {
        None
    };
    Ok(Ladder {
        baseline,
        outputs,
        output_indices,
        thumbhash,
        timings,
    })
}

fn validate_request(request: &LadderRequest<'_>, limits: &RenderLimits) -> Result<(), RenderError> {
    let count = request
        .outputs
        .len()
        .saturating_add(usize::from(request.baseline.is_some()));
    if (count == 0 && !request.thumbhash) || count > limits.max_outputs {
        return Err(RenderError::InvalidRequest("output count"));
    }
    if !(1..=i32::MAX as u32).contains(&limits.max_edge) {
        return Err(RenderError::InvalidRequest(
            "max edge must fit a positive libvips dimension",
        ));
    }
    for spec in request.baseline.iter().chain(request.outputs) {
        let (width, height) = match spec.geometry {
            Geometry::MaxEdge(edge) | Geometry::ScaleDownWidth(edge) => (edge, edge),
            Geometry::Cover { width, height, .. } => (width, height),
        };
        if width == 0 || height == 0 || !(1..=100).contains(&spec.quality) {
            return Err(RenderError::InvalidRequest(
                "geometry or quality is out of range",
            ));
        }
        if width.max(height) > limits.max_edge {
            return Err(RenderError::OutputLimit);
        }
        if spec.codec == Codec::Webp
            && matches!(spec.effort, WebpEffort::Fixed(effort) if effort > MAX_WEBP_EFFORT)
        {
            return Err(RenderError::InvalidRequest("WebP effort is out of range"));
        }
    }
    Ok(())
}

fn oriented_size(image: &VipsImage) -> (u32, u32) {
    let (width, height) = (image.width() as u32, image.height() as u32);
    if matches!(image.get_int("orientation"), Some(5..=8)) {
        (height, width)
    } else {
        (width, height)
    }
}

fn canonical_spec(mut spec: OutputSpec, size: (u32, u32)) -> OutputSpec {
    spec.geometry = match spec.geometry {
        Geometry::MaxEdge(edge) => Geometry::MaxEdge(edge.min(size.0.max(size.1))),
        Geometry::ScaleDownWidth(width) => Geometry::ScaleDownWidth(width.min(size.0)),
        cover => cover,
    };
    spec
}

fn resize(
    source: &VipsImage,
    reduced_input: Option<&[u8]>,
    spec: OutputSpec,
    limits: &RenderLimits,
) -> Result<(VipsImage, Duration), RenderError> {
    let started = Instant::now();
    let size = oriented_size(source);
    let geometry = canonical_spec(spec, size).geometry;
    let ThumbnailPlan {
        width,
        options: opts,
        center_crop,
    } = thumbnail_plan(source, geometry, limits)?;
    let image = if let Some(input) = reduced_input {
        let resized = VipsImage::thumbnail_buffer(input, width, opts).map_err(decode_error)?;
        if opts.crop.is_none() && center_crop.is_none() {
            let expected = contain_size(size, (width, opts.height.expect("thumbnail box")));
            if (resized.width(), resized.height()) != expected {
                VipsImage::thumbnail_buffer(
                    input,
                    expected.0,
                    ThumbnailOptions {
                        height: Some(expected.1),
                        size: Some(consts::VIPS_SIZE_FORCE),
                        ..opts
                    },
                )
                .map_err(decode_error)?
            } else {
                resized
            }
        } else {
            resized
        }
    } else {
        let profile =
            (spec.metadata == MetadataPolicy::Srgb && source.has_icc_profile()).then_some(c"srgb");
        source
            .thumbnail_with_profile(width, opts, profile)
            .map_err(backend_error)?
    };
    let image = if let Some((width, height)) = center_crop {
        let image = image
            .crop(
                (image.width() - width) / 2,
                (image.height() - height) / 2,
                width,
                height,
            )
            .map_err(backend_error)?;
        if !matches!(source.get_int("orientation"), None | Some(1)) {
            // Rotate only the cropped raster. thumbnail's autorotation would
            // otherwise force the full scaled raster into memory.
            image
                .materialize()
                .map_err(backend_error)?
                .autorot()
                .map_err(backend_error)?
        } else {
            image
        }
    } else {
        image
    };
    if image.width() <= 0
        || image.height() <= 0
        || image.width().max(image.height()) as u32 > limits.max_edge
    {
        return Err(RenderError::OutputLimit);
    }
    check_pixels(
        (image.width() as u64, image.height() as u64),
        limits.max_pixels,
    )?;
    let image = if spec.metadata == MetadataPolicy::Srgb {
        let mut srgb = image
            .colourspace(consts::VIPS_INTERPRETATION_sRGB)
            .map_err(backend_error)?;
        // The saver writes the standard profile explicitly. Give it the
        // normalized pixels without an inherited profile to validate.
        srgb.remove_icc_profile();
        srgb
    } else {
        image
    };
    let image = image.materialize().map_err(if reduced_input.is_some() {
        decode_error
    } else {
        backend_error
    })?;
    Ok((image, started.elapsed()))
}

struct ThumbnailPlan {
    width: i32,
    options: ThumbnailOptions,
    center_crop: Option<(i32, i32)>,
}

fn thumbnail_plan(
    source: &VipsImage,
    geometry: Geometry,
    limits: &RenderLimits,
) -> Result<ThumbnailPlan, RenderError> {
    let (source_width, source_height) = oriented_size(source);
    let (width, height, crop, size) = match geometry {
        Geometry::MaxEdge(edge) => (edge, edge, None, Some(consts::VIPS_SIZE_DOWN)),
        Geometry::ScaleDownWidth(width) => {
            // Derive the height as the HTTP pipeline does for width-only requests.
            let height = (f64::from(width) * (f64::from(source_height) / f64::from(source_width)))
                .round() as u32;
            (
                width,
                height.clamp(1, limits.max_edge),
                None,
                Some(consts::VIPS_SIZE_DOWN),
            )
        }
        Geometry::Cover {
            width,
            height,
            gravity,
        } => {
            let scale = (f64::from(width) / f64::from(source_width))
                .max(f64::from(height) / f64::from(source_height));
            let pre_crop = (
                (f64::from(source_width) * scale).round() as u64,
                (f64::from(source_height) * scale).round() as u64,
            );
            let pre_crop_check = check_pixels(pre_crop, limits.max_pixels);
            if gravity == Gravity::Center
                && matches!(pre_crop_check, Err(RenderError::PixelLimit { .. }))
            {
                check_pixels((u64::from(width), u64::from(height)), limits.max_pixels)?;
                // Without libvips' built-in crop, the scaled raster remains
                // lazy. Ceil the long edge so the short edge determines scale.
                let scaled_width = (f64::from(source_width) * scale)
                    .ceil()
                    .max(f64::from(width)) as u64;
                let scaled_height = (f64::from(source_height) * scale)
                    .ceil()
                    .max(f64::from(height)) as u64;
                let scaled_width =
                    i32::try_from(scaled_width).map_err(|_| RenderError::OutputLimit)?;
                let scaled_height =
                    i32::try_from(scaled_height).map_err(|_| RenderError::OutputLimit)?;
                let swapped = matches!(source.get_int("orientation"), Some(5..=8));
                let (scaled_width, scaled_height, width, height) = if swapped {
                    (scaled_height, scaled_width, height, width)
                } else {
                    (scaled_width, scaled_height, width, height)
                };
                return Ok(ThumbnailPlan {
                    width: scaled_width,
                    options: ThumbnailOptions {
                        height: Some(scaled_height),
                        no_rotate: true,
                        ..Default::default()
                    },
                    center_crop: Some((width as i32, height as i32)),
                });
            }
            pre_crop_check?;
            if pre_crop.0.max(pre_crop.1) > i32::MAX as u64 {
                return Err(RenderError::OutputLimit);
            }
            let crop = match gravity {
                Gravity::Center => consts::VIPS_INTERESTING_CENTRE,
                Gravity::Attention => consts::VIPS_INTERESTING_ATTENTION,
                Gravity::Entropy => consts::VIPS_INTERESTING_ENTROPY,
            };
            (width, height, Some(crop), None)
        }
    };
    let height = i32::try_from(height).map_err(|_| RenderError::OutputLimit)?;
    Ok(ThumbnailPlan {
        width: width as i32,
        options: ThumbnailOptions {
            height: Some(height),
            crop,
            size,
            no_rotate: false,
        },
        center_crop: None,
    })
}

fn contain_size(source: (u32, u32), target: (i32, i32)) -> (i32, i32) {
    let shrink = (f64::from(source.0) / f64::from(target.0))
        .max(f64::from(source.1) / f64::from(target.1))
        .max(1.0);
    let reduced_side = |side: u32| {
        let length = f64::from(side);
        let reduce = 1.0 / (1.0 / shrink.min(length));
        (length / reduce + 0.5) as i32
    };
    (reduced_side(source.0), reduced_side(source.1))
}

fn encode(
    image: &VipsImage,
    spec: OutputSpec,
    resize: Duration,
    fused: bool,
    source_is_lossless: bool,
) -> Result<(Encoded, OutputTimings), RenderError> {
    let started = Instant::now();
    let width = image.width() as u32;
    let height = image.height() as u32;
    let quality = i32::from(spec.quality);
    let strip = spec.metadata != MetadataPolicy::Keep;
    let profile: Option<&CStr> = (spec.metadata == MetadataPolicy::Srgb).then_some(c"srgb");
    let bytes = match spec.codec {
        Codec::Jpeg => image.save_jpeg_with_profile(quality, strip, profile),
        Codec::Png => image.save_png_with_profile(6, strip, profile),
        Codec::Webp => {
            let effort = match spec.effort {
                WebpEffort::ByWidth => released_webp_effort(width),
                WebpEffort::Fixed(effort) => effort,
            };
            let lossless = match spec.lossless {
                LosslessPolicy::Lossy => false,
                LosslessPolicy::Lossless => true,
                LosslessPolicy::Source => source_is_lossless,
            };
            image.save_webp_configured(
                quality,
                effort,
                strip,
                profile,
                lossless,
                !lossless && spec.lossless == LosslessPolicy::Source,
            )
        }
    }
    .map_err(backend_error)?;
    Ok((
        Encoded {
            bytes,
            width,
            height,
            codec: spec.codec,
        },
        OutputTimings {
            resize: if fused { Duration::ZERO } else { resize },
            decode_resize: fused.then_some(resize),
            encode: started.elapsed(),
        },
    ))
}

fn hash_image(image: &VipsImage) -> Result<Vec<u8>, RenderError> {
    let side = thumbhash::MAX_SIDE as i32;
    let small = image
        .thumbnail(
            side,
            ThumbnailOptions {
                height: Some(side),
                size: Some(consts::VIPS_SIZE_DOWN),
                ..Default::default()
            },
        )
        .map_err(backend_error)?;
    let pixels = small.write_to_memory().map_err(backend_error)?;
    let (width, height) = (small.width() as u32, small.height() as u32);
    let rgba = thumbhash::rgba_from_bands(&pixels, width, height, small.bands() as u32)?;
    Ok(thumbhash::encode(width, height, &rgba)?)
}

fn account_bytes(
    bytes: usize,
    total: &mut usize,
    limits: &RenderLimits,
) -> Result<(), RenderError> {
    if bytes == 0 {
        return Err(RenderError::Backend(VipsError::SaveFailed(
            "empty output".into(),
        )));
    }
    let new_total = total.checked_add(bytes).ok_or(RenderError::OutputLimit)?;
    if bytes > limits.max_output_bytes || new_total > limits.max_total_output_bytes {
        return Err(RenderError::OutputLimit);
    }
    *total = new_total;
    Ok(())
}

fn check_pixels((width, height): (u64, u64), limit: u64) -> Result<(), RenderError> {
    let actual = width.saturating_mul(height);
    if actual == 0 {
        return Err(RenderError::CorruptInput(VipsError::LoadFailed(
            "empty raster".into(),
        )));
    }
    if actual > limit {
        return Err(RenderError::PixelLimit { actual, limit });
    }
    Ok(())
}

fn decode_error(error: VipsError) -> RenderError {
    classify_native_failure(error, RenderError::CorruptInput)
}

fn backend_error(error: VipsError) -> RenderError {
    classify_native_failure(error, RenderError::Backend)
}

fn classify_native_failure(error: VipsError, failure: fn(VipsError) -> RenderError) -> RenderError {
    let message = match &error {
        VipsError::Resource(_) => return RenderError::Resource(error.to_string()),
        VipsError::LoadFailed(message) | VipsError::OperationFailed(message) => message,
        _ => return failure(error),
    };
    // libvips exposes a text buffer, but includes the original libheif
    // error code and subcode at the end of each "heif:" diagnostic.
    for line in message.lines().filter(|line| line.starts_with("heif: ")) {
        if line.ends_with(" (4.3000)") || line.ends_with(" (11.6003)") {
            // Unsupported codec (older libheif) or no matching decoder installed.
            return RenderError::UnsupportedInput;
        }
        if let Some((_, code)) = line.rsplit_once(" (6.")
            && let Some(subcode) = code
                .strip_suffix(')')
                .and_then(|code| code.parse::<u32>().ok())
            && subcode != 1000
        // heif_suberror_Security_limit_exceeded
        {
            return RenderError::Resource(error.to_string());
        }
    }
    failure(error)
}

fn resource_error(error: std::collections::TryReserveError) -> RenderError {
    RenderError::Resource(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = include_bytes!("../../../test/fixtures/alpha_4x4.png");
    const ANIMATED: &[u8] = include_bytes!("../../../test/fixtures/loading.gif");

    fn limits() -> RenderLimits {
        RenderLimits {
            max_input_bytes: 256 * 1024,
            max_pixels: 512 * 512,
            max_edge: 512,
            max_outputs: 4,
            max_output_bytes: 8 * 1024,
            max_total_output_bytes: 24 * 1024,
        }
    }

    fn output(width: u32) -> OutputSpec {
        OutputSpec {
            geometry: Geometry::ScaleDownWidth(width),
            codec: Codec::Webp,
            quality: 80,
            effort: WebpEffort::ByWidth,
            lossless: LosslessPolicy::Lossy,
            metadata: MetadataPolicy::Strip,
        }
    }

    #[test]
    fn resizes_normalized_baseline_and_aliases_widths_above_it() {
        let specs = [output(2), output(99), output(120)];
        let request = LadderRequest {
            baseline: Some(OutputSpec {
                geometry: Geometry::MaxEdge(3),
                quality: 95,
                effort: WebpEffort::Fixed(DEFAULT_WEBP_EFFORT),
                lossless: LosslessPolicy::Source,
                metadata: MetadataPolicy::Srgb,
                ..output(3)
            }),
            outputs: &specs,
            thumbhash: true,
            decode: DecodePolicy::Strict,
        };
        let ladder = render_ladder(PNG, &request, &limits()).unwrap();
        let baseline = ladder.baseline.unwrap();
        assert_eq!((baseline.width, baseline.height), (3, 3));
        assert!(
            VipsImage::from_buffer(&baseline.bytes)
                .unwrap()
                .has_icc_profile()
        );
        assert_eq!(ladder.output_indices, [0, 1, 1]);
        assert_eq!(ladder.outputs.len(), 2);
        assert_eq!((ladder.outputs[0].width, ladder.outputs[0].height), (2, 2));
        assert_eq!((ladder.outputs[1].width, ladder.outputs[1].height), (3, 3));
        assert!(ladder.thumbhash.is_some());
        assert_eq!(ladder.timings.outputs.len(), 2);
    }

    #[test]
    fn an_animated_source_renders_one_frame() {
        let request = LadderRequest {
            baseline: None,
            outputs: &[output(30)],
            thumbhash: false,
            decode: DecodePolicy::Strict,
        };
        let ladder = render_ladder(ANIMATED, &request, &limits()).unwrap();
        let image = VipsImage::from_buffer(&ladder.outputs[0].bytes).unwrap();
        assert_eq!(image.n_pages().unwrap_or(1), 1);
        assert_eq!(image.width(), 30);
    }

    #[test]
    fn rejects_a_source_that_exceeds_the_header_pixel_budget() {
        let request = LadderRequest {
            baseline: None,
            outputs: &[output(1)],
            thumbhash: false,
            decode: DecodePolicy::Strict,
        };
        let truncated = &PNG[..PNG.len() - 20];
        let result = render_ladder(
            truncated,
            &request,
            &RenderLimits {
                max_pixels: 15,
                ..limits()
            },
        );
        assert!(
            matches!(
                result,
                Err(RenderError::PixelLimit {
                    actual: 16,
                    limit: 15
                })
            ),
            "{result:?}"
        );
        assert!(matches!(
            render_ladder(truncated, &request, &limits()),
            Err(RenderError::CorruptInput(_))
        ));
    }

    #[test]
    fn every_distinct_output_counts_against_the_total_budget() {
        let outputs = [output(2), output(3)];
        let request = LadderRequest {
            baseline: None,
            outputs: &outputs,
            thumbhash: false,
            decode: DecodePolicy::Strict,
        };
        let ladder = render_ladder(PNG, &request, &limits()).unwrap();
        let sum = ladder
            .outputs
            .iter()
            .map(|image| image.bytes.len())
            .sum::<usize>();
        let result = render_ladder(
            PNG,
            &request,
            &RenderLimits {
                max_total_output_bytes: sum - 1,
                ..limits()
            },
        );
        assert!(matches!(result, Err(RenderError::OutputLimit)));
        let result = render_ladder(
            PNG,
            &request,
            &RenderLimits {
                max_output_bytes: 1,
                ..limits()
            },
        );
        assert!(matches!(result, Err(RenderError::OutputLimit)));
    }

    #[test]
    fn native_status_codes_distinguish_missing_codecs_and_allocation_failures() {
        for status in ["4.3000", "11.6003"] {
            let error = VipsError::OperationFailed(format!("heif: native diagnostic ({status})"));
            assert!(matches!(decode_error(error), RenderError::UnsupportedInput));
        }
        let error = VipsError::OperationFailed("heif: native diagnostic (6.0)".into());
        assert!(matches!(decode_error(error), RenderError::Resource(_)));
        let error = VipsError::OperationFailed("heif: native diagnostic (6.1000)".into());
        assert!(matches!(decode_error(error), RenderError::CorruptInput(_)));
    }

    #[test]
    fn refuses_other_loaders_and_invalid_requests() {
        let request = LadderRequest {
            baseline: None,
            outputs: &[output(4)],
            thumbhash: false,
            decode: DecodePolicy::Strict,
        };
        let result = render_ladder(
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>",
            &request,
            &limits(),
        );
        assert!(matches!(result, Err(RenderError::UnsupportedInput)));
        let result = render_ladder(
            PNG,
            &request,
            &RenderLimits {
                max_input_bytes: PNG.len() - 1,
                ..limits()
            },
        );
        assert!(matches!(result, Err(RenderError::InputLimit { .. })));
        let result = render_ladder(
            PNG,
            &request,
            &RenderLimits {
                max_outputs: 0,
                ..limits()
            },
        );
        assert!(matches!(result, Err(RenderError::InvalidRequest(_))));
    }
}
