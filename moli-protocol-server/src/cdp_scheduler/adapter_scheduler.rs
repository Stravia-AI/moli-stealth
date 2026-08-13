use moli_core::RendererOutputTransportMessage;
use moli_protocol::{
    CompletedDeferredMainDocumentLoadCompletion, DeferredMainDocumentLoadCompletionOutputAction,
    DeferredMainDocumentLoadCompletionOutputInterest, DeferredMainDocumentLoadObservationId,
    PendingDeferredMainDocumentLoadCompletion,
};
use tokio::sync::mpsc;

use super::{CdpScheduler, ProtocolOutputSequence, protocol_residence::ProtocolSchedulerStep};

/// Connection-local scheduling state shared by CDP, BiDi and Classic adapters.
///
/// The protocol scheduler owns concrete output and browser-owner work. This
/// driver owns only the asynchronous adapter boundary needed to select that
/// work in a later client turn:
///
/// - one coalesced self-turn signal;
/// - at most one exact main-document load observation;
/// - an adapter-specific attachment that remains inseparable from that exact
///   observation until its terminal is consumed.
///
/// It never stores a renderer publication, Page task capability or protocol
/// transport route. Switching a Classic connection to BiDi therefore keeps
/// this value alive instead of recreating scheduler or load-observer state.
pub(crate) struct ProtocolAdapterScheduler<A> {
    turn_tx: mpsc::UnboundedSender<()>,
    turn_rx: mpsc::UnboundedReceiver<()>,
    turn_scheduled: bool,
    load_completion_tx: mpsc::UnboundedSender<CompletedDeferredMainDocumentLoadCompletion>,
    load_completion_rx: mpsc::UnboundedReceiver<CompletedDeferredMainDocumentLoadCompletion>,
    pending_load: Option<PendingAdapterLoadObservation<A>>,
}

struct PendingAdapterLoadObservation<A> {
    observation_id: DeferredMainDocumentLoadObservationId,
    output_interest: DeferredMainDocumentLoadCompletionOutputInterest,
    attachment: A,
}

pub(crate) enum ProtocolAdapterSchedulerInput {
    Turn,
    DeferredLoadCompletion(Box<CompletedDeferredMainDocumentLoadCompletion>),
}

/// Result of consuming one shared adapter-scheduler input.
///
/// `DeferredLoadStarted` deliberately does not expose the pending observer or
/// its wake interest. The exact identity and adapter attachment remain owned
/// by `ProtocolAdapterScheduler` until `DeferredLoadCompleted`.
pub(crate) enum ProtocolAdapterSchedulerAdvance<A> {
    Idle,
    ClientTurnYielded,
    DeferredLoadStarted {
        observation_id: DeferredMainDocumentLoadObservationId,
    },
    ProtocolResidenceCompleted(ProtocolOutputSequence),
    DeferredLoadCompleted {
        observation_id: DeferredMainDocumentLoadObservationId,
        attachment: A,
        output: ProtocolOutputSequence,
    },
    StaleDeferredLoadCompletion {
        observation_id: DeferredMainDocumentLoadObservationId,
    },
}

impl<A> Default for ProtocolAdapterScheduler<A> {
    fn default() -> Self {
        let (turn_tx, turn_rx) = mpsc::unbounded_channel();
        let (load_completion_tx, load_completion_rx) = mpsc::unbounded_channel();
        Self {
            turn_tx,
            turn_rx,
            turn_scheduled: false,
            load_completion_tx,
            load_completion_rx,
            pending_load: None,
        }
    }
}

impl<A> ProtocolAdapterScheduler<A> {
    pub(crate) fn has_pending_load(&self) -> bool {
        self.pending_load.is_some()
    }

    pub(crate) fn pending_load_attachment_mut(&mut self) -> Option<&mut A> {
        self.pending_load
            .as_mut()
            .map(|pending| &mut pending.attachment)
    }

