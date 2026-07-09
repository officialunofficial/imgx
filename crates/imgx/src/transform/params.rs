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
}

impl OutputFormat {
    pub fn content_type(self) -> &'static str {
        match self {
            OutputFormat::Jpeg => "image/jpeg",
            OutputFormat::Png => "image/png",
            OutputFormat::Webp => "image/webp",
            OutputFormat::Avif => "image/avif",
            OutputFormat::Gif => "image/gif",
            OutputFormat::Auto => "application/octet-stream",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Jpeg => "jpg",
            OutputFormat::Png => "png",
            OutputFormat::Webp => "webp",
            OutputFormat::Avif => "avif",
            OutputFormat::Gif => "gif",
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
            "north" | "n" => Ok(Gravity::North),
            "south" | "s" => Ok(Gravity::South),
            "east" | "e" => Ok(Gravity::East),
            "west" | "w" => Ok(Gravity::West),
            "northeast" | "ne" => Ok(Gravity::Northeast),
            "northwest" | "nw" => Ok(Gravity::Northwest),
            "southeast" | "se" => Ok(Gravity::Southeast),
            "southwest" | "sw" => Ok(Gravity::Southwest),
            "smart" => Ok(Gravity::Smart),
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

#[derive(Debug, Clone, Copy, PartialEq)]
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
        if let Some(w) = self.width {
            if !(1..=8192).contains(&w) {
                return Err(ParseError::InvalidWidth);
            }
        }
        if let Some(h) = self.height {
            if !(1..=8192).contains(&h) {
                return Err(ParseError::InvalidHeight);
            }
        }
        if !(1..=100).contains(&self.quality) {
            return Err(ParseError::InvalidQuality);
        }
        if !(1.0..=5.0).contains(&self.dpr) {
            return Err(ParseError::InvalidDpr);
        }
        if let Some(v) = self.sharpen {
            if !(0.0..=10.0).contains(&v) {
                return Err(ParseError::InvalidSharpen);
            }
        }
        if let Some(v) = self.blur {
            if !(0.1..=250.0).contains(&v) {
                return Err(ParseError::InvalidBlur);
            }
        }
        if let Some(v) = self.brightness {
            if !(0.0..=2.0).contains(&v) {
                return Err(ParseError::InvalidBrightness);
            }
        }
        if let Some(v) = self.contrast {
            if !(0.0..=2.0).contains(&v) {
                return Err(ParseError::InvalidContrast);
            }
        }
        if let Some(v) = self.saturation {
            if !(0.0..=2.0).contains(&v) {
                return Err(ParseError::InvalidSaturation);
            }
        }
        if let Some(v) = self.gamma {
            if !(0.1..=10.0).contains(&v) {
                return Err(ParseError::InvalidGamma);
            }
        }
        if let Some(v) = self.trim {
            if !(1.0..=100.0).contains(&v) {
                return Err(ParseError::InvalidTrim);
            }
        }
        if let Some(f) = self.frame {
            if f > 999 {
                return Err(ParseError::InvalidFrame);
            }
        }
        Ok(())
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

        match key {
            "w" | "width" => params.width = Some(parse_u32(value).ok_or(ParseError::InvalidWidth)?),
            "h" | "height" => {
                params.height = Some(parse_u32(value).ok_or(ParseError::InvalidHeight)?)
            }
            "q" | "quality" => {
                let q = parse_u32(value).ok_or(ParseError::InvalidQuality)?;
                if q > 255 {
                    return Err(ParseError::InvalidQuality);
                }
                params.quality = q as u8;
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
            _ => return Err(ParseError::InvalidParameter),
        }
    }

    Ok(params)
}

fn parse_u32(s: &str) -> Option<u32> {
    s.parse::<u32>().ok()
}

fn parse_f32(s: &str) -> Option<f32> {
    let val = s.parse::<f32>().ok()?;
    if val.is_nan() || val.is_infinite() {
        None
    } else {
        Some(val)
    }
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
        assert!(TransformParams {
            sharpen: Some(5.0),
            ..Default::default()
        }
        .validate()
        .is_ok());
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
        assert!(TransformParams {
            blur: Some(1.0),
            ..Default::default()
        }
        .validate()
        .is_ok());
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
        assert!(TransformParams {
            dpr: 2.5,
            ..Default::default()
        }
        .validate()
        .is_ok());
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
        assert!(TransformParams {
            width: Some(1),
            ..Default::default()
        }
        .validate()
        .is_ok());
        assert!(TransformParams {
            width: Some(8192),
            ..Default::default()
        }
        .validate()
        .is_ok());
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
        assert!(TransformParams {
            brightness: Some(1.0),
            ..Default::default()
        }
        .validate()
        .is_ok());
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
        assert!(TransformParams {
            contrast: Some(0.0),
            ..Default::default()
        }
        .validate()
        .is_ok());
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
        assert!(TransformParams {
            saturation: Some(1.5),
            ..Default::default()
        }
        .validate()
        .is_ok());
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
        assert!(TransformParams {
            gamma: Some(2.2),
            ..Default::default()
        }
        .validate()
        .is_ok());
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
        assert!(TransformParams {
            trim: Some(50.0),
            ..Default::default()
        }
        .validate()
        .is_ok());
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
