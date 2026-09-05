//! Moli's asynchronous HTTP connection and transport library.
//!
//! TLS and HTTP fingerprinting code evolved from BrowserOxide; its repository
//! and imported revision remain recorded in this package's Cargo metadata.
//! This crate deliberately owns no browser cookie, redirect, cache, identity,
//! or authentication-challenge policy and creates no runtime of its own.

pub mod auth;
pub mod connection;
pub mod fingerprint;
mod net;

/// Receives typed metadata at the point each HTTP exchange is dispatched and answered.
///
/// Implementations must return promptly; callbacks run inline with transport progress.
pub trait TransportObserver: std::fmt::Debug + Send + Sync {
    fn request_sent(&self, headers: &[(String, String)]);
    fn response_received(&self, status: u16, headers: &[(String, String)]);
}

pub use auth::{AuthScheme, AuthSession, TransportAuth};
pub use connection::{BoxedStream, ConnectedStream, ConnectionOptions, IoStream, open_connection};
pub use fingerprint::{
    FingerprintPreset, H2Fingerprint, H2HeadersPriority, H2PseudoHeader, H2Setting, TlsFingerprint,
    TransportFingerprint, initialize_process_fingerprint, process_fingerprint,
};
pub use net::error::{ProxyResponse, TransportError};
pub use net::{
    ResponseBody, TlsSessionCache, Transport, TransportConfig, TransportRequest, TransportResponse,
};
