use crate::conn::{
    BackgroundProtocolEvent, BrowserPageTargetTerminationStart, BrowserTargetCloseStart,
    BrowserTargetTerminationProjection, CdpConnection, Cmd, DEFAULT_LOADER_ID,
    PendingBrowserPageTargetTermination, PendingBrowserTargetClose, PendingFetchNavigation,
    PendingSubresourceFetchAuthRequest, PendingSubresourceFetchRequest,
    PendingSubresourceFetchResponseRequest, PreparedTargetHostClosure, monotonic_timestamp_seconds,
};
use crate::devtools_runtime::AutomationEvent;
use moli_core::RendererOutputFence;
use moli_core::browser_host::{
    BrowserOwnerInput, BrowserTargetTerminationKind, BrowserTargetTerminationRequest,
};

use super::PageCommandTaskStep;
use crate::domains::command_output::CommandOutputPlan;
use crate::domains::network;

pub(crate) use crate::conn::BrowserTargetTerminationProjectionKind as PageTargetTerminationKind;

pub(crate) enum PageTargetTerminationOwnerTaskStep {
    Complete(crate::conn::CdpTurnOutcome),
    Pending(Box<PendingPageTargetTerminationOwnerTask>),
}

pub(crate) struct PendingPageTargetTerminationOwnerTask {
    pending: PendingBrowserPageTargetTermination,
    completion: TargetTerminationCompletion,
}

impl PendingPageTargetTerminationOwnerTask {
    pub(crate) async fn wait(self) -> CompletedPageTargetTerminationOwnerTask {
        CompletedPageTargetTerminationOwnerTask {
            projection: self.pending.wait().await,
            completion: self.completion,
        }
    }
}

pub(crate) struct CompletedPageTargetTerminationOwnerTask {
    projection: BrowserTargetTerminationProjection,
    completion: TargetTerminationCompletion,
}

pub(crate) enum TargetCloseOwnerTaskStep {
    Complete(crate::conn::CdpTurnOutcome),
    Pending(Box<PendingTargetCloseOwnerTask>),
}

pub(crate) struct PendingTargetCloseOwnerTask {
    pending: PendingBrowserTargetClose,
    completion: TargetTerminationCompletion,
}

impl PendingTargetCloseOwnerTask {
    pub(crate) async fn wait(self) -> CompletedTargetCloseOwnerTask {
        CompletedTargetCloseOwnerTask {
            completed: self.pending.wait().await,
            completion: self.completion,
        }
    }
}

pub(crate) struct CompletedTargetCloseOwnerTask {
    completed: crate::conn::CompletedBrowserTargetClose,
    completion: TargetTerminationCompletion,
}

struct TargetTerminationCompletion {
    expected_target_id: String,
    kind: PageTargetTerminationKind,
    out: Vec<BackgroundProtocolEvent>,
    target_host_closure: Option<PreparedTargetHostClosure>,
}

fn complete_page_termination_admission(
    conn: &CdpConnection,
    events: Vec<BackgroundProtocolEvent>,
    termination: BrowserTargetTerminationRequest,
) -> PageCommandTaskStep {
    let mut plan =
        match conn.publish_browser_owner_input(BrowserOwnerInput::page_termination(termination)) {
            Ok(()) => CommandOutputPlan::success(),
            Err(error) => CommandOutputPlan::error_without_session(
                -32000,
                format!("BrowserHostPageTerminationAdmissionFailed: {error}"),
            ),
        };
    for event in events {
        plan.push_background_event(event);
    }
    PageCommandTaskStep::Complete(plan)
}

