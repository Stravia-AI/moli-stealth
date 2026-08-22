use std::{future::Future, pin::Pin};

use moli_core::browser_host::BrowserNavigationFailure;
use moli_core::runtime::{
    NavigationEngine, PreparedDocumentPageCommitConfiguration, PreparedDocumentPageCommitPermit,
};

use crate::conn::{
    BackgroundNavigationGateKey, BackgroundNavigationLoadJob, BackgroundProtocolEvent,
    CdpConnection, CdpSessionRoute, CommandDispatchContext, CommittedRendererAgentAttachment,
    DocumentNavigationToken, NavigationDispatchState, NavigationLoadOutcome, ResponseCommitReady,
};
use crate::domains::{
    activity,
    command_output::{CommandOutputBuffer, CommandOutputPlan},
    network,
};

use super::{
    navigation::{
        MaterializedNavigationCompletion, apply_materialized_navigation_into_buffer_async,
        emit_navigation_ready_trace, push_navigation_commit_failure,
    },
    navigation_commit::{
        CompletedLoadedNavigationCommit, CompletedLoadedNavigationPageDisposal,
        CompletedLoadedNavigationPreloadListeners, LoadedNavigationCommitApplyStart,
        LoadedNavigationCommitStart, LoadedNavigationPostDisposalStart,
        PendingLoadedNavigationCommit, PendingLoadedNavigationPageDisposal,
        PendingLoadedNavigationPreloadListeners, complete_loaded_navigation_preload_listeners,
        start_completed_loaded_navigation_commit, start_loaded_navigation_commit,
        start_loaded_navigation_commit_after_page_disposal,
    },
    navigation_tail::{
        CompletedNavigationTail, NavigationTailStep, PendingNavigationTail,
        complete_materialized_navigation_tail_async, start_materialized_navigation_tail,
    },
};

/// Move-owned participant wait for one direct top-level navigation.
///
/// Each phase contains every exact browser/renderer capability needed by the
/// participant. No phase borrows `CdpConnection`, so the Browser Host lane can
/// register the wait and resume serving later owner inputs.
pub(crate) struct PendingNavigateCommand {
    prefix_events: Vec<BackgroundProtocolEvent>,
    phase: PendingNavigateCommandPhase,
}

/// Move-owned result of one navigation participant wait.
pub(crate) struct CompletedNavigateCommand {
    prefix_events: Vec<BackgroundProtocolEvent>,
    phase: CompletedNavigateCommandPhase,
}

enum PendingNavigateCommandPhase {
    Load {
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        job: Box<BackgroundNavigationLoadJob>,
    },
    ConfigurePreparedDocument {
        token: DocumentNavigationToken,
        state: Box<NavigationDispatchState>,
        navigation: Box<ResponseCommitReady>,
        engine: Box<Option<NavigationEngine>>,
        configuration: PreparedDocumentPageCommitConfiguration,
    },
    CommitPreparedDocument {
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        navigation: Box<ResponseCommitReady>,
        engine: Box<Option<NavigationEngine>>,
        permit: PreparedDocumentPageCommitPermit,
        renderer_attachment: CommittedRendererAgentAttachment,
    },
    RestoreLoadedPage {
        output: Box<CommandOutputBuffer>,
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        engine: Box<Option<NavigationEngine>>,
        pending: Box<PendingLoadedNavigationCommit>,
    },
    DisposeReplacedPage {
        output: Box<CommandOutputBuffer>,
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        engine: Box<Option<NavigationEngine>>,
        pending: Box<PendingLoadedNavigationPageDisposal>,
    },
    StartPreloadListeners {
        output: Box<CommandOutputBuffer>,
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        engine: Box<Option<NavigationEngine>>,
        pending: Box<PendingLoadedNavigationPreloadListeners>,
    },
    ReplayRendererCalls {
        output: Box<CommandOutputBuffer>,
        pending: PendingNavigationTail,
    },
}

