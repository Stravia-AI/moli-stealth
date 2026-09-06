use bytes::BytesMut;
use ratchet_rs::{
    CloseCode, CloseReason, Message,
    deflate::{DeflateDecoder, DeflateEncoder},
};
use tokio::sync::mpsc;

use crate::{
    Command, ConnectOptions, Event, FrameOpcode,
    events::{EventSender, send_error_and_close, send_event},
    headers::{header_map_entries, request_header_entries},
    limits::{acquire_pending_websocket_handshake_slot, acquire_websocket_connection_slot},
    request::build_websocket_request,
    stream::open_websocket_stream,
};

pub(crate) async fn run_websocket_connection(
    socket_id: u64,
    url: String,
    protocols: Vec<String>,
    context: ConnectOptions,
    mut command_rx: mpsc::UnboundedReceiver<Command>,
    event_tx: EventSender,
) {
    let Some(_connection_slot) = acquire_websocket_connection_slot() else {
        send_error_and_close(
            &event_tx,
            socket_id,
            "WebSocket connection failed: insufficient resources".to_owned(),
        )
        .await;
        return;
    };

    let request = match build_websocket_request(&url, &protocols, &context) {
        Ok(request) => request,
        Err(error) => {
            send_error_and_close(&event_tx, socket_id, error).await;
            return;
        }
    };

    let request_headers = request_header_entries(request.headers());
    let Some(pending_handshake_slot) = acquire_pending_websocket_handshake_slot() else {
        send_error_and_close(
            &event_tx,
            socket_id,
            "WebSocket connection failed: too many pending handshakes".to_owned(),
        )
        .await;
        return;
    };
    let handshake = open_websocket_stream(request, &context);
    tokio::pin!(handshake);
    let (stream, response) = loop {
        tokio::select! {
            biased;
            command = command_rx.recv() => {
                match command {
                    Some(Command::Close { .. }) => {
                        drop(pending_handshake_slot);
                        send_error_and_close(
                            &event_tx,
                            socket_id,
                            "WebSocket connection closed before opening".to_owned(),
                        )
                        .await;
                        return;
                    }
                    Some(Command::SendText(_))
                    | Some(Command::SendBinary(_))
                    | Some(Command::ReceiveText(_))
                    | Some(Command::ReceiveBinary(_))
                    | Some(Command::ServerClose { .. }) => {
                        // Browser-visible `send()` throws while CONNECTING, so these commands
                        // should only appear from direct crate users. Ignore them rather than
                        // queueing frames before the opening handshake has succeeded.
                    }
                    Some(Command::ContinueOpen { .. }) | Some(Command::FailOpen(_)) => {}
                    None => return,
                }
            }
            connected = &mut handshake => {
                match connected {
                    Ok(connected) => {
                        drop(pending_handshake_slot);
                        break connected;
                    }
                    Err(error) => {
                        drop(pending_handshake_slot);
                        send_error_and_close(
                            &event_tx,
                            socket_id,
                            format!("WebSocket connection failed: {error}"),
                        )
                        .await;
                        return;
                    }
                }
            }
        }
    };

    let mut response_status = response.status().as_u16();
    let mut response_headers = header_map_entries(response.headers());
    if context.pause_after_handshake {
        let _ = send_event(
            &event_tx,
            Event::HandshakeResponse {
                socket_id,
                protocol: response_header(&response_headers, "sec-websocket-protocol")
                    .unwrap_or_default()
                    .to_owned(),
                extensions: response_header(&response_headers, "sec-websocket-extensions")
                    .unwrap_or_default()
                    .to_owned(),
                request_headers: request_headers.clone(),
                response_status,
                response_headers: response_headers.clone(),
            },
        )
        .await;
        loop {
            match command_rx.recv().await {
                Some(Command::ContinueOpen {
                    response_status: override_status,
                    response_headers: override_headers,
                }) => {
                    if let Some(override_status) = override_status {
                        response_status = override_status;
                    }
                    if let Some(override_headers) = override_headers {
                        response_headers = override_headers;
                    }
                    break;
                }
                Some(Command::FailOpen(message)) => {
                    send_error_and_close(&event_tx, socket_id, message).await;
                    return;
                }
                Some(Command::Close { .. }) => {
                    send_error_and_close(
                        &event_tx,
                        socket_id,
                        "WebSocket connection closed before opening".to_owned(),
                    )
                    .await;
                    return;
                }
                Some(Command::SendText(_))
                | Some(Command::SendBinary(_))
                | Some(Command::ReceiveText(_))
                | Some(Command::ReceiveBinary(_))
                | Some(Command::ServerClose { .. }) => {
                    // Browser-visible `send()` throws until the open event, so crate users
                    // cannot enqueue application data while a response-stage pause is active.
                }
                None => return,
            }
        }
    }
    let protocol = response_header(&response_headers, "sec-websocket-protocol")
        .unwrap_or_default()
        .to_owned();
    let extensions = response_header(&response_headers, "sec-websocket-extensions")
        .unwrap_or_default()
        .to_owned();
    let _ = send_event(
        &event_tx,
        Event::Open {
            socket_id,
            protocol,
            extensions,
            request_headers,
            response_status,
            response_headers,
        },
    )
    .await;

    run_open_websocket_connection(socket_id, stream, command_rx, event_tx).await;
}

