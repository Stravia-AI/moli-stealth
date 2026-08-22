use std::{collections::VecDeque, time::Instant};

use moli_core::page::{
    CompletedPageCommand, PendingPageCommand, RendererCommandTurnOutput,
    RendererRuntimeInspectorMessage,
};
use serde_json::{Value, json};

use super::{
    CdpConnection, CompletedRuntimeProtocolMessageDispatch, RuntimeProtocolMessagePageRoute,
    collect_remote_object_paths,
};

/// One exact Page wait required to normalize frozen Runtime protocol output.
///
/// Context-id compatibility and DOM-node subtype discovery both require
/// renderer queries. Keeping those queries in a move-owned state machine
/// prevents a seemingly local output-apply turn from borrowing
/// `CdpConnection` across another Page command wait.
pub(crate) struct PendingRuntimeProtocolMessageNormalization {
    continuation: RuntimeProtocolMessageNormalizationContinuation,
    operation: PendingRuntimeProtocolMessageNormalizationOperation,
}

pub(crate) struct CompletedRuntimeProtocolMessageNormalization {
    continuation: RuntimeProtocolMessageNormalizationContinuation,
    operation: CompletedRuntimeProtocolMessageNormalizationOperation,
}

pub(crate) enum RuntimeProtocolMessageCompletionStep {
    Pending(Box<PendingRuntimeProtocolMessageNormalization>),
    Complete(Box<Result<Option<RendererCommandTurnOutput>, String>>),
}

struct RuntimeProtocolMessageNormalizationContinuation {
    session_id: Option<String>,
    route: RuntimeProtocolMessagePageRoute,
    output: RendererCommandTurnOutput,
    jobs: VecDeque<RuntimeProtocolMessageNormalizationJob>,
    timing_started: Option<Instant>,
}

enum RuntimeProtocolMessageNormalizationJob {
    ContextId(RuntimeContextIdNormalizationJob),
    Node(RuntimeNodeNormalizationJob),
}

struct RuntimeContextIdNormalizationJob {
    message_index: usize,
    context_pointer: &'static str,
    inspector_context_id: i64,
}

struct RuntimeNodeNormalizationJob {
    message_index: usize,
    path: String,
    object_id: String,
}

enum PendingRuntimeProtocolMessageNormalizationOperation {
    EnsureContextWorlds {
        job: RuntimeContextIdNormalizationJob,
        pending: PendingPageCommand,
    },
    ResolveContextId {
        job: RuntimeContextIdNormalizationJob,
        pending: PendingPageCommand,
    },
    ResolveNode {
        job: RuntimeNodeNormalizationJob,
        pending: PendingPageCommand,
    },
}

enum CompletedRuntimeProtocolMessageNormalizationOperation {
    EnsureContextWorlds {
        job: RuntimeContextIdNormalizationJob,
        completed: Result<CompletedPageCommand, String>,
    },
    ResolveContextId {
        job: RuntimeContextIdNormalizationJob,
        completed: Result<CompletedPageCommand, String>,
    },
    ResolveNode {
        job: RuntimeNodeNormalizationJob,
        completed: Result<CompletedPageCommand, String>,
    },
}

impl PendingRuntimeProtocolMessageNormalization {
    pub(crate) async fn wait(self) -> CompletedRuntimeProtocolMessageNormalization {
        let operation = match self.operation {
            PendingRuntimeProtocolMessageNormalizationOperation::EnsureContextWorlds {
                job,
                pending,
            } => CompletedRuntimeProtocolMessageNormalizationOperation::EnsureContextWorlds {
                job,
                completed: pending.wait().await.map_err(|error| error.to_string()),
            },
            PendingRuntimeProtocolMessageNormalizationOperation::ResolveContextId {
                job,
                pending,
            } => CompletedRuntimeProtocolMessageNormalizationOperation::ResolveContextId {
                job,
                completed: pending.wait().await.map_err(|error| error.to_string()),
            },
            PendingRuntimeProtocolMessageNormalizationOperation::ResolveNode { job, pending } => {
                CompletedRuntimeProtocolMessageNormalizationOperation::ResolveNode {
                    job,
                    completed: pending.wait().await.map_err(|error| error.to_string()),
                }
            }
        };
        CompletedRuntimeProtocolMessageNormalization {
            continuation: self.continuation,
            operation,
        }
    }
}

