use moli_core::page::{CompletedPageCommand, PendingPageCommand, RendererAgentAttachmentId};

use crate::conn::{
    BackgroundProtocolEvent, CdpConnection, CommandDispatchContext, DocumentStartScript,
};
use crate::devtools_runtime::{DevToolsAddPreloadScriptCommand, DevToolsError};
use crate::domains::command_output::CommandOutputPlan;
use crate::domains::runtime::{
    BidiPreloadListenerSetupStep, CompletedBidiPreloadListenerSetup,
    PendingBidiPreloadListenerSetup, complete_bidi_preload_listener_setup,
    start_bidi_preload_listener_setup,
};

use super::super::{PageCommandTaskStep, PendingPageCommandDispatch, PendingPageCommandKind};
use super::{
    add_preload_script_result_plan, devtools_preload_internal_error,
    document_start_script_from_add_preload_command, preload_missing_owner_error,
    push_background_events, record_document_start_script,
};

/// A target-scoped add-preload command owns one renderer wait at a time.
///
/// The script registry is committed before the current-Page installation is
/// started. If that exact Page is replaced while the renderer turn is in
/// flight, its completion is discarded and the committed registry result is
/// returned; the old completion must never be applied to the successor Page.
pub(in crate::domains::page) struct PendingAddScriptToEvaluateOnNewDocumentCommand {
    task: AddScriptToEvaluateOnNewDocumentTask,
    pending: PendingAddScriptToEvaluateOnNewDocumentPhase,
}

pub(in crate::domains::page) struct CompletedAddScriptToEvaluateOnNewDocumentCommand {
    task: AddScriptToEvaluateOnNewDocumentTask,
    completed: CompletedAddScriptToEvaluateOnNewDocumentPhase,
}

enum PendingAddScriptToEvaluateOnNewDocumentPhase {
    RendererPageCommand(PendingPageCommand),
    PreloadListeners(Box<PendingBidiPreloadListenerSetup>),
}

enum CompletedAddScriptToEvaluateOnNewDocumentPhase {
    RendererPageCommand(Box<Result<CompletedPageCommand, String>>),
    PreloadListeners(Box<CompletedBidiPreloadListenerSetup>),
}

struct AddScriptToEvaluateOnNewDocumentTask {
    identifier: String,
    has_bidi_channel_argument: bool,
    pending_renderer_agent_attachment_id: Option<RendererAgentAttachmentId>,
    phase: AddScriptToEvaluateOnNewDocumentPhase,
}

#[derive(Clone, Copy)]
enum AddScriptToEvaluateOnNewDocumentPhase {
    RuntimeActivity,
    PreloadListeners,
}

struct AddScriptToEvaluateOnNewDocumentSuccess {
    identifier: String,
    preload_listener_events: Vec<BackgroundProtocolEvent>,
}

enum AddScriptToEvaluateOnNewDocumentTaskStep {
    Pending(PendingAddScriptToEvaluateOnNewDocumentCommand),
    Complete(Result<AddScriptToEvaluateOnNewDocumentSuccess, DevToolsError>),
}

impl PendingAddScriptToEvaluateOnNewDocumentCommand {
    pub(in crate::domains::page) async fn wait(
        self,
    ) -> CompletedAddScriptToEvaluateOnNewDocumentCommand {
        let completed = match self.pending {
            PendingAddScriptToEvaluateOnNewDocumentPhase::RendererPageCommand(pending) => {
                CompletedAddScriptToEvaluateOnNewDocumentPhase::RendererPageCommand(Box::new(
                    pending.wait().await.map_err(|error| error.to_string()),
                ))
            }
            PendingAddScriptToEvaluateOnNewDocumentPhase::PreloadListeners(pending) => {
                CompletedAddScriptToEvaluateOnNewDocumentPhase::PreloadListeners(Box::new(
                    (*pending).wait().await,
                ))
            }
        };
        CompletedAddScriptToEvaluateOnNewDocumentCommand {
            task: self.task,
            completed,
        }
    }
}

