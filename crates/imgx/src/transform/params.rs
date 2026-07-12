//! Transform parameter parsing. Ported from src/transform/params.zig.
//! See docs/INVARIANTS.md INV-1 (cache key canonical format) and INV-6
//! (bounds/NaN/Inf rejection — the sole gate before values reach vips FFI).

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("invalid width")]
    InvalidWidth,
    #[error("invalid height")]
    InvalidHeight,
    #[error("invalid format")]
    InvalidFormat,
    #[error("invalid quality")]
    InvalidQuality,
    #[error("invalid fit mode")]
    InvalidFitMode,
    #[error("invalid gravity")]
    InvalidGravity,
    #[error("invalid sharpen value")]
    InvalidSharpen,
    #[error("invalid blur value")]
    InvalidBlur,
    #[error("invalid dpr value")]
    InvalidDpr,
    #[error("invalid rotation")]
    InvalidRotation,
    #[error("invalid flip")]
    InvalidFlip,
    #[error("invalid brightness value")]
    InvalidBrightness,
    #[error("invalid contrast value")]
    InvalidContrast,
    #[error("invalid saturation value")]
    InvalidSaturation,
    #[error("invalid gamma value")]
    InvalidGamma,
    #[error("invalid background color")]
    InvalidBackground,
    #[error("invalid metadata mode")]
    InvalidMetadata,
    #[error("invalid trim value")]
    InvalidTrim,
    #[error("invalid anim mode")]
    InvalidAnim,
    #[error("invalid frame index")]
    InvalidFrame,
    #[error("unknown transform parameter")]
    InvalidParameter,
    #[error("empty parameter value")]
    EmptyValue,
    #[error("invalid onerror mode")]
    InvalidOnError,
    #[error("invalid compression mode")]
    InvalidCompression,
    #[error("invalid border value")]
    InvalidBorder,
    #[error("invalid draw overlay value")]
    InvalidDraw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Auto,
    Jpeg,
    Png,
    Webp,
    Avif,
    Gif,
    /// Cloudflare's `baseline-jpeg` -- non-progressive (baseline
    /// sequential) JPEG. `vips_jpegsave_buffer` already defaults
    /// `interlace` to FALSE, so this shares the exact same encode path
    /// as `Jpeg` (see `encode_image` in pipeline.rs) -- it exists as its
    /// own variant so it round-trips distinctly through parsing/cache
    /// keys, not because the encoded bytes differ.
    BaselineJpeg,
    /// Cloudflare's `format=json` -- metadata-only response, no image
    /// bytes. See docs/CLOUDFLARE_PARITY.md for the (spec-derived) JSON
    /// schema pipeline.rs produces for this format.
    Json,
}

impl OutputFormat {
    pub fn content_type(self) -> &'static str {
        match self {
            OutputFormat::Jpeg | OutputFormat::BaselineJpeg => "image/jpeg",
            OutputFormat::Png => "image/png",
            OutputFormat::Webp => "image/webp",
            OutputFormat::Avif => "image/avif",
            OutputFormat::Gif => "image/gif",
            OutputFormat::Json => "application/json",
            OutputFormat::Auto => "application/octet-stream",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Jpeg | OutputFormat::BaselineJpeg => "jpg",
            OutputFormat::Png => "png",
            OutputFormat::Webp => "webp",
            OutputFormat::Avif => "avif",
            OutputFormat::Gif => "gif",
            OutputFormat::Json => "json",
            OutputFormat::Auto => "",
        }
    }

    pub fn parse_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "jpeg" | "jpg" => Ok(OutputFormat::Jpeg),
            "png" => Ok(OutputFormat::Png),
            "webp" => Ok(OutputFormat::Webp),
            "avif" => Ok(OutputFormat::Avif),
            "gif" => Ok(OutputFormat::Gif),
            "baseline-jpeg" => Ok(OutputFormat::BaselineJpeg),
            "json" => Ok(OutputFormat::Json),
            "auto" => Ok(OutputFormat::Auto),
            _ => Err(ParseError::InvalidFormat),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Jpeg => "jpeg",
            OutputFormat::Png => "png",
            OutputFormat::Webp => "webp",
            OutputFormat::Avif => "avif",
            OutputFormat::Gif => "gif",
            OutputFormat::BaselineJpeg => "baseline-jpeg",
            OutputFormat::Json => "json",
            OutputFormat::Auto => "auto",
        }
    }

    pub fn supports_animation(self) -> bool {
        matches!(self, OutputFormat::Gif | OutputFormat::Webp)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

impl Rotation {
    pub fn parse_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "0" => Ok(Rotation::Deg0),
            "90" => Ok(Rotation::Deg90),
            "180" => Ok(Rotation::Deg180),
            "270" => Ok(Rotation::Deg270),
            _ => Err(ParseError::InvalidRotation),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Rotation::Deg0 => "0",
            Rotation::Deg90 => "90",
            Rotation::Deg180 => "180",
            Rotation::Deg270 => "270",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlipMode {
    H,
    V,
    Hv,
}

impl FlipMode {
    pub fn parse_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "h" => Ok(FlipMode::H),
            "v" => Ok(FlipMode::V),
            "hv" | "vh" => Ok(FlipMode::Hv),
            _ => Err(ParseError::InvalidFlip),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FlipMode::H => "h",
            FlipMode::V => "v",
            FlipMode::Hv => "hv",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetadataMode {
    #[default]
    Strip,
    Keep,
    Copyright,
}

impl MetadataMode {
    pub fn parse_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "strip" | "none" => Ok(MetadataMode::Strip),
            "keep" | "all" => Ok(MetadataMode::Keep),
            "copyright" => Ok(MetadataMode::Copyright),
            _ => Err(ParseError::InvalidMetadata),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MetadataMode::Strip => "strip",
            MetadataMode::Keep => "keep",
            MetadataMode::Copyright => "copyright",
        }
    }
}

/// Cloudflare's `onerror` -- opt-in per-request behavior on transform
/// failure. Default (unset) preserves imgx's existing raw-bytes-fallback
/// (INV-13); `Redirect` additively opts a single request into a 302 to
/// the origin image URL instead. See docs/INVARIANTS.md's INV-13 note on
/// the opt-in path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnErrorMode {
    Redirect,
}

impl OnErrorMode {
    pub fn parse_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "redirect" => Ok(OnErrorMode::Redirect),
            _ => Err(ParseError::InvalidOnError),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OnErrorMode::Redirect => "redirect",
        }
    }
}

/// Cloudflare's `compression=fast` (gap 6, docs/CLOUDFLARE_PARITY.md):
/// prioritizes encode speed over output quality/size, biasing format
/// negotiation away from the slowest encoder (AVIF) toward JPEG. Only one
/// value is defined (`fast`) -- an enum rather than a bare bool to mirror
/// Cloudflare's own string-valued option and leave room for a future
/// value without a breaking field-type change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionMode {
    Fast,
}

impl CompressionMode {
    pub fn parse_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "fast" => Ok(CompressionMode::Fast),
            _ => Err(ParseError::InvalidCompression),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CompressionMode::Fast => "fast",
        }
    }
}

/// Cloudflare's `draw` overlay `repeat` sub-parameter (gap 11,
/// docs/CLOUDFLARE_PARITY.md): tile the overlay across the base image,
/// either in both directions (`true`) or a single axis (`x`/`y`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawRepeat {
    Both,
    X,
    Y,
}

impl DrawRepeat {
    pub fn parse_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "true" => Ok(DrawRepeat::Both),
            "x" => Ok(DrawRepeat::X),
            "y" => Ok(DrawRepeat::Y),
            _ => Err(ParseError::InvalidDraw),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DrawRepeat::Both => "true",
            DrawRepeat::X => "x",
            DrawRepeat::Y => "y",
        }
    }
}

/// A single entry of Cloudflare's `draw` overlay array (gap 11,
/// docs/CLOUDFLARE_PARITY.md). Parsed from flattened `draw.<N>.<field>`
/// URL keys -- Cloudflare only publishes this feature's syntax for the
/// Workers `cf.image.draw` array, not the URL-transform interface, so
/// this flattened dotted-index encoding is spec-derived (consistent with
/// imgx's existing `trim.top`-style dotted-key convention), not a
/// verified Cloudflare URL syntax. See CLOUDFLARE_PARITY.md gap 11 for
/// the full provenance note.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DrawOverlay {
    pub url: Option<String>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub top: Option<f32>,
    pub left: Option<f32>,
    pub bottom: Option<f32>,
    pub right: Option<f32>,
    pub opacity: Option<f32>,
    pub repeat: Option<DrawRepeat>,
    pub background: Option<[u8; 3]>,
    pub rotate: Option<Rotation>,
    pub fit: Option<FitMode>,
    pub gravity: Option<Gravity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimMode {
    #[default]
    Auto,
    Static,
    Animate,
}