pub(super) fn start_runtime_protocol_message_completion(
    conn: &mut CdpConnection,
    completed: CompletedRuntimeProtocolMessageDispatch,
) -> RuntimeProtocolMessageCompletionStep {
    let timing_started = moli_trace::cdp_nav_timing_enabled().then(Instant::now);
    let completion = match completed.completion {
        moli_core::page::CompletedRuntimeInspectorCommandDispatch::Owner(completion)
        | moli_core::page::CompletedRuntimeInspectorCommandDispatch::OwnerSessionResponse {
            completion,
            ..
        } => *completion,
        moli_core::page::CompletedRuntimeInspectorCommandDispatch::Inspector
        | moli_core::page::CompletedRuntimeInspectorCommandDispatch::InspectorSessionResponse {
            ..
        }
        | moli_core::page::CompletedRuntimeInspectorCommandDispatch::OwnerSessionErrorSettled(
            _,
        ) => {
            return RuntimeProtocolMessageCompletionStep::Complete(Box::new(Ok(None)));
        }
    };
    let mut output =
        match conn.consume_runtime_protocol_message_completion(&completed.route, completion) {
            Ok(output) => output,
            Err(error) => {
                return RuntimeProtocolMessageCompletionStep::Complete(Box::new(Err(error)));
            }
        };
    if let Some(started) = timing_started {
        tracing::info!(
            target: "moli_cdp_nav_timing",
            stage = "runtime_inspector_page_dispatch_done",
            output_messages = output
                .runtime_inspector_output()
                .map_or(0, |messages| messages.len()),
            elapsed_ms = started.elapsed().as_millis(),
        );
    }
    conn.ingest_runtime_protocol_message_started_route_output_updates(&completed.route);
    if let Some(started) = timing_started {
        tracing::info!(
            target: "moli_cdp_nav_timing",
            stage = "runtime_inspector_output_ingested",
            elapsed_ms = started.elapsed().as_millis(),
        );
    }
    let runtime_messages = match output.runtime_inspector_output_mut() {
        Some(runtime_messages) => runtime_messages,
        None => {
            return RuntimeProtocolMessageCompletionStep::Complete(Box::new(Err(
                "runtime inspector dispatch completed with a non-Runtime renderer reply".to_owned(),
            )));
        }
    };
    runtime_messages.bind_renderer_agent_attachment(completed.route.renderer_agent_attachment_id);
    let jobs = collect_runtime_protocol_message_normalization_jobs(runtime_messages.messages());
    start_next_runtime_protocol_message_normalization(
        conn,
        RuntimeProtocolMessageNormalizationContinuation {
            session_id: completed.session_id,
            route: completed.route,
            output,
            jobs,
            timing_started,
        },
    )
}