enum CompletedNavigateCommandPhase {
    Load {
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        engine: NavigationEngine,
        navigation: Result<NavigationLoadOutcome, String>,
    },
    Materialized {
        completion: MaterializedNavigationCompletion,
    },
    ConfigurePreparedDocument {
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        navigation: Box<ResponseCommitReady>,
        engine: Option<NavigationEngine>,
        result: Result<(), String>,
    },
    CommitPreparedDocument {
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        engine: Option<NavigationEngine>,
        renderer_attachment: CommittedRendererAgentAttachment,
        navigation: Box<Result<crate::conn::LoadedNavigation, String>>,
    },
    RestoreLoadedPage {
        output: Box<CommandOutputBuffer>,
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        engine: Box<Option<NavigationEngine>>,
        completed: Box<CompletedLoadedNavigationCommit>,
    },
    DisposeReplacedPage {
        output: Box<CommandOutputBuffer>,
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        engine: Box<Option<NavigationEngine>>,
        completed: Box<CompletedLoadedNavigationPageDisposal>,
    },
    StartPreloadListeners {
        output: Box<CommandOutputBuffer>,
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        engine: Box<Option<NavigationEngine>>,
        completed: Box<CompletedLoadedNavigationPreloadListeners>,
    },
    ReplayRendererCalls {
        output: Box<CommandOutputBuffer>,
        completed: CompletedNavigationTail,
    },
}

pub(crate) enum NavigateCommandCompletion {
    Pending(Box<PendingNavigateCommand>),
    Complete(CommandOutputPlan),
}

impl PendingNavigateCommand {
    pub(crate) fn load(
        prefix_events: Vec<BackgroundProtocolEvent>,
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        job: BackgroundNavigationLoadJob,
    ) -> Self {
        Self {
            prefix_events,
            phase: PendingNavigateCommandPhase::Load {
                token,
                state,
                job: Box::new(job),
            },
        }
    }

    pub(super) fn prefix_events_mut(&mut self) -> &mut Vec<BackgroundProtocolEvent> {
        &mut self.prefix_events
    }

    /// Waits for exactly one owner participant behind a type-erased heap
    /// boundary. Adding a concrete phase must not enlarge every protocol
    /// command wait future.
    pub(crate) fn wait(self: Box<Self>) -> Pin<Box<dyn Future<Output = CompletedNavigateCommand>>> {
        Box::pin(async move {
            let PendingNavigateCommand {
                prefix_events,
                phase,
            } = *self;
            let phase = match phase {
                PendingNavigateCommandPhase::Load { token, state, job } => {
                    let (engine, navigation, _early_outcome_sent) = job.run(None).await;
                    CompletedNavigateCommandPhase::Load {
                        token,
                        state,
                        engine,
                        navigation,
                    }
                }
                PendingNavigateCommandPhase::ConfigurePreparedDocument {
                    token,
                    state,
                    navigation,
                    engine,
                    configuration,
                } => {
                    let result = navigation.update_commit_configuration(configuration).await;
                    CompletedNavigateCommandPhase::ConfigurePreparedDocument {
                        token,
                        state: *state,
                        navigation,
                        engine: *engine,
                        result,
                    }
                }
                PendingNavigateCommandPhase::CommitPreparedDocument {
                    token,
                    state,
                    navigation,
                    engine,
                    permit,
                    renderer_attachment,
                } => {
                    let navigation = (*navigation).commit(permit).await;
                    CompletedNavigateCommandPhase::CommitPreparedDocument {
                        token,
                        state,
                        engine: *engine,
                        renderer_attachment,
                        navigation: Box::new(navigation),
                    }
                }
                PendingNavigateCommandPhase::RestoreLoadedPage {
                    output,
                    token,
                    state,
                    engine,
                    pending,
                } => CompletedNavigateCommandPhase::RestoreLoadedPage {
                    output,
                    token,
                    state,
                    engine,
                    completed: Box::new((*pending).wait().await),
                },
                PendingNavigateCommandPhase::DisposeReplacedPage {
                    output,
                    token,
                    state,
                    engine,
                    pending,
                } => CompletedNavigateCommandPhase::DisposeReplacedPage {
                    output,
                    token,
                    state,
                    engine,
                    completed: Box::new((*pending).wait().await),
                },
                PendingNavigateCommandPhase::StartPreloadListeners {
                    output,
                    token,
                    state,
                    engine,
                    pending,
                } => CompletedNavigateCommandPhase::StartPreloadListeners {
                    output,
                    token,
                    state,
                    engine,
                    // The preload batch owns multiple exact renderer operation
                    // shapes. It is one participant, but its concrete future must
                    // not inflate every navigation wait branch.
                    completed: Box::new(pending.wait().await),
                },
                PendingNavigateCommandPhase::ReplayRendererCalls { output, pending } => {
                    CompletedNavigateCommandPhase::ReplayRendererCalls {
                        output,
                        completed: pending.wait().await,
                    }
                }
            };
            CompletedNavigateCommand {
                prefix_events,
                phase,
            }
        })
    }
}

