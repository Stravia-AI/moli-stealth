/// A response `Content-Type` parsed with Chromium network-layer tolerances.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseContentType {
    mime_type: String,
    parameters: Vec<(String, String)>,
    charset: Option<String>,
}

impl ResponseContentType {
    /// The lower-case media type Chromium accepted from the response.
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// Parsed parameters in header order.
    pub fn parameters(&self) -> &[(String, String)] {
        &self.parameters
    }

    /// The first parameter with the given ASCII-case-insensitive name.
    pub fn parameter(&self, name: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The first `charset` value, with HTTP linear whitespace trimmed and
    /// ASCII letters lowercased to match Chromium's response metadata.
    pub fn charset(&self) -> Option<&str> {
        self.charset.as_deref()
    }

    /// The first `boundary` value, with HTTP linear whitespace trimmed.
    pub fn boundary(&self) -> Option<&str> {
        self.parameter("boundary").map(trim_http_lws)
    }
}

/// Parses a raw HTTP response `Content-Type` field like Chromium's
/// `net::HttpUtil::ParseContentType` and `net::ParseMimeType` path.
///
/// This intentionally accepts malformed values seen on the network, including
/// an unterminated quoted parameter and an empty subtype. Use this only for
/// Chromium transport-metadata behavior, such as selecting a response
/// `charset` or multipart `boundary`. A value does not belong here merely
/// because it came from a response header: web-platform MIME classification
/// and sniffing use [`crate::parse_mime_type`].
pub fn parse_response_content_type(input: &str) -> Option<ResponseContentType> {
    // Chromium treats an exact bare wildcard as meaningless, while retaining
    // wildcard values that have parameters or even trailing whitespace.
    if input == "*/*" {
        return None;
    }

    let bytes = input.as_bytes();
    let input_len = bytes.len();
    let type_start = skip_http_lws(bytes, 0);
    let type_end = bytes[type_start..]
        .iter()
        .position(|byte| is_http_lws(*byte) || matches!(*byte, b';' | b'('))
        .map_or(input_len, |offset| type_start + offset);
    let slash = bytes.iter().position(|byte| *byte == b'/')?;
    if slash > type_end {
        return None;
    }

    let mime_type = input[type_start..type_end].to_ascii_lowercase();
    let mut parameters = Vec::new();
    let mut offset = find_from(bytes, type_end, |byte| byte == b';').unwrap_or(input_len);

    while offset < input_len {
        offset = skip_http_lws(bytes, offset + 1);
        let name_start = offset;
        let Some(delimiter) = find_from(bytes, offset, |byte| matches!(byte, b';' | b'=')) else {
            break;
        };
        offset = delimiter;
        if bytes[offset] == b';' {
            continue;
        }

        let name = input[name_start..offset].to_owned();
        offset = skip_http_lws(bytes, offset + 1);
        if offset >= input_len || bytes[offset] == b';' {
            continue;
        }

        let value = if bytes[offset] == b'"' {
            let (value, next_offset) = parse_quoted_value(input, offset + 1);
            offset = find_from(bytes, next_offset, |byte| byte == b';').unwrap_or(input_len);
            value
        } else {
            let value_start = offset;
            offset = find_from(bytes, offset, |byte| byte == b';').unwrap_or(input_len);
            let value_end = trim_trailing_http_lws(bytes, value_start, offset);
            input[value_start..value_end].to_owned()
        };
        parameters.push((name, value));
    }

    let charset = parameters
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("charset"))
        .map(|(_, value)| trim_http_lws(value).to_ascii_lowercase());

    Some(ResponseContentType {
        mime_type,
        parameters,
        charset,
    })
}

fn parse_quoted_value(input: &str, mut offset: usize) -> (String, usize) {
    let bytes = input.as_bytes();
    let mut value = String::new();
    while offset < bytes.len() && bytes[offset] != b'"' {
        if bytes[offset] == b'\\' && offset + 1 < bytes.len() {
            offset += 1;
        }
        let character = input[offset..]
            .chars()
            .next()
            .expect("offset is inside the input");
        value.push(character);
        offset += character.len_utf8();
    }
    (value, offset)
}

fn find_from(bytes: &[u8], start: usize, predicate: impl Fn(u8) -> bool) -> Option<usize> {
    bytes[start..]
        .iter()
        .position(|byte| predicate(*byte))
        .map(|offset| start + offset)
}

fn skip_http_lws(bytes: &[u8], mut offset: usize) -> usize {
    while bytes.get(offset).is_some_and(|byte| is_http_lws(*byte)) {
        offset += 1;
    }
    offset
}

fn trim_trailing_http_lws(bytes: &[u8], start: usize, mut end: usize) -> usize {
    while end > start && is_http_lws(bytes[end - 1]) {
        end -= 1;
    }
    end
}

fn trim_http_lws(value: &str) -> &str {
    value.trim_matches(|character: char| matches!(character, ' ' | '\t'))
}

fn is_http_lws(byte: u8) -> bool {
    // Chromium's HTTP_LWS is exactly SP | HT and deliberately excludes
    // newlines.
    matches!(byte, b' ' | b'\t')
}
