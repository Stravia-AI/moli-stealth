use moli_core::browser_host::{
    BrowserPausedNavigationAuthDecision, BrowserPausedNavigationDecision,
    BrowserPausedNavigationFulfillDecision, BrowserPausedNavigationResponseDecision,
    PageResidenceIdentity,
};

use crate::conn::{
    BrowserOwnerPausedNavigationSidecar, CdpConnection, CommandDispatchContext,
    DocumentNavigationToken, NavigationDispatchState, PausedDocumentTransfer,
    PendingFetchAuthNavigation, PendingFetchNavigation,
};
use crate::domains::{command_output::CommandOutputPlan, network, page};

use super::navigation_resume::{
    CompletedPausedNavigationResumeOwnerTask, PausedNavigationFulfillSource,
    PausedNavigationResumeOwnerTaskStep, PendingPausedNavigationResumeOwnerTask,
    complete_paused_navigation_resume_owner_task, start_paused_navigation_auth_resume_owner_task,
    start_paused_navigation_fulfill_owner_task, start_paused_navigation_response_resume_owner_task,
    start_paused_navigation_resume_owner_task,
};

struct PausedNavigationDecisionProjection {
    navigate_id: Option<u64>,
    navigate_session_id: Option<String>,
    requested_url: String,
    command_context: CommandDispatchContext,
}

enum PendingPausedNavigationDecisionPhase {
    Apply {
        page_owner: PageResidenceIdentity,
        token: DocumentNavigationToken,
        navigation: Box<NavigationDispatchState>,
        error_text: String,
        projection: PausedNavigationDecisionProjection,
    },
    ContinueApply {
        page_owner: PageResidenceIdentity,
        pending: Box<PendingFetchNavigation>,
        decision: moli_core::browser_host::BrowserPausedNavigationContinueDecision,
        projection: PausedNavigationDecisionProjection,
    },
    ResponseContinueApply {
        page_owner: PageResidenceIdentity,
        transfer: Box<PausedDocumentTransfer>,
        decision: BrowserPausedNavigationResponseDecision,
        projection: PausedNavigationDecisionProjection,
    },
    FulfillApply {
        page_owner: PageResidenceIdentity,
        source: PausedNavigationFulfillSource,
        decision: BrowserPausedNavigationFulfillDecision,
        projection: PausedNavigationDecisionProjection,
    },
    AuthApply {
        page_owner: PageResidenceIdentity,
        pending: Box<PendingFetchAuthNavigation>,
        decision: BrowserPausedNavigationAuthDecision,
        projection: PausedNavigationDecisionProjection,
    },
    Resume {
        page_owner: PageResidenceIdentity,
        pending: Box<PendingPausedNavigationResumeOwnerTask>,
        projection: PausedNavigationDecisionProjection,
    },
    Navigation {
        page_owner: PageResidenceIdentity,
        pending: Box<page::PendingNavigateCommand>,
        projection: PausedNavigationDecisionProjection,
    },
}

enum CompletedPausedNavigationDecisionPhase {
    Apply {
        page_owner: PageResidenceIdentity,
        token: DocumentNavigationToken,
        navigation: Box<NavigationDispatchState>,
        error_text: String,
        projection: PausedNavigationDecisionProjection,
    },
    ContinueApply {
        page_owner: PageResidenceIdentity,
        pending: Box<PendingFetchNavigation>,
        decision: moli_core::browser_host::BrowserPausedNavigationContinueDecision,
        projection: PausedNavigationDecisionProjection,
    },
    ResponseContinueApply {
        page_owner: PageResidenceIdentity,
        transfer: Box<PausedDocumentTransfer>,
        decision: BrowserPausedNavigationResponseDecision,
        projection: PausedNavigationDecisionProjection,
    },
    FulfillApply {
        page_owner: PageResidenceIdentity,
        source: PausedNavigationFulfillSource,
        decision: BrowserPausedNavigationFulfillDecision,
        projection: PausedNavigationDecisionProjection,
    },
    AuthApply {
        page_owner: PageResidenceIdentity,
        pending: Box<PendingFetchAuthNavigation>,
        decision: BrowserPausedNavigationAuthDecision,
        projection: PausedNavigationDecisionProjection,
    },
    Resume {
        page_owner: PageResidenceIdentity,
        completed: Box<CompletedPausedNavigationResumeOwnerTask>,
        projection: PausedNavigationDecisionProjection,
    },
    Navigation {
        page_owner: PageResidenceIdentity,
        completed: Box<page::CompletedNavigateCommand>,
        projection: PausedNavigationDecisionProjection,
    },
}