impl CompletedNavigateCommand {
    pub(crate) fn loaded(
        prefix_events: Vec<BackgroundProtocolEvent>,
        token: DocumentNavigationToken,
        state: NavigationDispatchState,
        engine: NavigationEngine,
        navigation: Result<NavigationLoadOutcome, String>,
    ) -> Self {
        Self {
            prefix_events,
            phase: CompletedNavigateCommandPhase::Load {
                token,
                state,
                engine,
                navigation,
            },
        }
    }

    pub(crate) fn materialized(completion: MaterializedNavigationCompletion) -> Self {
        Self::materialized_with_prefix(Vec::new(), completion)
    }

    pub(crate) fn materialized_with_prefix(
        prefix_events: Vec<BackgroundProtocolEvent>,
        completion: MaterializedNavigationCompletion,
    ) -> Self {
        Self {
            prefix_events,
            phase: CompletedNavigateCommandPhase::Materialized { completion },
        }
    }

    pub(super) fn load_result(
        &self,
    ) -> Option<(
        &DocumentNavigationToken,
        &NavigationDispatchState,
        &Result<NavigationLoadOutcome, String>,
    )> {
        let CompletedNavigateCommandPhase::Load {
            token,
            state,
            navigation,
            ..
        } = &self.phase
        else {
            return None;
        };
        Some((token, state, navigation))
    }
}

