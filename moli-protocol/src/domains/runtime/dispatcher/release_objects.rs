use std::collections::VecDeque;

use super::*;

/// Protocol-neutral `ReleaseObjects` state carried across one exact renderer
/// command participant at a time.
///
/// A single WebDriver/BiDi command may contain several handles. The outer
/// command remains one transaction, while each `Runtime.releaseObject` owns
/// its own renderer attachment, response correlation, protocol observations,
/// and output predecessor.
pub(super) struct DevToolsReleaseObjectsCommandDispatchState {
    remaining_handles: VecDeque<DevToolsRemoteHandleId>,
    current_object_id: Option<String>,
    protocol_events: Vec<BackgroundProtocolEvent>,
    renderer_output_predecessor: Option<moli_core::RendererOutputFence>,
}

pub(super) async fn start_devtools_release_objects_command_dispatch(
    conn: &mut CdpConnection,
    command: DevToolsReleaseObjectsCommand,
) -> DevToolsRuntimeCommandTaskStep {
    let command_context = command.context.clone();
    let target = match devtools_runtime_target_async(
        conn,
        &DevToolsCommand::ReleaseObjects(command.clone()),
    )
    .await
    {
        Ok(target) => target,
        Err(error) => {
            return conn
                .complete_devtools_runtime_direct_result(
                    command_context,
                    Err(error),
                    Vec::new(),
                    None,
                )
                .await;
        }
    };
    let target_realm = devtools_realm_id_for_runtime_target_async(conn, &target).await;
    let state = DevToolsRuntimeCommandDispatchState {
        internal_command_id: 0,
        command_context,
        result_kind: DevToolsRuntimeCommandResultKind::ReleaseObjects,
        target,
        target_realm,
        kind: DevToolsRuntimeCommandDispatchKind::ReleaseObjects(
            DevToolsReleaseObjectsCommandDispatchState {
                remaining_handles: command.handles.into(),
                current_object_id: None,
                protocol_events: Vec::new(),
                renderer_output_predecessor: None,
            },
        ),
    };
    continue_devtools_release_objects_command(conn, state, None).await
}

pub(super) async fn complete_devtools_release_objects_command_plan(
    conn: &mut CdpConnection,
    state: DevToolsRuntimeCommandDispatchState,
    plan: CommandOutputPlan,
) -> DevToolsRuntimeCommandTaskStep {
    continue_devtools_release_objects_command(conn, state, Some(plan)).await
}

async fn continue_devtools_release_objects_command(
    conn: &mut CdpConnection,
    mut state: DevToolsRuntimeCommandDispatchState,
    mut completed_plan: Option<CommandOutputPlan>,
) -> DevToolsRuntimeCommandTaskStep {
    loop {
        if let Some(plan) = completed_plan.take()
            && let Err(error) = apply_completed_release_object_plan(conn, &mut state, plan)
        {
            return finish_devtools_release_objects_command(conn, state, Err(error)).await;
        }

        let object_id = match next_release_object_id(conn, &mut state) {
            Ok(Some(object_id)) => object_id,
            Ok(None) => {
                return finish_devtools_release_objects_command(
                    conn,
                    state,
                    Ok(DevToolsCommandResult::Empty),
                )
                .await;
            }
            Err(error) => {
                return finish_devtools_release_objects_command(conn, state, Err(error)).await;
            }
        };
        let internal_command_id = conn.next_internal_runtime_command_id();
        state.internal_command_id = internal_command_id;
        let release_state = match &mut state.kind {
            DevToolsRuntimeCommandDispatchKind::ReleaseObjects(release_state) => release_state,
            DevToolsRuntimeCommandDispatchKind::Script { .. } => {
                return finish_devtools_release_objects_command(
                    conn,
                    state,
                    Err(DevToolsError::new(
                        DevToolsErrorKind::Internal,
                        "MissingReleaseObjectsDispatchState",
                    )),
                )
                .await;
            }
        };
        release_state.current_object_id = Some(object_id.clone());

        match start_protocol_neutral_release_object_command(
            conn,
            &state.target,
            object_id,
            internal_command_id,
        ) {
            RuntimeCommandTaskStep::Pending(pending) => {
                return DevToolsRuntimeCommandTaskStep::Pending(Box::new(
                    PendingDevToolsRuntimeCommandDispatch {
                        state,
                        pending: *pending,
                        interleaved_protocol_events: Vec::new(),
                        scheduler_events: conn.take_scheduler_events(),
                    },
                ));
            }
            RuntimeCommandTaskStep::Complete(plan) => completed_plan = Some(plan),
        }
    }
}

