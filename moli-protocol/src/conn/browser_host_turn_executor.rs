#[cfg(test)]
use moli_core::browser_host::PageResidenceIdentity;
use moli_core::browser_host::{
    BrowserContextHandle, BrowserFrontendCommand, BrowserHistoryTraversalDestination,
    BrowserHistoryTraversalResult, BrowserHostActor, BrowserHostTurn, BrowserHostTurnExecutor,
    BrowserNavigateCommandOutcome, BrowserNavigateCommandResult, BrowserOwnerInput,
    BrowserTargetHandle, BrowserTargetId, RendererBrowserIntent,
};

use crate::domains::fetch::{
    CompletedPausedNavigationDecisionOwnerTask, PausedNavigationDecisionOwnerTaskStep,
    PendingPausedNavigationDecisionOwnerTask,
};
use crate::domains::page::{
    CompletedPageCommandDispatch, CompletedPageTargetTerminationOwnerTask,
    CompletedStopLoadingOwnerTask, CompletedTargetCloseOwnerTask, PageCommandTaskStep,
    PageTargetTerminationOwnerTaskStep, PendingPageCommandDispatch,
    PendingPageTargetTerminationOwnerTask, PendingStopLoadingOwnerTask,
    PendingTargetCloseOwnerTask, StopLoadingOwnerTaskStep, TargetCloseOwnerTaskStep,
};
use crate::domains::target::{
    BrowserContextDisposalOwnerTaskStep, CompletedBrowserContextDisposalOwnerTask,
    PendingBrowserContextDisposalOwnerTask,
};

use super::BrowserHostTurnExecutorOwner;
use super::browser_target_engine_handoff::{
    BrowserTargetPromotionStart, CompletedBrowserTargetPromotion, PendingBrowserTargetPromotion,
    TargetActivationTransition,
};
use super::{
    BrowserHostTurnExecution, CdpConnection, CdpRendererOwnerTurnOutcome, CdpTurnOutcome,
    CommandDispatchContext,
    browser_owner_input::{
        CompletedBrowserOwnerContextDisposalCommand,
        CompletedBrowserOwnerInitialTargetNavigationCommand,
        CompletedBrowserOwnerNavigationCommand,
        CompletedBrowserOwnerPausedNavigationDecisionCommand,
        CompletedBrowserOwnerStopLoadingCommand,
    },
};

enum BrowserHostPageStepCompletion {
    BrowserEffects,
    InitialTargetNavigationCommand {
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerInitialTargetNavigationCommand>,
    },
    FrontendNavigation {
        requested_url: String,
        history_traversal: Option<BrowserHistoryTraversalResult>,
        accepted_background_result: Option<BrowserNavigateCommandResult>,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerNavigationCommand>,
    },
}

enum BrowserHostFrontendNavigationAction {
    Navigate {
        url: String,
        referrer: Option<String>,
    },
    Reload {
        ignore_cache: bool,
        script_to_evaluate_on_load: Option<String>,
    },
    HistoryTraversal {
        destination: BrowserHistoryTraversalDestination,
    },
}

/// Result of one short, actor-selected Browser Host turn.
///
/// Physical/protocol projection remains in `CdpConnection` during migration,
/// but a renderer or network wait is returned as an exact pending operation
/// instead of being awaited while the Browser Host actor is borrowed.
pub struct BrowserHostTurnDispatch {
    outcome: CdpRendererOwnerTurnOutcome,
    pending: Option<PendingBrowserHostTurn>,
}

impl BrowserHostTurnDispatch {
    fn complete(outcome: impl Into<CdpRendererOwnerTurnOutcome>) -> Self {
        Self {
            outcome: outcome.into(),
            pending: None,
        }
    }

    fn pending(
        outcome: impl Into<CdpRendererOwnerTurnOutcome>,
        pending: PendingBrowserHostTurn,
    ) -> Self {
        Self {
            outcome: outcome.into(),
            pending: Some(pending),
        }
    }

    pub fn into_parts(self) -> (CdpRendererOwnerTurnOutcome, Option<PendingBrowserHostTurn>) {
        (self.outcome, self.pending)
    }
}

/// Exact participant wait registered by a short Browser Host turn.
///
/// Waiting consumes the capability. The completion retains the command owner
/// scope and renderer token captured at dispatch time, so a later Page cannot
/// accidentally satisfy an older operation.
#[must_use = "a pending Browser Host turn must be registered for completion"]
pub struct PendingBrowserHostTurn {
    participant: PendingBrowserHostTurnParticipant,
}

enum PendingBrowserHostTurnParticipant {
    PageCommand {
        pending: PendingPageCommandDispatch,
        command_context: CommandDispatchContext,
        completion: BrowserHostPageStepCompletion,
    },
    PageTermination(PendingPageTargetTerminationOwnerTask),
    StopLoading {
        pending: PendingStopLoadingOwnerTask,
        command_context: CommandDispatchContext,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerStopLoadingCommand>,
    },
    ContextDisposal {
        pending: Box<PendingBrowserContextDisposalOwnerTask>,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerContextDisposalCommand>,
    },
    PausedNavigationDecision {
        pending: Box<PendingPausedNavigationDecisionOwnerTask>,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerPausedNavigationDecisionCommand>,
    },
    AuxiliaryTargetActivation {
        pending: Option<Box<PendingBrowserTargetPromotion>>,
        transition: TargetActivationTransition,
    },
    TargetClose(PendingTargetCloseOwnerTask),
}

