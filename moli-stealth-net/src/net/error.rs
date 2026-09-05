use std::io;

/// The response returned by an HTTP proxy when a CONNECT tunnel cannot be
/// established. Header order and duplicate fields are preserved.
#[derive(Debug, Clone)]
pub struct ProxyResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Failures produced by the transport seam.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("name resolution failed: {0}")]
    Resolve(String),
    #[error("operation timed out")]
    Timeout,
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("certificate error: {0}")]
    Certificate(String),
    #[error("HTTP/1 error: {0}")]
    Http1(String),
    #[error("HTTP/2 error: {0}")]
    Http2(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("proxy CONNECT failed with status {status}", status = .0.status)]
    ProxyConnect(ProxyResponse),
    #[error("operation cancelled")]
    Cancelled,
    #[error("response exceeded its configured size limit")]
    TooLarge,
    #[error("authentication failed: {0}")]
    Authentication(String),
    #[error("connection closed before a response was received")]
    EmptyResponse,
}