pub(crate) fn take_pending_fetch_state(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
) -> (
    Vec<PendingFetchNavigation>,
    Vec<crate::conn::PendingFetchAuthNavigation>,
    Vec<crate::conn::PausedDocumentTransfer>,
    Vec<(String, PendingSubresourceFetchRequest)>,
    Vec<(String, crate::conn::PendingSubresourceFetchAuthRequest)>,
    Vec<(String, crate::conn::PendingSubresourceFetchResponseRequest)>,
) {
    conn.take_pending_fetch_state_for_session_owner(session_id)
        .unwrap_or_else(|| {
            (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        })
}

pub(crate) async fn fail_pending_fetch_state_background_events_async(
    conn: &mut CdpConnection,
    out: &mut Vec<BackgroundProtocolEvent>,
    session_id: Option<&str>,
    error_text: &str,
    pending_navigations: Vec<PendingFetchNavigation>,
    pending_auth_navigations: Vec<crate::conn::PendingFetchAuthNavigation>,
    pending_response_navigations: Vec<crate::conn::PausedDocumentTransfer>,
    pending_subresource_fetches: Vec<(String, PendingSubresourceFetchRequest)>,
    pending_subresource_auths: Vec<(String, crate::conn::PendingSubresourceFetchAuthRequest)>,
    pending_subresource_responses: Vec<(
        String,
        crate::conn::PendingSubresourceFetchResponseRequest,
    )>,
) -> Option<RendererOutputFence> {
    super::fetch_cancellation::drain_pending_fetch_state_cancellation_async(
        conn,
        out,
        session_id,
        error_text,
        pending_navigations,
        pending_auth_navigations,
        pending_response_navigations,
        pending_subresource_fetches,
        pending_subresource_auths,
        pending_subresource_responses,
    )
    .await
}

/// Completes protocol-owned subresource pauses after the renderer has crashed.
///
/// Normal Fetch cancellation first applies failure to the Page owner and then
/// projects its network backlog. `Page.crash` cannot use that path: the Page
/// owner may be blocked in JavaScript, and the IO termination which unblocks it
/// also retires the Page residence. The pending Fetch residences were already
/// claimed by [`take_pending_fetch_state`], so their terminal network state can
/// be emitted without returning to the renderer owner.
fn fail_crashed_subresource_fetches_background_events(
    conn: &CdpConnection,
    out: &mut Vec<BackgroundProtocolEvent>,
    session_id: Option<&str>,
    error_text: &str,
    pending_subresource_fetches: Vec<(String, PendingSubresourceFetchRequest)>,
    pending_subresource_auths: Vec<(String, PendingSubresourceFetchAuthRequest)>,
    pending_subresource_responses: Vec<(String, PendingSubresourceFetchResponseRequest)>,
) {
    let loader_id = conn
        .current_document_loader_id_for_session_owner(session_id)
        .unwrap_or_else(|| DEFAULT_LOADER_ID.to_owned());
    let event_session_ids = conn.network_event_session_ids_for_session_owner(session_id);
    let timestamp = monotonic_timestamp_seconds();

    let mut emit_failure =
        |network_request_id: &str,
         frame_id: &str,
         resource_type: moli_core::page::SubresourceResourceType| {
            for event_session_id in &event_session_ids {
                network::emit_loading_failed(
                    out,
                    event_session_id.as_deref(),
                    network_request_id,
                    frame_id,
                    &loader_id,
                    timestamp,
                    error_text,
                    resource_type.into(),
                );
            }
        };

    for (_, pending) in pending_subresource_fetches {
        if let Some(continuation) = pending.detached_parser_script_fetch_continuation() {
            let _ = continuation.fail(error_text.to_owned());
        }
        emit_failure(
            &pending.network_request_id,
            &pending.frame_id,
            pending.resource_type,
        );
    }
    for (_, pending) in pending_subresource_auths {
        emit_failure(
            &pending.network_request_id,
            &pending.frame_id,
            pending.resource_type,
        );
    }
    for (_, pending) in pending_subresource_responses {
        emit_failure(
            &pending.network_request_id,
            &pending.frame_id,
            pending.resource_type,
        );
    }
}

pub(super) fn try_start_crash_command_dispatch(
    conn: &CdpConnection,
    cmd: &Cmd<'_>,
) -> PageCommandTaskStep {
    match cmd.get_params::<serde_json::Value>() {
        Ok(Some(_)) | Ok(None) => {}
        Err(_) => {
            return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    }
    PageCommandTaskStep::Pending(super::PendingPageCommandDispatch {
        command_id: cmd.id,
        owner_scope: crate::conn::CommandOwnerScope::capture(conn, cmd.session_id),
        kind: Box::new(super::PendingPageCommandKind::Crash),
    })
}

pub(super) async fn complete_crash_command_dispatch(
    conn: &mut CdpConnection,
    _command_id: Option<u64>,
    session_id: Option<&str>,
    command_context: &mut crate::conn::CommandDispatchContext,
) -> PageCommandTaskStep {
    let mut out = Vec::new();
    let Some((_, target_id)) = conn.target_owner_identity_for_session(session_id) else {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error_without_session(
            -31998,
            "BrowserContextNotLoaded",
        ));
    };
    let Some(target_id) = target_id else {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error_without_session(
            -31998,
            "TargetNotLoaded",
        ));
    };

    let primary_session_id = conn.runtime_session_owner_primary_session_id(session_id);
    let fail_session_id = session_id.or(primary_session_id.as_deref());
    let (
        pending_navigations,
        pending_auth_navigations,
        pending_response_navigations,
        pending_subresource_fetches,
        pending_subresource_auths,
        pending_subresource_responses,
    ) = take_pending_fetch_state(conn, session_id);

    // Chromium handles Page.crash directly at the renderer IO-agent boundary;
    // it never enters a V8InspectorSession or the ordinary target IO task FIFO.
    // Seal both DevTools receivers and interrupt active V8 synchronously so
    // target retirement cannot wait behind earlier JavaScript or IO work.
    if let Ok(page) = conn.loaded_page_mut_for_interruptible_protocol_access(session_id) {
        page.crash_devtools_target_from_io();
    }

    // A crash retires the target, not just the DevTools session which issued
    // the command. Settle every attached session before the renderer Page is
    // removed, otherwise a late completion can retain a sender owned by the
    // retired target.
    let target_inspector_session_ids = conn.page_event_session_ids_for_session_owner(session_id);
    let mut pending_await_events = Vec::new();
    for inspector_session_id in &target_inspector_session_ids {
        conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
            &mut pending_await_events,
            command_context.protocol_events_mut(),
            inspector_session_id.as_deref(),
            "Page crashed",
        );
    }
    out.extend(pending_await_events);

    let renderer_output_predecessor = fail_pending_fetch_state_background_events_async(
        conn,
        &mut out,
        fail_session_id,
        "Page crashed",
        pending_navigations,
        pending_auth_navigations,
        pending_response_navigations,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .await;
    fail_crashed_subresource_fetches_background_events(
        conn,
        &mut out,
        fail_session_id,
        "Page crashed",
        pending_subresource_fetches,
        pending_subresource_auths,
        pending_subresource_responses,
    );
    if let Some(predecessor) = renderer_output_predecessor {
        command_context.set_renderer_output_predecessor(predecessor);
    }

    // Browser Core still owns the terminal transition and its fact. The
    // frontend only admits that exact request after all protocol-owned
    // cancellation records above have been frozen.
    let termination = conn
        .capture_browser_target_termination_for_session_owner(
            session_id,
            PageTargetTerminationKind::Crash,
        )
        .expect("validated Page.crash target must expose an exact Browser residence");
    debug_assert_eq!(termination.owner().target_id(), target_id);
    complete_page_termination_admission(conn, out, termination)
}

