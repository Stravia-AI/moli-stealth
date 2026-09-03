use std::time::Duration;

use moli_stealth_net::{ChromeMethod, ChromeRequest, ChromeTransport, ChromeTransportError};
use url::Url;

#[test]
fn rejects_hostless_requests_before_connecting() {
    let transport = ChromeTransport::new(None, None).expect("Chrome transport should initialize");
    let url = Url::parse("data:text/plain,hello").expect("valid hostless URL");
    let error = match transport.execute(ChromeRequest {
        url: &url,
        method: ChromeMethod::Get,
        headers: &[],
        resolved_addresses: Vec::new(),
        timeout: Duration::from_secs(1),
    }) {
        Ok(_) => panic!("hostless request must be rejected"),
        Err(error) => error,
    };

    assert!(matches!(error, ChromeTransportError::MissingHost(_)));
}
