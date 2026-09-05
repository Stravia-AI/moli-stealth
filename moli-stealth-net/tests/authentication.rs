use std::{net::SocketAddr, time::Duration};

use moli_stealth_net::{AuthScheme, Transport, TransportAuth, TransportConfig, TransportRequest};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use url::Url;

const TIMEOUT: Duration = Duration::from_secs(2);

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

fn request_head(request: &[u8]) -> &str {
    let end = request
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap()
        + 4;
    std::str::from_utf8(&request[..end]).unwrap()
}

fn request_body(request: &[u8]) -> &[u8] {
    let start = request
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .unwrap()
        + 4;
    &request[start..]
}

fn header<'a>(request: &'a [u8], wanted: &str) -> Option<&'a str> {
    request_head(request).lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(wanted).then(|| value.trim())
    })
}

fn digest_parameter<'a>(authorization: &'a str, wanted: &str) -> Option<&'a str> {
    authorization
        .strip_prefix("Digest ")?
        .split(',')
        .find_map(|field| {
            let (name, value) = field.trim().split_once('=')?;
            name.eq_ignore_ascii_case(wanted)
                .then(|| value.trim().trim_matches('"'))
        })
}

fn digest_auth(username: &str, password: &str) -> TransportAuth {
    TransportAuth {
        scheme: AuthScheme::Digest,
        username: username.into(),
        password: password.into(),
    }
}

#[tokio::test]
async fn digest_retry_preserves_custom_method_binary_body_and_streams_final_response() {
    let (listener, mut url, address) = local_server().await;
    url.set_query(Some("part=two"));
    let body = vec![0, 255, 1, 128, 2];
    let expected_body = body.clone();
    let (release_tx, release_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let initial = read_request(&mut stream).await;
        assert!(request_head(&initial).starts_with("X-CUSTOM /resource?part=two HTTP/1.1\r\n"));
        assert_eq!(request_body(&initial), expected_body);
        assert_eq!(header(&initial, "authorization"), None);
        stream
            .write_all(
                concat!(
                    "HTTP/1.1 401 Unauthorized\r\n",
                    "WWW-Authenticate: Digest realm=\"testrealm@host.com\", ",
                    "nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", algorithm=MD5\r\n",
                    "Content-Length: 0\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        let authenticated = read_request(&mut stream).await;
        assert!(
            request_head(&authenticated).starts_with("X-CUSTOM /resource?part=two HTTP/1.1\r\n")
        );
        assert_eq!(request_body(&authenticated), expected_body);
        let authorization = header(&authenticated, "authorization").unwrap();
        assert_eq!(digest_parameter(authorization, "username"), Some("Mufasa"));
        assert_eq!(
            digest_parameter(authorization, "realm"),
            Some("testrealm@host.com")
        );
        assert_eq!(
            digest_parameter(authorization, "uri"),
            Some("/resource?part=two")
        );
        // Independently precomputed from RFC 7616's MD5 formula for this method and target.
        assert_eq!(
            digest_parameter(authorization, "response"),
            Some("9d99d73e24c6da8b5fab5753fe3542a3")
        );
        stream
            .write_all(
                concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "X-Authenticated: yes\r\n",
                    "Transfer-Encoding: chunked\r\n\r\n",
                    "5\r\nfirst\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        release_rx.await.unwrap();
        stream.write_all(b"4\r\nlast\r\n0\r\n\r\n").await.unwrap();
    });

    let transport = Transport::new(TransportConfig::default()).unwrap();
    let mut request = TransportRequest::new(url, "X-CUSTOM");
    request.body = Some(body);
    request.auth = Some(digest_auth("Mufasa", "Circle Of Life"));
    request.connection.resolved_addresses = Some(vec![address]);
    request.connection.proxy = Some(String::new());

    let mut response = tokio::time::timeout(TIMEOUT, transport.execute(request))
        .await
        .expect("authentication and final response head must not hang")
        .unwrap();
    assert_eq!(response.status, 200);
    assert!(
        response.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("x-authenticated") && value == "yes"
        })
    );
    assert_eq!(
        tokio::time::timeout(TIMEOUT, response.body.chunk())
            .await
            .expect("first final-response chunk must be available before completion")
            .unwrap()
            .unwrap()
            .as_ref(),
        b"first"
    );
    release_tx.send(()).unwrap();
    assert_eq!(
        response.body.chunk().await.unwrap().unwrap().as_ref(),
        b"last"
    );
    assert!(response.body.chunk().await.unwrap().is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn digest_is_selected_from_multiple_challenges_and_refreshes_a_stale_nonce() {
    let (listener, url, address) = local_server().await;
    let server =
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let initial = read_request(&mut stream).await;
            assert_eq!(header(&initial, "authorization"), None);
            stream
            .write_all(concat!(
                "HTTP/1.1 401 Unauthorized\r\n",
                "WWW-Authenticate: Basic realm=\"fallback\", Digest realm=\"stale-realm\", ",
                "nonce=\"old-nonce\", algorithm=MD5\r\n",
                "Content-Length: 0\r\n\r\n"
            ).as_bytes())
            .await
            .unwrap();

            let old_nonce = read_request(&mut stream).await;
            let authorization = header(&old_nonce, "authorization").unwrap();
            assert_eq!(digest_parameter(authorization, "nonce"), Some("old-nonce"));
            assert_eq!(
                digest_parameter(authorization, "response"),
                Some("f68ea1a79bff62909fdd828d9549b3b5")
            );
            stream
                .write_all(
                    concat!(
                        "HTTP/1.1 401 Unauthorized\r\n",
                        "WWW-Authenticate: Digest realm=\"stale-realm\", nonce=\"fresh-nonce\", ",
                        "algorithm=MD5, stale=true\r\n",
                        "Content-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();

            let fresh_nonce = read_request(&mut stream).await;
            let authorization = header(&fresh_nonce, "authorization").unwrap();
            assert_eq!(
                digest_parameter(authorization, "nonce"),
                Some("fresh-nonce")
            );
            assert_eq!(
                digest_parameter(authorization, "response"),
                Some("b5ca654a0e5b586244e08cede8ab2176")
            );
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });

    let transport = Transport::new(TransportConfig::default()).unwrap();
    let mut request = TransportRequest::new(url, "PUT");
    request.auth = Some(digest_auth("Aladdin", "open sesame"));
    request.connection.resolved_addresses = Some(vec![address]);
    request.connection.proxy = Some(String::new());

    let response = tokio::time::timeout(TIMEOUT, transport.execute(request))
        .await
        .expect("multi-challenge and stale-nonce negotiation must not hang")
        .unwrap();
    assert_eq!(response.status, 204);
    server.await.unwrap();
}
