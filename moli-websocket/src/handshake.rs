use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bytes::BytesMut;
use moli_stealth_net::BoxedStream;
use ratchet_rs::{
    ExtensionProvider, Role, WebSocket, WebSocketConfig,
    deflate::{Deflate, DeflateExtProvider},
};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{headers::request_header_entries, request::chrome_deflate_config};

type Response = http::Response<()>;
pub(crate) type BrowserWebSocket = WebSocket<BoxedStream, Deflate>;

const MAX_WEBSOCKET_HANDSHAKE_RESPONSE_SIZE: usize = 64 * 1024;

pub(crate) async fn browser_client_handshake(
    request: http::Request<()>,
    mut stream: BoxedStream,
) -> Result<(BrowserWebSocket, Response), String> {
    write_handshake_request(&mut stream, &request).await?;
    let (response, tail) = read_handshake_response(&mut stream).await?;
    validate_handshake_response(&request, &response)?;
    validate_extension_selection(&request, &response)?;
    let mut extension = DeflateExtProvider::with_config(chrome_deflate_config())
        .negotiate_client(response.headers())
        .map_err(|error| format!("invalid WebSocket extension negotiation: {error}"))?;
    if extension.is_none()
        && response
            .headers()
            .contains_key(http::header::SEC_WEBSOCKET_EXTENSIONS)
    {
        return Err("WebSocket server selected an unacceptable extension configuration".to_owned());
    }
    let config = WebSocketConfig::default();
    if let Some(extension) = extension.as_mut() {
        extension.set_max_message_size(config.max_message_size);
    }
    let stream = WebSocket::from_upgraded(
        config,
        stream,
        extension,
        BytesMut::from(tail.as_slice()),
        Role::Client,
    );
    Ok((stream, response))
}

async fn write_handshake_request(
    stream: &mut BoxedStream,
    request: &http::Request<()>,
) -> Result<(), String> {
    let request_target = request
        .uri()
        .path_and_query()
        .map(|path| path.as_str())
        .unwrap_or("/");
    let mut raw = format!("GET {request_target} HTTP/1.1\r\n").into_bytes();
    for (name, value) in request_header_entries(request.headers()) {
        raw.extend_from_slice(name.as_bytes());
        raw.extend_from_slice(b": ");
        raw.extend_from_slice(value.as_bytes());
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"\r\n");
    stream
        .write_all(&raw)
        .await
        .map_err(|error| format!("failed to write WebSocket handshake request: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("failed to flush WebSocket handshake request: {error}"))
}

async fn read_handshake_response(stream: &mut BoxedStream) -> Result<(Response, Vec<u8>), String> {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 512];
    loop {
        let count = stream
            .read(&mut chunk)
            .await
            .map_err(|error| format!("failed to read WebSocket handshake response: {error}"))?;
        if count == 0 {
            return Err("WebSocket server closed during handshake".to_owned());
        }
        raw.extend_from_slice(&chunk[..count]);
        if let Some(header_end) = find_header_end(&raw) {
            let tail = raw[header_end..].to_vec();
            let response = parse_handshake_response(&raw[..header_end - 4])?;
            return Ok((response, tail));
        }
        if raw.len() > MAX_WEBSOCKET_HANDSHAKE_RESPONSE_SIZE {
            return Err("WebSocket handshake response is too large".to_owned());
        }
    }
}

fn find_header_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn parse_handshake_response(raw_headers: &[u8]) -> Result<Response, String> {
    let headers = std::str::from_utf8(raw_headers)
        .map_err(|error| format!("WebSocket handshake response is not UTF-8: {error}"))?;
    let mut lines = headers.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| "WebSocket handshake response is missing status line".to_owned())?;
    let mut status_parts = status_line.splitn(3, ' ');
    let version = status_parts.next().unwrap_or_default();
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(format!(
            "WebSocket handshake response has unsupported HTTP version `{version}`"
        ));
    }
    let status = status_parts
        .next()
        .ok_or_else(|| "WebSocket handshake response is missing status code".to_owned())?
        .parse::<u16>()
        .map_err(|error| format!("WebSocket handshake response has invalid status: {error}"))?;
    let mut response = Response::new(());
    *response.status_mut() = http::StatusCode::from_u16(status)
        .map_err(|error| format!("WebSocket handshake response has invalid status: {error}"))?;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!(
                "WebSocket handshake response has malformed header `{line}`"
            ));
        };
        let name =
            http::header::HeaderName::from_bytes(name.trim().as_bytes()).map_err(|error| {
                format!("WebSocket handshake response has invalid header name: {error}")
            })?;
        let value = http::header::HeaderValue::from_str(value.trim()).map_err(|error| {
            format!("WebSocket handshake response has invalid header value: {error}")
        })?;
        response.headers_mut().append(name, value);
    }

    Ok(response)
}

