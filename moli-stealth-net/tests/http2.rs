use std::{net::SocketAddr, sync::Arc, time::Duration};

use moli_stealth_net::{Transport, TransportConfig, TransportFingerprint, TransportRequest};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use url::Url;

fn h2_tls_acceptor() -> TlsAcceptor {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .expect("self-signed H2 certificate");
    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key_der = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("H2 TLS configuration");
    config.alpn_protocols = vec![b"h2".to_vec()];
    TlsAcceptor::from(Arc::new(config))
}

async fn local_h2_origin() -> (TcpListener, Url, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = Url::parse(&format!("https://localhost:{}/", address.port())).unwrap();
    (listener, url, address)
}

fn transport(max_h2_streams: usize, max_connections: Option<usize>) -> Transport {
    Transport::new(TransportConfig {
        fingerprint: TransportFingerprint::chrome(),
        tls_verify: false,
        max_connections,
        max_h2_streams: Some(max_h2_streams),
        ..TransportConfig::default()
    })
    .unwrap()
}

fn request(mut url: Url, path: &str, address: SocketAddr) -> TransportRequest {
    url.set_path(path);
    let mut request = TransportRequest::new(url, "GET");
    request.connection.resolved_addresses = Some(vec![address]);
    request.connection.proxy = Some(String::new());
    request
}

#[tokio::test]
async fn stream_limit_is_scoped_to_each_physical_h2_connection() {
    let (first_listener, first_url, first_address) = local_h2_origin().await;
    let (second_listener, second_url, second_address) = local_h2_origin().await;
    let acceptor = h2_tls_acceptor();
    let first_acceptor = acceptor.clone();
    let (release_first_tx, release_first_rx) = oneshot::channel();
    let first_server = tokio::spawn(async move {
        let (stream, _) = first_listener.accept().await.unwrap();
        let stream = first_acceptor.accept(stream).await.unwrap();
        let mut connection = h2::server::handshake(stream).await.unwrap();
        let mut release = Some(release_first_rx);
        while let Some(Ok((_request, mut respond))) = connection.accept().await {
            let response = http::Response::builder().status(200).body(()).unwrap();
            let mut body = respond.send_response(response, false).unwrap();
            let release = release.take().unwrap();
            tokio::spawn(async move {
                let _ = release.await;
                let _ = body.send_data("first".into(), true);
            });
        }
    });
    let second_server = tokio::spawn(async move {
        let (stream, _) = second_listener.accept().await.unwrap();
        let stream = acceptor.accept(stream).await.unwrap();
        let mut connection = h2::server::handshake(stream).await.unwrap();
        while let Some(Ok((_request, mut respond))) = connection.accept().await {
            let response = http::Response::builder()
                .status(200)
                .header("content-length", "6")
                .body(())
                .unwrap();
            let mut body = respond.send_response(response, false).unwrap();
            body.send_data("second".into(), true).unwrap();
        }
    });

    let transport = transport(1, None);
    let first = transport
        .execute(request(first_url, "/held", first_address))
        .await
        .unwrap();
    let mut second = tokio::time::timeout(
        Duration::from_secs(2),
        transport.execute(request(second_url, "/independent", second_address)),
    )
    .await
    .expect("one origin's stream limit must not block another H2 connection")
    .unwrap();
    assert_eq!(
        second.body.chunk().await.unwrap().unwrap().as_ref(),
        b"second"
    );
    assert!(second.body.chunk().await.unwrap().is_none());

    drop(first.body);
    let _ = release_first_tx.send(());
    first_server.abort();
    second_server.abort();
}

#[tokio::test]
async fn dropping_last_transport_preserves_an_owned_h2_response() {
    let (listener, url, address) = local_h2_origin().await;
    let acceptor = h2_tls_acceptor();
    let (release, waiting) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = acceptor.accept(stream).await.unwrap();
        let mut connection = h2::server::handshake(stream).await.unwrap();
        let mut waiting = Some(waiting);
        while let Some(Ok((_request, mut respond))) = connection.accept().await {
            let response = http::Response::builder().status(200).body(()).unwrap();
            let mut body = respond.send_response(response, false).unwrap();
            let waiting = waiting.take().unwrap();
            tokio::spawn(async move {
                waiting.await.unwrap();
                body.send_data("owned response".into(), true).unwrap();
            });
        }
    });
    let transport = transport(1, Some(1));
    let mut response = transport
        .execute(request(url, "/owned", address))
        .await
        .unwrap();
    drop(transport);
    release.send(()).unwrap();
    let body = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        while let Some(chunk) = response.body.chunk().await.unwrap() {
            bytes.extend_from_slice(&chunk);
        }
        bytes
    })
    .await
    .unwrap();
    assert_eq!(body, b"owned response");
    server.abort();
}

