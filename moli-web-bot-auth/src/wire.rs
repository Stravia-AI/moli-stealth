use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use url::{Host, Url};

use crate::{key::WebBotAuthKey, profile::WebBotAuthProfile};

pub(crate) struct SignatureOptions<'a> {
    pub(crate) signature_label: &'a str,
    pub(crate) signature_agent_label: &'a str,
    pub(crate) created: u64,
    pub(crate) expires: u64,
    pub(crate) nonce: String,
    pub(crate) cover_request_target: bool,
}

pub(crate) struct SignedRequest {
    pub(crate) headers: WebBotAuthHeaders,
    #[cfg(test)]
    pub(crate) signature_base: String,
}

pub(crate) struct WebBotAuthHeaders {
    pub(crate) signature_agent: String,
    pub(crate) signature_input: String,
    pub(crate) signature: String,
}

impl WebBotAuthHeaders {
    pub(crate) fn into_pairs(self) -> [(String, String); 3] {
        [
            ("Signature-Agent".to_owned(), self.signature_agent),
            ("Signature-Input".to_owned(), self.signature_input),
            ("Signature".to_owned(), self.signature),
        ]
    }
}

pub(crate) fn sign_request(
    key: &WebBotAuthKey,
    signature_agent_origin: &str,
    profile: WebBotAuthProfile,
    method: &str,
    request_url: &Url,
    options: &SignatureOptions<'_>,
) -> Result<SignedRequest> {
    let authority = request_authority(request_url)?;
    let method = method.to_ascii_uppercase();
    let path = request_url.path();
    let signature_agent = signature_agent_header(
        profile,
        signature_agent_origin,
        options.signature_agent_label,
    );
    let signature_agent_component =
        signature_agent_component_identifier(profile, options.signature_agent_label);

    let mut covered_components = vec!["\"@authority\"".to_owned()];
    if options.cover_request_target {
        covered_components.push("\"@method\"".to_owned());
        covered_components.push("\"@path\"".to_owned());
    }
    covered_components.push(signature_agent_component.clone());

    let signature_params = format!(
        "({});created={};keyid=\"{}\";alg=\"ed25519\";expires={};nonce=\"{}\";tag=\"web-bot-auth\"",
        covered_components.join(" "),
        options.created,
        key.keyid(),
        options.expires,
        options.nonce,
    );

    let mut signature_base_lines = vec![format!("\"@authority\": {authority}")];
    if options.cover_request_target {
        signature_base_lines.push(format!("\"@method\": {method}"));
        signature_base_lines.push(format!("\"@path\": {path}"));
    }
    signature_base_lines.push(format!(
        "{signature_agent_component}: \"{signature_agent_origin}\""
    ));
    signature_base_lines.push(format!("\"@signature-params\": {signature_params}"));
    let signature_base = signature_base_lines.join("\n");
    let signature = key.sign(signature_base.as_bytes())?;

    Ok(SignedRequest {
        headers: WebBotAuthHeaders {
            signature_agent,
            signature_input: format!("{}={signature_params}", options.signature_label),
            signature: format!(
                "{}=:{}:",
                options.signature_label,
                general_purpose::STANDARD.encode(signature)
            ),
        },
        #[cfg(test)]
        signature_base,
    })
}

fn signature_agent_header(
    profile: WebBotAuthProfile,
    signature_agent_origin: &str,
    label: &str,
) -> String {
    match profile {
        WebBotAuthProfile::Cloudflare => format!("\"{signature_agent_origin}\""),
        WebBotAuthProfile::IetfDraft01 => {
            format!("{label}=\"{signature_agent_origin}\"")
        }
    }
}

fn signature_agent_component_identifier(profile: WebBotAuthProfile, label: &str) -> String {
    match profile {
        WebBotAuthProfile::Cloudflare => "\"signature-agent\"".to_owned(),
        WebBotAuthProfile::IetfDraft01 => {
            format!("\"signature-agent\";key=\"{label}\"")
        }
    }
}

