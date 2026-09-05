use std::{
    collections::VecDeque,
    fmt,
    io::Cursor,
    pin::Pin,
    sync::{Arc, LazyLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use boring2::{
    ex_data::Index,
    ssl::{
        CertificateCompressionAlgorithm, CertificateCompressor, ConnectConfiguration,
        ExtensionType, Ssl, SslConnector, SslMethod, SslSession, SslSessionCacheMode,
        SslVerifyMode, SslVersion,
    },
    x509::{X509, store::X509StoreBuilder},
};
use foreign_types::ForeignTypeRef;
use parking_lot::Mutex;
use rand::prelude::SliceRandom;
use tokio_boring2::SslStream;

use crate::{
    H2Setting, TransportError, TransportFingerprint,
    connection::{BoxedStream, ConnectedStream},
    fingerprint::FingerprintPreset,
};

const DEFAULT_ALPN: &[&str] = &["h2", "http/1.1"];
const MAX_CACHED_CONNECTORS: usize = 64;
const MAX_CACHED_SESSIONS: usize = 128;

/// Context-scoped owner for reusable TLS contexts and server-issued sessions.
///
/// Clones refer to the same cache. A newly-created handle is isolated from every
/// other handle, so callers can align its lifetime with a browser context.
#[derive(Clone)]
pub struct TlsSessionCache {
    inner: Arc<Mutex<TlsSessionCacheState>>,
}

impl TlsSessionCache {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TlsSessionCacheState::default())),
        }
    }

    pub(crate) fn partition_id(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    fn connector(&self, key: ConnectorKey) -> Result<SslConnector, TransportError> {
        if let Some(connector) = self
            .inner
            .lock()
            .connectors
            .iter()
            .find_map(|(candidate, connector)| (candidate == &key).then(|| connector.clone()))
        {
            return Ok(connector);
        }

        let connector = build_connector(
            &key.fingerprint,
            key.tls_verify,
            key.http1_only,
            Some(Arc::downgrade(&self.inner)),
        )?;
        let mut state = self.inner.lock();
        if let Some(existing) = state
            .connectors
            .iter()
            .find_map(|(candidate, connector)| (candidate == &key).then(|| connector.clone()))
        {
            return Ok(existing);
        }
        if state.connectors.len() == MAX_CACHED_CONNECTORS {
            state.connectors.pop_front();
        }
        state.connectors.push_back((key, connector.clone()));
        Ok(connector)
    }

    fn take_session(&self, key: &SessionKey) -> Option<SslSession> {
        let now = unix_time();
        let mut state = self.inner.lock();
        state
            .sessions
            .retain(|(_, session)| session_is_current(session, now));
        let index = state
            .sessions
            .iter()
            .rposition(|(candidate, _)| candidate == key)?;
        if state.sessions[index].1.should_be_single_use() {
            state.sessions.remove(index).map(|(_, session)| session)
        } else {
            Some(state.sessions[index].1.clone())
        }
    }
}

impl Default for TlsSessionCache {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TlsSessionCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TlsSessionCache { .. }")
    }
}

#[derive(Default)]
struct TlsSessionCacheState {
    connectors: VecDeque<(ConnectorKey, SslConnector)>,
    sessions: VecDeque<(SessionKey, SslSession)>,
}

// Connector contexts are shared across origins with identical immutable TLS
// settings. Per-connection origin/route data lives in SSL ex_data below.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ConnectorKey {
    fingerprint: TransportFingerprint,
    tls_verify: bool,
    http1_only: bool,
    purpose: TlsPurpose,
}

