//! Moli-owned asynchronous HTTP transport.
//!
//! The low-level TLS and HTTP fingerprint implementation is derived from
//! BrowserOxide (upstream revision recorded in Cargo metadata). Browser policy,
//! cookies, redirects, caching, and identity intentionally live above this seam.

pub(crate) mod error;
pub(crate) mod h1_client;
pub(crate) mod h2_client;
pub(crate) mod pool;
pub(crate) mod proxy;
pub(crate) mod tcp;
pub(crate) mod tls;

use std::{
    collections::hash_map::DefaultHasher,
    fmt,
    hash::{Hash, Hasher},
    sync::Arc,
};

use bytes::Bytes;

use self::{
    h1_client::{BodyFraming, H1Body},
    h2_client::H2Body,
    pool::{ConnectionPool, OriginKey, PooledH1, PooledH2},
};
use crate::{
    AuthSession, ConnectionOptions, TransportAuth, TransportError, TransportFingerprint,
    connection::{open_connection, proxy_url},
};

pub use tls::{TlsConfig, TlsSessionCache};

const MAX_AUTH_EXCHANGES: usize = 8;

#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub fingerprint: TransportFingerprint,
    pub tls_verify: bool,
    pub max_connections: Option<usize>,
    pub max_host_connections: Option<usize>,
    pub max_h2_streams: Option<usize>,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            fingerprint: TransportFingerprint::default(),
            tls_verify: true,
            max_connections: None,
            max_host_connections: None,
            max_h2_streams: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransportRequest {
    pub url: url::Url,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub connection: ConnectionOptions,
    pub auth: Option<TransportAuth>,
    pub http1_only: bool,
    /// 请求优先级事实；显式进程传输参数优先于该提示。
    pub h2_priority: Option<crate::H2HeadersPriority>,
    pub observer: Option<Arc<dyn crate::TransportObserver>>,
}

impl TransportRequest {
    pub fn new(url: url::Url, method: impl Into<String>) -> Self {
        Self {
            url,
            method: method.into(),
            headers: Vec::new(),
            body: None,
            connection: ConnectionOptions::default(),
            auth: None,
            http1_only: false,
            h2_priority: None,
            observer: None,
        }
    }
}

pub struct TransportResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub version: http::Version,
    pub body: ResponseBody,
    pub sent_headers: Vec<(String, String)>,
}

pub struct ResponseBody {
    inner: BodyInner,
}

enum BodyInner {
    Empty,
    H1(H1Body),
    H2(H2Body),
}

/// Cloneable access to one transport's physical connection limits.
///
/// Protocols that open dedicated sockets retain the returned permit for the
/// socket lifetime so HTTP and upgraded connections compete for one budget.
#[derive(Clone)]
pub struct ConnectionBudget {
    pool: ConnectionPool,
}

impl fmt::Debug for ConnectionBudget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionBudget")
            .finish_non_exhaustive()
    }
}

pub struct ConnectionPermit {
    _inner: pool::ConnectionPermits,
}

impl ConnectionBudget {
    pub async fn acquire(&self, url: &url::Url) -> Result<ConnectionPermit, TransportError> {
        let host = url
            .host_str()
            .ok_or_else(|| TransportError::InvalidInput("connection URL has no host".into()))?;
        self.pool
            .acquire(host)
            .await
            .map(|inner| ConnectionPermit { _inner: inner })
    }
}

impl ResponseBody {
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, TransportError> {
        match &mut self.inner {
            BodyInner::Empty => Ok(None),
            BodyInner::H1(body) => body.chunk().await,
            BodyInner::H2(body) => body.chunk().await,
        }
    }
}

#[derive(Clone)]
pub struct Transport {
    inner: Arc<TransportInner>,
}

struct TransportInner {
    config: TransportConfig,
    pool: ConnectionPool,
    tls_session_cache: TlsSessionCache,
}

