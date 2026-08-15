use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose};
use moli_crypto::{DigestAlgorithm, fill_secure_random};
use url::Url;

use crate::{
    key::WebBotAuthKey,
    profile::WebBotAuthProfile,
    wire::{SignatureOptions, sign_request},
};

const SIGNATURE_LABEL: &str = "sig1";
const SIGNATURE_TTL_SECS: u64 = 60;
const NONCE_BYTES: usize = 64;

#[derive(Clone)]
pub struct WebBotAuthSigner {
    inner: Arc<WebBotAuthSignerInner>,
}

struct WebBotAuthSignerInner {
    key: WebBotAuthKey,
    signature_agent_origin: String,
    profile: WebBotAuthProfile,
    nonce_seed: [u8; 32],
    nonce_counter: AtomicU64,
}

impl WebBotAuthSigner {
    pub fn from_pem(
        private_key_pem: &[u8],
        domain: &str,
        expected_keyid: Option<&str>,
        profile: WebBotAuthProfile,
    ) -> Result<Self> {
        let key = WebBotAuthKey::from_pem(private_key_pem, expected_keyid)?;
        let signature_agent_origin = normalize_signature_agent_origin(domain)?;
        let mut nonce_seed = [0_u8; 32];
        fill_secure_random(&mut nonce_seed)
            .map_err(|_| anyhow!("failed to initialize web bot auth nonce generation"))?;

        Ok(Self {
            inner: Arc::new(WebBotAuthSignerInner {
                key,
                signature_agent_origin,
                profile,
                nonce_seed,
                nonce_counter: AtomicU64::new(0),
            }),
        })
    }

    pub fn keyid(&self) -> &str {
        self.inner.key.keyid()
    }

    pub fn public_key(&self) -> &[u8; 32] {
        self.inner.key.public_key()
    }

    pub fn signature_agent_origin(&self) -> &str {
        &self.inner.signature_agent_origin
    }

    pub fn profile(&self) -> WebBotAuthProfile {
        self.inner.profile
    }

    pub fn append_request_headers(
        &self,
        headers: &mut Vec<(String, String)>,
        method: &str,
        request_url: &Url,
    ) -> Result<()> {
        if request_url.scheme() != "https" {
            return Ok(());
        }

        headers.retain(|(name, _)| !is_web_bot_auth_header(name));
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let signed = sign_request(
            &self.inner.key,
            &self.inner.signature_agent_origin,
            self.inner.profile,
            method,
            request_url,
            &SignatureOptions {
                signature_label: SIGNATURE_LABEL,
                signature_agent_label: SIGNATURE_LABEL,
                created,
                expires: created.saturating_add(SIGNATURE_TTL_SECS),
                nonce: self.next_nonce(),
                cover_request_target: true,
            },
        )?;
        headers.extend(signed.headers.into_pairs());
        Ok(())
    }

    fn next_nonce(&self) -> String {
        let counter = self.inner.nonce_counter.fetch_add(1, Ordering::Relaxed);
        let mut material = Vec::with_capacity(32 + 8 + 24);
        material.extend_from_slice(b"moli-web-bot-auth-nonce\0");
        material.extend_from_slice(&self.inner.nonce_seed);
        material.extend_from_slice(&counter.to_be_bytes());
        let nonce = DigestAlgorithm::Sha512.digest_bytes(&material);
        debug_assert_eq!(nonce.len(), NONCE_BYTES);
        general_purpose::STANDARD.encode(nonce)
    }
}

impl fmt::Debug for WebBotAuthSigner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebBotAuthSigner")
            .field("keyid", &self.inner.key.keyid())
            .field("signature_agent_origin", &self.inner.signature_agent_origin)
            .field("profile", &self.inner.profile)
            .finish_non_exhaustive()
    }
}

impl PartialEq for WebBotAuthSigner {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
            || (self.inner.key.public_key() == other.inner.key.public_key()
                && self.inner.signature_agent_origin == other.inner.signature_agent_origin
                && self.inner.profile == other.inner.profile)
    }
}

impl Eq for WebBotAuthSigner {}

