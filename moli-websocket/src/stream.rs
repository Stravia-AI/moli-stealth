use moli_stealth_net::{ConnectionOptions, open_connection, process_fingerprint};
use url::Url;

use crate::{
    ConnectOptions,
    handshake::{BrowserWebSocket, browser_client_handshake},
};

type Response = http::Response<()>;

pub(crate) async fn open_websocket_stream(
    request: http::Request<()>,
    context: &ConnectOptions,
) -> Result<(BrowserWebSocket, Response), String> {
    let url = Url::parse(&request.uri().to_string())
        .map_err(|error| format!("invalid WebSocket URL: {error}"))?;
    let connection = ConnectionOptions {
        resolved_addresses: None,
        proxy: context.http_proxy.clone(),
        no_proxy: context.http_no_proxy.clone(),
        proxy_bearer_token: context.proxy_bearer_token.clone(),
        proxy_auth: None,
        connect_timeout: None,
        tls_session_cache: context.tls_session_cache.clone(),
    };
    let connected = open_connection(
        &url,
        &connection,
        process_fingerprint(),
        context.tls_verify_host,
        true,
        true,
    )
    .await
    .map_err(|error| error.to_string())?;
    browser_client_handshake(request, connected.stream).await
}
