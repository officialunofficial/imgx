//! Single-decode ladders and the HTTP renderer, with one corpus and recipe.

use std::fmt::Write;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use imgx::config::{Config, TransformFailureMode};
use imgx::server::{AppState, build_router};
use imgx_vips::ladder::{
    Codec, DecodePolicy, Encoded, Geometry, Gravity, LadderRequest, LosslessPolicy, MetadataPolicy,
    OutputSpec, RenderError, RenderLimits, WebpEffort, render_ladder,
};
use imgx_vips::{VipsImage, consts, thumbhash, vips_version};
use reqwest::{Client, StatusCode, header};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LIMITS: RenderLimits = RenderLimits::new(
    32 * 1024 * 1024,
    64_000_000,
    2560,
    9,
    32 * 1024 * 1024,
    64 * 1024 * 1024,
);

const WIDTHS: [u32; 7] = [160, 320, 640, 960, 1280, 1920, 2560];

const MASTER: OutputSpec = {
    let mut spec = OutputSpec::new(Geometry::MaxEdge(2560), Codec::Webp, 95);
    spec.effort = WebpEffort::Fixed(4);
    spec.lossless = LosslessPolicy::Source;
    spec.metadata = MetadataPolicy::Srgb;
    spec
};

const CARD: OutputSpec = OutputSpec::new(
    Geometry::Cover {
        width: 1200,
        height: 630,
        gravity: Gravity::Center,
    },
    Codec::Jpeg,
    80,
);

const fn rung(width: u32) -> OutputSpec {
    OutputSpec::new(Geometry::ScaleDownWidth(width), Codec::Webp, 80)
}

struct Case {
    name: &'static str,
    size: (u32, u32),
    alpha: bool,
    ssim_floor: f64,
}