/// One move-owned participant in an actor-selected paused-navigation decision.
///
/// The initial participant is an explicit handoff from the synchronous Host
/// executor to its async completion lane. Later participants are concrete
/// renderer waits owned by the shared navigation state machine.
pub(crate) struct PendingPausedNavigationDecisionOwnerTask {
    phase: PendingPausedNavigationDecisionPhase,
}

pub(crate) struct CompletedPausedNavigationDecisionOwnerTask {
    phase: CompletedPausedNavigationDecisionPhase,
}

pub(crate) enum PausedNavigationDecisionOwnerTaskStep {
    Pending(Box<PendingPausedNavigationDecisionOwnerTask>),
    Complete(PausedNavigationDecisionOwnerTaskOutput),
}

pub(crate) struct PausedNavigationDecisionOwnerTaskOutput {
    plan: CommandOutputPlan,
    command_context: CommandDispatchContext,
}

impl PausedNavigationDecisionOwnerTaskOutput {
    pub(crate) fn into_parts(self) -> (CommandOutputPlan, CommandDispatchContext) {
        (self.plan, self.command_context)
    }
}

impl PendingPausedNavigationDecisionOwnerTask {
    #[cfg(test)]
    pub(crate) fn page_owner_for_test(&self) -> &PageResidenceIdentity {
        match &self.phase {
            PendingPausedNavigationDecisionPhase::Apply { page_owner, .. }
            | PendingPausedNavigationDecisionPhase::ContinueApply { page_owner, .. }
            | PendingPausedNavigationDecisionPhase::ResponseContinueApply { page_owner, .. }
            | PendingPausedNavigationDecisionPhase::FulfillApply { page_owner, .. }
            | PendingPausedNavigationDecisionPhase::AuthApply { page_owner, .. }
            | PendingPausedNavigationDecisionPhase::Resume { page_owner, .. }
            | PendingPausedNavigationDecisionPhase::Navigation { page_owner, .. } => page_owner,
        }
    }

    pub(crate) async fn wait(self: Box<Self>) -> CompletedPausedNavigationDecisionOwnerTask {
        let phase = match self.phase {
            PendingPausedNavigationDecisionPhase::Apply {
                page_owner,
                token,
                navigation,
                error_text,
                projection,
            } => CompletedPausedNavigationDecisionPhase::Apply {
                page_owner,
                token,
                navigation,
                error_text,
                projection,
            },
            PendingPausedNavigationDecisionPhase::ContinueApply {
                page_owner,
                pending,
                decision,
                projection,
            } => CompletedPausedNavigationDecisionPhase::ContinueApply {
                page_owner,
                pending,
                decision,
                projection,
            },
            PendingPausedNavigationDecisionPhase::ResponseContinueApply {
                page_owner,
                transfer,
                decision,
                projection,
            } => CompletedPausedNavigationDecisionPhase::ResponseContinueApply {
                page_owner,
                transfer,
                decision,
                projection,
            },
            PendingPausedNavigationDecisionPhase::FulfillApply {
                page_owner,
                source,
                decision,
                projection,
            } => CompletedPausedNavigationDecisionPhase::FulfillApply {
                page_owner,
                source,
                decision,
                projection,
            },
            PendingPausedNavigationDecisionPhase::AuthApply {
                page_owner,
                pending,
                decision,
                projection,
            } => CompletedPausedNavigationDecisionPhase::AuthApply {
                page_owner,
                pending,
                decision,
                projection,
            },
            PendingPausedNavigationDecisionPhase::Resume {
                page_owner,
                pending,
                projection,
            } => CompletedPausedNavigationDecisionPhase::Resume {
                page_owner,
                completed: Box::new(pending.wait().await),
                projection,
            },
            PendingPausedNavigationDecisionPhase::Navigation {
                page_owner,
                pending,
                projection,
            } => CompletedPausedNavigationDecisionPhase::Navigation {
                page_owner,
                completed: Box::new(pending.wait().await),
                projection,
            },
        };
        CompletedPausedNavigationDecisionOwnerTask { phase }
    }
}