    /// Coalesces scheduler readiness into one later adapter turn.
    ///
    /// Sending through a local channel is intentional: satisfying a
    /// `ClientTurnPredecessor` must happen after control returns to the adapter
    /// loop, not recursively in the producer or command-completion stack.
    pub(crate) fn schedule_turn_if_needed(
        &mut self,
        scheduler: &CdpScheduler,
        page_javascript_blocked: bool,
    ) {
        if page_javascript_blocked {
            return;
        }
        let step = self.next_scheduler_step(scheduler);
        if self.turn_scheduled || step == ProtocolSchedulerStep::Wait {
            return;
        }
        if moli_trace::cdp_runtime_trace_enabled() {
            tracing::info!(
                target: "moli_cdp_runtime",
                stage = "protocol_adapter_turn_schedule",
                step = ?step,
            );
        }
        self.turn_scheduled = true;
        let turn_tx = self.turn_tx.clone();
        tokio::task::spawn_local(async move {
            let _ = turn_tx.send(());
        });
    }

    /// Waits for either the coalesced self turn or the exact load terminal.
    ///
    /// The driver retains both senders, so a closed internal channel is an
    /// invariant violation rather than an adapter shutdown signal.
    pub(crate) async fn recv_input(&mut self) -> ProtocolAdapterSchedulerInput {
        tokio::select! {
            biased;
            completion = self.load_completion_rx.recv(), if self.has_pending_load() => {
                ProtocolAdapterSchedulerInput::DeferredLoadCompletion(Box::new(
                    completion.expect("shared adapter load-completion channel must remain open"),
                ))
            }
            turn = self.turn_rx.recv(), if self.turn_scheduled => {
                turn.expect("shared adapter self-turn channel must remain open");
                ProtocolAdapterSchedulerInput::Turn
            }
            // Every adapter selects this future alongside its transport and
            // renderer inputs. A completely idle protocol scheduler therefore
            // means "this source is not ready", not an all-branches-disabled
            // error.
            else => std::future::pending::<ProtocolAdapterSchedulerInput>().await,
        }
    }

    /// Ingests one concrete renderer publication using the exact load
    /// observation, if any, currently owned by this connection-local driver.
    pub(crate) async fn ingest_renderer_publication(
        &mut self,
        scheduler: &mut CdpScheduler,
        publication: RendererOutputTransportMessage,
    ) -> ProtocolOutputSequence {
        let Some(pending) = self.pending_load.as_ref() else {
            return scheduler
                .ingest_renderer_publication_for_scheduler(publication)
                .await;
        };
        let observation_id = pending.observation_id;
        match scheduler.route_renderer_output_for_deferred_load_completion(
            &publication,
            &pending.output_interest,
        ) {
            DeferredMainDocumentLoadCompletionOutputAction::ProcessNow => {
                scheduler.ingest_renderer_publication_now(publication).await
            }
            DeferredMainDocumentLoadCompletionOutputAction::Queue => {
                scheduler
                    .ingest_renderer_publication_after_load(publication, observation_id)
                    .await
            }
        }
    }

    /// Consumes one input and advances at most one concrete scheduler
    /// residence.
    ///
    /// `make_load_attachment` is called only when this turn starts a new exact
    /// load observation. Keeping that attachment inside the shared driver
    /// prevents adapters from maintaining a parallel observation id or
    /// generation solely to associate output-routing state with the terminal.
    pub(crate) async fn advance_input(
        &mut self,
        scheduler: &mut CdpScheduler,
        input: ProtocolAdapterSchedulerInput,
        make_load_attachment: impl FnOnce() -> A,
    ) -> ProtocolAdapterSchedulerAdvance<A> {
        match input {
            ProtocolAdapterSchedulerInput::Turn => {
                self.turn_scheduled = false;
                self.advance_turn(scheduler, make_load_attachment).await
            }
            ProtocolAdapterSchedulerInput::DeferredLoadCompletion(completion) => {
                self.complete_load(scheduler, *completion).await
            }
        }
    }