impl Transport {
    pub fn new(config: TransportConfig) -> Result<Self, TransportError> {
        config.fingerprint.validate()?;
        if matches!(config.max_h2_streams, Some(0)) {
            return Err(TransportError::InvalidInput(
                "max_h2_streams must be greater than zero".into(),
            ));
        }
        let pool = ConnectionPool::new(config.max_connections, config.max_host_connections)?;
        Ok(Self {
            inner: Arc::new(TransportInner {
                config,
                pool,
                tls_session_cache: TlsSessionCache::new(),
            }),
        })
    }

    /// Returns the context-scoped TLS state owned by this transport.
    #[must_use]
    pub fn tls_session_cache(&self) -> TlsSessionCache {
        self.inner.tls_session_cache.clone()
    }

    /// Shares this transport's host and total physical-connection limits with
    /// protocols that own upgraded sockets outside the HTTP pool.
    #[must_use]
    pub fn connection_budget(&self) -> ConnectionBudget {
        ConnectionBudget {
            pool: self.inner.pool.clone(),
        }
    }

    /// Executes one approved request and resolves as soon as final response
    /// headers arrive. Dropping this future cancels in-flight connection/send
    /// work; dropping the returned body releases the corresponding H1
    /// connection or H2 stream without terminating sibling H2 streams.
    /// A stale pooled H1 connection is replaced at most once, and only for a
    /// safe request without a body before a response has been exposed.
    pub async fn execute(
        &self,
        mut request: TransportRequest,
    ) -> Result<TransportResponse, TransportError> {
        validate_request(&request)?;
        if request.connection.tls_session_cache.is_none() {
            request.connection.tls_session_cache = Some(self.tls_session_cache());
        }
        request
            .headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case("proxy-authorization"));
        let host = request.url.host_str().expect("validated host").to_owned();
        let request_target = request_target(&request.url);
        let mut auth_session = request
            .auth
            .as_ref()
            .map(|auth| AuthSession::new(auth, &host))
            .transpose()?;
        if matches!(
            request.auth.as_ref().map(|auth| auth.scheme),
            Some(crate::AuthScheme::Negotiate | crate::AuthScheme::Ntlm)
        ) {
            request.http1_only = true;
        }
        if matches!(
            request.auth.as_ref().map(|auth| auth.scheme),
            Some(crate::AuthScheme::Basic)
        ) && !request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            && let Some(value) = auth_session
                .as_mut()
                .expect("Basic auth initialized")
                .authorization(None, &request.method, &request_target)?
        {
            set_header(&mut request.headers, "authorization", value);
        }

        let selected_proxy = proxy_url(&request.url, &request.connection)?;
        let selected_proxy_config = selected_proxy
            .as_ref()
            .map(proxy::ProxyConfig::parse)
            .transpose()?;
        let is_forward_proxy = request.url.scheme() == "http"
            && selected_proxy_config
                .as_ref()
                .is_some_and(|config| config.kind() == proxy::ProxyKind::Http);
        let selected_proxy_auth = request
            .connection
            .proxy_auth
            .clone()
            .or_else(|| selected_proxy_config.as_ref()?.url_auth().cloned());
        let mut proxy_session = match (&selected_proxy_auth, selected_proxy.as_ref()) {
            (Some(auth), Some(proxy)) if is_forward_proxy => Some(AuthSession::new(
                auth,
                proxy
                    .host_str()
                    .ok_or_else(|| TransportError::InvalidInput("proxy URL has no host".into()))?,
            )?),
            _ => None,
        };
        let mut proxy_authorization = request
            .connection
            .proxy_bearer_token
            .as_ref()
            .map(|token| format!("Bearer {token}"));
        if is_forward_proxy
            && proxy_authorization.is_none()
            && matches!(
                selected_proxy_auth.as_ref().map(|auth| auth.scheme),
                Some(crate::AuthScheme::Basic)
            )
        {
            proxy_authorization = proxy_session
                .as_mut()
                .expect("Basic proxy auth initialized")
                .authorization(None, &request.method, request.url.as_str())?;
        }

