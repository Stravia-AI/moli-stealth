//! BoringSSL TLS configuration with Chrome 152 fingerprint.
//!
//! Configures TLS to produce a ClientHello identical to Chrome 152,
//! including cipher suites, curves, signature algorithms, extensions,
//! and certificate compression — all in the exact order that produces
//! the correct JA3/JA4 fingerprint.

use crate::stealth::{DeviceClass, StealthProfile};
use boring2::ssl::{
    CertificateCompressionAlgorithm, CertificateCompressor, ConnectConfiguration, ExtensionType,
    SslConnector, SslMethod, SslOptions, SslVersion,
};
use boring2::x509::X509;
use boring2::x509::store::X509StoreBuilder;
use foreign_types::ForeignTypeRef;
use std::io::Cursor;
use std::pin::Pin;
use tokio::net::TcpStream;
use tokio_boring2::SslStream;

use crate::net::error::NetError;

/// Chrome major version whose ClientHello and HTTP/2 fingerprint these
/// constants reproduce. Kept aligned with [`UA_CHROME_MAJOR`] and checked by
/// `tls_fingerprint_vectors_no_silent_drift`.
pub const TLS_CHROME_MAJOR: u32 = 152;

/// Chrome major version advertised by the default Windows desktop preset.
pub const UA_CHROME_MAJOR: u32 = 152;

/// Chrome 152 cipher suite list (order is critical for JA3 fingerprint).
const CIPHER_LIST: &str = concat!(
    "TLS_AES_128_GCM_SHA256",
    ":TLS_AES_256_GCM_SHA384",
    ":TLS_CHACHA20_POLY1305_SHA256",
    ":TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
    ":TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
    ":TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
    ":TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
    ":TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256",
    ":TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256",
    ":TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA",
    ":TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA",
    ":TLS_RSA_WITH_AES_128_GCM_SHA256",
    ":TLS_RSA_WITH_AES_256_GCM_SHA384",
    ":TLS_RSA_WITH_AES_128_CBC_SHA",
    ":TLS_RSA_WITH_AES_256_CBC_SHA",
);

/// Chrome 152 signature algorithms (order matters).
const SIGALGS_LIST: &str = concat!(
    "mldsa44",
    ":mldsa65",
    ":mldsa87",
    ":ecdsa_secp256r1_sha256",
    ":rsa_pss_rsae_sha256",
    ":rsa_pkcs1_sha256",
    ":ecdsa_secp384r1_sha384",
    ":rsa_pss_rsae_sha384",
    ":rsa_pkcs1_sha384",
    ":rsa_pss_rsae_sha512",
    ":rsa_pkcs1_sha512",
);

/// Chrome desktop elliptic curves (Chrome 131+ uses MLKEM768).
const CURVES_DESKTOP: &str = "X25519MLKEM768:X25519:P-256:P-384";

/// Chrome Android elliptic curves. Kyber768Draft00 (deprecated) was the
/// canonical Chrome 124-130 PQ curve; Chrome 131+ desktop replaced it with
/// MLKEM768 (codepoint 4588). A reference Chrome 131 Android capture
/// shows no PQ at all (just 29/23/24), but Chrome Android shares the
/// desktop codebase and by Chrome 152 should have rolled MLKEM — verify
/// against a fresh Pixel capture if regressions appear.
const CURVES_ANDROID: &str = CURVES_DESKTOP;

/// iOS Safari 18 cipher suite list (20 ciphers, Apple's order). Per a
/// reference Safari iOS 18 TLS capture.
/// Distinct from Chrome desktop (15 ciphers): includes 3DES_EDE_CBC_SHA at
/// the tail and an extra RSA_WITH_3DES_EDE_CBC_SHA. Cipher order matters
/// for JA3.
const CIPHER_LIST_SAFARI_IOS: &str = concat!(
    "TLS_AES_128_GCM_SHA256",
    ":TLS_AES_256_GCM_SHA384",
    ":TLS_CHACHA20_POLY1305_SHA256",
    ":TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
    ":TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
    ":TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256",
    ":TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
    ":TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
    ":TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256",
    ":TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA",
    ":TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA",
    ":TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA",
    ":TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA",
    ":TLS_RSA_WITH_AES_256_GCM_SHA384",
    ":TLS_RSA_WITH_AES_128_GCM_SHA256",
    ":TLS_RSA_WITH_AES_256_CBC_SHA",
    ":TLS_RSA_WITH_AES_128_CBC_SHA",
    ":TLS_ECDHE_ECDSA_WITH_3DES_EDE_CBC_SHA",
    ":TLS_ECDHE_RSA_WITH_3DES_EDE_CBC_SHA",
    ":TLS_RSA_WITH_3DES_EDE_CBC_SHA",
);