fn validate_handshake_response(
    request: &http::Request<()>,
    response: &Response,
) -> Result<(), String> {
    if response.status() != http::StatusCode::SWITCHING_PROTOCOLS {
        return Err(format!(
            "WebSocket server returned HTTP status {}",
            response.status()
        ));
    }
    if !header_values_contain_token(response.headers(), http::header::UPGRADE, "websocket") {
        return Err("WebSocket handshake response is missing `Upgrade: websocket`".to_owned());
    }
    if !header_values_contain_token(response.headers(), http::header::CONNECTION, "upgrade") {
        return Err("WebSocket handshake response is missing `Connection: Upgrade`".to_owned());
    }
    let key = request
        .headers()
        .get(http::header::SEC_WEBSOCKET_KEY)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "WebSocket request is missing Sec-WebSocket-Key".to_owned())?;
    let expected_accept = derive_accept_key(key);
    let accept = response
        .headers()
        .get(http::header::SEC_WEBSOCKET_ACCEPT)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "WebSocket handshake response is missing Sec-WebSocket-Accept".to_owned())?;
    if accept.trim() != expected_accept {
        return Err("WebSocket handshake response has invalid Sec-WebSocket-Accept".to_owned());
    }
    validate_response_subprotocol(request, response)
}

fn derive_accept_key(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    BASE64.encode(hasher.finalize())
}

fn validate_extension_selection(
    request: &http::Request<()>,
    response: &Response,
) -> Result<(), String> {
    let offered = request
        .headers()
        .get_all(http::header::SEC_WEBSOCKET_EXTENSIONS)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value.split(',').any(|extension| {
                extension
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("permessage-deflate")
            })
        });

    let mut selections = Vec::new();
    for value in response
        .headers()
        .get_all(http::header::SEC_WEBSOCKET_EXTENSIONS)
        .iter()
    {
        let value = value
            .to_str()
            .map_err(|error| format!("WebSocket response extensions are invalid: {error}"))?;
        selections.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        );
    }
    if selections.is_empty() {
        return Ok(());
    }
    if !offered {
        return Err("WebSocket server selected an unoffered extension".to_owned());
    }
    if selections.len() != 1 {
        return Err("WebSocket server selected multiple extensions".to_owned());
    }

    let mut parameters = selections[0].split(';');
    let extension = parameters.next().unwrap_or_default().trim();
    if !extension.eq_ignore_ascii_case("permessage-deflate") {
        return Err(format!(
            "WebSocket server selected unsupported extension `{extension}`"
        ));
    }
    for parameter in parameters {
        let name = parameter
            .trim()
            .split_once('=')
            .map(|(name, _)| name)
            .unwrap_or(parameter.trim());

        if name.eq_ignore_ascii_case("client_max_window_bits") && !parameter.trim().contains('=') {
            return Err(
                "WebSocket server selected invalid permessage-deflate parameter `client_max_window_bits` without a value"
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn header_values_contain_token(
    headers: &http::HeaderMap,
    name: http::header::HeaderName,
    token: &str,
) -> bool {
    headers.get_all(name).iter().any(|value| {
        value
            .to_str()
            .ok()
            .into_iter()
            .flat_map(|value| value.split(','))
            .any(|value| value.trim().eq_ignore_ascii_case(token))
    })
}

fn validate_response_subprotocol(
    request: &http::Request<()>,
    response: &Response,
) -> Result<(), String> {
    let selected_protocols = response
        .headers()
        .get_all(http::header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .map(|value| {
            value
                .to_str()
                .map(|value| value.trim())
                .map_err(|error| format!("WebSocket response protocol is invalid: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if selected_protocols.is_empty() {
        return Ok(());
    }
    if selected_protocols.len() != 1 {
        return Err("WebSocket server selected multiple subprotocols".to_owned());
    }
    let selected = selected_protocols[0];
    if selected.is_empty() {
        return Err("WebSocket server selected an empty subprotocol".to_owned());
    }
    if selected.contains(',') {
        return Err("WebSocket server selected multiple subprotocols".to_owned());
    }

    let requested = requested_subprotocols(request)?;
    if requested.iter().any(|protocol| protocol == selected) {
        Ok(())
    } else {
        Err(format!(
            "WebSocket server selected unrequested subprotocol `{selected}`"
        ))
    }
}

fn requested_subprotocols(request: &http::Request<()>) -> Result<Vec<String>, String> {
    let mut protocols = Vec::new();
    for value in request
        .headers()
        .get_all(http::header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
    {
        let value = value
            .to_str()
            .map_err(|error| format!("WebSocket request protocol is invalid: {error}"))?;
        protocols.extend(
            value
                .split(',')
                .map(str::trim)
                .filter(|protocol| !protocol.is_empty())
                .map(ToOwned::to_owned),
        );
    }
    Ok(protocols)
}