impl AnimMode {
    pub fn parse_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "auto" | "true" => Ok(AnimMode::Auto),
            "static" | "false" => Ok(AnimMode::Static),
            "animate" => Ok(AnimMode::Animate),
            _ => Err(ParseError::InvalidAnim),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AnimMode::Auto => "auto",
            AnimMode::Static => "static",
            AnimMode::Animate => "animate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FitMode {
    #[default]
    Contain,
    Cover,
    Fill,
    Inside,
    Outside,
    Pad,
    /// Cloudflare's `crop`: fills the target area like `cover`, but never
    /// upscales (falls back to `scale-down` semantics when the source is
    /// smaller than the target). No dimension-equivalent existing variant
    /// -- see docs/CLOUDFLARE_PARITY.md gap 3.
    Crop,
    /// Cloudflare's `aspect-crop`: crops to the target aspect ratio,
    /// downscaling (never upscaling) if the source is larger than the
    /// target's covering size, otherwise cropping the original-size
    /// image directly to the target ratio. See docs/CLOUDFLARE_PARITY.md
    /// gap 3.
    AspectCrop,
}

impl FitMode {
    pub fn parse_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "contain" => Ok(FitMode::Contain),
            "cover" => Ok(FitMode::Cover),
            "fill" => Ok(FitMode::Fill),
            "inside" => Ok(FitMode::Inside),
            "outside" => Ok(FitMode::Outside),
            "pad" => Ok(FitMode::Pad),
            "crop" => Ok(FitMode::Crop),
            "aspect-crop" => Ok(FitMode::AspectCrop),
            // Cloudflare aliases proven pixel-dimension-equivalent to an
            // existing variant (docs/CLOUDFLARE_PARITY.md gap 3) --
            // parsed straight into that variant rather than duplicated.
            "squeeze" => Ok(FitMode::Fill),
            "scale-up" => Ok(FitMode::Outside),
            "scale-down" => Ok(FitMode::Contain),
            _ => Err(ParseError::InvalidFitMode),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FitMode::Contain => "contain",
            FitMode::Cover => "cover",
            FitMode::Fill => "fill",
            FitMode::Inside => "inside",
            FitMode::Outside => "outside",
            FitMode::Pad => "pad",
            FitMode::Crop => "crop",
            FitMode::AspectCrop => "aspect-crop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Gravity {
    #[default]
    Center,
    North,
    South,
    East,
    West,
    Northeast,
    Northwest,
    Southeast,
    Southwest,
    Smart,
    Attention,
}

impl Gravity {
    pub fn parse_str(s: &str) -> Result<Self, ParseError> {
        match s {
            "center" | "centre" => Ok(Gravity::Center),
            // "top"/"bottom"/"left"/"right" are Cloudflare's own gravity
            // vocabulary (developers.cloudflare.com/images/optimization/
            // features/#gravity--g: "Sets the side of the image that
            // should not be cropped") -- mapped onto imgx's existing
            // compass words, which mean the same thing.
            "north" | "n" | "top" => Ok(Gravity::North),
            "south" | "s" | "bottom" => Ok(Gravity::South),
            "east" | "e" | "right" => Ok(Gravity::East),
            "west" | "w" | "left" => Ok(Gravity::West),
            "northeast" | "ne" => Ok(Gravity::Northeast),
            "northwest" | "nw" => Ok(Gravity::Northwest),
            "southeast" | "se" => Ok(Gravity::Southeast),
            "southwest" | "sw" => Ok(Gravity::Southwest),
            // Cloudflare's `auto` is a saliency algorithm picking the
            // most visually interesting pixels -- the same goal as
            // imgx's `smart` (VIPS_INTERESTING_ENTROPY), so it's aliased
            // rather than duplicated. `face` (face-detection-based) and
            // `XxY` focal-point coordinates have no imgx equivalent and
            // are NOT implemented -- see docs/CLOUDFLARE_PARITY.md gap 12
            // (unresolved: would need new crop math, not a parser alias).
            "smart" | "auto" => Ok(Gravity::Smart),
            "attention" | "att" => Ok(Gravity::Attention),
            _ => Err(ParseError::InvalidGravity),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Gravity::Center => "center",
            Gravity::North => "north",
            Gravity::South => "south",
            Gravity::East => "east",
            Gravity::West => "west",
            Gravity::Northeast => "northeast",
            Gravity::Northwest => "northwest",
            Gravity::Southeast => "southeast",
            Gravity::Southwest => "southwest",
            Gravity::Smart => "smart",
            Gravity::Attention => "attention",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransformParams {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub quality: u8,
    pub format: Option<OutputFormat>,
    pub fit: FitMode,
    pub gravity: Gravity,
    pub sharpen: Option<f32>,
    pub blur: Option<f32>,
    pub dpr: f32,
    pub rotate: Option<Rotation>,
    pub flip: Option<FlipMode>,
    pub brightness: Option<f32>,
    pub contrast: Option<f32>,
    pub saturation: Option<f32>,
    pub gamma: Option<f32>,
    pub background: Option<[u8; 3]>,
    pub metadata: MetadataMode,
    pub trim: Option<f32>,
    pub anim: AnimMode,
    pub frame: Option<u32>,
    /// Cloudflare per-side trim keys (`trim.top`/`trim.right`/
    /// `trim.bottom`/`trim.left`). Values `>= 1.0` are pixel counts;
    /// values in `0.0..1.0` are a fraction of that side's dimension,
    /// resolved against the actual image size in pipeline.rs (parse time
    /// doesn't know the image dimensions yet). Additive alongside the
    /// legacy numeric `trim` threshold -- see OQ-5 in the PRD.
    pub trim_top: Option<f32>,
    pub trim_right: Option<f32>,
    pub trim_bottom: Option<f32>,
    pub trim_left: Option<f32>,
    /// Cloudflare's `onerror=redirect` -- deliberately excluded from
    /// `to_cache_key` (see `cache_key_omits_onerror_since_it_does_not_affect_output_bytes`):
    /// it only changes failure-path handling in server.rs, never the
    /// successful transform's output bytes.
    pub onerror: Option<OnErrorMode>,
    /// Cloudflare's `compression=fast` (gap 6) -- see `CompressionMode`.
    pub compression: Option<CompressionMode>,
    /// Cloudflare's `slow-connection-quality`/`scq` (gap 8): overrides
    /// `quality` when the request carries client hints indicating a slow
    /// connection. Accepts the same fixed/perceptual values as `quality`.
    /// Deliberately excluded from `to_cache_key` -- see
    /// `cache_key_omits_scq_since_the_override_already_shows_up_in_quality`:
    /// when the override actually applies, `server.rs` mutates `quality`
    /// itself before the cache key is computed, so the effect is already
    /// captured by the existing `q=` field.
    pub scq: Option<u8>,
    /// Cloudflare's `border` (gap 10): uniform width in pixels applied to
    /// all four sides, overridable per-side via `border_top`/etc. No
    /// Cloudflare URL syntax is published for this feature (Cloudflare's
    /// own docs mark `border` "available only in Workers") -- this flat
    /// key encoding is spec-derived, documented in CLOUDFLARE_PARITY.md.
    pub border_width: Option<u32>,
    pub border_color: Option<[u8; 3]>,
    pub border_top: Option<u32>,
    pub border_right: Option<u32>,
    pub border_bottom: Option<u32>,
    pub border_left: Option<u32>,
    /// Cloudflare's `draw` overlay array (gap 11) -- see `DrawOverlay`.
    pub draw: Vec<DrawOverlay>,
}

impl Default for TransformParams {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            quality: 80,
            format: None,
            fit: FitMode::default(),
            gravity: Gravity::default(),
            sharpen: None,
            blur: None,
            dpr: 1.0,
            rotate: None,
            flip: None,
            brightness: None,
            contrast: None,
            saturation: None,
            gamma: None,
            background: None,
            metadata: MetadataMode::default(),
            trim: None,
            anim: AnimMode::default(),
            frame: None,
            trim_top: None,
            trim_right: None,
            trim_bottom: None,
            trim_left: None,
            onerror: None,
            compression: None,
            scq: None,
            border_width: None,
            border_color: None,
            border_top: None,
            border_right: None,
            border_bottom: None,
            border_left: None,
            draw: Vec::new(),
        }
    }
}

impl TransformParams {
    /// Effective width after applying the DPR multiplier, clamped to 8192.
    pub fn effective_width(&self) -> Option<u32> {
        let w = self.width?;
        let result = w as f32 * self.dpr;
        Some(result.min(8192.0) as u32)
    }

    /// Effective height after applying the DPR multiplier, clamped to 8192.
    pub fn effective_height(&self) -> Option<u32> {
        let h = self.height?;
        let result = h as f32 * self.dpr;
        Some(result.min(8192.0) as u32)
    }