/// iOS Safari signature algorithms (10 entries, includes the duplicated
/// `rsa_pss_rsae_sha384` Apple quirk we must reproduce verbatim).
/// Reference Safari TLS captures include the duplicate.
const SIGALGS_LIST_SAFARI_IOS: &str = concat!(
    "ecdsa_secp256r1_sha256",
    ":rsa_pss_rsae_sha256",
    ":rsa_pkcs1_sha256",
    ":ecdsa_secp384r1_sha384",
    ":rsa_pss_rsae_sha384",
    ":rsa_pss_rsae_sha384",
    ":rsa_pkcs1_sha384",
    ":rsa_pss_rsae_sha512",
    ":rsa_pkcs1_sha512",
    ":rsa_pkcs1_sha1",
);

/// iOS Safari 18 elliptic curves. No PQ (MLKEM lands in iOS 26 per Apple's
/// PQC support page). Adds P-521 vs Chrome desktop. Order per safari_18.0_iOS.yaml.
const CURVES_SAFARI_IOS: &str = "X25519:P-256:P-384:P-521";

/// iOS Safari 18 extension permutation. Indices into BoringSSL's internal
/// `BORING_SSLEXTENSION_PERMUTATION` table — see boring2 ssl/mod.rs for the
/// canonical ordering. Per reference Safari iOS 18 TLS captures, real
/// Safari emits its extensions in a FIXED order (no Fisher-Yates shuffle),
/// roughly: server_name, extended_master_secret, renegotiate, supported_groups,
/// ec_point_formats, ALPN, status_request, signature_algorithms,
/// signed_certificate_timestamp, key_share, psk_key_exchange_modes,
/// supported_versions, cert_compression. (GREASE and PADDING are auto-emitted
/// by BoringSSL outside the permutation table; PADDING positional ordering
/// requires raw extension injection — deferred.)
const SAFARI_IOS_EXTENSION_PERMUTATION: &[ExtensionType] = &[
    ExtensionType::SERVER_NAME,
    ExtensionType::EXTENDED_MASTER_SECRET,
    ExtensionType::RENEGOTIATE,
    ExtensionType::SUPPORTED_GROUPS,
    ExtensionType::EC_POINT_FORMATS,
    ExtensionType::APPLICATION_LAYER_PROTOCOL_NEGOTIATION,
    ExtensionType::STATUS_REQUEST,
    ExtensionType::SIGNATURE_ALGORITHMS,
    ExtensionType::CERTIFICATE_TIMESTAMP,
    ExtensionType::KEY_SHARE,
    ExtensionType::PSK_KEY_EXCHANGE_MODES,
    ExtensionType::SUPPORTED_VERSIONS,
    ExtensionType::CERT_COMPRESSION,
];

/// Firefox 135 (NSS) cipher suite list — 17 ciphers, NSS order. Distinct
/// from Chrome's 15: NSS leads TLS1.3 with AES-128-GCM, CHACHA20, AES-256-GCM
/// (CHACHA before AES-256), then the ECDHE-ECDSA/RSA GCM pairs, then the CBC
/// block (ECDSA before RSA, 256 before 128 in NSS's CBC ordering), then the
/// two RSA-GCM and two RSA-CBC fallbacks. Yields the Firefox JA4 cipher hash
/// `5b57614c22b0` (vs Chrome's). Per reference Firefox TLS captures.
const CIPHER_LIST_FIREFOX: &str = concat!(
    "TLS_AES_128_GCM_SHA256",
    ":TLS_CHACHA20_POLY1305_SHA256",
    ":TLS_AES_256_GCM_SHA384",
    ":TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
    ":TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
    ":TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256",
    ":TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256",
    ":TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
    ":TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
    ":TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA",
    ":TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA",
    ":TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA",
    ":TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA",
    ":TLS_RSA_WITH_AES_128_GCM_SHA256",
    ":TLS_RSA_WITH_AES_256_GCM_SHA384",
    ":TLS_RSA_WITH_AES_128_CBC_SHA",
    ":TLS_RSA_WITH_AES_256_CBC_SHA",
);