    async fn advance_turn(
        &mut self,
        scheduler: &mut CdpScheduler,
        make_load_attachment: impl FnOnce() -> A,
    ) -> ProtocolAdapterSchedulerAdvance<A> {
        match self.next_scheduler_step(scheduler) {
            ProtocolSchedulerStep::SatisfyClientTurnPredecessor => {
                scheduler.satisfy_front_protocol_residence_client_turn_predecessor();
                ProtocolAdapterSchedulerAdvance::ClientTurnYielded
            }
            ProtocolSchedulerStep::CompleteReadyResidence
                if scheduler.next_ready_protocol_residence_is_main_document_load_action() =>
            {
                assert!(
                    self.pending_load.is_none(),
                    "one adapter scheduler cannot start a second load observation"
                );
                let pending = scheduler
                    .start_next_deferred_load_completion()
                    .expect("ready load residence must produce an exact pending observation");
                let observation_id = pending.observation_id();
                let output_interest = pending.output_interest();
                self.pending_load = Some(PendingAdapterLoadObservation {
                    observation_id,
                    output_interest,
                    attachment: make_load_attachment(),
                });
                self.spawn_load_wait(pending);
                ProtocolAdapterSchedulerAdvance::DeferredLoadStarted { observation_id }
            }
            ProtocolSchedulerStep::CompleteReadyResidence => {
                ProtocolAdapterSchedulerAdvance::ProtocolResidenceCompleted(
                    scheduler.complete_next_protocol_residence().await,
                )
            }
            ProtocolSchedulerStep::Wait => ProtocolAdapterSchedulerAdvance::Idle,
        }
    }

    /// Returns the next concrete-residence transition this adapter may drive.
    ///
    /// An in-flight exact load observation reserves this driver's one load
    /// attachment slot; it does not precede unrelated scheduler work.
    /// `CdpScheduler` already makes exact `load_predecessors` authoritative.
    /// Keeping no-predecessor owner actions runnable is required because a
    /// replacement or termination action may itself settle the observation.
    fn next_scheduler_step(&self, scheduler: &CdpScheduler) -> ProtocolSchedulerStep {
        let step = scheduler.next_protocol_scheduler_step();
        self.enforce_load_attachment_capacity(
            step,
            scheduler.next_ready_protocol_residence_is_main_document_load_action(),
        )
    }

    fn enforce_load_attachment_capacity(
        &self,
        step: ProtocolSchedulerStep,
        next_ready_residence_is_main_document_load_action: bool,
    ) -> ProtocolSchedulerStep {
        if self.has_pending_load()
            && step == ProtocolSchedulerStep::CompleteReadyResidence
            && next_ready_residence_is_main_document_load_action
        {
            ProtocolSchedulerStep::Wait
        } else {
            step
        }
    }

    fn spawn_load_wait(&self, pending: PendingDeferredMainDocumentLoadCompletion) {
        if moli_trace::cdp_runtime_trace_enabled() {
            tracing::info!(
                target: "moli_cdp_runtime",
                stage = "protocol_adapter_load_wait_spawn",
                observation_id = ?pending.observation_id(),
            );
        }
        let completion_tx = self.load_completion_tx.clone();
        tokio::task::spawn_local(async move {
            let completion = pending.wait().await;
            let _ = completion_tx.send(completion);
        });
    }

    async fn complete_load(
        &mut self,
        scheduler: &mut CdpScheduler,
        completion: CompletedDeferredMainDocumentLoadCompletion,
    ) -> ProtocolAdapterSchedulerAdvance<A> {
        let observation_id = completion.observation_id();
        let Ok(pending) = self.take_pending_load(observation_id) else {
            return ProtocolAdapterSchedulerAdvance::StaleDeferredLoadCompletion { observation_id };
        };
        let output = scheduler
            .complete_deferred_load_completion(completion)
            .await;
        ProtocolAdapterSchedulerAdvance::DeferredLoadCompleted {
            observation_id,
            attachment: pending.attachment,
            output,
        }
    }