pub(crate) fn start_page_owned_paused_navigation_decision_owner_task(
    conn: &CdpConnection,
    page_owner: PageResidenceIdentity,
    pending: BrowserOwnerPausedNavigationSidecar,
    decision: BrowserPausedNavigationDecision,
    command_context: CommandDispatchContext,
) -> PausedNavigationDecisionOwnerTaskStep {
    let (navigate_id, navigate_session_id, requested_url) = match &pending {
        BrowserOwnerPausedNavigationSidecar::Request(pending) => (
            pending.navigation.navigate_id,
            pending.navigation.navigate_session_id.clone(),
            pending.navigation.requested_url.as_str().to_owned(),
        ),
        BrowserOwnerPausedNavigationSidecar::Response(transfer) => (
            transfer.navigation().navigate_id,
            transfer.navigation().navigate_session_id.clone(),
            transfer.navigation().requested_url.as_str().to_owned(),
        ),
        BrowserOwnerPausedNavigationSidecar::Auth(pending) => (
            pending.navigation.navigate_id,
            pending.navigation.navigate_session_id.clone(),
            pending.navigation.requested_url.as_str().to_owned(),
        ),
    };
    let projection = PausedNavigationDecisionProjection {
        navigate_id,
        navigate_session_id,
        requested_url,
        command_context,
    };
    if conn
        .target_page_owner_route_if_current(&page_owner)
        .is_none()
    {
        return finish_with_navigation_abort(projection);
    }
    let phase = match (pending, decision) {
        (
            BrowserOwnerPausedNavigationSidecar::Request(mut pending),
            BrowserPausedNavigationDecision::Fail { error_text },
        ) => {
            let Some(token) = pending.document_navigation_token else {
                return finish_with_navigation_abort(projection);
            };
            pending.navigation.navigate_session_id = None;
            PendingPausedNavigationDecisionPhase::Apply {
                page_owner,
                token,
                navigation: Box::new(pending.navigation),
                error_text,
                projection,
            }
        }
        (
            BrowserOwnerPausedNavigationSidecar::Request(pending),
            BrowserPausedNavigationDecision::Continue(decision),
        ) => PendingPausedNavigationDecisionPhase::ContinueApply {
            page_owner,
            pending: Box::new(pending),
            decision,
            projection,
        },
        (
            BrowserOwnerPausedNavigationSidecar::Response(transfer),
            BrowserPausedNavigationDecision::Fail { error_text },
        ) => {
            let (token, navigation, _) = (*transfer).fail(error_text.clone());
            let Some(token) = token else {
                return finish_with_navigation_abort(projection);
            };
            PendingPausedNavigationDecisionPhase::Apply {
                page_owner,
                token,
                navigation: Box::new(navigation),
                error_text,
                projection,
            }
        }
        (
            BrowserOwnerPausedNavigationSidecar::Response(transfer),
            BrowserPausedNavigationDecision::ContinueResponse(decision),
        ) => PendingPausedNavigationDecisionPhase::ResponseContinueApply {
            page_owner,
            transfer,
            decision,
            projection,
        },
        (
            BrowserOwnerPausedNavigationSidecar::Request(pending),
            BrowserPausedNavigationDecision::Fulfill(decision),
        ) => PendingPausedNavigationDecisionPhase::FulfillApply {
            page_owner,
            source: PausedNavigationFulfillSource::Request(Box::new(pending)),
            decision,
            projection,
        },
        (
            BrowserOwnerPausedNavigationSidecar::Response(transfer),
            BrowserPausedNavigationDecision::Fulfill(decision),
        ) => PendingPausedNavigationDecisionPhase::FulfillApply {
            page_owner,
            source: PausedNavigationFulfillSource::Response(transfer),
            decision,
            projection,
        },
        (
            BrowserOwnerPausedNavigationSidecar::Auth(pending),
            BrowserPausedNavigationDecision::Auth(decision),
        ) => PendingPausedNavigationDecisionPhase::AuthApply {
            page_owner,
            pending: Box::new(pending),
            decision,
            projection,
        },
        _ => {
            tracing::error!(
                "Browser Host paused-navigation decision did not match its prepared sidecar"
            );
            return finish_with_navigation_abort(projection);
        }
    };
    PausedNavigationDecisionOwnerTaskStep::Pending(Box::new(
        PendingPausedNavigationDecisionOwnerTask { phase },
    ))
}