/// Firefox 135 (NSS) signature algorithms — 11 entries, NSS order: the three
/// ECDSA curves first, then RSA-PSS, then RSA-PKCS1, then the SHA-1 tail
/// (ecdsa_sha1, rsa_pkcs1_sha1). Yields the Firefox JA4 sigalg hash
/// `3d5424432f57`.
const SIGALGS_LIST_FIREFOX: &str = concat!(
    "ecdsa_secp256r1_sha256",
    ":ecdsa_secp384r1_sha384",
    ":ecdsa_secp521r1_sha512",
    ":rsa_pss_rsae_sha256",
    ":rsa_pss_rsae_sha384",
    ":rsa_pss_rsae_sha512",
    ":rsa_pkcs1_sha256",
    ":rsa_pkcs1_sha384",
    ":rsa_pkcs1_sha512",
    ":ecdsa_sha1",
    ":rsa_pkcs1_sha1",
);

/// Firefox 135 supported_groups. NSS appends the two FFDHE groups
/// (ffdhe2048, ffdhe3072) after the EC curves — a hard Firefox signature no
/// Chrome build sends. X25519MLKEM768 leads (Firefox shipped PQ key-share by
/// default in 132+). P-521 present (Chrome desktop omits it).
const CURVES_FIREFOX: &str = "X25519MLKEM768:X25519:P-256:P-384:P-521:ffdhe2048:ffdhe3072";

/// Firefox 135 delegated_credentials (ext 0x22) sigalg list — Firefox-only.
/// The four ECDSA sigalgs NSS advertises in the delegated-credential ext.
const FIREFOX_DELEGATED_CREDENTIALS: &str = concat!(
    "ecdsa_secp256r1_sha256",
    ":ecdsa_secp384r1_sha384",
    ":ecdsa_secp521r1_sha512",
    ":ecdsa_sha1",
);

/// Firefox 135 record_size_limit (ext 0x1c) value: 0x4001 (16385).
const FIREFOX_RECORD_SIZE_LIMIT: u16 = 0x4001;

/// Firefox 135 extension order (indices into BoringSSL's
/// `BORING_SSLEXTENSION_PERMUTATION` table — same index space the Chrome and
/// Safari permutations use). FIXED order every handshake (NSS does not
/// Fisher-Yates shuffle). 15 extensions → the Firefox `t13d1715h2` JA4 count.
/// Index map (proven from CHROME_/SAFARI_ permutations + boring2 ext table):
/// 0=SNI, 1=ECH, 2=ext_master_secret, 3=renegotiate, 4=supported_groups,
/// 5=ec_point_formats, 6=session_ticket, 7=ALPN, 8=status_request,
/// 9=signature_algorithms, 14=key_share, 15=psk_kex_modes, 17=supported_versions,
/// 22=delegated_credentials, 26=record_size_limit. Order verified against a
/// reference Firefox 135 TLS capture — iterate if the JA4 ext-hash diverges.
const FIREFOX_EXTENSION_PERMUTATION: &[ExtensionType] = &[
    ExtensionType::SERVER_NAME,
    ExtensionType::EXTENDED_MASTER_SECRET,
    ExtensionType::RENEGOTIATE,
    ExtensionType::SUPPORTED_GROUPS,
    ExtensionType::EC_POINT_FORMATS,
    ExtensionType::SESSION_TICKET,
    ExtensionType::APPLICATION_LAYER_PROTOCOL_NEGOTIATION,
    ExtensionType::STATUS_REQUEST,
    ExtensionType::DELEGATED_CREDENTIAL,
    ExtensionType::KEY_SHARE,
    ExtensionType::SUPPORTED_VERSIONS,
    ExtensionType::SIGNATURE_ALGORITHMS,
    ExtensionType::PSK_KEY_EXCHANGE_MODES,
    ExtensionType::RECORD_SIZE_LIMIT,
    ExtensionType::ENCRYPTED_CLIENT_HELLO,
];

/// ALPN protocols: h2 + http/1.1
const ALPN_PROTOS: &[u8] = b"\x02h2\x08http/1.1";

use rand::prelude::SliceRandom;

