use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::Response,
    routing::{any, get},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Notify},
    task::JoinHandle,
};

#[derive(Default)]
pub struct Observations {
    pub requests: Mutex<Vec<(String, HeaderMap)>>,
    pub stream_continue: Notify,
    pub stream_dropped: Notify,
    pub request_started: Notify,
    pub request_dropped: Notify,
    pub echo_seen: Notify,
}

pub struct Fixture {
    pub address: SocketAddr,
    pub observations: Arc<Observations>,
    task: JoinHandle<()>,
}

impl Fixture {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let observations = Arc::new(Observations::default());
        let router = Router::new()
            .route("/echo", any(echo))
            .route("/stream", get(stream))
            .route("/hold", get(hold))
            .route("/redirect", any(|| async {
                Response::builder().status(307).header("location", "/echo").body(Body::empty()).unwrap()
            }))
            .route("/error", get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, [("content-type", "text/html")], "<!doctype html><title>HTTP error document</title><body id='error'>retained error page</body>") }))
            .route("/page", get(|| async { ([("content-type", "text/html")], PAGE) }))
            .route("/cookie-view", get(cookie_view))
            .route("/resources", get(|| async { ([("content-type", "text/html")], "<!doctype html><body><img src='/pixel.svg'><p>resource policy</p></body>") }))
            .route("/pixel.svg", get(pixel))
            .route("/frame", get(|| async { ([("content-type", "text/html")], "<body><script>parent.postMessage('frame-ready', '*')</script>child frame</body>") }))
            .route("/worker.js", get(|| async { ([("content-type", "application/javascript")], "postMessage('worker-ready')") }))
            .route("/tamper", get(|| async { ([("content-type", "text/html")], TAMPER) }))
            .route("/navigate", get(|| async { ([("content-type", "text/html")], "<!doctype html><body id='old'><script>location.replace('/page')</script>old document</body>") }))
            .route("/font", get(|| async { ([("content-type", "text/html")], FONT) }))
            .with_state(observations.clone());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            address,
            observations,
            task,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn echo(
    State(state): State<Arc<Observations>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    state.requests.lock().await.push(("echo".into(), headers));
    state.echo_seen.notify_one();
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        "x-fixture-connection",
        HeaderValue::from_str(&peer.to_string()).unwrap(),
    );
    response.headers_mut().append(
        "set-cookie",
        HeaderValue::from_static("first=one; Path=/; SameSite=Lax"),
    );
    response.headers_mut().append(
        "set-cookie",
        HeaderValue::from_static("second=two; Path=/; SameSite=Lax"),
    );
    response
}

async fn cookie_view(headers: HeaderMap) -> impl axum::response::IntoResponse {
    let cookies = headers
        .get("cookie")
        .map_or("", |value| value.to_str().unwrap());
    (
        [("content-type", "text/html")],
        format!("<!doctype html><body>{cookies}</body>"),
    )
}

async fn pixel(
    State(state): State<Arc<Observations>>,
    headers: HeaderMap,
) -> impl axum::response::IntoResponse {
    state.requests.lock().await.push(("pixel".into(), headers));
    (
        [("content-type", "image/svg+xml")],
        "<svg xmlns='http://www.w3.org/2000/svg' width='2' height='2'><rect width='2' height='2' fill='red'/></svg>",
    )
}

// 服务端不发送 EOF，直到消费者确实观察到首块并释放门闩。
async fn stream(State(state): State<Arc<Observations>>) -> Response {
    struct Lifetime(Arc<Observations>);
    impl Drop for Lifetime {
        fn drop(&mut self) {
            self.0.stream_dropped.notify_one();
        }
    }
    let body = futures_util::stream::unfold((0_u8, Lifetime(state)), |(step, owner)| async move {
        match step {
            0 => Some((
                Ok::<_, Infallible>(Bytes::from_static(b"\x00\xfffirst")),
                (1, owner),
            )),
            1 => {
                owner.0.stream_continue.notified().await;
                Some((Ok(Bytes::from_static(b"last\x00")), (2, owner)))
            }
            _ => None,
        }
    });
    Response::new(Body::from_stream(body))
}

