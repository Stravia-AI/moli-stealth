use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, error::TryRecvError};

use crate::domains::command_output::BrowserNavigateCommandOutcomeDelivery;

use super::BackgroundProtocolEvent;

/// Producer endpoint for the migration-period Browser/frontend output FIFO.
///
/// Existing Browser facts still enter as already projected protocol events.
/// Navigation command completion enters as a protocol-neutral outcome and is
/// projected only when the frontend consumes this FIFO. Both use one physical
/// channel so response-head Network events retain their exact ordering around
/// the early `Page.navigate` response.
#[derive(Clone, Debug)]
pub struct BackgroundEventSender {
    sender: UnboundedSender<BrowserBackgroundOutput>,
}

/// Consumer endpoint that projects frontend-specific output without owning or
/// advancing Browser execution.
#[derive(Debug)]
pub struct BrowserBackgroundOutputReceiver {
    receiver: UnboundedReceiver<BrowserBackgroundOutput>,
}

#[derive(Debug)]
enum BrowserBackgroundOutput {
    ProtocolEvent(BackgroundProtocolEvent),
    NavigateCommandOutcome(BrowserNavigateCommandOutcomeDelivery),
}

/// The frontend output queue was closed before an item could be admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackgroundOutputClosed;

pub fn browser_background_output_channel()
-> (BackgroundEventSender, BrowserBackgroundOutputReceiver) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    (
        BackgroundEventSender { sender },
        BrowserBackgroundOutputReceiver { receiver },
    )
}

impl BackgroundEventSender {
    pub fn send(&self, event: BackgroundProtocolEvent) -> Result<(), BackgroundOutputClosed> {
        self.sender
            .send(BrowserBackgroundOutput::ProtocolEvent(event))
            .map_err(|_| BackgroundOutputClosed)
    }

    pub(crate) fn send_browser_navigate_outcome(
        &self,
        delivery: BrowserNavigateCommandOutcomeDelivery,
    ) -> Result<(), BackgroundOutputClosed> {
        self.sender
            .send(BrowserBackgroundOutput::NavigateCommandOutcome(delivery))
            .map_err(|_| BackgroundOutputClosed)
    }

    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
}

impl BrowserBackgroundOutputReceiver {
    pub async fn recv(&mut self) -> Option<BackgroundProtocolEvent> {
        self.receiver
            .recv()
            .await
            .map(BrowserBackgroundOutput::into_protocol_event)
    }

    pub fn try_recv(&mut self) -> Result<BackgroundProtocolEvent, TryRecvError> {
        self.receiver
            .try_recv()
            .map(BrowserBackgroundOutput::into_protocol_event)
    }
}

impl BrowserBackgroundOutput {
    fn into_protocol_event(self) -> BackgroundProtocolEvent {
        match self {
            Self::ProtocolEvent(event) => event,
            Self::NavigateCommandOutcome(delivery) => delivery.into_background_protocol_event(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn navigate_outcome_retains_fifo_position_between_protocol_events() {
        let (sender, mut receiver) = browser_background_output_channel();
        sender
            .send(BackgroundProtocolEvent::immediate(json!({
                "method": "Network.beforeNavigateOutcome",
                "params": {"sequence": 1}
            })))
            .expect("prefix event should enter output FIFO");
        sender
            .send_browser_navigate_outcome(BrowserNavigateCommandOutcomeDelivery::completed(
                42,
                Some("SID-nav"),
                "https://example.test/",
                json!({"frameId": "FRAME-1", "loaderId": "LOADER-1"}),
            ))
            .expect("navigate outcome should enter output FIFO");
        sender
            .send(BackgroundProtocolEvent::immediate(json!({
                "method": "Network.afterNavigateOutcome",
                "params": {"sequence": 2}
            })))
            .expect("suffix event should enter output FIFO");

        let outputs = (0..3)
            .map(|_| {
                receiver
                    .receiver
                    .try_recv()
                    .expect("queued output should remain available")
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            &outputs[0],
            BrowserBackgroundOutput::ProtocolEvent(_)
        ));
        assert!(matches!(
            &outputs[1],
            BrowserBackgroundOutput::NavigateCommandOutcome(_)
        ));
        assert!(matches!(
            &outputs[2],
            BrowserBackgroundOutput::ProtocolEvent(_)
        ));

        let messages = outputs
            .into_iter()
            .map(BrowserBackgroundOutput::into_protocol_event)
            .map(BackgroundProtocolEvent::into_protocol_message)
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec![
                json!({
                    "method": "Network.beforeNavigateOutcome",
                    "params": {"sequence": 1}
                }),
                json!({
                    "id": 42,
                    "result": {"frameId": "FRAME-1", "loaderId": "LOADER-1"},
                    "sessionId": "SID-nav"
                }),
                json!({
                    "method": "Network.afterNavigateOutcome",
                    "params": {"sequence": 2}
                }),
            ]
        );
    }
}
