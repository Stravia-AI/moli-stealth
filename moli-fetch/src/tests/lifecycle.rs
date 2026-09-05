use std::{num::NonZeroU32, sync::Arc};

use anyhow::{Context, Result};
use moli_cookie_jar::new_shared_browser_cookie_store;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Notify, oneshot},
};

use crate::{FetchCancelHandle, FetchClient, FetchConfig, Request};

async fn read_request_head(stream: &mut TcpStream) -> Result<()> {
    let mut request = Vec::new();
    let mut chunk = [0; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count != 0, "client closed before sending a request head");
        request.extend_from_slice(&chunk[..count]);
    }
    Ok(())
}

async fn write_backpressured_response(mut stream: TcpStream, body_started: oneshot::Sender<()>) {
    let head = b"HTTP/1.1 200 OK\r\nContent-Length: 1073741824\r\nContent-Type: text/plain\r\n\r\n";
    if read_request_head(&mut stream).await.is_err() || stream.write_all(head).await.is_err() {
        return;
    }
    let body = vec![b'x'; 64 * 1024];
    if stream.write_all(&body).await.is_err() {
        return;
    }
    let _ = body_started.send(());
    while stream.write_all(&body).await.is_ok() {}
}

async fn spawn_backpressure_then_success_server() -> Result<(
    String,
    oneshot::Receiver<()>,
    tokio::task::JoinHandle<Result<()>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/stream", listener.local_addr()?);
    let (body_started_tx, body_started_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (first, _) = listener.accept().await?;
        tokio::spawn(write_backpressured_response(first, body_started_tx));

        let (mut second, _) = listener.accept().await?;
        read_request_head(&mut second).await?;
        second
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await?;
        second.shutdown().await?;
        Ok(())
    });
    Ok((url, body_started_rx, server))
}

async fn spawn_backpressure_server()
-> Result<(String, oneshot::Receiver<()>, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/stream", listener.local_addr()?);
    let (body_started_tx, body_started_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            write_backpressured_response(stream, body_started_tx).await;
        }
    });
    Ok((url, body_started_rx, server))
}

fn single_active_request_config() -> FetchConfig {
    let mut config = FetchConfig::default();
    config.set_request_timeout_ms(0);
    config.set_connection_limits(NonZeroU32::new(1), None, None);
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_unread_stream_can_be_cancelled_while_backpressured() -> Result<()> {
    let (url, body_started, server) = spawn_backpressure_then_success_server().await?;
    let client = FetchClient::new(
        &single_active_request_config(),
        new_shared_browser_cookie_store(),
    );
    let cancel = FetchCancelHandle::new();
    let response = client
        .fetch_raw_stream_with_cancel(Request::get(&url)?, cancel.clone())
        .await?;
    body_started
        .await
        .context("server closed before starting the streaming body")?;

    cancel.cancel();
    let follow_up = client.fetch(Request::get(&url)?).await?;
    assert_eq!(follow_up.body_text(), "ok");

    drop(response);
    assert!(client.shutdown().is_clean());
    server.await.context("backpressure server task failed")??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_aborts_retained_unread_html_stream_while_backpressured() -> Result<()> {
    let (url, body_started, server) = spawn_backpressure_server().await?;
    let client = FetchClient::new(
        &single_active_request_config(),
        new_shared_browser_cookie_store(),
    );
    let response = client.fetch_html_stream(Request::get(&url)?).await?;
    body_started
        .await
        .context("server closed before starting the streaming body")?;

    assert!(client.shutdown().is_clean());

    drop(response);
    server.await.context("backpressure server task failed")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_request_without_timeout_observes_explicit_cancellation() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/queued", listener.local_addr()?);
    let release_first = Arc::new(Notify::new());
    let server_release = Arc::clone(&release_first);
    let (first_admitted_tx, first_admitted_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await?;
        read_request_head(&mut first).await?;
        let _ = first_admitted_tx.send(());
        server_release.notified().await;
        first
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await?;
        first.shutdown().await?;
        Ok::<_, anyhow::Error>(())
    });

    let client = FetchClient::new(
        &single_active_request_config(),
        new_shared_browser_cookie_store(),
    );
    let handle = client.handle();
    let first = tokio::spawn({
        let handle = handle.clone();
        let url = url.clone();
        async move { handle.fetch(Request::get(&url)?).await }
    });
    first_admitted_rx
        .await
        .context("first request was not admitted by the server")?;

    let queued_cancel = FetchCancelHandle::new();
    let queued = handle.fetch_with_cancel(Request::get(&url)?, queued_cancel.clone());
    tokio::pin!(queued);
    tokio::select! {
        biased;
        result = &mut queued => {
            panic!("the second request did not remain queued behind the first: {result:?}");
        }
        _ = tokio::task::yield_now() => {}
    }

    queued_cancel.cancel();
    let error = queued
        .await
        .expect_err("cancelled queued request must not reach the server");
    assert!(
        format!("{error:#}").contains("fetch runtime request cancelled"),
        "unexpected queued cancellation error: {error:#}"
    );

    release_first.notify_one();
    first.await.context("first request task failed")??;
    assert!(client.shutdown().is_clean());
    server
        .await
        .context("queued request server task failed")??;
    Ok(())
}