pub(crate) async fn complete_page_owned_paused_navigation_decision_owner_task(
    conn: &mut CdpConnection,
    completed: CompletedPausedNavigationDecisionOwnerTask,
) -> PausedNavigationDecisionOwnerTaskStep {
    match completed.phase {
        CompletedPausedNavigationDecisionPhase::Apply {
            page_owner,
            token,
            navigation,
            error_text,
            mut projection,
        } => {
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                return finish_with_navigation_abort(projection);
            };
            let completion = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                let conn = owner_scope.conn_mut();
                let navigation = *navigation;
                let materialized =
                    network::materialize_navigation_load_result(conn, &navigation, Err(error_text));
                page::complete_pending_navigate_command(
                    conn,
                    page::CompletedNavigateCommand::materialized(
                        page::MaterializedNavigationCompletion::new(
                            token,
                            navigation,
                            materialized,
                        ),
                    ),
                    &mut projection.command_context,
                )
                .await
            };
            continue_navigation_completion(conn, page_owner, completion, projection)
        }
        CompletedPausedNavigationDecisionPhase::ContinueApply {
            page_owner,
            pending,
            decision,
            projection,
        } => {
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                return finish_with_navigation_abort(projection);
            };
            let step = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                start_paused_navigation_resume_owner_task(
                    owner_scope.conn_mut(),
                    *pending,
                    decision,
                )
            };
            continue_resume_step(conn, page_owner, step, projection).await
        }
        CompletedPausedNavigationDecisionPhase::ResponseContinueApply {
            page_owner,
            transfer,
            decision,
            projection,
        } => {
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                return finish_with_navigation_abort(projection);
            };
            let step = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                start_paused_navigation_response_resume_owner_task(
                    owner_scope.conn_mut(),
                    *transfer,
                    decision,
                )
            };
            continue_resume_step(conn, page_owner, step, projection).await
        }
        CompletedPausedNavigationDecisionPhase::FulfillApply {
            page_owner,
            source,
            decision,
            projection,
        } => {
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                return finish_with_navigation_abort(projection);
            };
            let step = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                start_paused_navigation_fulfill_owner_task(owner_scope.conn_mut(), source, decision)
            };
            continue_resume_step(conn, page_owner, step, projection).await
        }
        CompletedPausedNavigationDecisionPhase::AuthApply {
            page_owner,
            pending,
            decision,
            projection,
        } => {
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                return finish_with_navigation_abort(projection);
            };
            let step = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                start_paused_navigation_auth_resume_owner_task(
                    owner_scope.conn_mut(),
                    *pending,
                    decision,
                )
            };
            continue_resume_step(conn, page_owner, step, projection).await
        }
        CompletedPausedNavigationDecisionPhase::Resume {
            page_owner,
            completed,
            projection,
        } => {
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                return finish_with_navigation_abort(projection);
            };
            let step = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                complete_paused_navigation_resume_owner_task(owner_scope.conn_mut(), *completed)
            };
            continue_resume_step(conn, page_owner, step, projection).await
        }
        CompletedPausedNavigationDecisionPhase::Navigation {
            page_owner,
            completed,
            mut projection,
        } => {
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                return finish_with_navigation_abort(projection);
            };
            let completion = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                page::complete_pending_navigate_command(
                    owner_scope.conn_mut(),
                    *completed,
                    &mut projection.command_context,
                )
                .await
            };
            continue_navigation_completion(conn, page_owner, completion, projection)
        }
    }
}

