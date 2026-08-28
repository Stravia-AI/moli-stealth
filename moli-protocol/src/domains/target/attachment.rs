use serde::Deserialize;
use serde_json::json;

use crate::conn::{BackgroundProtocolEvent, PreparedTargetAttach};

use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachParams {
    target_id: String,
}

fn target_command_error_without_session(
    code: i32,
    message: impl Into<String>,
) -> TargetCommandTaskStep {
    TargetCommandTaskStep::Complete(CommandOutputPlan::error_without_session(code, message))
}

fn attach_session_events_then_result_plan(
    session_id: String,
    events: Vec<BackgroundProtocolEvent>,
) -> CommandOutputPlan {
    // Puppeteer creates and registers its CdpCDPSession while handling
    // Target.attachedToTarget. Once the Target.attachToTarget response resolves,
    // it immediately looks that session up by the returned sessionId and fails
    // with "CDPSession creation failed" if the event has not arrived yet.
    //
    // Chromium emits the synchronous attachedToTarget event before completing
    // the command response. Preserve that observable wire order here instead of
    // applying the usual response-before-background-events convention.
    let mut plan = CommandOutputPlan::default();
    for event in events {
        plan.push_background_event(event);
    }
    plan.push_result(json!({ "sessionId": session_id }));
    plan
}

fn attach_session_events_then_result_step(
    session_id: String,
    events: Vec<BackgroundProtocolEvent>,
) -> TargetCommandTaskStep {
    let plan = attach_session_events_then_result_plan(session_id, events);
    TargetCommandTaskStep::Complete(plan)
}

#[derive(Default)]
struct TargetCommandOutput {
    plan: CommandOutputPlan,
    side_effects: events::TargetProtocolSideEffects,
}

impl TargetCommandOutput {
    fn background_events_mut(&mut self) -> &mut Vec<BackgroundProtocolEvent> {
        self.side_effects.background_events_mut()
    }

    fn target_events_mut(&mut self) -> &mut events::TargetProtocolSideEffects {
        &mut self.side_effects
    }

    fn push_success(&mut self) {
        self.flush_side_effects();
        self.plan.push_success();
    }

    fn push_error(&mut self, code: i32, message: impl Into<String>) {
        self.flush_side_effects();
        self.plan.push_error_without_session(code, message);
    }

    fn insert_renderer_output_boundary(&mut self, cursor: moli_core::RendererOutputFence) {
        self.flush_side_effects();
        self.plan.insert_renderer_output_boundary(cursor);
    }

    fn into_plan(mut self) -> CommandOutputPlan {
        self.flush_side_effects();
        self.plan
    }

    fn flush_side_effects(&mut self) {
        self.plan.extend(self.side_effects.drain_into_plan());
    }
}

pub(super) fn start_attach_to_target_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> TargetCommandTaskStep {
    let params: AttachParams = match cmd.get_params() {
        Ok(Some(p)) => p,
        _ => {
            return target_command_error_without_session(-32602, "InvalidParams");
        }
    };
    do_attach(conn, cmd, &params.target_id)
}

pub(super) fn attach_to_browser_target_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> CommandOutputPlan {
    if cmd.session_id.is_some() && !conn.is_browser_session_id(cmd.session_id) {
        return CommandOutputPlan::error(-32000, "Not allowed");
    }
    attach_browser_target(conn, cmd.session_id)
}

fn attach_browser_target(
    conn: &mut CdpConnection,
    owner_session_id: Option<&str>,
) -> CommandOutputPlan {
    let session_id = conn.gen_session_id();
    let target_info = super::browser_context::devtools_browser_target_info();
    let mut plan = CommandOutputPlan::default();
    for event in conn.commit_browser_attached_session_event_plan(
        session_id.clone(),
        owner_session_id,
        super::browser_context::DEVTOOLS_BROWSER_TARGET_ID,
        target_info,
    ) {
        plan.push_background_event(event);
    }
    plan.push_result(json!({ "sessionId": session_id }));
    plan
}

