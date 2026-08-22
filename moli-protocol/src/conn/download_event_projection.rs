use std::sync::Arc;

use parking_lot::Mutex;

use super::{BackgroundEventSender, BackgroundProtocolEvent, CommandResponseFlushContext};

/// A bounded frontend projection gate for one accepted download action.
///
/// Download network and artifact work must not wait for a DevTools command
/// response. Before that response enters the output sequence, this gate keeps
/// only the fixed start prefix and the latest progress batch. Releasing the
/// response boundary publishes those batches in order; subsequent progress is
/// forwarded directly. A slow or disconnected frontend therefore cannot
/// block Browser progress or make pre-response buffering grow with body size.
#[derive(Clone)]
pub(super) struct DownloadBackgroundEventProjection {
    inner: Arc<Mutex<DownloadBackgroundEventProjectionInner>>,
}

struct DownloadBackgroundEventProjectionInner {
    sender: BackgroundEventSender,
    state: DownloadBackgroundEventProjectionState,
}

enum DownloadBackgroundEventProjectionState {
    Waiting {
        start_events: Vec<BackgroundProtocolEvent>,
        latest_progress_events: Vec<BackgroundProtocolEvent>,
    },
    Released,
    Canceled,
}

impl DownloadBackgroundEventProjection {
    pub(super) fn new(
        sender: BackgroundEventSender,
        response_flush: &CommandResponseFlushContext,
    ) -> Self {
        let projection = Self {
            inner: Arc::new(Mutex::new(DownloadBackgroundEventProjectionInner {
                sender,
                state: DownloadBackgroundEventProjectionState::Waiting {
                    start_events: Vec::new(),
                    latest_progress_events: Vec::new(),
                },
            })),
        };
        let Some(mut receiver) = response_flush.receiver() else {
            projection.release();
            return projection;
        };
        if *receiver.borrow() {
            projection.release();
            return projection;
        }
        let waiter = projection.clone();
        tokio::spawn(async move {
            while !*receiver.borrow() {
                if receiver.changed().await.is_err() {
                    waiter.cancel();
                    return;
                }
            }
            waiter.release();
        });
        projection
    }

    /// Retains one finite ordered prefix, such as will-begin plus the initial
    /// in-progress observation. This method is deliberately separate from
    /// progress replacement so terminal progress cannot overtake will-begin.
    pub(super) fn emit_start_events(&self, events: Vec<BackgroundProtocolEvent>) {
        if events.is_empty() {
            return;
        }
        let mut inner = self.inner.lock();
        match &mut inner.state {
            DownloadBackgroundEventProjectionState::Waiting { start_events, .. } => {
                start_events.extend(events);
            }
            DownloadBackgroundEventProjectionState::Released => {
                send_events(&inner.sender, events);
            }
            DownloadBackgroundEventProjectionState::Canceled => {}
        }
    }

    /// Coalesces progress while the command response is pending. Each batch is
    /// already fanned out to the finite observer set, so retaining one batch is
    /// bounded independently of the number of downloaded chunks.
    pub(super) fn emit_latest_progress_events(&self, events: Vec<BackgroundProtocolEvent>) {
        if events.is_empty() {
            return;
        }
        let mut inner = self.inner.lock();
        match &mut inner.state {
            DownloadBackgroundEventProjectionState::Waiting {
                latest_progress_events,
                ..
            } => {
                *latest_progress_events = events;
            }
            DownloadBackgroundEventProjectionState::Released => {
                send_events(&inner.sender, events);
            }
            DownloadBackgroundEventProjectionState::Canceled => {}
        }
    }

    fn release(&self) {
        let mut inner = self.inner.lock();
        let DownloadBackgroundEventProjectionState::Waiting {
            start_events,
            latest_progress_events,
        } = std::mem::replace(
            &mut inner.state,
            DownloadBackgroundEventProjectionState::Released,
        )
        else {
            return;
        };

        // Keep the lock while sending. UnboundedSender::send is synchronous
        // and non-blocking, and serialization with concurrent action progress
        // prevents a post-release terminal event from overtaking this prefix.
        send_events(&inner.sender, start_events);
        send_events(&inner.sender, latest_progress_events);
    }

    fn cancel(&self) {
        let mut inner = self.inner.lock();
        if matches!(
            &inner.state,
            DownloadBackgroundEventProjectionState::Waiting { .. }
        ) {
            inner.state = DownloadBackgroundEventProjectionState::Canceled;
        }
    }
}

fn send_events(sender: &BackgroundEventSender, events: Vec<BackgroundProtocolEvent>) {
    for event in events {
        let _ = sender.send(event);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc::error::TryRecvError;

    use crate::conn::{BackgroundProtocolEvent, CdpConnection, browser_background_output_channel};

    use super::DownloadBackgroundEventProjection;

    fn sequenced_event(sequence: u64) -> BackgroundProtocolEvent {
        BackgroundProtocolEvent::immediate(json!({
            "method": "Browser.downloadProgress",
            "params": { "sequence": sequence }
        }))
    }

    #[tokio::test]
    async fn response_gate_keeps_start_and_only_latest_progress_in_order() {
        let mut conn = CdpConnection::new();
        let (permit, response_flush) = conn.begin_command_response_flush_permit();
        let (sender, mut receiver) = browser_background_output_channel();
        let projection = DownloadBackgroundEventProjection::new(sender, &response_flush);

        projection.emit_start_events(vec![sequenced_event(1), sequenced_event(2)]);
        projection.emit_latest_progress_events(vec![sequenced_event(3)]);
        projection.emit_latest_progress_events(vec![sequenced_event(4)]);

        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));

        permit.finish();

        let sequences = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut sequences = Vec::new();
            for _ in 0..3 {
                sequences.push(
                    receiver
                        .recv()
                        .await
                        .expect("released download event")
                        .into_protocol_message()["params"]["sequence"]
                        .as_u64()
                        .expect("numeric sequence"),
                );
            }
            sequences
        })
        .await
        .expect("response release should wake download projection");
        assert_eq!(sequences, [1, 2, 4]);
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
    }
}