async fn run_open_websocket_connection(
    socket_id: u64,
    stream: crate::handshake::BrowserWebSocket,
    command_rx: mpsc::UnboundedReceiver<Command>,
    event_tx: EventSender,
) {
    let (write, read) = match stream.split() {
        Ok(parts) => parts,
        Err(error) => {
            send_error_and_close(
                &event_tx,
                socket_id,
                format!("WebSocket codec split failed: {error}"),
            )
            .await;
            return;
        }
    };
    let (reader_event_tx, mut reader_event_rx) = mpsc::channel(1);
    let reader = tokio::spawn(run_websocket_reader(read, reader_event_tx));
    let (writer_event_tx, mut writer_event_rx) = mpsc::unbounded_channel();
    let writer = tokio::spawn(run_websocket_writer(write, command_rx, writer_event_tx));
    let mut sent_close: Option<(u16, String)> = None;
    let mut writer_done = false;
    loop {
        tokio::select! {
            biased;
            // Incoming close/error frames should decide browser-visible state before
            // a concurrent writer-side send failure caused by the same remote close.
            message = reader_event_rx.recv() => {
                match message {
                    Some(ReaderEvent::Text(text)) => {
                        let _ = send_event(&event_tx, Event::TextMessage {
                            socket_id,
                            data: text,
                        })
                        .await;
                    }
                    Some(ReaderEvent::Binary(data)) => {
                        let _ = send_event(&event_tx, Event::BinaryMessage {
                            socket_id,
                            data,
                        })
                        .await;
                    }
                    Some(ReaderEvent::Close { code, reason }) => {
                        let _ = send_event(
                            &event_tx,
                            Event::Close {
                                socket_id,
                                code,
                                reason,
                                was_clean: true,
                            },
                        )
                        .await;
                        break;
                    }
                    Some(ReaderEvent::Control) => {}
                    Some(ReaderEvent::Error(error)) => {
                        // The main loop's biased reader-event arm wins races with
                        // `writer_event_rx`. Under load the client-initiated
                        // `Command::Close` can already be sitting in
                        // `writer_event_rx` as `WebSocketWriterEvent::Closing` by
                        // the time the server's TCP reset surfaces as a reader
                        // `Err`. Without draining the writer queue here we'd see
                        // `sent_close == None`, take the "unexpected error" path,
                        // and report `wasClean=false` plus a spurious `error`
                        // event — even though the JS caller had explicitly invoked
                        // `socket.close(...)`. Drain pending writer events first so
                        // the close classification reflects the caller's intent.
                        if matches!(
                            drain_pending_writer_events(
                                &mut writer_event_rx,
                                &event_tx,
                                socket_id,
                                &mut sent_close,
                                &mut writer_done,
                            )
                            .await,
                            WriterEventOutcome::Terminate
                        ) {
                            // The drain itself surfaced a writer `Error` and
                            // already emitted the `Error` + `Close` terminal
                            // events — don't emit a second terminal close
                            // (which would otherwise mask the real failure
                            // with a clean-looking `wasClean=true`).
                            break;
                        }
                        if let Some((code, reason)) = sent_close.clone() {
                            // Many servers reset the socket after receiving our close frame.
                            // Browser-observable state treats our initiated close as clean.
                            let _ = send_event(
                                &event_tx,
                                Event::Close {
                                    socket_id,
                                    code,
                                    reason,
                                    was_clean: true,
                                },
                            )
                            .await;
                        } else {
                            send_error_and_close(
                                &event_tx,
                                socket_id,
                                format!("WebSocket receive failed: {error}"),
                            )
                            .await;
                        }
                        break;
                    }
                    None => {
                        // Same race protection as the `Some(Err(_))` arm above:
                        // an EOF on the reader side can land before the writer's
                        // `Closing` event reaches us, so drain pending writer
                        // events to recover the caller's close intent.
                        if matches!(
                            drain_pending_writer_events(
                                &mut writer_event_rx,
                                &event_tx,
                                socket_id,
                                &mut sent_close,
                                &mut writer_done,
                            )
                            .await,
                            WriterEventOutcome::Terminate
                        ) {
                            break;
                        }
                        let (code, reason, was_clean) = sent_close
                            .clone()
                            .map(|(code, reason)| (code, reason, true))
                            .unwrap_or((1006, String::new(), false));
                        let _ = send_event(
                            &event_tx,
                            Event::Close {
                                socket_id,
                                code,
                                reason,
                                was_clean,
                            },
                        )
                        .await;
                        break;
                    }
                }
            }
            writer_event = writer_event_rx.recv(), if !writer_done => {
                let Some(writer_event) = writer_event else {
                    writer_done = true;
                    continue;
                };
                if matches!(
                    handle_writer_event(
                        writer_event,
                        &event_tx,
                        socket_id,
                        &mut sent_close,
                        &mut writer_done,
                    )
                    .await,
                    WriterEventOutcome::Terminate
                ) {
                    break;
                }
            }
        }
    }
    reader.abort();
    writer.abort();
}

