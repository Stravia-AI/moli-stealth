//! Web Bot Auth request signing built on RFC 9421 HTTP message signatures.

mod key;
mod profile;
mod signer;
mod wire;

#[cfg(test)]
mod test_support;

pub use profile::WebBotAuthProfile;
pub use signer::WebBotAuthSigner;
