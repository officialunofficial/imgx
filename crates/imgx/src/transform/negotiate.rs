//! Content negotiation for image format selection. Ported from
//! src/transform/negotiate.zig. See docs/INVARIANTS.md INV-7 — the
//! priority order is fixed and must always resolve to a concrete format.

use super::params::OutputFormat;

/// Result of parsing an HTTP Accept header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcceptResult {
    pub supports_avif: bool,
    pub supports_webp: bool,
    pub supports_jpeg: bool,
    pub supports_png: bool,
    pub supports_gif: bool,
    pub wildcard: bool,
}

impl AcceptResult {
    pub fn supports(&self, fmt: OutputFormat) -> bool {
        match fmt {
            OutputFormat::Avif => self.supports_avif || self.wildcard,
            OutputFormat::Webp => self.supports_webp || self.wildcard,
            OutputFormat::Jpeg => self.supports_jpeg || self.wildcard,
            OutputFormat::Png => self.supports_png || self.wildcard,
            OutputFormat::Gif => self.supports_gif || self.wildcard,
            OutputFormat::Auto => true,
        }
    }
}

/// Parse a standard HTTP Accept header value. Quality values are parsed
/// but only used to detect formats explicitly disabled via `q=0`; the
/// priority ordering itself is fixed (see `negotiate_format`).
pub fn parse_accept_header(accept: &str) -> AcceptResult {
    let mut result = AcceptResult::default();

    if accept.is_empty() {
        return result;
    }

    for raw_entry in accept.split(',') {
        let entry = raw_entry.trim();
        if entry.is_empty() {
            continue;
        }

        let mut parts = entry.split(';');
        let media_type = match parts.next() {
            Some(mt) => mt.trim(),
            None => continue,
        };

        let mut q_value: f32 = 1.0;
        for param_raw in parts {
            let param = param_raw.trim();
            if let Some(rest) = param
                .strip_prefix("q=")
                .or_else(|| param.strip_prefix("Q="))
            {
                q_value = rest.parse::<f32>().unwrap_or(1.0);
            }
        }

        if q_value == 0.0 {
            continue;
        }

        match media_type {
            "*/*" | "image/*" => result.wildcard = true,
            "image/avif" => result.supports_avif = true,
            "image/webp" => result.supports_webp = true,
            "image/jpeg" | "image/jpg" => result.supports_jpeg = true,
            "image/png" => result.supports_png = true,
            "image/gif" => result.supports_gif = true,
            _ => {}
        }
    }

    result
}

/// Select the best static output format. Explicit non-auto format always
/// wins. Otherwise: alpha source -> avif>webp>png>jpeg(last resort);
/// no-alpha source -> avif>webp>jpeg>png. Empty/absent Accept -> jpeg.
pub fn negotiate_format(
    accept_header: Option<&str>,
    source_has_alpha: bool,
    requested_format: Option<OutputFormat>,
) -> OutputFormat {
    if let Some(fmt) = requested_format {
        if fmt != OutputFormat::Auto {
            return fmt;
        }
    }

    let accept = accept_header.map(parse_accept_header).unwrap_or_default();

    if source_has_alpha {
        if accept.supports(OutputFormat::Avif) {
            return OutputFormat::Avif;
        }
        if accept.supports(OutputFormat::Webp) {
            return OutputFormat::Webp;
        }
        if accept.supports(OutputFormat::Png) {
            return OutputFormat::Png;
        }
        OutputFormat::Jpeg
    } else {
        if accept.supports(OutputFormat::Avif) {
            return OutputFormat::Avif;
        }
        if accept.supports(OutputFormat::Webp) {
            return OutputFormat::Webp;
        }
        if accept.supports(OutputFormat::Jpeg) {
            return OutputFormat::Jpeg;
        }
        if accept.supports(OutputFormat::Png) {
            return OutputFormat::Png;
        }
        OutputFormat::Jpeg
    }
}