/// Applies one completed navigation participant behind the same bounded
/// future boundary used by direct and background frontend loops.
pub(crate) fn complete_pending_navigate_command<'a>(
    conn: &'a mut CdpConnection,
    completed: CompletedNavigateCommand,
    command_context: &'a mut crate::conn::CommandDispatchContext,
) -> Pin<Box<dyn Future<Output = NavigateCommandCompletion> + 'a>> {
    Box::pin(async move {
        let CompletedNavigateCommand {
            prefix_events,
            phase,
        } = completed;
        match phase {
            CompletedNavigateCommandPhase::Load {
                token,
                state,
                engine,
                navigation,
            } => {
                let is_current = conn.accepts_pending_document_navigation_for_session_owner(
                    state.navigate_session_id.as_deref(),
                    &token,
                );
                let should_retain_engine = is_current
                    && matches!(
                        navigation,
                        Ok(NavigationLoadOutcome::ResponseCommitReady(_)
                            | NavigationLoadOutcome::Loaded(_))
                    );
                let navigation =
                    network::materialize_navigation_load_result(conn, &state, navigation);
                let completion = MaterializedNavigationCompletion::new(token, state, navigation);
                let completion = if should_retain_engine {
                    completion.with_navigation_engine(engine)
                } else {
                    completion
                };
                complete_materialized_navigate_command(
                    conn,
                    prefix_events,
                    completion,
                    command_context,
                )
                .await
            }
            CompletedNavigateCommandPhase::Materialized { completion } => {
                complete_materialized_navigate_command(
                    conn,
                    prefix_events,
                    completion,
                    command_context,
                )
                .await
            }
            CompletedNavigateCommandPhase::ConfigurePreparedDocument {
                token,
                state,
                navigation,
                engine,
                result,
            } => {
                if let Err(error) = result {
                    let mut output = CommandOutputBuffer::default();
                    output.extend_background_events_after_messages(prefix_events);
                    push_navigation_commit_failure(conn, &mut output, &token, &state, error);
                    return finish_or_suspend_navigation_tail(
                        conn, output, &token, &state, engine, None,
                    );
                }
                let renderer_page = navigation.renderer_page_residence_identity();
                let candidate = conn.prepare_renderer_agent_candidate_token_for_session_owner(
                    state.navigate_session_id.as_deref(),
                    &token,
                    navigation.renderer_devtools_agent_token(),
                );
                match candidate.and_then(|candidate| {
                    conn.commit_renderer_agent_candidate_for_session_owner(
                        state.navigate_session_id.as_deref(),
                        candidate,
                        renderer_page,
                    )
                }) {
                    Ok(renderer_attachment) => {
                        let permit = navigation.issue_commit_permit();
                        NavigateCommandCompletion::Pending(Box::new(PendingNavigateCommand {
                            prefix_events,
                            phase: PendingNavigateCommandPhase::CommitPreparedDocument {
                                token,
                                state,
                                navigation,
                                engine: Box::new(engine),
                                permit,
                                renderer_attachment,
                            },
                        }))
                    }
                    Err(error) => {
                        tracing::debug!(
                            %error,
                            session_id = state.navigate_session_id.as_deref(),
                            loader_id = token.loader_id(),
                            "dropping superseded response commit-ready navigation"
                        );
                        let mut output = CommandOutputBuffer::default();
                        output.extend_background_events_after_messages(prefix_events);
                        push_navigation_commit_failure(conn, &mut output, &token, &state, error);
                        finish_or_suspend_navigation_tail(
                            conn, output, &token, &state, engine, None,
                        )
                    }
                }
            }
            CompletedNavigateCommandPhase::CommitPreparedDocument {
                token,
                state,
                engine,
                renderer_attachment,
                navigation,
            } => {
                let mut output = CommandOutputBuffer::default();
                output.extend_background_events_after_messages(prefix_events);
                match *navigation {
                    Ok(navigation) => {
                        let navigation = network::materialize_loaded_navigation_progress(
                            conn, &state, navigation,
                        );
                        return start_or_complete_loaded_navigation(
                            conn,
                            output,
                            token,
                            state,
                            navigation,
                            Some(renderer_attachment),
                            engine,
                            command_context,
                        )
                        .await;
                    }
                    Err(error) => {
                        if let Err(rollback_error) = conn
                            .rollback_committed_renderer_agent_candidate_for_session_owner(
                                state.navigate_session_id.as_deref(),
                                renderer_attachment,
                            )
                        {
                            tracing::warn!(
                                %rollback_error,
                                session_id = state.navigate_session_id.as_deref(),
                                "failed to roll back renderer channel after prepared document commit failure"
                            );
                        }
                        push_navigation_commit_failure(conn, &mut output, &token, &state, error);
                    }
                }
                finish_or_suspend_navigation_tail(conn, output, &token, &state, engine, None)
            }
            CompletedNavigateCommandPhase::RestoreLoadedPage {
                output,
                token,
                state,
                engine,
                completed,
            } => {
                start_or_complete_loaded_navigation_page_install(
                    conn,
                    *output,
                    token,
                    state,
                    *engine,
                    *completed,
                    command_context,
                )
                .await
            }
            CompletedNavigateCommandPhase::DisposeReplacedPage {
                output,
                token,
                state,
                engine,
                completed,
            } => {
                let mut output = *output;
                let step = start_loaded_navigation_commit_after_page_disposal(
                    conn,
                    &mut output,
                    *completed,
                );
                continue_loaded_navigation_post_disposal(conn, output, token, state, *engine, step)
            }
            CompletedNavigateCommandPhase::StartPreloadListeners {
                output,
                token,
                state,
                engine,
                completed,
            } => {
                let mut output = *output;
                let step =
                    complete_loaded_navigation_preload_listeners(conn, &mut output, *completed);
                continue_loaded_navigation_post_disposal(conn, output, token, state, *engine, step)
            }
            CompletedNavigateCommandPhase::ReplayRendererCalls {
                mut output,
                completed,
            } => {
                match complete_materialized_navigation_tail_async(conn, &mut output, completed)
                    .await
                {
                    NavigationTailStep::Pending(pending) => {
                        NavigateCommandCompletion::Pending(Box::new(PendingNavigateCommand {
                            prefix_events: Vec::new(),
                            phase: PendingNavigateCommandPhase::ReplayRendererCalls {
                                output,
                                pending: *pending,
                            },
                        }))
                    }
                    NavigationTailStep::Complete => {
                        NavigateCommandCompletion::Complete((*output).into_plan())
                    }
                }
            }
        }
    })
}