fn do_attach(conn: &mut CdpConnection, cmd: &Cmd<'_>, target_id: &str) -> TargetCommandTaskStep {
    if target_id == super::browser_context::DEVTOOLS_BROWSER_TARGET_ID {
        // Chromium applies its browser access-mode check to the specialized
        // AttachToBrowserTarget action, not to generic attachment of a known host.
        return TargetCommandTaskStep::Complete(attach_browser_target(conn, cmd.session_id));
    }
    let attach_from_browser_session = conn.is_browser_session_id(cmd.session_id);
    let browser_snapshot = match conn.capture_browser_top_level_target_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return TargetCommandTaskStep::Complete(CommandOutputPlan::from_devtools_error(error));
        }
    };
    if let Some(page_target_id) = conn
        .page_target_id_for_tab_target_id(target_id)
        .map(str::to_owned)
    {
        let Some(page_snapshot) = browser_snapshot.target(&page_target_id) else {
            return target_command_error_without_session(-31998, "UnknownTargetId");
        };
        if let Err(error) = conn.project_top_level_target_snapshot(page_snapshot) {
            return TargetCommandTaskStep::Complete(CommandOutputPlan::from_devtools_error(error));
        }
        return do_attach_tab_target(conn, cmd.session_id, target_id, attach_from_browser_session);
    }
    if browser_snapshot.contexts().is_empty() {
        return target_command_error_without_session(-31998, "BrowserContextNotLoaded");
    }
    let any_target = browser_snapshot
        .contexts()
        .iter()
        .any(|context| !context.targets().is_empty())
        || conn.browser_contexts().any(|context| {
            context.has_any_shared_worker_targets()
                || context.has_any_dedicated_worker_targets()
                || context.has_any_service_worker_targets()
        });
    if !any_target {
        return target_command_error_without_session(-31998, "TargetNotLoaded");
    }
    let top_level_target_snapshot = browser_snapshot.target(target_id).cloned();
    let restore_browser_context_id =
        attach_from_browser_session.then(|| previously_active_browser_context_id(conn));
    let selected = if let Some(snapshot) = top_level_target_snapshot.as_ref() {
        conn.activate_browser_context_by_id(snapshot.browser_context_id())
    } else {
        select_browser_context_for_target(conn, target_id).is_ok()
    };
    if !selected {
        if let Some(restore_browser_context_id) = restore_browser_context_id.as_ref() {
            restore_previously_active_browser_context(conn, restore_browser_context_id.as_deref());
        }
        return target_command_error_without_session(-31998, "UnknownTargetId");
    }
    let bc = match conn.browser_context.as_ref() {
        Some(bc) => bc,
        None => {
            if let Some(restore_browser_context_id) = restore_browser_context_id.as_ref() {
                restore_previously_active_browser_context(
                    conn,
                    restore_browser_context_id.as_deref(),
                );
            }
            return target_command_error_without_session(-31998, "BrowserContextNotLoaded");
        }
    };
    if bc.has_shared_worker_target(target_id) {
        return do_attach_shared_worker_target(conn, cmd.session_id, target_id);
    }
    if bc.has_dedicated_worker_target(target_id) {
        return do_attach_dedicated_worker_target(conn, cmd.session_id, target_id);
    }
    if bc.has_service_worker_target(target_id) {
        return do_attach_service_worker_target(conn, cmd.session_id, target_id);
    }
    let Some(target_snapshot) = top_level_target_snapshot else {
        if let Some(restore_browser_context_id) = restore_browser_context_id.as_ref() {
            restore_previously_active_browser_context(conn, restore_browser_context_id.as_deref());
        }
        return target_command_error_without_session(-31998, "UnknownTargetId");
    };
    if let Err(error) = conn.project_top_level_target_snapshot(&target_snapshot) {
        if let Some(restore_browser_context_id) = restore_browser_context_id.as_ref() {
            restore_previously_active_browser_context(conn, restore_browser_context_id.as_deref());
        }
        return TargetCommandTaskStep::Complete(CommandOutputPlan::from_devtools_error(error));
    }
    if !bc.has_active_target() && bc.background_targets.is_empty() {
        if let Some(restore_browser_context_id) = restore_browser_context_id.as_ref() {
            restore_previously_active_browser_context(conn, restore_browser_context_id.as_deref());
        }
        return target_command_error_without_session(-31998, "TargetNotLoaded");
    }
    let active_target_identity = bc.active_target_identity();
    let target_has_primary_session = if matches!(
        active_target_identity,
        Some((ref active_target_id, _)) if active_target_id == target_id
    ) {
        active_target_identity
            .as_ref()
            .is_some_and(|(_, session_id)| session_id.is_some())
    } else if bc.background_target(target_id).is_some() {
        bc.primary_session_id_for_target(target_id).is_some()
    } else {
        if let Some(restore_browser_context_id) = restore_browser_context_id.as_ref() {
            restore_previously_active_browser_context(conn, restore_browser_context_id.as_deref());
        }
        return target_command_error_without_session(-31998, "UnknownTargetId");
    };

    let session_id = conn.gen_session_id();
    let bc = conn.browser_context.as_mut().unwrap();
    let assigned = if attach_from_browser_session || target_has_primary_session {
        bc.assign_auxiliary_session_to_target(target_id, session_id.clone())
    } else {
        bc.assign_session_to_target(target_id, session_id.clone())
    };
    if !assigned {
        if let Some(restore_browser_context_id) = restore_browser_context_id.as_ref() {
            restore_previously_active_browser_context(conn, restore_browser_context_id.as_deref());
        }
        return target_command_error_without_session(-31998, "UnknownTargetId");
    }

    let initial_document =
        match conn.start_initial_document_page_ensure_for_session_owner(Some(&session_id)) {
            Ok(pending) => pending.map(Box::new),
            Err(message) => {
                if let Some(restore_browser_context_id) = restore_browser_context_id.as_ref() {
                    restore_previously_active_browser_context(
                        conn,
                        restore_browser_context_id.as_deref(),
                    );
                }
                return target_command_error_without_session(-32000, message);
            }
        };
    if let Some(restore_browser_context_id) = restore_browser_context_id.as_ref() {
        restore_previously_active_browser_context(conn, restore_browser_context_id.as_deref());
    }
    TargetCommandTaskStep::Pending(PendingTargetCommandDispatch {
        command_id: cmd.id,
        session_id: cmd.session_id.map(str::to_owned),
        kind: Box::new(PendingTargetCommandKind::AttachToTarget {
            attached_session_id: session_id,
            target_snapshot,
            initial_document,
        }),
    })
}

