use encoding_rs::Encoding;
use moli_content_type::parse_response_content_type;

pub fn encoding_for_label(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(label.trim().as_bytes())
}

/// The `charset` parameter of a `Content-Type` value.
///
/// Response metadata is parsed with Chromium network-layer tolerances before
/// the label is handed to `encoding_rs`. In particular, quoted separators stay
/// inside their parameter and unterminated quoted values remain recoverable.
pub fn charset_from_content_type(value: &str) -> Option<String> {
    let parsed = parse_response_content_type(value)?;
    let charset = parsed.charset()?;
    (!charset.is_empty()).then(|| charset.to_owned())
}

pub fn charset_from_headers(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .and_then(|(_, value)| charset_from_content_type(value))
}