enum ReaderEvent {
    Text(String),
    Binary(Vec<u8>),
    Control,
    Close { code: u16, reason: String },
    Error(String),
}

enum WebSocketWriterEvent {
    FrameSent {
        opcode: FrameOpcode,
        payload_length: usize,
    },
    Closing {
        code: u16,
        reason: String,
    },
    Error(String),
    Done,
}

async fn run_websocket_reader(
    mut read: ratchet_rs::Receiver<moli_stealth_net::BoxedStream, DeflateDecoder>,
    reader_event_tx: mpsc::Sender<ReaderEvent>,
) {
    let mut buffer = BytesMut::new();
    loop {
        let event = match read.read(&mut buffer).await {
            Ok(Message::Text) => match std::str::from_utf8(&buffer) {
                Ok(text) => ReaderEvent::Text(text.to_owned()),
                Err(error) => ReaderEvent::Error(format!(
                    "WebSocket text message is not valid UTF-8: {error}"
                )),
            },
            Ok(Message::Binary) => ReaderEvent::Binary(buffer.to_vec()),
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => ReaderEvent::Control,
            Ok(Message::Close(reason)) => {
                let (code, reason) = reason
                    .map(|reason| {
                        (
                            u16::from(reason.code),
                            reason.description.unwrap_or_default(),
                        )
                    })
                    .unwrap_or((1005, String::new()));
                ReaderEvent::Close { code, reason }
            }
            Err(error) => ReaderEvent::Error(error.to_string()),
        };
        let terminal = matches!(event, ReaderEvent::Close { .. } | ReaderEvent::Error(_));
        let completed_message = matches!(event, ReaderEvent::Text(_) | ReaderEvent::Binary(_));
        if reader_event_tx.send(event).await.is_err() || terminal {
            return;
        }
        if completed_message {
            buffer.clear();
        }
    }
}

async fn run_websocket_writer(
    mut write: ratchet_rs::Sender<moli_stealth_net::BoxedStream, DeflateEncoder>,
    mut command_rx: mpsc::UnboundedReceiver<Command>,
    writer_event_tx: mpsc::UnboundedSender<WebSocketWriterEvent>,
) {
    while let Some(command) = command_rx.recv().await {
        if handle_websocket_writer_command(&mut write, command, &writer_event_tx).await {
            return;
        }
    }
    if let Err(error) = write.close(CloseReason::new(CloseCode::Normal, None)).await {
        let _ = writer_event_tx.send(WebSocketWriterEvent::Error(format!(
            "WebSocket close failed: {error}"
        )));
        return;
    }
    let _ = writer_event_tx.send(WebSocketWriterEvent::Done);
}

