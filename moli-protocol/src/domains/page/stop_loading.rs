use moli_core::{
    RendererOutputFence,
    page::{CompletedPageCommand, PendingPageCommand},
};

use crate::conn::{CdpConnection, Cmd, CommandDispatchContext, TargetPageResidenceIdentity};
use crate::domains::command_output::CommandOutputPlan;

use super::{PageCommandTaskStep, fetch_cancellation, termination};

/// One actor-selected stop-loading action.
///
/// Browser Host first resolves the captured stable Page slot to the Document
/// current at selection time, then exposes the renderer stop as a move-owned
/// participant. No frontend/session route is consulted after publication.
pub(crate) enum StopLoadingOwnerTaskStep {
    Complete(CommandOutputPlan),
    Pending(PendingStopLoadingOwnerTask),
}

pub(crate) struct PendingStopLoadingOwnerTask {
    phase: PendingStopLoadingOwnerTaskPhase,
}

pub(crate) struct CompletedStopLoadingOwnerTask {
    phase: CompletedStopLoadingOwnerTaskPhase,
}

enum PendingStopLoadingOwnerTaskPhase {
    RendererStop {
        page_owner: TargetPageResidenceIdentity,
        renderer_stop: Result<Option<PendingPageCommand>, String>,
    },
    FetchCancellation {
        pending: Box<fetch_cancellation::PendingFetchCancellationOwnerTask>,
        renderer_stop_predecessor: Option<RendererOutputFence>,
    },
}

enum CompletedStopLoadingOwnerTaskPhase {
    RendererStop {
        page_owner: TargetPageResidenceIdentity,
        renderer_stop: Box<Result<Option<CompletedPageCommand>, String>>,
    },
    FetchCancellation {
        completed: Box<fetch_cancellation::CompletedFetchCancellationOwnerTask>,
        renderer_stop_predecessor: Option<RendererOutputFence>,
    },
}

impl PendingStopLoadingOwnerTask {
    #[cfg(test)]
    pub(crate) fn page_owner_for_test(&self) -> Option<&TargetPageResidenceIdentity> {
        match &self.phase {
            PendingStopLoadingOwnerTaskPhase::RendererStop { page_owner, .. } => Some(page_owner),
            PendingStopLoadingOwnerTaskPhase::FetchCancellation { pending, .. } => {
                pending.page_owner_for_test()
            }
        }
    }

    pub(crate) async fn wait(self) -> CompletedStopLoadingOwnerTask {
        let phase = match self.phase {
            PendingStopLoadingOwnerTaskPhase::RendererStop {
                page_owner,
                renderer_stop,
            } => {
                let renderer_stop = match renderer_stop {
                    Ok(Some(pending)) => pending
                        .wait()
                        .await
                        .map(Some)
                        .map_err(|error| error.to_string()),
                    Ok(None) => Ok(None),
                    Err(error) => Err(error),
                };
                CompletedStopLoadingOwnerTaskPhase::RendererStop {
                    page_owner,
                    renderer_stop: Box::new(renderer_stop),
                }
            }
            PendingStopLoadingOwnerTaskPhase::FetchCancellation {
                pending,
                renderer_stop_predecessor,
            } => CompletedStopLoadingOwnerTaskPhase::FetchCancellation {
                completed: Box::new(pending.wait().await),
                renderer_stop_predecessor,
            },
        };
        CompletedStopLoadingOwnerTask { phase }
    }
}

pub(super) fn try_start_stop_loading_command_dispatch(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
    command_context: &CommandDispatchContext,
) -> PageCommandTaskStep {
    if cmd.session_id.is_some()
        && conn
            .target_owner_identity_for_session(cmd.session_id)
            .is_none()
    {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error_without_session(
            -32001,
            "Unknown sessionId",
        ));
    }
    let Some(page_owner) = conn.target_page_residence_identity_for_session(cmd.session_id) else {
        // Preserve Moli's existing idempotent no-op when no browsing
        // context is currently installed.
        return PageCommandTaskStep::Complete(CommandOutputPlan::success());
    };
    match conn.publish_browser_owner_stop_loading_command(
        page_owner,
        command_context.detached_participant_context(),
    ) {
        Ok(pending) => PageCommandTaskStep::Pending(super::PendingPageCommandDispatch {
            command_id: cmd.id,
            owner_scope: crate::conn::CommandOwnerScope::capture(conn, cmd.session_id),
            kind: Box::new(
                super::PendingPageCommandKind::BrowserOwnerStopLoadingCompletion(pending),
            ),
        }),
        Err(error) => PageCommandTaskStep::Complete(CommandOutputPlan::error_without_session(
            -32000,
            format!("BrowserHostStopLoadingAdmissionFailed: {error}"),
        )),
    }
}