fn do_attach_tab_target(
    conn: &mut CdpConnection,
    command_session_id: Option<&str>,
    tab_target_id: &str,
    attach_from_browser_session: bool,
) -> TargetCommandTaskStep {
    let attach_as_auxiliary = attach_from_browser_session
        || conn
            .primary_session_id_for_tab_target_id(tab_target_id)
            .is_some();
    let session_id = conn.gen_session_id();
    let event_plan = match conn.attach_tab_target_session_event_plan(
        session_id.clone(),
        command_session_id,
        tab_target_id,
        attach_as_auxiliary,
    ) {
        Ok(event_plan) => event_plan,
        Err(message) => return target_command_error_without_session(-31998, message),
    };
    attach_session_events_then_result_step(session_id, event_plan.into_background_events())
}

pub(super) async fn complete_attach_to_target_command_async(
    conn: &mut CdpConnection,
    command_session_id: Option<&str>,
    attached_session_id: String,
    target_snapshot: moli_core::browser_host::BrowserTargetStateSnapshot,
    initial_document: Option<
        Result<
            Box<crate::conn::CompletedInitialDocumentPageBuild>,
            crate::conn::FailedInitialDocumentPageBuild,
        >,
    >,
) -> CommandOutputPlan {
    let prepared_session = conn.prepare_direct_attach_session_commit(
        attached_session_id.clone(),
        command_session_id.map(str::to_owned),
        false,
    );
    if !conn.browser_target_state_snapshot_is_current(&target_snapshot) {
        conn.rollback_prepared_attach_session_without_event_async(&prepared_session)
            .await;
        return CommandOutputPlan::error_without_session(-31998, "UnknownTargetId");
    }
    match initial_document {
        Some(Ok(completed_initial_document)) => {
            let completed_initial_document = *completed_initial_document;
            if let Err(message) = conn
                .complete_initial_document_page_build_for_owner(completed_initial_document)
                .await
            {
                conn.rollback_prepared_attach_session_without_event_async(&prepared_session)
                    .await;
                return CommandOutputPlan::error_without_session(-32000, message);
            }
        }
        Some(Err(failed)) => {
            let message = conn.reset_failed_initial_document_page_build_for_owner(failed);
            conn.rollback_prepared_attach_session_without_event_async(&prepared_session)
                .await;
            return CommandOutputPlan::error_without_session(-32000, message);
        }
        None => {}
    }
    if let Err(message) = conn
        .apply_runtime_binding_state_for_session_owner_async(Some(&attached_session_id))
        .await
        && message != "NoDocumentLoaded"
    {
        tracing::warn!(
            %message,
            session_id = attached_session_id.as_str(),
            "target attach renderer binding state apply failed"
        );
    }
    if !conn.browser_target_state_snapshot_is_current(&target_snapshot) {
        conn.rollback_prepared_attach_session_without_event_async(&prepared_session)
            .await;
        return CommandOutputPlan::error_without_session(-31998, "UnknownTargetId");
    }
    let target_info = match conn.project_top_level_target_snapshot(&target_snapshot) {
        Ok(target_info) => target_info,
        Err(error) => {
            conn.rollback_prepared_attach_session_without_event_async(&prepared_session)
                .await;
            return CommandOutputPlan::from_devtools_error(error);
        }
    };
    if let Some(message) = super::transient_no_page_devtools_target_info_error(conn, &target_info) {
        conn.rollback_prepared_attach_session_without_event_async(&prepared_session)
            .await;
        return CommandOutputPlan::error_without_session(-32000, message);
    }
    let attached_target_id = target_snapshot.target_id().to_owned();
    let event_plan = conn.commit_prepared_attach_event_plan(PreparedTargetAttach::new(
        &attached_target_id,
        target_info,
        [prepared_session],
    ));
    attach_session_events_then_result_plan(attached_session_id, event_plan.into_background_events())
}

fn do_attach_shared_worker_target(
    conn: &mut CdpConnection,
    command_session_id: Option<&str>,
    target_id: &str,
) -> TargetCommandTaskStep {
    let session_id = conn.gen_session_id();
    let event_plan = match conn.attach_shared_worker_target_session_event_plan(
        session_id.clone(),
        command_session_id,
        target_id,
    ) {
        Ok(event_plan) => event_plan,
        Err(message) => return target_command_error_without_session(-31998, message),
    };
    attach_session_events_then_result_step(session_id, event_plan.into_background_events())
}

fn do_attach_service_worker_target(
    conn: &mut CdpConnection,
    command_session_id: Option<&str>,
    target_id: &str,
) -> TargetCommandTaskStep {
    let session_id = conn.gen_session_id();
    let event_plan = match conn.attach_service_worker_target_session_event_plan(
        session_id.clone(),
        command_session_id,
        target_id,
    ) {
        Ok(event_plan) => event_plan,
        Err(message) => return target_command_error_without_session(-31998, message),
    };
    attach_session_events_then_result_step(session_id, event_plan.into_background_events())
}

