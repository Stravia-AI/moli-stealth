use std::collections::VecDeque;

use moli_core::{
    RendererOutputFence,
    page::{CompletedPageCommand, PendingPageCommand},
};

use crate::conn::{
    BackgroundProtocolEvent, CdpConnection, CommandDispatchContext, PausedDocumentTransfer,
    PendingFetchAuthNavigation, PendingFetchNavigation, PendingSubresourceFetchAuthRequest,
    PendingSubresourceFetchRequest, PendingSubresourceFetchResponseRequest,
    TargetPageResidenceIdentity,
};
use crate::domains::{activity, command_output::CommandOutputPlan};

use super::navigation_completion::{
    CompletedNavigateCommand, NavigateCommandCompletion, PendingNavigateCommand,
    complete_pending_navigate_command, start_navigation_failure_preserving_committed_document,
};

/// All paused Fetch work captured from one exact Page before cancellation.
///
/// The public request ids are intentionally discarded here. Each pending
/// value already carries the exact navigation or Page capability needed to
/// settle it; request ids remain projection data inside those values.
pub(crate) struct PendingFetchStateCancellation {
    page_owner: Option<TargetPageResidenceIdentity>,
    projection_session_id: Option<String>,
    error_text: String,
    navigations: VecDeque<PendingFetchNavigation>,
    auth_navigations: VecDeque<PendingFetchAuthNavigation>,
    response_navigations: VecDeque<PausedDocumentTransfer>,
    subresource_fetches: VecDeque<PendingSubresourceFetchRequest>,
    subresource_auths: VecDeque<PendingSubresourceFetchAuthRequest>,
    subresource_responses: VecDeque<PendingSubresourceFetchResponseRequest>,
    events: Vec<BackgroundProtocolEvent>,
    renderer_output_predecessor: Option<RendererOutputFence>,
}

struct NavigationCancellationParticipant {
    command_id: Option<u64>,
    command_session_id: Option<String>,
    command_context: CommandDispatchContext,
}

enum PendingFetchCancellationParticipant {
    Navigation {
        pending: Box<PendingNavigateCommand>,
        projection: NavigationCancellationParticipant,
    },
    SubresourceFetch {
        page_owner: TargetPageResidenceIdentity,
        request: Box<PendingSubresourceFetchRequest>,
        pending: PendingPageCommand,
    },
    SubresourceAuth {
        page_owner: TargetPageResidenceIdentity,
        request: Box<PendingSubresourceFetchAuthRequest>,
        pending: PendingPageCommand,
    },
    SubresourceResponse {
        page_owner: TargetPageResidenceIdentity,
        request: Box<PendingSubresourceFetchResponseRequest>,
        pending: PendingPageCommand,
    },
}

enum CompletedFetchCancellationParticipant {
    Navigation {
        completed: Box<CompletedNavigateCommand>,
        projection: NavigationCancellationParticipant,
    },
    SubresourceFetch {
        page_owner: TargetPageResidenceIdentity,
        request: Box<PendingSubresourceFetchRequest>,
        completed: Result<CompletedPageCommand, String>,
    },
    SubresourceAuth {
        page_owner: TargetPageResidenceIdentity,
        request: Box<PendingSubresourceFetchAuthRequest>,
        completed: Result<CompletedPageCommand, String>,
    },
    SubresourceResponse {
        page_owner: TargetPageResidenceIdentity,
        request: Box<PendingSubresourceFetchResponseRequest>,
        completed: Result<CompletedPageCommand, String>,
    },
}

/// One move-owned renderer participant in a paused-Fetch cancellation chain.
///
/// Waiting consumes the exact Page/navigation capability. The Browser Host is
/// free to serve another owner input while this future is pending.
pub(crate) struct PendingFetchCancellationOwnerTask {
    state: PendingFetchStateCancellation,
    participant: PendingFetchCancellationParticipant,
}

/// Completion of exactly one paused-Fetch cancellation participant.
pub(crate) struct CompletedFetchCancellationOwnerTask {
    state: PendingFetchStateCancellation,
    participant: CompletedFetchCancellationParticipant,
}

pub(crate) enum FetchCancellationOwnerTaskStep {
    Pending(Box<PendingFetchCancellationOwnerTask>),
    Complete(FetchCancellationOwnerTaskOutput),
}