/// Select the best format for an animated source. Explicit
/// animation-capable format wins; explicit non-capable format degrades to
/// static (`None`); auto-negotiate prefers webp > gif > `None`.
pub fn negotiate_animated_format(
    accept_header: Option<&str>,
    requested_format: Option<OutputFormat>,
) -> Option<OutputFormat> {
    if let Some(fmt) = requested_format {
        if fmt != OutputFormat::Auto {
            return if fmt.supports_animation() {
                Some(fmt)
            } else {
                None
            };
        }
    }

    let accept = accept_header.map(parse_accept_header).unwrap_or_default();
    if accept.supports(OutputFormat::Webp) {
        return Some(OutputFormat::Webp);
    }
    if accept.supports(OutputFormat::Gif) {
        return Some(OutputFormat::Gif);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accept_header_single_format_webp() {
        let r = parse_accept_header("image/webp");
        assert!(r.supports_webp);
        assert!(!r.supports_avif);
        assert!(!r.supports_jpeg);
        assert!(!r.supports_png);
        assert!(!r.wildcard);
    }

    #[test]
    fn parse_accept_header_multiple_formats_with_image_wildcard() {
        let r = parse_accept_header("image/avif,image/webp,image/*");
        assert!(r.supports_avif);
        assert!(r.supports_webp);
        assert!(r.wildcard);
        assert!(r.supports(OutputFormat::Jpeg));
        assert!(r.supports(OutputFormat::Png));
    }

    #[test]
    fn parse_accept_header_star_star_wildcard() {
        let r = parse_accept_header("*/*");
        assert!(r.wildcard);
        assert!(r.supports(OutputFormat::Avif));
        assert!(r.supports(OutputFormat::Webp));
        assert!(r.supports(OutputFormat::Jpeg));
        assert!(r.supports(OutputFormat::Png));
    }

    #[test]
    fn parse_accept_header_image_wildcard() {
        let r = parse_accept_header("image/*");
        assert!(r.wildcard);
        assert!(r.supports(OutputFormat::Avif));
        assert!(r.supports(OutputFormat::Webp));
        assert!(r.supports(OutputFormat::Jpeg));
        assert!(r.supports(OutputFormat::Png));
    }

    #[test]
    fn parse_accept_header_empty_string() {
        let r = parse_accept_header("");
        assert!(!r.supports_avif);
        assert!(!r.supports_webp);
        assert!(!r.supports_jpeg);
        assert!(!r.supports_png);
        assert!(!r.wildcard);
    }

    #[test]
    fn parse_accept_header_q_values_parsed_q0_disables_format() {
        let r = parse_accept_header("image/webp;q=0,image/avif;q=1.0");
        assert!(r.supports_avif);
        assert!(!r.supports_webp);
    }

    #[test]
    fn parse_accept_header_q_values_nonzero_are_accepted() {
        let r = parse_accept_header("image/webp;q=0.9,image/avif;q=1.0");
        assert!(r.supports_avif);
        assert!(r.supports_webp);
    }

    #[test]
    fn parse_accept_header_spaces_around_entries_are_trimmed() {
        let r = parse_accept_header("  image/avif , image/webp ; q=0.8 , image/png ");
        assert!(r.supports_avif);
        assert!(r.supports_webp);
        assert!(r.supports_png);
        assert!(!r.supports_jpeg);
    }

    #[test]
    fn parse_accept_header_jpeg_and_jpg_aliases() {
        assert!(parse_accept_header("image/jpeg").supports_jpeg);
        assert!(parse_accept_header("image/jpg").supports_jpeg);
    }

    #[test]
    fn parse_accept_header_unknown_media_types_are_ignored() {
        let r = parse_accept_header("image/tiff,application/json,text/html");
        assert!(!r.supports_avif);
        assert!(!r.supports_webp);
        assert!(!r.supports_jpeg);
        assert!(!r.supports_png);
        assert!(!r.wildcard);
    }

    #[test]
    fn parse_accept_header_malformed_entries_are_gracefully_ignored() {
        assert!(!parse_accept_header(";;;").wildcard);
        assert!(!parse_accept_header(",,,").wildcard);
        assert!(parse_accept_header("image/webp;q=notanumber").supports_webp);
        assert!(parse_accept_header("image/webp;q=").supports_webp);
    }

    #[test]
    fn parse_accept_header_realistic_browser_accept_header() {
        let chrome = "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8";
        let r = parse_accept_header(chrome);
        assert!(r.supports_avif);
        assert!(r.supports_webp);
        assert!(r.wildcard);
    }

    #[test]
    fn negotiate_format_explicit_format_overrides_everything() {
        assert_eq!(
            negotiate_format(Some("image/webp"), false, Some(OutputFormat::Png)),
            OutputFormat::Png
        );
        assert_eq!(
            negotiate_format(Some("image/avif"), true, Some(OutputFormat::Jpeg)),
            OutputFormat::Jpeg
        );
        assert_eq!(
            negotiate_format(None, false, Some(OutputFormat::Avif)),
            OutputFormat::Avif
        );
    }

    #[test]
    fn negotiate_format_auto_triggers_negotiation() {
        assert_eq!(
            negotiate_format(Some("image/webp"), false, Some(OutputFormat::Auto)),
            OutputFormat::Webp
        );
    }

    #[test]
    fn negotiate_format_null_requested_format_triggers_negotiation() {
        assert_eq!(
            negotiate_format(Some("image/webp"), false, None),
            OutputFormat::Webp
        );
    }

    #[test]
    fn negotiate_format_null_accept_header_defaults_to_jpeg() {
        assert_eq!(negotiate_format(None, false, None), OutputFormat::Jpeg);
    }

    #[test]
    fn negotiate_format_empty_accept_header_defaults_to_jpeg() {
        assert_eq!(negotiate_format(Some(""), false, None), OutputFormat::Jpeg);
    }

    #[test]
    fn negotiate_format_avif_preferred_over_webp() {
        assert_eq!(
            negotiate_format(Some("image/avif,image/webp"), false, None),
            OutputFormat::Avif
        );
    }

    #[test]
    fn negotiate_format_webp_preferred_over_jpeg() {
        assert_eq!(
            negotiate_format(Some("image/webp,image/jpeg"), false, None),
            OutputFormat::Webp
        );
    }

    #[test]
    fn negotiate_format_jpeg_preferred_over_png_no_alpha() {
        assert_eq!(
            negotiate_format(Some("image/jpeg,image/png"), false, None),
            OutputFormat::Jpeg
        );
    }

    #[test]
    fn negotiate_format_alpha_source_webp_jpeg_webp_preferred() {
        assert_eq!(
            negotiate_format(Some("image/webp,image/jpeg"), true, None),
            OutputFormat::Webp
        );
    }

    #[test]
    fn negotiate_format_alpha_source_jpeg_only_still_jpeg() {
        assert_eq!(
            negotiate_format(Some("image/jpeg"), true, None),
            OutputFormat::Jpeg
        );
    }

    #[test]
    fn negotiate_format_alpha_source_png_jpeg_png_preferred() {
        assert_eq!(
            negotiate_format(Some("image/png,image/jpeg"), true, None),
            OutputFormat::Png
        );
    }

    #[test]
    fn negotiate_format_alpha_source_avif_webp_jpeg_avif_wins() {
        assert_eq!(
            negotiate_format(Some("image/avif,image/webp,image/jpeg"), true, None),
            OutputFormat::Avif
        );
    }

    #[test]
    fn negotiate_format_wildcard_accept_avif_highest_priority() {
        assert_eq!(
            negotiate_format(Some("*/*"), false, None),
            OutputFormat::Avif
        );
    }

    #[test]
    fn negotiate_format_wildcard_accept_alpha_avif() {
        assert_eq!(
            negotiate_format(Some("image/*"), true, None),
            OutputFormat::Avif
        );
    }

    #[test]
    fn negotiate_format_only_png_accepted_no_alpha() {
        assert_eq!(
            negotiate_format(Some("image/png"), false, None),
            OutputFormat::Png
        );
    }

    #[test]
    fn negotiate_format_malformed_accept_fallback_to_jpeg() {
        assert_eq!(
            negotiate_format(Some("garbage/nonsense"), false, None),
            OutputFormat::Jpeg
        );
    }

    #[test]
    fn negotiate_format_all_formats_disabled_by_q0_fallback_to_jpeg() {
        assert_eq!(
            negotiate_format(
                Some("image/avif;q=0,image/webp;q=0,image/png;q=0"),
                false,
                None
            ),
            OutputFormat::Jpeg
        );
    }

    #[test]
    fn negotiate_animated_format_accept_webp() {
        assert_eq!(
            negotiate_animated_format(Some("image/webp"), None),
            Some(OutputFormat::Webp)
        );
    }

    #[test]
    fn negotiate_animated_format_accept_gif() {
        assert_eq!(
            negotiate_animated_format(Some("image/gif"), None),
            Some(OutputFormat::Gif)
        );
    }

    #[test]
    fn negotiate_animated_format_accept_webp_gif_webp_preferred() {
        assert_eq!(
            negotiate_animated_format(Some("image/webp,image/gif"), None),
            Some(OutputFormat::Webp)
        );
    }

    #[test]
    fn negotiate_animated_format_accept_jpeg_only_static_fallback() {
        assert_eq!(negotiate_animated_format(Some("image/jpeg"), None), None);
    }

    #[test]
    fn negotiate_animated_format_explicit_gif_format() {
        assert_eq!(
            negotiate_animated_format(Some("image/jpeg"), Some(OutputFormat::Gif)),
            Some(OutputFormat::Gif)
        );
    }

    #[test]
    fn negotiate_animated_format_explicit_webp_format() {
        assert_eq!(
            negotiate_animated_format(Some("image/jpeg"), Some(OutputFormat::Webp)),
            Some(OutputFormat::Webp)
        );
    }

    #[test]
    fn negotiate_animated_format_explicit_jpeg_not_animated() {
        assert_eq!(
            negotiate_animated_format(Some("image/webp"), Some(OutputFormat::Jpeg)),
            None
        );
    }

    #[test]
    fn negotiate_animated_format_wildcard_accept_webp() {
        assert_eq!(
            negotiate_animated_format(Some("*/*"), None),
            Some(OutputFormat::Webp)
        );
    }

    #[test]
    fn parse_accept_header_gif_support() {
        let r = parse_accept_header("image/gif");
        assert!(r.supports_gif);
        assert!(!r.supports_webp);
    }

    #[test]
    fn accept_result_supports_gif_through_wildcard() {
        assert!(parse_accept_header("*/*").supports(OutputFormat::Gif));
    }
}
