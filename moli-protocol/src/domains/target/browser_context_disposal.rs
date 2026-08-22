use std::collections::{HashSet, VecDeque};

use moli_core::{
    browser_host::{
        BrowserContextDisposalReservation, BrowserContextHandle, BrowserPageOwnerKey,
        BrowserTargetTerminationRequest, PageResidenceIdentity,
    },
    page::{Page, RendererPageLifetimeOwner},
    runtime::RendererBrowserContextRuntimeOwner,
};

use crate::conn::{
    BackgroundProtocolEvent, BrowserTargetCloseStart, BrowserTargetTerminationProjection,
    CdpConnection, CommandDispatchContext, PendingBrowserTargetClose, PreparedTargetHostClosure,
};
use crate::devtools_runtime::{DevToolsError, DevToolsErrorKind, DevToolsTargetKind};
use crate::domains::{command_output::CommandOutputPlan, page::FetchCancellationOwnerTaskStep};

use super::{events, worker_target};

const DISPOSE_REASON: &str = "Browser context disposed";
const INSPECTOR_DETACHED_REASON: &str = "Render process gone.";

struct PageTargetDisposal {
    target_id: String,
    page_owner: PageResidenceIdentity,
    termination: Option<BrowserTargetTerminationRequest>,
    fetch_owner_session_id: Option<Option<String>>,
    host_closure: PreparedTargetHostClosure,
}

struct PageFetchDisposal {
    page_owner: PageResidenceIdentity,
    projection_session_id: Option<String>,
    pending_navigations: Vec<crate::conn::PendingFetchNavigation>,
    pending_auth_navigations: Vec<crate::conn::PendingFetchAuthNavigation>,
    pending_response_navigations: Vec<crate::conn::PausedDocumentTransfer>,
}

struct BrowserContextDisposalOwnerState {
    reservation: Option<BrowserContextDisposalReservation>,
    browser_context_id: String,
    page_targets: VecDeque<PageTargetDisposal>,
    fetch_disposals: VecDeque<PageFetchDisposal>,
    inspector_session_ids: Vec<String>,
    worker_targets_prepared: bool,
    active_page_target: Option<PageTargetDisposal>,
    prefix_events: Vec<BackgroundProtocolEvent>,
    all_page_target_facts_projected: bool,
    events: Vec<BackgroundProtocolEvent>,
    command_context: CommandDispatchContext,
}

enum PendingBrowserContextDisposalParticipant {
    FetchCancellation(Box<crate::domains::page::PendingFetchCancellationOwnerTask>),
    TargetClose(Box<PendingBrowserTargetClose>),
    RuntimeCleanup(BrowserContextRuntimeCleanup),
}

struct BrowserContextRuntimeCleanup {
    pages: Vec<Page>,
    owners: Vec<RendererPageLifetimeOwner>,
    runtime_owner: RendererBrowserContextRuntimeOwner,
}

impl BrowserContextRuntimeCleanup {
    fn new(
        pages: Vec<Page>,
        owners: Vec<RendererPageLifetimeOwner>,
        mut runtime_owner: RendererBrowserContextRuntimeOwner,
    ) -> Self {
        // Close producer admission in the exact terminal owner turn. Page
        // owners may then finish asynchronously, but no new work can enter
        // the retired Context while this participant is detached.
        runtime_owner.terminate_renderer_producers_for_owner_shutdown();
        Self {
            pages,
            owners,
            runtime_owner,
        }
    }

    async fn wait(mut self) {
        for owner in self.owners {
            let _ = owner.close_async().await;
        }
        for page in self.pages {
            let _ = page.close_async().await;
        }
        self.runtime_owner.shutdown_network_and_join();
    }
}

enum CompletedBrowserContextDisposalParticipant {
    FetchCancellation(Box<crate::domains::page::CompletedFetchCancellationOwnerTask>),
    TargetClose(Box<crate::conn::CompletedBrowserTargetClose>),
    RuntimeCleanup,
}

