# imgx vs Cloudflare Images — parameter parity tracker

Living checklist mirroring `docs/PRD-workspace-upgrade-and-cloudflare-parity.md`
section 3.b (the Stream B gap table). One row per gap. Kept separate from
`docs/PARITY.md` (that file tracks zimgx-vs-imgx byte-for-byte parity across
the Rust rewrite; this one tracks imgx-vs-Cloudflare-Images URL/parameter
parity).

Every claim below is marked as one of:

- **Verified** — checked against the real, current Cloudflare Images docs
  (`developers.cloudflare.com/images/optimization/features/`), fetched via
  the Cloudflare docs MCP search tool during this work.
- **Spec-derived** — Cloudflare documents the *existence* of the behavior but
  not exact numbers/schema; a defensible value was chosen and is called out
  explicitly below.
- **Not verified / unresolved** — could not be confirmed from available docs
  within this pass; implemented conservatively or skipped, noted below.

## Gap table

| # | Gap | Status | Note |
|---|---|---|---|
| 1 | URL shape | done | `/cdn-cgi/image/<OPTIONS>/<SOURCE>` — shipped in the prior URL migration (INV-5). |
| 2 | Arbitrary remote-URL source | not-planned (this pass) | Explicitly out of scope — SSRF/allowlist design needs its own pass (PRD OQ-2). |
| 3 | `fit` vocabulary | done | See "Gap 3 — fit" below. |
| 4 | `quality`/`q` perceptual strings | done (spec-derived mapping) | See "Gap 4 — quality" below. |
| 5 | `format`/`f`: `baseline-jpeg`, `json` | done | See "Gap 5 — format" below. |
| 6 | `compression=fast` | done | See "Gap 6 — compression" below. |
| 7 | `onerror=redirect` | done | See "Gap 7 — onerror" below. |
| 8 | `slow-connection-quality`/`scq` | done | See "Gap 8 — slow-connection-quality" below. |
| 9 | `trim` per-side keys | partial | Per-side pixel/fraction crop (`trim.top`/`.right`/`.bottom`/`.left`) implemented; legacy numeric `trim=<threshold>` preserved unchanged. `trim.height`/`trim.width` and the single combined `top;right;bottom;left` syntax are NOT implemented — see "Gap 9 — trim" below. |
| 10 | `border` | done (spec-derived URL syntax) | See "Gap 10 — border" below. |
| 11 | `draw` (overlays) | partial (parsing + compositing done, remote fetch deliberately gated) | See "Gap 11 — draw" below. |
| 12 | `gravity`/`g` | partial | Verified side/auto aliases implemented; `face` and `XxY` focal-point coordinates NOT implemented — see "Gap 12 — gravity" below. |
| 13 | `rotate` ordering | done (verified, already correct) | See "Gap 13 — rotate ordering" below. |

## Gap 13 — rotate ordering

**Verified** against `developers.cloudflare.com/images/optimization/features/`
(`rotate` section), fetched via the Cloudflare docs MCP tool:

> "Rotation is performed before resizing; `width` and `height` options will
> refer to the axes after the image is rotated."

imgx's pipeline (`crates/imgx/src/transform/pipeline.rs`) already applies the
`-- ROTATE / FLIP --` stage before the `-- RESIZE --` stage in source order —
this matches Cloudflare's documented behavior. Locked in as a regression test:
`rotate_is_applied_before_resize_axes_reflect_post_rotation_orientation`
(a 2000x1500 source rotated 90 degrees, resized to `w=200`, must produce a
taller-than-wide output reflecting the *post-rotation* 1500x2000 aspect ratio,
not the pre-rotation one).