impl PendingBrowserHostTurn {
    #[cfg(test)]
    pub(crate) fn stop_loading_page_owner_for_test(&self) -> Option<&PageResidenceIdentity> {
        match &self.participant {
            PendingBrowserHostTurnParticipant::StopLoading { pending, .. } => {
                pending.page_owner_for_test()
            }
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn paused_navigation_decision_page_owner_for_test(
        &self,
    ) -> Option<&PageResidenceIdentity> {
        match &self.participant {
            PendingBrowserHostTurnParticipant::PausedNavigationDecision { pending, .. } => {
                Some(pending.page_owner_for_test())
            }
            _ => None,
        }
    }

    pub async fn wait(self) -> CompletedBrowserHostTurn {
        let participant = match self.participant {
            PendingBrowserHostTurnParticipant::PageCommand {
                pending,
                command_context,
                completion,
            } => CompletedBrowserHostTurnParticipant::PageCommand {
                completed: pending.wait().await,
                command_context,
                completion,
            },
            PendingBrowserHostTurnParticipant::PageTermination(pending) => {
                CompletedBrowserHostTurnParticipant::PageTermination(pending.wait().await)
            }
            PendingBrowserHostTurnParticipant::StopLoading {
                pending,
                command_context,
                reply,
            } => CompletedBrowserHostTurnParticipant::StopLoading {
                completed: pending.wait().await,
                command_context,
                reply,
            },
            PendingBrowserHostTurnParticipant::ContextDisposal { pending, reply } => {
                CompletedBrowserHostTurnParticipant::ContextDisposal {
                    completed: Box::new(pending.wait().await),
                    reply,
                }
            }
            PendingBrowserHostTurnParticipant::PausedNavigationDecision { pending, reply } => {
                CompletedBrowserHostTurnParticipant::PausedNavigationDecision {
                    completed: Box::new(pending.wait().await),
                    reply,
                }
            }
            PendingBrowserHostTurnParticipant::AuxiliaryTargetActivation {
                pending,
                transition,
            } => CompletedBrowserHostTurnParticipant::AuxiliaryTargetActivation {
                completed: match pending {
                    Some(pending) => Some(pending.wait().await),
                    None => None,
                },
                transition,
            },
            PendingBrowserHostTurnParticipant::TargetClose(pending) => {
                CompletedBrowserHostTurnParticipant::TargetClose(pending.wait().await)
            }
        };
        CompletedBrowserHostTurn { participant }
    }
}

/// Move-owned participant completion ready for one later owner-loop turn.
#[must_use = "a completed Browser Host operation must be applied"]
pub struct CompletedBrowserHostTurn {
    participant: CompletedBrowserHostTurnParticipant,
}

enum CompletedBrowserHostTurnParticipant {
    PageCommand {
        completed: CompletedPageCommandDispatch,
        command_context: CommandDispatchContext,
        completion: BrowserHostPageStepCompletion,
    },
    PageTermination(CompletedPageTargetTerminationOwnerTask),
    StopLoading {
        completed: CompletedStopLoadingOwnerTask,
        command_context: CommandDispatchContext,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerStopLoadingCommand>,
    },
    ContextDisposal {
        completed: Box<CompletedBrowserContextDisposalOwnerTask>,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerContextDisposalCommand>,
    },
    PausedNavigationDecision {
        completed: Box<CompletedPausedNavigationDecisionOwnerTask>,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerPausedNavigationDecisionCommand>,
    },
    AuxiliaryTargetActivation {
        completed: Option<CompletedBrowserTargetPromotion>,
        transition: TargetActivationTransition,
    },
    TargetClose(CompletedTargetCloseOwnerTask),
}

/// Migration-period physical Page executor for actor-selected Browser Host
/// turns.
///
/// `CdpConnection` cannot construct [`BrowserHostTurn`], choose mailbox order,
/// or implement the executor capability. The application-owned execution lane
/// binds this short adapter only after Core has selected an exact turn. It
/// returns before any renderer/network participant wait.
impl BrowserHostTurnExecutor for BrowserHostTurnExecution<'_> {
    type Output = BrowserHostTurnDispatch;

    fn execute_browser_host_turn(&mut self, turn: BrowserHostTurn) -> Self::Output {
        if moli_trace::cdp_runtime_trace_enabled() {
            tracing::info!(
                target: "moli_cdp_runtime",
                stage = "browser_owner_input_start",
                kind = ?turn.kind(),
            );
        }
        match turn.into_input() {
            BrowserOwnerInput::FrontendCommand(BrowserFrontendCommand::DisposeBrowserContext(
                input,
            )) => {
                let (command_id, browser_context_handle) = input.into_parts();
                let Some(prepared) =
                    self.take_prepared_browser_owner_context_disposal_command(command_id)
                else {
                    tracing::error!(
                        browser_command_id = command_id.get(),
                        "dropping Browser Host Context-disposal turn without its prepared protocol projection"
                    );
                    return BrowserHostTurnDispatch::complete(
                        CdpTurnOutcome::new_with_protocol_events(
                            Vec::new(),
                            self.take_scheduler_events(),
                        ),
                    );
                };
                let (prefix_events, command_context, reply) = prepared.into_parts();
                let step = crate::domains::target::start_browser_context_disposal_owner_task(
                    self,
                    browser_context_handle,
                    prefix_events,
                    command_context,
                );
                self.browser_host_dispatch_from_context_disposal_step(step, reply)
            }
            BrowserOwnerInput::FrontendCommand(BrowserFrontendCommand::StopLoading(input)) => {
                let (command_id, page_owner) = input.into_parts();
                let Some(prepared) =
                    self.take_prepared_browser_owner_stop_loading_command(command_id)
                else {
                    tracing::error!(
                        browser_command_id = command_id.get(),
                        "dropping Browser Host stop-loading turn without its prepared protocol projection"
                    );
                    return BrowserHostTurnDispatch::complete(
                        CdpTurnOutcome::new_with_protocol_events(
                            Vec::new(),
                            self.take_scheduler_events(),
                        ),
                    );
                };
                let (command_context, reply) = prepared.into_parts();
                let step = crate::domains::page::start_page_owned_stop_loading_owner_task(
                    self,
                    &page_owner,
                );
                self.browser_host_dispatch_from_stop_loading_step(step, command_context, reply)
            }
            BrowserOwnerInput::FrontendCommand(
                BrowserFrontendCommand::ResolvePausedNavigation(input),
            ) => {
                let (command_id, page_owner, decision) = input.into_parts();
                let Some(prepared) =
                    self.take_prepared_browser_owner_paused_navigation_decision_command(command_id)
                else {
                    tracing::error!(
                        browser_command_id = command_id.get(),
                        "dropping Browser Host paused-navigation decision without its prepared protocol sidecar"
                    );
                    return BrowserHostTurnDispatch::complete(
                        CdpTurnOutcome::new_with_protocol_events(
                            Vec::new(),
                            self.take_scheduler_events(),
                        ),
                    );
                };
                let (pending_navigation, command_context, reply) = prepared.into_parts();
                let step =
                    crate::domains::fetch::start_page_owned_paused_navigation_decision_owner_task(
                        self,
                        page_owner,
                        pending_navigation,
                        decision,
                        command_context,
                    );
                self.browser_host_dispatch_from_paused_navigation_decision_step(step, reply)
            }
            BrowserOwnerInput::FrontendCommand(
                BrowserFrontendCommand::EnsureInitialTargetNavigation(input),
            ) => {
                let (command_id, page_owner, url) = input.into_parts();
                let reply = self
                    .take_prepared_browser_owner_initial_target_navigation_command(command_id)
                    .map(|prepared| prepared.into_reply());
                if reply.is_none() {
                    tracing::error!(
                        browser_command_id = command_id.get(),
                        "Browser Host initial-Target prerequisite lost its prepared Protocol continuation"
                    );
                }
                let Some(step) = crate::domains::page::start_page_owned_initial_target_navigation(
                    self,
                    &page_owner,
                    &url,
                ) else {
                    return match reply {
                        Some(reply) => self
                            .complete_browser_host_initial_target_navigation_command_projection(
                                crate::domains::command_output::CommandOutputPlan::default(),
                                reply,
                            ),
                        None => BrowserHostTurnDispatch::complete(
                            CdpTurnOutcome::new_with_protocol_events(
                                Vec::new(),
                                self.take_scheduler_events(),
                            ),
                        ),
                    };
                };
                let completion = match reply {
                    Some(reply) => {
                        BrowserHostPageStepCompletion::InitialTargetNavigationCommand { reply }
                    }
                    None => BrowserHostPageStepCompletion::BrowserEffects,
                };
                self.browser_host_dispatch_from_page_step_with_completion(
                    step,
                    CommandDispatchContext::default(),
                    completion,
                )
            }
            BrowserOwnerInput::FrontendCommand(command) => {
                let (command_id, page_owner, action) = match command {
                    BrowserFrontendCommand::Navigate(input) => {
                        let (command_id, page_owner, url, referrer) = input.into_parts();
                        (
                            command_id,
                            page_owner,
                            BrowserHostFrontendNavigationAction::Navigate { url, referrer },
                        )
                    }
                    BrowserFrontendCommand::Reload(input) => {
                        let (command_id, page_owner, ignore_cache, script_to_evaluate_on_load) =
                            input.into_parts();
                        (
                            command_id,
                            page_owner,
                            BrowserHostFrontendNavigationAction::Reload {
                                ignore_cache,
                                script_to_evaluate_on_load,
                            },
                        )
                    }
                    BrowserFrontendCommand::TraverseHistory(input) => {
                        let (command_id, page_owner, destination) = input.into_parts();
                        (
                            command_id,
                            page_owner,
                            BrowserHostFrontendNavigationAction::HistoryTraversal { destination },
                        )
                    }
                    BrowserFrontendCommand::StopLoading(_) => {
                        unreachable!("stop-loading handled by the preceding owner-input arm")
                    }
                    BrowserFrontendCommand::ResolvePausedNavigation(_) => unreachable!(
                        "paused navigation decision handled by the preceding owner-input arm"
                    ),
                    BrowserFrontendCommand::EnsureInitialTargetNavigation(_) => unreachable!(
                        "initial Target prerequisite handled by the preceding owner-input arm"
                    ),
                    BrowserFrontendCommand::DisposeBrowserContext(_) => {
                        tracing::error!(
                            "Context disposal escaped its dedicated Browser Host input arm"
                        );
                        return BrowserHostTurnDispatch::complete(
                            CdpTurnOutcome::new_with_protocol_events(
                                Vec::new(),
                                self.take_scheduler_events(),
                            ),
                        );
                    }
                };
                let Some(prepared) =
                    self.take_prepared_browser_owner_navigation_command(command_id)
                else {
                    tracing::error!(
                        browser_command_id = command_id.get(),
                        "dropping Browser Host navigation turn without its prepared protocol projection"
                    );
                    return BrowserHostTurnDispatch::complete(
                        CdpTurnOutcome::new_with_protocol_events(
                            Vec::new(),
                            self.take_scheduler_events(),
                        ),
                    );
                };
                let (
                    frontend_command_id,
                    frontend_session_id,
                    allow_background_navigation,
                    result_payload,
                    command_context,
                    reply,
                ) = prepared.into_parts();
                // Raw CDP retains a wire command id and completes through its
                // response-head early outcome. Only a direct typed wait:none
                // command needs the Host acceptance as its immediate result.
                let returns_direct_background_result =
                    allow_background_navigation && frontend_command_id.is_none();
                let accepted_background_target_id =
                    page_owner.target_id().map(BrowserTargetId::new);
                let accepted_background_loader_id = matches!(
                    &action,
                    BrowserHostFrontendNavigationAction::Navigate { .. }
                        | BrowserHostFrontendNavigationAction::Reload { .. }
                )
                .then(|| crate::domains::page::LOADER_ID.to_owned());
                let (requested_url, history_traversal, step) = match action {
                    BrowserHostFrontendNavigationAction::Navigate { url, referrer } => {
                        let step = crate::domains::page::start_page_owned_frontend_navigate_command(
                            self,
                            frontend_command_id,
                            frontend_session_id.as_deref(),
                            &page_owner,
                            &url,
                            referrer.as_deref(),
                            allow_background_navigation,
                            result_payload,
                        );
                        (url, None, step)
                    }
                    BrowserHostFrontendNavigationAction::Reload {
                        ignore_cache,
                        script_to_evaluate_on_load,
                    } => {
                        let (requested_url, step) =
                            crate::domains::page::start_page_owned_frontend_reload_command(
                                self,
                                frontend_command_id,
                                &page_owner,
                                ignore_cache,
                                script_to_evaluate_on_load,
                                allow_background_navigation,
                                result_payload,
                            );
                        (requested_url, None, step)
                    }
                    BrowserHostFrontendNavigationAction::HistoryTraversal { destination } => {
                        crate::domains::page::start_page_owned_frontend_history_traversal_command(
                            self,
                            frontend_command_id,
                            &page_owner,
                            destination,
                            allow_background_navigation,
                            result_payload,
                        )
                    }
                };
                let accepted_background_result = returns_direct_background_result.then(|| {
                    BrowserNavigateCommandResult::new(
                        requested_url.clone(),
                        accepted_background_target_id,
                        accepted_background_loader_id,
                        None,
                        None,
                    )
                });
                self.browser_host_dispatch_from_page_step_with_completion(
                    step,
                    command_context,
                    BrowserHostPageStepCompletion::FrontendNavigation {
                        requested_url,
                        history_traversal,
                        accepted_background_result,
                        reply,
                    },
                )
            }
            BrowserOwnerInput::RendererIntent(
                RendererBrowserIntent::TopLevelLocationNavigation(input),
            ) => {
                let (page_owner, navigation, trace) = input.into_parts();
                let step = crate::domains::page::start_page_owned_top_level_location_navigation(
                    self,
                    &page_owner,
                    navigation,
                    trace,
                );
                self.browser_host_dispatch_from_page_step(step, CommandDispatchContext::default())
            }
            BrowserOwnerInput::RendererIntent(RendererBrowserIntent::TopLevelHistoryTraversal(
                input,
            )) => {
                let (page_owner, delta) = input.into_parts();
                let step = crate::domains::page::start_page_owned_top_level_history_traversal(
                    self,
                    &page_owner,
                    delta,
                );
                self.browser_host_dispatch_from_page_step(step, CommandDispatchContext::default())
            }
            BrowserOwnerInput::InitialTargetNavigation(input) => {
                let (page_owner, url) = input.into_parts();
                let Some(step) = crate::domains::page::start_page_owned_initial_target_navigation(
                    self,
                    &page_owner,
                    &url,
                ) else {
                    return BrowserHostTurnDispatch::complete(
                        CdpTurnOutcome::new_with_protocol_events(
                            Vec::new(),
                            self.take_scheduler_events(),
                        ),
                    );
                };
                self.browser_host_dispatch_from_page_step_with_completion(
                    step,
                    CommandDispatchContext::default(),
                    BrowserHostPageStepCompletion::BrowserEffects,
                )
            }
            BrowserOwnerInput::RendererIntent(RendererBrowserIntent::AuxiliaryNavigation(
                input,
            )) => {
                let (page_owner, url, kind) = input.into_parts();
                let Some(step) = crate::domains::page::start_page_owned_auxiliary_navigation(
                    self,
                    &page_owner,
                    &url,
                    kind,
                    None,
                ) else {
                    return BrowserHostTurnDispatch::complete(
                        CdpTurnOutcome::new_with_protocol_events(
                            Vec::new(),
                            self.take_scheduler_events(),
                        ),
                    );
                };
                self.browser_host_dispatch_from_page_step(step, CommandDispatchContext::default())
            }
            BrowserOwnerInput::RendererIntent(
                RendererBrowserIntent::AuxiliaryTargetActivation(input),
            ) => {
                let (browser_context, target, navigation) = input.into_parts();
                let Some(navigation) = navigation else {
                    let scheduler_events = self.take_scheduler_events();
                    return self.start_browser_host_auxiliary_target_activation(
                        browser_context,
                        target,
                        Vec::new(),
                        scheduler_events,
                    );
                };
                let (page_owner, url, kind) = navigation.into_parts();
                let Some(step) = crate::domains::page::start_page_owned_auxiliary_navigation(
                    self,
                    &page_owner,
                    &url,
                    kind,
                    Some((browser_context, target)),
                ) else {
                    return BrowserHostTurnDispatch::complete(
                        CdpTurnOutcome::new_with_protocol_events(
                            Vec::new(),
                            self.take_scheduler_events(),
                        ),
                    );
                };
                self.browser_host_dispatch_from_page_step(step, CommandDispatchContext::default())
            }
            BrowserOwnerInput::PageTermination(input) => {
                let step = crate::domains::page::start_page_target_termination_owner_task(
                    self,
                    input.into_request(),
                );
                self.browser_host_dispatch_from_page_termination_step(step)
            }
            BrowserOwnerInput::TargetTermination(input) => {
                let step =
                    crate::domains::page::start_target_close_owner_task(self, input.into_request());
                self.browser_host_dispatch_from_target_close_step(step)
            }
        }
    }
}

// Unit tests inside this crate retain their compact direct-actor harness. The
// production library has no `BrowserHostTurnExecutor for CdpConnection` impl;
// application code must own `BrowserHostTurnExecutorOwner` and bind an exact
// short execution adapter.
#[cfg(test)]
impl BrowserHostTurnExecutor for CdpConnection {
    type Output = BrowserHostTurnDispatch;