fn do_attach_dedicated_worker_target(
    conn: &mut CdpConnection,
    command_session_id: Option<&str>,
    target_id: &str,
) -> TargetCommandTaskStep {
    let session_id = conn.gen_session_id();
    let event_plan = match conn.attach_dedicated_worker_target_session_event_plan(
        session_id.clone(),
        command_session_id,
        target_id,
    ) {
        Ok(event_plan) => event_plan,
        Err(message) => return target_command_error_without_session(-31998, message),
    };
    attach_session_events_then_result_step(session_id, event_plan.into_background_events())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageParams {
    message: String,
    session_id: Option<String>,
}

pub(super) fn start_send_message_to_target_command(cmd: &Cmd<'_>) -> TargetCommandTaskStep {
    let params: SendMessageParams = match cmd.get_params() {
        Ok(Some(p)) => p,
        _ => {
            return super::target_command_error(-32602, "InvalidParams");
        }
    };
    pending_send_message_to_target_command(
        cmd.id,
        cmd.session_id,
        params.message,
        params.session_id,
    )
}

pub(super) struct PendingSendMessageToTargetCommand {
    pending: Box<crate::conn::PendingCdpCommandDispatch>,
    nested_command_context: crate::conn::CommandDispatchContext,
    nested_output_session_id: Option<String>,
    target_session_id: Option<String>,
    restore_browser_context_id: Option<String>,
}

pub(super) struct CompletedSendMessageToTargetCommand {
    completed: Box<crate::conn::CompletedCdpCommandDispatch>,
    nested_command_context: crate::conn::CommandDispatchContext,
    nested_output_session_id: Option<String>,
    target_session_id: Option<String>,
    restore_browser_context_id: Option<String>,
}

impl PendingSendMessageToTargetCommand {
    pub(super) async fn wait(self) -> CompletedSendMessageToTargetCommand {
        CompletedSendMessageToTargetCommand {
            completed: Box::new(self.pending.wait().await),
            nested_command_context: self.nested_command_context,
            nested_output_session_id: self.nested_output_session_id,
            target_session_id: self.target_session_id,
            restore_browser_context_id: self.restore_browser_context_id,
        }
    }
}

pub(super) async fn complete_send_message_to_target_command_start_async(
    conn: &mut CdpConnection,
    outer_command_id: Option<u64>,
    outer_session_id: Option<&str>,
    command_context: &mut crate::conn::CommandDispatchContext,
    message: String,
    target_session_id: Option<String>,
) -> TargetCommandTaskStep {
    let restore_browser_context_id = previously_active_browser_context_id(conn);
    let mut out = TargetCommandOutput::default();
    if !select_send_message_to_target_owner(conn, &mut out, target_session_id.as_deref()) {
        restore_previously_active_browser_context(conn, restore_browser_context_id.as_deref());
        return TargetCommandTaskStep::Complete(out.into_plan());
    }

    let mut nested_command_context = crate::conn::CommandDispatchContext::default();
    nested_command_context.set_terminal_response_delivery_override(
        moli_page_types::RendererInspectorResponseDelivery::CommandReply,
    );
    let (nested_output_session_id, step) =
        conn.start_raw_command_dispatch_with_context(message, &mut nested_command_context);
    advance_send_message_to_target_command(
        conn,
        outer_command_id,
        outer_session_id,
        command_context,
        nested_command_context,
        nested_output_session_id,
        target_session_id,
        restore_browser_context_id,
        step,
    )
    .await
}

pub(super) async fn complete_send_message_to_target_command_continuation_async(
    conn: &mut CdpConnection,
    outer_command_id: Option<u64>,
    outer_session_id: Option<&str>,
    command_context: &mut crate::conn::CommandDispatchContext,
    continuation: CompletedSendMessageToTargetCommand,
) -> TargetCommandTaskStep {
    let CompletedSendMessageToTargetCommand {
        completed,
        mut nested_command_context,
        nested_output_session_id,
        target_session_id,
        restore_browser_context_id,
    } = continuation;
    let step =
        Box::pin(conn.complete_pending_command_dispatch_with_context(
            *completed,
            &mut nested_command_context,
        ))
        .await;
    advance_send_message_to_target_command(
        conn,
        outer_command_id,
        outer_session_id,
        command_context,
        nested_command_context,
        nested_output_session_id,
        target_session_id,
        restore_browser_context_id,
        step,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn advance_send_message_to_target_command(
    conn: &mut CdpConnection,
    outer_command_id: Option<u64>,
    outer_session_id: Option<&str>,
    command_context: &mut crate::conn::CommandDispatchContext,
    nested_command_context: crate::conn::CommandDispatchContext,
    nested_output_session_id: Option<String>,
    target_session_id: Option<String>,
    restore_browser_context_id: Option<String>,
    step: crate::conn::CdpCommandTaskStep,
) -> TargetCommandTaskStep {
    let outcome = match step {
        crate::conn::CdpCommandTaskStep::Pending(mut pending) => {
            // Publish nested scheduler work before yielding the outer Target
            // command. Most importantly, Browser Host may now select a nested
            // Page.navigate while this exact participant waits.
            conn.extend_scheduler_events(pending.take_scheduler_events());
            return super::pending_send_message_to_target_continuation(
                outer_command_id,
                outer_session_id,
                PendingSendMessageToTargetCommand {
                    pending,
                    nested_command_context,
                    nested_output_session_id,
                    target_session_id,
                    restore_browser_context_id,
                },
            );
        }
        crate::conn::CdpCommandTaskStep::Complete(outcome) => outcome,
    };
    let nested_outcome = conn
        .settle_direct_command_turn_outcome(
            nested_output_session_id.as_deref(),
            nested_command_context,
            Vec::new(),
            outcome,
        )
        .await;
    let mut out = TargetCommandOutput::default();
    append_send_message_to_target_outcome(
        conn,
        command_context,
        &mut out,
        target_session_id.as_deref(),
        nested_outcome,
    );
    restore_previously_active_browser_context(conn, restore_browser_context_id.as_deref());
    TargetCommandTaskStep::Complete(out.into_plan())
}

fn select_send_message_to_target_owner(
    conn: &mut CdpConnection,
    out: &mut TargetCommandOutput,
    target_session_id: Option<&str>,
) -> bool {
    if let Some(session_id) = target_session_id
        && !conn.activate_browser_context_for_session(session_id)
    {
        out.push_error(-31998, "InvalidSessionId");
        return false;
    }
    let Some(bc) = conn.browser_context.as_ref() else {
        out.push_error(-31998, "BrowserContextNotLoaded");
        return false;
    };
    let Some((_, current_session_id)) = bc.active_target_identity() else {
        out.push_error(-31998, "TargetNotLoaded");
        return false;
    };
    if current_session_id.as_deref() != target_session_id {
        out.push_error(-31998, "InvalidSessionId");
        return false;
    }
    true
}

fn append_send_message_to_target_outcome(
    conn: &mut CdpConnection,
    command_context: &mut crate::conn::CommandDispatchContext,
    out: &mut TargetCommandOutput,
    target_session_id: Option<&str>,
    nested_outcome: crate::conn::CdpRendererOwnerTurnOutcome,
) {
    let (
        nested_events,
        post_renderer_output_events,
        renderer_output_boundary,
        post_response_events,
        nested_scheduler_events,
        renderer_output_predecessor,
    ) = nested_outcome.into_renderer_owner_turn_parts();
    if let Some(predecessor) = renderer_output_predecessor {
        command_context.set_renderer_output_predecessor(predecessor);
    }
    conn.extend_scheduler_events(nested_scheduler_events);
    out.push_success();
    if let Some(session_id) = target_session_id {
        for nested in nested_events {
            out.background_events_mut().push(
                BackgroundProtocolEvent::target_received_message_from_target(session_id, nested),
            );
        }
        if let Some(renderer_output_boundary) = renderer_output_boundary {
            out.insert_renderer_output_boundary(renderer_output_boundary);
        } else {
            assert!(
                post_renderer_output_events.is_empty(),
                "nested post-renderer output requires an exact boundary"
            );
        }
        for nested in post_renderer_output_events
            .into_iter()
            .chain(post_response_events)
        {
            out.background_events_mut().push(
                BackgroundProtocolEvent::target_received_message_from_target(session_id, nested),
            );
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DetachParams {
    target_id: Option<String>,
    session_id: Option<String>,
}

pub(super) fn start_detach_from_target_command(cmd: &Cmd<'_>) -> TargetCommandTaskStep {
    let params: DetachParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        Ok(None) => DetachParams {
            target_id: None,
            session_id: None,
        },
        Err(e) => {
            return super::target_command_error(-32602, e);
        }
    };
    pending_detach_from_target_command(cmd.id, cmd.session_id, params.target_id, params.session_id)
}

pub(super) async fn complete_detach_from_target_command_async(
    conn: &mut CdpConnection,
    command_session_id: Option<&str>,
    target_id: Option<String>,
    detach_session_id: Option<String>,
    command_context: &mut crate::conn::CommandDispatchContext,
) -> CommandOutputPlan {
    let previously_active_browser_context_id = previously_active_browser_context_id(conn);
    let mut out = TargetCommandOutput::default();
    detach_from_target_inner_async(
        conn,
        &mut out,
        command_session_id,
        command_context,
        DetachParams {
            target_id,
            session_id: detach_session_id,
        },
    )
    .await;
    restore_previously_active_browser_context(
        conn,
        previously_active_browser_context_id.as_deref(),
    );
    out.into_plan()
}

async fn detach_from_target_inner_async(
    conn: &mut CdpConnection,
    out: &mut TargetCommandOutput,
    command_session_id: Option<&str>,
    command_context: &mut crate::conn::CommandDispatchContext,
    params: DetachParams,
) {
    if let Some(session_id) = params.session_id.as_deref() {
        let Some(route) = conn.session_route(Some(session_id)) else {
            out.push_error(-31998, "InvalidSessionId");
            return;
        };
        if let Some(browser_context_id) = route.browser_context_id()
            && !conn.activate_browser_context_by_id(browser_context_id)
        {
            out.push_error(-31998, "InvalidSessionId");
            return;
        }
        if let Some(requested_target_id) = params.target_id.as_deref()
            && detach_session_route_target_id(&route)
                .is_some_and(|target_id| target_id != requested_target_id)
        {
            out.push_error(-31998, "UnknownTargetId");
            return;
        }
    }
    if let Some(target_id) = params.target_id.as_deref()
        && let Err(message) = select_browser_context_for_target(conn, target_id)
    {
        out.push_error(-31998, message);
        return;
    }

    if let Some(session_id) = params.session_id.as_deref() {
        super::lifecycle::detach_attached_sessions_for_owner_async(
            conn,
            out.target_events_mut(),
            Some(session_id),
            command_context,
        )
        .await;
    }

    if let Some(session_id) = params.session_id.as_deref()
        && conn.is_browser_session_id(Some(session_id))
    {
        conn.cancel_tracing_for_session_owner_async(Some(session_id))
            .await;
        let detached = conn.detach_browser_session_owner_event_plan(session_id);
        debug_assert!(detached.is_some());
        if let Some(detached) = detached {
            out.target_events_mut().extend_background_events(detached);
        }
        out.push_success();
        return;
    }
    if conn.browser_context.is_none() {
        out.push_success();
        return;
    }
    if let Some(session_id) = params.session_id.as_deref()
        && let Some(tab_target_id) = conn
            .tab_target_id_for_session_id(session_id)
            .map(str::to_owned)
    {
        if let Some(requested_target_id) = params.target_id.as_deref()
            && requested_target_id != tab_target_id
        {
            out.push_error(-31998, "UnknownTargetId");
            return;
        }
        out.push_success();
        let event_plan = conn
            .detach_session_with_binding_cleanup_event_plan_async(
                crate::conn::TargetSessionDetachCleanupPlan::new(
                    tab_target_id,
                    session_id,
                    None,
                    command_session_id,
                ),
            )
            .await;
        out.target_events_mut().extend_background_events(event_plan);
        return;
    }
    if let Some(session_id) = params.session_id.as_deref()
        && let Some(target_id) = conn
            .browser_context
            .as_ref()
            .and_then(|bc| bc.auxiliary_target_id_for_session(session_id))
            .map(str::to_owned)
    {
        if let Some(requested_target_id) = params.target_id.as_deref()
            && requested_target_id != target_id
        {
            out.push_error(-31998, "UnknownTargetId");
            return;
        }
        conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
            out.background_events_mut(),
            command_context.protocol_events_mut(),
            Some(session_id),
            "Target detached",
        );
        let _ = conn
            .detach_runtime_inspector_session_for_session_owner_async(Some(session_id))
            .await;
        super::lifecycle::clear_emulated_media_for_detached_session_best_effort(conn, session_id)
            .await;
        super::clear_detached_target_fetch_state_background_events_async(
            conn,
            out.background_events_mut(),
            session_id,
        )
        .await;
        out.push_success();
        let event_plan = conn
            .detach_session_with_binding_cleanup_event_plan_async(
                crate::conn::TargetSessionDetachCleanupPlan::new(
                    target_id,
                    session_id,
                    None,
                    command_session_id,
                ),
            )
            .await;
        out.target_events_mut().extend_background_events(event_plan);
        return;
    }
    if let Some(session_id) = params.session_id.as_deref()
        && let Some(target_id) = conn
            .browser_context
            .as_ref()
            .and_then(|bc| bc.dedicated_worker_target_id_for_session(session_id))
            .map(str::to_owned)
    {
        if let Some(requested_target_id) = params.target_id.as_deref()
            && requested_target_id != target_id
        {
            out.push_error(-31998, "UnknownTargetId");
            return;
        }
        let renderer_detach = conn.browser_context.as_ref().and_then(|bc| {
            bc.dedicated_worker_target(&target_id)
                .map(|target| (bc.renderer_runtime(), target.renderer_instance_id))
        });
        conn.release_shared_worker_runtime_remote_objects_for_session_best_effort_async(session_id)
            .await;
        out.push_success();
        conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
            out.background_events_mut(),
            command_context.protocol_events_mut(),
            Some(session_id),
            "Target detached",
        );
        if let Some((renderer_runtime, instance_id)) = renderer_detach {
            renderer_runtime.detach_dedicated_worker_runtime_inspector_session(
                instance_id,
                Some(session_id.to_owned()),
            );
        }
        let event_plan = conn
            .detach_session_with_binding_cleanup_event_plan_async(
                crate::conn::TargetSessionDetachCleanupPlan::new(target_id, session_id, None, None),
            )
            .await;
        out.target_events_mut().extend_background_events(event_plan);
        return;
    }
    if let Some(session_id) = params.session_id.as_deref()
        && let Some(target_id) = conn
            .browser_context
            .as_ref()
            .and_then(|bc| bc.shared_worker_target_id_for_session(session_id))
            .map(str::to_owned)
    {
        if let Some(requested_target_id) = params.target_id.as_deref()
            && requested_target_id != target_id
        {
            out.push_error(-31998, "UnknownTargetId");
            return;
        }
        let renderer_detach = conn.browser_context.as_ref().and_then(|bc| {
            bc.shared_worker_target(&target_id)
                .map(|target| (bc.renderer_runtime(), target.renderer_instance_id))
        });
        conn.release_shared_worker_runtime_remote_objects_for_session_best_effort_async(session_id)
            .await;
        out.push_success();
        conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
            out.background_events_mut(),
            command_context.protocol_events_mut(),
            Some(session_id),
            "Target detached",
        );
        if let Some((renderer_runtime, instance_id)) = renderer_detach {
            renderer_runtime.detach_shared_worker_runtime_inspector_session(
                instance_id,
                Some(session_id.to_owned()),
            );
        }
        let event_plan = conn
            .detach_session_with_binding_cleanup_event_plan_async(
                crate::conn::TargetSessionDetachCleanupPlan::new(target_id, session_id, None, None),
            )
            .await;
        out.target_events_mut().extend_background_events(event_plan);
        return;
    }
    if let Some(session_id) = params.session_id.as_deref()
        && let Some(target_id) = conn
            .browser_context
            .as_ref()
            .and_then(|bc| bc.service_worker_target_id_for_session(session_id))
            .map(str::to_owned)
    {
        if let Some(requested_target_id) = params.target_id.as_deref()
            && requested_target_id != target_id
        {
            out.push_error(-31998, "UnknownTargetId");
            return;
        }
        out.push_success();
        conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
            out.background_events_mut(),
            command_context.protocol_events_mut(),
            Some(session_id),
            "Target detached",
        );
        super::set_service_worker_pause_on_start_owner(conn, Some(session_id), false);
        let event_plan = conn
            .detach_session_with_binding_cleanup_event_plan_async(
                crate::conn::TargetSessionDetachCleanupPlan::new(target_id, session_id, None, None),
            )
            .await;
        out.target_events_mut().extend_background_events(event_plan);
        return;
    }
    if let Some(session_id) = params.session_id.as_deref() {
        let background_target_id = conn.browser_context.as_ref().and_then(|bc| {
            bc.primary_attachment_target_id_for_session(session_id)
                .filter(|target_id| !bc.is_active_target(target_id))
                .map(str::to_owned)
        });
        if let Some(target_id) = background_target_id.as_deref() {
            if let Some(requested_target_id) = params.target_id.as_deref()
                && requested_target_id != target_id
            {
                out.push_error(-31998, "UnknownTargetId");
                return;
            }
            conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
                out.background_events_mut(),
                command_context.protocol_events_mut(),
                Some(session_id),
                "Target detached",
            );
            let _ = conn
                .detach_runtime_inspector_session_for_session_owner_async(Some(session_id))
                .await;
            match conn
                .detach_background_target_session_binding_event_plan_async(
                    crate::conn::TargetSessionDetachCleanupPlan::new(
                        target_id,
                        session_id,
                        None,
                        command_session_id,
                    ),
                )
                .await
            {
                Ok(Some(event_plan)) => {
                    out.push_success();
                    out.target_events_mut().extend_background_events(event_plan);
                    return;
                }
                Ok(None) => {}
                Err(message) => {
                    out.push_error(-32000, message);
                    return;
                }
            }
        }
    }
    if let Some(target_id) = params.target_id.as_deref() {
        let shared_worker_detach_plan = conn.browser_context.as_ref().and_then(|bc| {
            bc.shared_worker_target(target_id).map(|target| {
                (
                    bc.renderer_runtime(),
                    target.renderer_instance_id,
                    target.session_ids(),
                )
            })
        });
        if conn
            .browser_context
            .as_ref()
            .is_some_and(|bc| bc.has_shared_worker_target(target_id))
        {
            out.push_success();
            for session_id in shared_worker_detach_plan
                .as_ref()
                .map(|(_, _, sessions)| sessions.as_slice())
                .unwrap_or_default()
            {
                conn.release_shared_worker_runtime_remote_objects_for_session_best_effort_async(
                    session_id,
                )
                .await;
                conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
                    out.background_events_mut(),
                    command_context.protocol_events_mut(),
                    Some(session_id),
                    "Target detached",
                );
            }
            if let Some((renderer_runtime, instance_id, session_ids)) = &shared_worker_detach_plan {
                for session_id in session_ids {
                    renderer_runtime.detach_shared_worker_runtime_inspector_session(
                        *instance_id,
                        Some(session_id.clone()),
                    );
                }
            }
            let detached_sessions = shared_worker_detach_plan
                .as_ref()
                .map(|(_, _, sessions)| sessions.clone())
                .unwrap_or_default();
            if !detached_sessions.is_empty() {
                let event_plan = conn
                    .detach_target_sessions_with_binding_cleanup_event_plan_async(
                        crate::conn::TargetClosureCleanupPlan::new(
                            target_id,
                            None,
                            detached_sessions,
                        ),
                        None,
                    )
                    .await;
                out.target_events_mut().extend_background_events(event_plan);
            }
            return;
        }
        let dedicated_worker_detach_plan = conn.browser_context.as_ref().and_then(|bc| {
            bc.dedicated_worker_target(target_id).map(|target| {
                (
                    bc.renderer_runtime(),
                    target.renderer_instance_id,
                    target.session_ids(),
                )
            })
        });
        if conn
            .browser_context
            .as_ref()
            .is_some_and(|bc| bc.has_dedicated_worker_target(target_id))
        {
            out.push_success();
            for session_id in dedicated_worker_detach_plan
                .as_ref()
                .map(|(_, _, sessions)| sessions.as_slice())
                .unwrap_or_default()
            {
                conn.release_shared_worker_runtime_remote_objects_for_session_best_effort_async(
                    session_id,
                )
                .await;
                conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
                    out.background_events_mut(),
                    command_context.protocol_events_mut(),
                    Some(session_id),
                    "Target detached",
                );
            }
            if let Some((renderer_runtime, instance_id, session_ids)) =
                &dedicated_worker_detach_plan
            {
                for session_id in session_ids {
                    renderer_runtime.detach_dedicated_worker_runtime_inspector_session(
                        *instance_id,
                        Some(session_id.clone()),
                    );
                }
            }
            let detached_sessions = dedicated_worker_detach_plan
                .as_ref()
                .map(|(_, _, sessions)| sessions.clone())
                .unwrap_or_default();
            if !detached_sessions.is_empty() {
                let event_plan = conn
                    .detach_target_sessions_with_binding_cleanup_event_plan_async(
                        crate::conn::TargetClosureCleanupPlan::new(
                            target_id,
                            None,
                            detached_sessions,
                        ),
                        None,
                    )
                    .await;
                out.target_events_mut().extend_background_events(event_plan);
            }
            return;
        }
        if conn
            .browser_context
            .as_ref()
            .is_some_and(|bc| bc.has_service_worker_target(target_id))
        {
            out.push_success();
            let service_worker_session_ids: Vec<String> = conn
                .browser_context
                .as_ref()
                .and_then(|bc| bc.service_worker_target(target_id))
                .map(|target| target.session_ids())
                .unwrap_or_default();
            for session_id in &service_worker_session_ids {
                conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
                    out.background_events_mut(),
                    command_context.protocol_events_mut(),
                    Some(session_id.as_str()),
                    "Target detached",
                );
                super::set_service_worker_pause_on_start_owner(
                    conn,
                    Some(session_id.as_str()),
                    false,
                );
            }
            if !service_worker_session_ids.is_empty() {
                let event_plan = conn
                    .detach_target_sessions_with_binding_cleanup_event_plan_async(
                        crate::conn::TargetClosureCleanupPlan::new(
                            target_id,
                            None,
                            service_worker_session_ids,
                        ),
                        None,
                    )
                    .await;
                out.target_events_mut().extend_background_events(event_plan);
            }
            return;
        }
    }
    let Some(bc) = conn.browser_context.as_mut() else {
        out.push_success();
        return;
    };
    let Some((target_id, Some(current_session_id))) = bc.active_target_identity() else {
        out.push_success();
        return;
    };
    if let Some(session_id) = params.session_id.as_deref()
        && session_id != current_session_id
    {
        out.push_error(-31998, "InvalidSessionId");
        return;
    }
    if let Some(requested_target_id) = params.target_id.as_deref()
        && requested_target_id != target_id
    {
        out.push_error(-31998, "UnknownTargetId");
        return;
    }

    out.push_success();

    let (
        pending_navigations,
        pending_auth_navigations,
        pending_response_navigations,
        pending_subresource_fetches,
        pending_subresource_auths,
        pending_subresource_responses,
    ) = page::take_pending_fetch_state(conn, Some(&current_session_id));

    let renderer_output_predecessor = page::fail_pending_fetch_state_background_events_async(
        conn,
        out.background_events_mut(),
        Some(&current_session_id),
        "Target detached",
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

    conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
        out.background_events_mut(),
        command_context.protocol_events_mut(),
        Some(&current_session_id),
        "Target detached",
    );

    let _ = conn
        .detach_runtime_inspector_session_for_session_owner_async(Some(&current_session_id))
        .await;
    let event_plan = conn
        .detach_session_with_binding_cleanup_event_plan_async(
            crate::conn::TargetSessionDetachCleanupPlan::new(
                target_id,
                current_session_id,
                None,
                None,
            ),
        )
        .await;
    out.target_events_mut().extend_background_events(event_plan);
}

fn detach_session_route_target_id(route: &crate::conn::CdpSessionRoute) -> Option<&str> {
    match route {
        crate::conn::CdpSessionRoute::Browser => None,
        crate::conn::CdpSessionRoute::TabTarget { tab_target_id, .. } => Some(tab_target_id),
        crate::conn::CdpSessionRoute::ActiveTarget { target_id, .. } => target_id.as_deref(),
        crate::conn::CdpSessionRoute::AuxiliaryTarget { target_id, .. }
        | crate::conn::CdpSessionRoute::BackgroundTarget { target_id, .. }
        | crate::conn::CdpSessionRoute::SharedWorkerTarget { target_id, .. }
        | crate::conn::CdpSessionRoute::DedicatedWorkerTarget { target_id, .. }
        | crate::conn::CdpSessionRoute::ServiceWorkerTarget { target_id, .. } => Some(target_id),
    }
}