pub(crate) struct FetchCancellationOwnerTaskOutput {
    events: Vec<BackgroundProtocolEvent>,
    renderer_output_predecessor: Option<RendererOutputFence>,
}

impl FetchCancellationOwnerTaskOutput {
    pub(crate) fn into_parts(self) -> (Vec<BackgroundProtocolEvent>, Option<RendererOutputFence>) {
        (self.events, self.renderer_output_predecessor)
    }
}

impl PendingFetchCancellationOwnerTask {
    #[cfg(test)]
    pub(crate) fn page_owner_for_test(&self) -> Option<&TargetPageResidenceIdentity> {
        match &self.participant {
            PendingFetchCancellationParticipant::Navigation { .. } => {
                self.state.page_owner.as_ref()
            }
            PendingFetchCancellationParticipant::SubresourceFetch { page_owner, .. }
            | PendingFetchCancellationParticipant::SubresourceAuth { page_owner, .. }
            | PendingFetchCancellationParticipant::SubresourceResponse { page_owner, .. } => {
                Some(page_owner)
            }
        }
    }

    pub(crate) async fn wait(self: Box<Self>) -> CompletedFetchCancellationOwnerTask {
        let Self { state, participant } = *self;
        let participant = match participant {
            PendingFetchCancellationParticipant::Navigation {
                pending,
                projection,
            } => CompletedFetchCancellationParticipant::Navigation {
                completed: Box::new(pending.wait().await),
                projection,
            },
            PendingFetchCancellationParticipant::SubresourceFetch {
                page_owner,
                request,
                pending,
            } => CompletedFetchCancellationParticipant::SubresourceFetch {
                page_owner,
                request,
                completed: pending.wait().await.map_err(|error| error.to_string()),
            },
            PendingFetchCancellationParticipant::SubresourceAuth {
                page_owner,
                request,
                pending,
            } => CompletedFetchCancellationParticipant::SubresourceAuth {
                page_owner,
                request,
                completed: pending.wait().await.map_err(|error| error.to_string()),
            },
            PendingFetchCancellationParticipant::SubresourceResponse {
                page_owner,
                request,
                pending,
            } => CompletedFetchCancellationParticipant::SubresourceResponse {
                page_owner,
                request,
                completed: pending.wait().await.map_err(|error| error.to_string()),
            },
        };
        CompletedFetchCancellationOwnerTask { state, participant }
    }
}

impl PendingFetchStateCancellation {
    #[allow(clippy::too_many_arguments)]
    fn new(
        page_owner: Option<TargetPageResidenceIdentity>,
        projection_session_id: Option<String>,
        error_text: String,
        pending_navigations: Vec<PendingFetchNavigation>,
        pending_auth_navigations: Vec<PendingFetchAuthNavigation>,
        pending_response_navigations: Vec<PausedDocumentTransfer>,
        pending_subresource_fetches: Vec<(String, PendingSubresourceFetchRequest)>,
        pending_subresource_auths: Vec<(String, PendingSubresourceFetchAuthRequest)>,
        pending_subresource_responses: Vec<(String, PendingSubresourceFetchResponseRequest)>,
    ) -> Self {
        Self {
            page_owner,
            projection_session_id,
            error_text,
            navigations: pending_navigations.into(),
            auth_navigations: pending_auth_navigations.into(),
            response_navigations: pending_response_navigations.into(),
            subresource_fetches: pending_subresource_fetches
                .into_iter()
                .map(|(_, pending)| pending)
                .collect(),
            subresource_auths: pending_subresource_auths
                .into_iter()
                .map(|(_, pending)| pending)
                .collect(),
            subresource_responses: pending_subresource_responses
                .into_iter()
                .map(|(_, pending)| pending)
                .collect(),
            events: Vec::new(),
            renderer_output_predecessor: None,
        }
    }

    fn merge_renderer_output_predecessor(&mut self, predecessor: Option<RendererOutputFence>) {
        if let Some(predecessor) = predecessor {
            predecessor.merge_into_same_stream_tail(&mut self.renderer_output_predecessor);
        }
    }

    fn finish(self) -> FetchCancellationOwnerTaskOutput {
        FetchCancellationOwnerTaskOutput {
            events: self.events,
            renderer_output_predecessor: self.renderer_output_predecessor,
        }
    }