    fn execute_browser_host_turn(&mut self, turn: BrowserHostTurn) -> Self::Output {
        let mut owner = BrowserHostTurnExecutorOwner::for_application_owner_lane();
        let mut execution = owner.bind_turn(self);
        BrowserHostTurnExecutor::execute_browser_host_turn(&mut execution, turn)
    }
}

impl BrowserHostTurnExecutorOwner {
    /// Starts the exact turn already selected by the application-owned Browser
    /// Host actor. The application-owned DevTools Host adapter supplies only
    /// a borrowed renderer/protocol projection and does not receive the turn
    /// selection capability itself.
    pub fn start_next_turn(
        &mut self,
        actor: &mut BrowserHostActor,
        host_adapter: &mut crate::DevToolsHostAdapter,
    ) -> Option<BrowserHostTurnDispatch> {
        let mut execution = host_adapter.bind_browser_host_turn(self);
        actor.complete_next_turn(&mut execution)
    }

    /// Applies one move-owned participant completion as a distinct later Host
    /// turn. No execution adapter survives the participant wait.
    pub async fn complete_turn(
        &mut self,
        host_adapter: &mut crate::DevToolsHostAdapter,
        completed: CompletedBrowserHostTurn,
    ) -> BrowserHostTurnDispatch {
        host_adapter
            .bind_browser_host_turn(self)
            .complete_browser_host_turn(completed)
            .await
    }
}

impl BrowserHostTurnExecution<'_> {
    /// Applies one exact participant completion as a later physical owner turn.
    ///
    /// This remains an async migration adapter because some completion paths
    /// still perform Page commit work in Protocol. The Browser Host mailbox is
    /// no longer borrowed while the participant itself is pending.
    pub async fn complete_browser_host_turn(
        &mut self,
        completed: CompletedBrowserHostTurn,
    ) -> BrowserHostTurnDispatch {
        let CompletedBrowserHostTurn { participant } = completed;
        match participant {
            CompletedBrowserHostTurnParticipant::PageCommand {
                completed,
                command_context,
                completion,
            } => {
                self.complete_browser_host_page_command_turn(completed, command_context, completion)
                    .await
            }
            CompletedBrowserHostTurnParticipant::PageTermination(completed) => {
                BrowserHostTurnDispatch::complete(
                    crate::domains::page::complete_page_target_termination_owner_task(
                        self, completed,
                    ),
                )
            }
            CompletedBrowserHostTurnParticipant::StopLoading {
                completed,
                command_context,
                reply,
            } => {
                let step = crate::domains::page::complete_page_owned_stop_loading_owner_task(
                    self, completed,
                )
                .await;
                self.browser_host_dispatch_from_stop_loading_step(step, command_context, reply)
            }
            CompletedBrowserHostTurnParticipant::ContextDisposal { completed, reply } => {
                let step = crate::domains::target::complete_browser_context_disposal_owner_task(
                    self, *completed,
                )
                .await;
                self.browser_host_dispatch_from_context_disposal_step(step, reply)
            }
            CompletedBrowserHostTurnParticipant::PausedNavigationDecision { completed, reply } => {
                let step = crate::domains::fetch::complete_page_owned_paused_navigation_decision_owner_task(
                    self,
                    *completed,
                )
                .await;
                self.browser_host_dispatch_from_paused_navigation_decision_step(step, reply)
            }
            CompletedBrowserHostTurnParticipant::AuxiliaryTargetActivation {
                completed,
                transition,
            } => {
                let activation_succeeded = match completed {
                    Some(completed) => match self
                        .finish_promote_background_target_to_active_for_connection(completed)
                    {
                        Ok(true) => true,
                        Ok(false) => false,
                        Err(error) => {
                            tracing::debug!(
                                %error,
                                "popup Target activation synchronization could not complete"
                            );
                            false
                        }
                    },
                    None => true,
                };
                let protocol_events = if activation_succeeded {
                    self.complete_staged_target_activation_async(&transition)
                        .await
                        .into_protocol_events()
                } else {
                    Vec::new()
                };
                BrowserHostTurnDispatch::complete(CdpTurnOutcome::new_with_protocol_events(
                    protocol_events,
                    self.take_scheduler_events(),
                ))
            }
            CompletedBrowserHostTurnParticipant::TargetClose(completed) => {
                let step = crate::domains::page::complete_target_close_owner_task(self, completed);
                self.browser_host_dispatch_from_target_close_step(step)
            }
        }
    }

