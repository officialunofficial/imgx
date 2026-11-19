//! Single-decode ladders and the HTTP renderer, with one corpus and recipe.

use std::fmt::Write;
use std::path::Path;
use std::sync::Arc;

use imgx::config::{Config, TransformFailureMode};
use imgx::server::{AppState, build_router};
use imgx_vips::ladder::{
    Codec, DecodePolicy, Encoded, Geometry, Gravity, LadderRequest, LosslessPolicy, MetadataPolicy,
    OutputSpec, RenderError, RenderLimits, WebpEffort, render_ladder,
};
use imgx_vips::{VipsImage, consts};
use reqwest::{Client, StatusCode, header};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LIMITS: RenderLimits = RenderLimits {
    max_input_bytes: 32 * 1024 * 1024,
    max_pixels: 64_000_000,
    max_edge: 2560,
    max_outputs: 9,
    max_output_bytes: 32 * 1024 * 1024,
    max_total_output_bytes: 64 * 1024 * 1024,
};

const WIDTHS: [u32; 7] = [160, 320, 640, 960, 1280, 1920, 2560];

const MASTER: OutputSpec = OutputSpec {
    geometry: Geometry::MaxEdge(2560),
    codec: Codec::Webp,
    quality: 95,
    effort: WebpEffort::Fixed(4),
    lossless: LosslessPolicy::Source,
    metadata: MetadataPolicy::Srgb,
};

const CARD: OutputSpec = OutputSpec {
    geometry: Geometry::Cover {
        width: 1200,
        height: 630,
        gravity: Gravity::Center,
    },
    codec: Codec::Jpeg,
    ..rung(1200)
};

const fn rung(width: u32) -> OutputSpec {
    OutputSpec {
        geometry: Geometry::ScaleDownWidth(width),
        codec: Codec::Webp,
        quality: 80,
        effort: WebpEffort::ByWidth,
        lossless: LosslessPolicy::Lossy,
        metadata: MetadataPolicy::Strip,
    }
}

struct Case {
    name: &'static str,
    size: (u32, u32),
    alpha: bool,
    ssim_floor: f64,
}

const CASES: [Case; 6] = [
    Case {
        name: "blue-marble.jpg",
        size: (2048, 2048),
        alpha: false,
        ssim_floor: 0.955,
    },
    Case {
        name: "portrait-icc.jpg",
        size: (257, 403),
        alpha: false,
        ssim_floor: 0.965,
    },
    Case {
        name: "cmyk.jpg",
        size: (403, 257),
        alpha: false,
        ssim_floor: 0.955,
    },
    Case {
        name: "transparent.png",
        size: (319, 173),
        alpha: true,
        ssim_floor: 0.91,
    },
    Case {
        name: "small.png",
        size: (17, 9),
        alpha: true,
        ssim_floor: 0.995,
    },
    Case {
        name: "animated.gif",
        size: (403, 257),
        alpha: false,
        ssim_floor: 0.965,
    },
];

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test/fixtures/golden")
        .join(name);
    std::fs::read(path).unwrap()
}

fn decode(bytes: &[u8]) -> VipsImage {
    VipsImage::from_buffer_strict(bytes).unwrap()
}