fn normalize_signature_agent_origin(domain: &str) -> Result<String> {
    if domain.is_empty() || domain.trim() != domain {
        bail!("--web-bot-auth-domain must be a non-empty domain without surrounding whitespace");
    }
    if domain.contains(['/', '?', '#', '@']) {
        bail!(
            "--web-bot-auth-domain must contain only a host and optional port, without a scheme, credentials, path, query, or fragment"
        );
    }

    let url = Url::parse(&format!("https://{domain}"))
        .context("failed to parse --web-bot-auth-domain as an HTTPS origin")?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("--web-bot-auth-domain must identify a single HTTPS origin");
    }
    Ok(url.origin().ascii_serialization())
}

fn is_web_bot_auth_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("signature-agent")
        || name.eq_ignore_ascii_case("signature-input")
        || name.eq_ignore_ascii_case("signature")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EXPECTED_KEYID, RFC_9421_ED25519_PRIVATE_KEY, signer};

    #[test]
    fn normalizes_signature_agent_origin() {
        let signer = signer(WebBotAuthProfile::Cloudflare);

        assert_eq!(signer.keyid(), EXPECTED_KEYID);
        assert_eq!(
            signer.signature_agent_origin(),
            "https://signature-agent.test"
        );
    }

    #[test]
    fn production_requests_use_unique_nonces() {
        let signer = signer(WebBotAuthProfile::Cloudflare);
        let request_url = Url::parse("https://example.com/path").unwrap();
        let mut first_headers = Vec::new();
        signer
            .append_request_headers(&mut first_headers, "post", &request_url)
            .unwrap();
        let mut second_headers = Vec::new();
        signer
            .append_request_headers(&mut second_headers, "post", &request_url)
            .unwrap();

        let first_input = header_value(&first_headers, "signature-input");
        assert!(first_input.contains("(\"@authority\" \"@method\" \"@path\" \"signature-agent\")"));
        assert!(first_input.contains(";expires="));
        assert!(first_input.contains(";tag=\"web-bot-auth\""));
        assert_ne!(
            nonce_from_signature_input(first_input),
            nonce_from_signature_input(header_value(&second_headers, "signature-input"))
        );
    }

    #[test]
    fn omits_signatures_on_insecure_requests_and_replaces_spoofed_https_headers() {
        let signer = signer(WebBotAuthProfile::Cloudflare);
        let spoofed = vec![
            ("Signature-Agent".to_owned(), "spoofed".to_owned()),
            ("Signature-Input".to_owned(), "spoofed".to_owned()),
            ("Signature".to_owned(), "spoofed".to_owned()),
        ];
        let mut http_headers = spoofed.clone();
        signer
            .append_request_headers(
                &mut http_headers,
                "GET",
                &Url::parse("http://example.com/").unwrap(),
            )
            .unwrap();
        assert_eq!(http_headers, spoofed);

        let mut https_headers = spoofed;
        signer
            .append_request_headers(
                &mut https_headers,
                "GET",
                &Url::parse("https://example.com/").unwrap(),
            )
            .unwrap();
        assert_eq!(https_headers.len(), 3);
        assert!(https_headers.iter().all(|(_, value)| value != "spoofed"));
    }

    #[test]
    fn validates_domain() {
        for domain in [
            "",
            " example.com",
            "https://example.com",
            "user@example.com",
            "example.com/path",
            "example.com?query",
        ] {
            let error = WebBotAuthSigner::from_pem(
                RFC_9421_ED25519_PRIVATE_KEY.as_bytes(),
                domain,
                None,
                WebBotAuthProfile::Cloudflare,
            )
            .unwrap_err();
            assert!(error.to_string().contains("--web-bot-auth-domain"));
        }
    }

    #[test]
    fn debug_output_never_contains_private_key_material() {
        let signer = signer(WebBotAuthProfile::Cloudflare);
        let rendered = format!("{signer:?}");

        assert!(rendered.contains(EXPECTED_KEYID));
        assert!(!rendered.contains("IJ+DYvh6"));
        assert!(!rendered.contains("PRIVATE KEY"));
    }

    fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
        headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
            .unwrap()
    }

    fn nonce_from_signature_input(input: &str) -> &str {
        input
            .split(";nonce=\"")
            .nth(1)
            .and_then(|value| value.split('"').next())
            .unwrap()
    }
}