    fn start_browser_host_auxiliary_target_activation(
        &mut self,
        browser_context_handle: BrowserContextHandle,
        target_handle: BrowserTargetHandle,
        prefix_protocol_events: Vec<crate::conn::BackgroundProtocolEvent>,
        prefix_scheduler_events: Vec<crate::conn::CdpSchedulerEvent>,
    ) -> BrowserHostTurnDispatch {
        let browser_context_id = browser_context_handle.browser_context_id().to_owned();
        let target_id = target_handle.target_id().to_owned();
        let core_is_current_and_selected = {
            let browser_host_state = self.browser_host_state();
            let owner = browser_host_state.navigation_owner();
            owner.selected_browser_context_id() == Some(browser_context_id.as_str())
                && owner.browser_context_handle_is_current(&browser_context_handle)
                && owner.target_handle_is_current(&target_handle)
        };
        let physical_is_current = self
            .browser_context
            .as_ref()
            .is_some_and(|browser_context| {
                browser_context.browser_context_handle() == &browser_context_handle
                    && browser_context.top_level_target_handle(&target_id) == Some(&target_handle)
                    && (browser_context.is_active_target(&target_id)
                        || browser_context.background_target(&target_id).is_some())
            });
        if !core_is_current_and_selected || !physical_is_current {
            tracing::debug!(
                browser_context_id,
                target_id,
                "dropping popup activation after its exact selected Target retired or moved"
            );
            return BrowserHostTurnDispatch::complete(CdpTurnOutcome::new_with_protocol_events(
                prefix_protocol_events,
                prefix_scheduler_events,
            ));
        }

        let previous_active_target_id = self
            .browser_context
            .as_ref()
            .and_then(|browser_context| browser_context.active_target_id_owned());
        let transition =
            TargetActivationTransition::new(target_id.clone(), previous_active_target_id);
        let pending =
            match self.start_promote_background_target_to_active_for_connection(&target_id) {
                Ok(BrowserTargetPromotionStart::Complete(true)) => None,
                Ok(BrowserTargetPromotionStart::Pending(pending)) => Some(pending),
                Ok(BrowserTargetPromotionStart::Complete(false)) => {
                    tracing::debug!(
                        browser_context_id,
                        target_id,
                        "dropping popup activation after its Target lost foreground eligibility"
                    );
                    return BrowserHostTurnDispatch::complete(
                        CdpTurnOutcome::new_with_protocol_events(
                            prefix_protocol_events,
                            prefix_scheduler_events,
                        ),
                    );
                }
                Err(error) => {
                    tracing::debug!(
                        %error,
                        browser_context_id,
                        target_id,
                        "popup Target could not begin activation"
                    );
                    return BrowserHostTurnDispatch::complete(
                        CdpTurnOutcome::new_with_protocol_events(
                            prefix_protocol_events,
                            prefix_scheduler_events,
                        ),
                    );
                }
            };
        BrowserHostTurnDispatch::pending(
            CdpTurnOutcome::new_with_protocol_events(
                prefix_protocol_events,
                prefix_scheduler_events,
            ),
            PendingBrowserHostTurn {
                participant: PendingBrowserHostTurnParticipant::AuxiliaryTargetActivation {
                    pending,
                    transition,
                },
            },
        )
    }
}