pub(super) fn try_start_close_command_dispatch(
    conn: &CdpConnection,
    cmd: &Cmd<'_>,
) -> PageCommandTaskStep {
    PageCommandTaskStep::Pending(super::PendingPageCommandDispatch {
        command_id: cmd.id,
        owner_scope: crate::conn::CommandOwnerScope::capture(conn, cmd.session_id),
        kind: Box::new(super::PendingPageCommandKind::Close),
    })
}

pub(super) async fn complete_close_command_dispatch(
    conn: &mut CdpConnection,
    _command_id: Option<u64>,
    session_id: Option<&str>,
    command_context: &mut crate::conn::CommandDispatchContext,
) -> PageCommandTaskStep {
    let mut out = Vec::new();
    let Some((_, target_id)) = conn.target_owner_identity_for_session(session_id) else {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error_without_session(
            -31998,
            "BrowserContextNotLoaded",
        ));
    };
    if target_id.is_none() {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error_without_session(
            -31998,
            "TargetNotLoaded",
        ));
    };
    let target_id = target_id.expect("validated Page target identity");
    let primary_session_id = conn.runtime_session_owner_primary_session_id(session_id);
    let fail_session_id = session_id.or(primary_session_id.as_deref());

    let (
        pending_navigations,
        pending_auth_navigations,
        pending_response_navigations,
        pending_subresource_fetches,
        pending_subresource_auths,
        pending_subresource_responses,
    ) = take_pending_fetch_state(conn, session_id);

    let mut pending_await_events = Vec::new();
    conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
        &mut pending_await_events,
        command_context.protocol_events_mut(),
        session_id,
        "Page closed",
    );
    out.extend(pending_await_events);

    let renderer_output_predecessor = fail_pending_fetch_state_background_events_async(
        conn,
        &mut out,
        fail_session_id,
        "Page closed",
        pending_navigations,
        pending_auth_navigations,
        pending_response_navigations,
        pending_subresource_fetches,
        pending_subresource_auths,
        pending_subresource_responses,
    )
    .await;
    if let Some(predecessor) = renderer_output_predecessor {
        command_context.set_renderer_output_predecessor(predecessor);
    }

    // Closing the target here would retire its session route before the
    // separately transported final Page publication crosses protocol ingress.
    // Publish a concrete protocol-owner continuation instead. The command
    // fence first admits every renderer record produced above, then the
    // scheduler sends the Page.close response and runs this teardown action.
    let termination = conn
        .capture_browser_target_termination_for_session_owner(
            session_id,
            PageTargetTerminationKind::PageClose,
        )
        .expect("validated Page.close target must expose an exact Browser residence");
    debug_assert_eq!(termination.owner().target_id(), target_id);
    complete_page_termination_admission(conn, out, termination)
}