#[tokio::test]
async fn native_ladder_matches_legacy_http_render_perceptually() {
    let origin = MockServer::start().await;
    let config = Config {
        origin: imgx::config::OriginConfig {
            base_url: origin.uri(),
            ..Default::default()
        },
        transform: imgx::config::TransformConfig {
            on_failure: TransformFailureMode::Error,
            ..Default::default()
        },
        ..Default::default()
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, build_router(Arc::new(AppState::new(config))))
            .await
            .unwrap();
    });
    let client = Client::builder().no_proxy().build().unwrap();
    let mut specs: Vec<_> = WIDTHS.into_iter().map(rung).collect();
    specs.push(CARD);

    for case in CASES {
        let input = fixture(case.name);
        let ladder = render_ladder(
            &input,
            &LadderRequest {
                baseline: Some(MASTER),
                outputs: &specs,
                thumbhash: true,
                decode: DecodePolicy::Strict,
            },
            &LIMITS,
        )
        .unwrap();
        let master = ladder.baseline.unwrap();
        let master_image = decode(&master.bytes);
        assert_eq!((master.width, master.height), case.size, "{}", case.name);
        assert_eq!(master_image.get_int("orientation"), None);
        assert!(has_icc(&master.bytes, Codec::Webp));
        assert_eq!(master_image.has_alpha(), case.alpha);
        assert_eq!(master_image.n_pages().unwrap_or(1), 1);
        assert!(ladder.thumbhash.is_some());
        assert_master_golden(case.name, &master.bytes);

        Mock::given(method("GET"))
            .and(path(format!("/{}.webp", case.name)))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(master.bytes.clone())
                    .insert_header("content-type", "image/webp"),
            )
            .mount(&origin)
            .await;

        let mut minimum_ssim = 1.0_f64;
        let mut max_alpha_error = 0.0_f64;
        for (i, spec) in specs.iter().enumerate() {
            let options = if i == WIDTHS.len() {
                "width=1200,height=630,fit=cover,format=jpeg,quality=80".into()
            } else {
                format!("width={},fit=scale-down,format=webp,quality=80", WIDTHS[i])
            };
            let response = client
                .get(format!("{server}/image/{options}/{}.webp", case.name))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{} {options}", case.name);
            let media = if spec.codec == Codec::Jpeg {
                "image/jpeg"
            } else {
                "image/webp"
            };
            assert_eq!(response.headers()[header::CONTENT_TYPE], media);
            let fallback = response.bytes().await.unwrap();

            let same_bytes = render_ladder(
                &master.bytes,
                &LadderRequest {
                    baseline: None,
                    outputs: std::slice::from_ref(spec),
                    thumbhash: false,
                    decode: DecodePolicy::LenientReduced,
                },
                &LIMITS,
            )
            .unwrap();
            assert!(
                same_bytes.outputs[0].bytes.as_slice() == fallback.as_ref(),
                "{} {options}",
                case.name
            );

            let native = &ladder.outputs[ladder.output_indices[i]];
            check_dimensions(case.size, native, spec);
            assert!(!has_icc(&native.bytes, spec.codec));
            assert!(!has_icc(&fallback, spec.codec), "{} {options}", case.name);
            let native_pixels = decode(&native.bytes);
            let fallback_pixels = decode(&fallback);
            let alpha = case.alpha && spec.codec == Codec::Webp;
            for image in [&native_pixels, &fallback_pixels] {
                assert_eq!(
                    (image.width(), image.height()),
                    (native.width as i32, native.height as i32),
                    "{} {options}",
                    case.name
                );
                assert_eq!(image.has_alpha(), alpha);
                assert_eq!(image.get_int("orientation"), None);
                assert_eq!(image.n_pages().unwrap_or(1), 1);
            }
            let (ssim, alpha_error) = compare_pixels(&native_pixels, &fallback_pixels);
            minimum_ssim = minimum_ssim.min(ssim);
            max_alpha_error = max_alpha_error.max(alpha_error);
            assert!(
                alpha_error <= 0.5,
                "{} {options} mean alpha error {alpha_error}",
                case.name
            );
            assert!(
                ssim >= case.ssim_floor,
                "{} {options} SSIM {ssim}",
                case.name
            );
        }
        for (i, width) in WIDTHS.iter().enumerate().skip(1) {
            let aliases_previous = (*width).min(case.size.0) == WIDTHS[i - 1].min(case.size.0);
            assert_eq!(
                ladder.output_indices[i] == ladder.output_indices[i - 1],
                aliases_previous
            );
        }
        eprintln!(
            "{} minimum SSIM {minimum_ssim:.5}, maximum mean alpha error {max_alpha_error:.5}",
            case.name
        );
    }
    Mock::given(method("GET"))
        .and(path("/corrupt.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(fixture("corrupt.png"))
                .insert_header("content-type", "image/png"),
        )
        .mount(&origin)
        .await;
    let refused = client
        .get(format!("{server}/image/format=thumbhash/corrupt.png"))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.headers()[header::CONTENT_TYPE], "application/json");
    task.abort();
}

// Byte goldens are tied to the native codec build used by CI.
fn assert_master_golden(name: &str, bytes: &[u8]) {
    if std::env::var_os("IMGX_PINNED_GOLDENS").is_none() {
        return;
    }
    let expected = match name {
        "blue-marble.jpg" => "9b3e4f749fe1ae2c596ca3a6e47e5ec8a8b5c589c86de9720d3fc6067d0b8d1d",
        "transparent.png" => "8e415ff7f2863367f97d39e001ffacfcef2ce3e03c50bf63dc8f187d534705a8",
        _ => return,
    };
    let mut digest = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(&mut digest, "{byte:02x}").unwrap();
    }
    assert_eq!(digest, expected, "{name} baseline bytes");
}

