use tokio::sync::mpsc;

#[cfg(test)]
use crate::runtime::NavigationEngine;

use super::{
    BrowserHostHandle, BrowserHostState, BrowserHostTurn, BrowserHostTurnExecutor,
    BrowserOwnerInput,
};

/// Single-owner Browser Host input actor.
///
/// It owns the authoritative Host-state residence, admission order and ready
/// queue while the selected input's physical Page execution adapter is still
/// being migrated out of Protocol. It intentionally contains no CDP session,
/// command id, domain subscription or socket state.
#[derive(Debug)]
pub struct BrowserHostActor {
    state: BrowserHostState,
    input_rx: mpsc::UnboundedReceiver<BrowserOwnerInput>,
    selected_turn: Option<BrowserHostTurn>,
}

/// Result of waiting for the Browser Host to select its next exact turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a selected Browser Host turn must be completed before selecting again"]
pub enum BrowserHostTurnSelection {
    Selected,
    Closed,
}

impl BrowserHostActor {
    /// Creates the owner queue over an application-created Browser Host
    /// residence. The actor, rather than any protocol adapter, keeps the
    /// authoritative state allocation alive.
    pub fn new(state: BrowserHostState) -> (Self, BrowserHostHandle) {
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        (
            Self {
                state,
                input_rx,
                selected_turn: None,
            },
            BrowserHostHandle::new(input_tx),
        )
    }

    pub fn state(&self) -> &BrowserHostState {
        &self.state
    }

    /// Returns whether at least one input is ready for a later owner turn.
    pub fn has_ready_input(&self) -> bool {
        self.selected_turn.is_some() || !self.input_rx.is_empty()
    }

    /// Waits until this actor has selected one exact FIFO turn.
    ///
    /// Selection stores the turn inside the actor rather than returning its
    /// payload to application scheduling code. The underlying Tokio receive
    /// is cancellation-safe while pending; once it returns, the selected turn
    /// is installed synchronously before this future becomes ready.
    pub async fn select_next_turn_when_ready(&mut self) -> BrowserHostTurnSelection {
        if self.selected_turn.is_some() {
            return BrowserHostTurnSelection::Selected;
        }
        let Some(input) = self.input_rx.recv().await else {
            return BrowserHostTurnSelection::Closed;
        };
        self.selected_turn = Some(BrowserHostTurn::new(input, self.input_rx.len()));
        BrowserHostTurnSelection::Selected
    }

    /// Selects and starts one FIFO input as a single short Browser Host turn.
    ///
    /// The executor must return synchronously. Any network, renderer or other
    /// participant wait is represented in its output and completed as a later
    /// owner-loop input. An empty mailbox does not invoke the physical adapter.
    pub fn complete_next_turn<Executor>(
        &mut self,
        executor: &mut Executor,
    ) -> Option<Executor::Output>
    where
        Executor: BrowserHostTurnExecutor + ?Sized,
    {
        let turn = if let Some(turn) = self.selected_turn.take() {
            turn
        } else {
            let input = self.input_rx.try_recv().ok()?;
            BrowserHostTurn::new(input, self.input_rx.len())
        };
        Some(executor.execute_browser_host_turn(turn))
    }

    pub fn ready_len(&self) -> usize {
        usize::from(self.selected_turn.is_some()) + self.input_rx.len()
    }
}

#[cfg(test)]
mod tests {
    use std::future::ready;

    use crate::{
        PageId,
        browser_host::{BrowserOwnerInputKind, PageResidenceIdentity, RendererBrowserIntent},
        page::{
            RendererDocumentLifecycleIdentity, RendererDocumentSourcedTopLevelLocationNavigation,
            RendererDocumentToken, RendererFrameToken, RendererLifecycleEpoch,
        },
    };

    use super::*;

    #[derive(Default)]
    struct RecordingExecutor {
        calls: usize,
    }

    impl BrowserHostTurnExecutor for RecordingExecutor {
        type Output = BrowserHostTurn;

        fn execute_browser_host_turn(&mut self, turn: BrowserHostTurn) -> Self::Output {
            self.calls += 1;
            turn
        }
    }

    fn input(target: &str, generation: u64) -> BrowserOwnerInput {
        let page_id = PageId::new_for_testing(generation + 1);
        let document = RendererDocumentLifecycleIdentity {
            frame: RendererFrameToken { page_id },
            document: RendererDocumentToken::new_for_testing(page_id, generation),
            epoch: RendererLifecycleEpoch(generation),
        };
        BrowserOwnerInput::renderer_top_level_location_navigation(
            PageResidenceIdentity::new(
                "context-browser-host-actor".to_owned(),
                Some(target.to_owned()),
                generation,
            ),
            RendererDocumentSourcedTopLevelLocationNavigation::new(
                document,
                format!("https://example.test/{generation}"),
            ),
            None,
        )
    }