    fn push_navigation_abort(&mut self, command_id: Option<u64>, command_session_id: Option<&str>) {
        self.events.extend(
            CommandOutputPlan::error(-32000, "Navigation aborted")
                .into_background_events(command_id, command_session_id),
        );
    }

    fn settle_navigation_plan(
        &mut self,
        mut plan: CommandOutputPlan,
        mut projection: NavigationCancellationParticipant,
    ) {
        let predecessor = projection
            .command_context
            .take_renderer_output_predecessor()
            .or_else(|| plan.take_renderer_output_predecessor());
        self.merge_renderer_output_predecessor(predecessor);
        self.events.extend(plan.into_background_events(
            projection.command_id,
            projection.command_session_id.as_deref(),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn start_pending_fetch_state_cancellation(
    conn: &mut CdpConnection,
    page_owner: Option<TargetPageResidenceIdentity>,
    projection_session_id: Option<String>,
    error_text: String,
    pending_navigations: Vec<PendingFetchNavigation>,
    pending_auth_navigations: Vec<PendingFetchAuthNavigation>,
    pending_response_navigations: Vec<PausedDocumentTransfer>,
    pending_subresource_fetches: Vec<(String, PendingSubresourceFetchRequest)>,
    pending_subresource_auths: Vec<(String, PendingSubresourceFetchAuthRequest)>,
    pending_subresource_responses: Vec<(String, PendingSubresourceFetchResponseRequest)>,
) -> FetchCancellationOwnerTaskStep {
    drive_pending_fetch_state_cancellation(
        conn,
        PendingFetchStateCancellation::new(
            page_owner,
            projection_session_id,
            error_text,
            pending_navigations,
            pending_auth_navigations,
            pending_response_navigations,
            pending_subresource_fetches,
            pending_subresource_auths,
            pending_subresource_responses,
        ),
    )
}

pub(crate) fn complete_pending_fetch_state_cancellation<'a>(
    conn: &'a mut CdpConnection,
    completed: CompletedFetchCancellationOwnerTask,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = FetchCancellationOwnerTaskStep> + 'a>> {
    Box::pin(async move {
        let CompletedFetchCancellationOwnerTask {
            mut state,
            participant,
        } = completed;
        match participant {
            CompletedFetchCancellationParticipant::Navigation {
                completed,
                mut projection,
            } => {
                let Some(page_owner) = state.page_owner.as_ref() else {
                    state.push_navigation_abort(
                        projection.command_id,
                        projection.command_session_id.as_deref(),
                    );
                    return drive_pending_fetch_state_cancellation(conn, state);
                };
                let Some(owner_route) = conn.target_page_owner_route_if_current(page_owner) else {
                    state.push_navigation_abort(
                        projection.command_id,
                        projection.command_session_id.as_deref(),
                    );
                    return drive_pending_fetch_state_cancellation(conn, state);
                };
                let completion = {
                    let mut owner_scope =
                        conn.scoped_none_session_owner_route_override(owner_route);
                    complete_pending_navigate_command(
                        owner_scope.conn_mut(),
                        *completed,
                        &mut projection.command_context,
                    )
                    .await
                };
                match completion {
                    NavigateCommandCompletion::Pending(pending) => {
                        return FetchCancellationOwnerTaskStep::Pending(Box::new(
                            PendingFetchCancellationOwnerTask {
                                state,
                                participant: PendingFetchCancellationParticipant::Navigation {
                                    pending,
                                    projection,
                                },
                            },
                        ));
                    }
                    NavigateCommandCompletion::Complete(plan) => {
                        state.settle_navigation_plan(plan, projection);
                    }
                }
            }
            CompletedFetchCancellationParticipant::SubresourceFetch {
                page_owner,
                request,
                completed,
            } => {
                if let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) {
                    let mut owner_scope =
                        conn.scoped_none_session_owner_route_override(owner_route);
                    let conn = owner_scope.conn_mut();
                    if let Ok(completed) = completed
                        && let Ok(mut page) = conn.loaded_page_mut_for_protocol_access(None)
                        && let Ok(predecessor) =
                            page.finish_fail_pending_subresource_fetch(completed)
                    {
                        drop(page);
                        state.merge_renderer_output_predecessor(predecessor);
                        activity::flush_post_subresource_fetch_request_activity_background_events_async(
                            conn,
                            &mut state.events,
                            state.projection_session_id.as_deref(),
                            &request,
                        )
                        .await;
                    }
                }
            }
            CompletedFetchCancellationParticipant::SubresourceAuth {
                page_owner,
                request,
                completed,
            } => {
                if let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) {
                    let mut owner_scope =
                        conn.scoped_none_session_owner_route_override(owner_route);
                    let conn = owner_scope.conn_mut();
                    if let Ok(completed) = completed
                        && let Ok(mut page) = conn.loaded_page_mut_for_protocol_access(None)
                        && let Ok(predecessor) =
                            page.finish_fail_pending_subresource_auth(completed)
                    {
                        drop(page);
                        state.merge_renderer_output_predecessor(predecessor);
                        activity::flush_post_subresource_auth_activity_background_events_async(
                            conn,
                            &mut state.events,
                            state.projection_session_id.as_deref(),
                            &request,
                        )
                        .await;
                    }
                }
            }
            CompletedFetchCancellationParticipant::SubresourceResponse {
                page_owner,
                request,
                completed,
            } => {
                if let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) {
                    let mut owner_scope =
                        conn.scoped_none_session_owner_route_override(owner_route);
                    let conn = owner_scope.conn_mut();
                    if let Ok(completed) = completed
                        && let Ok(mut page) = conn.loaded_page_mut_for_protocol_access(None)
                        && let Ok(predecessor) =
                            page.finish_fail_pending_subresource_response(completed)
                    {
                        drop(page);
                        state.merge_renderer_output_predecessor(predecessor);
                        activity::flush_post_subresource_response_activity_background_events_async(
                            conn,
                            &mut state.events,
                            state.projection_session_id.as_deref(),
                            &request,
                        )
                        .await;
                    }
                }
            }
        }
        drive_pending_fetch_state_cancellation(conn, state)
    })
}