pub(crate) fn start_page_owned_stop_loading_owner_task(
    conn: &mut CdpConnection,
    accepted_page_owner: &TargetPageResidenceIdentity,
) -> StopLoadingOwnerTaskStep {
    let Some(owner_route) = conn.target_page_owner_route_if_same_slot(accepted_page_owner) else {
        // The exact Target/Page slot disappeared before selection. Stopping a
        // retired browsing context is already satisfied and must not be
        // redirected to a recreated Target with the same public id.
        return StopLoadingOwnerTaskStep::Complete(CommandOutputPlan::success());
    };
    let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
    let conn = owner_scope.conn_mut();
    let Some(page_owner) = conn.target_page_residence_identity_for_session(None) else {
        return StopLoadingOwnerTaskStep::Complete(CommandOutputPlan::success());
    };
    let renderer_stop = match conn.runtime_session_owner_slot_mut(None) {
        Ok(slot) => slot
            .loaded_page()
            .map(|page| {
                page.start_stop_document_lifecycle()
                    .map_err(|error| error.to_string())
            })
            .transpose(),
        Err(error) => Err(error),
    };
    StopLoadingOwnerTaskStep::Pending(PendingStopLoadingOwnerTask {
        phase: PendingStopLoadingOwnerTaskPhase::RendererStop {
            page_owner,
            renderer_stop,
        },
    })
}

pub(crate) async fn complete_page_owned_stop_loading_owner_task(
    conn: &mut CdpConnection,
    completed: CompletedStopLoadingOwnerTask,
) -> StopLoadingOwnerTaskStep {
    match completed.phase {
        CompletedStopLoadingOwnerTaskPhase::RendererStop {
            page_owner,
            renderer_stop,
        } => {
            let Some(owner_route) = conn.target_page_owner_route_if_current(&page_owner) else {
                // A replacement committed while the old renderer stop was pending.
                // Its completion belongs to the retired Document and cannot cancel
                // fetch state or lifecycle work in the successor Page.
                return StopLoadingOwnerTaskStep::Complete(CommandOutputPlan::success());
            };
            let (renderer_stop_predecessor, pending_fetch_state) = {
                let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
                let conn = owner_scope.conn_mut();
                let mut renderer_stop_predecessor = None;
                match *renderer_stop {
                    Ok(Some(completed)) => {
                        let result = conn
                            .runtime_session_owner_slot_mut(None)
                            .ok()
                            .and_then(|slot| slot.loaded_page_mut())
                            .ok_or_else(|| "NoDocumentLoaded".to_owned())
                            .and_then(|mut page| {
                                page.finish_stop_document_lifecycle(completed)
                                    .map_err(|error| error.to_string())
                            });
                        match result {
                            Ok(Some(predecessor)) => predecessor
                                .merge_into_same_stream_tail(&mut renderer_stop_predecessor),
                            Ok(None) => {}
                            Err(error) => {
                                tracing::debug!(%error, "failed to stop renderer document lifecycle");
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::debug!(%error, "failed to start or await renderer document lifecycle stop");
                    }
                }
                (
                    renderer_stop_predecessor,
                    termination::take_pending_fetch_state(conn, None),
                )
            };
            let (
                pending_navigations,
                pending_auth_navigations,
                pending_response_navigations,
                pending_subresource_fetches,
                pending_subresource_auths,
                pending_subresource_responses,
            ) = pending_fetch_state;
            let step = fetch_cancellation::start_pending_fetch_state_cancellation(
                conn,
                Some(page_owner),
                None,
                "Navigation stopped".to_owned(),
                pending_navigations,
                pending_auth_navigations,
                pending_response_navigations,
                pending_subresource_fetches,
                pending_subresource_auths,
                pending_subresource_responses,
            );
            stop_loading_step_from_fetch_cancellation(step, renderer_stop_predecessor)
        }
        CompletedStopLoadingOwnerTaskPhase::FetchCancellation {
            completed,
            renderer_stop_predecessor,
        } => {
            let step =
                fetch_cancellation::complete_pending_fetch_state_cancellation(conn, *completed)
                    .await;
            stop_loading_step_from_fetch_cancellation(step, renderer_stop_predecessor)
        }
    }
}

fn stop_loading_step_from_fetch_cancellation(
    step: fetch_cancellation::FetchCancellationOwnerTaskStep,
    renderer_stop_predecessor: Option<RendererOutputFence>,
) -> StopLoadingOwnerTaskStep {
    match step {
        fetch_cancellation::FetchCancellationOwnerTaskStep::Pending(pending) => {
            StopLoadingOwnerTaskStep::Pending(PendingStopLoadingOwnerTask {
                phase: PendingStopLoadingOwnerTaskPhase::FetchCancellation {
                    pending,
                    renderer_stop_predecessor,
                },
            })
        }
        fetch_cancellation::FetchCancellationOwnerTaskStep::Complete(completed) => {
            let (events, cancellation_predecessor) = completed.into_parts();
            let mut renderer_output_predecessor = renderer_stop_predecessor;
            if let Some(predecessor) = cancellation_predecessor {
                predecessor.merge_into_same_stream_tail(&mut renderer_output_predecessor);
            }
            let mut plan = CommandOutputPlan::success();
            for event in events {
                plan.push_background_event(event);
            }
            if let Some(predecessor) = renderer_output_predecessor {
                plan.set_renderer_output_predecessor(predecessor);
            }
            StopLoadingOwnerTaskStep::Complete(plan)
        }
    }
}
