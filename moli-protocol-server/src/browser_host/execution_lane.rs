use moli_core::browser_host::{BrowserHostActor, BrowserHostTurnSelection};
use moli_protocol::{
    BrowserHostTurnDispatch, BrowserHostTurnExecutorOwner, CdpTurnOutcome,
    CompletedBrowserHostTurn, DevToolsHostAdapter,
};
use tokio::sync::mpsc;

/// Application-side execution lane for Browser Host inputs and exact
/// participant completions.
///
/// Core owns FIFO input selection. The application-owned DevTools adapter
/// temporarily supplies the remaining renderer/protocol projection. Socket
/// frontends never enter this seam: a short Core turn may register a
/// move-owned wait, and the completed value is later received as a separate
/// owner-loop input.
pub(crate) struct BrowserHostOwnerLane {
    actor: BrowserHostActor,
    executor_owner: BrowserHostTurnExecutorOwner,
    completion_tx: mpsc::UnboundedSender<CompletedBrowserHostTurn>,
    completion_rx: mpsc::UnboundedReceiver<CompletedBrowserHostTurn>,
}

pub(crate) enum BrowserHostOwnerWake {
    TurnSelected,
    ParticipantCompleted(Box<CompletedBrowserHostTurn>),
    Closed,
}

impl BrowserHostOwnerLane {
    pub(crate) fn new(actor: BrowserHostActor) -> Self {
        let (completion_tx, completion_rx) = mpsc::unbounded_channel();
        Self {
            actor,
            executor_owner: BrowserHostTurnExecutorOwner::for_application_owner_lane(),
            completion_tx,
            completion_rx,
        }
    }

    pub(crate) fn has_ready_input(&self) -> bool {
        self.actor.has_ready_input()
    }

    pub(crate) async fn recv_wake(&mut self) -> BrowserHostOwnerWake {
        let actor = &mut self.actor;
        let completion_rx = &mut self.completion_rx;
        tokio::select! {
            biased;
            maybe_completed = completion_rx.recv() => {
                match maybe_completed {
                    Some(completed) => {
                        BrowserHostOwnerWake::ParticipantCompleted(Box::new(completed))
                    }
                    None => BrowserHostOwnerWake::Closed,
                }
            }
            selection = actor.select_next_turn_when_ready() => {
                match selection {
                    BrowserHostTurnSelection::Selected => BrowserHostOwnerWake::TurnSelected,
                    BrowserHostTurnSelection::Closed => BrowserHostOwnerWake::Closed,
                }
            }
        }
    }

    pub(crate) fn start_next_turn(
        &mut self,
        host_adapter: &mut DevToolsHostAdapter,
    ) -> Option<CdpTurnOutcome> {
        let dispatch = self
            .executor_owner
            .start_next_turn(&mut self.actor, host_adapter)?;
        Some(self.register_dispatch(dispatch))
    }

    pub(crate) async fn complete_turn(
        &mut self,
        host_adapter: &mut DevToolsHostAdapter,
        completed: CompletedBrowserHostTurn,
    ) -> CdpTurnOutcome {
        let dispatch = self
            .executor_owner
            .complete_turn(host_adapter, completed)
            .await;
        self.register_dispatch(dispatch)
    }

    fn register_dispatch(&self, dispatch: BrowserHostTurnDispatch) -> CdpTurnOutcome {
        let (outcome, pending) = dispatch.into_parts();
        if let Some(pending) = pending {
            let completion_tx = self.completion_tx.clone();
            tokio::task::spawn_local(async move {
                let completed = pending.wait().await;
                let _ = completion_tx.send(completed);
            });
        }
        outcome
    }
}