impl CdpConnection {
    #[cfg(test)]
    pub async fn complete_browser_host_turn(
        &mut self,
        completed: CompletedBrowserHostTurn,
    ) -> BrowserHostTurnDispatch {
        let mut owner = BrowserHostTurnExecutorOwner::for_application_owner_lane();
        owner
            .bind_turn(self)
            .complete_browser_host_turn(completed)
            .await
    }
}

impl BrowserHostTurnExecution<'_> {
    async fn complete_browser_host_page_command_turn(
        &mut self,
        completed: CompletedPageCommandDispatch,
        mut command_context: CommandDispatchContext,
        mut completion: BrowserHostPageStepCompletion,
    ) -> BrowserHostTurnDispatch {
        if completed.renderer_accepted_same_document_history_traversal() == Some(false)
            && let BrowserHostPageStepCompletion::FrontendNavigation {
                history_traversal, ..
            } = &mut completion
            && matches!(
                history_traversal,
                Some(BrowserHistoryTraversalResult::SameDocument)
            )
        {
            *history_traversal = Some(BrowserHistoryTraversalResult::CrossDocument);
        }
        let step = crate::domains::page::complete_pending_page_command(
            self,
            completed,
            &mut command_context,
        )
        .await;
        self.browser_host_dispatch_from_page_step_with_completion(step, command_context, completion)
    }
}

