use moli_sdk::{ErrorKind, Fingerprint, Request, Session, SessionConfig, TransportConfig};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};

pub async fn streaming_and_cancel() {
    let fixture = crate::fixture::Fixture::start().await;
    let session = Session::new(SessionConfig::default()).await.unwrap();
    let transport = session.transport(TransportConfig::default()).await.unwrap();
    let mut response = transport
        .execute(Request::get(fixture.url("/stream")))
        .await
        .unwrap();
    let mut first = Vec::new();
    timeout(Duration::from_secs(3), async {
        while first.len() < 7 {
            first.extend(
                response
                    .body
                    .chunk()
                    .await
                    .unwrap()
                    .expect("EOF before first block"),
            );
        }
    })
    .await
    .expect("SDK buffered until EOF instead of exposing the first block");
    assert_eq!(first, b"\x00\xfffirst");
    fixture.observations.stream_continue.notify_one();
    while let Some(chunk) = response.body.chunk().await.unwrap() {
        first.extend(chunk);
    }
    assert_eq!(first, b"\x00\xfffirstlast\x00");
    fixture.observations.stream_dropped.notified().await;

    let mut response = transport
        .execute(Request::get(fixture.url("/stream")))
        .await
        .unwrap();
    let mut first = Vec::new();
    while first.len() < 7 {
        first.extend(response.body.chunk().await.unwrap().unwrap());
    }
    assert!(
        timeout(Duration::from_millis(100), response.body.chunk())
            .await
            .is_err()
    );
    timeout(
        Duration::from_secs(3),
        fixture.observations.stream_dropped.notified(),
    )
    .await
    .expect("abandoned body read did not terminate the request");
    assert_eq!(
        response
            .body
            .chunk()
            .await
            .expect_err("cancelled body remained usable")
            .kind,
        ErrorKind::Closed
    );

    {
        let pending = transport.execute(Request::get(fixture.url("/hold")));
        tokio::pin!(pending);
        tokio::select! {
            _ = &mut pending => panic!("fixture unexpectedly responded"),
            _ = fixture.observations.request_started.notified() => {},
        }
        assert!(timeout(Duration::from_millis(100), pending).await.is_err());
    }
    timeout(
        Duration::from_secs(3),
        fixture.observations.request_dropped.notified(),
    )
    .await
    .expect("dropped request Future did not terminate the request");
    let following = transport
        .execute(Request::get(fixture.url("/echo")))
        .await
        .unwrap();
    assert_eq!(following.metadata.status, 200);
    drop(following);
    drop(response);
    transport.close().await.unwrap();
    session.close().await.unwrap();
}

pub async fn pinned_single_hop_and_proxy() {
    let fixture = crate::fixture::Fixture::start().await;
    // 子句柄必须保留临时 Session；无需宿主额外维护所有者对象。
    let transport = Session::new(SessionConfig::default())
        .await
        .unwrap()
        .transport(TransportConfig::default())
        .await
        .unwrap();
    let mut pinned = Request::get(format!(
        "http://sdk-target.invalid:{}/echo",
        fixture.address.port()
    ));
    pinned.method = "POST".into();
    pinned.body = b"pinned-body".to_vec();
    pinned.connection.resolved_addresses = Some(vec![fixture.address]);
    let mut response = transport.execute(pinned).await.unwrap();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.chunk().await.unwrap() {
        bytes.extend(chunk);
    }
    assert_eq!(bytes, b"pinned-body");
    assert_eq!(
        fixture.observations.requests.lock().await.last().unwrap().1["host"],
        format!("sdk-target.invalid:{}", fixture.address.port())
    );
    let response = transport
        .execute(Request::get(fixture.url("/redirect")))
        .await
        .unwrap();
    assert_eq!(response.metadata.status, 307);
    assert!(
        response
            .metadata
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("location") && value == "/echo")
    );
    drop(response);

    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = proxy.local_addr().unwrap();
    let proxy_task = tokio::spawn(async move {
        let (mut socket, _) = proxy.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            header.push(byte[0]);
            assert!(header.len() < 8192);
        }
        assert!(
            String::from_utf8(header)
                .unwrap()
                .starts_with("GET http://sdk-target.invalid:")
        );
        socket.write_all(b"HTTP/1.1 218 Proxy fixture\r\nContent-Length: 5\r\nConnection: close\r\n\r\nproxy").await.unwrap();
    });
    let mut request = Request::get(format!(
        "http://sdk-target.invalid:{}/echo",
        fixture.address.port()
    ));
    request.connection.proxy = Some(format!("http://{address}"));
    request.connection.no_proxy = Some(String::new());
    request.connection.resolved_addresses = Some(vec![fixture.address]);
    let response = transport.execute(request.clone()).await.unwrap();
    assert_eq!(
        response.metadata.status, 218,
        "explicit empty no_proxy must not bypass configured proxy"
    );
    drop(response);
    proxy_task.await.unwrap();
    request.connection.no_proxy = Some("*".into());
    let response = transport.execute(request).await.unwrap();
    assert_eq!(
        response.metadata.status, 200,
        "explicit proxy bypass must connect to the pinned origin"
    );
    drop(response);
    transport.close().await.unwrap();
}