fn drive_pending_fetch_state_cancellation(
    conn: &mut CdpConnection,
    mut state: PendingFetchStateCancellation,
) -> FetchCancellationOwnerTaskStep {
    loop {
        let navigation = state
            .navigations
            .pop_front()
            .map(|pending| (pending.document_navigation_token, pending.navigation))
            .or_else(|| {
                state
                    .auth_navigations
                    .pop_front()
                    .map(|pending| (pending.document_navigation_token, pending.navigation))
            })
            .or_else(|| {
                state.response_navigations.pop_front().map(|pending| {
                    let (token, navigation, _) = pending.fail(state.error_text.clone());
                    (token, navigation)
                })
            });
        if let Some((token, mut navigation_state)) = navigation {
            // Execution resumes through the exact Page route captured by the
            // cancellation owner. The original frontend session remains only
            // the destination for a pending command response.
            let command_session_id = navigation_state.navigate_session_id.take();
            let command_id = navigation_state.navigate_id;
            let Some(token) = token else {
                state.push_navigation_abort(command_id, command_session_id.as_deref());
                continue;
            };
            let Some(page_owner) = state.page_owner.as_ref() else {
                state.push_navigation_abort(command_id, command_session_id.as_deref());
                continue;
            };
            let Some(owner_route) = conn.target_page_owner_route_if_current(page_owner) else {
                state.push_navigation_abort(command_id, command_session_id.as_deref());
                continue;
            };
            let projection = NavigationCancellationParticipant {
                command_id,
                command_session_id,
                command_context: CommandDispatchContext::default(),
            };
            let completion = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                let conn = owner_scope.conn_mut();
                start_navigation_failure_preserving_committed_document(
                    conn,
                    token,
                    navigation_state,
                    state.error_text.clone(),
                )
            };
            match completion {
                NavigateCommandCompletion::Pending(pending) => {
                    return FetchCancellationOwnerTaskStep::Pending(Box::new(
                        PendingFetchCancellationOwnerTask {
                            state,
                            participant: PendingFetchCancellationParticipant::Navigation {
                                pending,
                                projection,
                            },
                        },
                    ));
                }
                NavigateCommandCompletion::Complete(plan) => {
                    state.settle_navigation_plan(plan, projection);
                    continue;
                }
            }
        }

        if let Some(request) = state.subresource_fetches.pop_front() {
            let Some(page_owner) = request.installed_page_owner().cloned() else {
                continue;
            };
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                continue;
            };
            let pending = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                owner_scope
                    .conn_mut()
                    .loaded_page_mut_for_protocol_access(None)
                    .and_then(|page| {
                        page.start_fail_pending_subresource_fetch(
                            request.internal_id,
                            state.error_text.clone(),
                        )
                        .map_err(|error| error.to_string())
                    })
            };
            if let Ok(pending) = pending {
                return FetchCancellationOwnerTaskStep::Pending(Box::new(
                    PendingFetchCancellationOwnerTask {
                        state,
                        participant: PendingFetchCancellationParticipant::SubresourceFetch {
                            page_owner,
                            request: Box::new(request),
                            pending,
                        },
                    },
                ));
            }
            continue;
        }

        if let Some(request) = state.subresource_auths.pop_front() {
            let page_owner = request.page_owner.clone();
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                continue;
            };
            let pending = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                owner_scope
                    .conn_mut()
                    .loaded_page_mut_for_protocol_access(None)
                    .and_then(|page| {
                        page.start_fail_pending_subresource_auth(
                            request.internal_id,
                            state.error_text.clone(),
                        )
                        .map_err(|error| error.to_string())
                    })
            };
            if let Ok(pending) = pending {
                return FetchCancellationOwnerTaskStep::Pending(Box::new(
                    PendingFetchCancellationOwnerTask {
                        state,
                        participant: PendingFetchCancellationParticipant::SubresourceAuth {
                            page_owner,
                            request: Box::new(request),
                            pending,
                        },
                    },
                ));
            }
            continue;
        }

        if let Some(request) = state.subresource_responses.pop_front() {
            let page_owner = request.page_owner.clone();
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                continue;
            };
            let pending = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                owner_scope
                    .conn_mut()
                    .loaded_page_mut_for_protocol_access(None)
                    .and_then(|page| {
                        page.start_fail_pending_subresource_response(
                            request.internal_id,
                            state.error_text.clone(),
                        )
                        .map_err(|error| error.to_string())
                    })
            };
            if let Ok(pending) = pending {
                return FetchCancellationOwnerTaskStep::Pending(Box::new(
                    PendingFetchCancellationOwnerTask {
                        state,
                        participant: PendingFetchCancellationParticipant::SubresourceResponse {
                            page_owner,
                            request: Box::new(request),
                            pending,
                        },
                    },
                ));
            }
            continue;
        }

        return FetchCancellationOwnerTaskStep::Complete(state.finish());
    }
}