#[tokio::test]
async fn dropping_one_h2_body_does_not_abort_a_sibling_stream() {
    let (listener, url, address) = local_h2_origin().await;
    let acceptor = h2_tls_acceptor();
    let (release_cancelled_tx, release_cancelled_rx) = oneshot::channel();
    let (release_sibling_tx, release_sibling_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = acceptor.accept(stream).await.unwrap();
        let mut connection = h2::server::handshake(stream).await.unwrap();
        let mut cancelled_release = Some(release_cancelled_rx);
        let mut sibling_release = Some(release_sibling_rx);
        while let Some(Ok((request, mut respond))) = connection.accept().await {
            match request.uri().path() {
                "/cancelled" => {
                    let response = http::Response::builder().status(200).body(()).unwrap();
                    let mut body = respond.send_response(response, false).unwrap();
                    let release = cancelled_release.take().unwrap();
                    tokio::spawn(async move {
                        let _ = release.await;
                        let _ = body.send_data("unused".into(), true);
                    });
                }
                "/sibling" => {
                    let response = http::Response::builder()
                        .status(200)
                        .header("content-length", "2")
                        .body(())
                        .unwrap();
                    let mut body = respond.send_response(response, false).unwrap();
                    let release = sibling_release.take().unwrap();
                    tokio::spawn(async move {
                        let _ = release.await;
                        let _ = body.send_data("ok".into(), true);
                    });
                }
                "/replacement" => {
                    let response = http::Response::builder()
                        .status(200)
                        .header("content-length", "3")
                        .body(())
                        .unwrap();
                    let mut body = respond.send_response(response, false).unwrap();
                    body.send_data("new".into(), true).unwrap();
                }
                path => panic!("unexpected request path {path}"),
            }
        }
    });

    let transport = transport(2, Some(1));
    let cancelled_request = request(url.clone(), "/cancelled", address);
    let sibling_request = request(url.clone(), "/sibling", address);
    let (cancelled, sibling) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            transport.execute(cancelled_request),
            transport.execute(sibling_request)
        )
    })
    .await
    .expect("same-origin requests must reuse the connection without waiting on its permit");
    let cancelled = cancelled.unwrap();
    let mut sibling = sibling.unwrap();

    drop(cancelled.body);
    let _ = release_cancelled_tx.send(());
    let mut replacement = tokio::time::timeout(
        Duration::from_secs(2),
        transport.execute(request(url, "/replacement", address)),
    )
    .await
    .expect("dropping a body must release its per-connection stream slot")
    .unwrap();
    assert_eq!(
        replacement.body.chunk().await.unwrap().unwrap().as_ref(),
        b"new"
    );
    assert!(replacement.body.chunk().await.unwrap().is_none());

    release_sibling_tx.send(()).unwrap();
    assert_eq!(sibling.body.chunk().await.unwrap().unwrap().as_ref(), b"ok");
    assert!(sibling.body.chunk().await.unwrap().is_none());

    server.abort();
}