const CASES: &[Case] = &[
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
        ssim_floor: 0.90,
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
    Case {
        name: "thin-tall.png",
        size: (40, 2400),
        alpha: false,
        ssim_floor: 0.96,
    },
    Case {
        name: "thin-wide.png",
        size: (2400, 40),
        alpha: false,
        ssim_floor: 0.96,
    },
    Case {
        name: "wide-master.jpg",
        size: (2560, 1280),
        alpha: false,
        ssim_floor: 0.93,
    },
    Case {
        name: "portrait.heic",
        size: (257, 403),
        alpha: false,
        ssim_floor: 0.96,
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
    check_http_goldens(false).await;
}

#[tokio::test]
async fn native_ladder_matches_opt_in_http_render_perceptually() {
    check_http_goldens(true).await;
}

async fn check_http_goldens(static_ladder: bool) {
    let origin = MockServer::start().await;
    let client = Client::builder().no_proxy().build().unwrap();
    let (server, _server) = start_server(&origin, &client, static_ladder).await;
    let mut specs: Vec<_> = WIDTHS.into_iter().map(rung).collect();
    specs.push(CARD);

    eprintln!("linked libvips {:?}", vips_version());
    for case in CASES {
        let input = fixture(case.name);
        let mut request = LadderRequest::new(&specs, DecodePolicy::Strict);
        request.baseline = Some(MASTER);
        request.thumbhash = true;
        let ladder = match render_ladder(&input, &request, &LIMITS) {
            Err(RenderError::UnsupportedInput)
                if case.name == "portrait.heic"
                    && std::env::var_os("IMGX_PINNED_GOLDENS").is_none() =>
            {
                eprintln!("portrait.heic skipped: system HEVC decoder unavailable");
                continue;
            }
            result => result.unwrap_or_else(|error| panic!("{}: {error}", case.name)),
        };
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
            let url = format!("{server}/image/{options}/{}.webp", case.name);
            let response = client.get(&url).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{} {options}", case.name);
            let media = if spec.codec == Codec::Jpeg {
                "image/jpeg"
            } else {
                "image/webp"
            };
            assert_eq!(response.headers()[header::CONTENT_TYPE], media);
            let etag = response.headers()[header::ETAG].clone();
            let fallback = response.bytes().await.unwrap();
            if i == 0 {
                let cached = client
                    .get(&url)
                    .header(header::IF_NONE_MATCH, etag)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
                assert!(cached.bytes().await.unwrap().is_empty());
            }

            let same_bytes = render_ladder(
                &master.bytes,
                &LadderRequest::new(std::slice::from_ref(spec), DecodePolicy::LenientReduced),
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
}

enum GoldenServer {
    Embedded(tokio::task::JoinHandle<()>),
    Runtime(Child),
}

impl Drop for GoldenServer {
    fn drop(&mut self) {
        match self {
            Self::Embedded(task) => task.abort(),
            Self::Runtime(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

async fn start_server(
    origin: &MockServer,
    client: &Client,
    static_ladder: bool,
) -> (String, GoldenServer) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = format!("http://127.0.0.1:{port}");
    let server_process = if let Some(binary) = std::env::var_os("IMGX_GOLDEN_SERVER") {
        // The installed binary binds its own port.
        drop(listener);
        let child = Command::new(binary)
            .env("IMGX_SERVER_HOST", "127.0.0.1")
            .env("IMGX_SERVER_PORT", port.to_string())
            .env("IMGX_ORIGIN_BASE_URL", origin.uri())
            .env("IMGX_TRANSFORM_FAILURE", "error")
            .env("IMGX_TRANSFORM_STATIC_LADDER", static_ladder.to_string())
            .env("RUST_LOG", "warn")
            .spawn()
            .expect("start imgx from the runtime image");
        GoldenServer::Runtime(child)
    } else {
        let config = Config {
            origin: imgx::config::OriginConfig {
                base_url: origin.uri(),
                ..Default::default()
            },
            transform: imgx::config::TransformConfig {
                on_failure: TransformFailureMode::Error,
                static_ladder,
                ..Default::default()
            },
            ..Default::default()
        };
        GoldenServer::Embedded(tokio::spawn(async move {
            axum::serve(listener, build_router(Arc::new(AppState::new(config))))
                .await
                .unwrap();
        }))
    };
    let health = format!("{server}/health");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if client
                .get(&health)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("imgx health endpoint");
    (server, server_process)
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
            match (size, width) {
                // The rounded height is the tighter side of these boxes.
                ((2400, 40), 320) => (300, 5),
                ((2400, 40), 1280) => (1260, 21),
                _ => (width, height),
            }
        }
        Geometry::Cover { width, height, .. } => (width, height),
        _ => unreachable!(),
    };
    assert_eq!((output.width, output.height), expected);
    match spec.codec {
        Codec::Jpeg => assert!(output.bytes.starts_with(b"\xff\xd8\xff")),
        Codec::Webp => assert_eq!(&output.bytes[8..12], b"WEBP"),
        _ => unreachable!(),
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
    let outputs = [rung(160)];
    let mut request = LadderRequest::new(&outputs, DecodePolicy::Strict);
    request.baseline = Some(MASTER);
    request.thumbhash = true;
    let result = render_ladder(&fixture("corrupt.png"), &request, &LIMITS);
    assert!(
        matches!(result, Err(RenderError::CorruptInput(_))),
        "{result:?}"
    );
}

#[test]
fn direct_thumbhash_uses_opaque_cmyk_and_full_range_rgb16() {
    let mut request = LadderRequest::new(&[], DecodePolicy::Strict);
    request.thumbhash = true;
    for name in ["cmyk.jpg", "rgb16.png"] {
        let input = fixture(name);
        let hash = render_ladder(&input, &request, &LIMITS)
            .unwrap()
            .thumbhash
            .unwrap();
        assert_eq!(hash[2] & 0x80, 0, "{name} has no alpha");

        // Encode an explicitly normalized 8-bit reference at the hash size.
        let reference = decode(&input)
            .thumbnail(
                thumbhash::MAX_SIDE as i32,
                imgx_vips::ThumbnailOptions {
                    height: Some(thumbhash::MAX_SIDE as i32),
                    size: Some(consts::VIPS_SIZE_DOWN),
                    ..Default::default()
                },
            )
            .unwrap()
            .colourspace(consts::VIPS_INTERPRETATION_sRGB)
            .unwrap();
        let reference = reference.save_png(6, true).unwrap();
        let expected = render_ladder(&reference, &request, &LIMITS)
            .unwrap()
            .thumbhash
            .unwrap();
        assert_eq!(hash, expected, "{name} matches its 8-bit sRGB reference");
    }
}

#[test]
fn smart_crop_keeps_the_pre_crop_pixel_limit() {
    let input = fixture("thin-tall.png");
    for gravity in [Gravity::Attention, Gravity::Entropy] {
        let mut spec = CARD;
        spec.geometry = Geometry::Cover {
            width: 1200,
            height: 630,
            gravity,
        };
        let outputs = [spec];
        let request = LadderRequest::new(&outputs, DecodePolicy::Strict);
        assert!(matches!(
            render_ladder(&input, &request, &LIMITS),
            Err(RenderError::PixelLimit {
                actual: 86_400_000,
                limit: 64_000_000
            })
        ));
    }
}

#[test]
#[ignore = "requires libheif with its HEVC plugin disabled"]
fn heic_without_decoder_is_unsupported() {
    let outputs = [rung(160)];
    let request = LadderRequest::new(&outputs, DecodePolicy::Strict);
    let result = render_ladder(&fixture("portrait.heic"), &request, &LIMITS);
    assert!(
        matches!(result, Err(RenderError::UnsupportedInput)),
        "{result:?}"
    );
}
