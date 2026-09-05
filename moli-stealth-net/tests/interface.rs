use std::{net::SocketAddr, time::Duration};

use moli_stealth_net::{
    AuthScheme, Transport, TransportAuth, TransportConfig, TransportError, TransportRequest,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use url::Url;

async fn local_server() -> (TcpListener, Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = Url::parse(&format!("http://localhost:{}/resource", address.port())).unwrap();
    (listener, url, address)
}

async fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let head_end = loop {
        let mut bytes = [0; 1024];
        let count = stream.read(&mut bytes).await.unwrap();
        assert_ne!(count, 0, "client closed before finishing request");
        request.extend_from_slice(&bytes[..count]);
        if let Some(position) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let head = String::from_utf8_lossy(&request[..head_end]);
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while request.len() < head_end + content_length {
        let mut bytes = [0; 1024];
        let count = stream.read(&mut bytes).await.unwrap();
        assert_ne!(count, 0, "client closed in request body");
        request.extend_from_slice(&bytes[..count]);
    }
    request
}

#[tokio::test]
async fn final_headers_and_chunks_arrive_before_a_nonending_response_finishes() {
    let (listener, url, address) = local_server().await;
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        assert!(request.starts_with(b"PATCH /resource HTTP/1.1\r\n"));
        assert!(request.ends_with(&[0, 255, 1, 128]));
        stream
            .write_all(
                b"HTTP/1.1 103 Early Hints\r\nLink: </asset>\r\n\r\n\
                  HTTP/1.1 200 OK\r\nX-Duplicate: one\r\nX-Duplicate: two\r\nTransfer-Encoding: chunked\r\n\r\n\
                  4\r\n\0\xff\x01\x80\r\n",
            )
            .await
            .unwrap();
        release_rx.await.unwrap();
        stream.write_all(b"0\r\n\r\n").await.unwrap();
    });

    let transport = Transport::new(TransportConfig::default()).unwrap();
    let mut request = TransportRequest::new(url, "PATCH");
    request.body = Some(vec![0, 255, 1, 128]);
    request.connection.resolved_addresses = Some(vec![address]);
    request.connection.proxy = Some(String::new());
    let mut response = tokio::time::timeout(Duration::from_secs(2), transport.execute(request))
        .await
        .expect("final headers must not wait for EOF")
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.version, http::Version::HTTP_11);
    assert_eq!(
        response
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("x-duplicate"))
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(
        response.body.chunk().await.unwrap().unwrap().as_ref(),
        &[0, 255, 1, 128]
    );
    release_tx.send(()).unwrap();
    assert!(response.body.chunk().await.unwrap().is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn no_body_status_completes_without_waiting_for_a_persistent_connection() {
    let (listener, url, address) = local_server().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nConnection: keep-alive\r\n\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let transport = Transport::new(TransportConfig::default()).unwrap();
    let mut request = TransportRequest::new(url, "HEAD");
    request.connection.resolved_addresses = Some(vec![address]);
    request.connection.proxy = Some(String::new());
    let mut response = tokio::time::timeout(Duration::from_secs(1), transport.execute(request))
        .await
        .expect("no-body response must complete while the socket stays open")
        .unwrap();
    assert_eq!(response.status, 204);
    assert!(response.body.chunk().await.unwrap().is_none());
    server.abort();
}

#[tokio::test]
async fn completed_http1_body_returns_its_connection_to_the_pool() {
    let (listener, url, address) = local_server().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\na")
            .await
            .unwrap();
        let _ = read_request(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nb")
            .await
            .unwrap();
    });

    let transport = Transport::new(TransportConfig::default()).unwrap();
    let request = |url: Url| {
        let mut request = TransportRequest::new(url, "GET");
        request.connection.resolved_addresses = Some(vec![address]);
        request.connection.proxy = Some(String::new());
        request
    };
    let mut first = transport.execute(request(url.clone())).await.unwrap();
    assert_eq!(first.body.chunk().await.unwrap().unwrap().as_ref(), b"a");
    assert!(first.body.chunk().await.unwrap().is_none());
    let mut second = transport.execute(request(url)).await.unwrap();
    assert_eq!(second.body.chunk().await.unwrap().unwrap().as_ref(), b"b");
    assert!(second.body.chunk().await.unwrap().is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn stale_http1_recovery_requires_a_safe_method_without_a_body() {
    for (method, body, recover) in [
        ("GET", None, true),
        ("POST", None, false),
        ("GET", Some(vec![0, 255]), false),
    ] {
        let (listener, url, address) = local_server().await;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                .await
                .unwrap();
            let _ = read_request(&mut stream).await;
            drop(stream);

            let (mut replacement, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut replacement).await;
            replacement
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nrecovered")
                .await
                .unwrap();
        });
        let transport = Transport::new(TransportConfig::default()).unwrap();
        let mut request = TransportRequest::new(url, "GET");
        request.connection.resolved_addresses = Some(vec![address]);
        request.connection.proxy = Some(String::new());
        assert_eq!(
            transport.execute(request.clone()).await.unwrap().status,
            204
        );
        request.method = method.into();
        request.body = body;
        let result = tokio::time::timeout(Duration::from_secs(2), transport.execute(request))
            .await
            .expect("stale connection recovery must be bounded");
        if recover {
            let mut response = result.expect("a safe request must recover on a fresh connection");
            assert_eq!(response.status, 200);
            assert_eq!(
                response.body.chunk().await.unwrap().unwrap().as_ref(),
                b"recovered"
            );
            server.await.unwrap();
        } else {
            assert!(matches!(result, Err(TransportError::EmptyResponse)));
            server.abort();
        }
    }
}