impl CdpConnection {
    #[cfg(test)]
    pub(crate) async fn finish_browser_host_turn_for_test(
        &mut self,
        mut dispatch: BrowserHostTurnDispatch,
    ) -> CdpRendererOwnerTurnOutcome {
        let mut protocol_events = Vec::new();
        let mut post_renderer_output_events = Vec::new();
        let mut renderer_output_boundary = None;
        let mut post_response_events = Vec::new();
        let mut scheduler_events = Vec::new();
        let mut renderer_output_predecessor = None;
        loop {
            let (outcome, pending) = dispatch.into_parts();
            let (
                events,
                post_boundary_events,
                boundary,
                outcome_post_response_events,
                nested_scheduler_events,
                predecessor,
            ) = outcome.into_renderer_owner_turn_parts();
            if renderer_output_boundary.is_some() {
                assert!(
                    boundary.is_none(),
                    "one Browser Host action cannot introduce multiple renderer boundaries"
                );
                post_renderer_output_events.extend(events);
                post_renderer_output_events.extend(post_boundary_events);
            } else {
                protocol_events.extend(events);
                if let Some(boundary) = boundary {
                    renderer_output_boundary = Some(boundary);
                    post_renderer_output_events.extend(post_boundary_events);
                } else {
                    assert!(
                        post_boundary_events.is_empty(),
                        "post-renderer output requires an exact boundary"
                    );
                }
            }
            post_response_events.extend(outcome_post_response_events);
            scheduler_events.extend(nested_scheduler_events);
            if let Some(predecessor) = predecessor {
                predecessor.merge_into_same_stream_tail(&mut renderer_output_predecessor);
            }
            let Some(pending) = pending else {
                return CdpTurnOutcome::new_with_protocol_and_post_response_events(
                    protocol_events,
                    post_response_events,
                    scheduler_events,
                )
                .with_renderer_output_boundary(
                    renderer_output_boundary,
                    post_renderer_output_events,
                )
                .with_renderer_output_predecessor(renderer_output_predecessor);
            };
            dispatch = self.complete_browser_host_turn(pending.wait().await).await;
        }
    }

