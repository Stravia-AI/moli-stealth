use encoding_rs::Encoding;
use moli_content_type::parse_response_content_type;

/// Looks up an encoding label after the caller has applied its own whitespace
/// rules, matching Blink's exact `TextEncoding` registry lookup.
pub fn encoding_for_label(label: &str) -> Option<&'static Encoding> {
    let bytes = label.as_bytes();
    // `encoding_rs` implements the Encoding Standard's preprocessing and
    // trims ASCII whitespace itself. Blink's response path has already
    // removed HTTP LWS, so accepting anything still present here (notably VT
    // and FF) would turn an invalid response charset into a valid label.
    if bytes.first().is_some_and(|byte| byte.is_ascii_whitespace())
        || bytes.last().is_some_and(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }

    Encoding::for_label(bytes)
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