// ALPN mode is intentionally absent: TLS 1.3 tickets for one origin/security
// context may be offered by a later HTTPS (h2) or WSS (http/1.1) connection.
#[derive(Clone, PartialEq, Eq, Hash)]
struct SessionKey {
    host: String,
    port: u16,
    fingerprint: TransportFingerprint,
    tls_verify: bool,
    purpose: TlsPurpose,
    route: TlsRoute,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TlsPurpose {
    Origin,
    Proxy,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum TlsRoute {
    Direct,
    Proxy {
        scheme: String,
        host: String,
        port: u16,
    },
}

static TLS_SESSION_KEY_INDEX: LazyLock<Index<Ssl, SessionKey>> =
    LazyLock::new(|| Ssl::new_ex_index().expect("TLS session key ex-data index"));

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn session_is_current(session: &SslSession, now: u64) -> bool {
    let start = session.time();
    start <= now && start.saturating_add(u64::from(session.timeout())) > now
}

fn retain_new_session(
    cache: &Weak<Mutex<TlsSessionCacheState>>,
    key: &SessionKey,
    session: SslSession,
) {
    if !session_is_current(&session, unix_time()) {
        return;
    }
    let Some(cache) = cache.upgrade() else {
        return;
    };
    let mut state = cache.lock();
    if state.sessions.len() == MAX_CACHED_SESSIONS {
        state.sessions.pop_front();
    }
    state.sessions.push_back((key.clone(), session));
}

// Exact 32-ID desktop snapshot from
// `moli-cdp-smoke/evidence/transport/chrome-152-windows-direct.json`:
// Chrome 152.0.7977.82, revision d04cdb24d67b081f6cf80200ffc5233f44b61109.
// The fixture's repeated/proxied captures show that Chrome permutes these opaque IDs;
// this preset binds their measured membership, not an order shared by other browsers.
const CHROME_TRUST_ANCHOR_IDS: &[u8] = &[
    0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x13, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x01, 0x04,
    0xd6, 0x79, 0x09, 0x04, 0x04, 0xd6, 0x79, 0x09, 0x0c, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d,
    0x01, 0x12, 0x04, 0xd6, 0x79, 0x09, 0x0f, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x14, 0x04, 0xd6, 0x79,
    0x09, 0x07, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x0b, 0x04, 0xd6, 0x79, 0x09, 0x0a,
    0x04, 0xd6, 0x79, 0x09, 0x0e, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x06, 0x05, 0x82, 0xdf, 0x13, 0x02,
    0x13, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x0a, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b,
    0x2d, 0x01, 0x09, 0x04, 0xd6, 0x79, 0x09, 0x02, 0x04, 0xd6, 0x79, 0x09, 0x06, 0x08, 0x83, 0x9a,
    0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x08, 0x04, 0xd6, 0x79, 0x09, 0x09, 0x05, 0x82, 0xdf, 0x13, 0x02,
    0x0e, 0x04, 0xd6, 0x79, 0x09, 0x01, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x0d, 0x04, 0xd6, 0x79, 0x09,
    0x08, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x12, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x0f, 0x08, 0x83, 0x9a,
    0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x0d, 0x04, 0xd6, 0x79, 0x09, 0x0d, 0x08, 0x83, 0x9a, 0x64, 0x8c,
    0x9b, 0x2d, 0x01, 0x0c, 0x04, 0xd6, 0x79, 0x09, 0x0b, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d,
    0x01, 0x07, 0x04, 0xd6, 0x79, 0x09, 0x05, 0x04, 0xd6, 0x79, 0x09, 0x03,
];

const CHROME_EXTENSIONS: &[ExtensionType] = &[
    ExtensionType::KEY_SHARE,
    ExtensionType::ENCRYPTED_CLIENT_HELLO,
    ExtensionType::SUPPORTED_GROUPS,
    ExtensionType::CERTIFICATE_TIMESTAMP,
    ExtensionType::PSK_KEY_EXCHANGE_MODES,
    ExtensionType::EXTENDED_MASTER_SECRET,
    ExtensionType::APPLICATION_SETTINGS,
    ExtensionType::CERT_COMPRESSION,
    ExtensionType::SUPPORTED_VERSIONS,
    ExtensionType::SERVER_NAME,
    ExtensionType::RENEGOTIATE,
    ExtensionType::EC_POINT_FORMATS,
    ExtensionType::STATUS_REQUEST,
    ExtensionType::APPLICATION_LAYER_PROTOCOL_NEGOTIATION,
    ExtensionType::SESSION_TICKET,
    ExtensionType::SIGNATURE_ALGORITHMS,
    ExtensionType::TRUST_ANCHORS,
];

struct BrotliCertificateDecompressor;

impl CertificateCompressor for BrotliCertificateDecompressor {
    const ALGORITHM: CertificateCompressionAlgorithm = CertificateCompressionAlgorithm::BROTLI;
    const CAN_COMPRESS: bool = false;
    const CAN_DECOMPRESS: bool = true;

    fn decompress<W: std::io::Write>(&self, input: &[u8], output: &mut W) -> std::io::Result<()> {
        brotli::BrotliDecompress(&mut Cursor::new(input), output)
    }
}

pub(crate) fn validate_fingerprint(
    fingerprint: &TransportFingerprint,
) -> Result<(), TransportError> {
    build_connector(fingerprint, true, false, None).map(|_| ())
}

fn build_connector(
    fingerprint: &TransportFingerprint,
    tls_verify: bool,
    http1_only: bool,
    cache: Option<Weak<Mutex<TlsSessionCacheState>>>,
) -> Result<SslConnector, TransportError> {
    let mut builder = SslConnector::builder(SslMethod::tls()).map_err(tls_error)?;
    if let Some(cache) = cache {
        builder
            .set_session_cache_mode(SslSessionCacheMode::CLIENT | SslSessionCacheMode::NO_INTERNAL);
        builder.set_new_session_callback(move |ssl, session| {
            if let Some(key) = ssl.ex_data(*TLS_SESSION_KEY_INDEX) {
                retain_new_session(&cache, key, session);
            }
        });
    }

    if let Some(cipher_list) = fingerprint.tls.cipher_list.as_deref() {
        require_nonempty("TLS cipher list", cipher_list)?;
        builder.set_cipher_list(cipher_list).map_err(|error| {
            TransportError::InvalidInput(format!("invalid TLS cipher list: {error}"))
        })?;
    }
    if let Some(curves_list) = fingerprint.tls.curves_list.as_deref() {
        require_nonempty("TLS curves list", curves_list)?;
        builder.set_curves_list(curves_list).map_err(|error| {
            TransportError::InvalidInput(format!("invalid TLS curves list: {error}"))
        })?;
    }
    if let Some(signature_algorithms) = fingerprint.tls.signature_algorithms.as_deref() {
        require_nonempty("TLS signature algorithms", signature_algorithms)?;
        builder
            .set_sigalgs_list(signature_algorithms)
            .map_err(|error| {
                TransportError::InvalidInput(format!("invalid TLS signature algorithms: {error}"))
            })?;
    }

    let encoded_alpn = if http1_only {
        encode_alpn(&["http/1.1"])?
    } else if let Some(protocols) = fingerprint.tls.alpn_protocols.as_deref() {
        encode_alpn(protocols)?
    } else {
        encode_alpn(DEFAULT_ALPN)?
    };
    builder.set_alpn_protos(&encoded_alpn).map_err(|error| {
        TransportError::InvalidInput(format!("invalid TLS ALPN protocols: {error}"))
    })?;

    builder
        .set_min_proto_version(Some(SslVersion::TLS1_2))
        .map_err(tls_error)?;
    builder
        .set_max_proto_version(Some(SslVersion::TLS1_3))
        .map_err(tls_error)?;
    builder.set_verify(if tls_verify {
        SslVerifyMode::PEER
    } else {
        SslVerifyMode::NONE
    });

    let mut cert_store = X509StoreBuilder::new().map_err(tls_error)?;
    for certificate in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        let certificate = X509::from_der(certificate.as_ref()).map_err(|error| {
            TransportError::Certificate(format!("failed to parse trusted root: {error}"))
        })?;
        let _ = cert_store.add_cert(certificate);
    }
    builder.set_cert_store(cert_store.build());

    if fingerprint.preset == FingerprintPreset::Chrome152 {
        builder.set_grease_enabled(true);
        builder.set_grease_sigalgs_enabled(true);
        builder.set_permute_extensions(false);
        builder.enable_ocsp_stapling();
        builder.enable_signed_cert_timestamps();
        builder
            .add_certificate_compression_algorithm(BrotliCertificateDecompressor)
            .map_err(tls_error)?;
        let mut extensions = CHROME_EXTENSIONS.to_vec();
        extensions.shuffle(&mut rand::rng());
        builder
            .set_extension_permutation(&extensions)
            .map_err(tls_error)?;
        builder
            .set_requested_trust_anchors(CHROME_TRUST_ANCHOR_IDS)
            .map_err(tls_error)?;
    }

    Ok(builder.build())
}

fn require_nonempty(name: &str, value: &str) -> Result<(), TransportError> {
    if value.trim().is_empty() {
        Err(TransportError::InvalidInput(format!(
            "{name} must not be empty"
        )))
    } else {
        Ok(())
    }
}

fn encode_alpn<S: AsRef<str>>(protocols: &[S]) -> Result<Vec<u8>, TransportError> {
    if protocols.is_empty() {
        return Err(TransportError::InvalidInput(
            "TLS ALPN protocol list must not be empty".into(),
        ));
    }
    let mut encoded = Vec::new();
    let mut seen = Vec::with_capacity(protocols.len());
    for protocol in protocols {
        let protocol = protocol.as_ref();
        if !matches!(protocol, "h2" | "http/1.1") {
            return Err(TransportError::InvalidInput(format!(
                "unsupported TLS ALPN protocol `{protocol}`"
            )));
        }
        if seen.contains(&protocol) {
            return Err(TransportError::InvalidInput(format!(
                "duplicate TLS ALPN protocol `{protocol}`"
            )));
        }
        seen.push(protocol);
        encoded.push(protocol.len() as u8);
        encoded.extend_from_slice(protocol.as_bytes());
    }
    Ok(encoded)
}

fn configure_connection(
    connector: &SslConnector,
    fingerprint: &TransportFingerprint,
    domain: &str,
    tls_verify: bool,
    http1_only: bool,
) -> Result<ConnectConfiguration, TransportError> {
    let mut config = connector.configure().map_err(tls_error)?;
    let hostname = domain.trim_start_matches('[').trim_end_matches(']');
    let is_ip_address = hostname.parse::<std::net::IpAddr>().is_ok();
    config.set_verify_hostname(tls_verify);
    if is_ip_address {
        config.set_use_server_name_indication(false);
    } else {
        config.set_hostname(hostname).map_err(|error| {
            TransportError::InvalidInput(format!("invalid TLS server name `{hostname}`: {error}"))
        })?;
    }

    if fingerprint.preset == FingerprintPreset::Chrome152 {
        config.set_enable_ech_grease(true);
    }
    let advertises_h2 = fingerprint
        .tls
        .alpn_protocols
        .as_ref()
        .is_none_or(|protocols| protocols.iter().any(|protocol| protocol == "h2"));
    if fingerprint.preset == FingerprintPreset::Chrome152 && !http1_only && advertises_h2 {
        let alps = encode_alps(fingerprint)?;
        if !alps.is_empty() {
            // SAFETY: BoringSSL copies both buffers during this call. The SSL
            // configuration and both slices remain live for the complete call.
            unsafe {
                if boring_sys2::SSL_add_application_settings(
                    config.as_ptr(),
                    b"h2".as_ptr(),
                    2,
                    alps.as_ptr(),
                    alps.len(),
                ) != 1
                {
                    return Err(TransportError::Tls(
                        "failed to configure TLS application settings".into(),
                    ));
                }
            }
            config.set_alps_use_new_codepoint(true);
        }
    }
    Ok(config)
}

fn encode_alps(fingerprint: &TransportFingerprint) -> Result<Vec<u8>, TransportError> {
    if fingerprint.h2.settings_order.is_empty() {
        return Ok(Vec::new());
    }
    let payload_len = fingerprint.h2.settings_order.len() * 6;
    if payload_len > 0x00ff_ffff {
        return Err(TransportError::InvalidInput(
            "HTTP/2 settings list is too long for ALPS".into(),
        ));
    }
    let mut result = Vec::with_capacity(9 + payload_len + 9);
    result.extend_from_slice(&[
        ((payload_len >> 16) & 0xff) as u8,
        ((payload_len >> 8) & 0xff) as u8,
        (payload_len & 0xff) as u8,
        0x04,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
    ]);
    for setting in &fingerprint.h2.settings_order {
        let (identifier, value) = match setting {
            H2Setting::HeaderTableSize => (1, fingerprint.h2.header_table_size),
            H2Setting::EnablePush => (2, fingerprint.h2.enable_push.map(u32::from)),
            H2Setting::MaxConcurrentStreams => (3, fingerprint.h2.max_concurrent_streams),
            H2Setting::InitialWindowSize => (4, fingerprint.h2.initial_stream_window_size),
            H2Setting::MaxFrameSize => (5, fingerprint.h2.max_frame_size),
            H2Setting::MaxHeaderListSize => (6, fingerprint.h2.max_header_list_size),
        };
        let value = value.ok_or_else(|| {
            TransportError::InvalidInput(
                "HTTP/2 settings order references an unconfigured setting".into(),
            )
        })?;
        result.extend_from_slice(&(identifier as u16).to_be_bytes());
        result.extend_from_slice(&value.to_be_bytes());
    }
    // Chrome advertises an empty ACCEPT_CH frame after SETTINGS in ALPS.
    result.extend_from_slice(&[0x00, 0x00, 0x00, 0x89, 0x00, 0x00, 0x00, 0x00, 0x00]);
    Ok(result)
}

pub async fn wrap_tls(
    stream: BoxedStream,
    domain: &str,
    port: u16,
    fingerprint: &TransportFingerprint,
    tls_verify: bool,
    http1_only: bool,
    session_cache: Option<&TlsSessionCache>,
    purpose: TlsPurpose,
    route: TlsRoute,
) -> Result<ConnectedStream, TransportError> {
    let hostname = domain.trim_start_matches('[').trim_end_matches(']');
    let session_key = SessionKey {
        host: hostname.to_ascii_lowercase(),
        port,
        fingerprint: fingerprint.clone(),
        tls_verify,
        purpose,
        route,
    };
    let connector = match session_cache {
        Some(cache) => cache.connector(ConnectorKey {
            fingerprint: fingerprint.clone(),
            tls_verify,
            http1_only,
            purpose,
        })?,
        None => build_connector(fingerprint, tls_verify, http1_only, None)?,
    };
    let config = configure_connection(&connector, fingerprint, domain, tls_verify, http1_only)?;
    let mut ssl = config.into_ssl(hostname).map_err(tls_error)?;
    if session_cache.is_some() {
        ssl.set_ex_data(*TLS_SESSION_KEY_INDEX, session_key.clone());
    }
    if let Some(session) = session_cache.and_then(|cache| cache.take_session(&session_key)) {
        // SAFETY: the cached session was issued by BoringSSL for this exact
        // origin, fingerprint, verification policy, purpose, and proxy route.
        // The handshake has not started and `set_session` retains the session.
        unsafe { ssl.set_session(&session).map_err(tls_error)? };
    }
    let mut stream = SslStream::new(ssl, stream).map_err(tls_error)?;
    Pin::new(&mut stream).connect().await.map_err(|error| {
        if tls_verify
            && error
                .to_string()
                .to_ascii_lowercase()
                .contains("certificate")
        {
            TransportError::Certificate(error.to_string())
        } else {
            TransportError::Tls(format!("TLS handshake failed: {error}"))
        }
    })?;
    let version = if !http1_only && stream.ssl().selected_alpn_protocol() == Some(b"h2") {
        http::Version::HTTP_2
    } else {
        http::Version::HTTP_11
    };
    Ok(ConnectedStream {
        stream: Box::new(stream),
        version,
        absolute_form: false,
    })
}

fn tls_error(error: impl std::fmt::Display) -> TransportError {
    TransportError::Tls(error.to_string())
}