/// Compatibility drain for owners that have not yet exposed this participant
/// chain through Browser Host. It shares the exact state machine, so migrating
/// context disposal later does not duplicate cancellation semantics.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn drain_pending_fetch_state_cancellation_async(
    conn: &mut CdpConnection,
    out: &mut Vec<BackgroundProtocolEvent>,
    session_id: Option<&str>,
    error_text: &str,
    pending_navigations: Vec<PendingFetchNavigation>,
    pending_auth_navigations: Vec<PendingFetchAuthNavigation>,
    pending_response_navigations: Vec<PausedDocumentTransfer>,
    pending_subresource_fetches: Vec<(String, PendingSubresourceFetchRequest)>,
    pending_subresource_auths: Vec<(String, PendingSubresourceFetchAuthRequest)>,
    pending_subresource_responses: Vec<(String, PendingSubresourceFetchResponseRequest)>,
) -> Option<RendererOutputFence> {
    let page_owner = conn.target_page_residence_identity_for_session(session_id);
    let mut step = start_pending_fetch_state_cancellation(
        conn,
        page_owner,
        session_id.map(str::to_owned),
        error_text.to_owned(),
        pending_navigations,
        pending_auth_navigations,
        pending_response_navigations,
        pending_subresource_fetches,
        pending_subresource_auths,
        pending_subresource_responses,
    );
    loop {
        match step {
            FetchCancellationOwnerTaskStep::Pending(pending) => {
                step = complete_pending_fetch_state_cancellation(conn, pending.wait().await).await;
            }
            FetchCancellationOwnerTaskStep::Complete(completed) => {
                let (events, predecessor) = completed.into_parts();
                out.extend(events);
                return predecessor;
            }
        }
    }
}