fn has_icc(bytes: &[u8], codec: Codec) -> bool {
    if codec == Codec::Jpeg {
        return bytes.windows(12).any(|window| window == b"ICC_PROFILE\0");
    }
    assert_eq!(codec, Codec::Webp);
    let mut chunks = &bytes[12..];
    while let Some((header, body)) = chunks.split_first_chunk::<8>() {
        if &header[..4] == b"ICCP" {
            return true;
        }
        let size = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
        chunks = &body[size.next_multiple_of(2)..];
    }
    false
}

fn check_dimensions(size: (u32, u32), output: &Encoded, spec: &OutputSpec) {
    let expected = match spec.geometry {
        Geometry::ScaleDownWidth(target) => {
            let width = target.min(size.0);
            let height = ((u64::from(width) * u64::from(size.1) + u64::from(size.0 / 2))
                / u64::from(size.0))
            .max(1) as u32;
            (width, height)
        }
        Geometry::Cover { width, height, .. } => (width, height),
        Geometry::MaxEdge(_) => unreachable!(),
    };
    assert_eq!((output.width, output.height), expected);
    match spec.codec {
        Codec::Jpeg => assert!(output.bytes.starts_with(b"\xff\xd8\xff")),
        Codec::Webp => assert_eq!(&output.bytes[8..12], b"WEBP"),
        Codec::Png => unreachable!(),
    }
}

fn compare_pixels(native: &VipsImage, fallback: &VipsImage) -> (f64, f64) {
    let (width, height) = (native.width() as usize, native.height() as usize);
    let bands = if native.has_alpha() { 4 } else { 3 };
    let rgb = |image: &VipsImage| {
        let srgb = image.colourspace(consts::VIPS_INTERPRETATION_sRGB).unwrap();
        assert_eq!(srgb.bands() as usize, bands);
        srgb.write_to_memory().unwrap()
    };
    let (native, fallback) = (rgb(native), rgb(fallback));
    let mut ssim = 0.0;
    let mut windows = 0;
    let mut alpha_error = 0;
    for top in (0..height).step_by(8) {
        for left in (0..width).step_by(8) {
            let mut stats = [[0.0; 5]; 3];
            let mut count = 0.0;
            for y in top..height.min(top + 8) {
                for x in left..width.min(left + 8) {
                    let offset = (y * width + x) * bands;
                    for (channel, values) in stats.iter_mut().enumerate() {
                        let a = f64::from(native[offset + channel]);
                        let b = f64::from(fallback[offset + channel]);
                        for (sum, value) in values.iter_mut().zip([a, b, a * a, b * b, a * b]) {
                            *sum += value;
                        }
                    }
                    if bands == 4 {
                        alpha_error += u64::from(native[offset + 3].abs_diff(fallback[offset + 3]));
                    }
                    count += 1.0;
                }
            }
            for [a, b, aa, bb, ab] in stats {
                let (mean_a, mean_b) = (a / count, b / count);
                let (var_a, var_b) = (aa / count - mean_a * mean_a, bb / count - mean_b * mean_b);
                let cov = ab / count - mean_a * mean_b;
                let (c1, c2) = ((0.01_f64 * 255.0).powi(2), (0.03_f64 * 255.0).powi(2));
                ssim += ((2.0 * mean_a * mean_b + c1) * (2.0 * cov + c2))
                    / ((mean_a * mean_a + mean_b * mean_b + c1) * (var_a + var_b + c2));
                windows += 1;
            }
        }
    }
    (
        ssim / f64::from(windows),
        alpha_error as f64 / (width * height) as f64,
    )
}

#[test]
fn corrupt_input_returns_no_batch() {
    let result = render_ladder(
        &fixture("corrupt.png"),
        &LadderRequest {
            baseline: Some(MASTER),
            outputs: &[rung(160)],
            thumbhash: true,
            decode: DecodePolicy::Strict,
        },
        &LIMITS,
    );
    assert!(
        matches!(result, Err(RenderError::CorruptInput(_))),
        "{result:?}"
    );
}