        let key = origin_key(&request)?;
        let mut retained_h1 = None;
        for exchange in 0..MAX_AUTH_EXCHANGES {
            let request_body = if auth_session.is_some() || proxy_session.is_some() {
                request.body.clone()
            } else {
                request.body.take()
            };
            let result = self
                .execute_once(
                    &request,
                    &key,
                    retained_h1.take(),
                    request_body,
                    proxy_authorization.as_deref(),
                )
                .await?;
            if result.status() == 401
                && let Some(session) = auth_session.as_mut()
            {
                let Some(challenge) = crate::auth::select_challenge(
                    result.headers(),
                    request.auth.as_ref().expect("session requires auth").scheme,
                    "www-authenticate",
                ) else {
                    return Ok(result.into_public());
                };
                if exchange + 1 == MAX_AUTH_EXCHANGES {
                    return Ok(result.into_public());
                }
                let Some(authorization) =
                    session.authorization(Some(challenge), &request.method, &request_target)?
                else {
                    return Ok(result.into_public());
                };
                set_header(&mut request.headers, "authorization", authorization);
                retained_h1 = result.prepare_retry().await?;
                continue;
            }
            if result.status() == 407
                && is_forward_proxy
                && request.connection.proxy_bearer_token.is_none()
                && let Some(session) = proxy_session.as_mut()
            {
                let Some(challenge) = crate::auth::select_challenge(
                    result.headers(),
                    selected_proxy_auth
                        .as_ref()
                        .expect("session requires proxy auth")
                        .scheme,
                    "proxy-authenticate",
                ) else {
                    return Ok(result.into_public());
                };
                if exchange + 1 == MAX_AUTH_EXCHANGES {
                    return Ok(result.into_public());
                }
                let Some(authorization) = session.authorization(
                    Some(challenge),
                    &request.method,
                    request.url.as_str(),
                )?
                else {
                    return Ok(result.into_public());
                };
                proxy_authorization = Some(authorization);
                retained_h1 = result.prepare_retry().await?;
                continue;
            }
            return Ok(result.into_public());
        }
        unreachable!("bounded authentication loop returns")
    }

    async fn execute_once(
        &self,
        request: &TransportRequest,
        key: &OriginKey,
        retained_h1: Option<PooledH1>,
        body: Option<Vec<u8>>,
        proxy_authorization: Option<&str>,
    ) -> Result<ExchangeResponse, TransportError> {
        if let Some(connection) = retained_h1 {
            return self
                .execute_h1(
                    request,
                    key.clone(),
                    connection,
                    body.as_deref(),
                    proxy_authorization,
                )
                .await;
        }

        if !request.http1_only
            && let Some(connection) = self.inner.pool.get_h2(key).await
        {
            return self
                .execute_h2(request, key.clone(), connection, body)
                .await;
        }

        if let Some(connection) = self.inner.pool.take_h1(key).await {
            let result = self
                .execute_h1(
                    request,
                    key.clone(),
                    connection,
                    body.as_deref(),
                    proxy_authorization,
                )
                .await;
            match result {
                Err(error)
                    if body.is_none()
                        && matches!(
                            request.method.as_str(),
                            "GET" | "HEAD" | "OPTIONS" | "TRACE"
                        )
                        && is_stale_http1_error(&error) => {}
                result => return result,
            }
        }

        let h2_connection_gate = if request.http1_only {
            None
        } else {
            let gate = self.inner.pool.acquire_h2_connection_gate(key).await?;
            if let Some(connection) = self.inner.pool.get_h2(key).await {
                drop(gate);
                return self
                    .execute_h2(request, key.clone(), connection, body)
                    .await;
            }
            Some(gate)
        };

        let host = request.url.host_str().expect("validated host");
        let permits = self.inner.pool.acquire(host).await?;
        let connected = open_connection(
            &request.url,
            &request.connection,
            &self.inner.config.fingerprint,
            self.inner.config.tls_verify,
            request.http1_only,
            false,
        )
        .await?;

        if connected.version == http::Version::HTTP_2 && !request.http1_only {
            let (sender, driver) =
                h2_client::handshake(connected.stream, &self.inner.config.fingerprint).await?;
            tokio::spawn(async move {
                let _permits = permits;
                let _ = driver.await;
            });
            let connection = self
                .inner
                .pool
                .new_h2_connection(sender, self.inner.config.max_h2_streams);
            self.inner
                .pool
                .put_h2(key.clone(), connection.clone())
                .await;
            drop(h2_connection_gate);
            self.execute_h2(request, key.clone(), connection, body)
                .await
        } else if matches!(
            connected.version,
            http::Version::HTTP_10 | http::Version::HTTP_11
        ) {
            drop(h2_connection_gate);
            self.execute_h1(
                request,
                key.clone(),
                PooledH1 {
                    connected,
                    _permits: permits,
                },
                body.as_deref(),
                proxy_authorization,
            )
            .await
        } else {
            Err(TransportError::InvalidInput(format!(
                "connector negotiated unsupported protocol {:?}",
                connected.version
            )))
        }
    }

    async fn execute_h2(
        &self,
        request: &TransportRequest,
        key: OriginKey,
        connection: PooledH2,
        body: Option<Vec<u8>>,
    ) -> Result<ExchangeResponse, TransportError> {
        let stream_permit = connection.acquire_stream().await?;
        let priority = self
            .inner
            .config
            .fingerprint
            .h2
            .headers_priority
            .or(request.h2_priority)
            .or_else(|| {
                (self.inner.config.fingerprint.preset == crate::FingerprintPreset::Chrome152)
                    .then_some(crate::H2HeadersPriority {
                        stream_dependency: 0,
                        weight: 255,
                        exclusive: true,
                    })
            });
        match h2_client::send_request(
            connection.sender.clone(),
            request,
            body,
            stream_permit,
            priority,
        )
        .await
        {
            Ok(response) => Ok(ExchangeResponse::H2(response)),
            Err(failure) => {
                if failure.connection_unusable {
                    self.inner.pool.evict_h2(&key, &connection).await;
                }
                Err(failure.error)
            }
        }
    }

    async fn execute_h1(
        &self,
        request: &TransportRequest,
        key: OriginKey,
        mut connection: PooledH1,
        body: Option<&[u8]>,
        proxy_authorization: Option<&str>,
    ) -> Result<ExchangeResponse, TransportError> {
        let mut outgoing_headers;
        let headers = if connection.connected.absolute_form {
            outgoing_headers = request.headers.clone();
            if let Some(value) = proxy_authorization {
                set_header(
                    &mut outgoing_headers,
                    "proxy-authorization",
                    value.to_owned(),
                );
            }
            outgoing_headers.as_slice()
        } else {
            request.headers.as_slice()
        };
        let sent_headers =
            h1_client::write_request(&mut connection.connected, request, headers, body).await?;
        let head = h1_client::read_final_head(&mut connection.connected, &request.method).await?;
        let status = head.status;
        let headers = head.headers.clone();
        if let Some(observer) = &request.observer {
            observer.response_received(status, &headers);
        }
        let version = head.version;
        if matches!(head.framing, BodyFraming::Empty) {
            if head.reusable {
                self.inner.pool.put_h1(key, connection).await;
            }
            return Ok(ExchangeResponse::Complete(TransportResponse {
                status,
                headers,
                version,
                body: ResponseBody {
                    inner: BodyInner::Empty,
                },
                sent_headers,
            }));
        }
        Ok(ExchangeResponse::H1 {
            status,
            headers,
            version,
            body: H1Body::new(connection, self.inner.pool.clone(), key, head),
            sent_headers,
        })
    }
}