/// Starts one actor-selected Page.crash/Page.close terminal action.
///
/// Browser Core commit and physical Page absence are synchronous. Destruction
/// of a retired renderer Page, when present, becomes a move-owned participant
/// and resumes through the Browser Host completion mailbox.
pub(crate) fn start_page_target_termination_owner_task(
    conn: &mut CdpConnection,
    termination: BrowserTargetTerminationRequest,
) -> PageTargetTerminationOwnerTaskStep {
    let expected_target_id = termination.owner().target_id().to_owned();
    let (kind, close_reason) = match termination.kind() {
        BrowserTargetTerminationKind::Crash => (PageTargetTerminationKind::Crash, "Page crashed"),
        BrowserTargetTerminationKind::Close => {
            (PageTargetTerminationKind::PageClose, "Page closed")
        }
    };
    let target_host_closure = (kind == PageTargetTerminationKind::PageClose)
        .then(|| conn.prepare_target_host_closure(&expected_target_id));
    let mut out = Vec::new();
    let completion = TargetTerminationCompletion {
        expected_target_id,
        kind,
        out: Vec::new(),
        target_host_closure,
    };
    match conn.start_browser_page_target_termination(termination, kind, &mut out, close_reason) {
        Some(BrowserPageTargetTerminationStart::Complete(projection)) => {
            let mut completion = completion;
            completion.out = out;
            PageTargetTerminationOwnerTaskStep::Complete(finish_page_target_termination_projection(
                conn, projection, completion,
            ))
        }
        Some(BrowserPageTargetTerminationStart::Pending(pending)) => {
            let mut completion = completion;
            completion.out = out;
            PageTargetTerminationOwnerTaskStep::Pending(Box::new(
                PendingPageTargetTerminationOwnerTask {
                    pending,
                    completion,
                },
            ))
        }
        None => PageTargetTerminationOwnerTaskStep::Complete(
            crate::conn::CdpTurnOutcome::new_with_protocol_events(
                out,
                conn.take_scheduler_events(),
            ),
        ),
    }
}

pub(crate) fn complete_page_target_termination_owner_task(
    conn: &mut CdpConnection,
    completed: CompletedPageTargetTerminationOwnerTask,
) -> crate::conn::CdpTurnOutcome {
    finish_page_target_termination_projection(conn, completed.projection, completed.completion)
}

pub(crate) fn start_target_close_owner_task(
    conn: &mut CdpConnection,
    termination: BrowserTargetTerminationRequest,
) -> TargetCloseOwnerTaskStep {
    let expected_target_id = termination.owner().target_id().to_owned();
    let mut out = Vec::new();
    let completion = TargetTerminationCompletion {
        target_host_closure: Some(conn.prepare_target_host_closure(&expected_target_id)),
        expected_target_id,
        kind: PageTargetTerminationKind::TargetClose,
        out: Vec::new(),
    };
    match conn.start_browser_target_close(termination, &mut out, "Target closed") {
        Some(BrowserTargetCloseStart::Complete(projection)) => {
            let mut completion = completion;
            completion.out = out;
            TargetCloseOwnerTaskStep::Complete(finish_page_target_termination_projection(
                conn, projection, completion,
            ))
        }
        Some(BrowserTargetCloseStart::Pending(pending)) => {
            let mut completion = completion;
            completion.out = out;
            TargetCloseOwnerTaskStep::Pending(Box::new(PendingTargetCloseOwnerTask {
                pending,
                completion,
            }))
        }
        None => TargetCloseOwnerTaskStep::Complete(
            crate::conn::CdpTurnOutcome::new_with_protocol_events(
                out,
                conn.take_scheduler_events(),
            ),
        ),
    }
}