    /// Claims the attachment only for the exact observation that produced a
    /// terminal.
    ///
    /// A delayed terminal from an already-retired observation must not detach
    /// the current adapter mode or command-routing state. Restoring the
    /// nonmatching value here keeps that invariant independent of how each
    /// adapter reacts to `StaleDeferredLoadCompletion`.
    fn take_pending_load(
        &mut self,
        observation_id: DeferredMainDocumentLoadObservationId,
    ) -> Result<PendingAdapterLoadObservation<A>, ()> {
        let Some(pending) = self.pending_load.take() else {
            return Err(());
        };
        if pending.observation_id != observation_id {
            self.pending_load = Some(pending);
            return Err(());
        }
        Ok(pending)
    }
}

#[cfg(test)]
mod tests {
    use moli_core::{PageId, RendererOutputResidenceIdentity, RendererOwnerLocalHostId};
    use moli_protocol::{
        CdpConnection, CdpSchedulerEvent, ProtocolSchedulerWork,
        test_support::{
            deferred_main_document_load_observation_id,
            deferred_main_document_load_output_interest, root_frame_stopped_loading_work,
        },
    };
    use tokio::task::LocalSet;

    use super::{
        PendingAdapterLoadObservation, ProtocolAdapterScheduler, ProtocolAdapterSchedulerAdvance,
        ProtocolAdapterSchedulerInput,
    };
    use crate::cdp_scheduler::{CdpScheduler, protocol_residence::ProtocolSchedulerStep};

    fn protocol_observation(publish_sequence: u64) -> ProtocolSchedulerWork {
        root_frame_stopped_loading_work(
            publish_sequence,
            vec![Some("SID-adapter".to_owned())],
            "FRAME-adapter".to_owned(),
            "LOADER-adapter".to_owned(),
        )
    }

    fn page_residence() -> RendererOutputResidenceIdentity {
        RendererOutputResidenceIdentity::Page {
            owner_local_host_id: RendererOwnerLocalHostId::new_for_testing(1),
            page_id: PageId::new_for_testing(7),
        }
    }

    #[test]
    fn pending_exact_load_observation_allows_independent_protocol_residence() {
        let observation_id = deferred_main_document_load_observation_id(1);
        let mut scheduler = CdpScheduler::new(CdpConnection::new());
        scheduler.apply_scheduler_events(vec![CdpSchedulerEvent::ProtocolWorkPublished {
            work: protocol_observation(1),
        }]);
        let adapter = ProtocolAdapterScheduler::<()> {
            pending_load: Some(PendingAdapterLoadObservation {
                observation_id,
                output_interest: deferred_main_document_load_output_interest(
                    page_residence(),
                    None,
                ),
                attachment: (),
            }),
            ..Default::default()
        };

        assert_eq!(
            adapter.next_scheduler_step(&scheduler),
            ProtocolSchedulerStep::SatisfyClientTurnPredecessor,
            "an exact load observation must not block an independent client-turn boundary"
        );
        scheduler.satisfy_front_protocol_residence_client_turn_predecessor();
        assert_eq!(
            adapter.next_scheduler_step(&scheduler),
            ProtocolSchedulerStep::CompleteReadyResidence,
            "independent protocol work must remain runnable while the exact observation waits"
        );
        assert!(
            adapter.has_pending_load(),
            "running independent work must not release the exact load attachment"
        );
    }

    #[test]
    fn pending_exact_load_observation_blocks_second_load_attachment() {
        let observation_id = deferred_main_document_load_observation_id(1);
        let adapter = ProtocolAdapterScheduler::<()> {
            pending_load: Some(PendingAdapterLoadObservation {
                observation_id,
                output_interest: deferred_main_document_load_output_interest(
                    page_residence(),
                    None,
                ),
                attachment: (),
            }),
            ..Default::default()
        };

        assert_eq!(
            adapter.enforce_load_attachment_capacity(
                ProtocolSchedulerStep::CompleteReadyResidence,
                true,
            ),
            ProtocolSchedulerStep::Wait,
            "one adapter cannot start a second exact load observation"
        );
        assert_eq!(
            adapter.enforce_load_attachment_capacity(
                ProtocolSchedulerStep::CompleteReadyResidence,
                false,
            ),
            ProtocolSchedulerStep::CompleteReadyResidence,
            "load attachment capacity must not become a global execution lock"
        );
    }

