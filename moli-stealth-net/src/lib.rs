//! BrowserOxide-derived Chrome transport behind a Moli-specific interface.
// Keep the imported BrowserOxide modules source-shaped so upstream transport
// updates remain reviewable even when Moli uses only the Chrome adapter.
#![allow(dead_code, unused_imports)]

mod net;
mod stealth;

use std::{net::SocketAddr, time::Duration};

use net::HttpClient;
use stealth::chrome_148_windows;
use thiserror::Error;
use url::Url;

/// Browser request methods supported by the fingerprinted transport.
#[derive(Debug, Clone, Copy)]
pub enum ChromeMethod<'a> {
    Get,
    Post(&'a [u8]),
}

/// One policy-approved request. DNS resolution remains owned by Moli so its
/// SSRF policy can validate and pin every address before this module connects.
pub struct ChromeRequest<'a> {
    pub url: &'a Url,
    pub method: ChromeMethod<'a>,
    pub headers: &'a [(String, String)],
    pub resolved_addresses: Vec<SocketAddr>,
    pub timeout: Duration,
}

/// Materialized response returned at the transport seam.
pub struct ChromeResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub url: String,
}

/// Synchronous adapter around BrowserOxide's async Chrome TLS/HTTP stack.
///
/// One instance owns connection pools for direct and proxied traffic. Callers
/// only provide validated addresses and browser-generated headers; TLS, HTTP/2,
/// cookies internal to the transport, and proxy bypass matching stay private.
pub struct ChromeTransport {
    runtime: tokio::runtime::Runtime,
    direct_client: HttpClient,
    proxied_client: Option<HttpClient>,
    no_proxy: NoProxy,
}

impl ChromeTransport {
    pub fn new(proxy: Option<&str>, no_proxy: Option<&str>) -> Result<Self, ChromeTransportError> {
        let mut profile = chrome_148_windows();
        let direct_client = HttpClient::new(&profile)?;
        profile.proxy = proxy.map(str::to_owned);
        let proxied_client = profile
            .proxy
            .as_ref()
            .map(|_| HttpClient::new(&profile))
            .transpose()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self {
            runtime,
            direct_client,
            proxied_client,
            no_proxy: NoProxy::parse(no_proxy),
        })
    }

    pub fn execute(
        &self,
        request: ChromeRequest<'_>,
    ) -> Result<ChromeResponse, ChromeTransportError> {
        let host = request
            .url
            .host_str()
            .ok_or_else(|| ChromeTransportError::MissingHost(request.url.to_string()))?;
        let client = if self.no_proxy.contains(host) {
            &self.direct_client
        } else {
            self.proxied_client.as_ref().unwrap_or(&self.direct_client)
        };
        let dns = client.dns_cache();
        let future = async {
            dns.insert_resolved(host, request.resolved_addresses).await;
            match request.method {
                ChromeMethod::Get => {
                    client
                        .get_with_exact_headers(request.url.as_str(), request.headers)
                        .await
                }
                ChromeMethod::Post(body) => {
                    client
                        .post_bytes_with_exact_headers(request.url.as_str(), body, request.headers)
                        .await
                }
            }
        };
        let response = if request.timeout.is_zero() {
            self.runtime.block_on(future)?
        } else {
            self.runtime
                .block_on(async { tokio::time::timeout(request.timeout, future).await })
                .map_err(|_| ChromeTransportError::Timeout {
                    timeout: request.timeout,
                    url: request.url.to_string(),
                })??
        };
        let mut headers: Vec<_> = response.headers.into_iter().collect();
        headers.extend(
            response
                .set_cookies
                .into_iter()
                .map(|value| ("set-cookie".to_owned(), value)),
        );
        Ok(ChromeResponse {
            status: response.status,
            headers,
            body: response.body,
            url: response.url,
        })
    }
}

#[derive(Debug, Error)]
pub enum ChromeTransportError {
    #[error(transparent)]
    Network(#[from] net::error::NetError),
    #[error("failed to create Chrome transport runtime: {0}")]
    Runtime(#[from] std::io::Error),
    #[error("request URL has no host: `{0}`")]
    MissingHost(String),
    #[error("Chrome transport timed out after {timeout:?} for {url}")]
    Timeout { timeout: Duration, url: String },
}

#[derive(Default)]
struct NoProxy(Vec<String>);

impl NoProxy {
    fn parse(value: Option<&str>) -> Self {
        Self(
            value
                .into_iter()
                .flat_map(|value| value.split([',', ';', ' ']))
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(|entry| {
                    entry
                        .trim_start_matches("*.")
                        .trim_start_matches('.')
                        .trim_end_matches('.')
                        .to_ascii_lowercase()
                })
                .collect(),
        )
    }

    fn contains(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.0
            .iter()
            .any(|entry| entry == "*" || host == *entry || host.ends_with(&format!(".{entry}")))
    }
}
