use std::time::Duration;

use socket2::{SockRef, TcpKeepalive};
use tokio::net::TcpStream;

// Chromium applies the same 45-second desktop TCP keepalive delay to accepted
// DevTools connections. This is transport liveness, not an application idle
// timeout: a healthy quiet connection remains open.
const PROTOCOL_TCP_KEEPALIVE_DELAY: Duration = Duration::from_secs(45);

pub(super) fn configure_accepted_protocol_stream(stream: &TcpStream) {
    if let Err(error) = stream.set_nodelay(true) {
        tracing::warn!(%error, "failed to set TCP_NODELAY on incoming protocol connection");
    }

    let keepalive = protocol_tcp_keepalive();
    if let Err(error) = SockRef::from(stream).set_tcp_keepalive(&keepalive) {
        tracing::warn!(%error, "failed to enable TCP keepalive on incoming protocol connection");
    }
}

fn protocol_tcp_keepalive() -> TcpKeepalive {
    let keepalive = TcpKeepalive::new().with_time(PROTOCOL_TCP_KEEPALIVE_DELAY);

    // Chromium sets both TCP_KEEPIDLE and TCP_KEEPINTVL on Linux and both
    // fields of SIO_KEEPALIVE_VALS on Windows. Other supported platforms keep
    // their native probe interval while using the same initial delay.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let keepalive = keepalive.with_interval(PROTOCOL_TCP_KEEPALIVE_DELAY);

    keepalive
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn accepted_protocol_stream_uses_chromium_aligned_tcp_options() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind TCP options test listener");
        let client = TcpStream::connect(
            listener
                .local_addr()
                .expect("read TCP options test listener address"),
        )
        .await
        .expect("connect TCP options test client");
        let (accepted, _) = listener
            .accept()
            .await
            .expect("accept TCP options test connection");

        configure_accepted_protocol_stream(&accepted);

        assert!(accepted.nodelay().expect("read TCP_NODELAY"));
        let socket = SockRef::from(&accepted);
        assert!(socket.keepalive().expect("read SO_KEEPALIVE"));

        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                socket.tcp_keepalive_time().expect("read TCP_KEEPIDLE"),
                PROTOCOL_TCP_KEEPALIVE_DELAY
            );
            assert_eq!(
                socket.tcp_keepalive_interval().expect("read TCP_KEEPINTVL"),
                PROTOCOL_TCP_KEEPALIVE_DELAY
            );
        }

        drop(client);
    }
}