    #[tokio::test]
    async fn actor_executes_fifo_turns_and_preserves_exact_page_identity() {
        let (mut actor, handle) =
            BrowserHostActor::new(BrowserHostState::new(NavigationEngine::new()));
        let mut executor = RecordingExecutor::default();
        handle
            .publish(input("target-a", 1))
            .expect("live Browser Host should accept first input");
        handle
            .publish(input("target-b", 2))
            .expect("live Browser Host should accept second input");

        assert_eq!(actor.ready_len(), 2);
        let first = actor
            .complete_next_turn(&mut executor)
            .expect("first owner turn");
        assert_eq!(first.ready_after_selection(), 1);
        let first = first.into_input();
        let BrowserOwnerInput::RendererIntent(RendererBrowserIntent::TopLevelLocationNavigation(
            first,
        )) = first
        else {
            panic!("expected renderer navigation input");
        };
        assert_eq!(first.page_owner().target_id(), Some("target-a"));
        assert_eq!(first.page_owner().loaded_page_generation(), 1);

        let second = actor
            .complete_next_turn(&mut executor)
            .expect("second owner turn");
        assert_eq!(second.ready_after_selection(), 0);
        let second = second.into_input();
        let BrowserOwnerInput::RendererIntent(RendererBrowserIntent::TopLevelLocationNavigation(
            second,
        )) = second
        else {
            panic!("expected renderer navigation input");
        };
        assert_eq!(second.page_owner().target_id(), Some("target-b"));
        assert_eq!(second.page_owner().loaded_page_generation(), 2);
        assert_eq!(executor.calls, 2);
        assert!(!actor.has_ready_input());
    }

    #[tokio::test]
    async fn dropping_one_producer_handle_does_not_stop_browser_host() {
        let (mut actor, host_handle) =
            BrowserHostActor::new(BrowserHostState::new(NavigationEngine::new()));
        let mut executor = RecordingExecutor::default();
        let detached_producer = host_handle.clone();
        drop(detached_producer);

        host_handle
            .publish(input("target-after-detach", 3))
            .expect("dropping one producer endpoint must not stop Browser Host");
        assert!(actor.has_ready_input());
        assert!(actor.complete_next_turn(&mut executor).is_some());
    }

    #[test]
    fn actor_owns_browser_host_state_residence() {
        let state = BrowserHostState::new(NavigationEngine::new());
        let browser_instance_id = state.navigation_owner().browser_instance_id();
        let (actor, _handle) = BrowserHostActor::new(state);

        assert_eq!(
            actor.state().navigation_owner().browser_instance_id(),
            browser_instance_id
        );
    }

    #[test]
    fn publication_after_actor_shutdown_is_typed_rejection() {
        let (actor, handle) = BrowserHostActor::new(BrowserHostState::new(NavigationEngine::new()));
        drop(actor);

        let error = handle
            .publish(input("target-stopped", 4))
            .expect_err("stopped Browser Host must reject publication");
        assert_eq!(
            error.kind(),
            BrowserOwnerInputKind::RendererTopLevelLocationNavigation
        );
    }

    #[tokio::test]
    async fn empty_mailbox_does_not_invoke_physical_executor() {
        let (mut actor, _handle) =
            BrowserHostActor::new(BrowserHostState::new(NavigationEngine::new()));
        let mut executor = RecordingExecutor::default();

        assert!(actor.complete_next_turn(&mut executor).is_none());
        assert_eq!(executor.calls, 0);
    }

    #[tokio::test]
    async fn mailbox_wake_selects_without_executing_until_the_owner_turn() {
        let (mut actor, handle) =
            BrowserHostActor::new(BrowserHostState::new(NavigationEngine::new()));
        let mut executor = RecordingExecutor::default();

        let (selection, publication) = tokio::join!(actor.select_next_turn_when_ready(), async {
            handle.publish(input("target-wake", 4))
        });
        publication.expect("live Browser Host should accept the waking input");
        assert_eq!(selection, BrowserHostTurnSelection::Selected);
        assert_eq!(actor.ready_len(), 1);
        assert_eq!(executor.calls, 0, "selection must not execute recursively");

        let turn = actor
            .complete_next_turn(&mut executor)
            .expect("selected turn should remain actor-owned until completion");
        assert_eq!(
            turn.into_input().kind(),
            BrowserOwnerInputKind::RendererTopLevelLocationNavigation
        );
        assert_eq!(executor.calls, 1);
        assert!(!actor.has_ready_input());
    }

    #[tokio::test]
    async fn canceled_pending_selection_does_not_consume_the_next_input() {
        let (mut actor, handle) =
            BrowserHostActor::new(BrowserHostState::new(NavigationEngine::new()));
        let mut pending_selection = Box::pin(actor.select_next_turn_when_ready());

        tokio::select! {
            biased;
            _ = &mut pending_selection => {
                panic!("an empty Browser Host mailbox must remain pending");
            }
            _ = ready(()) => {}
        }
        drop(pending_selection);

        handle
            .publish(input("target-after-cancel", 5))
            .expect("canceled selection must leave the mailbox receiver live");
        assert_eq!(
            actor.select_next_turn_when_ready().await,
            BrowserHostTurnSelection::Selected
        );
        let mut executor = RecordingExecutor::default();
        let selected = actor
            .complete_next_turn(&mut executor)
            .expect("input published after cancellation must remain selectable");
        assert_eq!(
            selected.into_input().kind(),
            BrowserOwnerInputKind::RendererTopLevelLocationNavigation
        );
    }

    #[tokio::test]
    async fn mailbox_shutdown_is_a_typed_selection_terminal() {
        let (mut actor, handle) =
            BrowserHostActor::new(BrowserHostState::new(NavigationEngine::new()));
        drop(handle);

        assert_eq!(
            actor.select_next_turn_when_ready().await,
            BrowserHostTurnSelection::Closed
        );
    }
}