pub(super) fn complete_runtime_protocol_message_normalization(
    conn: &mut CdpConnection,
    completed: CompletedRuntimeProtocolMessageNormalization,
) -> RuntimeProtocolMessageCompletionStep {
    let CompletedRuntimeProtocolMessageNormalization {
        mut continuation,
        operation,
    } = completed;
    if conn
        .runtime_protocol_message_started_page_mut(&continuation.route)
        .is_err()
    {
        return finish_runtime_protocol_message_normalization(conn, continuation);
    }
    match operation {
        CompletedRuntimeProtocolMessageNormalizationOperation::EnsureContextWorlds {
            job,
            completed,
        } => {
            let ensured = completed.and_then(|completed| {
                conn.runtime_protocol_message_started_page_mut(&continuation.route)
                    .and_then(|mut page| {
                        page.finish_ensure_isolated_worlds_attached_to_inspector(completed)
                            .map_err(|error| error.to_string())
                    })
            });
            if let Err(error) = ensured {
                tracing::debug!(%error, "Runtime context-id normalization setup failed");
                return start_next_runtime_protocol_message_normalization(conn, continuation);
            }
            let pending = conn
                .runtime_protocol_message_started_page_mut(&continuation.route)
                .and_then(|page| {
                    page.start_isolated_execution_context_id_for_inspector_context(
                        job.inspector_context_id,
                    )
                    .map_err(|error| error.to_string())
                });
            match pending {
                Ok(pending) => RuntimeProtocolMessageCompletionStep::Pending(Box::new(
                    PendingRuntimeProtocolMessageNormalization {
                        continuation,
                        operation:
                            PendingRuntimeProtocolMessageNormalizationOperation::ResolveContextId {
                                job,
                                pending,
                            },
                    },
                )),
                Err(error) => {
                    tracing::debug!(%error, "Runtime context-id normalization lookup failed to start");
                    start_next_runtime_protocol_message_normalization(conn, continuation)
                }
            }
        }
        CompletedRuntimeProtocolMessageNormalizationOperation::ResolveContextId {
            job,
            completed,
        } => {
            let compatibility_context_id = completed
                .and_then(|completed| {
                    conn.runtime_protocol_message_started_page_mut(&continuation.route)
                        .and_then(|mut page| {
                            page.finish_isolated_execution_context_id_for_inspector_context(
                                completed,
                            )
                            .map_err(|error| error.to_string())
                        })
                })
                .ok()
                .flatten();
            if let Some(compatibility_context_id) = compatibility_context_id
                && let Some(mut message) =
                    runtime_protocol_message_value_mut(&mut continuation.output, job.message_index)
                && let Some(context_id) = message.pointer_mut(job.context_pointer)
            {
                *context_id = json!(compatibility_context_id);
            }
            start_next_runtime_protocol_message_normalization(conn, continuation)
        }
        CompletedRuntimeProtocolMessageNormalizationOperation::ResolveNode { job, completed } => {
            let is_node = completed
                .and_then(|completed| {
                    conn.runtime_protocol_message_started_page_mut(&continuation.route)
                        .and_then(|mut page| {
                            page.finish_document_node_snapshot_for_object_id(completed)
                                .map_err(|error| error.to_string())
                        })
                })
                .ok()
                .flatten()
                .is_some();
            if is_node
                && let Some(mut message) =
                    runtime_protocol_message_value_mut(&mut continuation.output, job.message_index)
                && let Some(remote_object) = message
                    .pointer_mut(&job.path)
                    .and_then(Value::as_object_mut)
            {
                remote_object.insert("subtype".to_owned(), json!("node"));
            }
            start_next_runtime_protocol_message_normalization(conn, continuation)
        }
    }
}

fn start_next_runtime_protocol_message_normalization(
    conn: &mut CdpConnection,
    mut continuation: RuntimeProtocolMessageNormalizationContinuation,
) -> RuntimeProtocolMessageCompletionStep {
    loop {
        let Some(job) = continuation.jobs.pop_front() else {
            return finish_runtime_protocol_message_normalization(conn, continuation);
        };
        let operation = match job {
            RuntimeProtocolMessageNormalizationJob::ContextId(job) => {
                let pending = conn
                    .runtime_protocol_message_started_page_mut(&continuation.route)
                    .and_then(|page| {
                        page.start_ensure_isolated_worlds_attached_to_inspector()
                            .map_err(|error| error.to_string())
                    });
                match pending {
                    Ok(pending) => {
                        PendingRuntimeProtocolMessageNormalizationOperation::EnsureContextWorlds {
                            job,
                            pending,
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "Runtime context-id normalization failed to start");
                        continue;
                    }
                }
            }
            RuntimeProtocolMessageNormalizationJob::Node(job) => {
                let include_whitespace =
                    crate::domains::dom::dom_agent_includes_whitespace_for_session(
                        conn,
                        continuation.session_id.as_deref(),
                    );
                let pending = conn
                    .runtime_protocol_message_started_page_mut(&continuation.route)
                    .and_then(|page| {
                        page.start_document_node_snapshot_for_object_id_in_inspector_session(
                            continuation.route.inspector_session_id.clone(),
                            include_whitespace,
                            &job.object_id,
                            0,
                            false,
                        )
                        .map_err(|error| error.to_string())
                    });
                match pending {
                    Ok(pending) => {
                        PendingRuntimeProtocolMessageNormalizationOperation::ResolveNode {
                            job,
                            pending,
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "Runtime node normalization failed to start");
                        continue;
                    }
                }
            }
        };
        return RuntimeProtocolMessageCompletionStep::Pending(Box::new(
            PendingRuntimeProtocolMessageNormalization {
                continuation,
                operation,
            },
        ));
    }
}

