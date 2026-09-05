use anyhow::{Result, bail};
use http::StatusCode;
use moli_stealth_net::TransportError;

pub const NET_ERR_ABORTED_ERROR_TEXT: &str = "net::ERR_ABORTED";

pub(crate) fn browser_network_error_text(error: &anyhow::Error) -> &'static str {
    let error_chain = format!("{error:#}");
    if error_chain.contains("request cancelled") {
        return NET_ERR_ABORTED_ERROR_TEXT;
    }

    match error.downcast_ref::<TransportError>() {
        Some(TransportError::Resolve(_)) => "net::ERR_NAME_NOT_RESOLVED",
        Some(TransportError::ProxyConnect(_)) => "net::ERR_PROXY_CONNECTION_FAILED",
        Some(TransportError::Timeout) => "net::ERR_TIMED_OUT",
        Some(TransportError::Certificate(_)) => "net::ERR_CERT_AUTHORITY_INVALID",
        Some(TransportError::EmptyResponse) => "net::ERR_EMPTY_RESPONSE",
        Some(TransportError::Cancelled) => NET_ERR_ABORTED_ERROR_TEXT,
        Some(TransportError::Io(io)) if io.kind() == std::io::ErrorKind::ConnectionRefused => {
            "net::ERR_CONNECTION_REFUSED"
        }
        Some(TransportError::Io(io))
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
            ) =>
        {
            "net::ERR_CONNECTION_RESET"
        }
        Some(
            TransportError::Io(_)
            | TransportError::Tls(_)
            | TransportError::Http1(_)
            | TransportError::Http2(_)
            | TransportError::InvalidInput(_)
            | TransportError::TooLarge
            | TransportError::Authentication(_),
        )
        | None => "net::ERR_FAILED",
    }
}

pub fn ensure_http_status_success(
    request_url: &str,
    status: u16,
    allow_http_auth_challenge_status: bool,
) -> Result<()> {
    if (200..=299).contains(&status) {
        return Ok(());
    }
    if allow_http_auth_challenge_status && matches!(status, 401 | 407) {
        return Ok(());
    }
    let reason = StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or("Unknown");
    bail!("HTTP request `{request_url}` returned {} {reason}", status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_receive_failure_maps_to_browser_connection_reset() {
        let error = anyhow::Error::new(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "peer reset",
        )));
        assert_eq!(
            browser_network_error_text(&error),
            "net::ERR_CONNECTION_RESET"
        );
    }
}
