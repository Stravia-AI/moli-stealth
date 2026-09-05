pub(crate) fn request_header_entries(headers: &http::HeaderMap) -> Vec<(String, String)> {
    const ORDER: [(&str, &str); 13] = [
        ("host", "Host"),
        ("connection", "Connection"),
        ("pragma", "Pragma"),
        ("cache-control", "Cache-Control"),
        ("user-agent", "User-Agent"),
        ("upgrade", "Upgrade"),
        ("origin", "Origin"),
        ("sec-websocket-version", "Sec-WebSocket-Version"),
        ("accept-encoding", "Accept-Encoding"),
        ("accept-language", "Accept-Language"),
        ("sec-websocket-key", "Sec-WebSocket-Key"),
        ("sec-websocket-extensions", "Sec-WebSocket-Extensions"),
        ("sec-websocket-protocol", "Sec-WebSocket-Protocol"),
    ];
    let mut entries = Vec::with_capacity(headers.len());
    for (name, display_name) in ORDER {
        for value in headers.get_all(name) {
            entries.push((
                display_name.to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            ));
        }
    }
    for (name, value) in headers {
        if !ORDER
            .iter()
            .any(|(ordered, _)| name.as_str().eq_ignore_ascii_case(ordered))
        {
            entries.push((
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            ));
        }
    }
    entries
}

pub(crate) fn header_map_entries(headers: &http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

pub(crate) fn insert_header_if_absent(
    request: &mut http::Request<()>,
    name: http::header::HeaderName,
    value: &str,
) -> Result<(), http::header::InvalidHeaderValue> {
    if !request.headers().contains_key(&name) {
        request.headers_mut().insert(name, value.parse()?);
    }
    Ok(())
}