async fn hold(State(state): State<Arc<Observations>>) -> Response {
    struct PendingRequest(Arc<Observations>);
    impl Drop for PendingRequest {
        fn drop(&mut self) {
            self.0.request_dropped.notify_one();
        }
    }
    state.request_started.notify_one();
    let _request = PendingRequest(state);
    std::future::pending().await
}

const PAGE: &str = r#"<!doctype html><title>SDK document</title><body id="ready"><p>rendered document 中文</p><script>
document.cookie = 'dynamic=present; Path=/; SameSite=Lax';
window.addEventListener('message', e => { if(e.data === 'frame-ready') document.body.dataset.frame = 'ready'; });
const worker = new Worker('/worker.js'); worker.onmessage = e => { document.body.dataset.worker = e.data; };
</script><iframe src="/frame"></iframe></body>"#;

const TAMPER: &str = r#"<!doctype html><title>isolated document</title><body id="real">real DOM<script>
window.JSON = { stringify() { throw new Error('page JSON poisoned'); } };
window.XMLSerializer = function() { throw new Error('page serializer poisoned'); };
Object.defineProperty(window, 'document', { value: { documentElement: { outerHTML: 'fake DOM' } }, configurable: true });
</script></body>"#;

const FONT: &str = r#"<!doctype html><meta charset="utf-8"><style>
html,body { margin:0; background:white; color:black; }
span { display:inline-block; font-size:48px; line-height:64px; }
i { display:inline-block; font-style:normal; }
#latin { font-family:'DejaVu Sans'; } #cjk { font-family:'Noto Sans CJK SC'; }
#fallback { font-family:'SDK intentionally absent font','Noto Sans CJK SC'; }
</style><span id="latin"><i>W</i><i>i</i><i>M</i><i>m</i></span><br><span id="cjk">中文国</span><br><span id="fallback">中文国</span>"#;

pub struct TlsFixture {
    pub address: SocketAddr,
    task: JoinHandle<()>,
}

fn tls_acceptor(protocols: Vec<Vec<u8>>) -> tokio_rustls::TlsAcceptor {
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certificate.key_pair.serialize_der(),
    ));
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![certificate.cert.der().clone()], key)
    .unwrap();
    config.alpn_protocols = protocols;
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

impl TlsFixture {
    pub async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let acceptor = tls_acceptor(Vec::new());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap();
                        let acceptor = acceptor.clone();
                        connections.spawn(async move {
                            // 默认信任策略应拒绝此自签证书，握手失败是受控测试结果。
                            let Ok(mut stream) = acceptor.accept(socket).await else { return };
                            let mut request = Vec::new();
                            while !request.ends_with(b"\r\n\r\n") {
                                let mut byte = [0];
                                if stream.read_exact(&mut byte).await.is_err() { return; }
                                request.push(byte[0]);
                                assert!(request.len() <= 8192, "unexpected oversized fixture request");
                            }
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nsecure-body").await;
                            let _ = stream.shutdown().await;
                        });
                    }
                    result = connections.join_next(), if !connections.is_empty() => {
                        result.unwrap().unwrap();
                    }
                }
            }
        });
        Self { address, task }
    }
}

impl Drop for TlsFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct H2Fixture {
    pub address: SocketAddr,
    pub closed: tokio::sync::oneshot::Receiver<()>,
    task: JoinHandle<()>,
}

impl H2Fixture {
    pub async fn start() -> Self {
        let acceptor = tls_acceptor(vec![b"h2".to_vec()]);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (closed, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let stream = acceptor.accept(socket).await.unwrap();
            let mut connection = h2::server::handshake(stream).await.unwrap();
            let mut streams = Vec::new();
            while let Some(incoming) = connection.accept().await {
                let (_, mut respond) = match incoming {
                    Ok(request) => request,
                    Err(error) => {
                        assert!(error.is_io() || error.is_go_away(), "{error}");
                        break;
                    }
                };
                let response = axum::http::Response::builder()
                    .status(200)
                    .body(())
                    .unwrap();
                let mut body = respond.send_response(response, false).unwrap();
                body.send_data(vec![0, 255, 4, 5].into(), false).unwrap();
                streams.push(body);
            }
            drop(streams);
            drop(connection);
            let _ = closed.send(());
        });
        Self {
            address,
            closed: receiver,
            task,
        }
    }
}

impl Drop for H2Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
