//! Outbound proxy protocol mechanisms used by the shared connector.
//!
//! Routing policy lives in `connection::proxy_url`; this module only parses a
//! selected proxy and performs HTTP CONNECT or SOCKS protocol exchanges.

use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use url::Url;

use crate::{
    AuthScheme, ProxyResponse, TransportAuth, TransportError, auth::AuthSession,
    connection::IoStream,
};

const MAX_PROXY_RESPONSE: usize = 64 * 1024;
const MAX_PROXY_HEADERS: usize = 256 * 1024;
const MAX_AUTH_EXCHANGES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProxyKind {
    Http,
    Socks4 { remote_dns: bool },
    Socks5 { remote_dns: bool },
}

#[derive(Debug, Clone)]
pub(crate) struct ProxyConfig {
    kind: ProxyKind,
    host: String,
    port: u16,
    tls: bool,
    url_auth: Option<TransportAuth>,
}

impl ProxyConfig {
    pub(crate) fn parse(url: &Url) -> Result<Self, TransportError> {
        let host = url
            .host_str()
            .ok_or_else(|| TransportError::InvalidInput("proxy URL is missing a host".into()))?
            .trim_matches(['[', ']'])
            .to_owned();
        let (kind, tls, default_port) = match url.scheme() {
            "http" => (ProxyKind::Http, false, 80),
            "https" => (ProxyKind::Http, true, 443),
            "socks4" => (ProxyKind::Socks4 { remote_dns: false }, false, 1080),
            "socks4a" => (ProxyKind::Socks4 { remote_dns: true }, false, 1080),
            "socks5" => (ProxyKind::Socks5 { remote_dns: false }, false, 1080),
            "socks5h" => (ProxyKind::Socks5 { remote_dns: true }, false, 1080),
            scheme => {
                return Err(TransportError::InvalidInput(format!(
                    "unsupported proxy scheme `{scheme}`"
                )));
            }
        };
        let url_auth = if url.username().is_empty() {
            None
        } else {
            Some(TransportAuth {
                scheme: AuthScheme::Basic,
                username: decode_userinfo(url.username())?,
                password: decode_userinfo(url.password().unwrap_or_default())?,
            })
        };
        Ok(Self {
            kind,
            host,
            port: url.port().unwrap_or(default_port),
            tls,
            url_auth,
        })
    }

    pub(crate) fn kind(&self) -> ProxyKind {
        self.kind
    }

    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn uses_tls(&self) -> bool {
        self.tls
    }

    pub(crate) fn url_auth(&self) -> Option<&TransportAuth> {
        self.url_auth.as_ref()
    }

    pub(crate) fn url_credentials(&self) -> Option<(&str, &str)> {
        self.url_auth
            .as_ref()
            .map(|auth| (auth.username.as_str(), auth.password.as_str()))
    }
}

fn decode_userinfo(value: &str) -> Result<String, TransportError> {
    percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .map(|value| value.into_owned())
        .map_err(|_| TransportError::InvalidInput("proxy credentials contain invalid UTF-8".into()))
}