/// One exact renderer/resource participant in whole-Context disposal.
///
/// The state owns the Core disposal reservation. Dropping a frontend reply
/// does not drop this value: Browser Host continues the chain until Context
/// removal commits or the reservation is explicitly rolled back.
pub(crate) struct PendingBrowserContextDisposalOwnerTask {
    state: BrowserContextDisposalOwnerState,
    participant: PendingBrowserContextDisposalParticipant,
}

pub(crate) struct CompletedBrowserContextDisposalOwnerTask {
    state: BrowserContextDisposalOwnerState,
    participant: CompletedBrowserContextDisposalParticipant,
}

pub(crate) enum BrowserContextDisposalOwnerTaskStep {
    Pending(Box<PendingBrowserContextDisposalOwnerTask>),
    Complete(BrowserContextDisposalOwnerTaskOutput),
}

pub(crate) struct BrowserContextDisposalOwnerTaskOutput {
    plan: CommandOutputPlan,
    command_context: CommandDispatchContext,
}

impl BrowserContextDisposalOwnerTaskOutput {
    fn new(plan: CommandOutputPlan, command_context: CommandDispatchContext) -> Self {
        Self {
            plan,
            command_context,
        }
    }

    pub(crate) fn into_parts(self) -> (CommandOutputPlan, CommandDispatchContext) {
        (self.plan, self.command_context)
    }
}

impl PendingBrowserContextDisposalOwnerTask {
    pub(crate) async fn wait(self: Box<Self>) -> CompletedBrowserContextDisposalOwnerTask {
        let Self { state, participant } = *self;
        let participant = match participant {
            PendingBrowserContextDisposalParticipant::FetchCancellation(pending) => {
                CompletedBrowserContextDisposalParticipant::FetchCancellation(Box::new(
                    pending.wait().await,
                ))
            }
            PendingBrowserContextDisposalParticipant::TargetClose(pending) => {
                CompletedBrowserContextDisposalParticipant::TargetClose(Box::new(
                    (*pending).wait().await,
                ))
            }
            PendingBrowserContextDisposalParticipant::RuntimeCleanup(cleanup) => {
                cleanup.wait().await;
                CompletedBrowserContextDisposalParticipant::RuntimeCleanup
            }
        };
        CompletedBrowserContextDisposalOwnerTask { state, participant }
    }
}

/// Starts disposal after Browser Host has selected the exact Context input.
///
/// All frontend parsing and response routing happened before publication. The
/// first mutation is the Core reservation, which prevents new Target/Page
/// work from entering while cleanup participants are outstanding.
pub(crate) fn start_browser_context_disposal_owner_task(
    conn: &mut CdpConnection,
    browser_context_handle: BrowserContextHandle,
    prefix_events: Vec<BackgroundProtocolEvent>,
    mut command_context: CommandDispatchContext,
) -> BrowserContextDisposalOwnerTaskStep {
    let reservation = match conn.begin_browser_context_disposal(&browser_context_handle) {
        Ok(reservation) => reservation,
        Err(error) => {
            return BrowserContextDisposalOwnerTaskStep::Complete(
                BrowserContextDisposalOwnerTaskOutput::new(
                    CommandOutputPlan::from_devtools_error(DevToolsError::new(
                        DevToolsErrorKind::Internal,
                        error.to_string(),
                    )),
                    command_context,
                ),
            );
        }
    };

    let preparation = prepare_browser_context_disposal(
        conn,
        &browser_context_handle,
        prefix_events,
        &mut command_context,
    );
    let (page_targets, fetch_disposals, inspector_session_ids, prefix_events, events) =
        match preparation {
            Ok(preparation) => preparation,
            Err(error) => {
                let _ = conn.rollback_browser_context_disposal(reservation);
                return BrowserContextDisposalOwnerTaskStep::Complete(
                    BrowserContextDisposalOwnerTaskOutput::new(
                        CommandOutputPlan::from_devtools_error(error),
                        command_context,
                    ),
                );
            }
        };

    drive_browser_context_disposal_owner_task(
        conn,
        BrowserContextDisposalOwnerState {
            reservation: Some(reservation),
            browser_context_id: browser_context_handle.browser_context_id().to_owned(),
            page_targets: page_targets.into(),
            fetch_disposals: fetch_disposals.into(),
            inspector_session_ids,
            worker_targets_prepared: false,
            active_page_target: None,
            prefix_events,
            all_page_target_facts_projected: true,
            events,
            command_context,
        },
    )
}