fn finish_runtime_protocol_message_normalization(
    conn: &mut CdpConnection,
    mut continuation: RuntimeProtocolMessageNormalizationContinuation,
) -> RuntimeProtocolMessageCompletionStep {
    let runtime_messages = match continuation.output.runtime_inspector_output_mut() {
        Some(runtime_messages) => runtime_messages.messages_mut(),
        None => {
            return RuntimeProtocolMessageCompletionStep::Complete(Box::new(Err(
                "runtime inspector output disappeared during normalization".to_owned(),
            )));
        }
    };
    conn.restore_frontend_command_ids_in_runtime_messages(
        continuation.session_id.as_deref(),
        Some(continuation.route.renderer_agent_attachment_id),
        runtime_messages,
    );
    if let Some(started) = continuation.timing_started {
        tracing::info!(
            target: "moli_cdp_nav_timing",
            stage = "runtime_inspector_command_output_ready",
            output_messages = runtime_messages.len(),
            elapsed_ms = started.elapsed().as_millis(),
        );
    }
    RuntimeProtocolMessageCompletionStep::Complete(Box::new(Ok(Some(continuation.output))))
}

fn collect_runtime_protocol_message_normalization_jobs(
    messages: &[RendererRuntimeInspectorMessage],
) -> VecDeque<RuntimeProtocolMessageNormalizationJob> {
    let mut jobs = VecDeque::new();
    for (message_index, message) in messages.iter().enumerate() {
        let RendererRuntimeInspectorMessage::Protocol(message) = message else {
            continue;
        };
        let value = message.value();
        let context_pointer = match value.get("method").and_then(Value::as_str) {
            Some("Runtime.consoleAPICalled") => Some("/params/executionContextId"),
            Some("Runtime.exceptionThrown") => Some("/params/exceptionDetails/executionContextId"),
            _ => None,
        };
        if let Some(context_pointer) = context_pointer
            && let Some(inspector_context_id) =
                value.pointer(context_pointer).and_then(Value::as_i64)
        {
            jobs.push_back(RuntimeProtocolMessageNormalizationJob::ContextId(
                RuntimeContextIdNormalizationJob {
                    message_index,
                    context_pointer,
                    inspector_context_id,
                },
            ));
        }

        let mut paths = Vec::new();
        collect_remote_object_paths(value, "", &mut paths);
        for path in paths {
            let Some(remote_object) = value.pointer(&path) else {
                continue;
            };
            let Some(object_id) = remote_object
                .get("objectId")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                continue;
            };
            let has_subtype = remote_object.get("subtype").is_some();
            let is_object_like = remote_object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|ty| matches!(ty, "object" | "function"));
            if has_subtype || !is_object_like {
                continue;
            }
            jobs.push_back(RuntimeProtocolMessageNormalizationJob::Node(
                RuntimeNodeNormalizationJob {
                    message_index,
                    path,
                    object_id,
                },
            ));
        }
    }
    jobs
}

