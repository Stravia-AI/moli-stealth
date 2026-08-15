use crate::{WebBotAuthProfile, WebBotAuthSigner, key::WebBotAuthKey};

pub(crate) const RFC_9421_ED25519_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MC4CAQAwBQYDK2VwBCIEIJ+DYvh6SEqVTm50DFtMDoQikTmiCqirVv9mWG9qfSnF\n\
-----END PRIVATE KEY-----\n";
pub(crate) const EXPECTED_KEYID: &str = "poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U";

pub(crate) fn key() -> WebBotAuthKey {
    WebBotAuthKey::from_pem(
        RFC_9421_ED25519_PRIVATE_KEY.as_bytes(),
        Some(EXPECTED_KEYID),
    )
    .unwrap()
}

pub(crate) fn signer(profile: WebBotAuthProfile) -> WebBotAuthSigner {
    WebBotAuthSigner::from_pem(
        RFC_9421_ED25519_PRIVATE_KEY.as_bytes(),
        "signature-agent.test",
        Some(EXPECTED_KEYID),
        profile,
    )
    .unwrap()
}