pub(crate) async fn complete_browser_context_disposal_owner_task(
    conn: &mut CdpConnection,
    completed: CompletedBrowserContextDisposalOwnerTask,
) -> BrowserContextDisposalOwnerTaskStep {
    let CompletedBrowserContextDisposalOwnerTask {
        mut state,
        participant,
    } = completed;
    match participant {
        CompletedBrowserContextDisposalParticipant::FetchCancellation(completed) => {
            match crate::domains::page::complete_pending_fetch_state_cancellation(conn, *completed)
                .await
            {
                FetchCancellationOwnerTaskStep::Pending(pending) => {
                    return BrowserContextDisposalOwnerTaskStep::Pending(Box::new(
                        PendingBrowserContextDisposalOwnerTask {
                            state,
                            participant:
                                PendingBrowserContextDisposalParticipant::FetchCancellation(pending),
                        },
                    ));
                }
                FetchCancellationOwnerTaskStep::Complete(output) => {
                    merge_fetch_cancellation_output(&mut state, output);
                }
            }
        }
        CompletedBrowserContextDisposalParticipant::TargetClose(completed) => {
            match conn.continue_browser_target_close(*completed) {
                BrowserTargetCloseStart::Pending(pending) => {
                    return BrowserContextDisposalOwnerTaskStep::Pending(Box::new(
                        PendingBrowserContextDisposalOwnerTask {
                            state,
                            participant: PendingBrowserContextDisposalParticipant::TargetClose(
                                Box::new(pending),
                            ),
                        },
                    ));
                }
                BrowserTargetCloseStart::Complete(projection) => {
                    finish_active_page_target_disposal(conn, &mut state, projection);
                }
            }
        }
        CompletedBrowserContextDisposalParticipant::RuntimeCleanup => {
            conn.release_idle_navigation_engine_memory_after_target_close();
            return BrowserContextDisposalOwnerTaskStep::Complete(finish_disposal_success(state));
        }
    }
    drive_browser_context_disposal_owner_task(conn, state)
}

type BrowserContextDisposalPreparation = (
    Vec<PageTargetDisposal>,
    Vec<PageFetchDisposal>,
    Vec<String>,
    Vec<BackgroundProtocolEvent>,
    Vec<BackgroundProtocolEvent>,
);