**Bug found and fixed while writing this test** (unrelated to ordering
itself, but it made the ordering test observably wrong until fixed):
`vips_thumbnail_image` defaults its `height` option to the `width` value when
no height is passed at all — i.e. it fits within a `width` x `width` *square*
box, not "preserve aspect ratio using only the width axis." Every width-only
resize request (`w=` with no `h=`) against a non-square source was silently
producing squared-off dimensions instead of the expected aspect-preserving
resize. Confirmed independently via `vipsthumbnail --size <N>` on
`test/fixtures/bench_2000x1500.png`. Fixed in `pipeline.rs` by deriving an
explicit height option from the (already-rotated) source aspect ratio
whenever only `width` is requested, for fit modes that don't already define
their own target box (`Cover`/`Fill`/`Crop`/`AspectCrop` are excluded — they
require both dimensions to mean anything). See the `derived_height` comment
in `pipeline.rs`'s `-- RESIZE --` section.

## Gap 12 — gravity

**Verified** against `developers.cloudflare.com/images/optimization/features/`
(`gravity`/`g` section) via the Cloudflare docs MCP tool:

> "Accepts `auto`, `face`, a side (`left`, `right`, `top`, `bottom`), and
> relative coordinates (`XxY`)."

Implemented as aliases onto imgx's existing compass-word `Gravity` enum
(`crates/imgx/src/transform/params.rs`) rather than new variants, since they
mean the same thing:

- `top` → `North`, `bottom` → `South`, `left` → `West`, `right` → `East`
  (Cloudflare's own docs describe these as "the side of the image that
  should not be cropped," which is exactly imgx's existing compass-word
  semantics under a different name).
- `auto` → `Smart` (`VIPS_INTERESTING_ENTROPY`) — both are automatic
  saliency-based cropping, the same goal, different underlying algorithm
  (Cloudflare doesn't document its saliency algorithm; libvips' is
  entropy-based). Aliased, not claimed to be pixel-identical.

**Not implemented** (unresolved/deferred, not guessed):

- `face` — real face-detection-based gravity. imgx's `attention`
  (`VIPS_INTERESTING_ATTENTION`) is libvips' saliency-based cropping, NOT
  face detection, so aliasing `face` onto it would overclaim capability that
  doesn't exist. No face-detection library is wired into this crate.
  Left unparsed (`g=face` currently returns `ParseError::InvalidGravity`).
- `XxY` relative-coordinate focal points (e.g. `g=0.33x0.5`) — real Cloudflare
  syntax, confirmed via docs, but implementing it means new crop-math (an
  explicit focal-point-relative crop rectangle), not a parser alias, and was
  judged out of scope for this pass given the size of the remaining P0/P1
  work. Tracked as unresolved, not silently dropped.

## Gap 3 — fit vocabulary

**Verified** against `developers.cloudflare.com/images/optimization/features/`
(`fit` section) via the Cloudflare docs MCP tool — full 8-value table with
per-value upscale/crop/aspect-preservation semantics.

imgx's default (`fit=contain`, `VIPS_SIZE_DOWN` — no crop, downscale-only) is
**unchanged** — that's PRD OQ-3, an explicitly open decision for the repo
owner, not touched in this pass.

Proven pixel-dimension-equivalent to an existing imgx `FitMode` variant, so
parsed as an alias rather than duplicated (see the pipeline tests named after
each pairing in `crates/imgx/src/transform/pipeline.rs`):

- `squeeze` → `Fill` (`VIPS_SIZE_FORCE`): both force the exact requested
  width/height, distorting aspect ratio if needed.
- `scale-up` → `Outside` (`VIPS_SIZE_UP`): both upscale-only, never
  downscale, preserve aspect ratio.