fn continue_loaded_navigation_post_disposal(
    conn: &mut CdpConnection,
    output: CommandOutputBuffer,
    token: DocumentNavigationToken,
    state: NavigationDispatchState,
    engine: Option<NavigationEngine>,
    step: LoadedNavigationPostDisposalStart,
) -> NavigateCommandCompletion {
    match step {
        LoadedNavigationPostDisposalStart::Pending(pending) => {
            NavigateCommandCompletion::Pending(Box::new(PendingNavigateCommand {
                prefix_events: Vec::new(),
                phase: PendingNavigateCommandPhase::StartPreloadListeners {
                    output: Box::new(output),
                    token,
                    state,
                    engine: Box::new(engine),
                    pending,
                },
            }))
        }
        LoadedNavigationPostDisposalStart::Ready(committed_owner) => {
            finish_or_suspend_navigation_tail(conn, output, &token, &state, engine, committed_owner)
        }
    }
}

fn finish_or_suspend_navigation_tail(
    conn: &mut CdpConnection,
    mut output: CommandOutputBuffer,
    token: &DocumentNavigationToken,
    state: &NavigationDispatchState,
    engine: Option<NavigationEngine>,
    committed_owner: Option<moli_core::browser_host::BrowserPageOwnerKey>,
) -> NavigateCommandCompletion {
    // Engine residence is target-keyed during this migration. Commit it in
    // the same apply turn as the exact Document replacement, before replay
    // waits allow a successor navigation to replace that Target again.
    conn.adopt_materialized_navigation_engine(engine, committed_owner);
    match start_materialized_navigation_tail(conn, &mut output, token, state) {
        NavigationTailStep::Pending(pending) => {
            NavigateCommandCompletion::Pending(Box::new(PendingNavigateCommand {
                prefix_events: Vec::new(),
                phase: PendingNavigateCommandPhase::ReplayRendererCalls {
                    output: Box::new(output),
                    pending: *pending,
                },
            }))
        }
        NavigationTailStep::Complete => NavigateCommandCompletion::Complete(output.into_plan()),
    }
}

/// Starts cancellation of one already-admitted main-Document navigation
/// while preserving the currently committed Document.
///
/// This is the synchronous subset used by Browser-owner cancellation chains.
/// The failure body itself cannot replace or discard the Page; only an exact
/// renderer replay tail may remain, and that tail is returned as a normal
/// `PendingNavigateCommand` participant. Keeping this seam synchronous lets a
/// Browser Host input expose its first real participant without an artificial
/// ready continuation.
pub(crate) fn start_navigation_failure_preserving_committed_document(
    conn: &mut CdpConnection,
    token: DocumentNavigationToken,
    state: NavigationDispatchState,
    error_text: String,
) -> NavigateCommandCompletion {
    let navigation = network::materialize_navigation_failure_preserving_committed_document(
        conn, &state, error_text,
    );
    if !conn.accepts_pending_document_navigation_for_session_owner(
        state.navigate_session_id.as_deref(),
        &token,
    ) {
        let mut output = CommandOutputBuffer::default();
        if state.navigate_id.is_some() {
            output.push_error_after_messages(-32000, "Navigation aborted");
        }
        return NavigateCommandCompletion::Complete(output.into_plan());
    }

    debug_assert!(!navigation.document_policy.invalidates_committed_document());
    emit_navigation_ready_trace(conn, &token, "navigation_request_failed", "request-failed");
    if !conn.fail_document_navigation_for_session_owner_if_matches(
        state.navigate_session_id.as_deref(),
        &token,
        BrowserNavigationFailure::Canceled {
            error_text: navigation.error_text.clone(),
        },
    ) {
        let mut output = CommandOutputBuffer::default();
        if state.navigate_id.is_some() {
            output.push_error_after_messages(-32000, "Navigation aborted");
        }
        return NavigateCommandCompletion::Complete(output.into_plan());
    }
    let _ = conn.clear_pending_navigation_history_update_for_session_owner(
        state.navigate_session_id.as_deref(),
    );
    let mut output = CommandOutputBuffer::default();
    activity::MainDocumentFailedNavigationActivity::new(
        state.clone(),
        navigation.progress_gate,
        navigation.response_mode,
    )
    .emit_navigation_error_into_buffer(&mut output, &navigation.error_text);
    finish_or_suspend_navigation_tail(conn, output, &token, &state, None, None)
}

