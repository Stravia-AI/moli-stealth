use tokio::sync::mpsc;

use super::{BrowserOwnerInput, BrowserOwnerInputKind};

/// Cloneable producer endpoint for the protocol-neutral Browser Owner lane.
///
/// Publishing is synchronous and never executes browser work recursively in
/// the renderer-output or frontend stack. The single Browser Host receiver
/// remains the only input-selection authority.
#[derive(Clone, Debug)]
pub struct BrowserHostHandle {
    input_tx: mpsc::UnboundedSender<BrowserOwnerInput>,
}

impl BrowserHostHandle {
    pub(super) fn new(input_tx: mpsc::UnboundedSender<BrowserOwnerInput>) -> Self {
        Self { input_tx }
    }

    pub fn publish(&self, input: BrowserOwnerInput) -> Result<(), BrowserHostInputPublishError> {
        let kind = input.kind();
        self.input_tx
            .send(input)
            .map_err(|_| BrowserHostInputPublishError { kind })
    }

    pub fn is_stopped(&self) -> bool {
        self.input_tx.is_closed()
    }
}

/// Typed rejection returned when the Browser Host input owner has stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrowserHostInputPublishError {
    kind: BrowserOwnerInputKind,
}

impl BrowserHostInputPublishError {
    pub fn kind(self) -> BrowserOwnerInputKind {
        self.kind
    }
}

impl std::fmt::Display for BrowserHostInputPublishError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Browser Host stopped before accepting {:?}",
            self.kind
        )
    }
}

impl std::error::Error for BrowserHostInputPublishError {}
