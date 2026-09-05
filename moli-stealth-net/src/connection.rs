use std::{env, fmt, net::SocketAddr, time::Duration};

use http::Version;
use tokio::io::{AsyncRead, AsyncWrite};
use url::Url;

use crate::net::{proxy, tcp, tls};
use crate::{TransportAuth, TransportError, TransportFingerprint};

pub trait IoStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> IoStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

pub type BoxedStream = Box<dyn IoStream>;

pub struct ConnectedStream {
    pub stream: BoxedStream,
    pub version: Version,
    pub absolute_form: bool,
}

#[derive(Clone, Default)]
pub struct ConnectionOptions {
    /// Caller-approved direct addresses. When present, DNS is never used for
    /// the origin and no address outside this list is attempted.
    pub resolved_addresses: Option<Vec<SocketAddr>>,
    /// Explicit proxy URL. An empty string explicitly disables environment
    /// proxy discovery; `None` permits it.
    pub proxy: Option<String>,
    pub no_proxy: Option<String>,
    pub proxy_bearer_token: Option<String>,
    pub proxy_auth: Option<TransportAuth>,
    /// Deadline for the complete connection phase: DNS, TCP, proxy exchange,
    /// and TLS. Request/response deadlines remain the caller's responsibility.
    pub connect_timeout: Option<Duration>,
    /// Context-scoped TLS state. Supplying the same handle allows eligible
    /// HTTPS and WSS connections to reuse server-issued sessions.
    pub tls_session_cache: Option<tls::TlsSessionCache>,
}

impl fmt::Debug for ConnectionOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionOptions")
            .field("resolved_addresses", &self.resolved_addresses)
            .field("proxy", &self.proxy.as_ref().map(|_| "[CONFIGURED]"))
            .field("no_proxy", &self.no_proxy)
            .field(
                "proxy_bearer_token",
                &self.proxy_bearer_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("proxy_auth", &self.proxy_auth)
            .field("connect_timeout", &self.connect_timeout)
            .field(
                "tls_session_cache",
                &self.tls_session_cache.as_ref().map(|_| "[CONFIGURED]"),
            )
            .finish()
    }
}

/// Establishes the byte stream used by HTTP and outbound WebSocket clients.
///
/// `tunnel` forces an HTTP CONNECT tunnel even for a clear-text target, which
/// is required for `ws:` so proxy credentials cannot enter the Upgrade
/// request. Direct callers may pin approved addresses through
/// [`ConnectionOptions::resolved_addresses`].
pub async fn open_connection(
    url: &Url,
    options: &ConnectionOptions,
    fingerprint: &TransportFingerprint,
    tls_verify: bool,
    http1_only: bool,
    tunnel: bool,
) -> Result<ConnectedStream, TransportError> {
    fingerprint.validate()?;
    let future = open_connection_inner(url, options, fingerprint, tls_verify, http1_only, tunnel);
    match options.connect_timeout {
        Some(deadline) => tokio::time::timeout(deadline, future)
            .await
            .map_err(|_| TransportError::Timeout)?,
        None => future.await,
    }
}