fn request_authority(request_url: &Url) -> Result<String> {
    let host = match request_url
        .host()
        .context("Web Bot Auth request URL must have an authority")?
    {
        Host::Domain(domain) => domain.to_owned(),
        Host::Ipv4(address) => address.to_string(),
        Host::Ipv6(address) => format!("[{address}]"),
    };
    Ok(match request_url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EXPECTED_KEYID, key};
    use aws_lc_rs::signature::{ED25519, UnparsedPublicKey};

    #[test]
    fn matches_ietf_draft_01_ed25519_test_vector() {
        let key = key();
        let request_url = Url::parse("https://example.com/foo?param=Value&Pet=dog").unwrap();
        let signed = sign_request(
            &key,
            "https://signature-agent.test",
            WebBotAuthProfile::IetfDraft01,
            "POST",
            &request_url,
            &SignatureOptions {
                signature_label: "sig2",
                signature_agent_label: "agent2",
                created: 1_735_689_600,
                expires: 4_889_289_600,
                nonce: "n9p433xm+NJ3ph3upfBIGmsuwHw387YV7Q/F+6BSpGCVjYCqQw6rznNA8PVVLySrAWsv0hQtFioQb6E1YsauiA==".to_owned(),
                cover_request_target: false,
            },
        )
        .unwrap();

        assert_eq!(key.keyid(), EXPECTED_KEYID);
        assert_eq!(
            signed.headers.signature_agent,
            "agent2=\"https://signature-agent.test\""
        );
        assert_eq!(
            signed.headers.signature_input,
            "sig2=(\"@authority\" \"signature-agent\";key=\"agent2\");created=1735689600;keyid=\"poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U\";alg=\"ed25519\";expires=4889289600;nonce=\"n9p433xm+NJ3ph3upfBIGmsuwHw387YV7Q/F+6BSpGCVjYCqQw6rznNA8PVVLySrAWsv0hQtFioQb6E1YsauiA==\";tag=\"web-bot-auth\""
        );
        assert_eq!(
            signed.headers.signature,
            "sig2=:RdNFx5Bj6au3YgAMQL/RzmUlZE8QZLIaXGRpw985hWnwPfMxT228NMk6ehRS1PSl4e8PhbNZACSanGdhEwYCCg==:"
        );
    }

    #[test]
    fn matches_cloudflare_legacy_ed25519_test_vector() {
        let key = key();
        let request_url = Url::parse("https://example.com/foo?param=Value&Pet=dog").unwrap();
        let signed = sign_request(
            &key,
            "https://signature-agent.test",
            WebBotAuthProfile::Cloudflare,
            "POST",
            &request_url,
            &SignatureOptions {
                signature_label: "sig2",
                signature_agent_label: "agent2",
                created: 1_735_689_600,
                expires: 1_735_693_200,
                nonce: "e8N7S2MFd/qrd6T2R3tdfAuuANngKI7LFtKYI/vowzk4lAZYadIX6wW25MwG7DCT9RUKAJ0qVkU0mEeLElW1qg==".to_owned(),
                cover_request_target: false,
            },
        )
        .unwrap();

        assert_eq!(
            signed.headers.signature_agent,
            "\"https://signature-agent.test\""
        );
        assert_eq!(
            signed.headers.signature,
            "sig2=:jdq0SqOwHdyHr9+r5jw3iYZH6aNGKijYp/EstF4RQTQdi5N5YYKrD+mCT1HA1nZDsi6nJKuHxUi/5Syp3rLWBA==:"
        );
    }

    #[test]
    fn production_components_cover_authority_method_and_path() {
        let key = key();
        let request_url = Url::parse("https://example.com:8443/a%20path?query=yes").unwrap();
        let signed = sign_request(
            &key,
            "https://signature-agent.test",
            WebBotAuthProfile::Cloudflare,
            "post",
            &request_url,
            &SignatureOptions {
                signature_label: "sig1",
                signature_agent_label: "sig1",
                created: 1_735_689_600,
                expires: 1_735_689_660,
                nonce: general_purpose::STANDARD.encode([7_u8; 64]),
                cover_request_target: true,
            },
        )
        .unwrap();

        assert!(
            signed
                .signature_base
                .contains("\"@authority\": example.com:8443")
        );
        assert!(signed.signature_base.contains("\"@method\": POST"));
        assert!(signed.signature_base.contains("\"@path\": /a%20path"));
        let signature = signed
            .headers
            .signature
            .strip_prefix("sig1=:")
            .and_then(|value| value.strip_suffix(':'))
            .and_then(|value| general_purpose::STANDARD.decode(value).ok())
            .unwrap();
        UnparsedPublicKey::new(&ED25519, key.public_key())
            .verify(signed.signature_base.as_bytes(), &signature)
            .unwrap();
    }

    #[test]
    fn signing_rejects_a_request_url_without_an_authority() {
        let key = key();
        let request_url = Url::parse("data:text/plain,no-authority").unwrap();
        let result = sign_request(
            &key,
            "https://signature-agent.test",
            WebBotAuthProfile::Cloudflare,
            "GET",
            &request_url,
            &SignatureOptions {
                signature_label: "sig1",
                signature_agent_label: "sig1",
                created: 1_735_689_600,
                expires: 1_735_689_660,
                nonce: general_purpose::STANDARD.encode([7_u8; 64]),
                cover_request_target: true,
            },
        );

        let error = match result {
            Ok(_) => panic!("signing a URL without an authority must fail"),
            Err(error) => error,
        };
        assert_eq!(
            error.to_string(),
            "Web Bot Auth request URL must have an authority"
        );
    }
}