impl CompletedAddScriptToEvaluateOnNewDocumentCommand {
    pub(in crate::domains::page) fn renderer_output_predecessor(
        &self,
    ) -> Option<moli_core::RendererOutputFence> {
        match &self.completed {
            CompletedAddScriptToEvaluateOnNewDocumentPhase::RendererPageCommand(completed) => {
                completed
                    .as_ref()
                    .as_ref()
                    .ok()
                    .and_then(CompletedPageCommand::renderer_output_predecessor)
            }
            CompletedAddScriptToEvaluateOnNewDocumentPhase::PreloadListeners(completed) => {
                completed.renderer_output_predecessor()
            }
        }
    }
}

fn start_target_add_script_to_evaluate_on_new_document_task(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    script: DocumentStartScript,
    run_immediately: bool,
) -> AddScriptToEvaluateOnNewDocumentTaskStep {
    let Some((_, target_id)) = conn.target_owner_identity_for_session(session_id) else {
        return AddScriptToEvaluateOnNewDocumentTaskStep::Complete(Err(
            preload_missing_owner_error(conn),
        ));
    };
    let Some(recorded) = conn.with_target_owner_state_for_session_mut(session_id, |owner_state| {
        record_document_start_script(owner_state, target_id.as_deref(), &script)
    }) else {
        return AddScriptToEvaluateOnNewDocumentTaskStep::Complete(Err(
            preload_missing_owner_error(conn),
        ));
    };
    let super::RecordedDocumentStartScript {
        identifier,
        script,
        inserted,
    } = recorded;
    let mut task = AddScriptToEvaluateOnNewDocumentTask {
        identifier,
        has_bidi_channel_argument: script.has_bidi_channel_argument,
        pending_renderer_agent_attachment_id: None,
        phase: AddScriptToEvaluateOnNewDocumentPhase::RuntimeActivity,
    };
    if !inserted {
        return finish_add_script_to_evaluate_on_new_document_task(task, Vec::new());
    }

    let renderer_runtime_inspector_session_id =
        conn.target_renderer_runtime_inspector_session_id_for_session(session_id);
    let slot = match conn.runtime_session_owner_slot_mut(session_id) {
        Ok(slot) => slot,
        Err(error) => {
            return AddScriptToEvaluateOnNewDocumentTaskStep::Complete(Err(
                devtools_preload_internal_error(error),
            ));
        }
    };
    let renderer_agent_attachment_id = slot
        .current_renderer_attachment()
        .map(|attachment| attachment.id());
    let Some(page) = slot.loaded_page_mut() else {
        return finish_add_script_to_evaluate_on_new_document_task(task, Vec::new());
    };
    let Some(renderer_agent_attachment_id) = renderer_agent_attachment_id else {
        return AddScriptToEvaluateOnNewDocumentTaskStep::Complete(Err(
            devtools_preload_internal_error("NoDocumentLoaded"),
        ));
    };
    task.pending_renderer_agent_attachment_id = Some(renderer_agent_attachment_id);
    match page.start_add_document_start_script_runtime_activity(
        renderer_runtime_inspector_session_id.as_deref(),
        &script,
        run_immediately,
    ) {
        Ok(pending) => AddScriptToEvaluateOnNewDocumentTaskStep::Pending(
            PendingAddScriptToEvaluateOnNewDocumentCommand {
                task,
                pending: PendingAddScriptToEvaluateOnNewDocumentPhase::RendererPageCommand(pending),
            },
        ),
        Err(error) => AddScriptToEvaluateOnNewDocumentTaskStep::Complete(Err(
            devtools_preload_internal_error(error.to_string()),
        )),
    }
}

