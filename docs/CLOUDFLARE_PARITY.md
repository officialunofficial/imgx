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
| 6 | `compression=fast` | not-planned | P2, explicitly deferred per PRD. |
| 7 | `onerror=redirect` | done | See "Gap 7 — onerror" below. |
| 8 | `slow-connection-quality`/`scq` | not-planned | P2, explicitly deferred per PRD. |
| 9 | `trim` per-side keys | partial | Per-side pixel/fraction crop (`trim.top`/`.right`/`.bottom`/`.left`) implemented; legacy numeric `trim=<threshold>` preserved unchanged. `trim.height`/`trim.width` and the single combined `top;right;bottom;left` syntax are NOT implemented — see "Gap 9 — trim" below. |
| 10 | `border` | not-planned | P2, explicitly deferred per PRD. |
| 11 | `draw` (overlays) | not-planned | P2, explicitly deferred per PRD (largest net-new feature; PRD OQ-9). |
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