fn prepare_browser_context_disposal(
    conn: &mut CdpConnection,
    browser_context_handle: &BrowserContextHandle,
    prefix_events: Vec<BackgroundProtocolEvent>,
    command_context: &mut CommandDispatchContext,
) -> Result<BrowserContextDisposalPreparation, DevToolsError> {
    let browser_context_id = browser_context_handle.browser_context_id();
    let Some(browser_context) = conn.browser_context_by_id(browser_context_id) else {
        return Err(browser_context_not_found(browser_context_id));
    };
    if browser_context.browser_context_handle() != browser_context_handle {
        return Err(browser_context_not_found(browser_context_id));
    }

    let active_page_target_id = browser_context.active_target_id().map(str::to_owned);
    let mut page_target_ids = browser_context
        .background_targets
        .iter()
        .rev()
        .map(|target| target.target_id().to_owned())
        .collect::<Vec<_>>();
    if let Some(active_page_target_id) = active_page_target_id.as_ref() {
        page_target_ids.push(active_page_target_id.clone());
    }

    let target_ids = browser_context
        .devtools_target_infos()
        .into_iter()
        .filter_map(|target_info| {
            target_info
                .target_id
                .map(|target_id| (target_info.kind, target_id.into_string()))
        })
        .collect::<Vec<_>>();

    let mut page_targets = Vec::with_capacity(page_target_ids.len());
    for target_id in page_target_ids {
        let owner = BrowserPageOwnerKey::new(browser_context_id, target_id.as_str());
        let termination = conn
            .capture_browser_target_termination_for_owner(
                &owner,
                crate::conn::BrowserTargetTerminationProjectionKind::TargetClose,
            )
            .ok_or_else(|| browser_context_not_found(browser_context_id))?;
        let browser_context = conn
            .browser_context_by_id(browser_context_id)
            .ok_or_else(|| browser_context_not_found(browser_context_id))?;
        let session_ids = target_session_ids(conn, browser_context, &target_id);
        let fetch_owner_session_id =
            page_fetch_owner_session_id(browser_context, &target_id, &session_ids);
        page_targets.push(PageTargetDisposal {
            target_id: target_id.clone(),
            page_owner: termination.page().clone(),
            termination: Some(termination),
            fetch_owner_session_id,
            host_closure: conn.prepare_target_host_closure(&target_id),
        });
    }

    let mut seen_sessions = HashSet::new();
    let inspector_session_ids = target_ids
        .into_iter()
        .filter(|(kind, _)| {
            matches!(
                kind,
                DevToolsTargetKind::Page
                    | DevToolsTargetKind::SharedWorker
                    | DevToolsTargetKind::ServiceWorker
            )
        })
        .flat_map(|(_, target_id)| {
            conn.browser_context_by_id(browser_context_id)
                .map(|browser_context| target_session_ids(conn, browser_context, &target_id))
                .unwrap_or_default()
        })
        .filter(|session_id| seen_sessions.insert(session_id.clone()))
        .collect::<Vec<_>>();

    let mut events = Vec::new();
    if let Some(active_target_id) = active_page_target_id.as_deref()
        && let Some(active_page) = page_targets
            .iter()
            .find(|target| target.target_id == active_target_id)
        && let Some(route) = conn.target_page_owner_route_if_current(&active_page.page_owner)
    {
        let mut owner_scope = conn.scoped_none_session_owner_route_override(route);
        owner_scope
            .conn_mut()
            .fail_pending_inspector_awaits_for_session_owner_background_events_into(
                &mut events,
                command_context.protocol_events_mut(),
                None,
                DISPOSE_REASON,
            );
    }
    for session_id in &inspector_session_ids {
        conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
            &mut events,
            command_context.protocol_events_mut(),
            Some(session_id),
            DISPOSE_REASON,
        );
    }

    let mut fetch_disposals = Vec::new();
    for page_target in &page_targets {
        let Some(projection_session_id) = page_target.fetch_owner_session_id.clone() else {
            continue;
        };
        let Some(route) = conn.target_page_owner_route_if_current(&page_target.page_owner) else {
            continue;
        };
        let pending = {
            let mut owner_scope = conn.scoped_none_session_owner_route_override(route);
            crate::domains::page::take_pending_fetch_state(owner_scope.conn_mut(), None)
        };
        let (
            pending_navigations,
            pending_auth_navigations,
            pending_response_navigations,
            _pending_subresource_fetches,
            _pending_subresource_auths,
            _pending_subresource_responses,
        ) = pending;
        fetch_disposals.push(PageFetchDisposal {
            page_owner: page_target.page_owner.clone(),
            projection_session_id,
            pending_navigations,
            pending_auth_navigations,
            pending_response_navigations,
        });
    }

    Ok((
        page_targets,
        fetch_disposals,
        inspector_session_ids,
        prefix_events,
        events,
    ))
}