#[tokio::test]
async fn connection_pressure_preserves_an_active_h2_connections_reuse() {
    let (listener, url, address) = local_h2_origin().await;
    let acceptor = h2_tls_acceptor();
    let h2_server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = acceptor.accept(stream).await.unwrap();
        let mut connection = h2::server::handshake(stream).await.unwrap();
        let mut held_body = None;
        while let Some(Ok((request, mut respond))) = connection.accept().await {
            let response = http::Response::builder().status(200).body(()).unwrap();
            let mut body = respond.send_response(response, false).unwrap();
            match request.uri().path() {
                "/held" => held_body = Some(body),
                "/sibling" => {
                    body.send_data("b".into(), true).unwrap();
                    held_body
                        .take()
                        .unwrap()
                        .send_data("a".into(), true)
                        .unwrap();
                }
                path => panic!("unexpected request path {path}"),
            }
        }
    });
    let (h1_listener, mut h1_url, h1_address) = local_h2_origin().await;
    h1_url.set_scheme("http").unwrap();
    let h1_server = tokio::spawn(async move {
        let (mut stream, _) = h1_listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(stream.read_u8().await.unwrap());
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx")
            .await
            .unwrap();
        let (mut queued, _) = h1_listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(queued.read_u8().await.unwrap());
        }
        queued
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nq")
            .await
            .unwrap();
        assert_eq!(stream.read(&mut [0]).await.unwrap(), 0);
    });
    let transport = transport(2, Some(2));
    let mut held = transport
        .execute(request(url.clone(), "/held", address))
        .await
        .unwrap();
    let h1 = transport
        .execute(request(h1_url.clone(), "/held", h1_address))
        .await
        .unwrap();
    let queued = transport.execute(request(h1_url, "/queued", h1_address));
    tokio::pin!(queued);
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(queued.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;

    let mut sibling = tokio::time::timeout(
        Duration::from_secs(2),
        transport.execute(request(url, "/sibling", address)),
    )
    .await
    .expect("capacity pressure must not evict a multiplexable active H2 connection")
    .unwrap();
    assert_eq!(sibling.body.chunk().await.unwrap().unwrap().as_ref(), b"b");
    assert!(sibling.body.chunk().await.unwrap().is_none());
    assert_eq!(held.body.chunk().await.unwrap().unwrap().as_ref(), b"a");
    assert!(held.body.chunk().await.unwrap().is_none());
    let mut queued = tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .expect("the final H2 body must return idle capacity to an existing waiter")
        .unwrap();
    assert_eq!(queued.body.chunk().await.unwrap().unwrap().as_ref(), b"q");
    assert!(queued.body.chunk().await.unwrap().is_none());
    drop(h1.body);
    h1_server.await.unwrap();
    h2_server.abort();
}

#[tokio::test]
async fn reused_connection_emits_each_requests_own_headers_priority() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (listener, url, address) = local_h2_origin().await;
        let acceptor = h2_tls_acceptor();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(stream).await.unwrap();
            let mut preface = [0; 24];
            stream.read_exact(&mut preface).await.unwrap();
            assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
            stream
                .write_all(&[0, 0, 0, 4, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let mut priorities = Vec::new();
            while priorities.len() < 2 {
                let mut head = [0; 9];
                stream.read_exact(&mut head).await.unwrap();
                let length = u32::from_be_bytes([0, head[0], head[1], head[2]]) as usize;
                let mut payload = vec![0; length];
                stream.read_exact(&mut payload).await.unwrap();
                if head[3] == 4 && head[4] & 1 == 0 {
                    stream
                        .write_all(&[0, 0, 0, 4, 1, 0, 0, 0, 0])
                        .await
                        .unwrap();
                } else if head[3] == 1 {
                    assert_ne!(head[4] & 0x20, 0, "HEADERS must carry the request priority");
                    priorities.push((
                        u32::from_be_bytes(payload[..4].try_into().unwrap()),
                        payload[4],
                    ));
                    // HPACK 的静态 :status 200；空响应在这一个帧内完成。
                    let mut response = vec![0, 0, 1, 1, 5];
                    response.extend_from_slice(&head[5..9]);
                    response.push(0x88);
                    stream.write_all(&response).await.unwrap();
                }
            }
            priorities
        });
        let transport = transport(2, Some(1));
        for (path, dependency, weight, exclusive) in
            [("/first", 0, 17, true), ("/reused", 1, 219, false)]
        {
            let mut request = request(url.clone(), path, address);
            request.h2_priority = Some(moli_stealth_net::H2HeadersPriority {
                stream_dependency: dependency,
                weight,
                exclusive,
            });
            let mut response = transport.execute(request).await.unwrap();
            assert_eq!(response.status, 200);
            assert!(response.body.chunk().await.unwrap().is_none());
        }
        assert_eq!(server.await.unwrap(), [(0x8000_0000, 17), (1, 219)]);
    })
    .await
    .expect("sequential requests must share the established HTTP/2 connection");
}