pub(crate) async fn establish_http_tunnel<S: IoStream + AsyncBufRead>(
    stream: &mut S,
    proxy_host: &str,
    target_host: &str,
    target_port: u16,
    bearer_token: Option<&str>,
    configured_auth: Option<&TransportAuth>,
    url_auth: Option<&TransportAuth>,
) -> Result<(), TransportError> {
    let authority = format_authority(target_host, target_port);
    let selected_auth = configured_auth.or(url_auth);
    let mut session = selected_auth
        .map(|auth| AuthSession::new(auth, proxy_host))
        .transpose()?;
    let mut authorization = if let Some(token) = bearer_token {
        validate_header_value(token)?;
        Some(format!("Bearer {token}"))
    } else if let Some(session) = session.as_mut()
        && selected_auth.is_some_and(|auth| auth.scheme == AuthScheme::Basic)
    {
        session.authorization(None, "CONNECT", &authority)?
    } else {
        None
    };

    for exchange in 0..MAX_AUTH_EXCHANGES {
        write_connect_request(stream, &authority, authorization.as_deref()).await?;
        let response = read_proxy_response(stream).await?;
        if (200..300).contains(&response.status) {
            return Ok(());
        }
        let connection_closes = response.headers.iter().any(|(name, value)| {
            (name.eq_ignore_ascii_case("connection")
                || name.eq_ignore_ascii_case("proxy-connection"))
                && value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("close"))
        });
        if response.status != 407
            || bearer_token.is_some()
            || connection_closes
            || exchange + 1 == MAX_AUTH_EXCHANGES
        {
            return Err(TransportError::ProxyConnect(response));
        }
        let Some(session) = session.as_mut() else {
            return Err(TransportError::ProxyConnect(response));
        };
        let Some(challenge) = crate::auth::select_challenge(
            &response.headers,
            selected_auth.unwrap().scheme,
            "proxy-authenticate",
        ) else {
            return Err(TransportError::ProxyConnect(response));
        };
        authorization = session.authorization(Some(challenge), "CONNECT", &authority)?;
        if authorization.is_none() {
            return Err(TransportError::ProxyConnect(response));
        }
    }
    unreachable!("bounded proxy authentication loop always returns")
}

async fn write_connect_request(
    stream: &mut dyn IoStream,
    authority: &str,
    authorization: Option<&str>,
) -> Result<(), TransportError> {
    let mut request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: keep-alive\r\n"
    );
    if let Some(value) = authorization {
        validate_header_value(value)?;
        request.push_str("Proxy-Authorization: ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

fn validate_header_value(value: &str) -> Result<(), TransportError> {
    if value.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
        return Err(TransportError::InvalidInput(
            "proxy authorization contains a newline".into(),
        ));
    }
    Ok(())
}

async fn read_proxy_head<S: IoStream + AsyncBufRead>(
    stream: &mut S,
) -> Result<(u16, Vec<(String, String)>), TransportError> {
    let mut raw = Vec::with_capacity(1024);
    let header_end = loop {
        if raw.ends_with(b"\r\n\r\n") {
            break raw.len();
        }
        let available = stream.fill_buf().await?;
        if available.is_empty() {
            return Err(TransportError::EmptyResponse);
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if raw.len().saturating_add(count) > MAX_PROXY_HEADERS {
            return Err(TransportError::InvalidInput(
                "proxy response headers are too large".into(),
            ));
        }
        raw.extend_from_slice(&available[..count]);
        stream.consume(count);
    };

    let head = String::from_utf8_lossy(&raw[..header_end - 4]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| TransportError::InvalidInput("proxy response has invalid status".into()))?;
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or_else(|| {
            TransportError::InvalidInput("proxy response contains a malformed header".into())
        })?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok((status, headers))
}

async fn read_proxy_response<S: IoStream + AsyncBufRead>(
    stream: &mut S,
) -> Result<ProxyResponse, TransportError> {
    let (status, headers) = loop {
        let (status, headers) = read_proxy_head(stream).await?;
        if !(100..200).contains(&status) || status == 101 {
            break (status, headers);
        }
    };
    if (200..300).contains(&status) {
        // CONNECT 成功即切换为隧道；忽略代理附带的实体长度，保留缓冲中隧道字节。
        return Ok(ProxyResponse {
            status,
            headers,
            body: Vec::new(),
        });
    }

    let declared_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok());
    if declared_length.is_some_and(|length| length > MAX_PROXY_RESPONSE) {
        return Err(TransportError::InvalidInput(
            "proxy response body is too large".into(),
        ));
    }
    let mut body = Vec::new();
    let chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding")
            && value
                .split(',')
                .any(|coding| coding.trim().eq_ignore_ascii_case("chunked"))
    });
    if chunked {
        body = read_chunked_body(stream, body).await?;
    } else if let Some(declared_length) = declared_length {
        let mut chunk = [0_u8; 4096];
        while body.len() < declared_length {
            let remaining = declared_length - body.len();
            let count = stream.read(&mut chunk[..remaining.min(4096)]).await?;
            if count == 0 {
                return Err(TransportError::EmptyResponse);
            }
            body.extend_from_slice(&chunk[..count]);
        }
        body.truncate(declared_length);
    } else if headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("connection") && value.eq_ignore_ascii_case("close")
    }) {
        loop {
            if body.len() >= MAX_PROXY_RESPONSE {
                return Err(TransportError::InvalidInput(
                    "proxy response body is too large".into(),
                ));
            }
            let mut chunk = [0_u8; 4096];
            let count = stream.read(&mut chunk).await?;
            if count == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..count]);
            if body.len() > MAX_PROXY_RESPONSE {
                return Err(TransportError::InvalidInput(
                    "proxy response body is too large".into(),
                ));
            }
        }
    }
    Ok(ProxyResponse {
        status,
        headers,
        body,
    })
}

