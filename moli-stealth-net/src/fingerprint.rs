use std::sync::{LazyLock, OnceLock};

use crate::TransportError;

pub const CHROME_REFERENCE_VERSION: &str = "152.0.7977.82";

const CHROME_CIPHER_LIST: &str = concat!(
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

const CHROME_SIGNATURE_ALGORITHMS: &str = concat!(
    "mldsa44:mldsa65:mldsa87:",
    "ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:rsa_pkcs1_sha256:",
    "ecdsa_secp384r1_sha384:rsa_pss_rsae_sha384:rsa_pkcs1_sha384:",
    "rsa_pss_rsae_sha512:rsa_pkcs1_sha512",
);

const CHROME_CURVES: &str = "X25519MLKEM768:X25519:P-256:P-384";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FingerprintPreset {
    Ordinary,
    Chrome152,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct TlsFingerprint {
    pub cipher_list: Option<String>,
    pub curves_list: Option<String>,
    pub signature_algorithms: Option<String>,
    pub alpn_protocols: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum H2Setting {
    HeaderTableSize,
    EnablePush,
    MaxConcurrentStreams,
    InitialWindowSize,
    MaxFrameSize,
    MaxHeaderListSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum H2PseudoHeader {
    Method,
    Authority,
    Scheme,
    Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct H2HeadersPriority {
    pub stream_dependency: u32,
    pub weight: u8,
    pub exclusive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct H2Fingerprint {
    pub header_table_size: Option<u32>,
    pub enable_push: Option<bool>,
    pub max_concurrent_streams: Option<u32>,
    pub initial_stream_window_size: Option<u32>,
    pub max_frame_size: Option<u32>,
    pub max_header_list_size: Option<u32>,
    pub connection_window_size: Option<u32>,
    pub settings_order: Vec<H2Setting>,
    pub pseudo_header_order: Vec<H2PseudoHeader>,
    /// 显式固定所有请求的 HEADERS 优先级；未设置时允许预设按请求语义选择。
    pub headers_priority: Option<H2HeadersPriority>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TransportFingerprint {
    pub preset: FingerprintPreset,
    pub tls: TlsFingerprint,
    pub h2: H2Fingerprint,
}

impl Default for TransportFingerprint {
    fn default() -> Self {
        Self {
            preset: FingerprintPreset::Ordinary,
            tls: TlsFingerprint::default(),
            h2: H2Fingerprint::default(),
        }
    }
}

impl TransportFingerprint {
    pub fn chrome() -> Self {
        Self {
            preset: FingerprintPreset::Chrome152,
            tls: TlsFingerprint {
                cipher_list: Some(CHROME_CIPHER_LIST.to_owned()),
                curves_list: Some(CHROME_CURVES.to_owned()),
                signature_algorithms: Some(CHROME_SIGNATURE_ALGORITHMS.to_owned()),
                alpn_protocols: Some(vec!["h2".to_owned(), "http/1.1".to_owned()]),
            },
            h2: H2Fingerprint {
                header_table_size: Some(65_536),
                enable_push: Some(false),
                max_concurrent_streams: None,
                initial_stream_window_size: Some(6_291_456),
                max_frame_size: None,
                max_header_list_size: Some(262_144),
                connection_window_size: Some(15_728_640),
                settings_order: vec![
                    H2Setting::HeaderTableSize,
                    H2Setting::EnablePush,
                    H2Setting::InitialWindowSize,
                    H2Setting::MaxHeaderListSize,
                ],
                pseudo_header_order: vec![
                    H2PseudoHeader::Method,
                    H2PseudoHeader::Authority,
                    H2PseudoHeader::Scheme,
                    H2PseudoHeader::Path,
                ],
                headers_priority: None,
            },
        }
    }

    pub fn validate(&self) -> Result<(), TransportError> {
        validate_h2(&self.h2)?;
        if !self.h2.settings_order.is_empty()
            && self
                .tls
                .alpn_protocols
                .as_ref()
                .is_some_and(|protocols| !protocols.iter().any(|protocol| protocol == "h2"))
        {
            return Err(invalid(
                "HTTP/2 fingerprint settings require h2 in the TLS ALPN protocol list",
            ));
        }
        crate::net::tls::validate_fingerprint(self)
    }

    pub fn set_h2_setting(&mut self, setting: H2Setting, value: Option<u32>) {
        match setting {
            H2Setting::HeaderTableSize => self.h2.header_table_size = value,
            H2Setting::EnablePush => self.h2.enable_push = value.map(|value| value != 0),
            H2Setting::MaxConcurrentStreams => self.h2.max_concurrent_streams = value,
            H2Setting::InitialWindowSize => self.h2.initial_stream_window_size = value,
            H2Setting::MaxFrameSize => self.h2.max_frame_size = value,
            H2Setting::MaxHeaderListSize => self.h2.max_header_list_size = value,
        }
        if value.is_some() && !self.h2.settings_order.contains(&setting) {
            self.h2.settings_order.push(setting);
        }
        if value.is_none() {
            self.h2
                .settings_order
                .retain(|candidate| *candidate != setting);
        }
    }
}

fn validate_h2(h2: &H2Fingerprint) -> Result<(), TransportError> {
    if h2
        .initial_stream_window_size
        .is_some_and(|value| value > 0x7fff_ffff)
    {
        return Err(invalid("HTTP/2 initial window size exceeds 2^31-1"));
    }
    if h2
        .connection_window_size
        .is_some_and(|value| !(65_535..=0x7fff_ffff).contains(&value))
    {
        return Err(invalid(
            "HTTP/2 connection window size must be between 65535 and 2^31-1",
        ));
    }
    if h2
        .max_frame_size
        .is_some_and(|value| !(16_384..=16_777_215).contains(&value))
    {
        return Err(invalid(
            "HTTP/2 max frame size must be between 16384 and 16777215",
        ));
    }
    if h2
        .headers_priority
        .is_some_and(|priority| priority.stream_dependency > 0x7fff_ffff)
    {
        return Err(invalid("HTTP/2 priority stream dependency exceeds 2^31-1"));
    }

    let mut seen = Vec::with_capacity(h2.settings_order.len());
    for setting in &h2.settings_order {
        if seen.contains(setting) {
            return Err(invalid(
                "HTTP/2 settings order contains a duplicate setting",
            ));
        }
        seen.push(*setting);
        let configured = match setting {
            H2Setting::HeaderTableSize => h2.header_table_size.is_some(),
            H2Setting::EnablePush => h2.enable_push.is_some(),
            H2Setting::MaxConcurrentStreams => h2.max_concurrent_streams.is_some(),
            H2Setting::InitialWindowSize => h2.initial_stream_window_size.is_some(),
            H2Setting::MaxFrameSize => h2.max_frame_size.is_some(),
            H2Setting::MaxHeaderListSize => h2.max_header_list_size.is_some(),
        };
        if !configured {
            return Err(invalid(
                "HTTP/2 settings order references an unconfigured setting",
            ));
        }
    }

    for (setting, configured) in [
        (H2Setting::HeaderTableSize, h2.header_table_size.is_some()),
        (H2Setting::EnablePush, h2.enable_push.is_some()),
        (
            H2Setting::MaxConcurrentStreams,
            h2.max_concurrent_streams.is_some(),
        ),
        (
            H2Setting::InitialWindowSize,
            h2.initial_stream_window_size.is_some(),
        ),
        (H2Setting::MaxFrameSize, h2.max_frame_size.is_some()),
        (
            H2Setting::MaxHeaderListSize,
            h2.max_header_list_size.is_some(),
        ),
    ] {
        if configured && !h2.settings_order.contains(&setting) {
            return Err(invalid(
                "configured HTTP/2 setting is absent from settings order",
            ));
        }
    }

    if !h2.pseudo_header_order.is_empty() {
        let required = [
            H2PseudoHeader::Method,
            H2PseudoHeader::Authority,
            H2PseudoHeader::Scheme,
            H2PseudoHeader::Path,
        ];
        if h2.pseudo_header_order.len() != required.len()
            || required
                .iter()
                .any(|item| !h2.pseudo_header_order.contains(item))
        {
            return Err(invalid(
                "HTTP/2 pseudo-header order must contain method, authority, scheme, and path exactly once",
            ));
        }
    }

    Ok(())
}

fn invalid(message: impl Into<String>) -> TransportError {
    TransportError::InvalidInput(message.into())
}

static ORDINARY_FINGERPRINT: LazyLock<TransportFingerprint> =
    LazyLock::new(TransportFingerprint::default);
static PROCESS_FINGERPRINT: OnceLock<TransportFingerprint> = OnceLock::new();

pub fn process_fingerprint() -> &'static TransportFingerprint {
    PROCESS_FINGERPRINT.get().unwrap_or(&ORDINARY_FINGERPRINT)
}

pub fn initialize_process_fingerprint(
    fingerprint: TransportFingerprint,
) -> Result<(), TransportError> {
    fingerprint.validate()?;
    if let Some(initialized) = PROCESS_FINGERPRINT.get() {
        return if initialized == &fingerprint {
            Ok(())
        } else {
            Err(invalid(
                "transport fingerprint is already initialized with a different process configuration",
            ))
        };
    }
    match PROCESS_FINGERPRINT.set(fingerprint) {
        Ok(()) => Ok(()),
        Err(attempted) if PROCESS_FINGERPRINT.get() == Some(&attempted) => Ok(()),
        Err(_) => Err(invalid(
            "transport fingerprint initialization raced with a different configuration",
        )),
    }
}