pub async fn tls_verification() {
    let fixture = crate::fixture::TlsFixture::start().await;
    let session = Session::new(SessionConfig::default()).await.unwrap();
    let conflicting = Session::new(SessionConfig {
        fingerprint: Fingerprint::Ordinary,
        ..Default::default()
    })
    .await
    .err()
    .expect("a second Session silently changed the process transport fingerprint");
    assert_eq!(conflicting.kind, ErrorKind::Initialization);
    let verified = session.transport(TransportConfig::default()).await.unwrap();
    let mut request = Request::get(format!("https://localhost:{}/", fixture.address.port()));
    request.connection.resolved_addresses = Some(vec![fixture.address]);
    let error = verified
        .execute(request.clone())
        .await
        .err()
        .expect("default TLS accepted an untrusted self-signed certificate");
    assert_eq!(error.kind, ErrorKind::Transport);
    // 只对此受控自签服务器显式关闭验证；默认路径必须保持拒绝。
    let fixture_only = session
        .transport(TransportConfig {
            tls_verify: false,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut response = fixture_only.execute(request).await.unwrap();
    assert_eq!(response.metadata.status, 200);
    let mut bytes = Vec::new();
    while let Some(chunk) = response.body.chunk().await.unwrap() {
        bytes.extend(chunk);
    }
    assert_eq!(bytes, b"secure-body");
    session.close().await.unwrap();
}

pub async fn connection_timeout_releases_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        socket.read_to_end(&mut received).await.unwrap();
        assert_eq!(
            received.first(),
            Some(&22),
            "peer did not receive a TLS handshake record"
        );
    });
    let session = Session::new(SessionConfig::default()).await.unwrap();
    let transport = session.transport(TransportConfig::default()).await.unwrap();
    let mut request = Request::get(format!("https://localhost:{}/", address.port()));
    request.connection.resolved_addresses = Some(vec![address]);
    request.connection.connect_timeout = Some(Duration::from_millis(200));
    let result = timeout(Duration::from_secs(5), transport.execute(request))
        .await
        .expect("configured connection timeout did not end the stalled handshake");
    assert_eq!(result.err().unwrap().kind, ErrorKind::Transport);
    timeout(Duration::from_secs(3), peer)
        .await
        .expect("connection timeout left the peer connected")
        .unwrap();
    let fixture = crate::fixture::Fixture::start().await;
    let response = transport
        .execute(Request::get(fixture.url("/echo")))
        .await
        .unwrap();
    assert_eq!(response.metadata.status, 200);
    session.close().await.unwrap();
}

pub async fn drop_final_handle() {
    let fixture = crate::fixture::Fixture::start().await;
    let transport = Session::new(SessionConfig::default())
        .await
        .unwrap()
        .transport(TransportConfig::default())
        .await
        .unwrap();
    let mut response = transport
        .execute(Request::get(fixture.url("/stream")))
        .await
        .unwrap();
    assert!(response.body.chunk().await.unwrap().is_some());
    drop(transport);
    let started = std::time::Instant::now();
    drop(response);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "Drop blocked on owner cleanup"
    );
    timeout(
        Duration::from_secs(3),
        fixture.observations.stream_dropped.notified(),
    )
    .await
    .expect("last external handle did not arrange request cleanup");
}

pub async fn h2_close_reclaims_connection() {
    let mut fixture = crate::fixture::H2Fixture::start().await;
    let session = Session::new(SessionConfig::default()).await.unwrap();
    let transport = session
        .transport(TransportConfig {
            tls_verify: false,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut request = Request::get(format!("https://localhost:{}/", fixture.address.port()));
    request.connection.resolved_addresses = Some(vec![fixture.address]);
    let mut response = transport.execute(request).await.unwrap();
    assert_eq!(response.metadata.version, moli_sdk::HttpVersion::Http2);
    let mut bytes = Vec::new();
    while bytes.len() < 4 {
        bytes.extend(response.body.chunk().await.unwrap().unwrap());
    }
    assert_eq!(bytes, [0, 255, 4, 5]);
    timeout(Duration::from_secs(3), transport.close())
        .await
        .expect("Transport close did not finish with an active H2 response")
        .unwrap();
    assert_eq!(
        response
            .body
            .chunk()
            .await
            .expect_err("closed H2 body remained usable")
            .kind,
        ErrorKind::Closed
    );
    timeout(Duration::from_secs(3), &mut fixture.closed)
        .await
        .expect("H2 peer remained connected after Transport close")
        .unwrap();
    let following = session.transport(TransportConfig::default()).await.unwrap();
    let http = crate::fixture::Fixture::start().await;
    assert_eq!(
        following
            .execute(Request::get(http.url("/echo")))
            .await
            .unwrap()
            .metadata
            .status,
        200
    );
    session.close().await.unwrap();
}