async fn read_chunked_body(
    stream: &mut dyn IoStream,
    mut pending: Vec<u8>,
) -> Result<Vec<u8>, TransportError> {
    let mut body = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let line_end = loop {
            if let Some(position) = pending.windows(2).position(|window| window == b"\r\n") {
                break position;
            }
            if pending.len() > 128 {
                return Err(TransportError::InvalidInput(
                    "proxy chunk size line is too large".into(),
                ));
            }
            let count = stream.read(&mut chunk).await?;
            if count == 0 {
                return Err(TransportError::EmptyResponse);
            }
            pending.extend_from_slice(&chunk[..count]);
        };
        let size_text = std::str::from_utf8(&pending[..line_end])
            .map_err(|_| TransportError::InvalidInput("invalid proxy chunk size".into()))?;
        let size =
            usize::from_str_radix(size_text.split(';').next().unwrap_or_default().trim(), 16)
                .map_err(|_| TransportError::InvalidInput("invalid proxy chunk size".into()))?;
        drop(pending.drain(..line_end + 2));
        if size == 0 {
            loop {
                if pending.starts_with(b"\r\n") {
                    return Ok(body);
                }
                if let Some(end) = pending.windows(4).position(|window| window == b"\r\n\r\n") {
                    drop(pending.drain(..end + 4));
                    return Ok(body);
                }
                if pending.len() > MAX_PROXY_RESPONSE {
                    return Err(TransportError::InvalidInput(
                        "proxy response trailers are too large".into(),
                    ));
                }
                let count = stream.read(&mut chunk).await?;
                if count == 0 {
                    return Err(TransportError::EmptyResponse);
                }
                pending.extend_from_slice(&chunk[..count]);
            }
        }
        if body.len().saturating_add(size) > MAX_PROXY_RESPONSE {
            return Err(TransportError::InvalidInput(
                "proxy response body is too large".into(),
            ));
        }
        while pending.len() < size + 2 {
            let count = stream.read(&mut chunk).await?;
            if count == 0 {
                return Err(TransportError::EmptyResponse);
            }
            pending.extend_from_slice(&chunk[..count]);
        }
        if &pending[size..size + 2] != b"\r\n" {
            return Err(TransportError::InvalidInput(
                "proxy chunk is missing its terminator".into(),
            ));
        }
        body.extend_from_slice(&pending[..size]);
        drop(pending.drain(..size + 2));
    }
}

pub(crate) async fn establish_socks4_tunnel(
    stream: &mut dyn IoStream,
    target_host: &str,
    target_port: u16,
    username: Option<&str>,
) -> Result<(), TransportError> {
    let username = username.unwrap_or_default();
    if username.contains('\0') || target_host.contains('\0') {
        return Err(TransportError::InvalidInput(
            "SOCKS4 strings contain NUL".into(),
        ));
    }
    let address = target_host.parse::<Ipv4Addr>().ok();
    if target_host.parse::<Ipv6Addr>().is_ok() {
        return Err(TransportError::InvalidInput(
            "SOCKS4 does not support IPv6".into(),
        ));
    }
    let mut request = Vec::with_capacity(10 + username.len() + target_host.len());
    request.extend_from_slice(&[4, 1]);
    request.extend_from_slice(&target_port.to_be_bytes());
    request.extend_from_slice(&address.map_or([0, 0, 0, 1], |ip| ip.octets()));
    request.extend_from_slice(username.as_bytes());
    request.push(0);
    if address.is_none() {
        request.extend_from_slice(target_host.as_bytes());
        request.push(0);
    }
    stream.write_all(&request).await?;
    stream.flush().await?;
    let mut response = [0; 8];
    stream.read_exact(&mut response).await?;
    if response[0] != 0 || response[1] != 90 {
        return Err(TransportError::Io(std::io::Error::other(format!(
            "SOCKS4 CONNECT failed with status {}",
            response[1]
        ))));
    }
    Ok(())
}