async fn open_connection_inner(
    url: &Url,
    options: &ConnectionOptions,
    fingerprint: &TransportFingerprint,
    tls_verify: bool,
    http1_only: bool,
    tunnel: bool,
) -> Result<ConnectedStream, TransportError> {
    let scheme = url.scheme();
    if !matches!(scheme, "http" | "https" | "ws" | "wss") {
        return Err(TransportError::InvalidInput(format!(
            "unsupported connection URL scheme `{scheme}`"
        )));
    }
    let target_host = url
        .host_str()
        .ok_or_else(|| TransportError::InvalidInput("connection URL is missing a host".into()))?
        .trim_matches(['[', ']']);
    let target_port = url
        .port_or_known_default()
        .ok_or_else(|| TransportError::InvalidInput("connection URL is missing a port".into()))?;
    let target_uses_tls = matches!(scheme, "https" | "wss");

    let selected_proxy = proxy_url(url, options)?;
    let tls_route = match selected_proxy.as_ref() {
        None => tls::TlsRoute::Direct,
        Some(proxy_url) => {
            let config = proxy::ProxyConfig::parse(proxy_url)?;
            tls::TlsRoute::Proxy {
                scheme: proxy_url.scheme().to_ascii_lowercase(),
                host: config.host().to_ascii_lowercase(),
                port: config.port(),
            }
        }
    };
    let (stream, absolute_form) = match selected_proxy {
        None => {
            let stream = tcp::connect_target(
                target_host,
                target_port,
                options.resolved_addresses.as_deref(),
            )
            .await?;
            (Box::new(stream) as BoxedStream, false)
        }
        Some(proxy_url) => {
            let config = proxy::ProxyConfig::parse(&proxy_url)?;
            let proxy_socket = tcp::connect_target(config.host(), config.port(), None).await?;
            let mut proxy_stream: BoxedStream = Box::new(proxy_socket);
            if config.uses_tls() {
                proxy_stream = tls::wrap_tls(
                    proxy_stream,
                    config.host(),
                    config.port(),
                    fingerprint,
                    tls_verify,
                    true,
                    options.tls_session_cache.as_ref(),
                    tls::TlsPurpose::Proxy,
                    tls::TlsRoute::Direct,
                )
                .await?
                .stream;
            }

            match config.kind() {
                proxy::ProxyKind::Http if target_uses_tls || tunnel => {
                    let mut tunnel_stream = tokio::io::BufReader::new(proxy_stream);
                    proxy::establish_http_tunnel(
                        &mut tunnel_stream,
                        config.host(),
                        target_host,
                        target_port,
                        options.proxy_bearer_token.as_deref(),
                        options.proxy_auth.as_ref(),
                        config.url_auth(),
                    )
                    .await?;
                    (Box::new(tunnel_stream) as BoxedStream, false)
                }
                proxy::ProxyKind::Http => (proxy_stream, true),
                kind @ (proxy::ProxyKind::Socks4 { remote_dns }
                | proxy::ProxyKind::Socks5 { remote_dns }) => {
                    if options.proxy_bearer_token.is_some() {
                        return Err(TransportError::InvalidInput(
                            "Bearer authentication is not supported by SOCKS proxies".into(),
                        ));
                    }
                    if options
                        .proxy_auth
                        .as_ref()
                        .is_some_and(|auth| auth.scheme != crate::AuthScheme::Basic)
                    {
                        return Err(TransportError::InvalidInput(
                            "SOCKS username/password authentication requires Basic credentials"
                                .into(),
                        ));
                    }
                    let credentials = options
                        .proxy_auth
                        .as_ref()
                        .map(|auth| (auth.username.as_str(), auth.password.as_str()))
                        .or_else(|| config.url_credentials());
                    let socks4 = matches!(kind, proxy::ProxyKind::Socks4 { .. });
                    let resolved;
                    let destination = if remote_dns {
                        target_host
                    } else {
                        let addresses = match options.resolved_addresses.as_ref() {
                            Some(addresses) => addresses.clone(),
                            None => tokio::net::lookup_host((target_host, target_port))
                                .await
                                .map_err(|error| TransportError::Resolve(error.to_string()))?
                                .collect(),
                        };
                        let address = addresses
                            .into_iter()
                            .find(|address| !socks4 || address.is_ipv4())
                            .ok_or_else(|| {
                                TransportError::Resolve(
                                    "no approved address is supported by the SOCKS proxy".into(),
                                )
                            })?;
                        resolved = address.ip().to_string();
                        resolved.as_str()
                    };
                    if socks4 {
                        proxy::establish_socks4_tunnel(
                            proxy_stream.as_mut(),
                            destination,
                            target_port,
                            credentials.map(|(username, _)| username),
                        )
                        .await?;
                    } else {
                        proxy::establish_socks5_tunnel(
                            proxy_stream.as_mut(),
                            destination,
                            target_port,
                            credentials,
                        )
                        .await?;
                    }
                    (proxy_stream, false)
                }
            }
        }
    };

    if target_uses_tls {
        let mut connected = tls::wrap_tls(
            stream,
            target_host,
            target_port,
            fingerprint,
            tls_verify,
            http1_only,
            options.tls_session_cache.as_ref(),
            tls::TlsPurpose::Origin,
            tls_route,
        )
        .await?;
        connected.absolute_form = false;
        Ok(connected)
    } else {
        Ok(ConnectedStream {
            stream,
            version: Version::HTTP_11,
            absolute_form,
        })
    }
}

