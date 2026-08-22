use super::{BrowserOwnerInput, BrowserOwnerInputKind};

/// One input selected by the Browser Host for immediate execution.
///
/// Construction is restricted to the Core Browser Host module; production
/// turns are issued by the actor after FIFO mailbox selection. Physical
/// migration adapters may consume this capability, but cannot manufacture a
/// turn from a raw renderer intent or choose a different mailbox entry.
#[derive(Debug)]
#[must_use = "a selected Browser Host turn must be consumed by its executor"]
pub struct BrowserHostTurn {
    input: BrowserOwnerInput,
    ready_after_selection: usize,
}

impl BrowserHostTurn {
    pub(super) fn new(input: BrowserOwnerInput, ready_after_selection: usize) -> Self {
        Self {
            input,
            ready_after_selection,
        }
    }

    pub fn kind(&self) -> BrowserOwnerInputKind {
        self.input.kind()
    }

    /// Snapshot of inputs still ready immediately after this FIFO selection.
    pub fn ready_after_selection(&self) -> usize {
        self.ready_after_selection
    }

    pub fn into_input(self) -> BrowserOwnerInput {
        self.input
    }
}

/// Physical execution boundary used by [`super::BrowserHostActor`].
///
/// Implementations may temporarily project into protocol-owned Page payload,
/// but they receive only an actor-selected [`BrowserHostTurn`]. The trait has
/// no frontend identity, command id, subscription or socket contract.
pub trait BrowserHostTurnExecutor {
    type Output;

    fn execute_browser_host_turn(&mut self, turn: BrowserHostTurn) -> Self::Output;
}