async fn handle_websocket_writer_command(
    write: &mut ratchet_rs::Sender<moli_stealth_net::BoxedStream, DeflateEncoder>,
    command: Command,
    writer_event_tx: &mpsc::UnboundedSender<WebSocketWriterEvent>,
) -> bool {
    match command {
        Command::SendText(text) => {
            let amount = text.len();
            match write.write_text(&text).await {
                Ok(()) => {
                    let _ = writer_event_tx.send(WebSocketWriterEvent::FrameSent {
                        opcode: FrameOpcode::Text,
                        payload_length: amount,
                    });
                }
                Err(error) => {
                    let _ = writer_event_tx.send(WebSocketWriterEvent::Error(format!(
                        "WebSocket send failed: {error}"
                    )));
                    return true;
                }
            }
        }
        Command::SendBinary(bytes) => {
            let amount = bytes.len();
            match write.write_binary(&bytes).await {
                Ok(()) => {
                    let _ = writer_event_tx.send(WebSocketWriterEvent::FrameSent {
                        opcode: FrameOpcode::Binary,
                        payload_length: amount,
                    });
                }
                Err(error) => {
                    let _ = writer_event_tx.send(WebSocketWriterEvent::Error(format!(
                        "WebSocket send failed: {error}"
                    )));
                    return true;
                }
            }
        }
        Command::ReceiveText(_) | Command::ReceiveBinary(_) | Command::ServerClose { .. } => {
            // Synthetic-only commands are consumed by the synthetic transport, not real sockets.
        }
        Command::Close { code, reason } => {
            let close_event_code = code.unwrap_or(1005);
            let close_event_reason = code.map(|_| reason.clone()).unwrap_or_else(String::new);
            let _ = writer_event_tx.send(WebSocketWriterEvent::Closing {
                code: close_event_code,
                reason: close_event_reason,
            });
            let close_code = match code {
                Some(code) => match CloseCode::try_from(code.to_be_bytes()) {
                    Ok(code) => code,
                    Err(error) => {
                        let _ = writer_event_tx.send(WebSocketWriterEvent::Error(format!(
                            "WebSocket close failed: {error}"
                        )));
                        return true;
                    }
                },
                None => CloseCode::Normal,
            };
            let description = code.map(|_| reason);
            if let Err(error) = write.close(CloseReason::new(close_code, description)).await {
                let _ = writer_event_tx.send(WebSocketWriterEvent::Error(format!(
                    "WebSocket close failed: {error}"
                )));
            }
            return true;
        }
        Command::ContinueOpen { .. } | Command::FailOpen(_) => {}
    }
    false
}

fn response_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Outcome of processing a single `WebSocketWriterEvent`. `Terminate` means
/// the event itself emitted the connection's final `Event::Error` +
/// `Event::Close` pair (today only the `Error` writer event does this), so
/// the caller should break out of the read/write loop without emitting any
/// further terminal events of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriterEventOutcome {
    Continue,
    Terminate,
}

/// Single-event processor shared by the main `tokio::select!` writer arm and
/// the reader-side terminal drain. Centralising the per-variant side effects
/// here keeps the two code paths in lock-step — when we add a new
/// `WebSocketWriterEvent` variant in the future, both call sites pick up the
/// new behaviour automatically.
async fn handle_writer_event(
    writer_event: WebSocketWriterEvent,
    event_tx: &EventSender,
    socket_id: u64,
    sent_close: &mut Option<(u16, String)>,
    writer_done: &mut bool,
) -> WriterEventOutcome {
    match writer_event {
        WebSocketWriterEvent::FrameSent {
            opcode,
            payload_length,
        } => {
            let _ = send_event(
                event_tx,
                Event::FrameSent {
                    socket_id,
                    opcode,
                    payload_length,
                },
            )
            .await;
            // 发送已完成；扣减不能依赖对端是否发送消息或关闭连接。
            let _ = send_event(
                event_tx,
                Event::BufferedAmountConsumed {
                    socket_id,
                    amount: payload_length,
                },
            )
            .await;
            WriterEventOutcome::Continue
        }
        WebSocketWriterEvent::Closing { code, reason } => {
            *sent_close = Some((code, reason));
            let _ = send_event(event_tx, Event::Closing { socket_id }).await;
            WriterEventOutcome::Continue
        }
        WebSocketWriterEvent::Error(message) => {
            send_error_and_close(event_tx, socket_id, message).await;
            WriterEventOutcome::Terminate
        }
        WebSocketWriterEvent::Done => {
            *writer_done = true;
            WriterEventOutcome::Continue
        }
    }
}

/// Drain whatever writer events have already been published into
/// `writer_event_rx` but haven't been processed by the main select loop yet.
/// Used by the reader's terminal arms (`Some(Err(_))` and `None`) so that a
/// `Closing` event published by the writer between the reader's terminal
/// signal landing in the OS socket and the main loop polling it doesn't get
/// dropped — without this drain the close would be reported with
/// `wasClean=false` plus a spurious `error` event, even when the JS caller
/// had explicitly invoked `socket.close(...)`.
///
/// Returns `Terminate` if the drain itself surfaced a writer `Error` (which
/// emits the final `Error` + `Close` pair on its own); the reader-side
/// caller must then break without emitting another terminal close, lest the
/// JS layer see `wasClean=true` despite the writer never actually putting
/// the close frame on the wire.
async fn drain_pending_writer_events(
    writer_event_rx: &mut mpsc::UnboundedReceiver<WebSocketWriterEvent>,
    event_tx: &EventSender,
    socket_id: u64,
    sent_close: &mut Option<(u16, String)>,
    writer_done: &mut bool,
) -> WriterEventOutcome {
    while let Ok(writer_event) = writer_event_rx.try_recv() {
        if matches!(
            handle_writer_event(writer_event, event_tx, socket_id, sent_close, writer_done,).await,
            WriterEventOutcome::Terminate
        ) {
            return WriterEventOutcome::Terminate;
        }
    }
    WriterEventOutcome::Continue
}