    /// Validate that all parameter values are within acceptable bounds.
    /// See docs/INVARIANTS.md INV-6 — this is the sole gate before values
    /// reach libvips FFI calls that do not themselves validate ranges.
    pub fn validate(&self) -> Result<(), ParseError> {
        if let Some(w) = self.width
            && !(1..=8192).contains(&w)
        {
            return Err(ParseError::InvalidWidth);
        }
        if let Some(h) = self.height
            && !(1..=8192).contains(&h)
        {
            return Err(ParseError::InvalidHeight);
        }
        if !(1..=100).contains(&self.quality) {
            return Err(ParseError::InvalidQuality);
        }
        if !(1.0..=5.0).contains(&self.dpr) {
            return Err(ParseError::InvalidDpr);
        }
        if let Some(v) = self.sharpen
            && !(0.0..=10.0).contains(&v)
        {
            return Err(ParseError::InvalidSharpen);
        }
        if let Some(v) = self.blur
            && !(0.1..=250.0).contains(&v)
        {
            return Err(ParseError::InvalidBlur);
        }
        if let Some(v) = self.brightness
            && !(0.0..=2.0).contains(&v)
        {
            return Err(ParseError::InvalidBrightness);
        }
        if let Some(v) = self.contrast
            && !(0.0..=2.0).contains(&v)
        {
            return Err(ParseError::InvalidContrast);
        }
        if let Some(v) = self.saturation
            && !(0.0..=2.0).contains(&v)
        {
            return Err(ParseError::InvalidSaturation);
        }
        if let Some(v) = self.gamma
            && !(0.1..=10.0).contains(&v)
        {
            return Err(ParseError::InvalidGamma);
        }
        if let Some(v) = self.trim
            && !(1.0..=100.0).contains(&v)
        {
            return Err(ParseError::InvalidTrim);
        }
        if let Some(f) = self.frame
            && f > 999
        {
            return Err(ParseError::InvalidFrame);
        }
        for v in [
            self.trim_top,
            self.trim_right,
            self.trim_bottom,
            self.trim_left,
        ]
        .into_iter()
        .flatten()
        {
            if v < 0.0 {
                return Err(ParseError::InvalidTrim);
            }
        }
        if let Some(v) = self.scq
            && !(1..=100).contains(&v)
        {
            return Err(ParseError::InvalidQuality);
        }
        // Conservative pixel bound, not a Cloudflare-published limit --
        // Cloudflare's own border docs don't state a numeric range beyond
        // "in pixels." Chosen to keep the padded canvas well within the
        // existing 8192 FFI-safety ceiling even when combined with a
        // large `w`/`h`.
        for v in [
            self.border_width,
            self.border_top,
            self.border_right,
            self.border_bottom,
            self.border_left,
        ]
        .into_iter()
        .flatten()
        {
            if v > 2000 {
                return Err(ParseError::InvalidBorder);
            }
        }
        for entry in &self.draw {
            if entry.url.as_deref().is_none_or(str::is_empty) {
                return Err(ParseError::InvalidDraw);
            }
            if entry.top.is_some() && entry.bottom.is_some() {
                return Err(ParseError::InvalidDraw);
            }
            if entry.left.is_some() && entry.right.is_some() {
                return Err(ParseError::InvalidDraw);
            }
            if let Some(o) = entry.opacity
                && !(0.0..=1.0).contains(&o)
            {
                return Err(ParseError::InvalidDraw);
            }
            for v in [
                entry.width,
                entry.height,
                entry.top,
                entry.left,
                entry.bottom,
                entry.right,
            ]
            .into_iter()
            .flatten()
            {
                if v < 0.0 {
                    return Err(ParseError::InvalidDraw);
                }
            }
        }
        Ok(())
    }

    /// Apply the Cloudflare `slow-connection-quality`/`scq` override (gap
    /// 8): when `is_slow` (computed by the caller from client-hint
    /// headers via `is_slow_connection`) and `scq` was set, `quality` is
    /// overwritten with the `scq` value. Must be called before
    /// `to_cache_key`/`transform` so the effective quality -- not the
    /// unconditional one -- is what actually gets cached and encoded.
    pub fn apply_scq_override(&mut self, is_slow: bool) {
        if is_slow && let Some(v) = self.scq {
            self.quality = v;
        }
    }