- `scale-down` → `Contain` (`VIPS_SIZE_DOWN`, imgx's existing default):
  both downscale-only, never upscale, preserve aspect ratio. This is an
  additional accepted *string*, not a change to which mode is the default.

New enum variants added (no existing imgx mode has equivalent semantics —
each layers a "never upscale" constraint onto cropping in a way nothing else
in imgx did):

- `crop` (`FitMode::Crop`): fills the target area like `cover`, but never
  upscales — falls back to `scale-down`-like behavior when the source is
  smaller than the target.
- `aspect-crop` (`FitMode::AspectCrop`): crops to the target aspect ratio.
  When the source is large enough to cover the target without upscaling,
  behaves like `crop`/`cover` (downscale then crop). When smaller, crops the
  *original-size* image directly to the target ratio rather than upscaling.

## Gap 4 — quality

**Verified** (existence of the feature) against
`developers.cloudflare.com/images/optimization/features/` (`quality`/`q`
section) via the Cloudflare docs MCP tool:

> "Perceptual quality — Accepts `high`, `medium-high`, `medium-low`, and
> `low`."

Cloudflare does **not** publish exact integer values for these four tiers in
the indexed docs. **Spec-derived mapping** chosen for this pass
(`crates/imgx/src/transform/params.rs`'s `parse_quality`):

| String | Integer |
|---|---|
| `high` | 90 |
| `medium-high` | 80 |
| `medium-low` | 60 |
| `low` | 40 |

imgx's own default quality (80) is **unchanged** (Cloudflare's default is 85
— also PRD OQ-3, not touched here).

## Gap 5 — format additions

**Verified** against `developers.cloudflare.com/images/optimization/features/`
(`format`/`f` section) via the Cloudflare docs MCP tool:

> "`jpeg` — Transcodes the image in interlaced progressive JPEG format...
> `baseline-jpeg` — Transcode the image in baseline sequential JPEG format...
> `json` — Outputs information about the image as a JSON object. This
> contains data such as image size (before and after resizing), the source
> image's MIME type, and file size."

**`baseline-jpeg`**: `vips_jpegsave_buffer` already defaults its `interlace`
option to `FALSE` (baseline) when omitted — imgx's existing `format=jpeg`
encode path was already producing baseline JPEG (a pre-existing divergence
from Cloudflare's own `jpeg` default, which is progressive — NOT changed in
this pass, since making plain `jpeg` progressive would be a default-behavior
change adjacent to OQ-3). `baseline-jpeg` is added as its own `OutputFormat`
variant that shares the exact same encode call as `Jpeg` — no new FFI needed.

**`format=json`**: metadata-only response, no image bytes
(`OutputFormat::Json`, handled in `pipeline.rs`'s `transform()` before the
normal encode step). **Spec-derived schema** (Cloudflare's docs describe the
*content* — "image size before and after resizing... source MIME type, and
file size" — but no exact field-by-field schema was found in the indexed
docs):

```json
{
  "original": { "width": 2000, "height": 1500, "file_size": 123456 },
  "transformed": { "width": 100, "height": 75, "format": "webp", "file_size": 4821 }
}
```

`original.*` reflects the raw probed source (before rotate/resize/crop);
`transformed.*` reflects the actual post-pipeline image, and `format`/
`file_size` are computed by actually negotiating and encoding a real codec
for the transformed image (not guessed) — so the numbers are real, not
placeholders. `original.file_size` is the input byte length; there's no
separate "source MIME type" field in this implementation (would require new
loader-sniffing FFI beyond this pass's scope) — noted here as a schema
simplification, not silently omitted.

## Gap 7 — onerror

**Verified** against `developers.cloudflare.com/images/optimization/features/`
(`onerror` section) via the Cloudflare docs MCP tool:

> "Redirects the end-user to the URL of the original source image when a
> fatal error prevents the image from being transformed. Accepts `redirect`.
> The default is none... This option works only if the image is in the same
> zone."

Implemented as `onerror=redirect`, opt-in per-request
(`TransformParams::onerror: Option<OnErrorMode>`,
`crates/imgx/src/transform/params.rs`). On a transform failure in
`crates/imgx/src/server.rs`'s `handle_image_request`:

- Default (`onerror` unset): imgx's existing raw-bytes fallback, unchanged
  (INV-13 — never cached under the success key).
- `onerror=redirect`: a `302 Found` to the origin image URL instead, via a
  new `Fetcher::origin_url`/`AppOrigin::redirect_url` accessor. Only offered
  for an `Http` origin (a real, redirectable URL) — an R2 origin has no
  public URL, so it silently falls back to the raw-bytes default in that
  case, same as if `onerror` were unset.
- `onerror` is deliberately **excluded** from the transform cache key
  (`to_cache_key`): it only changes failure-path behavior, never the
  successful transform's output bytes, so including it would needlessly
  split one cache entry into two for functionally-identical requests.

See `docs/INVARIANTS.md` INV-13 for the amended note documenting this
additive opt-in path.

## Gap 9 — trim

**Verified** against `developers.cloudflare.com/images/optimization/features/`
(`trim` section) via the Cloudflare docs MCP tool:

> "`top;right;bottom;left` — Specifies the number of pixels to remove from
> the sides of an image... All trim values accept either an integer (pixel
> count) or a decimal between 0 and 1 representing a fraction of the image
> dimension... Trim can also be applied to a specific side using
> `trim.top`/`trim.left`/`trim.height`/`trim.width`."

Cloudflare's `trim` is fundamentally different from imgx's pre-existing
`trim=<threshold>` (a border-color-uniformity threshold passed to libvips'
`find_trim`, already wrapped in `crates/imgx-vips/src/image.rs`) — Cloudflare's
model is a fixed pixel/fraction crop from each edge, blind to border color.
Per OQ-5 in the PRD, the legacy numeric `trim=<threshold>` is preserved
**unchanged** and works alongside the new keys.

**Implemented**: `trim.top`, `trim.right`, `trim.bottom`, `trim.left` — each
accepts either a pixel count (`>= 1.0`) or a `0.0..1.0` fraction of that
side's dimension, resolved in `pipeline.rs` (parse time doesn't know the
image's actual dimensions). All four may be combined in one request.

**Not implemented** (unresolved, not guessed at):

- The combined `top;right;bottom;left` single-value syntax (e.g.
  `trim=0.1;0.2;0.1;0.2`) — collides with imgx's pre-existing numeric
  `trim=<threshold>` key, and disambiguating the two (a bare number vs. a
  semicolon-delimited quad) was judged too easy to get subtly wrong for this
  pass; the four dotted per-side keys cover the same functionality
  individually.
- `trim.height` / `trim.width` ("set the height/width from the top/left edge,
  then trim everything beyond that point") — a related but distinct
  operation from `trim.top`/`trim.left`; deferred to keep this gap's scope
  bounded. Not silently broken — simply unrecognized parameter keys, so a
  request using them today gets `ParseError::InvalidParameter` (400), not a
  wrong/silent result.

## Gap 6 — compression

**Verified** against `developers.cloudflare.com/images/optimization/features/`
(`compression` section) via the Cloudflare docs MCP tool:

> "Selects the output format that is quickest to compress. Accepts `fast`.
> The default is none. The `compression=fast` option prioritizes encoding
> speed over output quality and file size, and will usually override the
> `format` parameter to choose JPEG over more efficient formats like AVIF
> or WebP."

Implemented as `compression=fast` (`TransformParams::compression:
Option<CompressionMode>`, `crates/imgx/src/transform/params.rs`). The actual
bias is applied as a new pure function,
`negotiate::apply_compression_fast(format, compression_fast)`
(`crates/imgx/src/transform/negotiate.rs`), called from `pipeline.rs` *after*
`negotiate_format`/`negotiate_animated_format` resolve a format — deliberately
not folded into `negotiate_format` itself, so that function's existing
30-test priority-order API stays untouched. Behavior:

- If the resolved format is `Avif` or `Webp`, it is downgraded to `Jpeg`.
  This applies whether the format came from Accept-header negotiation *or*
  an explicit `format=avif`/`format=webp` — matching the doc's literal
  "will usually override the format parameter," which our implementation
  treats as an unconditional override (imgx has no per-request encoder
  benchmarking to make Cloudflare's "usually" a variable decision).
- `Jpeg`/`Png`/`Gif`/`BaselineJpeg` are left unchanged (nothing to speed up
  by switching away from).
- Skipped entirely for animated output (`is_animated_output` in the
  pipeline) — forcing JPEG would silently drop animation, an interaction
  Cloudflare's docs don't describe, so animated requests are left alone
  rather than guessing.
- Reducing libvips per-format encoder effort/speed settings (the task's
  "and/or" alternative) was **not implemented**: none of `imgx-vips`'s
  existing save wrappers (`save_jpeg`/`save_webp`/`save_avif`/`save_gif`)
  expose an effort knob today, and since compression=fast already routes
  around the AVIF encoder entirely (the slow one), adding a new FFI-level
  effort parameter for JPEG (which has none) or WebP (whose "effort" option
  is a much smaller win than avoiding AVIF) was judged unnecessary scope for
  this pass.

Cache key: `compression=fast` is included when set
(`cache_key_includes_compression_when_set`).

## Gap 8 — slow-connection-quality / scq

**Verified** against `developers.cloudflare.com/images/optimization/features/`
(`slow-connection-quality`/`scq` section) via the Cloudflare docs MCP tool:

> "Overrides the `quality` value whenever a slow connection is detected.
> Accepts the same fixed or perceptual settings as quality... To detect slow
> connections, enable any of the following client hints via HTTP in a
> header: `accept-ch: rtt, save-data, ect, downlink`. `slow-connection-quality`
> applies when the client hint is present and any of the following
> conditions are met: rtt greater than 150ms; save-data is "on"; ect is one
> of slow-2g/2g/3g; downlink is less than 5Mbps."

Implemented as `scq`/`slow-connection-quality`
(`TransformParams::scq: Option<u8>`, reusing the existing `parse_quality`
fixed/perceptual parser). The trigger conditions are a pure, independently
unit-tested predicate, `params::is_slow_connection(rtt, save_data, ect,
downlink)` — deliberately decoupled from the HTTP layer so the exact
threshold logic is directly testable without spinning up a request.
`crates/imgx/src/server.rs`'s `handle_image_request` reads the four raw
header values (`rtt`, `save-data`, `ect`, `downlink`) off the incoming
request, calls `is_slow_connection`, and — if true and `scq` was requested —
calls `TransformParams::apply_scq_override(true)`, which overwrites
`quality` with the `scq` value *before* the cache key is computed.

**Cache key**: `scq` itself is deliberately **excluded** from
`to_cache_key()` — see `cache_key_omits_scq_since_the_override_already_shows_up_in_quality`.
Instead of a parallel cache-key field, `handle_image_request` now builds the
operational cache key from `TransformParams::to_cache_key()` (the existing
canonical, order-independent serialization) rather than the raw URL options
segment it previously used. Since the override mutates `quality` directly,
and `to_cache_key()` already includes `q=`, two otherwise-identical requests
that differ only in whether the client hints indicate a slow connection
naturally get different cache entries — no bolt-on mechanism needed. This
also incidentally fixes a preexisting divergence: the operational cache key
was previously the raw URL options string (parameter-order-sensitive),
not the canonical, order-independent `to_cache_key()` INV-1 describes;
`w=400,h=300` and `h=300,w=400` now share one cache entry end-to-end, not
just in `to_cache_key()`'s own unit tests.

This feature's `Note` in Cloudflare's docs restricts it to "the URL
interface on Chromium-based browsers" — imgx has no way to detect browser
engine server-side beyond `Accept`/`User-Agent` sniffing, so this
implementation applies the override whenever the qualifying headers are
present, regardless of client, matching the *conditions* but not the
browser-family restriction (a strictly server-observable subset of
Cloudflare's full behavior).

## Gap 10 — border

**Verified (feature existence + semantics), but Workers-only** against
`developers.cloudflare.com/images/optimization/features/` (`border` section)
via the Cloudflare docs MCP tool:

> "Adds a border around the image. Note: This feature is available only in
> Workers. Accepts the following properties: `color`... `width`...
> `top`, `right`, `bottom`, `left`... The border is applied after the image
> has been resized. The border width automatically scales with the dpr
> parameter."

Cloudflare does **not** publish a URL-interface syntax for `border` at all
(it's Workers/`cf.image.border`-only) — so the key names below are
**spec-derived**, chosen to be consistent with imgx's existing conventions
rather than a verified Cloudflare URL form:

- `border=<width>` — uniform width in pixels on all four sides.
- `border.color=<hex>` — 6-hex RGB, reusing the exact `bg`/`background`
  hex-color parser (`parse_hex_color`). Defaults to black (`000000`) when
  unset — Cloudflare's own docs don't state a default, every example shows
  an explicit color.
- `border.top`/`.right`/`.bottom`/`.left=<width>` — per-side pixel width,
  overriding the uniform `border` value for that side only, using the same
  dotted-key convention as `trim.top`/etc.

Implemented in `pipeline.rs`'s new `-- BORDER --` stage, placed after all
resize/effect steps (matching "applied after the image has been resized"),
reusing the existing `VipsImage::embed` wrapper — **no new FFI needed**.
Border pixel values are scaled by `tp.dpr` (rounded), matching "automatically
scales with the dpr parameter." Skipped for animated output, for the same
reason INV-2's resize-with-crop workaround exists: an `embed` on the
vertically-stacked frame buffer would corrupt frame boundaries.

A conservative `0..=2000`px bound is enforced in `validate()`
(`ParseError::InvalidBorder`) — not a Cloudflare-published limit (no numeric
range beyond "in pixels" is documented), chosen to keep the padded canvas
well within the existing 8192 FFI-safety ceiling.

## Gap 11 — draw (overlays)

**Verified (feature existence + semantics), but Workers-only** against
`developers.cloudflare.com/images/optimization/transformations/draw-overlays/`
and `developers.cloudflare.com/images/optimization/draw-overlays/` via the
Cloudflare docs MCP tool. Both pages describe `draw` exclusively as a
`cf.image.draw` array on a Workers `fetch()` subrequest — "This feature is
available only in Workers" — with fields `url`, `width`/`height` (pixels or
a `0..1` fraction of the base image's dimension), `fit`/`gravity` (reusing
the main image's), `opacity` (`0..1`), `repeat` (`true`/`"x"`/`"y"`),
`top`/`left`/`bottom`/`right` (pixel offsets; setting both `top`+`bottom` or
both `left`+`right` is documented as an error), `background`, and `rotate`.

**No URL-interface syntax is published for this feature at all** — so, per
the task's framing, this is designed rather than verified:
`draw.<N>.<field>` (e.g. `draw.0.url=...`, `draw.0.width=...`,
`draw.1.url=...` for a second overlay), a flattened array encoding chosen to
be consistent with imgx's existing dotted-key conventions
(`trim.top`/`border.top`), **not** claimed to be byte-for-byte identical to
any real Cloudflare URL form — that detail isn't confidently verifiable
since Cloudflare has never published one.

**Scope decision — option (b) from the task, not option (a)**: implement
the `draw` array parsing and the libvips compositing pipeline in full,
proven against local/already-fetched image buffers, but do **not** build
the actual remote-URL fetch this pass. Reasoning: a correct, safe SSRF-gated
fetcher (non-http(s) scheme rejection, DNS resolution with private/
loopback/link-local IP-range rejection, redirect-count capping, response-size
capping) is a substantial, security-sensitive undertaking in its own right —
essentially the same shape of problem as gap 2's arbitrary remote-source
fetch, which this task explicitly keeps out of scope as a separate
owner-facing decision (PRD OQ-2). Building a second, narrower SSRF-gated
fetcher just for overlay URLs, correctly, within an already-large multi-gap
pass, was judged more likely to ship a subtly-wrong security boundary than
to build it right. Splitting gap 11 into "prove the parsing and compositing
math work" (done, tested) vs. "fetch arbitrary attacker-influenced URLs
safely" (deferred, alongside gap 2) keeps the security-sensitive surface
area unimplemented rather than half-implemented.

What's implemented and tested:

- **Parsing**: `draw.<N>.<field>` for all documented fields (`url`, `width`,
  `height`, `top`, `left`, `bottom`, `right`, `opacity`, `repeat`,
  `background`, `rotate`, `fit`, `gravity`) into `TransformParams::draw:
  Vec<DrawOverlay>`. `validate()` enforces: `url` non-empty; `top`+`bottom`
  mutually exclusive; `left`+`right` mutually exclusive (matching
  Cloudflare's documented error case); `opacity` in `0.0..=1.0`; no negative
  width/height/position values.
- **Compositing pipeline**: `transform::pipeline::composite_draw_overlay`
  (`crates/imgx/src/transform/pipeline.rs`) takes an already-decoded base
  `VipsImage` and already-fetched overlay bytes and: resizes the overlay to
  `width`/`height` (pixel or base-relative fraction) via `fit`/`gravity`;
  rotates it (reusing the main image's `Rotation` enum — Cloudflare's docs
  say "same as for the main image," which only supports 0/90/180/270, so
  overlay rotation is bound by the same constraint); tiles it via `repeat`
  (`VipsImage::tile_to_size`, `VIPS_EXTEND_REPEAT`); flattens it onto
  `background` if set; scales its alpha band by `opacity` if set; positions
  it via `top`/`left`/`bottom`/`right` (pixel or fraction, default centered);
  and composites it onto the base via the new `VipsImage::composite_over`
  (`vips_composite2`, `VIPS_BLEND_MODE_OVER` — verified against
  `/usr/include/vips/conversion.h` on the installed libvips 8.15.1). All of
  this is tested directly against local fixture bytes
  (`crates/imgx/src/transform/pipeline.rs`'s `composite_draw_overlay_*`
  tests and `crates/imgx-vips/src/image.rs`'s `composite_over_*`/
  `tile_to_size_*` tests) — no network fetch involved.
- **Documented scope limitations** (not silently dropped): opacity
  attenuation only has an effect on overlays that already carry an alpha
  channel (matches Cloudflare's own guidance to use PNG/WebP for overlays);
  when both `background` and `opacity` are set, `background` is applied
  first and destroys the alpha band `opacity` would need, so `opacity` has
  no further effect in that combination; blend mode is always "over"
  (Cloudflare's `draw` doesn't expose a configurable one via its published
  options — the newer `composite` blend-mode option mentioned in
  Cloudflare's June 2026 changelog is a distinct, newer feature not covered
  by the docs indexed during this pass, and is out of scope here).

What's deliberately **not** implemented: the remote-URL fetch itself.
`crates/imgx/src/server.rs`'s `handle_image_request` rejects any request
naming a `draw` overlay with a `422` before attempting an origin fetch, with
a message pointing at `IMGX_ALLOW_DRAW_OVERLAYS` — **a reserved name for
future work, not a config flag that exists in `config.rs` today**. There is
no fetch code path for such a flag to gate yet in this pass; the name is
recorded here so that whoever eventually builds gap 2's general SSRF-safe
remote-source fetcher has a documented, narrower sibling use case to wire
this feature's `Some(url)` fields into once that fetcher exists, per the
task's instruction to note this in
`docs/PRD-workspace-upgrade-and-cloudflare-parity.md`'s open questions.
