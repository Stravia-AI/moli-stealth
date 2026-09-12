use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use futures_util::{SinkExt, StreamExt};
use moli_stealth_net::TlsConfig;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use tokio::{
    net::TcpListener,
    task::JoinHandle,
    time::{Duration, timeout},
};
use tokio_rustls::rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer},
    server::WebPkiClientVerifier,
};
use tokio_tungstenite::tungstenite::Message;

/// Private trust and client identity files for real WSS connection tests.
pub struct TlsWebSocketFixture {
    dir: PathBuf,
    roots: RootCertStore,
    server_certificate: CertificateDer<'static>,
    server_key: PrivatePkcs8KeyDer<'static>,
    pub client_certificate: Vec<u8>,
}

impl Default for TlsWebSocketFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl TlsWebSocketFixture {
    pub fn new() -> Self {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "Moli WSS test CA");
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let server_key = KeyPair::generate().unwrap();
        let mut server_params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let server_cert = server_params.signed_by(&server_key, &ca, &ca_key).unwrap();
        let client_key = KeyPair::generate().unwrap();
        let mut client_params = CertificateParams::new(Vec::new()).unwrap();
        client_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let client_cert = client_params.signed_by(&client_key, &ca, &ca_key).unwrap();

        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "moli-websocket-tls-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
        fs::write(dir.join("client.pem"), client_cert.pem()).unwrap();
        fs::write(dir.join("client-key.pem"), client_key.serialize_pem()).unwrap();

        let mut roots = RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        Self {
            dir,
            roots,
            server_certificate: server_cert.der().clone(),
            server_key: server_key.serialize_der().into(),
            client_certificate: client_cert.der().to_vec(),
        }
    }

    pub fn tls_config(&self) -> TlsConfig {
        TlsConfig {
            ca_cert: Some(self.dir.join("ca.pem")),
            client_cert: Some(self.dir.join("client.pem")),
            client_key: Some(self.dir.join("client-key.pem")),
            ..TlsConfig::default()
        }
    }

    /// Echoes until a clean WebSocket close and returns the actual TLS peer chain.
    pub async fn spawn(
        &self,
        require_identity: bool,
    ) -> (String, JoinHandle<Result<Vec<Vec<u8>>, String>>) {
        let verifier = WebPkiClientVerifier::builder(Arc::new(self.roots.clone()));
        let verifier = if require_identity {
            verifier
        } else {
            verifier.allow_unauthenticated()
        };
        let config = ServerConfig::builder()
            .with_client_cert_verifier(verifier.build().unwrap())
            .with_single_cert(
                vec![self.server_certificate.clone()],
                self.server_key.clone_key().into(),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            timeout(Duration::from_secs(10), async move {
                let (tcp, _) = listener.accept().await.map_err(|error| error.to_string())?;
                let stream = acceptor
                    .accept(tcp)
                    .await
                    .map_err(|error| format!("TLS: {error}"))?;
                let certificates = stream
                    .get_ref()
                    .1
                    .peer_certificates()
                    .unwrap_or_default()
                    .iter()
                    .map(|cert| cert.to_vec())
                    .collect();
                let mut socket = tokio_tungstenite::accept_async(stream)
                    .await
                    .map_err(|error| format!("Upgrade: {error}"))?;
                while let Some(message) = socket.next().await {
                    match message.map_err(|error| error.to_string())? {
                        message @ (Message::Text(_) | Message::Binary(_)) => socket
                            .send(message)
                            .await
                            .map_err(|error| error.to_string())?,
                        Message::Close(_) => {
                            socket.flush().await.map_err(|error| error.to_string())?;
                            return Ok(certificates);
                        }
                        _ => {}
                    }
                }
                Err("WebSocket ended without a Close frame".to_owned())
            })
            .await
            .map_err(|error| format!("TLS fixture deadline: {error}"))?
        });
        (format!("wss://localhost:{port}/tls"), task)
    }
}

impl Drop for TlsWebSocketFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