    #[tokio::test]
    async fn idle_adapter_input_remains_pending() {
        let mut adapter = ProtocolAdapterScheduler::<()>::default();
        tokio::select! {
            biased;
            _ = adapter.recv_input() => {
                panic!("an idle shared scheduler source must remain pending");
            }
            _ = std::future::ready(()) => {}
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn self_turn_is_coalesced_and_preserves_the_client_turn_boundary() {
        LocalSet::new()
            .run_until(async {
                let mut scheduler = CdpScheduler::new(CdpConnection::new());
                scheduler.apply_scheduler_events(vec![CdpSchedulerEvent::ProtocolWorkPublished {
                    work: protocol_observation(1),
                }]);
                let mut adapter = ProtocolAdapterScheduler::<()>::default();

                adapter.schedule_turn_if_needed(&scheduler, false);
                adapter.schedule_turn_if_needed(&scheduler, false);
                let first = adapter.recv_input().await;
                assert!(matches!(first, ProtocolAdapterSchedulerInput::Turn));
                assert!(matches!(
                    adapter
                        .advance_input(&mut scheduler, first, || {
                            panic!("ordinary protocol work cannot create a load attachment")
                        })
                        .await,
                    ProtocolAdapterSchedulerAdvance::ClientTurnYielded
                ));
                assert!(
                    adapter.turn_rx.try_recv().is_err(),
                    "coalescing must not leave a duplicate adapter turn queued"
                );

                adapter.schedule_turn_if_needed(&scheduler, false);
                let second = adapter.recv_input().await;
                assert!(matches!(second, ProtocolAdapterSchedulerInput::Turn));
                assert!(matches!(
                    adapter
                        .advance_input(&mut scheduler, second, || {
                            panic!("ordinary protocol work cannot create a load attachment")
                        })
                        .await,
                    ProtocolAdapterSchedulerAdvance::ProtocolResidenceCompleted(_)
                ));
            })
            .await;
    }

    #[derive(Debug, PartialEq, Eq)]
    enum AdapterMode {
        Classic,
        Bidi { session_id: &'static str },
    }

    #[test]
    fn exact_load_attachment_survives_adapter_switch_and_stale_terminal() {
        let current = deferred_main_document_load_observation_id(2);
        let stale = deferred_main_document_load_observation_id(1);
        let mut adapter = ProtocolAdapterScheduler::<AdapterMode> {
            pending_load: Some(PendingAdapterLoadObservation {
                observation_id: current,
                output_interest: deferred_main_document_load_output_interest(
                    page_residence(),
                    None,
                ),
                attachment: AdapterMode::Classic,
            }),
            ..Default::default()
        };

        *adapter
            .pending_load_attachment_mut()
            .expect("exact load attachment should remain resident") = AdapterMode::Bidi {
            session_id: "SID-upgraded",
        };

        assert!(
            adapter.take_pending_load(stale).is_err(),
            "a delayed terminal must not claim the current load attachment"
        );
        assert_eq!(
            adapter
                .pending_load_attachment_mut()
                .expect("stale terminal must preserve current attachment"),
            &AdapterMode::Bidi {
                session_id: "SID-upgraded",
            }
        );
        let claimed = adapter
            .take_pending_load(current)
            .expect("the exact terminal should claim its attachment");
        assert_eq!(
            claimed.attachment,
            AdapterMode::Bidi {
                session_id: "SID-upgraded",
            }
        );
        assert!(!adapter.has_pending_load());
    }
}