    fn browser_host_dispatch_from_page_step(
        &mut self,
        step: PageCommandTaskStep,
        command_context: CommandDispatchContext,
    ) -> BrowserHostTurnDispatch {
        self.browser_host_dispatch_from_page_step_with_completion(
            step,
            command_context,
            BrowserHostPageStepCompletion::BrowserEffects,
        )
    }

    fn browser_host_dispatch_from_page_step_with_completion(
        &mut self,
        step: PageCommandTaskStep,
        command_context: CommandDispatchContext,
        completion: BrowserHostPageStepCompletion,
    ) -> BrowserHostTurnDispatch {
        let scheduler_events = self.take_scheduler_events();
        match step {
            PageCommandTaskStep::Complete(plan) => match completion {
                BrowserHostPageStepCompletion::BrowserEffects => {
                    let (_, protocol_events) = plan.into_command_status_and_background_events();
                    BrowserHostTurnDispatch::complete(CdpTurnOutcome::new_with_protocol_events(
                        protocol_events,
                        scheduler_events,
                    ))
                }
                BrowserHostPageStepCompletion::InitialTargetNavigationCommand { reply } => self
                    .complete_browser_host_initial_target_navigation_command_projection(
                        plan, reply,
                    ),
                BrowserHostPageStepCompletion::FrontendNavigation {
                    requested_url,
                    history_traversal,
                    accepted_background_result,
                    reply,
                } => {
                    let (mut outcome, projection) =
                        plan.into_browser_navigate_outcome_and_projection(requested_url.as_str());
                    if outcome.is_none()
                        && let Some(result) = accepted_background_result
                    {
                        // wait:none reaches its response boundary when this
                        // actor-selected start has admitted the detached
                        // navigation. DCL/load remain later Browser facts.
                        outcome = Some(BrowserNavigateCommandOutcome::Completed(result));
                    }
                    if let Some(history_traversal) = history_traversal {
                        outcome = outcome
                            .map(|outcome| outcome.with_history_traversal(history_traversal));
                    }
                    let completed = CompletedBrowserOwnerNavigationCommand::new(
                        outcome,
                        projection,
                        command_context,
                    );
                    match reply.send(completed) {
                        Ok(()) => BrowserHostTurnDispatch::complete(
                            CdpTurnOutcome::new_with_protocol_events(Vec::new(), scheduler_events),
                        ),
                        Err(completed) => {
                            let (_, projection, command_context) = completed.into_parts();
                            BrowserHostTurnDispatch::complete(
                                self.settle_detached_command_projection(
                                    command_context,
                                    projection.into_browser_effects_plan(),
                                    scheduler_events,
                                ),
                            )
                        }
                    }
                }
            },
            PageCommandTaskStep::Pending(pending) => BrowserHostTurnDispatch::pending(
                CdpTurnOutcome::new_with_protocol_events(Vec::new(), scheduler_events),
                PendingBrowserHostTurn {
                    participant: PendingBrowserHostTurnParticipant::PageCommand {
                        pending,
                        command_context,
                        completion,
                    },
                },
            ),
        }
    }

    fn browser_host_dispatch_from_page_termination_step(
        &mut self,
        step: PageTargetTerminationOwnerTaskStep,
    ) -> BrowserHostTurnDispatch {
        match step {
            PageTargetTerminationOwnerTaskStep::Complete(outcome) => {
                BrowserHostTurnDispatch::complete(outcome)
            }
            PageTargetTerminationOwnerTaskStep::Pending(pending) => {
                BrowserHostTurnDispatch::pending(
                    CdpTurnOutcome::new_with_protocol_events(
                        Vec::new(),
                        self.take_scheduler_events(),
                    ),
                    PendingBrowserHostTurn {
                        participant: PendingBrowserHostTurnParticipant::PageTermination(*pending),
                    },
                )
            }
        }
    }

    fn complete_browser_host_initial_target_navigation_command_projection(
        &mut self,
        plan: crate::domains::command_output::CommandOutputPlan,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerInitialTargetNavigationCommand>,
    ) -> BrowserHostTurnDispatch {
        let scheduler_events = self.take_scheduler_events();
        let completed = CompletedBrowserOwnerInitialTargetNavigationCommand::new(plan);
        match reply.send(completed) {
            Ok(()) => BrowserHostTurnDispatch::complete(CdpTurnOutcome::new_with_protocol_events(
                Vec::new(),
                scheduler_events,
            )),
            Err(completed) => {
                BrowserHostTurnDispatch::complete(self.settle_detached_command_projection(
                    CommandDispatchContext::default(),
                    completed.into_plan(),
                    scheduler_events,
                ))
            }
        }
    }