async fn complete_materialized_navigate_command(
    conn: &mut CdpConnection,
    prefix_events: Vec<BackgroundProtocolEvent>,
    completion: MaterializedNavigationCompletion,
    command_context: &mut CommandDispatchContext,
) -> NavigateCommandCompletion {
    if !completion.is_current_for_connection(conn) {
        let mut output = CommandOutputBuffer::default();
        output.extend_background_events_after_messages(prefix_events);
        if completion.navigate_id().is_some() {
            output.push_error_after_messages(-32000, "Navigation aborted");
        }
        return NavigateCommandCompletion::Complete(output.into_plan());
    }
    let (token, state, navigation, engine) = completion.into_parts();
    match navigation {
        network::MaterializedNavigationLoadOutcome::ResponseCommitReady(navigation) => {
            emit_navigation_ready_trace(conn, &token, "response_commit_ready", "commit-ready");
            let configuration = conn.prepared_document_commit_configuration_for_session_owner(
                state.navigate_session_id.as_deref(),
                navigation.final_url(),
            );
            NavigateCommandCompletion::Pending(Box::new(PendingNavigateCommand {
                prefix_events,
                phase: PendingNavigateCommandPhase::ConfigurePreparedDocument {
                    token,
                    state: Box::new(state),
                    navigation,
                    engine: Box::new(engine),
                    configuration,
                },
            }))
        }
        network::MaterializedNavigationLoadOutcome::Loaded(navigation) => {
            let mut output = CommandOutputBuffer::default();
            output.extend_background_events_after_messages(prefix_events);
            start_or_complete_loaded_navigation(
                conn,
                output,
                token,
                state,
                *navigation,
                None,
                engine,
                command_context,
            )
            .await
        }
        navigation => {
            let mut output = CommandOutputBuffer::default();
            output.extend_background_events_after_messages(prefix_events);
            let tail_state = state.clone();
            let committed_owner = apply_materialized_navigation_into_buffer_async(
                conn,
                &mut output,
                &token,
                state,
                navigation,
                command_context,
            )
            .await;
            finish_or_suspend_navigation_tail(
                conn,
                output,
                &token,
                &tail_state,
                engine,
                committed_owner,
            )
        }
    }
}

async fn start_or_complete_loaded_navigation(
    conn: &mut CdpConnection,
    mut output: CommandOutputBuffer,
    token: DocumentNavigationToken,
    state: NavigationDispatchState,
    navigation: network::MaterializedLoadedDocumentProgress,
    committed_renderer_attachment: Option<CommittedRendererAgentAttachment>,
    engine: Option<NavigationEngine>,
    command_context: &mut CommandDispatchContext,
) -> NavigateCommandCompletion {
    let tail_state = state.clone();
    match start_loaded_navigation_commit(
        conn,
        &mut output,
        token.clone(),
        state,
        navigation,
        committed_renderer_attachment,
    ) {
        LoadedNavigationCommitStart::Pending(pending) => {
            NavigateCommandCompletion::Pending(Box::new(PendingNavigateCommand {
                prefix_events: Vec::new(),
                phase: PendingNavigateCommandPhase::RestoreLoadedPage {
                    output: Box::new(output),
                    token,
                    state: tail_state,
                    engine: Box::new(engine),
                    pending,
                },
            }))
        }
        LoadedNavigationCommitStart::Ready(completed) => {
            start_or_complete_loaded_navigation_page_install(
                conn,
                output,
                token,
                tail_state,
                engine,
                *completed,
                command_context,
            )
            .await
        }
        LoadedNavigationCommitStart::Rejected => {
            let _ = conn.fail_document_navigation_for_session_owner_if_matches(
                tail_state.navigate_session_id.as_deref(),
                &token,
                BrowserNavigationFailure::Commit {
                    error_text: "loaded navigation restore was rejected".to_owned(),
                },
            );
            finish_or_suspend_navigation_tail(conn, output, &token, &tail_state, engine, None)
        }
    }
}