pub(crate) fn complete_target_close_owner_task(
    conn: &mut CdpConnection,
    completed: CompletedTargetCloseOwnerTask,
) -> TargetCloseOwnerTaskStep {
    match conn.continue_browser_target_close(completed.completed) {
        BrowserTargetCloseStart::Complete(projection) => TargetCloseOwnerTaskStep::Complete(
            finish_page_target_termination_projection(conn, projection, completed.completion),
        ),
        BrowserTargetCloseStart::Pending(pending) => {
            TargetCloseOwnerTaskStep::Pending(Box::new(PendingTargetCloseOwnerTask {
                pending,
                completion: completed.completion,
            }))
        }
    }
}

fn finish_page_target_termination_projection(
    conn: &mut CdpConnection,
    projection: BrowserTargetTerminationProjection,
    completion: TargetTerminationCompletion,
) -> crate::conn::CdpTurnOutcome {
    let TargetTerminationCompletion {
        expected_target_id,
        kind,
        mut out,
        target_host_closure,
    } = completion;
    let closed = match projection {
        BrowserTargetTerminationProjection::Crashed {
            inspector_session_ids,
            browser_fact,
        } => {
            if let Some(browser_fact) = browser_fact {
                tracing::trace!(
                    browser_fact_sequence = browser_fact.envelope().sequence().get(),
                    target_id = expected_target_id,
                    "projecting Target crash from exact Browser fact"
                );
                for inspector_session_id in inspector_session_ids {
                    out.push(BackgroundProtocolEvent::inspector_target_crashed(
                        inspector_session_id.as_deref(),
                    ));
                }
                out.extend(conn.target_crashed_events_for_all_discovery_owners(
                    &expected_target_id,
                    "crashed",
                    1,
                ));
            } else {
                tracing::error!(
                    target_id = expected_target_id,
                    "suppressing Target crash events without an exact Browser fact"
                );
            }
            return crate::conn::CdpTurnOutcome::new_with_protocol_events(
                out,
                conn.take_scheduler_events(),
            );
        }
        BrowserTargetTerminationProjection::Closed {
            closed,
            browser_fact,
        } => (closed, browser_fact),
    };
    let (closed, browser_fact) = closed;
    let Some(target_host_closure) = target_host_closure else {
        tracing::error!(
            target_id = expected_target_id,
            ?kind,
            "closed Target projection lost its prepared host closure"
        );
        return crate::conn::CdpTurnOutcome::new_with_protocol_events(
            out,
            conn.take_scheduler_events(),
        );
    };
    let target_destroyed_lifecycle =
        target_host_closure.destroyed_target_lifecycle_event(&expected_target_id);
    let project_bidi_lifecycle = conn.webdriver_bidi_target_lifecycle_projection_enabled();
    let closed_target_id = closed.target_id.clone();
    let (target_detached_info_deltas, target_destroyed_deltas) = target_host_closure.into_parts();
    let mut terminal_events = Vec::new();
    for sid in closed.inspector_detached_session_ids() {
        terminal_events.push(BackgroundProtocolEvent::inspector_detached(
            Some(sid),
            "Render process gone.",
        ));
    }
    if project_bidi_lifecycle {
        terminal_events.extend(
            conn.prepared_top_level_target_host_deltas_event_plan(target_detached_info_deltas),
        );
    } else {
        terminal_events
            .extend(conn.prepared_target_host_deltas_event_plan(target_detached_info_deltas));
    }
    terminal_events.extend(conn.detach_target_closure_cleanup_event_plan(
        closed.into_detach_cleanup_plan(Some("Render process gone.")),
        None,
    ));
    terminal_events.extend(conn.detach_closed_top_level_target_sessions_event_plan(
        &closed_target_id,
        Some("Render process gone."),
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
            target_id = closed_target_id,
            "projecting Target close from exact Browser fact"
        );
        out.extend(terminal_events);
        if project_bidi_lifecycle {
            if let Some(event) = target_destroyed_lifecycle {
                out.push(BackgroundProtocolEvent::automation_only(
                    AutomationEvent::TargetDestroyed(event),
                ));
            } else {
                tracing::error!(
                    target_id = closed_target_id,
                    "exact Target close lost its frozen lifecycle snapshot"
                );
            }
        } else if target_destroyed_lifecycle.is_none() {
            tracing::error!(
                target_id = closed_target_id,
                "exact Target close lost its frozen lifecycle snapshot"
            );
        }
    } else {
        tracing::error!(
            target_id = closed_target_id,
            "suppressing Target close events without an exact Browser fact"
        );
    }
    conn.release_idle_navigation_engine_memory_after_target_close();
    crate::conn::CdpTurnOutcome::new_with_protocol_events(out, conn.take_scheduler_events())
}