fn next_release_object_id(
    conn: &mut CdpConnection,
    state: &mut DevToolsRuntimeCommandDispatchState,
) -> Result<Option<String>, DevToolsError> {
    loop {
        let handle = match &mut state.kind {
            DevToolsRuntimeCommandDispatchKind::ReleaseObjects(release_state) => {
                let Some(handle) = release_state.remaining_handles.pop_front() else {
                    return Ok(None);
                };
                handle
            }
            DevToolsRuntimeCommandDispatchKind::Script { .. } => {
                return Err(DevToolsError::new(
                    DevToolsErrorKind::Internal,
                    "MissingReleaseObjectsDispatchState",
                ));
            }
        };
        let object_id = handle.as_str().to_owned();
        let mut route_scope =
            conn.scoped_none_session_owner_route_override(state.target.route.clone());
        let owner = route_scope.conn_mut();
        if !owner.runtime_remote_object_id_known_for_session_owner(None, &object_id) {
            continue;
        }
        if let Some(target_realm) = state.target_realm.as_ref()
            && let Some(owner_realm) =
                owner.runtime_remote_object_realm_for_session_owner(None, &object_id)
            && owner_realm != target_realm.as_str()
        {
            continue;
        }
        return Ok(Some(object_id));
    }
}

fn start_protocol_neutral_release_object_command(
    conn: &mut CdpConnection,
    target: &DevToolsRuntimeTarget,
    object_id: String,
    internal_command_id: u64,
) -> RuntimeCommandTaskStep {
    let params = json!({ "objectId": object_id });
    let raw_json =
        runtime_inspector_command_json(internal_command_id, "Runtime.releaseObject", &params);
    let parsed = match parse_synthesized_runtime_command(raw_json) {
        Ok(command) => command,
        Err(message) => {
            return RuntimeCommandTaskStep::Complete(runtime_inspector_error_plan(
                Some(internal_command_id),
                message,
            ));
        }
    };
    let command = Cmd::from_parsed(&parsed)
        .expect("synthesized Runtime command must contain a domain separator");
    let mut route_scope = conn.scoped_none_session_owner_route_override(target.route.clone());
    let step =
        try_start_runtime_command_dispatch(route_scope.conn_mut(), &command).unwrap_or_else(|| {
            RuntimeCommandTaskStep::Complete(CommandOutputPlan::error(
                -32601,
                "UnsupportedReleaseObjectCommand",
            ))
        });
    drop(route_scope);
    step.with_owner_scope(CommandOwnerScope::from_session_and_owner_route(
        None,
        Some(target.route.clone()),
    ))
}

fn apply_completed_release_object_plan(
    conn: &mut CdpConnection,
    state: &mut DevToolsRuntimeCommandDispatchState,
    mut plan: CommandOutputPlan,
) -> Result<(), DevToolsError> {
    let release_state = match &mut state.kind {
        DevToolsRuntimeCommandDispatchKind::ReleaseObjects(release_state) => release_state,
        DevToolsRuntimeCommandDispatchKind::Script { .. } => {
            return Err(DevToolsError::new(
                DevToolsErrorKind::Internal,
                "MissingReleaseObjectsDispatchState",
            ));
        }
    };
    if let Some(predecessor) = plan.take_renderer_output_predecessor() {
        release_state.renderer_output_predecessor = Some(predecessor);
    }
    let (response, protocol_events) =
        plan.into_runtime_inspector_response_and_background_events(state.internal_command_id, None);
    release_state.protocol_events.extend(protocol_events);
    let object_id = release_state.current_object_id.take().ok_or_else(|| {
        DevToolsError::new(
            DevToolsErrorKind::Internal,
            "MissingReleaseObjectCompletionHandle",
        )
    })?;
    let response = response.ok_or_else(|| {
        DevToolsError::new(DevToolsErrorKind::Internal, "MissingDevToolsCommandResult")
    })?;
    match BackgroundCommandResponsePayload::from_runtime_inspector_message(&response) {
        BackgroundCommandResponsePayload::Success { .. } => Ok(()),
        BackgroundCommandResponsePayload::Error { code, message, .. } => {
            let error = devtools_error_from_cdp_error_parts(Some(i64::from(code)), &message);
            if !matches!(error.kind, DevToolsErrorKind::NoSuchHandle) {
                return Err(error);
            }
            let mut route_scope =
                conn.scoped_none_session_owner_route_override(state.target.route.clone());
            route_scope
                .conn_mut()
                .unregister_runtime_remote_object_ids_for_session_owner(None, &[object_id]);
            Ok(())
        }
    }
}

async fn finish_devtools_release_objects_command(
    conn: &mut CdpConnection,
    state: DevToolsRuntimeCommandDispatchState,
    result: Result<DevToolsCommandResult, DevToolsError>,
) -> DevToolsRuntimeCommandTaskStep {
    let (protocol_events, renderer_output_predecessor) = match state.kind {
        DevToolsRuntimeCommandDispatchKind::ReleaseObjects(release_state) => (
            release_state.protocol_events,
            release_state.renderer_output_predecessor,
        ),
        DevToolsRuntimeCommandDispatchKind::Script { .. } => (Vec::new(), None),
    };
    conn.complete_devtools_runtime_direct_result(
        state.command_context,
        result,
        protocol_events,
        renderer_output_predecessor,
    )
    .await
}