enum ExchangeResponse {
    Complete(TransportResponse),
    H1 {
        status: u16,
        headers: Vec<(String, String)>,
        version: http::Version,
        body: H1Body,
        sent_headers: Vec<(String, String)>,
    },
    H2(h2_client::H2Response),
}

impl ExchangeResponse {
    fn status(&self) -> u16 {
        match self {
            Self::Complete(response) => response.status,
            Self::H1 { status, .. } => *status,
            Self::H2(response) => response.status,
        }
    }

    fn headers(&self) -> &[(String, String)] {
        match self {
            Self::Complete(response) => &response.headers,
            Self::H1 { headers, .. } => headers,
            Self::H2(response) => &response.headers,
        }
    }

    async fn prepare_retry(self) -> Result<Option<PooledH1>, TransportError> {
        match self {
            Self::Complete(_) => Ok(None),
            Self::H1 { body, .. } => body.drain_for_retry().await,
            Self::H2(response) => {
                response.body.drain().await?;
                Ok(None)
            }
        }
    }

    fn into_public(self) -> TransportResponse {
        match self {
            Self::Complete(response) => response,
            Self::H1 {
                status,
                headers,
                version,
                body,
                sent_headers,
            } => TransportResponse {
                status,
                headers,
                version,
                body: ResponseBody {
                    inner: BodyInner::H1(body),
                },
                sent_headers,
            },
            Self::H2(response) => TransportResponse {
                status: response.status,
                headers: response.headers,
                version: http::Version::HTTP_2,
                body: ResponseBody {
                    inner: BodyInner::H2(response.body),
                },
                sent_headers: response.sent_headers,
            },
        }
    }
}