    /// Serialize into a deterministic cache key string. Fields are written
    /// in a fixed canonical order, independent of parse order; optional
    /// fields at their default/unset value are omitted entirely. Must stay
    /// byte-for-byte identical to the Zig implementation — see
    /// docs/INVARIANTS.md INV-1 (existing R2-cached variants depend on it).
    pub fn to_cache_key(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        if let Some(w) = self.width {
            parts.push(format!("w={w}"));
        }
        if let Some(h) = self.height {
            parts.push(format!("h={h}"));
        }
        parts.push(format!("q={}", self.quality));
        if let Some(f) = self.format {
            parts.push(format!("f={}", f.as_str()));
        }
        parts.push(format!("fit={}", self.fit.as_str()));
        parts.push(format!("g={}", self.gravity.as_str()));
        if let Some(v) = self.sharpen {
            parts.push(format!("sharpen={v:.2}"));
        }
        if let Some(v) = self.blur {
            parts.push(format!("blur={v:.2}"));
        }
        parts.push(format!("dpr={:.1}", self.dpr));
        if let Some(r) = self.rotate {
            parts.push(format!("rotate={}", r.as_str()));
        }
        if let Some(fl) = self.flip {
            parts.push(format!("flip={}", fl.as_str()));
        }
        if let Some(v) = self.brightness {
            parts.push(format!("brightness={v:.2}"));
        }
        if let Some(v) = self.contrast {
            parts.push(format!("contrast={v:.2}"));
        }
        if let Some(v) = self.saturation {
            parts.push(format!("saturation={v:.2}"));
        }
        if let Some(v) = self.gamma {
            parts.push(format!("gamma={v:.2}"));
        }
        if let Some(bg) = self.background {
            parts.push(format!("bg={:02X}{:02X}{:02X}", bg[0], bg[1], bg[2]));
        }
        if self.metadata != MetadataMode::default() {
            parts.push(format!("metadata={}", self.metadata.as_str()));
        }
        if let Some(v) = self.trim {
            parts.push(format!("trim={v:.1}"));
        }
        if self.anim != AnimMode::default() {
            parts.push(format!("anim={}", self.anim.as_str()));
        }
        if let Some(f) = self.frame {
            parts.push(format!("frame={f}"));
        }
        if let Some(v) = self.trim_top {
            parts.push(format!("trim.top={v:.1}"));
        }
        if let Some(v) = self.trim_right {
            parts.push(format!("trim.right={v:.1}"));
        }
        if let Some(v) = self.trim_bottom {
            parts.push(format!("trim.bottom={v:.1}"));
        }
        if let Some(v) = self.trim_left {
            parts.push(format!("trim.left={v:.1}"));
        }
        // onerror is deliberately omitted -- see the field doc comment
        // and cache_key_omits_onerror_since_it_does_not_affect_output_bytes.
        if let Some(c) = self.compression {
            parts.push(format!("compression={}", c.as_str()));
        }
        // scq is deliberately omitted -- see the field doc comment and
        // cache_key_omits_scq_since_the_override_already_shows_up_in_quality.
        if let Some(v) = self.border_width {
            parts.push(format!("border={v}"));
        }
        if let Some(bg) = self.border_color {
            parts.push(format!(
                "border.color={:02X}{:02X}{:02X}",
                bg[0], bg[1], bg[2]
            ));
        }
        if let Some(v) = self.border_top {
            parts.push(format!("border.top={v}"));
        }
        if let Some(v) = self.border_right {
            parts.push(format!("border.right={v}"));
        }
        if let Some(v) = self.border_bottom {
            parts.push(format!("border.bottom={v}"));
        }
        if let Some(v) = self.border_left {
            parts.push(format!("border.left={v}"));
        }
        for (i, d) in self.draw.iter().enumerate() {
            if let Some(url) = &d.url {
                parts.push(format!("draw.{i}.url={url}"));
            }
            if let Some(v) = d.width {
                parts.push(format!("draw.{i}.width={v:.2}"));
            }
            if let Some(v) = d.height {
                parts.push(format!("draw.{i}.height={v:.2}"));
            }
            if let Some(v) = d.top {
                parts.push(format!("draw.{i}.top={v:.2}"));
            }
            if let Some(v) = d.left {
                parts.push(format!("draw.{i}.left={v:.2}"));
            }
            if let Some(v) = d.bottom {
                parts.push(format!("draw.{i}.bottom={v:.2}"));
            }
            if let Some(v) = d.right {
                parts.push(format!("draw.{i}.right={v:.2}"));
            }
            if let Some(v) = d.opacity {
                parts.push(format!("draw.{i}.opacity={v:.2}"));
            }
            if let Some(r) = d.repeat {
                parts.push(format!("draw.{i}.repeat={}", r.as_str()));
            }
            if let Some(bg) = d.background {
                parts.push(format!(
                    "draw.{i}.background={:02X}{:02X}{:02X}",
                    bg[0], bg[1], bg[2]
                ));
            }
            if let Some(r) = d.rotate {
                parts.push(format!("draw.{i}.rotate={}", r.as_str()));
            }
            if let Some(f) = d.fit {
                parts.push(format!("draw.{i}.fit={}", f.as_str()));
            }
            if let Some(g) = d.gravity {
                parts.push(format!("draw.{i}.gravity={}", g.as_str()));
            }
        }

        parts.join(",")
    }
}

/// Parse a comma-separated key=value string into TransformParams. An
/// empty string returns default parameters.
pub fn parse(input: &str) -> Result<TransformParams, ParseError> {
    let mut params = TransformParams::default();

    if input.is_empty() {
        return Ok(params);
    }

    for pair in input.split(',') {
        if pair.is_empty() {
            continue;
        }

        let eq_pos = pair.find('=').ok_or(ParseError::InvalidParameter)?;
        let key = &pair[..eq_pos];
        let value = &pair[eq_pos + 1..];

        if value.is_empty() {
            return Err(ParseError::EmptyValue);
        }

        if let Some(rest) = key.strip_prefix("draw.") {
            parse_draw_field(&mut params.draw, rest, value)?;
            continue;
        }

        match key {
            "w" | "width" => params.width = Some(parse_u32(value).ok_or(ParseError::InvalidWidth)?),
            "h" | "height" => {
                params.height = Some(parse_u32(value).ok_or(ParseError::InvalidHeight)?)
            }
            "q" | "quality" => {
                params.quality = parse_quality(value)?;
            }
            "format" | "fmt" | "f" => params.format = Some(OutputFormat::parse_str(value)?),
            "fit" => params.fit = FitMode::parse_str(value)?,
            "gravity" | "g" => params.gravity = Gravity::parse_str(value)?,
            "sharpen" => params.sharpen = Some(parse_f32(value).ok_or(ParseError::InvalidSharpen)?),
            "blur" => params.blur = Some(parse_f32(value).ok_or(ParseError::InvalidBlur)?),
            "dpr" => params.dpr = parse_f32(value).ok_or(ParseError::InvalidDpr)?,
            "rotate" => params.rotate = Some(Rotation::parse_str(value)?),
            "flip" => params.flip = Some(FlipMode::parse_str(value)?),
            "brightness" => {
                params.brightness = Some(parse_f32(value).ok_or(ParseError::InvalidBrightness)?)
            }
            "contrast" => {
                params.contrast = Some(parse_f32(value).ok_or(ParseError::InvalidContrast)?)
            }
            "saturation" => {
                params.saturation = Some(parse_f32(value).ok_or(ParseError::InvalidSaturation)?)
            }
            "gamma" => params.gamma = Some(parse_f32(value).ok_or(ParseError::InvalidGamma)?),
            "bg" | "background" => {
                params.background =
                    Some(parse_hex_color(value).ok_or(ParseError::InvalidBackground)?)
            }
            "metadata" => params.metadata = MetadataMode::parse_str(value)?,
            "trim" => params.trim = Some(parse_f32(value).ok_or(ParseError::InvalidTrim)?),
            "anim" => params.anim = AnimMode::parse_str(value)?,
            "frame" => params.frame = Some(parse_u32(value).ok_or(ParseError::InvalidFrame)?),
            "trim.top" => params.trim_top = Some(parse_f32(value).ok_or(ParseError::InvalidTrim)?),
            "trim.right" => {
                params.trim_right = Some(parse_f32(value).ok_or(ParseError::InvalidTrim)?)
            }
            "trim.bottom" => {
                params.trim_bottom = Some(parse_f32(value).ok_or(ParseError::InvalidTrim)?)
            }
            "trim.left" => {
                params.trim_left = Some(parse_f32(value).ok_or(ParseError::InvalidTrim)?)
            }
            "onerror" => params.onerror = Some(OnErrorMode::parse_str(value)?),
            "compression" => params.compression = Some(CompressionMode::parse_str(value)?),
            "scq" | "slow-connection-quality" => {
                params.scq = Some(parse_quality(value)?);
            }
            "border" => {
                params.border_width = Some(parse_u32(value).ok_or(ParseError::InvalidBorder)?)
            }
            "border.color" => {
                params.border_color = Some(parse_hex_color(value).ok_or(ParseError::InvalidBorder)?)
            }
            "border.top" => {
                params.border_top = Some(parse_u32(value).ok_or(ParseError::InvalidBorder)?)
            }
            "border.right" => {
                params.border_right = Some(parse_u32(value).ok_or(ParseError::InvalidBorder)?)
            }
            "border.bottom" => {
                params.border_bottom = Some(parse_u32(value).ok_or(ParseError::InvalidBorder)?)
            }
            "border.left" => {
                params.border_left = Some(parse_u32(value).ok_or(ParseError::InvalidBorder)?)
            }
            _ => return Err(ParseError::InvalidParameter),
        }
    }

    Ok(params)
}

fn parse_u32(s: &str) -> Option<u32> {
    s.parse::<u32>().ok()
}

/// Parse a `quality`/`q` value: either a numeric 1-100 (imgx's original
/// behavior) or one of Cloudflare's perceptual quality strings (`high`,
/// `medium-high`, `medium-low`, `low` -- see
/// developers.cloudflare.com/images, `quality` section). Cloudflare
/// doesn't publish exact integer values for the perceptual tiers, so this
/// mapping is spec-derived -- documented as such in
/// docs/CLOUDFLARE_PARITY.md. Does not change imgx's own default quality
/// (80).
fn parse_quality(s: &str) -> Result<u8, ParseError> {
    match s {
        "high" => Ok(90),
        "medium-high" => Ok(80),
        "medium-low" => Ok(60),
        "low" => Ok(40),
        _ => {
            let q = parse_u32(s).ok_or(ParseError::InvalidQuality)?;
            if q > 255 {
                return Err(ParseError::InvalidQuality);
            }
            Ok(q as u8)
        }
    }
}

fn parse_f32(s: &str) -> Option<f32> {
    let val = s.parse::<f32>().ok()?;
    if val.is_nan() || val.is_infinite() {
        None
    } else {
        Some(val)
    }
}

/// Parse one `draw.<N>.<field>` key/value pair into `draw[N]`, growing the
/// vec with default entries as needed. Capped at 64 entries -- a sane
/// bound against a pathological URL trying to grow an unbounded `Vec`.
fn parse_draw_field(
    draw: &mut Vec<DrawOverlay>,
    rest: &str,
    value: &str,
) -> Result<(), ParseError> {
    let mut parts = rest.splitn(2, '.');
    let idx_str = parts.next().ok_or(ParseError::InvalidParameter)?;
    let field = parts.next().ok_or(ParseError::InvalidParameter)?;
    let idx: usize = idx_str.parse().map_err(|_| ParseError::InvalidParameter)?;
    if idx >= 64 {
        return Err(ParseError::InvalidDraw);
    }
    while draw.len() <= idx {
        draw.push(DrawOverlay::default());
    }
    let entry = &mut draw[idx];
    match field {
        "url" => entry.url = Some(value.to_string()),
        "width" => entry.width = Some(parse_f32(value).ok_or(ParseError::InvalidDraw)?),
        "height" => entry.height = Some(parse_f32(value).ok_or(ParseError::InvalidDraw)?),
        "top" => entry.top = Some(parse_f32(value).ok_or(ParseError::InvalidDraw)?),
        "left" => entry.left = Some(parse_f32(value).ok_or(ParseError::InvalidDraw)?),
        "bottom" => entry.bottom = Some(parse_f32(value).ok_or(ParseError::InvalidDraw)?),
        "right" => entry.right = Some(parse_f32(value).ok_or(ParseError::InvalidDraw)?),
        "opacity" => entry.opacity = Some(parse_f32(value).ok_or(ParseError::InvalidDraw)?),
        "repeat" => entry.repeat = Some(DrawRepeat::parse_str(value)?),
        "background" => {
            entry.background = Some(parse_hex_color(value).ok_or(ParseError::InvalidDraw)?)
        }
        "rotate" => entry.rotate = Some(Rotation::parse_str(value)?),
        "fit" => entry.fit = Some(FitMode::parse_str(value)?),
        "gravity" => entry.gravity = Some(Gravity::parse_str(value)?),
        _ => return Err(ParseError::InvalidParameter),
    }
    Ok(())
}

/// Cloudflare's `slow-connection-quality`/`scq` (gap 8, verified against
/// developers.cloudflare.com/images/optimization/features/): "applies when
/// the [rtt/save-data/ect/downlink] client hint is present and any of
/// [rtt > 150ms, save-data == \"on\", ect in {slow-2g,2g,3g}, downlink <
/// 5Mbps]" is met. Pure and independent of the HTTP layer so it's directly
/// unit-testable; `server.rs` is the only caller, supplying the raw header
/// values it read off the request.
pub fn is_slow_connection(
    rtt: Option<&str>,
    save_data: Option<&str>,
    ect: Option<&str>,
    downlink: Option<&str>,
) -> bool {
    let rtt_slow = rtt
        .and_then(|v| v.trim().parse::<f32>().ok())
        .is_some_and(|v| v > 150.0);
    let save_data_slow = save_data.is_some_and(|v| v.trim().eq_ignore_ascii_case("on"));
    let ect_slow = ect.is_some_and(|v| matches!(v.trim(), "slow-2g" | "2g" | "3g"));
    let downlink_slow = downlink
        .and_then(|v| v.trim().parse::<f32>().ok())
        .is_some_and(|v| v < 5.0);
    rtt_slow || save_data_slow || ect_slow || downlink_slow
}

fn parse_hex_color(s: &str) -> Option<[u8; 3]> {
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_content_type() {
        assert_eq!(OutputFormat::Jpeg.content_type(), "image/jpeg");
        assert_eq!(OutputFormat::Png.content_type(), "image/png");
        assert_eq!(OutputFormat::Webp.content_type(), "image/webp");
        assert_eq!(OutputFormat::Avif.content_type(), "image/avif");
    }

    #[test]
    fn output_format_extension() {
        assert_eq!(OutputFormat::Jpeg.extension(), "jpg");
        assert_eq!(OutputFormat::Webp.extension(), "webp");
    }

    #[test]
    fn parse_empty_string_returns_default_params() {
        let params = parse("").unwrap();
        assert_eq!(params.width, None);
        assert_eq!(params.height, None);
        assert_eq!(params.quality, 80);
        assert_eq!(params.format, None);
        assert_eq!(params.fit, FitMode::Contain);
        assert_eq!(params.gravity, Gravity::Center);
        assert_eq!(params.sharpen, None);
        assert_eq!(params.blur, None);
        assert_eq!(params.dpr, 1.0);
    }

    #[test]
    fn parse_width_only() {
        let params = parse("w=400").unwrap();
        assert_eq!(params.width, Some(400));
        assert_eq!(params.height, None);
        assert_eq!(params.quality, 80);
    }

    #[test]
    fn parse_multiple_params() {
        let params = parse("w=400,h=300,format=webp,q=85").unwrap();
        assert_eq!(params.width, Some(400));
        assert_eq!(params.height, Some(300));
        assert_eq!(params.format, Some(OutputFormat::Webp));
        assert_eq!(params.quality, 85);
    }

    #[test]
    fn parse_all_fit_modes() {
        let modes = [
            ("contain", FitMode::Contain),
            ("cover", FitMode::Cover),
            ("fill", FitMode::Fill),
            ("inside", FitMode::Inside),
            ("outside", FitMode::Outside),
            ("pad", FitMode::Pad),
        ];
        for (s, expected) in modes {
            let params = parse(&format!("fit={s}")).unwrap();
            assert_eq!(params.fit, expected);
        }
    }

    #[test]
    fn parse_all_gravity_values() {
        let gravities = [
            ("center", Gravity::Center),
            ("north", Gravity::North),
            ("south", Gravity::South),
            ("east", Gravity::East),
            ("west", Gravity::West),
            ("northeast", Gravity::Northeast),
            ("northwest", Gravity::Northwest),
            ("southeast", Gravity::Southeast),
            ("southwest", Gravity::Southwest),
            ("smart", Gravity::Smart),
            ("attention", Gravity::Attention),
        ];
        for (s, expected) in gravities {
            let params = parse(&format!("g={s}")).unwrap();
            assert_eq!(params.gravity, expected);
        }
    }

    #[test]
    fn parse_format_aliases() {
        assert_eq!(parse("format=png").unwrap().format, Some(OutputFormat::Png));
        assert_eq!(parse("fmt=jpeg").unwrap().format, Some(OutputFormat::Jpeg));
        assert_eq!(parse("f=avif").unwrap().format, Some(OutputFormat::Avif));
    }

    #[test]
    fn validate_width_0_returns_error() {
        let mut p = TransformParams {
            width: Some(0),
            ..Default::default()
        };
        assert_eq!(p.validate(), Err(ParseError::InvalidWidth));
        p.width = Some(1);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_width_9000_returns_error() {
        let p = TransformParams {
            width: Some(9000),
            ..Default::default()
        };
        assert_eq!(p.validate(), Err(ParseError::InvalidWidth));
    }

    #[test]
    fn validate_quality_0_returns_error() {
        let p = TransformParams {
            quality: 0,
            ..Default::default()
        };
        assert_eq!(p.validate(), Err(ParseError::InvalidQuality));
    }

    #[test]
    fn validate_quality_101_returns_error() {
        let p = TransformParams {
            quality: 101,
            ..Default::default()
        };
        assert_eq!(p.validate(), Err(ParseError::InvalidQuality));
    }

    #[test]
    fn invalid_key_returns_error() {
        assert_eq!(parse("banana=42"), Err(ParseError::InvalidParameter));
    }

    #[test]
    fn invalid_value_non_numeric_width_returns_error() {
        assert_eq!(parse("w=abc"), Err(ParseError::InvalidWidth));
    }

    #[test]
    fn cache_key_is_deterministic() {
        let params = parse("w=400,h=300,format=webp,q=85").unwrap();
        assert_eq!(params.to_cache_key(), params.to_cache_key());
    }

    #[test]
    fn cache_key_differs_when_params_differ() {
        let p1 = parse("w=400,h=300").unwrap();
        let p2 = parse("w=400,h=301").unwrap();
        assert_ne!(p1.to_cache_key(), p2.to_cache_key());
    }

    #[test]
    fn dpr_multiplies_effective_dimensions() {
        let params = parse("w=400,h=300,dpr=2.0").unwrap();
        assert_eq!(params.effective_width(), Some(800));
        assert_eq!(params.effective_height(), Some(600));
    }

    #[test]
    fn dpr_effective_dimensions_with_null_width_and_height() {
        let params = parse("dpr=3.0").unwrap();
        assert_eq!(params.effective_width(), None);
        assert_eq!(params.effective_height(), None);
    }

    #[test]
    fn sharpen_bounds_validation() {
        assert!(
            TransformParams {
                sharpen: Some(5.0),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert_eq!(
            TransformParams {
                sharpen: Some(11.0),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidSharpen)
        );
        assert_eq!(
            TransformParams {
                sharpen: Some(-1.0),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidSharpen)
        );
    }

    #[test]
    fn blur_bounds_validation() {
        assert!(
            TransformParams {
                blur: Some(1.0),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert_eq!(
            TransformParams {
                blur: Some(300.0),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidBlur)
        );
        assert_eq!(
            TransformParams {
                blur: Some(0.05),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidBlur)
        );
    }

    #[test]
    fn dpr_bounds_validation() {
        assert!(
            TransformParams {
                dpr: 2.5,
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert_eq!(
            TransformParams {
                dpr: 0.5,
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidDpr)
        );
        assert_eq!(
            TransformParams {
                dpr: 6.0,
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidDpr)
        );
    }

    #[test]
    fn empty_value_returns_error() {
        assert_eq!(parse("w="), Err(ParseError::EmptyValue));
    }

    #[test]
    fn parse_sharpen_and_blur_values() {
        let params = parse("sharpen=1.5,blur=3.0").unwrap();
        assert_eq!(params.sharpen, Some(1.5));
        assert_eq!(params.blur, Some(3.0));
    }

    #[test]
    fn parse_rejects_nan_values() {
        assert_eq!(parse("sharpen=nan"), Err(ParseError::InvalidSharpen));
        assert_eq!(parse("blur=nan"), Err(ParseError::InvalidBlur));
        assert_eq!(parse("dpr=nan"), Err(ParseError::InvalidDpr));
    }

    #[test]
    fn parse_rejects_inf_values() {
        assert_eq!(parse("sharpen=inf"), Err(ParseError::InvalidSharpen));
        assert_eq!(parse("blur=inf"), Err(ParseError::InvalidBlur));
        assert_eq!(parse("gamma=inf"), Err(ParseError::InvalidGamma));
    }

    #[test]
    fn cache_key_order_is_canonical_regardless_of_parse_order() {
        let p1 = parse("h=300,w=400,q=90").unwrap();
        let p2 = parse("w=400,q=90,h=300").unwrap();
        assert_eq!(p1.to_cache_key(), p2.to_cache_key());
    }

    #[test]
    fn valid_params_pass_validation() {
        let params = parse("w=800,h=600,q=90,format=webp,fit=cover,g=north,dpr=2.0").unwrap();
        assert!(params.validate().is_ok());
    }

    #[test]
    fn width_boundary_values() {
        assert!(
            TransformParams {
                width: Some(1),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert!(
            TransformParams {
                width: Some(8192),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert_eq!(
            TransformParams {
                width: Some(8193),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidWidth)
        );
    }

    #[test]
    fn height_validation_at_boundaries() {
        assert_eq!(
            TransformParams {
                height: Some(0),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidHeight)
        );
        assert_eq!(
            TransformParams {
                height: Some(8193),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidHeight)
        );
    }

    #[test]
    fn invalid_format_returns_error() {
        assert_eq!(parse("format=bmp"), Err(ParseError::InvalidFormat));
    }

    #[test]
    fn invalid_fit_mode_returns_error() {
        assert_eq!(parse("fit=stretch"), Err(ParseError::InvalidFitMode));
    }

    #[test]
    fn invalid_gravity_returns_error() {
        assert_eq!(parse("g=diagonal"), Err(ParseError::InvalidGravity));
    }

    #[test]
    fn parse_rotate_values() {
        let rotations = [
            ("0", Rotation::Deg0),
            ("90", Rotation::Deg90),
            ("180", Rotation::Deg180),
            ("270", Rotation::Deg270),
        ];
        for (s, expected) in rotations {
            let params = parse(&format!("rotate={s}")).unwrap();
            assert_eq!(params.rotate, Some(expected));
        }
    }

    #[test]
    fn invalid_rotate_returns_error() {
        assert_eq!(parse("rotate=45"), Err(ParseError::InvalidRotation));
    }

    #[test]
    fn parse_flip_values() {
        assert_eq!(parse("flip=h").unwrap().flip, Some(FlipMode::H));
        assert_eq!(parse("flip=v").unwrap().flip, Some(FlipMode::V));
        assert_eq!(parse("flip=hv").unwrap().flip, Some(FlipMode::Hv));
    }

    #[test]
    fn invalid_flip_returns_error() {
        assert_eq!(parse("flip=x"), Err(ParseError::InvalidFlip));
    }

    #[test]
    fn parse_brightness_contrast_gamma() {
        let params = parse("brightness=1.5,contrast=0.8,gamma=2.2").unwrap();
        assert_eq!(params.brightness, Some(1.5));
        assert_eq!(params.contrast, Some(0.8));
        assert_eq!(params.gamma, Some(2.2));
    }

    #[test]
    fn brightness_bounds_validation() {
        assert!(
            TransformParams {
                brightness: Some(1.0),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert_eq!(
            TransformParams {
                brightness: Some(2.1),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidBrightness)
        );
    }

    #[test]
    fn contrast_bounds_validation() {
        assert!(
            TransformParams {
                contrast: Some(0.0),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert_eq!(
            TransformParams {
                contrast: Some(2.1),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidContrast)
        );
    }

    #[test]
    fn saturation_bounds_validation() {
        assert!(
            TransformParams {
                saturation: Some(1.5),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert_eq!(
            TransformParams {
                saturation: Some(2.1),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidSaturation)
        );
    }

    #[test]
    fn gamma_bounds_validation() {
        assert!(
            TransformParams {
                gamma: Some(2.2),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert_eq!(
            TransformParams {
                gamma: Some(0.05),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidGamma)
        );
        assert_eq!(
            TransformParams {
                gamma: Some(11.0),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidGamma)
        );
    }

    #[test]
    fn parse_background_hex_color() {
        let params = parse("bg=FF0000").unwrap();
        assert_eq!(params.background, Some([255, 0, 0]));
    }

    #[test]
    fn parse_background_alias() {
        let params = parse("background=00FF00").unwrap();
        assert_eq!(params.background, Some([0, 255, 0]));
    }

    #[test]
    fn invalid_background_returns_error() {
        assert_eq!(parse("bg=red"), Err(ParseError::InvalidBackground));
        assert_eq!(parse("bg=GGHHII"), Err(ParseError::InvalidBackground));
        assert_eq!(parse("bg=FFF"), Err(ParseError::InvalidBackground));
    }

    #[test]
    fn parse_metadata_modes() {
        assert_eq!(
            parse("metadata=strip").unwrap().metadata,
            MetadataMode::Strip
        );
        assert_eq!(parse("metadata=keep").unwrap().metadata, MetadataMode::Keep);
        assert_eq!(
            parse("metadata=copyright").unwrap().metadata,
            MetadataMode::Copyright
        );
    }

    #[test]
    fn invalid_metadata_returns_error() {
        assert_eq!(parse("metadata=partial"), Err(ParseError::InvalidMetadata));
    }

    #[test]
    fn parse_trim_value() {
        assert_eq!(parse("trim=10").unwrap().trim, Some(10.0));
    }

    #[test]
    fn trim_bounds_validation() {
        assert!(
            TransformParams {
                trim: Some(50.0),
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert_eq!(
            TransformParams {
                trim: Some(0.5),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidTrim)
        );
        assert_eq!(
            TransformParams {
                trim: Some(101.0),
                ..Default::default()
            }
            .validate(),
            Err(ParseError::InvalidTrim)
        );
    }

    #[test]
    fn cache_key_includes_new_params() {
        let params = parse("w=400,rotate=90,flip=h,brightness=1.5,bg=FF0000").unwrap();
        let key = params.to_cache_key();
        assert!(key.contains("rotate=90"));
        assert!(key.contains("flip=h"));
        assert!(key.contains("brightness=1.50"));
        assert!(key.contains("bg=FF0000"));
    }

    #[test]
    fn cache_key_omits_default_metadata() {
        let params = parse("w=400").unwrap();
        assert!(!params.to_cache_key().contains("metadata"));
    }

    #[test]
    fn cache_key_includes_non_default_metadata() {
        let params = parse("w=400,metadata=keep").unwrap();
        assert!(params.to_cache_key().contains("metadata=keep"));
    }

    #[test]
    fn parse_anim_modes() {
        assert_eq!(parse("anim=auto").unwrap().anim, AnimMode::Auto);
        assert_eq!(parse("anim=static").unwrap().anim, AnimMode::Static);
        assert_eq!(parse("anim=animate").unwrap().anim, AnimMode::Animate);
    }

    #[test]
    fn parse_anim_cloudflare_aliases() {
        assert_eq!(parse("anim=true").unwrap().anim, AnimMode::Auto);
        assert_eq!(parse("anim=false").unwrap().anim, AnimMode::Static);
    }

    #[test]
    fn invalid_anim_returns_error() {
        assert_eq!(parse("anim=fast"), Err(ParseError::InvalidAnim));
    }

    #[test]
    fn parse_frame_value() {
        assert_eq!(parse("frame=2").unwrap().frame, Some(2));
    }

    #[test]
    fn parse_frame_0() {
        assert_eq!(parse("frame=0").unwrap().frame, Some(0));
    }

    #[test]
    fn invalid_frame_returns_error() {
        assert_eq!(parse("frame=abc"), Err(ParseError::InvalidFrame));
    }

    #[test]
    fn frame_validation_rejects_large_values() {
        let p = TransformParams {
            frame: Some(1000),
            ..Default::default()
        };
        assert_eq!(p.validate(), Err(ParseError::InvalidFrame));
    }

    #[test]
    fn frame_validation_accepts_valid_values() {
        let p = TransformParams {
            frame: Some(999),
            ..Default::default()
        };
        assert!(p.validate().is_ok());
    }

    #[test]
    fn cache_key_includes_anim_when_not_default() {
        let params = parse("w=400,anim=static").unwrap();
        assert!(params.to_cache_key().contains("anim=static"));
    }

    #[test]
    fn cache_key_omits_anim_when_auto() {
        let params = parse("w=400").unwrap();
        assert!(!params.to_cache_key().contains("anim"));
    }

    #[test]
    fn cache_key_includes_frame_when_set() {
        let params = parse("w=400,frame=1").unwrap();
        assert!(params.to_cache_key().contains("frame=1"));
    }

    #[test]
    fn output_format_gif_content_type() {
        assert_eq!(OutputFormat::Gif.content_type(), "image/gif");
    }

    #[test]
    fn output_format_gif_extension() {
        assert_eq!(OutputFormat::Gif.extension(), "gif");
    }

    #[test]
    fn output_format_gif_parse_str() {
        assert_eq!(OutputFormat::parse_str("gif"), Ok(OutputFormat::Gif));
    }

    #[test]
    fn output_format_supports_animation() {
        assert!(OutputFormat::Gif.supports_animation());
        assert!(OutputFormat::Webp.supports_animation());
        assert!(!OutputFormat::Jpeg.supports_animation());
        assert!(!OutputFormat::Png.supports_animation());
        assert!(!OutputFormat::Avif.supports_animation());
    }

    // ------------------------------------------------------------------
    // Cloudflare parity gaps (docs/CLOUDFLARE_PARITY.md) -- TDD: these
    // tests were written before the corresponding parse/validate logic.
    // ------------------------------------------------------------------

    /// Gap 12 -- Cloudflare's real gravity model uses side names
    /// (`left`/`right`/`top`/`bottom`) rather than compass words, plus
    /// `auto` for saliency-based cropping (imgx's `smart`). Verified
    /// against developers.cloudflare.com/images/optimization/features/
    /// (`gravity` section) via the Cloudflare docs MCP search tool.
    #[test]
    fn parse_gravity_accepts_cloudflare_side_aliases() {
        assert_eq!(parse("g=left").unwrap().gravity, Gravity::West);
        assert_eq!(parse("g=right").unwrap().gravity, Gravity::East);
        assert_eq!(parse("g=top").unwrap().gravity, Gravity::North);
        assert_eq!(parse("g=bottom").unwrap().gravity, Gravity::South);
    }

    #[test]
    fn parse_gravity_accepts_cloudflare_auto_alias_for_smart() {
        assert_eq!(parse("gravity=auto").unwrap().gravity, Gravity::Smart);
    }

    /// Gap 3 -- Cloudflare's `squeeze` (distort to exact dims) is
    /// pixel-dimension equivalent to imgx's existing `fill`: both force
    /// the output to the exact requested width/height regardless of
    /// source aspect ratio. Proven by the shared pipeline test
    /// `transform_with_fit_squeeze_matches_fill_dimensions` in pipeline.rs;
    /// this test only proves the parser accepts the alias into the same
    /// enum variant.
    #[test]
    fn parse_fit_squeeze_aliases_to_fill() {
        assert_eq!(parse("fit=squeeze").unwrap().fit, FitMode::Fill);
    }

    /// Gap 3 -- Cloudflare's `scale-up` (never downscale, preserve aspect,
    /// upscale-only) is dimension-equivalent to imgx's existing `outside`
    /// (`VIPS_SIZE_UP`). Proven by pipeline test
    /// `transform_with_fit_scale_up_matches_outside_never_downscales`.
    #[test]
    fn parse_fit_scale_up_aliases_to_outside() {
        assert_eq!(parse("fit=scale-up").unwrap().fit, FitMode::Outside);
    }

    /// Gap 3 -- Cloudflare's `scale-down` (never upscale, preserve aspect)
    /// is dimension-equivalent to imgx's existing default `contain`
    /// (`VIPS_SIZE_DOWN`). Does NOT change imgx's default -- only adds the
    /// alias string.
    #[test]
    fn parse_fit_scale_down_aliases_to_contain() {
        assert_eq!(parse("fit=scale-down").unwrap().fit, FitMode::Contain);
    }

    /// Gap 3 -- Cloudflare's `crop` and `aspect-crop` have no dimension
    /// equivalent among imgx's existing fit modes (both add a
    /// never-upscale constraint layered on cropping semantics that no
    /// existing variant expresses) -- these get their own new enum
    /// variants with dedicated resize/crop math in pipeline.rs.
    #[test]
    fn parse_fit_crop_and_aspect_crop_are_new_variants() {
        assert_eq!(parse("fit=crop").unwrap().fit, FitMode::Crop);
        assert_eq!(parse("fit=aspect-crop").unwrap().fit, FitMode::AspectCrop);
    }

    #[test]
    fn fit_crop_and_aspect_crop_as_str_round_trip() {
        assert_eq!(FitMode::Crop.as_str(), "crop");
        assert_eq!(FitMode::AspectCrop.as_str(), "aspect-crop");
    }

    /// Gap 4 -- Cloudflare accepts perceptual quality strings in addition
    /// to 1-100. Verified against developers.cloudflare.com/images
    /// (`quality` section): "Perceptual quality — Accepts `high`,
    /// `medium-high`, `medium-low`, and `low`." No exact integer mapping
    /// is published, so the mapping below is spec-derived (documented as
    /// such in docs/CLOUDFLARE_PARITY.md) -- imgx's own default quality
    /// (80) is unchanged.
    #[test]
    fn parse_quality_perceptual_strings() {
        assert_eq!(parse("quality=high").unwrap().quality, 90);
        assert_eq!(parse("q=medium-high").unwrap().quality, 80);
        assert_eq!(parse("q=medium-low").unwrap().quality, 60);
        assert_eq!(parse("quality=low").unwrap().quality, 40);
    }

    #[test]
    fn parse_quality_numeric_still_works_alongside_perceptual_strings() {
        assert_eq!(parse("q=42").unwrap().quality, 42);
    }

    #[test]
    fn parse_quality_invalid_perceptual_string_returns_error() {
        assert_eq!(parse("q=ultra"), Err(ParseError::InvalidQuality));
    }

    /// Gap 5 -- `baseline-jpeg` (non-progressive JPEG). libvips'
    /// `vips_jpegsave_buffer` already defaults `interlace` to FALSE (i.e.
    /// baseline), so imgx's existing `format=jpeg` output is already
    /// baseline JPEG -- `baseline-jpeg` is an explicit alias for the same
    /// encode path, added as its own enum variant so it round-trips
    /// through the cache key distinctly from plain `jpeg`.
    #[test]
    fn parse_format_baseline_jpeg() {
        assert_eq!(
            parse("format=baseline-jpeg").unwrap().format,
            Some(OutputFormat::BaselineJpeg)
        );
    }

    #[test]
    fn baseline_jpeg_content_type_and_extension() {
        assert_eq!(OutputFormat::BaselineJpeg.content_type(), "image/jpeg");
        assert_eq!(OutputFormat::BaselineJpeg.extension(), "jpg");
        assert_eq!(OutputFormat::BaselineJpeg.as_str(), "baseline-jpeg");
    }

    /// Gap 5 -- `format=json`: metadata-only response, no image bytes.
    /// See docs/CLOUDFLARE_PARITY.md for the (spec-derived) JSON schema --
    /// Cloudflare's docs describe the content ("image size before and
    /// after resizing... source MIME type, and file size") but don't
    /// publish an exact field-by-field schema in the indexed docs.
    #[test]
    fn parse_format_json() {
        assert_eq!(
            parse("format=json").unwrap().format,
            Some(OutputFormat::Json)
        );
    }

    #[test]
    fn json_format_content_type_and_extension() {
        assert_eq!(OutputFormat::Json.content_type(), "application/json");
        assert_eq!(OutputFormat::Json.extension(), "json");
        assert!(!OutputFormat::Json.supports_animation());
    }

    /// Gap 7 -- `onerror=redirect`: opt-in per-request parameter. Default
    /// (`onerror` unset) preserves imgx's existing raw-bytes fallback on
    /// transform failure (INV-13) -- this only changes behavior when the
    /// caller explicitly asks for it.
    #[test]
    fn parse_onerror_redirect() {
        assert_eq!(
            parse("onerror=redirect").unwrap().onerror,
            Some(OnErrorMode::Redirect)
        );
    }

    #[test]
    fn onerror_defaults_to_none() {
        assert_eq!(parse("w=400").unwrap().onerror, None);
    }

    #[test]
    fn invalid_onerror_value_returns_error() {
        assert_eq!(parse("onerror=ignore"), Err(ParseError::InvalidOnError));
    }

    /// Gap 9 -- Cloudflare's per-side trim keys (`trim.top`, `trim.right`,
    /// `trim.bottom`, `trim.left`), verified against
    /// developers.cloudflare.com/images (`trim` section): pixel counts or
    /// a decimal 0.0-1.0 fraction of that side's dimension. The legacy
    /// numeric `trim=<threshold>` (border-uniformity threshold, a
    /// different semantic) keeps working unchanged alongside these --
    /// see OQ-5 in the PRD.
    #[test]
    fn parse_trim_per_side_keys() {
        let p = parse("trim.top=10,trim.right=0.2,trim.bottom=5,trim.left=0.1").unwrap();
        assert_eq!(p.trim_top, Some(10.0));
        assert_eq!(p.trim_right, Some(0.2));
        assert_eq!(p.trim_bottom, Some(5.0));
        assert_eq!(p.trim_left, Some(0.1));
    }

    #[test]
    fn legacy_numeric_trim_still_works_alongside_per_side_keys() {
        let p = parse("trim=25,trim.top=10").unwrap();
        assert_eq!(p.trim, Some(25.0));
        assert_eq!(p.trim_top, Some(10.0));
    }

    #[test]
    fn trim_per_side_defaults_to_none() {
        let p = parse("w=400").unwrap();
        assert_eq!(p.trim_top, None);
        assert_eq!(p.trim_right, None);
        assert_eq!(p.trim_bottom, None);
        assert_eq!(p.trim_left, None);
    }

    #[test]
    fn cache_key_includes_per_side_trim_when_set() {
        let params = parse("w=400,trim.top=10").unwrap();
        assert!(params.to_cache_key().contains("trim.top=10.0"));
    }

    /// `onerror` only changes failure-path behavior (server.rs), never
    /// the successful transform's output bytes -- see INV-13 (a failed
    /// transform is never cached under the success key in the first
    /// place). Deliberately excluded from the cache key so two otherwise-
    /// identical requests that differ only in `onerror` share one cache
    /// entry instead of needlessly duplicating it.
    #[test]
    fn cache_key_omits_onerror_since_it_does_not_affect_output_bytes() {
        let params = parse("w=400,onerror=redirect").unwrap();
        assert!(!params.to_cache_key().contains("onerror"));
    }

    // ------------------------------------------------------------------
    // Remaining Cloudflare parity gaps (docs/CLOUDFLARE_PARITY.md) -- TDD:
    // written before the corresponding parse/validate/cache-key logic.
    // ------------------------------------------------------------------

    /// Gap 6 -- `compression=fast`. Verified against
    /// developers.cloudflare.com/images/optimization/features/
    /// (`compression` section): "Selects the output format that is
    /// quickest to compress. Accepts `fast`."
    #[test]
    fn parse_compression_fast() {
        assert_eq!(
            parse("compression=fast").unwrap().compression,
            Some(CompressionMode::Fast)
        );
    }

    #[test]
    fn compression_defaults_to_none() {
        assert_eq!(parse("w=400").unwrap().compression, None);
    }

    #[test]
    fn invalid_compression_value_returns_error() {
        assert_eq!(
            parse("compression=slow"),
            Err(ParseError::InvalidCompression)
        );
    }

    #[test]
    fn cache_key_includes_compression_when_set() {
        let params = parse("w=400,compression=fast").unwrap();
        assert!(params.to_cache_key().contains("compression=fast"));
    }

    #[test]
    fn cache_key_omits_compression_when_unset() {
        let params = parse("w=400").unwrap();
        assert!(!params.to_cache_key().contains("compression"));
    }

    /// Gap 8 -- `slow-connection-quality`/`scq`. Verified against
    /// developers.cloudflare.com/images/optimization/features/
    /// (`slow-connection-quality`/`scq` section): accepts the same
    /// fixed/perceptual values as `quality`.
    #[test]
    fn parse_scq_numeric_value() {
        assert_eq!(parse("scq=40").unwrap().scq, Some(40));
    }

    #[test]
    fn parse_scq_alias_slow_connection_quality() {
        assert_eq!(parse("slow-connection-quality=50").unwrap().scq, Some(50));
    }

    #[test]
    fn parse_scq_perceptual_string() {
        assert_eq!(parse("scq=low").unwrap().scq, Some(40));
    }

    #[test]
    fn scq_defaults_to_none() {
        assert_eq!(parse("w=400").unwrap().scq, None);
    }

    #[test]
    fn scq_out_of_range_returns_error() {
        let p = TransformParams {
            scq: Some(101),
            ..Default::default()
        };
        assert_eq!(p.validate(), Err(ParseError::InvalidQuality));
    }

    #[test]
    fn cache_key_omits_scq_since_the_override_already_shows_up_in_quality() {
        let params = parse("w=400,scq=40").unwrap();
        assert!(!params.to_cache_key().contains("scq"));
    }

    /// Pure predicate: is the connection "slow" per Cloudflare's
    /// documented trigger conditions (rtt > 150ms, save-data == "on",
    /// ect in {slow-2g,2g,3g}, downlink < 5Mbps), independent of the HTTP
    /// header-parsing layer.
    #[test]
    fn is_slow_connection_no_hints_present_is_false() {
        assert!(!is_slow_connection(None, None, None, None));
    }

    #[test]
    fn is_slow_connection_rtt_over_150ms_is_true() {
        assert!(is_slow_connection(Some("200"), None, None, None));
        assert!(!is_slow_connection(Some("150"), None, None, None));
        assert!(!is_slow_connection(Some("100"), None, None, None));
    }

    #[test]
    fn is_slow_connection_save_data_on_is_true() {
        assert!(is_slow_connection(None, Some("on"), None, None));
        assert!(!is_slow_connection(None, Some("off"), None, None));
    }

    #[test]
    fn is_slow_connection_ect_slow_values_are_true() {
        assert!(is_slow_connection(None, None, Some("slow-2g"), None));
        assert!(is_slow_connection(None, None, Some("2g"), None));
        assert!(is_slow_connection(None, None, Some("3g"), None));
        assert!(!is_slow_connection(None, None, Some("4g"), None));
    }

    #[test]
    fn is_slow_connection_downlink_under_5mbps_is_true() {
        assert!(is_slow_connection(None, None, None, Some("2.5")));
        assert!(!is_slow_connection(None, None, None, Some("10")));
    }

    #[test]
    fn apply_scq_override_sets_quality_when_slow_and_scq_present() {
        let mut p = parse("q=80,scq=40").unwrap();
        p.apply_scq_override(true);
        assert_eq!(p.quality, 40);
    }

    #[test]
    fn apply_scq_override_leaves_quality_unchanged_when_not_slow() {
        let mut p = parse("q=80,scq=40").unwrap();
        p.apply_scq_override(false);
        assert_eq!(p.quality, 80);
    }

    #[test]
    fn apply_scq_override_leaves_quality_unchanged_when_no_scq_set() {
        let mut p = parse("q=80").unwrap();
        p.apply_scq_override(true);
        assert_eq!(p.quality, 80);
    }

    #[test]
    fn override_changes_the_cache_key_via_the_existing_quality_field() {
        let mut fast = parse("w=400,q=80,scq=40").unwrap();
        let mut slow = parse("w=400,q=80,scq=40").unwrap();
        fast.apply_scq_override(false);
        slow.apply_scq_override(true);
        assert_ne!(fast.to_cache_key(), slow.to_cache_key());
        assert!(slow.to_cache_key().contains("q=40"));
    }

    /// Gap 10 -- `border`. No published Cloudflare URL syntax (Cloudflare
    /// marks this feature "available only in Workers") -- this flat-key
    /// encoding (`border`, `border.color`, `border.top`/etc.) is
    /// spec-derived, reusing imgx's existing `bg`-style hex color parsing
    /// and `trim.top`-style dotted per-side convention.
    #[test]
    fn parse_border_uniform_width() {
        assert_eq!(parse("border=10").unwrap().border_width, Some(10));
    }

    #[test]
    fn parse_border_color() {
        assert_eq!(
            parse("border.color=FF0000").unwrap().border_color,
            Some([255, 0, 0])
        );
    }

    #[test]
    fn parse_border_per_side() {
        let p = parse("border.top=5,border.right=10,border.bottom=5,border.left=10").unwrap();
        assert_eq!(p.border_top, Some(5));
        assert_eq!(p.border_right, Some(10));
        assert_eq!(p.border_bottom, Some(5));
        assert_eq!(p.border_left, Some(10));
    }

    #[test]
    fn border_defaults_to_none() {
        let p = parse("w=400").unwrap();
        assert_eq!(p.border_width, None);
        assert_eq!(p.border_color, None);
    }

    #[test]
    fn invalid_border_value_returns_error() {
        assert_eq!(parse("border=abc"), Err(ParseError::InvalidBorder));
        assert_eq!(parse("border.color=xyz"), Err(ParseError::InvalidBorder));
    }

    #[test]
    fn border_width_over_bound_returns_error() {
        let p = TransformParams {
            border_width: Some(2001),
            ..Default::default()
        };
        assert_eq!(p.validate(), Err(ParseError::InvalidBorder));
    }

    #[test]
    fn cache_key_includes_border_when_set() {
        let params = parse("w=400,border=10,border.color=000000").unwrap();
        let key = params.to_cache_key();
        assert!(key.contains("border=10"));
        assert!(key.contains("border.color=000000"));
    }

    /// Gap 11 -- `draw` overlays. No published Cloudflare URL syntax
    /// either (also "available only in Workers") -- the `draw.<N>.<field>`
    /// flattened-array encoding below is spec-derived, designed to be
    /// consistent with imgx's existing dotted-key conventions rather than
    /// a verified byte-for-byte match of any real Cloudflare URL form.
    #[test]
    fn parse_draw_single_overlay() {
        let p =
            parse("draw.0.url=https://example.com/logo.png,draw.0.width=100,draw.0.opacity=0.5")
                .unwrap();
        assert_eq!(p.draw.len(), 1);
        assert_eq!(
            p.draw[0].url.as_deref(),
            Some("https://example.com/logo.png")
        );
        assert_eq!(p.draw[0].width, Some(100.0));
        assert_eq!(p.draw[0].opacity, Some(0.5));
    }

    #[test]
    fn parse_draw_multiple_overlays_by_index() {
        let p = parse("draw.0.url=https://example.com/a.png,draw.1.url=https://example.com/b.png")
            .unwrap();
        assert_eq!(p.draw.len(), 2);
        assert_eq!(p.draw[0].url.as_deref(), Some("https://example.com/a.png"));
        assert_eq!(p.draw[1].url.as_deref(), Some("https://example.com/b.png"));
    }

    #[test]
    fn parse_draw_positioning_and_repeat_and_rotate() {
        let p = parse(
            "draw.0.url=https://example.com/a.png,draw.0.bottom=5,draw.0.right=5,\
             draw.0.repeat=x,draw.0.rotate=90,draw.0.background=FFFFFF",
        )
        .unwrap();
        let d = &p.draw[0];
        assert_eq!(d.bottom, Some(5.0));
        assert_eq!(d.right, Some(5.0));
        assert_eq!(d.repeat, Some(DrawRepeat::X));
        assert_eq!(d.rotate, Some(Rotation::Deg90));
        assert_eq!(d.background, Some([255, 255, 255]));
    }

    #[test]
    fn draw_defaults_to_empty() {
        assert!(parse("w=400").unwrap().draw.is_empty());
    }

    #[test]
    fn draw_missing_url_returns_error_on_validate() {
        let p = parse("draw.0.width=100").unwrap();
        assert_eq!(p.validate(), Err(ParseError::InvalidDraw));
    }

    #[test]
    fn draw_both_top_and_bottom_returns_error_on_validate() {
        let p = parse("draw.0.url=https://example.com/a.png,draw.0.top=5,draw.0.bottom=5").unwrap();
        assert_eq!(p.validate(), Err(ParseError::InvalidDraw));
    }

    #[test]
    fn draw_both_left_and_right_returns_error_on_validate() {
        let p = parse("draw.0.url=https://example.com/a.png,draw.0.left=5,draw.0.right=5").unwrap();
        assert_eq!(p.validate(), Err(ParseError::InvalidDraw));
    }

    #[test]
    fn draw_opacity_out_of_range_returns_error_on_validate() {
        let p = parse("draw.0.url=https://example.com/a.png,draw.0.opacity=1.5").unwrap();
        assert_eq!(p.validate(), Err(ParseError::InvalidDraw));
    }

    #[test]
    fn draw_valid_entry_passes_validation() {
        let p = parse("draw.0.url=https://example.com/a.png,draw.0.width=0.25,draw.0.opacity=0.8")
            .unwrap();
        assert!(p.validate().is_ok());
    }

    #[test]
    fn cache_key_includes_draw_overlay_fields_when_set() {
        let p = parse("draw.0.url=https://example.com/a.png,draw.0.opacity=0.5").unwrap();
        let key = p.to_cache_key();
        assert!(key.contains("draw.0.url=https://example.com/a.png"));
        assert!(key.contains("draw.0.opacity=0.50"));
    }

    #[test]
    fn cache_key_omits_draw_when_empty() {
        let p = parse("w=400").unwrap();
        assert!(!p.to_cache_key().contains("draw"));
    }

    /// Golden literal cache-key strings, captured from the Zig
    /// implementation's exact output format, per docs/INVARIANTS.md INV-1.
    /// This is the byte-for-byte parity check, not just internal
    /// self-consistency (existing R2-cached variants depend on this).
    #[test]
    fn cache_key_matches_zig_golden_strings() {
        assert_eq!(
            parse("w=400,h=300,format=webp,q=85")
                .unwrap()
                .to_cache_key(),
            "w=400,h=300,q=85,f=webp,fit=contain,g=center,dpr=1.0"
        );
        assert_eq!(
            TransformParams::default().to_cache_key(),
            "q=80,fit=contain,g=center,dpr=1.0"
        );
        assert_eq!(
            parse("w=400,rotate=90,flip=h,brightness=1.5,bg=FF0000")
                .unwrap()
                .to_cache_key(),
            "w=400,q=80,fit=contain,g=center,dpr=1.0,rotate=90,flip=h,brightness=1.50,bg=FF0000"
        );
        assert_eq!(
            parse("w=400,h=400,fit=cover,g=smart,dpr=2")
                .unwrap()
                .to_cache_key(),
            "w=400,h=400,q=80,fit=cover,g=smart,dpr=2.0"
        );
    }
}
