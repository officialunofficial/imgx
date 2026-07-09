//! Structured HTTP error responses. See docs/INVARIANTS.md INV-9 — status
//! codes and the JSON envelope shape are a fixed contract.

/// A structured HTTP error that serializes to a JSON response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    pub status: u16,
    pub message: &'static str,
    pub detail: Option<String>,
}

impl HttpError {
    pub fn bad_request(detail: impl Into<Option<String>>) -> Self {
        Self {
            status: 400,
            message: "Bad Request",
            detail: detail.into(),
        }
    }

    pub fn not_found(detail: impl Into<Option<String>>) -> Self {
        Self {
            status: 404,
            message: "Not Found",
            detail: detail.into(),
        }
    }

    pub fn payload_too_large(detail: impl Into<Option<String>>) -> Self {
        Self {
            status: 413,
            message: "Payload Too Large",
            detail: detail.into(),
        }
    }

    pub fn unprocessable_entity(detail: impl Into<Option<String>>) -> Self {
        Self {
            status: 422,
            message: "Unprocessable Entity",
            detail: detail.into(),
        }
    }

    pub fn internal_error(detail: impl Into<Option<String>>) -> Self {
        Self {
            status: 500,
            message: "Internal Server Error",
            detail: detail.into(),
        }
    }

    pub fn bad_gateway(detail: impl Into<Option<String>>) -> Self {
        Self {
            status: 502,
            message: "Bad Gateway",
            detail: detail.into(),
        }
    }

    pub fn gateway_timeout(detail: impl Into<Option<String>>) -> Self {
        Self {
            status: 504,
            message: "Gateway Timeout",
            detail: detail.into(),
        }
    }

    /// Serialize to the JSON response body, e.g.:
    /// `{"error":{"status":400,"message":"Bad Request","detail":"invalid width"}}`
    /// The `detail` key is entirely absent (not `null`) when there is no detail.
    pub fn to_json_response(&self) -> String {
        match &self.detail {
            Some(detail) => format!(
                "{{\"error\":{{\"status\":{},\"message\":\"{}\",\"detail\":\"{}\"}}}}",
                self.status,
                self.message,
                json_escape(detail)
            ),
            None => format!(
                "{{\"error\":{{\"status\":{},\"message\":\"{}\"}}}}",
                self.status, self.message
            ),
        }
    }
}

/// Minimal JSON string escaping for the `detail` field (backslash, quote,
/// and control characters). Detail strings originate from our own error
/// messages, not untrusted input, but escape defensively regardless.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Standard HTTP reason phrase for a status code.
pub fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_request_has_status_400() {
        let err = HttpError::bad_request(Some("invalid width".to_string()));
        assert_eq!(err.status, 400);
        assert_eq!(err.message, "Bad Request");
        assert_eq!(err.detail.as_deref(), Some("invalid width"));
    }

    #[test]
    fn not_found_has_status_404() {
        let err = HttpError::not_found(Some("image not found".to_string()));
        assert_eq!(err.status, 404);
        assert_eq!(err.message, "Not Found");
    }

    #[test]
    fn payload_too_large_has_status_413() {
        let err = HttpError::payload_too_large(Some("exceeds 10MB limit".to_string()));
        assert_eq!(err.status, 413);
        assert_eq!(err.message, "Payload Too Large");
    }

    #[test]
    fn unprocessable_entity_has_status_422() {
        let err = HttpError::unprocessable_entity(Some("invalid parameters".to_string()));
        assert_eq!(err.status, 422);
        assert_eq!(err.message, "Unprocessable Entity");
    }

    #[test]
    fn internal_error_has_status_500() {
        let err = HttpError::internal_error(Some("unexpected failure".to_string()));
        assert_eq!(err.status, 500);
        assert_eq!(err.message, "Internal Server Error");
    }

    #[test]
    fn bad_gateway_has_status_502() {
        let err = HttpError::bad_gateway(Some("origin unreachable".to_string()));
        assert_eq!(err.status, 502);
        assert_eq!(err.message, "Bad Gateway");
    }

    #[test]
    fn gateway_timeout_has_status_504() {
        let err = HttpError::gateway_timeout(Some("origin timed out".to_string()));
        assert_eq!(err.status, 504);
        assert_eq!(err.message, "Gateway Timeout");
    }

    #[test]
    fn to_json_response_with_detail() {
        let err = HttpError::bad_request(Some("invalid width".to_string()));
        assert_eq!(
            err.to_json_response(),
            "{\"error\":{\"status\":400,\"message\":\"Bad Request\",\"detail\":\"invalid width\"}}"
        );
    }

    #[test]
    fn to_json_response_without_detail_omits_detail_field() {
        let err = HttpError::not_found(None);
        assert_eq!(
            err.to_json_response(),
            "{\"error\":{\"status\":404,\"message\":\"Not Found\"}}"
        );
    }

    #[test]
    fn to_json_response_for_internal_error_with_detail() {
        let err = HttpError::internal_error(Some("disk full".to_string()));
        assert_eq!(
            err.to_json_response(),
            "{\"error\":{\"status\":500,\"message\":\"Internal Server Error\",\"detail\":\"disk full\"}}"
        );
    }

    #[test]
    fn status_text_maps_400() {
        assert_eq!(status_text(400), "Bad Request");
    }

    #[test]
    fn status_text_maps_404() {
        assert_eq!(status_text(404), "Not Found");
    }

    #[test]
    fn status_text_maps_413() {
        assert_eq!(status_text(413), "Payload Too Large");
    }

    #[test]
    fn status_text_maps_422() {
        assert_eq!(status_text(422), "Unprocessable Entity");
    }

    #[test]
    fn status_text_maps_500() {
        assert_eq!(status_text(500), "Internal Server Error");
    }

    #[test]
    fn status_text_maps_502() {
        assert_eq!(status_text(502), "Bad Gateway");
    }

    #[test]
    fn status_text_maps_504() {
        assert_eq!(status_text(504), "Gateway Timeout");
    }

    #[test]
    fn status_text_unknown_code_returns_unknown() {
        assert_eq!(status_text(999), "Unknown");
    }

    #[test]
    fn error_constructor_with_null_detail() {
        let err = HttpError::bad_request(None);
        assert_eq!(err.status, 400);
        assert_eq!(err.detail, None);
    }
}