async fn continue_resume_step(
    conn: &mut CdpConnection,
    page_owner: PageResidenceIdentity,
    step: PausedNavigationResumeOwnerTaskStep,
    mut projection: PausedNavigationDecisionProjection,
) -> PausedNavigationDecisionOwnerTaskStep {
    match step {
        PausedNavigationResumeOwnerTaskStep::Pending(pending) => {
            PausedNavigationDecisionOwnerTaskStep::Pending(Box::new(
                PendingPausedNavigationDecisionOwnerTask {
                    phase: PendingPausedNavigationDecisionPhase::Resume {
                        page_owner,
                        pending,
                        projection,
                    },
                },
            ))
        }
        PausedNavigationResumeOwnerTaskStep::NavigatePending(pending) => {
            PausedNavigationDecisionOwnerTaskStep::Pending(Box::new(
                PendingPausedNavigationDecisionOwnerTask {
                    phase: PendingPausedNavigationDecisionPhase::Navigation {
                        page_owner,
                        pending,
                        projection,
                    },
                },
            ))
        }
        PausedNavigationResumeOwnerTaskStep::NavigateReady(completed) => {
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                return finish_with_navigation_abort(projection);
            };
            let completion = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                page::complete_pending_navigate_command(
                    owner_scope.conn_mut(),
                    *completed,
                    &mut projection.command_context,
                )
                .await
            };
            continue_navigation_completion(conn, page_owner, completion, projection)
        }
        PausedNavigationResumeOwnerTaskStep::CommandRejected(plan) => {
            PausedNavigationDecisionOwnerTaskStep::Complete(
                PausedNavigationDecisionOwnerTaskOutput {
                    plan,
                    command_context: projection.command_context,
                },
            )
        }
        PausedNavigationResumeOwnerTaskStep::Complete(plan) => {
            finish_with_navigation_plan(plan, projection)
        }
    }
}

fn continue_navigation_completion(
    conn: &mut CdpConnection,
    previous_page_owner: PageResidenceIdentity,
    completion: page::NavigateCommandCompletion,
    projection: PausedNavigationDecisionProjection,
) -> PausedNavigationDecisionOwnerTaskStep {
    match completion {
        page::NavigateCommandCompletion::Pending(pending) => {
            let Some(page_owner) = current_page_for_same_slot(conn, &previous_page_owner) else {
                return finish_with_navigation_abort(projection);
            };
            PausedNavigationDecisionOwnerTaskStep::Pending(Box::new(
                PendingPausedNavigationDecisionOwnerTask {
                    phase: PendingPausedNavigationDecisionPhase::Navigation {
                        page_owner,
                        pending,
                        projection,
                    },
                },
            ))
        }
        page::NavigateCommandCompletion::Complete(plan) => {
            finish_with_navigation_plan(plan, projection)
        }
    }
}

fn current_page_for_same_slot(
    conn: &mut CdpConnection,
    previous: &PageResidenceIdentity,
) -> Option<PageResidenceIdentity> {
    let route = conn.target_page_owner_route_if_same_slot(previous)?;
    let mut owner_scope = conn.scoped_none_session_owner_route_override(route);
    owner_scope
        .conn_mut()
        .target_page_residence_identity_for_session(None)
}

fn finish_with_navigation_abort(
    projection: PausedNavigationDecisionProjection,
) -> PausedNavigationDecisionOwnerTaskStep {
    finish_with_navigation_plan(
        CommandOutputPlan::error(-32000, "Navigation aborted"),
        projection,
    )
}

fn finish_with_navigation_plan(
    navigation_plan: CommandOutputPlan,
    projection: PausedNavigationDecisionProjection,
) -> PausedNavigationDecisionOwnerTaskStep {
    let PausedNavigationDecisionProjection {
        navigate_id,
        navigate_session_id,
        requested_url,
        command_context,
    } = projection;
    let mut plan = CommandOutputPlan::success();
    let navigation_plan = if navigate_id.is_some() {
        navigation_plan.into_browser_navigate_background_event_plan(
            requested_url.as_str(),
            navigate_id,
            navigate_session_id.as_deref(),
        )
    } else {
        navigation_plan.into_background_event_plan(None, navigate_session_id.as_deref())
    };
    plan.extend(navigation_plan);
    PausedNavigationDecisionOwnerTaskStep::Complete(PausedNavigationDecisionOwnerTaskOutput {
        plan,
        command_context,
    })
}