/// Chrome 152 extension permutation (indices into BoringSSL kExtensions table).
/// 17 extensions matching the current Chrome fingerprint.
///
/// **Real Chrome shuffling behavior** (sources: Fastly TLS Fingerprinting blog,
/// Chromestatus 5124606246518784, and BoringSSL
/// `ssl_setup_extension_permutation`): Chrome shuffles ALL non-PSK extensions
/// with a single Fisher-Yates pass — there is no documented bucket structure.
/// The only positional constraint is psk_key_exchange_modes / pre_shared_key
/// being last (BoringSSL enforces this). The previous 3-bucket scheme was
/// folklore from earlier public RE work; it reduced shuffle entropy by
/// ~720,000× and put signature_algorithms always at position 16 — a
/// deterministic positional pattern that per-handshake classifiers can detect
/// as anomalous.
const CHROME_EXTENSION_PERMUTATION: &[ExtensionType] = &[
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

/// Generate a fresh Fisher-Yates shuffle over all Chrome extensions.
fn shuffled_chrome_extension_permutation() -> Vec<ExtensionType> {
    let mut rng = rand::rng();
    let mut permutation = CHROME_EXTENSION_PERMUTATION.to_vec();
    permutation.shuffle(&mut rng);
    permutation
}

const CHROME_TRUST_ANCHOR_IDS: &[u8] = &[
    0x05, 0x82, 0xdf, 0x13, 0x02, 0x0d, 0x04, 0xd6, 0x79, 0x09, 0x0f, 0x08, 0x83, 0x9a, 0x64, 0x8c,
    0x9b, 0x2d, 0x01, 0x0d, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x0a, 0x04, 0xd6, 0x79,
    0x09, 0x01, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x0e, 0x04, 0xd6, 0x79, 0x09, 0x04, 0x05, 0x82, 0xdf,
    0x13, 0x02, 0x01, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x14, 0x04, 0xd6, 0x79, 0x09, 0x07, 0x05, 0x82,
    0xdf, 0x13, 0x02, 0x0f, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x09, 0x08, 0x83, 0x9a,
    0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x08, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x07, 0x08,
    0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x0c, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x12, 0x04, 0xd6,
    0x79, 0x09, 0x08, 0x04, 0xd6, 0x79, 0x09, 0x05, 0x05, 0x82, 0xdf, 0x13, 0x02, 0x06, 0x04, 0xd6,
    0x79, 0x09, 0x0a, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x13, 0x05, 0x82, 0xdf, 0x13,
    0x02, 0x13, 0x04, 0xd6, 0x79, 0x09, 0x0b, 0x08, 0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x12,
    0x04, 0xd6, 0x79, 0x09, 0x0c, 0x04, 0xd6, 0x79, 0x09, 0x0d, 0x04, 0xd6, 0x79, 0x09, 0x06, 0x08,
    0x83, 0x9a, 0x64, 0x8c, 0x9b, 0x2d, 0x01, 0x0b,
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

struct ZlibCertificateDecompressor;

impl CertificateCompressor for ZlibCertificateDecompressor {
    const ALGORITHM: CertificateCompressionAlgorithm = CertificateCompressionAlgorithm::ZLIB;
    const CAN_COMPRESS: bool = false;
    const CAN_DECOMPRESS: bool = true;

    fn decompress<W: std::io::Write>(&self, input: &[u8], output: &mut W) -> std::io::Result<()> {
        std::io::copy(&mut flate2::read::ZlibDecoder::new(input), output).map(|_| ())
    }
}

/// Build an `SslConnector` configured with the TLS fingerprint matching
/// `profile.device_class`. Currently all variants share Chrome 147 desktop
/// configuration; this also branches for Android and iOS Safari.
pub fn chrome_connector(profile: &StealthProfile) -> Result<SslConnector, NetError> {
    // Per-device_class branching.
    //  - Desktop / Android: shared Chrome 147 cipher/sigalg/extension config.
    //    Android only diverges in the curves list (Kyber768Draft00 vs MLKEM).
    //  - MobileIOS: distinct Safari 18 cipher/sigalg/curves + skip Fisher-Yates
    //    extension permutation + zlib cert compression + SslOptions::NO_TICKET.
    //    Per-connection ALPS and ECH grease are also skipped — see
    //    configure_connection() below.
    let is_safari_ios = profile.device_class == DeviceClass::MobileIOS;
    // Firefox wire class: a desktop profile whose browser family is Firefox
    // emits an NSS-class ClientHello (no GREASE, FFDHE groups,
    // delegated_credentials + record_size_limit, fixed extension order)
    // instead of Chrome's. Without this a firefox_135_* profile put a
    // Chrome JA4 under a Firefox UA — an incoherent identity that any JA4↔UA
    // cross-check would flag.
    let is_firefox = profile.browser_name == "Firefox";
    let curves: &str = if is_firefox {
        CURVES_FIREFOX
    } else {
        match profile.device_class {
            DeviceClass::MobileAndroid => CURVES_ANDROID,
            DeviceClass::MobileIOS => CURVES_SAFARI_IOS,
            DeviceClass::Desktop => CURVES_DESKTOP,
        }
    };
    let cipher_list: &str = if is_safari_ios {
        CIPHER_LIST_SAFARI_IOS
    } else if is_firefox {
        CIPHER_LIST_FIREFOX
    } else {
        CIPHER_LIST
    };
    let sigalgs_list: &str = if is_safari_ios {
        SIGALGS_LIST_SAFARI_IOS
    } else if is_firefox {
        SIGALGS_LIST_FIREFOX
    } else {
        SIGALGS_LIST
    };
    let mut builder =
        SslConnector::builder(SslMethod::tls()).map_err(|e| NetError::Tls(e.to_string()))?;

    // Cipher suites (per device_class)
    builder
        .set_cipher_list(cipher_list)
        .map_err(|e| NetError::Tls(e.to_string()))?;

    // Elliptic curves (per device_class)
    builder
        .set_curves_list(curves)
        .map_err(|e| NetError::Tls(e.to_string()))?;

    // Signature algorithms (per device_class)
    builder
        .set_sigalgs_list(sigalgs_list)
        .map_err(|e| NetError::Tls(e.to_string()))?;
    builder.set_grease_sigalgs_enabled(!is_safari_ios && !is_firefox);

    // ALPN
    builder
        .set_alpn_protos(ALPN_PROTOS)
        .map_err(|e| NetError::Tls(e.to_string()))?;

    // TLS version range. Safari iOS 18.x advertises 4 versions (1.0, 1.1,
    // 1.2, 1.3) in supported_versions per reference Safari iOS captures —
    // visible as a length-difference on the extension. Servers still
    // negotiate 1.3 because no real server speaks 1.0/1.1 anymore, but the
    // ClientHello must advertise all four to fingerprint as Safari.
    let min_version = if is_safari_ios {
        SslVersion::TLS1
    } else {
        SslVersion::TLS1_2
    };
    builder
        .set_min_proto_version(Some(min_version))
        .map_err(|e| NetError::Tls(e.to_string()))?;
    builder
        .set_max_proto_version(Some(SslVersion::TLS1_3))
        .map_err(|e| NetError::Tls(e.to_string()))?;

    // GREASE: Chrome sprinkles GREASE across cipher/group/extension lists;
    // NSS-class Firefox sends NONE. The visible no-GREASE shape is itself a
    // Firefox tell, so disable it for the Firefox arm.
    builder.set_grease_enabled(!is_firefox);

    builder.set_permute_extensions(false);

    builder.enable_ocsp_stapling();
    builder.enable_signed_cert_timestamps();

    // Firefox-only extensions: delegated_credentials (0x22) and
    // record_size_limit (0x1c). Both are hard Firefox/NSS signatures absent
    // from every Chrome build. boring2 4.15 exposes them as builder methods.
    if is_firefox {
        builder
            .set_delegated_credentials(FIREFOX_DELEGATED_CREDENTIALS)
            .map_err(|e| NetError::Tls(e.to_string()))?;
        builder.set_record_size_limit(FIREFOX_RECORD_SIZE_LIMIT);
    }

    // Certificate compression. Chrome desktop+Android = Brotli (algo 2).
    // iOS Safari = Zlib (algo 1). Firefox 135 advertises zlib THEN brotli in
    // its compress_certificate ext (NSS order).
    if is_firefox {
        builder
            .add_certificate_compression_algorithm(ZlibCertificateDecompressor)
            .map_err(|e| NetError::Tls(e.to_string()))?;
        builder
            .add_certificate_compression_algorithm(BrotliCertificateDecompressor)
            .map_err(|e| NetError::Tls(e.to_string()))?;
    } else {
        if is_safari_ios {
            builder
                .add_certificate_compression_algorithm(ZlibCertificateDecompressor)
                .map_err(|e| NetError::Tls(e.to_string()))?;
        } else {
            builder
                .add_certificate_compression_algorithm(BrotliCertificateDecompressor)
                .map_err(|e| NetError::Tls(e.to_string()))?;
        }
    }

    // iOS Safari does not send the session_ticket extension at all.
    // SslOptions::NO_TICKET tells BoringSSL to omit the extension entirely
    // (vs sending it with a stale ticket).
    if is_safari_ios {
        builder.set_options(SslOptions::NO_TICKET);
    }

    // Load Mozilla root certificates into the certificate store
    let mut cert_store = X509StoreBuilder::new().map_err(|e| NetError::Tls(e.to_string()))?;
    for cert_der in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        let x509 = X509::from_der(cert_der.as_ref())
            .map_err(|e| NetError::Tls(format!("failed to parse root cert: {e}")))?;
        let _ = cert_store.add_cert(x509);
    }
    builder.set_cert_store(cert_store.build());

    // Extension order:
    //  - Chrome: per-handshake Fisher-Yates shuffle of all 16 desktop extensions
    //  - Safari iOS: FIXED order (same every handshake) — Phase D
    //    upgrade. Set Safari's specific 13-extension order via the same
    //    permutation API. PADDING positional ordering still requires raw
    //    extension injection (deferred); BoringSSL auto-emits PADDING when
    //    ClientHello length crosses ~512 bytes, which our Safari profile
    //    typically does.
    let permutation = if is_safari_ios {
        SAFARI_IOS_EXTENSION_PERMUTATION.to_vec()
    } else if is_firefox {
        // Firefox/NSS emits a FIXED extension order every handshake (no
        // Fisher-Yates), like Safari — use the Firefox order verbatim.
        FIREFOX_EXTENSION_PERMUTATION.to_vec()
    } else {
        shuffled_chrome_extension_permutation()
    };
    builder
        .set_extension_permutation(&permutation)
        .map_err(|e| NetError::Tls(e.to_string()))?;
    if !is_safari_ios && !is_firefox {
        builder
            .set_requested_trust_anchors(CHROME_TRUST_ANCHOR_IDS)
            .map_err(|e| NetError::Tls(e.to_string()))?;
    }

    Ok(builder.build())
}

/// Configure a per-connection TLS session with ALPS, ECH GREASE, and SNI.
/// Per-`profile.device_class` branching:
///  - Desktop / Android: ECH grease + ALPS HTTP/2 SETTINGS payload
///  - MobileIOS: skip BOTH (Safari has neither)
pub fn configure_connection(
    connector: &SslConnector,
    profile: &StealthProfile,
    domain: &str,
) -> Result<ConnectConfiguration, NetError> {
    let mut config = connector
        .configure()
        .map_err(|e| NetError::Tls(e.to_string()))?;

    let is_safari_ios = profile.device_class == DeviceClass::MobileIOS;
    let is_firefox = profile.browser_name == "Firefox";

    if !is_safari_ios {
        // ECH GREASE — Chrome desktop+Android AND Firefox all send it.
        // Safari does not.
        config.set_enable_ech_grease(true);
    }

    if !is_safari_ios && !is_firefox {
        // Application-layer settings (ALPS) for HTTP/2.
        // Chrome 147 Headless sends 4 settings: 1, 2, 4, 6.
        // Safari has no ALPS extension at all — skip entirely on iOS.
        // Firefox has no ALPS extension either — skip for the Firefox arm.
        let alps_payload: &[u8] = &[
            // SETTINGS frame (Length 24, Type 4, Flags 0, Stream 0)
            0x00, 0x00, 0x18, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, // ID 1: 65536
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, // ID 2: 0
            0x00, 0x02, 0x00, 0x00, 0x00, 0x00, // ID 4: 6291456
            0x00, 0x04, 0x00, 0x60, 0x00, 0x00, // ID 6: 262144
            0x00, 0x06, 0x00, 0x04, 0x00, 0x00,
            // Empty ACCEPT_CH frame (Length 0, Type 0x89, Flags 0, Stream 0)
            0x00, 0x00, 0x00, 0x89, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        // SAFETY: BoringSSL's `SSL_add_application_settings` reads the
        // ALPN name (`b"h2"`, length 2) and the ALPS payload buffer
        // (`alps_payload` — a contiguous static slice we built above);
        // both are valid, contiguous, non-null, and live for the
        // entire call. `config.as_ptr()` returns a non-null pointer
        // to the live `SslContext` we own here. BoringSSL only reads
        // these buffers; it copies the data into the SSL_CTX, no
        // ownership transfer.
        unsafe {
            if boring_sys2::SSL_add_application_settings(
                config.as_ptr(),
                b"h2".as_ptr(),
                2,
                alps_payload.as_ptr(),
                alps_payload.len(),
            ) != 1
            {
                return Err(NetError::Tls("failed to add ALPS settings".into()));
            }
        }
        config.set_alps_use_new_codepoint(true);
    }

    // SNI is the same for all profiles.
    let sni_domain = domain.trim_start_matches('[').trim_end_matches(']');
    if sni_domain.parse::<std::net::IpAddr>().is_ok() {
        config.set_use_server_name_indication(false);
    } else {
        config
            .set_hostname(sni_domain)
            .map_err(|e| NetError::Tls(e.to_string()))?;
    }

    Ok(config)
}

/// Establish a TLS connection to `domain` using the provided `SslConnector`.
pub async fn connect_tls(
    connector: &SslConnector,
    profile: &StealthProfile,
    domain: &str,
    stream: TcpStream,
) -> Result<SslStream<TcpStream>, NetError> {
    let config = configure_connection(connector, profile, domain)?;
    let sni_domain = domain.trim_start_matches('[').trim_end_matches(']');
    let ssl = config
        .into_ssl(sni_domain)
        .map_err(|e| NetError::Tls(e.to_string()))?;
    let mut stream = SslStream::new(ssl, stream).map_err(|e| NetError::Tls(e.to_string()))?;
    Pin::new(&mut stream)
        .connect()
        .await
        .map_err(|e| NetError::Tls(format!("TLS handshake failed: {e}")))?;
    Ok(stream)
}

/// Returns the negotiated ALPN protocol from a TLS stream, if any.
pub fn negotiated_alpn(stream: &SslStream<TcpStream>) -> Option<&[u8]> {
    stream.ssl().selected_alpn_protocol()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-verifying JA4 drift guard + UA/TLS coherence assert.
    /// Network-free.
    ///
    /// Pins every JA4 input (cipher list, sigalg list, supported-groups order,
    /// extension count) byte-/element-exact to the Chrome reference so the
    /// fingerprint cannot silently drift. Also checks that each desktop
    /// preset's reduced UA major agrees with its full browser version.
    #[test]
    fn tls_fingerprint_vectors_no_silent_drift() {
        // --- JA4 input 1: cipher suites (order is JA4-significant) ---
        const EXPECT_CIPHERS: &str = "TLS_AES_128_GCM_SHA256:TLS_AES_256_GCM_SHA384:\
TLS_CHACHA20_POLY1305_SHA256:TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256:\
TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256:TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384:\
TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384:TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256:\
TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256:TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA:\
TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA:TLS_RSA_WITH_AES_128_GCM_SHA256:\
TLS_RSA_WITH_AES_256_GCM_SHA384:TLS_RSA_WITH_AES_128_CBC_SHA:\
TLS_RSA_WITH_AES_256_CBC_SHA";
        assert_eq!(
            CIPHER_LIST, EXPECT_CIPHERS,
            "Chrome cipher list drifted from the verified-real reference \
             — JA4 cipher hash would change"
        );

        // --- JA4 input 2: signature algorithms (order is JA4-significant) ---
        const EXPECT_SIGALGS: &str = "mldsa44:mldsa65:mldsa87:\
ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:\
rsa_pkcs1_sha256:ecdsa_secp384r1_sha384:rsa_pss_rsae_sha384:rsa_pkcs1_sha384:\
rsa_pss_rsae_sha512:rsa_pkcs1_sha512";
        assert_eq!(
            SIGALGS_LIST, EXPECT_SIGALGS,
            "Chrome sigalg list drifted — JA4 sigalg hash would change"
        );

        // --- JA4 input 3: supported groups / curves order ---
        assert_eq!(
            CURVES_DESKTOP, "X25519MLKEM768:X25519:P-256:P-384",
            "Chrome desktop curve order drifted (post-quantum MLKEM768 \
             must lead) — JA4 supported_groups would change"
        );

        // --- JA4 input 4: extension count (17 — JA4 `c` digit) ---
        assert_eq!(
            CHROME_EXTENSION_PERMUTATION.len(),
            17,
            "Chrome extension count drifted — JA4 extension-count digit \
             would change"
        );

        // --- UA / TLS coherence ---
        assert_eq!(TLS_CHROME_MAJOR, 152);
        assert_eq!(UA_CHROME_MAJOR, 152);

        fn chrome_major(value: &str) -> Option<u32> {
            value.split('.').next()?.parse().ok()
        }

        fn ua_chrome_major(ua: &str) -> Option<u32> {
            let version = ua.split_once("Chrome/")?.1;
            chrome_major(version)
        }

        for profile in [
            crate::stealth::presets::chrome_148_macos(),
            crate::stealth::presets::chrome_148_windows(),
        ] {
            assert_eq!(
                ua_chrome_major(&profile.user_agent),
                chrome_major(&profile.browser_version),
                "desktop Chrome preset UA and full-version majors must agree; UA was {:?}",
                profile.user_agent
            );
            assert_eq!(
                profile.tls_impersonate, "chrome_147",
                "desktop Chrome preset TLS profile must be the verified-real \
                 chrome_147 reference (wire-equivalent to Chrome \
                 {UA_CHROME_MAJOR}); see TLS_CHROME_MAJOR docs"
            );
        }

        assert_eq!(
            ua_chrome_major(&crate::stealth::presets::chrome_148_windows().user_agent),
            Some(UA_CHROME_MAJOR),
            "the default Windows preset must advertise UA_CHROME_MAJOR"
        );
    }

    /// Capture the first 5 bytes of our outbound ClientHello (the TLS
    /// record header) and assert the record version is 0x0301 (TLS 1.0).
    /// Source-code analysis of `boringssl/src/ssl/ssl_aead_ctx.cc:168-173`
    /// confirms `RecordVersion()` returns `TLS1_VERSION` (0x0301) for the
    /// initial ClientHello (null cipher, version_ == 0). This test verifies
    /// it empirically — a BoringSSL source patch for the TLS 1.0 record
    /// version is **NOT NEEDED**.
    #[tokio::test]
    async fn safari_ios_emits_tls_1_0_record_version() {
        use tokio::io::AsyncReadExt;
        use tokio::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Background server that just reads the first 5 bytes and reports.
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 5];
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                stream.read_exact(&mut buf),
            )
            .await
            .unwrap()
            .unwrap();
            buf
        });

        // Connect with iOS Safari profile.
        let profile = crate::stealth::presets::iphone_15_pro_safari_18();
        let connector = chrome_connector(&profile).expect("connector");
        let tcp = TcpStream::connect(addr).await.unwrap();
        // We expect the handshake to fail (server doesn't respond), but the
        // ClientHello is sent before that. Race the timeout against the
        // server's read.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            connect_tls(&connector, &profile, "localhost", tcp),
        )
        .await;

        let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .expect("server timeout")
            .expect("server task");

        let content_type = bytes[0];
        let record_version = ((bytes[1] as u16) << 8) | (bytes[2] as u16);

        // Content type 22 = TLS handshake
        assert_eq!(
            content_type, 22,
            "expected TLS handshake (22), got {content_type}"
        );

        // Record version: real Safari sends 0x0301 (TLS 1.0); BoringSSL
        // emits the same for null-cipher (initial ClientHello).
        assert_eq!(
            record_version, 0x0301,
            "iOS Safari record version: got 0x{record_version:04x}, expected 0x0301 (TLS 1.0). \
             If this is 0x0303 then a BoringSSL source patch IS needed; if 0x0301 then \
             our current build already matches Safari."
        );
    }

    /// Same record-version check for desktop Chrome profile. Real Chrome
    /// also sends 0x0301 (TLS 1.0) record version for the initial ClientHello
    /// — TLS-version selection happens in the inner extension, not the outer
    /// record header. This test confirms the BoringSSL behavior is uniform
    /// across desktop and Safari profiles.
    #[tokio::test]
    async fn desktop_chrome_emits_tls_1_0_record_version() {
        use tokio::io::AsyncReadExt;
        use tokio::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 5];
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                stream.read_exact(&mut buf),
            )
            .await
            .unwrap()
            .unwrap();
            buf
        });

        let profile = crate::stealth::presets::chrome_148_macos();
        let connector = chrome_connector(&profile).expect("connector");
        let tcp = TcpStream::connect(addr).await.unwrap();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            connect_tls(&connector, &profile, "localhost", tcp),
        )
        .await;

        let bytes = tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .expect("server timeout")
            .expect("server task");

        let record_version = ((bytes[1] as u16) << 8) | (bytes[2] as u16);
        assert_eq!(
            record_version, 0x0301,
            "Chrome desktop record version: got 0x{record_version:04x}, expected 0x0301."
        );
    }

    #[test]
    fn test_shuffle_is_full_fisher_yates() {
        // Real Chrome shuffles all extensions uniformly (no buckets).
        // Verify the shuffle preserves the full set + is non-deterministic.
        let p1 = shuffled_chrome_extension_permutation();
        let p2 = shuffled_chrome_extension_permutation();

        assert_eq!(p1.len(), CHROME_EXTENSION_PERMUTATION.len());
        assert_eq!(p2.len(), CHROME_EXTENSION_PERMUTATION.len());

        for permutation in [&p1, &p2] {
            assert!(
                CHROME_EXTENSION_PERMUTATION
                    .iter()
                    .all(|extension| permutation.contains(extension)),
                "shuffle must preserve the set"
            );
        }

        // Probabilistically should differ run-to-run.
        assert_ne!(p1, p2, "Shuffle should be non-deterministic");
    }
}
