use std::fmt;

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};

const ED25519_PUBLIC_KEY_LENGTH: usize = 32;
const ED25519_SIGNATURE_LENGTH: usize = 64;

pub struct Ed25519SigningKey {
    key_pair: Ed25519KeyPair,
    public_key: [u8; ED25519_PUBLIC_KEY_LENGTH],
}

impl Ed25519SigningKey {
    pub fn from_pkcs8(der: &[u8]) -> Result<Self, Ed25519Error> {
        let key_pair = Ed25519KeyPair::from_pkcs8(der).map_err(|_| Ed25519Error)?;
        let public_key = key_pair
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| Ed25519Error)?;
        Ok(Self {
            key_pair,
            public_key,
        })
    }

    pub fn public_key(&self) -> &[u8; ED25519_PUBLIC_KEY_LENGTH] {
        &self.public_key
    }

    pub fn sign(&self, message: &[u8]) -> Result<[u8; ED25519_SIGNATURE_LENGTH], Ed25519Error> {
        self.key_pair
            .try_sign(message)
            .map_err(|_| Ed25519Error)?
            .as_ref()
            .try_into()
            .map_err(|_| Ed25519Error)
    }
}

impl fmt::Debug for Ed25519SigningKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Ed25519SigningKey")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ed25519Error;

impl fmt::Display for Ed25519Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid Ed25519 key or signing operation failed")
    }
}

impl std::error::Error for Ed25519Error {}

#[cfg(test)]
mod tests {
    use super::*;

    const RFC_9421_PRIVATE_KEY_DER: &[u8] = &[
        0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04,
        0x20, 0x9f, 0x83, 0x62, 0xf8, 0x7a, 0x48, 0x4a, 0x95, 0x4e, 0x6e, 0x74, 0x0c, 0x5b, 0x4c,
        0x0e, 0x84, 0x22, 0x91, 0x39, 0xa2, 0x0a, 0xa8, 0xab, 0x56, 0xff, 0x66, 0x58, 0x6f, 0x6a,
        0x7d, 0x29, 0xc5,
    ];

    #[test]
    fn imports_pkcs8_and_signs_deterministically() {
        let key = Ed25519SigningKey::from_pkcs8(RFC_9421_PRIVATE_KEY_DER).unwrap();
        assert_eq!(
            key.public_key(),
            &[
                0x26, 0xb4, 0x0b, 0x8f, 0x93, 0xff, 0xf3, 0xd8, 0x97, 0x11, 0x2f, 0x7e, 0xbc, 0x58,
                0x2b, 0x23, 0x2d, 0xbd, 0x72, 0x51, 0x7d, 0x08, 0x2f, 0xe8, 0x3c, 0xfb, 0x30, 0xdd,
                0xce, 0x43, 0xd1, 0xbb,
            ]
        );

        let first = key.sign(b"web bot auth").unwrap();
        let second = key.sign(b"web bot auth").unwrap();
        assert_eq!(first.len(), ED25519_SIGNATURE_LENGTH);
        assert_eq!(first, second);
        assert_ne!(first, key.sign(b"different message").unwrap());
    }

    #[test]
    fn rejects_non_ed25519_pkcs8() {
        assert_eq!(
            Ed25519SigningKey::from_pkcs8(b"not a key").unwrap_err(),
            Ed25519Error
        );
    }
}