    fn browser_host_dispatch_from_target_close_step(
        &mut self,
        step: TargetCloseOwnerTaskStep,
    ) -> BrowserHostTurnDispatch {
        match step {
            TargetCloseOwnerTaskStep::Complete(outcome) => {
                BrowserHostTurnDispatch::complete(outcome)
            }
            TargetCloseOwnerTaskStep::Pending(pending) => BrowserHostTurnDispatch::pending(
                CdpTurnOutcome::new_with_protocol_events(Vec::new(), self.take_scheduler_events()),
                PendingBrowserHostTurn {
                    participant: PendingBrowserHostTurnParticipant::TargetClose(*pending),
                },
            ),
        }
    }

    fn browser_host_dispatch_from_context_disposal_step(
        &mut self,
        step: BrowserContextDisposalOwnerTaskStep,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerContextDisposalCommand>,
    ) -> BrowserHostTurnDispatch {
        match step {
            BrowserContextDisposalOwnerTaskStep::Pending(pending) => {
                BrowserHostTurnDispatch::pending(
                    CdpTurnOutcome::new_with_protocol_events(
                        Vec::new(),
                        self.take_scheduler_events(),
                    ),
                    PendingBrowserHostTurn {
                        participant: PendingBrowserHostTurnParticipant::ContextDisposal {
                            pending,
                            reply,
                        },
                    },
                )
            }
            BrowserContextDisposalOwnerTaskStep::Complete(output) => {
                let (plan, command_context) = output.into_parts();
                let scheduler_events = self.take_scheduler_events();
                let completed =
                    CompletedBrowserOwnerContextDisposalCommand::new(plan, command_context);
                match reply.send(completed) {
                    Ok(()) => BrowserHostTurnDispatch::complete(
                        CdpTurnOutcome::new_with_protocol_events(Vec::new(), scheduler_events),
                    ),
                    Err(completed) => {
                        let (plan, command_context) = completed.into_parts();
                        BrowserHostTurnDispatch::complete(self.settle_detached_command_projection(
                            command_context,
                            plan,
                            scheduler_events,
                        ))
                    }
                }
            }
        }
    }

    fn browser_host_dispatch_from_stop_loading_step(
        &mut self,
        step: StopLoadingOwnerTaskStep,
        command_context: CommandDispatchContext,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerStopLoadingCommand>,
    ) -> BrowserHostTurnDispatch {
        match step {
            StopLoadingOwnerTaskStep::Complete(plan) => {
                self.complete_browser_host_stop_loading_projection(plan, command_context, reply)
            }
            StopLoadingOwnerTaskStep::Pending(pending) => BrowserHostTurnDispatch::pending(
                CdpTurnOutcome::new_with_protocol_events(Vec::new(), self.take_scheduler_events()),
                PendingBrowserHostTurn {
                    participant: PendingBrowserHostTurnParticipant::StopLoading {
                        pending,
                        command_context,
                        reply,
                    },
                },
            ),
        }
    }

    fn complete_browser_host_stop_loading_projection(
        &mut self,
        plan: crate::domains::command_output::CommandOutputPlan,
        command_context: CommandDispatchContext,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerStopLoadingCommand>,
    ) -> BrowserHostTurnDispatch {
        let scheduler_events = self.take_scheduler_events();
        let completed = CompletedBrowserOwnerStopLoadingCommand::new(plan, command_context);
        match reply.send(completed) {
            Ok(()) => BrowserHostTurnDispatch::complete(CdpTurnOutcome::new_with_protocol_events(
                Vec::new(),
                scheduler_events,
            )),
            Err(completed) => {
                let (plan, command_context) = completed.into_parts();
                BrowserHostTurnDispatch::complete(self.settle_detached_command_projection(
                    command_context,
                    plan,
                    scheduler_events,
                ))
            }
        }
    }

    fn browser_host_dispatch_from_paused_navigation_decision_step(
        &mut self,
        step: PausedNavigationDecisionOwnerTaskStep,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerPausedNavigationDecisionCommand>,
    ) -> BrowserHostTurnDispatch {
        match step {
            PausedNavigationDecisionOwnerTaskStep::Complete(output) => {
                let (plan, command_context) = output.into_parts();
                self.complete_browser_host_paused_navigation_decision_projection(
                    plan,
                    command_context,
                    reply,
                )
            }
            PausedNavigationDecisionOwnerTaskStep::Pending(pending) => {
                BrowserHostTurnDispatch::pending(
                    CdpTurnOutcome::new_with_protocol_events(
                        Vec::new(),
                        self.take_scheduler_events(),
                    ),
                    PendingBrowserHostTurn {
                        participant: PendingBrowserHostTurnParticipant::PausedNavigationDecision {
                            pending,
                            reply,
                        },
                    },
                )
            }
        }
    }

    fn complete_browser_host_paused_navigation_decision_projection(
        &mut self,
        plan: crate::domains::command_output::CommandOutputPlan,
        command_context: CommandDispatchContext,
        reply: tokio::sync::oneshot::Sender<CompletedBrowserOwnerPausedNavigationDecisionCommand>,
    ) -> BrowserHostTurnDispatch {
        let scheduler_events = self.take_scheduler_events();
        let completed =
            CompletedBrowserOwnerPausedNavigationDecisionCommand::new(plan, command_context);
        match reply.send(completed) {
            Ok(()) => BrowserHostTurnDispatch::complete(CdpTurnOutcome::new_with_protocol_events(
                Vec::new(),
                scheduler_events,
            )),
            Err(completed) => {
                let (plan, command_context) = completed.into_parts();
                BrowserHostTurnDispatch::complete(self.settle_detached_command_projection(
                    command_context,
                    plan,
                    scheduler_events,
                ))
            }
        }
    }
}