fn complete_target_add_script_to_evaluate_on_new_document_task(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    mut completed: CompletedAddScriptToEvaluateOnNewDocumentCommand,
    command_context: &mut CommandDispatchContext,
) -> AddScriptToEvaluateOnNewDocumentTaskStep {
    match completed.task.phase {
        AddScriptToEvaluateOnNewDocumentPhase::RuntimeActivity => {
            if renderer_completion_is_stale(conn, session_id, &completed.task) {
                return finish_add_script_to_evaluate_on_new_document_task(
                    completed.task,
                    Vec::new(),
                );
            }
            let completion = match completed.completed {
                CompletedAddScriptToEvaluateOnNewDocumentPhase::RendererPageCommand(completed) => {
                    match *completed {
                        Ok(completion) => completion,
                        Err(error) => {
                            return AddScriptToEvaluateOnNewDocumentTaskStep::Complete(Err(
                                devtools_preload_internal_error(error),
                            ));
                        }
                    }
                }
                CompletedAddScriptToEvaluateOnNewDocumentPhase::PreloadListeners(_) => {
                    return AddScriptToEvaluateOnNewDocumentTaskStep::Complete(Err(
                        devtools_preload_internal_error(
                            "Invalid addScriptToEvaluateOnNewDocument runtime completion",
                        ),
                    ));
                }
            };
            let completed_script = conn
                .runtime_session_owner_slot_mut(session_id)
                .map_err(devtools_preload_internal_error)
                .and_then(|slot| {
                    slot.loaded_page_mut()
                        .ok_or_else(|| devtools_preload_internal_error("NoDocumentLoaded"))
                })
                .and_then(|mut page| {
                    page.finish_document_start_script_result_command_turn(completion)
                        .map_err(|error| devtools_preload_internal_error(error.to_string()))
                });
            let (run_immediately_result, output) = match completed_script {
                Ok(completed_script) => completed_script,
                Err(error) => {
                    return AddScriptToEvaluateOnNewDocumentTaskStep::Complete(Err(error));
                }
            };
            command_context.consume_renderer_command_turn_output(output);
            if completed.task.has_bidi_channel_argument
                && let Some((execution_context_id, _)) = run_immediately_result
            {
                completed.task.pending_renderer_agent_attachment_id = None;
                let step =
                    start_bidi_preload_listener_setup(conn, session_id, execution_context_id);
                return continue_preload_listeners(completed.task, step);
            }
            finish_add_script_to_evaluate_on_new_document_task(completed.task, Vec::new())
        }
        AddScriptToEvaluateOnNewDocumentPhase::PreloadListeners => {
            let completed_listeners = match completed.completed {
                CompletedAddScriptToEvaluateOnNewDocumentPhase::PreloadListeners(completed) => {
                    *completed
                }
                CompletedAddScriptToEvaluateOnNewDocumentPhase::RendererPageCommand(_) => {
                    return AddScriptToEvaluateOnNewDocumentTaskStep::Complete(Err(
                        devtools_preload_internal_error(
                            "Invalid addScriptToEvaluateOnNewDocument preload-listener completion",
                        ),
                    ));
                }
            };
            let step = complete_bidi_preload_listener_setup(conn, completed_listeners);
            continue_preload_listeners(completed.task, step)
        }
    }
}

fn continue_preload_listeners(
    mut task: AddScriptToEvaluateOnNewDocumentTask,
    step: BidiPreloadListenerSetupStep,
) -> AddScriptToEvaluateOnNewDocumentTaskStep {
    match step {
        BidiPreloadListenerSetupStep::Pending(pending) => {
            task.phase = AddScriptToEvaluateOnNewDocumentPhase::PreloadListeners;
            AddScriptToEvaluateOnNewDocumentTaskStep::Pending(
                PendingAddScriptToEvaluateOnNewDocumentCommand {
                    task,
                    pending: PendingAddScriptToEvaluateOnNewDocumentPhase::PreloadListeners(
                        pending,
                    ),
                },
            )
        }
        BidiPreloadListenerSetupStep::Complete(events) => {
            finish_add_script_to_evaluate_on_new_document_task(task, events)
        }
    }
}

