use std::str;

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose};
use moli_crypto::{Ed25519SigningKey, sha256_digest};

pub(crate) struct WebBotAuthKey {
    signing_key: Ed25519SigningKey,
    public_key: [u8; 32],
    keyid: String,
}

impl WebBotAuthKey {
    pub(crate) fn from_pem(private_key_pem: &[u8], expected_keyid: Option<&str>) -> Result<Self> {
        let private_key_der = decode_pkcs8_private_key_pem(private_key_pem)?;
        let signing_key = Ed25519SigningKey::from_pkcs8(&private_key_der).map_err(|_| {
            anyhow!("web bot auth key must be an unencrypted PKCS#8 Ed25519 private key")
        })?;
        let public_key = *signing_key.public_key();
        let keyid = jwk_thumbprint(&public_key);
        if let Some(expected_keyid) = expected_keyid
            && expected_keyid != keyid
        {
            bail!(
                "--web-bot-auth-keyid does not match the private key; expected `{expected_keyid}`, computed `{keyid}`"
            );
        }

        Ok(Self {
            signing_key,
            public_key,
            keyid,
        })
    }

    pub(crate) fn keyid(&self) -> &str {
        &self.keyid
    }

    pub(crate) fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }

    pub(crate) fn sign(&self, message: &[u8]) -> Result<[u8; 64]> {
        self.signing_key
            .sign(message)
            .context("failed to create web bot auth Ed25519 signature")
    }
}

fn decode_pkcs8_private_key_pem(private_key_pem: &[u8]) -> Result<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
    const END: &str = "-----END PRIVATE KEY-----";

    let pem = str::from_utf8(private_key_pem)
        .context("web bot auth private key PEM is not valid UTF-8")?;
    if pem.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----") {
        bail!("encrypted web bot auth private keys are not supported");
    }
    let pem = pem.trim();
    let body = pem
        .strip_prefix(BEGIN)
        .and_then(|pem| pem.strip_suffix(END))
        .ok_or_else(|| {
            anyhow!("web bot auth key must use BEGIN PRIVATE KEY PKCS#8 PEM encoding")
        })?;
    if body.contains("-----") {
        bail!("web bot auth key PEM must contain exactly one private key");
    }
    let encoded = body
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
    if encoded.is_empty() {
        bail!("web bot auth private key PEM body is empty");
    }
    general_purpose::STANDARD
        .decode(encoded)
        .context("web bot auth private key PEM contains invalid base64")
}

fn jwk_thumbprint(public_key: &[u8; 32]) -> String {
    let x = general_purpose::URL_SAFE_NO_PAD.encode(public_key);
    let canonical_jwk = format!(r#"{{"crv":"Ed25519","kty":"OKP","x":"{x}"}}"#);
    general_purpose::URL_SAFE_NO_PAD.encode(sha256_digest(canonical_jwk))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EXPECTED_KEYID, RFC_9421_ED25519_PRIVATE_KEY};

    #[test]
    fn derives_rfc_jwk_thumbprint_and_public_key() {
        let key = WebBotAuthKey::from_pem(
            RFC_9421_ED25519_PRIVATE_KEY.as_bytes(),
            Some(EXPECTED_KEYID),
        )
        .unwrap();

        assert_eq!(key.keyid(), EXPECTED_KEYID);
        assert_eq!(
            general_purpose::URL_SAFE_NO_PAD.encode(key.public_key()),
            "JrQLj5P_89iXES9-vFgrIy29clF9CC_oPPsw3c5D0bs"
        );
    }

    #[test]
    fn rejects_mismatched_keyid_and_invalid_pem() {
        let wrong_keyid =
            WebBotAuthKey::from_pem(RFC_9421_ED25519_PRIVATE_KEY.as_bytes(), Some("wrong"))
                .err()
                .expect("mismatched keyid should fail")
                .to_string();
        assert!(wrong_keyid.contains("does not match"));
        assert!(wrong_keyid.contains(EXPECTED_KEYID));

        let invalid_pem = WebBotAuthKey::from_pem(b"not a private key", None)
            .err()
            .expect("invalid PEM should fail")
            .to_string();
        assert!(invalid_pem.contains("BEGIN PRIVATE KEY"));

        let encrypted = WebBotAuthKey::from_pem(
            b"-----BEGIN ENCRYPTED PRIVATE KEY-----\nAA==\n-----END ENCRYPTED PRIVATE KEY-----",
            None,
        )
        .err()
        .expect("encrypted PEM should fail")
        .to_string();
        assert!(encrypted.contains("encrypted"));
    }
}
