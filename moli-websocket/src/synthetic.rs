use tokio::sync::mpsc;

use crate::{
    Command, Event, FrameOpcode,
    events::{EventSender, send_error_and_close, send_event},
    limits::acquire_websocket_connection_slot,
};

pub(crate) async fn run_synthetic_websocket_connection(
    socket_id: u64,
    mut command_rx: mpsc::UnboundedReceiver<Command>,
    event_tx: EventSender,
    request_headers: Vec<(String, String)>,
    response_status: u16,
    response_headers: Vec<(String, String)>,
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

    while let Some(command) = command_rx.recv().await {
        match command {
            Command::SendText(text) => {
                let amount = text.len();
                let _ = send_event(
                    &event_tx,
                    Event::FrameSent {
                        socket_id,
                        opcode: FrameOpcode::Text,
                        payload_length: amount,
                    },
                )
                .await;
                let _ = send_event(
                    &event_tx,
                    Event::BufferedAmountConsumed { socket_id, amount },
                )
                .await;
            }
            Command::SendBinary(bytes) => {
                let amount = bytes.len();
                let _ = send_event(
                    &event_tx,
                    Event::FrameSent {
                        socket_id,
                        opcode: FrameOpcode::Binary,
                        payload_length: amount,
                    },
                )
                .await;
                let _ = send_event(
                    &event_tx,
                    Event::BufferedAmountConsumed { socket_id, amount },
                )
                .await;
            }
            Command::ReceiveText(data) => {
                let _ = send_event(&event_tx, Event::TextMessage { socket_id, data }).await;
            }
            Command::ReceiveBinary(data) => {
                let _ = send_event(&event_tx, Event::BinaryMessage { socket_id, data }).await;
            }
            Command::ServerClose { code, reason } => {
                let close_event_code = code.unwrap_or(1005);
                let close_event_reason = code.map(|_| reason).unwrap_or_default();
                let _ = send_event(
                    &event_tx,
                    Event::Close {
                        socket_id,
                        code: close_event_code,
                        reason: close_event_reason,
                        was_clean: true,
                    },
                )
                .await;
                return;
            }
            Command::Close { code, reason } => {
                let close_event_code = code.unwrap_or(1005);
                let close_event_reason = code.map(|_| reason).unwrap_or_default();
                let _ = send_event(&event_tx, Event::Closing { socket_id }).await;
                let _ = send_event(
                    &event_tx,
                    Event::Close {
                        socket_id,
                        code: close_event_code,
                        reason: close_event_reason,
                        was_clean: true,
                    },
                )
                .await;
                return;
            }
            Command::ContinueOpen { .. } => {}
            Command::FailOpen(message) => {
                send_error_and_close(&event_tx, socket_id, message).await;
                return;
            }
        }
    }

    // Owner retirement can close the command channel before task cancellation
    // is observed. The vanished consumer must not receive a synthetic close.
}

fn response_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}