async fn start_or_complete_loaded_navigation_page_install(
    conn: &mut CdpConnection,
    mut output: CommandOutputBuffer,
    token: DocumentNavigationToken,
    state: NavigationDispatchState,
    mut engine: Option<NavigationEngine>,
    completed: CompletedLoadedNavigationCommit,
    command_context: &mut CommandDispatchContext,
) -> NavigateCommandCompletion {
    let install =
        start_completed_loaded_navigation_commit(conn, &mut output, completed, command_context);
    let committed_owner = install.committed_owner().cloned();
    if committed_owner.is_some() {
        conn.adopt_materialized_navigation_engine(engine.take(), committed_owner.clone());
    }
    match install {
        LoadedNavigationCommitApplyStart::Pending(pending) => {
            NavigateCommandCompletion::Pending(Box::new(PendingNavigateCommand {
                prefix_events: Vec::new(),
                phase: PendingNavigateCommandPhase::DisposeReplacedPage {
                    output: Box::new(output),
                    token,
                    state,
                    engine: Box::new(engine),
                    pending,
                },
            }))
        }
        LoadedNavigationCommitApplyStart::Ready(completed) => {
            let step =
                start_loaded_navigation_commit_after_page_disposal(conn, &mut output, *completed);
            continue_loaded_navigation_post_disposal(conn, output, token, state, engine, step)
        }
        LoadedNavigationCommitApplyStart::Rejected => {
            let _ = conn.fail_document_navigation_for_session_owner_if_matches(
                state.navigate_session_id.as_deref(),
                &token,
                BrowserNavigationFailure::Commit {
                    error_text: "loaded navigation Page install was rejected".to_owned(),
                },
            );
            finish_or_suspend_navigation_tail(conn, output, &token, &state, engine, None)
        }
    }
}

/// Opaque participant completion routed through the existing background
/// navigation input channel.
///
/// Protocol/application code may route this value, but only the navigation
/// completion owner can inspect the Page and renderer capabilities inside it.
pub struct BackgroundNavigationParticipantCompletion {
    completed: CompletedNavigateCommand,
    command_context: CommandDispatchContext,
    command_id: Option<u64>,
    command_session_id: Option<String>,
    none_session_owner_route: Option<CdpSessionRoute>,
    requested_url: String,
    gate_key: Option<BackgroundNavigationGateKey>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundNavigationTurnDisposition {
    ParticipantPending,
    Terminal,
}

impl BackgroundNavigationTurnDisposition {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal)
    }
}

impl BackgroundNavigationParticipantCompletion {
    pub(crate) fn new(
        completed: CompletedNavigateCommand,
        command_context: CommandDispatchContext,
        command_id: Option<u64>,
        command_session_id: Option<String>,
        none_session_owner_route: Option<CdpSessionRoute>,
        requested_url: String,
        gate_key: Option<BackgroundNavigationGateKey>,
    ) -> Self {
        Self {
            completed,
            command_context,
            command_id,
            command_session_id,
            none_session_owner_route,
            requested_url,
            gate_key,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        CompletedNavigateCommand,
        CommandDispatchContext,
        Option<u64>,
        Option<String>,
        Option<CdpSessionRoute>,
        String,
        Option<BackgroundNavigationGateKey>,
    ) {
        (
            self.completed,
            self.command_context,
            self.command_id,
            self.command_session_id,
            self.none_session_owner_route,
            self.requested_url,
            self.gate_key,
        )
    }

    pub(crate) fn requested_url(&self) -> &str {
        &self.requested_url
    }

    pub(crate) fn gate_key(&self) -> Option<&BackgroundNavigationGateKey> {
        self.gate_key.as_ref()
    }
}