fn runtime_protocol_message_value_mut(
    output: &mut RendererCommandTurnOutput,
    message_index: usize,
) -> Option<moli_core::page::RendererRuntimeInspectorProtocolMessageValueMut<'_>> {
    let message = output
        .runtime_inspector_output_mut()?
        .messages_mut()
        .get_mut(message_index)?;
    let RendererRuntimeInspectorMessage::Protocol(message) = message else {
        return None;
    };
    Some(message.value_mut())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::{BrowserContext, CommandResponseFlushContext};
    use crate::testing::TestContext;

    #[tokio::test]
    async fn node_normalization_completion_does_not_enter_replacement_renderer_attachment() {
        let mut ctx = TestContext::new();
        let mut browser_context = BrowserContext::new("BID-normalization-stale".to_owned());
        browser_context.set_active_target_id("TID-normalization-stale".to_owned());
        ctx.conn.insert_browser_context(browser_context);
        ctx.install_navigation_fixture_for_session_owner(
            "data:text/html,<!doctype html><body>old</body>",
            None,
        )
        .await;

        let old_attachment = ctx
            .conn
            .current_renderer_agent_attachment_id_for_session_owner(None)
            .expect("the old Page should have an exact renderer attachment");
        let pending = ctx
            .conn
            .start_runtime_protocol_message_for_session_owner(
                None,
                json!({
                    "id": 41,
                    "method": "Runtime.evaluate",
                    "params": { "expression": "document.body" },
                })
                .to_string(),
            )
            .expect("Runtime.evaluate should start on the old Page");
        let completed = pending
            .wait()
            .await
            .expect("Runtime.evaluate should complete on the old Page");
        let RuntimeProtocolMessageCompletionStep::Pending(pending) = ctx
            .conn
            .start_runtime_protocol_message_completion(completed)
        else {
            panic!("a DOM remote object must require an exact-Page normalization command");
        };
        let completed = (*pending).wait().await;

        let replacement = ctx
            .conn
            .load_page_via_runtime_async("data:text/html,<!doctype html><body>replacement</body>")
            .await
            .expect("replacement Page should load");
        let old_page = ctx
            .conn
            .runtime_session_owner_slot_mut(None)
            .expect("the old target should remain resident")
            .clear_loaded_page_for_test_fixture();
        drop(old_page);
        ctx.conn
            .runtime_session_owner_slot_mut(None)
            .expect("the replacement target should remain resident")
            .set_loaded_page_for_test(replacement);
        let replacement_attachment = ctx
            .conn
            .current_renderer_agent_attachment_id_for_session_owner(None)
            .expect("the replacement Page should have an exact renderer attachment");
        assert_ne!(old_attachment, replacement_attachment);

        let RuntimeProtocolMessageCompletionStep::Complete(result) = ctx
            .conn
            .complete_runtime_protocol_message_normalization(completed)
        else {
            panic!("a stale normalization must finish without entering the replacement Page");
        };
        let Ok(Some(output)) = *result else {
            panic!("a stale normalization must preserve the immutable old-Page output");
        };
        let response = output
            .runtime_inspector_output()
            .and_then(|output| {
                output.messages().iter().find_map(|message| match message {
                    RendererRuntimeInspectorMessage::Protocol(message) => Some(message.value()),
                    RendererRuntimeInspectorMessage::RuntimeContext(_) => None,
                })
            })
            .expect("Runtime.evaluate should retain its frozen old-Page response");
        assert_eq!(response.pointer("/result/result/subtype"), None);

        let mut events = Vec::new();
        let mut post_response_events = Vec::new();
        let (seen, _) = ctx.conn.route_normalized_renderer_command_turn_output_into(
            output,
            Some(41),
            None,
            &CommandResponseFlushContext::default(),
            &mut events,
            &mut post_response_events,
        );
        assert!(!seen);
        assert!(events.is_empty() && post_response_events.is_empty());
        assert_eq!(
            ctx.conn
                .current_renderer_agent_attachment_id_for_session_owner(None),
            Some(replacement_attachment),
            "stale normalization must leave the successor renderer attachment untouched"
        );
    }
}
