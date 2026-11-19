//! Adapter for static renditions of a WebP master.

use imgx_vips::VipsError;
use imgx_vips::ladder::{
    Codec, DecodePolicy, Geometry, Gravity, LadderRequest, LosslessPolicy, MetadataPolicy,
    OutputSpec, RenderError, RenderLimits, WebpEffort, render_ladder,
};

use super::params::{AnimMode, FitMode, MetadataMode, OutputFormat, TransformParams};
use super::pipeline::{TransformError, TransformResult};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_EDGE: u32 = 8192;

/// Render a compatible request. Other requests continue through the full pipeline.
pub(crate) fn render(
    input: &[u8],
    params: &TransformParams,
    max_pixels: u64,
) -> Option<Result<TransformResult, TransformError>> {
    if input.len() > MAX_BYTES || !input.starts_with(b"RIFF") || input.get(8..12) != Some(b"WEBP") {
        return None;
    }
    let (format, output) = recipe(params)?;
    // The legacy renderer can keep animation for WebP. The shared decoder
    // checks its header before we decide which path to use.
    let decode = if format == OutputFormat::Webp && params.anim != AnimMode::Static {
        DecodePolicy::LenientReducedStill
    } else {
        DecodePolicy::LenientReduced
    };
    let request = LadderRequest {
        baseline: None,
        outputs: &[output],
        thumbhash: false,
        decode,
    };
    let limits = RenderLimits {
        max_input_bytes: MAX_BYTES,
        max_pixels,
        max_edge: MAX_EDGE,
        max_outputs: 1,
        max_output_bytes: MAX_BYTES,
        max_total_output_bytes: MAX_BYTES,
    };
    let rendered = match render_ladder(input, &request, &limits) {
        Ok(rendered) => rendered,
        Err(RenderError::UnsupportedInput | RenderError::AnimatedInput) => return None,
        Err(RenderError::PixelLimit { actual, limit }) => {
            return Some(Err(TransformError::ExceedsMaxPixels(actual, limit)));
        }
        Err(error) => return Some(Err(VipsError::OperationFailed(error.to_string()).into())),
    };
    let output = rendered
        .outputs
        .into_iter()
        .next()
        .expect("one requested output");
    Some(Ok(TransformResult {
        data: output.bytes,
        format,
        width: output.width,
        height: output.height,
        is_animated: false,
        frame_count: None,
        decoded_at_reduced_size: rendered.timings.outputs[0].decode_resize.is_some(),
    }))
}

fn recipe(params: &TransformParams) -> Option<(OutputFormat, OutputSpec)> {
    // Use a default-based comparison so future transform parameters need an
    // explicit opt-in before the shared path can receive them.
    if *params
        != (TransformParams {
            width: params.width,
            height: params.height,
            format: params.format,
            quality: params.quality,
            gravity: params.gravity,
            fit: params.fit,
            anim: params.anim,
            dpr: params.dpr,
            onerror: params.onerror,
            scq: params.scq,
            ..Default::default()
        })
        || params.metadata != MetadataMode::Strip
        || params.anim == AnimMode::Animate
    {
        return None;
    }
    let format = params.format?;
    let width = params.effective_width()?;
    let geometry = match (params.fit, params.effective_height()) {
        (FitMode::Contain | FitMode::Inside, None) => Geometry::ScaleDownWidth(width),
        (FitMode::Cover, Some(height)) if params.gravity == super::params::Gravity::Center => {
            Geometry::Cover {
                width,
                height,
                gravity: Gravity::Center,
            }
        }
        _ => return None,
    };
    let codec = match format {
        OutputFormat::Webp => Codec::Webp,
        OutputFormat::Jpeg | OutputFormat::BaselineJpeg => Codec::Jpeg,
        _ => return None,
    };
    Some((
        format,
        OutputSpec {
            geometry,
            codec,
            quality: params.quality,
            effort: WebpEffort::ByWidth,
            lossless: LosslessPolicy::Lossy,
            metadata: MetadataPolicy::Strip,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::super::params::parse;
    use super::*;
    use imgx_vips::VipsImage;

    #[test]
    fn shared_static_resizes_webp_and_defers_effects() {
        imgx_vips::init().unwrap();
        let input = include_bytes!("../../../../test/fixtures/static.webp");
        let params = parse("width=128,format=webp").unwrap();
        let rendered = render(input, &params, 1_000_000)
            .expect("shared recipe")
            .unwrap();
        assert_eq!(rendered.format, OutputFormat::Webp);
        assert!(!rendered.is_animated);
        assert!(rendered.decoded_at_reduced_size);
        assert_eq!(
            rendered.width,
            128.min(VipsImage::from_buffer(input).unwrap().width() as u32)
        );
        let params = parse("width=128,format=webp,sharpen=1").unwrap();
        assert!(render(input, &params, 1_000_000).is_none());
    }

    #[test]
    fn automatic_animation_stays_on_the_existing_renderer() {
        imgx_vips::init().unwrap();
        let gif = include_bytes!("../../../../test/fixtures/loading.gif");
        let webp = VipsImage::from_buffer_animated(gif, 2)
            .unwrap()
            .save_webp(80, true)
            .unwrap();
        let params = parse("width=128,format=webp").unwrap();
        assert!(render(&webp, &params, 1_000_000).is_none());
        let params = parse("width=128,format=webp,anim=static").unwrap();
        let rendered = render(&webp, &params, 1_000_000)
            .expect("static request")
            .unwrap();
        let image = VipsImage::from_buffer(&rendered.data).unwrap();
        assert_eq!(image.n_pages().unwrap_or(1), 1);
    }
}