#[tokio::test]
async fn queued_requests_resume_when_an_http1_body_returns_idle_capacity() {
    for (max_connections, max_host_connections) in [(Some(1), None), (None, Some(1))] {
        let (listener, url, address) = local_server().await;
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut first).await;
            first
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\na")
                .await
                .unwrap();
            let (mut second, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut second).await;
            second
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nb")
                .await
                .unwrap();
        });
        let transport = Transport::new(TransportConfig {
            max_connections,
            max_host_connections,
            ..TransportConfig::default()
        })
        .unwrap();
        let request = || {
            let mut request = TransportRequest::new(url.clone(), "GET");
            request.connection.resolved_addresses = Some(vec![address]);
            request.connection.proxy = Some(String::new());
            request
        };
        let mut first = transport.execute(request()).await.unwrap();
        let second = transport.execute(request());
        tokio::pin!(second);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(second.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;

        assert_eq!(first.body.chunk().await.unwrap().unwrap().as_ref(), b"a");
        assert!(first.body.chunk().await.unwrap().is_none());
        let mut second = tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .expect("an idle connection must not strand an already queued request")
            .unwrap();
        assert_eq!(second.body.chunk().await.unwrap().unwrap().as_ref(), b"b");
        assert!(second.body.chunk().await.unwrap().is_none());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn a_saturated_host_does_not_reserve_other_hosts_global_capacity() {
    let (first_listener, first_url, first_address) = local_server().await;
    let (other_listener, mut other_url, other_address) = local_server().await;
    other_url.set_host(Some("other.invalid")).unwrap();
    let first_server = tokio::spawn(async move {
        let (mut stream, _) = first_listener.accept().await.unwrap();
        let _ = read_request(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\na")
            .await
            .unwrap();
        assert_eq!(stream.read(&mut [0]).await.unwrap(), 0);
    });
    let other_server = tokio::spawn(async move {
        let (mut stream, _) = other_listener.accept().await.unwrap();
        let _ = read_request(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nb")
            .await
            .unwrap();
    });
    let transport = Transport::new(TransportConfig {
        max_connections: Some(2),
        max_host_connections: Some(1),
        ..TransportConfig::default()
    })
    .unwrap();
    let request = |url, address| {
        let mut request = TransportRequest::new(url, "GET");
        request.connection.resolved_addresses = Some(vec![address]);
        request.connection.proxy = Some(String::new());
        request
    };
    let first = transport
        .execute(request(first_url.clone(), first_address))
        .await
        .unwrap();
    let queued = transport.execute(request(first_url, first_address));
    tokio::pin!(queued);
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(queued.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;

    let mut other = tokio::time::timeout(
        Duration::from_secs(2),
        transport.execute(request(other_url, other_address)),
    )
    .await
    .expect("a host waiter must not consume capacity available to another host")
    .unwrap();
    assert_eq!(other.body.chunk().await.unwrap().unwrap().as_ref(), b"b");
    assert!(other.body.chunk().await.unwrap().is_none());
    drop(first.body);
    first_server.await.unwrap();
    other_server.await.unwrap();
}

#[tokio::test]
async fn dropping_an_unfinished_body_releases_the_connection_cap() {
    let (listener, url, address) = local_server().await;
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut first).await;
        first
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\n")
            .await
            .unwrap();
        let (mut second, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut second).await;
        second
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nb")
            .await
            .unwrap();
    });

    let transport = Transport::new(TransportConfig {
        max_connections: Some(1),
        ..TransportConfig::default()
    })
    .unwrap();
    let mut first = TransportRequest::new(url.clone(), "GET");
    first.connection.resolved_addresses = Some(vec![address]);
    first.connection.proxy = Some(String::new());
    let response = transport.execute(first).await.unwrap();
    drop(response.body);

    let mut second = TransportRequest::new(url, "GET");
    second.connection.resolved_addresses = Some(vec![address]);
    second.connection.proxy = Some(String::new());
    let mut response = tokio::time::timeout(Duration::from_secs(2), transport.execute(second))
        .await
        .expect("dropped body must release its physical connection slot")
        .unwrap();
    assert_eq!(response.body.chunk().await.unwrap().unwrap().as_ref(), b"b");
    server.await.unwrap();
}

#[tokio::test]
async fn forward_proxy_auth_is_sent_to_the_proxy_but_not_origin_metadata() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = proxy.accept().await.unwrap();
        let request = read_request(&mut stream).await;
        let head = String::from_utf8_lossy(&request);
        assert!(head.starts_with("GET http://example.invalid/resource HTTP/1.1\r\n"));
        assert!(
            head.lines()
                .any(|line| line == "proxy-authorization: Basic dXNlcjpwYXNz")
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });

    let transport = Transport::new(TransportConfig::default()).unwrap();
    let mut request = TransportRequest::new(
        Url::parse("http://example.invalid/resource").unwrap(),
        "GET",
    );
    request.connection.proxy = Some(format!("http://{proxy_address}"));
    request.connection.no_proxy = Some(String::new());
    request.connection.proxy_auth = Some(TransportAuth {
        scheme: AuthScheme::Basic,
        username: "user".into(),
        password: "pass".into(),
    });
    let response = transport.execute(request).await.unwrap();
    assert_eq!(response.status, 200);
    assert!(
        response
            .sent_headers
            .iter()
            .all(|(name, _)| !name.eq_ignore_ascii_case("proxy-authorization"))
    );
    server.await.unwrap();
}

