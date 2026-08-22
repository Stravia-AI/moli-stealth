use super::*;

/// Frozen command state carried while one exact-Page Runtime output
/// normalization participant is outside the Protocol owner turn.
pub(super) enum RuntimeCommandNormalizationContinuation {
    Inspector {
        renderer_response_rx: Option<RuntimeInspectorAsyncCompletionReceiver>,
        page_owner_access_allowed: bool,
        response_delivery: RendererInspectorResponseDelivery,
        session_response_predecessor: Option<moli_core::RendererOutputFence>,
        session_response_succeeded: Option<bool>,
        timing_started: Option<std::time::Instant>,
    },
    BindingInspector {
        task: RuntimeBindingCommandTask,
        renderer_response_rx: Option<RuntimeInspectorAsyncCompletionReceiver>,
    },
}

pub(super) async fn start_pending_runtime_inspector_completion(
    conn: &mut CdpConnection,
    completed: RuntimeCommandCompletionMeta,
    completed_inspector: Result<CompletedRuntimeProtocolMessageDispatch, String>,
    timing_started: Option<std::time::Instant>,
    response_flush: &crate::conn::CommandResponseFlushContext,
) -> RuntimeCommandTaskStep {
    let mut completed_protocol = match completed_inspector {
        Ok(completed_protocol) => completed_protocol,
        Err(message) => {
            if let Some(command_id) = completed.command_id {
                conn.forget_pending_inspector_await(command_id, completed.session_id());
                let correlation = conn.take_renderer_call_for_frontend_for_session_owner(
                    completed.session_id(),
                    command_id,
                );
                if correlation.is_none() {
                    tracing::debug!(
                        command_id,
                        session_id = completed.session_id(),
                        error = %message,
                        "ignored canceled renderer route after another route settled the frontend call"
                    );
                    return RuntimeCommandTaskStep::Complete(CommandOutputPlan::default());
                }
            }
            return RuntimeCommandTaskStep::Complete(runtime_inspector_error_plan(
                completed.command_id,
                message,
            ));
        }
    };
    let page_owner_access_allowed = completed_protocol.page_owner_access_allowed();
    let response_delivery = completed_protocol.response_delivery();
    let session_response_predecessor = completed_protocol.session_response_predecessor();
    let session_response_succeeded = completed_protocol.session_response_succeeded();
    let renderer_response_rx = completed_protocol.take_deferred_response_receiver();
    let step = conn.start_runtime_protocol_message_completion(completed_protocol);
    complete_runtime_command_normalization_step(
        conn,
        completed,
        RuntimeCommandNormalizationContinuation::Inspector {
            renderer_response_rx,
            page_owner_access_allowed,
            response_delivery,
            session_response_predecessor,
            session_response_succeeded,
            timing_started,
        },
        step,
        response_flush,
    )
    .await
}

pub(super) async fn start_pending_runtime_binding_inspector_completion(
    conn: &mut CdpConnection,
    completed: RuntimeCommandCompletionMeta,
    task: RuntimeBindingCommandTask,
    completed_inspector: Result<CompletedRuntimeProtocolMessageDispatch, String>,
    response_flush: &crate::conn::CommandResponseFlushContext,
) -> RuntimeCommandTaskStep {
    let mut completed_protocol = match completed_inspector {
        Ok(completed_protocol) => completed_protocol,
        Err(message) => {
            return RuntimeCommandTaskStep::Complete(runtime_inspector_error_plan(
                completed.command_id,
                message,
            ));
        }
    };
    let renderer_response_rx = completed_protocol.take_deferred_response_receiver();
    let step = conn.start_runtime_protocol_message_completion(completed_protocol);
    complete_runtime_command_normalization_step(
        conn,
        completed,
        RuntimeCommandNormalizationContinuation::BindingInspector {
            task,
            renderer_response_rx,
        },
        step,
        response_flush,
    )
    .await
}

pub(super) async fn complete_runtime_command_normalization_step(
    conn: &mut CdpConnection,
    completed: RuntimeCommandCompletionMeta,
    continuation: RuntimeCommandNormalizationContinuation,
    step: RuntimeProtocolMessageCompletionStep,
    response_flush: &crate::conn::CommandResponseFlushContext,
) -> RuntimeCommandTaskStep {
    match step {
        RuntimeProtocolMessageCompletionStep::Pending(pending) => {
            RuntimeCommandTaskStep::Pending(Box::new(PendingRuntimeCommandDispatch {
                command_id: completed.command_id,
                action: completed.action,
                owner_scope: completed.owner_scope,
                object_group: completed.object_group,
                release_object_ids: completed.release_object_ids,
                release_object_group: completed.release_object_group,
                await_promise: completed.await_promise,
                wait_for_deferred_reply: completed.wait_for_deferred_reply,
                pending: PendingRuntimeCommandKind::ProtocolMessageNormalization {
                    continuation,
                    pending,
                },
            }))
        }
        RuntimeProtocolMessageCompletionStep::Complete(result) => match continuation {
            RuntimeCommandNormalizationContinuation::Inspector {
                renderer_response_rx,
                page_owner_access_allowed,
                response_delivery,
                session_response_predecessor,
                session_response_succeeded,
                timing_started,
            } => {
                complete_pending_runtime_inspector_command_after_normalization(
                    conn,
                    completed,
                    *result,
                    renderer_response_rx,
                    page_owner_access_allowed,
                    response_delivery,
                    session_response_predecessor,
                    session_response_succeeded,
                    timing_started,
                    response_flush,
                )
                .await
            }
            RuntimeCommandNormalizationContinuation::BindingInspector {
                task,
                renderer_response_rx,
            } => {
                complete_pending_runtime_binding_inspector_command_after_normalization(
                    conn,
                    completed,
                    task,
                    *result,
                    renderer_response_rx,
                    response_flush,
                )
                .await
            }
        },
    }
}