/// Selects the proxy for a URL. Explicit configuration wins over environment
/// variables, and an explicit empty proxy disables fallback. Uppercase
/// `HTTP_PROXY` is intentionally never trusted (CGI environments commonly
/// populate it from an attacker-controlled request header).
pub fn proxy_url(url: &Url, options: &ConnectionOptions) -> Result<Option<Url>, TransportError> {
    proxy_url_with_env(url, options, |name| env::var(name).ok())
}

fn proxy_url_with_env(
    url: &Url,
    options: &ConnectionOptions,
    mut get_env: impl FnMut(&str) -> Option<String>,
) -> Result<Option<Url>, TransportError> {
    let candidate = match options.proxy.as_deref() {
        Some("") => return Ok(None),
        Some(value) => Some(value.to_owned()),
        None => env_proxy(url.scheme(), &mut get_env),
    };
    let Some(candidate) = candidate.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };

    let host = url
        .host_str()
        .ok_or_else(|| TransportError::InvalidInput("connection URL is missing a host".into()))?;
    let port = url.port_or_known_default();
    let no_proxy = options
        .no_proxy
        .clone()
        .or_else(|| get_env("no_proxy").filter(|value| !value.is_empty()))
        .or_else(|| get_env("NO_PROXY").filter(|value| !value.is_empty()));
    if no_proxy_matches(host, port, no_proxy.as_deref()) {
        return Ok(None);
    }

    let parsed = Url::parse(&if candidate.contains("://") {
        candidate
    } else {
        format!("http://{candidate}")
    })
    .map_err(|_| TransportError::InvalidInput("invalid proxy URL".into()))?;
    proxy::ProxyConfig::parse(&parsed)?;
    Ok(Some(parsed))
}

fn env_proxy(scheme: &str, get_env: &mut impl FnMut(&str) -> Option<String>) -> Option<String> {
    let names: &[&str] = match scheme {
        "http" | "ws" => &["http_proxy"],
        "https" | "wss" => &["https_proxy", "HTTPS_PROXY"],
        _ => &[],
    };
    names
        .iter()
        .chain(["all_proxy", "ALL_PROXY"].iter())
        .find_map(|name| get_env(name).filter(|value| !value.is_empty()))
}

fn no_proxy_matches(host: &str, port: Option<u16>, no_proxy: Option<&str>) -> bool {
    let Some(no_proxy) = no_proxy else {
        return false;
    };
    let host = host
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_ascii_lowercase();
    no_proxy.split([',', ';', ' ']).any(|entry| {
        let entry = entry.trim();
        if entry.is_empty() {
            return false;
        }
        if entry == "*" {
            return true;
        }
        let (entry_host, entry_port) = split_no_proxy_host_port(entry);
        if entry_port.is_some_and(|candidate| Some(candidate) != port) {
            return false;
        }
        let entry_host = entry_host
            .trim_matches(['[', ']'])
            .trim_start_matches("*.")
            .trim_start_matches('.')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        !entry_host.is_empty()
            && (host == entry_host
                || host
                    .strip_suffix(&entry_host)
                    .is_some_and(|prefix| prefix.ends_with('.')))
    })
}

fn split_no_proxy_host_port(entry: &str) -> (&str, Option<u16>) {
    if entry.starts_with('[') {
        if let Some(end) = entry.find(']') {
            let host = &entry[1..end];
            let port = entry
                .get(end + 1..)
                .and_then(|suffix| suffix.strip_prefix(':'))
                .and_then(|value| value.parse().ok());
            return (host, port);
        }
        return (entry, None);
    }
    let Some((host, port)) = entry.rsplit_once(':') else {
        return (entry, None);
    };
    if host.contains(':') {
        return (entry, None);
    }
    match port.parse() {
        Ok(port) => (host, Some(port)),
        Err(_) => (entry, None),
    }
}