#[tokio::test]
async fn explicit_no_proxy_bypass_uses_the_approved_direct_address() {
    let (listener, url, address) = local_server().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });

    let transport = Transport::new(TransportConfig::default()).unwrap();
    let mut request = TransportRequest::new(url, "GET");
    request.connection.proxy = Some("http://127.0.0.1:9".into());
    request.connection.no_proxy = Some("*".into());
    request.connection.resolved_addresses = Some(vec![address]);
    assert_eq!(transport.execute(request).await.unwrap().status, 200);
    server.await.unwrap();
}

#[tokio::test]
async fn connect_digest_challenge_preserves_the_authenticated_tunnel_bytes() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = proxy.accept().await.unwrap();
        let first = String::from_utf8(read_request(&mut stream).await).unwrap();
        assert!(first.starts_with("CONNECT example.test:80 HTTP/1.1\r\n"));
        assert!(!first.to_ascii_lowercase().contains("proxy-authorization:"));
        stream
            .write_all(
                concat!(
                    "HTTP/1.1 407 Proxy Authentication Required\r\n",
                    "Proxy-Authenticate: Digest realm=\"testrealm@host.com\", ",
                    "nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", algorithm=MD5\r\n",
                    "Content-Length: 0\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let second = String::from_utf8(read_request(&mut stream).await).unwrap();
        assert!(second.contains("uri=\"example.test:80\""));
        assert!(second.contains("response=\"f293769225ff85d7aea9501545d2a815\""));
        stream
            .write_all(
                concat!(
                    "HTTP/1.1 100 Continue\r\n\r\n",
                    "HTTP/1.1 200 Connection Established\r\nContent-Length: 999\r\n\r\n",
                    "tunnel"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let options = moli_stealth_net::ConnectionOptions {
        proxy: Some(format!("http://{proxy_address}")),
        no_proxy: Some(String::new()),
        proxy_auth: Some(TransportAuth {
            scheme: AuthScheme::Digest,
            username: "Mufasa".into(),
            password: "Circle Of Life".into(),
        }),
        ..Default::default()
    };
    let mut connected = tokio::time::timeout(
        Duration::from_secs(2),
        moli_stealth_net::open_connection(
            &Url::parse("ws://example.test/").unwrap(),
            &options,
            &moli_stealth_net::TransportFingerprint::default(),
            true,
            true,
            true,
        ),
    )
    .await
    .expect("CONNECT must wait for its challenge, not a success response body")
    .unwrap();
    let mut bytes = [0; 6];
    connected.stream.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"tunnel");
    server.await.unwrap();
}

async fn assert_socks_destination(scheme: &str, expected: &[u8]) {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let expected = expected.to_vec();
    let socks5 = scheme.starts_with("socks5");
    let server = tokio::spawn(async move {
        let (mut stream, _) = proxy.accept().await.unwrap();
        if socks5 {
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            stream.write_all(&[5, 0]).await.unwrap();
        }
        let mut request = vec![0; expected.len()];
        stream.read_exact(&mut request).await.unwrap();
        assert_eq!(request, expected);
        let reply: &[u8] = if socks5 {
            &[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]
        } else {
            &[0, 90, 0, 80, 127, 0, 0, 1]
        };
        stream.write_all(reply).await.unwrap();
        let request = read_request(&mut stream).await;
        assert!(request.starts_with(b"GET / HTTP/1.1\r\n"));
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .await
            .unwrap();
    });
    let transport = Transport::new(TransportConfig::default()).unwrap();
    let mut request = TransportRequest::new(Url::parse("http://example.invalid/").unwrap(), "GET");
    request.connection.proxy = Some(format!("{scheme}://{proxy_address}"));
    request.connection.no_proxy = Some(String::new());
    request.connection.resolved_addresses = Some(vec!["127.0.0.9:80".parse().unwrap()]);
    let response = tokio::time::timeout(Duration::from_secs(2), transport.execute(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status, 204);
    server.await.unwrap();
}

#[tokio::test]
async fn socks5_local_resolution_uses_pins_while_socks5h_delegates_dns() {
    assert_socks_destination("socks5", &[5, 1, 0, 1, 127, 0, 0, 9, 0, 80]).await;
    assert_socks_destination("socks5h", b"\x05\x01\x00\x03\x0fexample.invalid\x00\x50").await;
}

#[tokio::test]
async fn socks4_local_resolution_uses_pins_while_socks4a_delegates_dns() {
    assert_socks_destination("socks4", &[4, 1, 0, 80, 127, 0, 0, 9, 0]).await;
    assert_socks_destination(
        "socks4a",
        b"\x04\x01\x00\x50\x00\x00\x00\x01\x00example.invalid\x00",
    )
    .await;
}

#[tokio::test]
async fn hostless_requests_are_rejected_before_connecting() {
    let transport = Transport::new(TransportConfig::default()).unwrap();
    let error = match transport
        .execute(TransportRequest::new(
            Url::parse("data:text/plain,hello").unwrap(),
            "GET",
        ))
        .await
    {
        Ok(_) => panic!("hostless request must fail"),
        Err(error) => error,
    };
    assert!(matches!(error, TransportError::InvalidInput(_)));
}