fn is_stale_http1_error(error: &TransportError) -> bool {
    match error {
        TransportError::EmptyResponse => true,
        TransportError::Io(error) => matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}

fn validate_request(request: &TransportRequest) -> Result<(), TransportError> {
    if !matches!(request.url.scheme(), "http" | "https") {
        return Err(TransportError::InvalidInput(format!(
            "unsupported request URL scheme `{}`",
            request.url.scheme()
        )));
    }
    if request.url.host_str().is_none() {
        return Err(TransportError::InvalidInput(
            "request URL has no host".into(),
        ));
    }
    http::Method::from_bytes(request.method.as_bytes()).map_err(|_| {
        TransportError::InvalidInput(format!("invalid HTTP method `{}`", request.method))
    })?;
    Ok(())
}

fn request_target(url: &url::Url) -> String {
    let mut target = if url.path().is_empty() {
        "/".to_owned()
    } else {
        url.path().to_owned()
    };
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    target
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    headers.retain(|(candidate, _)| !candidate.eq_ignore_ascii_case(name));
    headers.push((name.to_owned(), value));
}

fn origin_key(request: &TransportRequest) -> Result<OriginKey, TransportError> {
    let host = request.url.host_str().expect("validated host").to_owned();
    let mut hasher = DefaultHasher::new();
    request.url.scheme().hash(&mut hasher);
    host.hash(&mut hasher);
    request.url.port_or_known_default().hash(&mut hasher);
    proxy_url(&request.url, &request.connection)?
        .map(|url| url.as_str().to_owned())
        .hash(&mut hasher);
    request.connection.resolved_addresses.hash(&mut hasher);
    request.connection.tls.hash(&mut hasher);
    request
        .connection
        .tls_session_cache
        .as_ref()
        .map(TlsSessionCache::partition_id)
        .hash(&mut hasher);
    request.connection.proxy_bearer_token.hash(&mut hasher);
    if let Some(auth) = &request.connection.proxy_auth {
        std::mem::discriminant(&auth.scheme).hash(&mut hasher);
        auth.username.hash(&mut hasher);
        auth.password.hash(&mut hasher);
    }
    if let Some(auth) = &request.auth {
        std::mem::discriminant(&auth.scheme).hash(&mut hasher);
        auth.username.hash(&mut hasher);
        auth.password.hash(&mut hasher);
    }
    request.http1_only.hash(&mut hasher);
    Ok(OriginKey {
        route: format!("{:016x}", hasher.finish()),
        host,
    })
}