fn finish_add_script_to_evaluate_on_new_document_task(
    task: AddScriptToEvaluateOnNewDocumentTask,
    preload_listener_events: Vec<BackgroundProtocolEvent>,
) -> AddScriptToEvaluateOnNewDocumentTaskStep {
    AddScriptToEvaluateOnNewDocumentTaskStep::Complete(Ok(
        AddScriptToEvaluateOnNewDocumentSuccess {
            identifier: task.identifier,
            preload_listener_events,
        },
    ))
}

fn renderer_completion_is_stale(
    conn: &CdpConnection,
    session_id: Option<&str>,
    task: &AddScriptToEvaluateOnNewDocumentTask,
) -> bool {
    let Some(expected_attachment_id) = task.pending_renderer_agent_attachment_id else {
        return false;
    };
    conn.runtime_session_owner_slot(session_id)
        .map(|slot| {
            slot.current_renderer_attachment()
                .map(|attachment| attachment.id())
                != Some(expected_attachment_id)
                || !slot.has_loaded_page()
        })
        .unwrap_or(true)
}

fn page_step(
    conn: &CdpConnection,
    command_id: Option<u64>,
    session_id: Option<&str>,
    step: AddScriptToEvaluateOnNewDocumentTaskStep,
) -> PageCommandTaskStep {
    match step {
        AddScriptToEvaluateOnNewDocumentTaskStep::Pending(pending) => {
            PageCommandTaskStep::Pending(PendingPageCommandDispatch {
                command_id,
                owner_scope: crate::conn::CommandOwnerScope::capture(conn, session_id),
                kind: Box::new(PendingPageCommandKind::AddScriptToEvaluateOnNewDocument(
                    pending,
                )),
            })
        }
        AddScriptToEvaluateOnNewDocumentTaskStep::Complete(result) => {
            let mut plan = CommandOutputPlan::default();
            match result {
                Ok(success) => {
                    push_background_events(&mut plan, success.preload_listener_events);
                    plan.extend(add_preload_script_result_plan(success.identifier));
                }
                Err(error) => plan.extend(CommandOutputPlan::from_devtools_error(error)),
            }
            PageCommandTaskStep::Complete(plan)
        }
    }
}

pub(in crate::domains::page) fn complete_pending_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    session_id: Option<&str>,
    completed: CompletedAddScriptToEvaluateOnNewDocumentCommand,
    command_context: &mut CommandDispatchContext,
) -> PageCommandTaskStep {
    let step = complete_target_add_script_to_evaluate_on_new_document_task(
        conn,
        session_id,
        completed,
        command_context,
    );
    page_step(conn, command_id, session_id, step)
}

/// Compatibility adapter for protocol-neutral callers which still drain one
/// command locally. The participant itself remains move-owned and never holds
/// a `CdpConnection` borrow across a renderer wait.
pub(super) async fn execute_direct_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    command: DevToolsAddPreloadScriptCommand,
    side_effects: &mut CommandOutputPlan,
    command_context: &mut CommandDispatchContext,
) -> Result<String, DevToolsError> {
    let script = document_start_script_from_add_preload_command(&command)?;
    let mut step = start_target_add_script_to_evaluate_on_new_document_task(
        conn,
        session_id,
        script,
        command.run_immediately,
    );
    loop {
        match step {
            AddScriptToEvaluateOnNewDocumentTaskStep::Pending(pending) => {
                let completed = pending.wait().await;
                if let Some(predecessor) = completed.renderer_output_predecessor() {
                    command_context.set_renderer_output_predecessor(predecessor);
                }
                step = complete_target_add_script_to_evaluate_on_new_document_task(
                    conn,
                    session_id,
                    completed,
                    command_context,
                );
            }
            AddScriptToEvaluateOnNewDocumentTaskStep::Complete(result) => {
                let success = result?;
                push_background_events(side_effects, success.preload_listener_events);
                return Ok(success.identifier);
            }
        }
    }
}