pub(crate) async fn establish_socks5_tunnel(
    stream: &mut dyn IoStream,
    target_host: &str,
    target_port: u16,
    credentials: Option<(&str, &str)>,
) -> Result<(), TransportError> {
    let methods: &[u8] = if credentials.is_some() {
        &[0x00, 0x02]
    } else {
        &[0x00]
    };
    let mut greeting = vec![0x05, methods.len() as u8];
    greeting.extend_from_slice(methods);
    stream.write_all(&greeting).await?;
    stream.flush().await?;

    let mut selection = [0_u8; 2];
    stream.read_exact(&mut selection).await?;
    if selection[0] != 0x05 || selection[1] == 0xff {
        return Err(TransportError::Authentication(
            "SOCKS5 proxy rejected authentication methods".into(),
        ));
    }
    if selection[1] == 0x02 {
        let (username, password) = credentials.ok_or_else(|| {
            TransportError::Authentication("SOCKS5 proxy requested credentials".into())
        })?;
        if username.len() > u8::MAX as usize || password.len() > u8::MAX as usize {
            return Err(TransportError::InvalidInput(
                "SOCKS5 proxy credentials are too long".into(),
            ));
        }
        let mut request = vec![0x01, username.len() as u8];
        request.extend_from_slice(username.as_bytes());
        request.push(password.len() as u8);
        request.extend_from_slice(password.as_bytes());
        stream.write_all(&request).await?;
        stream.flush().await?;
        let mut response = [0_u8; 2];
        stream.read_exact(&mut response).await?;
        if response[0] != 1 || response[1] != 0 {
            return Err(TransportError::Authentication(
                "SOCKS5 proxy rejected credentials".into(),
            ));
        }
    } else if selection[1] != 0x00 {
        return Err(TransportError::Authentication(format!(
            "SOCKS5 proxy selected unsupported authentication method {}",
            selection[1]
        )));
    }

    let mut request = vec![0x05, 0x01, 0x00];
    if let Ok(address) = target_host.parse::<Ipv4Addr>() {
        request.push(0x01);
        request.extend_from_slice(&address.octets());
    } else if let Ok(address) = target_host.parse::<Ipv6Addr>() {
        request.push(0x04);
        request.extend_from_slice(&address.octets());
    } else {
        if target_host.len() > u8::MAX as usize {
            return Err(TransportError::InvalidInput(
                "SOCKS5 target host is too long".into(),
            ));
        }
        request.extend_from_slice(&[0x03, target_host.len() as u8]);
        request.extend_from_slice(target_host.as_bytes());
    }
    request.extend_from_slice(&target_port.to_be_bytes());
    stream.write_all(&request).await?;
    stream.flush().await?;

    let mut response = [0_u8; 4];
    stream.read_exact(&mut response).await?;
    if response[0] != 0x05 || response[1] != 0x00 {
        return Err(TransportError::Io(std::io::Error::other(format!(
            "SOCKS5 CONNECT failed with status {}",
            response[1]
        ))));
    }
    let address_length = match response[3] {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut length = [0_u8; 1];
            stream.read_exact(&mut length).await?;
            length[0] as usize
        }
        _ => {
            return Err(TransportError::InvalidInput(
                "SOCKS5 proxy returned invalid address type".into(),
            ));
        }
    };
    let mut bound_address_and_port = vec![0_u8; address_length + 2];
    stream.read_exact(&mut bound_address_and_port).await?;
    Ok(())
}

fn format_authority(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