fn drive_browser_context_disposal_owner_task(
    conn: &mut CdpConnection,
    mut state: BrowserContextDisposalOwnerState,
) -> BrowserContextDisposalOwnerTaskStep {
    loop {
        if let Some(fetch) = state.fetch_disposals.pop_front() {
            let step = crate::domains::page::start_pending_fetch_state_cancellation(
                conn,
                Some(fetch.page_owner),
                fetch.projection_session_id,
                DISPOSE_REASON.to_owned(),
                fetch.pending_navigations,
                fetch.pending_auth_navigations,
                fetch.pending_response_navigations,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            match step {
                FetchCancellationOwnerTaskStep::Pending(pending) => {
                    return BrowserContextDisposalOwnerTaskStep::Pending(Box::new(
                        PendingBrowserContextDisposalOwnerTask {
                            state,
                            participant:
                                PendingBrowserContextDisposalParticipant::FetchCancellation(pending),
                        },
                    ));
                }
                FetchCancellationOwnerTaskStep::Complete(output) => {
                    merge_fetch_cancellation_output(&mut state, output);
                    continue;
                }
            }
        }

        if !state.worker_targets_prepared {
            state
                .events
                .extend(state.command_context.take_protocol_events());
            for session_id in &state.inspector_session_ids {
                state.events.push(events::inspector_detached_event(
                    session_id,
                    INSPECTOR_DETACHED_REASON,
                ));
            }
            let worker_outputs = worker_target::prepare_browser_context_worker_targets_for_dispose(
                conn,
                &state.browser_context_id,
                DISPOSE_REASON,
            );
            state.events.extend(
                worker_target::browser_context_worker_target_removal_background_events(
                    conn,
                    worker_outputs,
                ),
            );
            state.worker_targets_prepared = true;
        }

        if let Some(mut page_target) = state.page_targets.pop_front() {
            let mut termination_events = Vec::new();
            let Some(termination) = page_target.termination.take() else {
                state.all_page_target_facts_projected = false;
                tracing::error!(
                    target_id = page_target.target_id,
                    "prepared Context disposal Target lost its termination capability"
                );
                continue;
            };
            let step = conn.start_browser_target_close_for_context_disposal(
                termination,
                &mut termination_events,
                DISPOSE_REASON,
            );
            state.events.extend(termination_events);
            match step {
                Some(BrowserTargetCloseStart::Pending(pending)) => {
                    state.active_page_target = Some(page_target);
                    return BrowserContextDisposalOwnerTaskStep::Pending(Box::new(
                        PendingBrowserContextDisposalOwnerTask {
                            state,
                            participant: PendingBrowserContextDisposalParticipant::TargetClose(
                                Box::new(pending),
                            ),
                        },
                    ));
                }
                Some(BrowserTargetCloseStart::Complete(projection)) => {
                    state.active_page_target = Some(page_target);
                    finish_active_page_target_disposal(conn, &mut state, projection);
                    continue;
                }
                None => {
                    state.all_page_target_facts_projected = false;
                    tracing::debug!(
                        target_id = page_target.target_id,
                        "BrowserContext disposal skipped a Target already retired by another owner turn"
                    );
                    continue;
                }
            }
        }

        conn.apply_browser_download_policy_update(
            moli_core::browser_host::BrowserDownloadPolicyUpdate::RemoveBrowserContext {
                browser_context_id: state.browser_context_id.clone(),
            },
        );
        conn.clear_automation_download_events_for_browser_context(
            state.browser_context_id.as_str(),
        );
        let mut permission_overrides = conn
            .browser_host_policy_snapshot()
            .permission_overrides()
            .to_vec();
        permission_overrides.retain(|entry| {
            entry.browser_context_id.as_deref() != Some(state.browser_context_id.as_str())
        });
        conn.apply_browser_host_policy_update(
            moli_core::browser_host::BrowserHostPolicyUpdate::ReplacePermissionOverrides(
                permission_overrides,
            ),
        );
        let Some(reservation) = state.reservation.take() else {
            return BrowserContextDisposalOwnerTaskStep::Complete(finish_disposal_error(
                state,
                DevToolsError::new(
                    DevToolsErrorKind::Internal,
                    "BrowserContextDisposalReservationMissing",
                ),
            ));
        };
        let (mut removed, retired_renderer_page_owners, renderer_runtime_owner) =
            match conn.remove_browser_context_for_disposal(&reservation) {
                Ok(removed) => removed,
                Err(error) => {
                    let _ = conn.rollback_browser_context_disposal(reservation);
                    return BrowserContextDisposalOwnerTaskStep::Complete(finish_disposal_error(
                        state,
                        error.into(),
                    ));
                }
            };
        drop(reservation);
        let residual_pages = removed.take_residual_pages_for_browser_context_disposal();
        drop(removed);
        let cleanup = BrowserContextRuntimeCleanup::new(
            residual_pages,
            retired_renderer_page_owners,
            renderer_runtime_owner,
        );
        if !cleanup.pages.is_empty() || !cleanup.owners.is_empty() {
            tracing::warn!(
                browser_context_id = state.browser_context_id,
                physical_page_count = cleanup.pages.len(),
                core_owner_count = cleanup.owners.len(),
                "BrowserContext removal retained renderer Pages outside the normal Target termination chain"
            );
        }
        return BrowserContextDisposalOwnerTaskStep::Pending(Box::new(
            PendingBrowserContextDisposalOwnerTask {
                state,
                participant: PendingBrowserContextDisposalParticipant::RuntimeCleanup(cleanup),
            },
        ));
    }
}

fn merge_fetch_cancellation_output(
    state: &mut BrowserContextDisposalOwnerState,
    output: crate::domains::page::FetchCancellationOwnerTaskOutput,
) {
    let (events, predecessor) = output.into_parts();
    state.events.extend(events);
    if let Some(predecessor) = predecessor {
        state
            .command_context
            .set_renderer_output_predecessor(predecessor);
    }
}

fn finish_active_page_target_disposal(
    conn: &mut CdpConnection,
    state: &mut BrowserContextDisposalOwnerState,
    projection: BrowserTargetTerminationProjection,
) {
    let Some(page_target) = state.active_page_target.take() else {
        tracing::error!("BrowserContext disposal lost its active Target projection state");
        return;
    };
    let BrowserTargetTerminationProjection::Closed {
        closed,
        browser_fact,
    } = projection
    else {
        tracing::error!(
            target_id = page_target.target_id,
            "BrowserContext disposal Target close projected a crash"
        );
        return;
    };

    let target_id = page_target.target_id;
    let (target_detached_info_deltas, target_destroyed_deltas) =
        page_target.host_closure.into_parts();
    let project_bidi_lifecycle = conn.webdriver_bidi_target_lifecycle_projection_enabled();
    let mut terminal_events = if project_bidi_lifecycle {
        conn.prepared_top_level_target_host_deltas_event_plan(target_detached_info_deltas)
    } else {
        conn.prepared_target_host_deltas_event_plan(target_detached_info_deltas)
    };
    terminal_events.extend(conn.detach_target_closure_cleanup_event_plan(
        closed.into_detach_cleanup_plan(Some(INSPECTOR_DETACHED_REASON)),
        None,
    ));
    terminal_events.extend(conn.detach_closed_top_level_target_sessions_event_plan(
        &target_id,
        Some(INSPECTOR_DETACHED_REASON),
    ));
    if project_bidi_lifecycle {
        terminal_events
            .extend(conn.prepared_top_level_target_host_deltas_event_plan(target_destroyed_deltas));
    } else {
        terminal_events
            .extend(conn.prepared_target_host_deltas_event_plan(target_destroyed_deltas));
    }
    if let Some(browser_fact) = browser_fact {
        tracing::trace!(
            browser_fact_sequence = browser_fact.envelope().sequence().get(),
            target_id,
            "projecting BrowserContext disposal Target close from exact Browser fact"
        );
        state.events.extend(terminal_events);
    } else {
        state.all_page_target_facts_projected = false;
        tracing::error!(
            target_id,
            "suppressing BrowserContext disposal Target events without an exact Browser fact"
        );
    }
}

fn finish_disposal_success(
    mut state: BrowserContextDisposalOwnerState,
) -> BrowserContextDisposalOwnerTaskOutput {
    let mut plan = CommandOutputPlan::success();
    if state.all_page_target_facts_projected {
        plan.extend_background_events(std::mem::take(&mut state.prefix_events));
    } else {
        tracing::error!(
            browser_context_id = state.browser_context_id,
            "suppressing frozen BrowserContext lifecycle events because a Target fact was missing"
        );
    }
    plan.extend_background_events(state.events);
    BrowserContextDisposalOwnerTaskOutput::new(plan, state.command_context)
}

fn finish_disposal_error(
    state: BrowserContextDisposalOwnerState,
    error: DevToolsError,
) -> BrowserContextDisposalOwnerTaskOutput {
    let mut plan = CommandOutputPlan::from_devtools_error(error);
    plan.extend_background_events(state.events);
    BrowserContextDisposalOwnerTaskOutput::new(plan, state.command_context)
}

fn target_session_ids(
    conn: &CdpConnection,
    browser_context: &crate::conn::BrowserContext,
    target_id: &str,
) -> Vec<String> {
    let mut session_ids = browser_context.devtools_session_ids_for_target(target_id);
    session_ids.extend(conn.attached_sessions_for_target(target_id));
    session_ids.sort();
    session_ids.dedup();
    session_ids
}

fn page_fetch_owner_session_id(
    browser_context: &crate::conn::BrowserContext,
    target_id: &str,
    session_ids: &[String],
) -> Option<Option<String>> {
    if browser_context.is_active_target(target_id) {
        return Some(browser_context.active_session_id_owned());
    }
    browser_context
        .primary_session_id_for_target(target_id)
        .map(str::to_owned)
        .or_else(|| session_ids.first().cloned())
        .map(Some)
}

/// Compatibility drain for direct `CdpConnection` callers that have not yet
/// moved their application loop to the Browser Host frontend adapter.
/// Production raw CDP/BiDi scheduling publishes the same owner task instead.
pub(super) async fn execute_browser_context_disposal_async(
    conn: &mut CdpConnection,
    browser_context_id: String,
    prefix_events: Vec<BackgroundProtocolEvent>,
    out: &mut events::TargetProtocolSideEffects,
    command_context: &mut CommandDispatchContext,
) -> Result<(), DevToolsError> {
    let Some(handle) = conn
        .browser_context_by_id(&browser_context_id)
        .map(|context| context.browser_context_handle().clone())
    else {
        return Err(browser_context_not_found(&browser_context_id));
    };
    let mut step = start_browser_context_disposal_owner_task(
        conn,
        handle,
        prefix_events,
        std::mem::take(command_context),
    );
    loop {
        match step {
            BrowserContextDisposalOwnerTaskStep::Pending(pending) => {
                step =
                    complete_browser_context_disposal_owner_task(conn, pending.wait().await).await;
            }
            BrowserContextDisposalOwnerTaskStep::Complete(output) => {
                let (plan, returned_command_context) = output.into_parts();
                *command_context = returned_command_context;
                let (status, events) = plan.into_command_status_and_background_events();
                out.extend_background_events(events);
                return status.unwrap_or(Ok(()));
            }
        }
    }
}

fn browser_context_not_found(browser_context_id: &str) -> DevToolsError {
    DevToolsError::new(
        DevToolsErrorKind::Internal,
        format!("Failed to find context with id {browser_context_id}"),
    )
}