pub(super) fn start_page_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    session_id: Option<&str>,
    script: DocumentStartScript,
    run_immediately: bool,
) -> PageCommandTaskStep {
    let step = start_target_add_script_to_evaluate_on_new_document_task(
        conn,
        session_id,
        script,
        run_immediately,
    );
    page_step(conn, command_id, session_id, step)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        CompletedAddScriptToEvaluateOnNewDocumentCommand,
        PendingAddScriptToEvaluateOnNewDocumentCommand,
        PendingAddScriptToEvaluateOnNewDocumentPhase,
    };
    use crate::conn::{BrowserContext, CommandDispatchContext};
    use crate::devtools_runtime::{
        DevToolsAddPreloadScriptCommand, DevToolsCommandContext, DevToolsPreloadScriptSource,
        DevToolsProtocol, DevToolsTargetId,
    };
    use crate::domains::command_output::CommandOutputPlan;
    use crate::domains::page::{PageCommandTaskStep, PendingPageCommandKind};
    use crate::domains::runtime::BidiPreloadListenerSetupOperationKind;
    use crate::testing::TestContext;

    async fn loaded_target_context() -> TestContext {
        let mut ctx = TestContext::new();
        let mut browser_context = BrowserContext::new("BID-add-preload-participant".to_owned());
        browser_context.set_active_target_id("TID-add-preload-participant".to_owned());
        ctx.conn.insert_browser_context(browser_context);
        ctx.install_navigation_fixture_for_session_owner(
            "data:text/html,<!doctype html><body>add preload participant</body>",
            None,
        )
        .await;
        ctx
    }

    fn targeted_add_preload_command(
        source: DevToolsPreloadScriptSource,
        run_immediately: bool,
    ) -> DevToolsAddPreloadScriptCommand {
        DevToolsAddPreloadScriptCommand {
            context: DevToolsCommandContext {
                protocol: DevToolsProtocol::Cdp,
                session_id: None,
                target_id: Some(DevToolsTargetId::from("TID-add-preload-participant")),
                browser_context_id: None,
            },
            source,
            world_name: None,
            target_ids: Some(vec![DevToolsTargetId::from("TID-add-preload-participant")]),
            browser_context_ids: Vec::new(),
            run_immediately,
            include_command_line_api: false,
        }
    }

    fn channel_add_preload_command() -> DevToolsAddPreloadScriptCommand {
        let mut command = targeted_add_preload_command(
            DevToolsPreloadScriptSource::FunctionDeclaration {
                function_declaration: "(channel) => channel('ready')".to_owned(),
                arguments: vec![json!({
                    "type": "channel",
                    "value": {
                        "channel": "add-preload-participant-channel"
                    }
                })],
            },
            true,
        );
        command.world_name = Some("utility".to_owned());
        command
    }

    fn take_pending(step: PageCommandTaskStep) -> PendingAddScriptToEvaluateOnNewDocumentCommand {
        let PageCommandTaskStep::Pending(pending) = step else {
            panic!("addScriptToEvaluateOnNewDocument should expose its next participant");
        };
        let PendingPageCommandKind::AddScriptToEvaluateOnNewDocument(pending) = *pending.kind
        else {
            panic!("addScriptToEvaluateOnNewDocument must remain on its Page command task");
        };
        pending
    }

    fn complete_participant(
        ctx: &mut TestContext,
        completed: CompletedAddScriptToEvaluateOnNewDocumentCommand,
        command_context: &mut CommandDispatchContext,
    ) -> PageCommandTaskStep {
        super::complete_pending_command(&mut ctx.conn, Some(81), None, completed, command_context)
    }

    fn response(plan: CommandOutputPlan) -> Value {
        plan.into_background_events(Some(81), None)
            .into_iter()
            .find(|event| event.protocol_message_id() == Some(81))
            .expect("addScriptToEvaluateOnNewDocument should produce its command response")
            .into_parts()
            .0
    }

    #[tokio::test]
    async fn channel_run_immediately_advances_through_owned_participants() {
        let mut ctx = loaded_target_context().await;
        let mut pending = take_pending(super::super::start_devtools_add_preload_script_command(
            &mut ctx.conn,
            Some(81),
            None,
            channel_add_preload_command(),
        ));
        let mut command_context = CommandDispatchContext::default();
        let mut saw_renderer_activity = false;
        let mut saw_realm_inventory = false;
        let mut saw_listener_batch = false;

        for _ in 0..64 {
            match &pending.pending {
                PendingAddScriptToEvaluateOnNewDocumentPhase::RendererPageCommand(_) => {
                    saw_renderer_activity = true;
                }
                PendingAddScriptToEvaluateOnNewDocumentPhase::PreloadListeners(setup) => {
                    match setup.operation_kind() {
                        BidiPreloadListenerSetupOperationKind::RealmInventory => {
                            saw_realm_inventory = true;
                        }
                        BidiPreloadListenerSetupOperationKind::ListenerBatch => {
                            saw_listener_batch = true;
                        }
                    }
                }
            }
            let completed = pending.wait().await;
            match complete_participant(&mut ctx, completed, &mut command_context) {
                PageCommandTaskStep::Pending(next) => {
                    let PendingPageCommandKind::AddScriptToEvaluateOnNewDocument(next) = *next.kind
                    else {
                        panic!("all preload setup waits must remain on the Page command task");
                    };
                    pending = next;
                }
                PageCommandTaskStep::Complete(plan) => {
                    assert!(saw_renderer_activity);
                    assert!(saw_realm_inventory);
                    assert!(saw_listener_batch);
                    assert_eq!(response(plan)["result"]["identifier"], json!("1"));
                    assert!(!ctx.conn.has_pending_inspector_awaits());
                    return;
                }
            }
        }
        panic!("addScriptToEvaluateOnNewDocument participant chain did not terminate");
    }

    #[tokio::test]
    async fn stale_renderer_completion_keeps_registry_without_entering_replacement_page() {
        let mut ctx = loaded_target_context().await;
        let old_attachment = ctx
            .conn
            .current_renderer_agent_attachment_id_for_session_owner(None)
            .expect("the old Page should have an exact renderer attachment");
        let command = targeted_add_preload_command(
            DevToolsPreloadScriptSource::RawScript(
                "globalThis.__staleAddPreload = true;".to_owned(),
            ),
            true,
        );
        let pending = take_pending(super::super::start_devtools_add_preload_script_command(
            &mut ctx.conn,
            Some(81),
            None,
            command,
        ));
        assert!(matches!(
            &pending.pending,
            PendingAddScriptToEvaluateOnNewDocumentPhase::RendererPageCommand(_)
        ));
        let completed = pending.wait().await;

        let replacement = ctx
            .conn
            .load_page_via_runtime_async("data:text/html,<!doctype html><body>replacement</body>")
            .await
            .expect("replacement Page should load");
        let old_page = ctx
            .conn
            .runtime_session_owner_slot_mut(None)
            .expect("the target should remain resident")
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

        let mut command_context = CommandDispatchContext::default();
        let PageCommandTaskStep::Complete(plan) =
            complete_participant(&mut ctx, completed, &mut command_context)
        else {
            panic!("stale renderer completion must not start work on the replacement Page");
        };
        assert_eq!(response(plan)["result"]["identifier"], json!("1"));
        assert_eq!(
            ctx.conn
                .current_renderer_agent_attachment_id_for_session_owner(None),
            Some(replacement_attachment)
        );
        assert!(
            ctx.conn
                .target_owner_state_for_session(None)
                .is_some_and(|owner_state| owner_state
                    .document_start_scripts
                    .iter()
                    .any(|(identifier, script)| identifier == "1"
                        && script.source == "globalThis.__staleAddPreload = true;"))
        );
        assert!(!ctx.conn.has_pending_inspector_awaits());
    }
}
