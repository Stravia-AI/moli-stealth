use crate::devtools_runtime::{
    DevToolsAddPreloadScriptCommand, DevToolsAddPreloadScriptResult, DevToolsCommand,
    DevToolsCommandResult, DevToolsError, DevToolsErrorKind, DevToolsPreloadScriptId,
    DevToolsPreloadScriptSource, DevToolsProtocol, DevToolsRemovePreloadScriptCommand,
    DevToolsTargetId, DevToolsTargetKind,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::{
    AddScriptToEvaluateOnNewDocumentParams, RemoveScriptToEvaluateOnNewDocumentParams,
};
use serde::Deserialize;
use serde_json::json;

use crate::conn::{
    BackgroundProtocolEvent, BrowserContext, CdpConnection, CdpSessionRoute, Cmd,
    CommandDispatchContext, DocumentStartScript, PendingBrowserOwnerInitialTargetNavigationCommand,
};
use crate::domains::command_output::CommandOutputPlan;
use crate::domains::runtime::{
    BidiPreloadListenerSetupStep, CompletedBidiPreloadListenerSetup,
    PendingBidiPreloadListenerSetup, bidi_preload_function_declaration_source,
    complete_bidi_preload_listener_setup, start_bidi_preload_listener_setup,
};

use super::{PageCommandTaskStep, PendingPageCommandDispatch, PendingPageCommandKind};
use moli_core::page::{CompletedPageCommand, PendingPageCommand, RendererAgentAttachmentId};

mod add_command;

pub(super) use add_command::{
    CompletedAddScriptToEvaluateOnNewDocumentCommand,
    PendingAddScriptToEvaluateOnNewDocumentCommand,
    complete_pending_command as complete_pending_add_script_to_evaluate_on_new_document_command,
};

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CreateIsolatedWorldParams {
    frame_id: String,
    world_name: String,
    // Chromium's CDP schema contains this historical misspelling.
    #[serde(
        default,
        rename = "grantUniveralAccess",
        alias = "grantUniversalAccess"
    )]
    grant_universal_access: bool,
}

pub(super) struct PendingCreateIsolatedWorldCommand {
    task: CreateIsolatedWorldCommandTask,
    pending: PendingCreateIsolatedWorldPhase,
}

pub(super) struct CompletedCreateIsolatedWorldCommand {
    task: CreateIsolatedWorldCommandTask,
    completed: CompletedCreateIsolatedWorldPhase,
}

struct RecordedDocumentStartScript {
    identifier: String,
    script: DocumentStartScript,
    inserted: bool,
}

enum PendingCreateIsolatedWorldPhase {
    InitialTargetNavigation(PendingBrowserOwnerInitialTargetNavigationCommand),
    RendererPageCommand(PendingPageCommand),
    PreloadListeners(Box<PendingBidiPreloadListenerSetup>),
}

enum CompletedCreateIsolatedWorldPhase {
    InitialTargetNavigation(
        Result<crate::conn::CompletedBrowserOwnerInitialTargetNavigationCommand, String>,
    ),
    RendererPageCommand(Box<Result<CompletedPageCommand, String>>),
    PreloadListeners(Box<CompletedBidiPreloadListenerSetup>),
}

struct CreateIsolatedWorldCommandTask {
    target_id: String,
    params: CreateIsolatedWorldParams,
    has_bidi_channel_argument: bool,
    prefix_output: CommandOutputPlan,
    pending_renderer_agent_attachment_id: Option<RendererAgentAttachmentId>,
    created_execution_context_id: Option<i64>,
    phase: CreateIsolatedWorldPhase,
}

#[derive(Clone)]
enum CreateIsolatedWorldPhase {
    InitialTargetNavigation,
    RuntimeActivity,
    PreloadListeners,
}

impl PendingCreateIsolatedWorldCommand {
    pub(super) async fn wait(self) -> CompletedCreateIsolatedWorldCommand {
        let completed = match self.pending {
            PendingCreateIsolatedWorldPhase::InitialTargetNavigation(pending) => {
                CompletedCreateIsolatedWorldPhase::InitialTargetNavigation(pending.wait().await)
            }
            PendingCreateIsolatedWorldPhase::RendererPageCommand(pending) => {
                CompletedCreateIsolatedWorldPhase::RendererPageCommand(Box::new(
                    pending.wait().await.map_err(|error| error.to_string()),
                ))
            }
            PendingCreateIsolatedWorldPhase::PreloadListeners(pending) => {
                CompletedCreateIsolatedWorldPhase::PreloadListeners(Box::new(
                    (*pending).wait().await,
                ))
            }
        };
        CompletedCreateIsolatedWorldCommand {
            task: self.task,
            completed,
        }
    }
}

async fn append_loaded_page_document_start_script_for_session_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    script: &DocumentStartScript,
) -> Result<(), String> {
    let renderer_runtime_inspector_session_id =
        conn.target_renderer_runtime_inspector_session_id_for_session(session_id);
    let slot = conn.runtime_session_owner_slot_mut(session_id)?;
    if let Some(mut page) = slot.loaded_page_mut() {
        page.add_document_start_script_runtime_activity_async(
            renderer_runtime_inspector_session_id.as_deref(),
            script,
            false,
        )
        .await
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn remove_loaded_page_document_start_script_for_session_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    registry_key: &str,
) -> Result<(), String> {
    let slot = conn.runtime_session_owner_slot_mut(session_id)?;
    if let Some(mut page) = slot.loaded_page_mut() {
        page.remove_document_start_script_by_registry_key_async(registry_key)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn start_default_document_start_script_append(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    identifier: String,
    script: &DocumentStartScript,
) -> PageCommandTaskStep {
    let renderer_runtime_inspector_session_id =
        conn.target_renderer_runtime_inspector_session_id_for_session(command_session_id);
    let Some(page) = conn
        .runtime_session_owner_slot_mut(command_session_id)
        .ok()
        .and_then(|slot| slot.loaded_page_mut())
    else {
        return PageCommandTaskStep::Complete(add_preload_script_result_plan(identifier));
    };
    match page.start_add_document_start_script_runtime_activity(
        renderer_runtime_inspector_session_id.as_deref(),
        script,
        false,
    ) {
        Ok(pending) => PageCommandTaskStep::Pending(PendingPageCommandDispatch {
            command_id,
            owner_scope: crate::conn::CommandOwnerScope::capture(conn, command_session_id),
            kind: Box::new(PendingPageCommandKind::AppendDefaultDocumentStartScript {
                identifier,
                pending,
            }),
        }),
        Err(error) => {
            PageCommandTaskStep::Complete(CommandOutputPlan::error(-32000, error.to_string()))
        }
    }
}

fn start_document_start_script_remove(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    registry_key: Option<String>,
) -> PageCommandTaskStep {
    let Some(registry_key) = registry_key else {
        return PageCommandTaskStep::Complete(CommandOutputPlan::success());
    };
    let Some(page) = conn
        .runtime_session_owner_slot_mut(command_session_id)
        .ok()
        .and_then(|slot| slot.loaded_page_mut())
    else {
        return PageCommandTaskStep::Complete(CommandOutputPlan::success());
    };
    match page.start_remove_document_start_script_by_registry_key(&registry_key) {
        Ok(pending) => PageCommandTaskStep::Pending(PendingPageCommandDispatch {
            command_id,
            owner_scope: crate::conn::CommandOwnerScope::capture(conn, command_session_id),
            kind: Box::new(PendingPageCommandKind::RemoveDocumentStartScript { pending }),
        }),
        Err(error) => {
            PageCommandTaskStep::Complete(CommandOutputPlan::error(-32000, error.to_string()))
        }
    }
}

pub(super) fn add_preload_script_result_plan(identifier: String) -> CommandOutputPlan {
    CommandOutputPlan::from_devtools_result(DevToolsCommandResult::AddPreloadScript(
        DevToolsAddPreloadScriptResult {
            script_id: DevToolsPreloadScriptId::from(identifier),
        },
    ))
}

fn push_background_events(
    plan: &mut CommandOutputPlan,
    events: impl IntoIterator<Item = BackgroundProtocolEvent>,
) {
    for event in events {
        plan.push_background_event(event);
    }
}

fn build_cdp_add_preload_script_command(
    cmd: &Cmd<'_>,
    target_id: Option<&str>,
    browser_context_id: Option<&str>,
    params: AddScriptToEvaluateOnNewDocumentParams,
) -> DevToolsAddPreloadScriptCommand {
    DevToolsAddPreloadScriptCommand {
        context: cmd.devtools_command_context(target_id, browser_context_id),
        source: DevToolsPreloadScriptSource::RawScript(params.source),
        world_name: params.world_name,
        target_ids: target_id.map(|target_id| vec![target_id.into()]),
        browser_context_ids: browser_context_id
            .map(|browser_context_id| vec![browser_context_id.into()])
            .unwrap_or_default(),
        run_immediately: params.run_immediately.unwrap_or(false),
        include_command_line_api: params.include_command_line_api.unwrap_or(false),
    }
}

fn build_cdp_remove_preload_script_command(
    cmd: &Cmd<'_>,
    target_id: Option<&str>,
    browser_context_id: Option<&str>,
    params: RemoveScriptToEvaluateOnNewDocumentParams,
) -> DevToolsRemovePreloadScriptCommand {
    DevToolsRemovePreloadScriptCommand {
        context: cmd.devtools_command_context(target_id, browser_context_id),
        script_id: DevToolsPreloadScriptId::from(params.identifier.as_ref()),
    }
}

fn document_start_script_from_add_preload_command(
    command: &DevToolsAddPreloadScriptCommand,
) -> Result<DocumentStartScript, DevToolsError> {
    match &command.source {
        DevToolsPreloadScriptSource::RawScript(source) => Ok(DocumentStartScript {
            registry_key: None,
            source: source.clone(),
            world_name: command.world_name.clone(),
            has_bidi_channel_argument: false,
            bidi_channel_handoffs: Vec::new(),
        }),
        DevToolsPreloadScriptSource::FunctionDeclaration {
            function_declaration,
            arguments,
        } => {
            let source = if arguments.is_empty() {
                crate::domains::runtime::BidiPreloadFunctionDeclaration {
                    source: format!("({function_declaration})();"),
                    channel_handoffs: Vec::new(),
                }
            } else {
                bidi_preload_function_declaration_source(function_declaration, arguments)
                    .map_err(devtools_preload_internal_error)?
                    .ok_or_else(|| {
                        devtools_preload_internal_error("UnsupportedPreloadScriptArguments")
                    })?
            };
            let has_bidi_channel_argument = !source.channel_handoffs.is_empty();
            Ok(DocumentStartScript {
                registry_key: None,
                source: source.source,
                world_name: command.world_name.clone(),
                has_bidi_channel_argument,
                bidi_channel_handoffs: source.channel_handoffs,
            })
        }
    }
}
fn start_devtools_preload_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    command: DevToolsCommand,
) -> PageCommandTaskStep {
    match command {
        DevToolsCommand::AddPreloadScript(command) => {
            start_devtools_add_preload_script_command(conn, command_id, command_session_id, command)
        }
        DevToolsCommand::RemovePreloadScript(command) => {
            start_devtools_remove_preload_script_command(
                conn,
                command_id,
                command_session_id,
                command,
            )
        }
        _ => PageCommandTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "UnsupportedDevToolsCommand",
        )),
    }
}

pub(crate) async fn execute_devtools_preload_command_async(
    conn: &mut CdpConnection,
    command: DevToolsCommand,
) -> (
    Result<DevToolsCommandResult, DevToolsError>,
    Vec<BackgroundProtocolEvent>,
    Option<moli_core::RendererOutputFence>,
) {
    match command {
        DevToolsCommand::AddPreloadScript(command) => {
            if !command.browser_context_ids.is_empty() {
                return (
                    execute_devtools_browser_context_add_preload_script_command(conn, command)
                        .await,
                    Vec::new(),
                    None,
                );
            }
            execute_devtools_single_route_preload_command_async(
                conn,
                DevToolsCommand::AddPreloadScript(command),
            )
            .await
        }
        DevToolsCommand::RemovePreloadScript(command)
            if command.context.protocol == DevToolsProtocol::WebDriverBidi
                && command.context.target_id.is_none()
                && split_bidi_preload_script_id(&command.script_id).is_none() =>
        {
            (
                execute_devtools_bidi_browser_context_remove_preload_script_command(conn, command)
                    .await,
                Vec::new(),
                None,
            )
        }
        command => execute_devtools_single_route_preload_command_async(conn, command).await,
    }
}

async fn execute_devtools_single_route_preload_command_async(
    conn: &mut CdpConnection,
    command: DevToolsCommand,
) -> (
    Result<DevToolsCommandResult, DevToolsError>,
    Vec<BackgroundProtocolEvent>,
    Option<moli_core::RendererOutputFence>,
) {
    let result_kind = match DevToolsPreloadResultKind::from_command(&command) {
        Ok(result_kind) => result_kind,
        Err(error) => return (Err(error), Vec::new(), None),
    };
    let (route, command) = match devtools_preload_command_target_route(conn, command) {
        Ok(resolved) => resolved,
        Err(error) => return (Err(error), Vec::new(), None),
    };
    let mut route_scope = conn.scoped_none_session_owner_route_override(route);
    let mut events = Vec::new();
    let mut command_context = CommandDispatchContext::default();
    let result = match command {
        DevToolsCommand::AddPreloadScript(command) => {
            execute_devtools_single_route_add_preload_script_command(
                route_scope.conn_mut(),
                command,
                result_kind,
                &mut events,
                &mut command_context,
            )
            .await
        }
        DevToolsCommand::RemovePreloadScript(command) => {
            match execute_devtools_single_route_remove_preload_script_command(
                route_scope.conn_mut(),
                command,
            )
            .await
            {
                Ok(()) => Ok(DevToolsCommandResult::Empty),
                Err(error) => Err(error),
            }
        }
        _ => Err(DevToolsError::new(
            DevToolsErrorKind::Unsupported,
            "UnsupportedDevToolsCommand",
        )),
    };
    let mut ordered_events = command_context.take_protocol_events();
    ordered_events.extend(events);
    ordered_events.extend(command_context.take_post_response_events());
    (
        result,
        ordered_events,
        command_context.take_renderer_output_predecessor(),
    )
}

async fn execute_devtools_browser_context_add_preload_script_command(
    conn: &mut CdpConnection,
    command: DevToolsAddPreloadScriptCommand,
) -> Result<DevToolsCommandResult, DevToolsError> {
    let script = document_start_script_from_add_preload_command(&command)?;
    let browser_context_ids =
        resolve_bidi_preload_browser_context_ids(conn, &command.browser_context_ids)?;
    let Some(first_browser_context_id) = browser_context_ids.first() else {
        return Err(DevToolsError::new(
            DevToolsErrorKind::InvalidArgument,
            "PreloadScriptUserContextsMustNotBeEmpty",
        ));
    };
    let identifier = conn
        .browser_context_by_id_mut(first_browser_context_id)
        .ok_or_else(|| DevToolsError::new(DevToolsErrorKind::NoSuchTarget, "NoSuchTarget"))?
        .reserve_default_document_start_script_id();
    let renderer_script = script.with_registry_key(
        BrowserContext::default_document_start_script_registry_key(&identifier),
    );
    for browser_context_id in &browser_context_ids {
        let Some(browser_context) = conn.browser_context_by_id_mut(browser_context_id) else {
            return Err(DevToolsError::new(
                DevToolsErrorKind::NoSuchTarget,
                "UnknownBrowserContextId",
            ));
        };
        browser_context
            .record_default_document_start_script_with_identifier(identifier.clone(), &script);
    }
    for browser_context_id in &browser_context_ids {
        let has_active_target = conn
            .browser_context_by_id(browser_context_id)
            .is_some_and(|browser_context| browser_context.active_target_id().is_some());
        if !has_active_target {
            continue;
        }
        let route = CdpSessionRoute::ActiveTarget {
            browser_context_id: browser_context_id.clone(),
            target_id: None,
        };
        let mut route_scope = conn.scoped_none_session_owner_route_override(route);
        append_loaded_page_document_start_script_for_session_async(
            route_scope.conn_mut(),
            None,
            &renderer_script,
        )
        .await
        .map_err(|message| devtools_preload_owner_error(&message))?;
    }
    Ok(DevToolsCommandResult::AddPreloadScript(
        DevToolsAddPreloadScriptResult {
            script_id: DevToolsPreloadScriptId::from(identifier),
        },
    ))
}

async fn execute_devtools_single_route_add_preload_script_command(
    conn: &mut CdpConnection,
    command: DevToolsAddPreloadScriptCommand,
    result_kind: DevToolsPreloadResultKind,
    events: &mut Vec<BackgroundProtocolEvent>,
    command_context: &mut CommandDispatchContext,
) -> Result<DevToolsCommandResult, DevToolsError> {
    let identifier = if is_bidi_default_preload_command(&command) {
        execute_devtools_default_add_preload_script_command(conn, command).await?
    } else {
        if conn.target_owner_identity_for_session(None).is_none() {
            return Err(preload_missing_owner_error(conn));
        }
        let mut side_effects = CommandOutputPlan::default();
        let identifier = add_command::execute_direct_async(
            conn,
            None,
            command,
            &mut side_effects,
            command_context,
        )
        .await?;
        events.extend(side_effects.into_background_events(None, None));
        identifier
    };
    result_kind.add_result(identifier)
}

async fn execute_devtools_default_add_preload_script_command(
    conn: &mut CdpConnection,
    command: DevToolsAddPreloadScriptCommand,
) -> Result<String, DevToolsError> {
    let script = document_start_script_from_add_preload_command(&command)?;
    let Some(browser_context) = conn.browser_context.as_mut() else {
        return Err(DevToolsError::new(
            DevToolsErrorKind::NoSuchTarget,
            "BrowserContextNotLoaded",
        ));
    };
    let identifier = browser_context.record_default_document_start_script(&script);
    let script = script.with_registry_key(
        BrowserContext::default_document_start_script_registry_key(&identifier),
    );
    append_default_document_start_script_direct_async(conn, None, identifier, &script).await
}

async fn append_default_document_start_script_direct_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    identifier: String,
    script: &DocumentStartScript,
) -> Result<String, DevToolsError> {
    let renderer_runtime_inspector_session_id =
        conn.target_renderer_runtime_inspector_session_id_for_session(session_id);
    let Some(page) = conn
        .runtime_session_owner_slot_mut(session_id)
        .ok()
        .and_then(|slot| slot.loaded_page_mut())
    else {
        return Ok(identifier);
    };
    let pending = page
        .start_add_document_start_script_runtime_activity(
            renderer_runtime_inspector_session_id.as_deref(),
            script,
            false,
        )
        .map_err(|error| devtools_preload_internal_error(error.to_string()))?;
    drop(page);
    let completion = pending
        .wait()
        .await
        .map_err(|error| devtools_preload_internal_error(error.to_string()))?;
    let Some(mut page) = conn
        .runtime_session_owner_slot_mut(session_id)
        .ok()
        .and_then(|slot| slot.loaded_page_mut())
    else {
        return Err(devtools_preload_internal_error("NoDocumentLoaded"));
    };
    page.finish_document_start_script_result(completion)
        .map_err(|error| devtools_preload_internal_error(error.to_string()))?;
    Ok(identifier)
}

async fn execute_devtools_single_route_remove_preload_script_command(
    conn: &mut CdpConnection,
    command: DevToolsRemovePreloadScriptCommand,
) -> Result<(), DevToolsError> {
    let protocol = command.context.protocol;
    let script_id = command.script_id.into_string();
    let owner_identity = conn.target_owner_identity_for_session(None);
    let target_registry_key = owner_identity.as_ref().map(|(_, target_id)| {
        BrowserContext::target_document_start_script_registry_key(target_id.as_deref(), &script_id)
    });
    let remove_result = conn.with_target_owner_state_for_session_mut(None, |owner_state| {
        remove_stored_document_start_script_registry_key(
            &mut owner_state.document_start_scripts,
            &script_id,
            target_registry_key.clone(),
        )
    });
    let Some((mut removed, mut registry_key)) = remove_result else {
        return Err(preload_missing_owner_error(conn));
    };
    if !removed
        && let Some((browser_context_id, _)) = owner_identity.as_ref()
        && let Some(browser_context) = conn.browser_context_by_id_mut(browser_context_id)
        && let Some(default_registry_key) =
            browser_context.remove_default_document_start_script(&script_id)
    {
        removed = true;
        registry_key = Some(default_registry_key);
    }
    if !removed && protocol == DevToolsProtocol::WebDriverBidi {
        return Err(DevToolsError::new(
            DevToolsErrorKind::NoSuchScript,
            "NoSuchScript",
        ));
    }
    remove_document_start_script_direct_async(conn, None, registry_key).await
}

async fn remove_document_start_script_direct_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    registry_key: Option<String>,
) -> Result<(), DevToolsError> {
    let Some(registry_key) = registry_key else {
        return Ok(());
    };
    let Some(page) = conn
        .runtime_session_owner_slot_mut(session_id)
        .ok()
        .and_then(|slot| slot.loaded_page_mut())
    else {
        return Ok(());
    };
    let pending = page
        .start_remove_document_start_script_by_registry_key(&registry_key)
        .map_err(|error| devtools_preload_internal_error(error.to_string()))?;
    drop(page);
    let completion = pending
        .wait()
        .await
        .map_err(|error| devtools_preload_internal_error(error.to_string()))?;
    let Some(mut page) = conn
        .runtime_session_owner_slot_mut(session_id)
        .ok()
        .and_then(|slot| slot.loaded_page_mut())
    else {
        return Err(devtools_preload_internal_error("NoDocumentLoaded"));
    };
    page.finish_unit_runtime_page_command(completion, "remove document-start script")
        .map_err(|error| devtools_preload_internal_error(error.to_string()))
}

async fn execute_devtools_bidi_browser_context_remove_preload_script_command(
    conn: &mut CdpConnection,
    command: DevToolsRemovePreloadScriptCommand,
) -> Result<DevToolsCommandResult, DevToolsError> {
    let script_id = command.script_id.as_str().to_owned();
    let browser_context_ids = conn
        .browser_contexts()
        .filter(|browser_context| browser_context.has_default_document_start_script(&script_id))
        .map(|browser_context| browser_context.id.clone())
        .collect::<Vec<_>>();
    if browser_context_ids.is_empty() {
        return Err(DevToolsError::new(
            DevToolsErrorKind::NoSuchScript,
            "NoSuchScript",
        ));
    }
    let mut removed_contexts = Vec::new();
    for browser_context_id in &browser_context_ids {
        if let Some(browser_context) = conn.browser_context_by_id_mut(browser_context_id)
            && let Some(registry_key) =
                browser_context.remove_default_document_start_script(&script_id)
        {
            removed_contexts.push((browser_context_id.clone(), registry_key));
        }
    }
    for (browser_context_id, registry_key) in &removed_contexts {
        let has_active_target = conn
            .browser_context_by_id(browser_context_id)
            .is_some_and(|browser_context| browser_context.active_target_id().is_some());
        if !has_active_target {
            continue;
        }
        let route = CdpSessionRoute::ActiveTarget {
            browser_context_id: browser_context_id.clone(),
            target_id: None,
        };
        let mut route_scope = conn.scoped_none_session_owner_route_override(route);
        remove_loaded_page_document_start_script_for_session_async(
            route_scope.conn_mut(),
            None,
            registry_key,
        )
        .await
        .map_err(|message| devtools_preload_owner_error(&message))?;
    }
    Ok(DevToolsCommandResult::Empty)
}

fn resolve_bidi_preload_browser_context_ids(
    conn: &mut CdpConnection,
    browser_context_ids: &[crate::devtools_runtime::DevToolsBrowserContextId],
) -> Result<Vec<String>, DevToolsError> {
    let mut resolved = Vec::new();
    for browser_context_id in browser_context_ids {
        let browser_context_id = browser_context_id.as_str();
        if browser_context_id == "default" {
            let mut default_context_ids = conn
                .browser_contexts()
                .filter(|context| is_moli_internal_default_user_context(&context.id))
                .map(|context| context.id.clone())
                .collect::<Vec<_>>();
            if default_context_ids.is_empty() {
                let id = conn.default_browser_context_id().to_owned();
                conn.try_insert_browser_context(conn.new_browser_context(id.clone()))?;
                default_context_ids.push(id);
            }
            resolved.extend(default_context_ids);
            continue;
        }
        if !conn.has_browser_context_id(browser_context_id) {
            return Err(DevToolsError::new(
                DevToolsErrorKind::NoSuchTarget,
                "UnknownBrowserContextId",
            ));
        }
        resolved.push(browser_context_id.to_owned());
    }
    resolved.sort();
    resolved.dedup();
    Ok(resolved)
}

fn is_moli_internal_default_user_context(browser_context_id: &str) -> bool {
    browser_context_id == "BID-default"
        || browser_context_id
            .strip_prefix("BID-")
            .is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
            })
}

fn devtools_preload_owner_error(message: &str) -> DevToolsError {
    if message == "BrowserContextNotLoaded" || message == "TargetNotLoaded" {
        DevToolsError::new(DevToolsErrorKind::NoSuchTarget, message)
    } else {
        DevToolsError::new(DevToolsErrorKind::Internal, message)
    }
}

fn preload_missing_owner_error(conn: &CdpConnection) -> DevToolsError {
    let message = if conn.browser_context.is_none() {
        "BrowserContextNotLoaded"
    } else {
        "TargetNotLoaded"
    };
    DevToolsError::new(DevToolsErrorKind::NoSuchTarget, message)
}

fn devtools_preload_internal_error(message: impl Into<String>) -> DevToolsError {
    DevToolsError::new(DevToolsErrorKind::Internal, message)
}

#[derive(Clone)]
enum DevToolsPreloadResultKind {
    Add {
        protocol: DevToolsProtocol,
        target_id: Option<DevToolsTargetId>,
    },
    Remove,
}

impl DevToolsPreloadResultKind {
    fn from_command(command: &DevToolsCommand) -> Result<Self, DevToolsError> {
        match command {
            DevToolsCommand::AddPreloadScript(command) => {
                let target_id = add_preload_command_target_id(command)?.cloned();
                Ok(Self::Add {
                    protocol: command.context.protocol,
                    target_id,
                })
            }
            DevToolsCommand::RemovePreloadScript(_) => Ok(Self::Remove),
            _ => Err(DevToolsError::new(
                DevToolsErrorKind::Unsupported,
                "UnsupportedDevToolsCommand",
            )),
        }
    }

    fn add_result(self, identifier: String) -> Result<DevToolsCommandResult, DevToolsError> {
        let Self::Add {
            protocol,
            target_id,
        } = self
        else {
            return Err(DevToolsError::new(
                DevToolsErrorKind::Internal,
                "UnexpectedPreloadResultKind",
            ));
        };
        let script_id = if protocol == DevToolsProtocol::WebDriverBidi
            && let Some(target_id) = target_id
        {
            format!("{}:{identifier}", target_id.as_str())
        } else {
            identifier
        };
        Ok(DevToolsCommandResult::AddPreloadScript(
            DevToolsAddPreloadScriptResult {
                script_id: DevToolsPreloadScriptId::from(script_id),
            },
        ))
    }
}

fn devtools_preload_command_target_route(
    conn: &mut CdpConnection,
    command: DevToolsCommand,
) -> Result<(CdpSessionRoute, DevToolsCommand), DevToolsError> {
    match command {
        DevToolsCommand::AddPreloadScript(command) => {
            let route = if let Some(target_id) = add_preload_command_target_id(&command)? {
                if conn
                    .target_session_route_for_child_frame_id(target_id.as_str())
                    .is_some()
                {
                    return Err(DevToolsError::new(
                        DevToolsErrorKind::InvalidArgument,
                        "PreloadScriptContextMustBeTopLevel",
                    ));
                }
                conn.target_session_route_for_target_id(target_id.as_str())
                    .ok_or_else(|| {
                        DevToolsError::new(DevToolsErrorKind::NoSuchTarget, "NoSuchTarget")
                    })?
            } else {
                default_add_preload_command_route(conn)?
            };
            Ok((route, DevToolsCommand::AddPreloadScript(command)))
        }
        DevToolsCommand::RemovePreloadScript(command) => {
            let (route, command) = remove_preload_command_target_route(conn, command)?;
            Ok((route, DevToolsCommand::RemovePreloadScript(command)))
        }
        _ => Err(DevToolsError::new(
            DevToolsErrorKind::Unsupported,
            "UnsupportedDevToolsCommand",
        )),
    }
}

fn add_preload_command_target_id(
    command: &DevToolsAddPreloadScriptCommand,
) -> Result<Option<&DevToolsTargetId>, DevToolsError> {
    if !command.browser_context_ids.is_empty() {
        return Err(DevToolsError::new(
            DevToolsErrorKind::Unsupported,
            "UnsupportedPreloadScriptUserContexts",
        ));
    }
    match command.target_ids.as_deref() {
        Some([target_id]) => Ok(Some(target_id)),
        Some([]) => Err(DevToolsError::new(
            DevToolsErrorKind::InvalidArgument,
            "PreloadScriptContextsMustNotBeEmpty",
        )),
        Some(_) => Err(DevToolsError::new(
            DevToolsErrorKind::Unsupported,
            "MultiplePreloadScriptContextsUnsupported",
        )),
        None => Ok(command.context.target_id.as_ref()),
    }
}

fn is_bidi_default_preload_command(command: &DevToolsAddPreloadScriptCommand) -> bool {
    command.context.protocol == DevToolsProtocol::WebDriverBidi
        && command.target_ids.is_none()
        && command.context.target_id.is_none()
        && command.browser_context_ids.is_empty()
}

fn default_add_preload_command_route(
    conn: &mut CdpConnection,
) -> Result<CdpSessionRoute, DevToolsError> {
    if conn.browser_context.is_none() {
        let browser_context =
            conn.new_browser_context(conn.default_browser_context_id().to_owned());
        conn.try_insert_browser_context(browser_context)?;
    }
    let browser_context = conn
        .browser_context
        .as_ref()
        .ok_or_else(|| DevToolsError::new(DevToolsErrorKind::NoSuchTarget, "NoSuchTarget"))?;
    Ok(CdpSessionRoute::ActiveTarget {
        browser_context_id: browser_context.id.clone(),
        target_id: None,
    })
}

fn remove_preload_command_target_route(
    conn: &mut CdpConnection,
    mut command: DevToolsRemovePreloadScriptCommand,
) -> Result<(CdpSessionRoute, DevToolsRemovePreloadScriptCommand), DevToolsError> {
    if let Some(target_id) = command.context.target_id.as_ref() {
        let route = conn
            .target_session_route_for_target_id(target_id.as_str())
            .ok_or_else(|| DevToolsError::new(DevToolsErrorKind::NoSuchTarget, "NoSuchTarget"))?;
        return Ok((route, command));
    }
    if command.context.protocol == DevToolsProtocol::WebDriverBidi
        && let Some((target_id, local_script_id)) = split_bidi_preload_script_id(&command.script_id)
    {
        let route = conn
            .target_session_route_for_target_id(target_id.as_str())
            .ok_or_else(|| DevToolsError::new(DevToolsErrorKind::NoSuchScript, "NoSuchScript"))?;
        command.context.target_id = Some(target_id);
        command.script_id = local_script_id;
        return Ok((route, command));
    }
    let script_id = command.script_id.as_str().to_owned();
    if command.context.protocol == DevToolsProtocol::WebDriverBidi
        && let Some(route) = find_default_preload_script_route(conn, &script_id)
    {
        return Ok((route, command));
    }
    let Some(route) = find_preload_script_route(conn, &script_id) else {
        return Err(DevToolsError::new(
            DevToolsErrorKind::NoSuchScript,
            "NoSuchScript",
        ));
    };
    Ok((route, command))
}

fn split_bidi_preload_script_id(
    script_id: &DevToolsPreloadScriptId,
) -> Option<(DevToolsTargetId, DevToolsPreloadScriptId)> {
    let (target_id, local_script_id) = script_id.as_str().split_once(':')?;
    if target_id.is_empty() || local_script_id.is_empty() {
        return None;
    }
    Some((
        DevToolsTargetId::from(target_id),
        DevToolsPreloadScriptId::from(local_script_id),
    ))
}

fn find_default_preload_script_route(
    conn: &mut CdpConnection,
    script_id: &str,
) -> Option<CdpSessionRoute> {
    conn.browser_contexts()
        .find(|browser_context| browser_context.has_default_document_start_script(script_id))
        .map(|browser_context| CdpSessionRoute::ActiveTarget {
            browser_context_id: browser_context.id.clone(),
            target_id: None,
        })
}

fn find_preload_script_route(conn: &mut CdpConnection, script_id: &str) -> Option<CdpSessionRoute> {
    let target_ids = conn
        .browser_contexts()
        .flat_map(|browser_context| browser_context.devtools_target_infos())
        .filter_map(|info| {
            if !matches!(
                info.kind,
                DevToolsTargetKind::Page | DevToolsTargetKind::Frame
            ) {
                return None;
            }
            info.target_id
                .map(|target_id| target_id.as_str().to_owned())
        })
        .collect::<Vec<_>>();
    for target_id in target_ids {
        let Some(route) = conn.target_session_route_for_target_id(&target_id) else {
            continue;
        };
        let mut route_scope = conn.scoped_none_session_owner_route_override(route.clone());
        let scoped_conn = route_scope.conn_mut();
        let has_script = scoped_conn
            .target_owner_state_for_session(None)
            .is_some_and(|owner_state| {
                owner_state
                    .document_start_scripts
                    .iter()
                    .any(|(identifier, _)| identifier == script_id)
            })
            || route
                .browser_context_id()
                .and_then(|browser_context_id| {
                    scoped_conn.browser_context_by_id(browser_context_id)
                })
                .is_some_and(|browser_context| {
                    browser_context.has_default_document_start_script(script_id)
                });
        if has_script {
            return Some(route);
        }
    }
    None
}

fn start_devtools_add_preload_script_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    command: DevToolsAddPreloadScriptCommand,
) -> PageCommandTaskStep {
    let script = match document_start_script_from_add_preload_command(&command) {
        Ok(script) => script,
        Err(error) => {
            return PageCommandTaskStep::Complete(CommandOutputPlan::from_devtools_error(error));
        }
    };
    if !is_bidi_default_preload_command(&command) {
        let run_immediately = command.run_immediately;
        return add_command::start_page_command(
            conn,
            command_id,
            command_session_id,
            script,
            run_immediately,
        );
    }

    let Some(browser_context) = conn.browser_context.as_mut() else {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error(
            -31998,
            "BrowserContextNotLoaded",
        ));
    };
    let identifier = browser_context.record_default_document_start_script(&script);
    let script = script.with_registry_key(
        BrowserContext::default_document_start_script_registry_key(&identifier),
    );
    start_default_document_start_script_append(
        conn,
        command_id,
        command_session_id,
        identifier,
        &script,
    )
}

fn start_devtools_remove_preload_script_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    command: DevToolsRemovePreloadScriptCommand,
) -> PageCommandTaskStep {
    let protocol = command.context.protocol;
    let script_id = command.script_id.into_string();
    let owner_identity = conn.target_owner_identity_for_session(command_session_id);
    let target_registry_key = owner_identity.as_ref().map(|(_, target_id)| {
        BrowserContext::target_document_start_script_registry_key(target_id.as_deref(), &script_id)
    });
    let remove_result =
        conn.with_target_owner_state_for_session_mut(command_session_id, |owner_state| {
            remove_stored_document_start_script_registry_key(
                &mut owner_state.document_start_scripts,
                &script_id,
                target_registry_key.clone(),
            )
        });
    let Some((mut removed, mut registry_key)) = remove_result else {
        if conn.browser_context.is_none() {
            return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                -31998,
                "BrowserContextNotLoaded",
            ));
        }
        return PageCommandTaskStep::Complete(CommandOutputPlan::error(-31998, "TargetNotLoaded"));
    };
    if !removed
        && let Some((browser_context_id, _)) = owner_identity.as_ref()
        && let Some(browser_context) = conn.browser_context_by_id_mut(browser_context_id)
        && let Some(default_registry_key) =
            browser_context.remove_default_document_start_script(&script_id)
    {
        removed = true;
        registry_key = Some(default_registry_key);
    }
    if !removed && protocol == DevToolsProtocol::WebDriverBidi {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error(-32000, "NoSuchScript"));
    }
    start_document_start_script_remove(conn, command_id, command_session_id, registry_key)
}

fn remove_stored_document_start_script_registry_key(
    scripts: &mut Vec<(String, DocumentStartScript)>,
    script_id: &str,
    fallback_registry_key: Option<String>,
) -> (bool, Option<String>) {
    let Some(index) = scripts
        .iter()
        .position(|(identifier, _)| identifier == script_id)
    else {
        return (false, None);
    };
    let (_, script) = scripts.remove(index);
    (true, script.registry_key.or(fallback_registry_key))
}

pub(super) fn try_start_add_script_to_evaluate_on_new_document_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> Option<PageCommandTaskStep> {
    let params: AddScriptToEvaluateOnNewDocumentParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return Some(PageCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            )));
        }
    };
    let (browser_context_id, target_id) = conn
        .target_owner_identity_for_session(cmd.session_id)
        .unwrap_or((String::new(), None));
    let browser_context_id = (!browser_context_id.is_empty()).then_some(browser_context_id);
    let command = build_cdp_add_preload_script_command(
        cmd,
        target_id.as_deref(),
        browser_context_id.as_deref(),
        params,
    );
    Some(start_devtools_preload_command(
        conn,
        cmd.id,
        cmd.session_id,
        DevToolsCommand::AddPreloadScript(command),
    ))
}

pub(super) fn try_start_remove_script_to_evaluate_on_new_document_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> Option<PageCommandTaskStep> {
    let params: RemoveScriptToEvaluateOnNewDocumentParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return Some(PageCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            )));
        }
    };
    let (browser_context_id, target_id) = conn
        .target_owner_identity_for_session(cmd.session_id)
        .unwrap_or((String::new(), None));
    let browser_context_id = (!browser_context_id.is_empty()).then_some(browser_context_id);
    let command = build_cdp_remove_preload_script_command(
        cmd,
        target_id.as_deref(),
        browser_context_id.as_deref(),
        params,
    );
    Some(start_devtools_preload_command(
        conn,
        cmd.id,
        cmd.session_id,
        DevToolsCommand::RemovePreloadScript(command),
    ))
}

pub(super) fn try_start_create_isolated_world_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> PageCommandTaskStep {
    let params: CreateIsolatedWorldParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let Some((_, Some(target_id))) = conn.target_owner_identity_for_session(cmd.session_id) else {
        if conn.browser_context.is_none() {
            return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                -31998,
                "BrowserContextNotLoaded",
            ));
        }
        return PageCommandTaskStep::Complete(CommandOutputPlan::error(-31998, "TargetNotLoaded"));
    };
    let Some(task) = prepare_create_isolated_world_task(conn, cmd.session_id, target_id, params)
    else {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error(-31998, "TargetNotLoaded"));
    };

    if conn
        .runtime_session_owner_slot(cmd.session_id)
        .is_ok_and(|slot| !slot.has_loaded_page())
    {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error(-32000, "NoDocumentLoaded"));
    }

    start_create_isolated_world_initial_target_prerequisite_or_renderer_phase(
        conn,
        cmd.id,
        cmd.session_id,
        task,
    )
}

fn prepare_create_isolated_world_task(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    target_id: String,
    params: CreateIsolatedWorldParams,
) -> Option<CreateIsolatedWorldCommandTask> {
    let has_bidi_channel_argument =
        conn.with_target_owner_state_for_session_mut(session_id, |owner_state| {
            owner_state
                .document_start_scripts
                .iter()
                .any(|(_, script)| {
                    script.world_name.as_deref() == Some(params.world_name.as_str())
                        && script.has_bidi_channel_argument
                })
        })?;
    Some(CreateIsolatedWorldCommandTask {
        target_id,
        params,
        has_bidi_channel_argument,
        prefix_output: CommandOutputPlan::default(),
        pending_renderer_agent_attachment_id: None,
        created_execution_context_id: None,
        phase: CreateIsolatedWorldPhase::RuntimeActivity,
    })
}

fn pending_create_isolated_world_command_for_session(
    conn: &CdpConnection,
    command_id: Option<u64>,
    session_id: Option<&str>,
    task: CreateIsolatedWorldCommandTask,
    pending: PendingCreateIsolatedWorldPhase,
) -> PageCommandTaskStep {
    PageCommandTaskStep::Pending(PendingPageCommandDispatch {
        command_id,
        owner_scope: crate::conn::CommandOwnerScope::capture(conn, session_id),
        kind: Box::new(PendingPageCommandKind::CreateIsolatedWorld(
            PendingCreateIsolatedWorldCommand { task, pending },
        )),
    })
}

fn start_create_isolated_world_initial_target_prerequisite_or_renderer_phase(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    session_id: Option<&str>,
    mut task: CreateIsolatedWorldCommandTask,
) -> PageCommandTaskStep {
    match conn.publish_initial_target_navigation_prerequisite_for_session_owner(session_id) {
        Ok(Some(pending)) => {
            task.phase = CreateIsolatedWorldPhase::InitialTargetNavigation;
            pending_create_isolated_world_command_for_session(
                conn,
                command_id,
                session_id,
                task,
                PendingCreateIsolatedWorldPhase::InitialTargetNavigation(pending),
            )
        }
        Ok(None) => {
            start_create_isolated_world_frame_or_world_phase(conn, command_id, session_id, task)
        }
        Err(error) => {
            PageCommandTaskStep::Complete(CommandOutputPlan::error(-32000, error.to_string()))
        }
    }
}

fn start_create_isolated_world_frame_or_world_phase(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    session_id: Option<&str>,
    mut task: CreateIsolatedWorldCommandTask,
) -> PageCommandTaskStep {
    task.phase = CreateIsolatedWorldPhase::RuntimeActivity;
    let frame_id = (task.params.frame_id != task.target_id).then(|| task.params.frame_id.clone());
    let world_name = task.params.world_name.clone();
    let grant_universal_access = task.params.grant_universal_access;
    let renderer_runtime_inspector_session_id =
        conn.target_renderer_runtime_inspector_session_id_for_session(session_id);
    let page = match loaded_page_mut_for_create_isolated_world_renderer_command(
        conn, session_id, &mut task,
    ) {
        Ok(page) => page,
        Err(plan) => return PageCommandTaskStep::Complete(plan),
    };
    match page.start_create_isolated_world_runtime_activity_capturing_runtime_inspector_messages(
        renderer_runtime_inspector_session_id.as_deref(),
        frame_id.as_deref(),
        &world_name,
        grant_universal_access,
    ) {
        Ok(pending) => pending_create_isolated_world_command_for_session(
            conn,
            command_id,
            session_id,
            task,
            PendingCreateIsolatedWorldPhase::RendererPageCommand(pending),
        ),
        Err(error) => {
            PageCommandTaskStep::Complete(CommandOutputPlan::error(-32000, error.to_string()))
        }
    }
}

fn loaded_page_mut_for_create_isolated_world_renderer_command(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    task: &mut CreateIsolatedWorldCommandTask,
) -> Result<moli_core::browser_host::BrowserPageRuntimeLease, CommandOutputPlan> {
    let slot = conn
        .runtime_session_owner_slot_mut(session_id)
        .map_err(|error| CommandOutputPlan::error(-32000, error))?;
    task.pending_renderer_agent_attachment_id = Some(
        slot.current_renderer_attachment()
            .ok_or_else(|| CommandOutputPlan::error(-32000, "NoDocumentLoaded"))?
            .id(),
    );
    slot.loaded_page_mut()
        .ok_or_else(|| CommandOutputPlan::error(-32000, "NoDocumentLoaded"))
}

fn create_isolated_world_renderer_completion_is_stale(
    conn: &CdpConnection,
    session_id: Option<&str>,
    task: &CreateIsolatedWorldCommandTask,
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

fn restart_create_isolated_world_after_stale_renderer_completion(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    session_id: Option<&str>,
    mut task: CreateIsolatedWorldCommandTask,
) -> PageCommandTaskStep {
    task.pending_renderer_agent_attachment_id = None;
    start_create_isolated_world_frame_or_world_phase(conn, command_id, session_id, task)
}

fn record_document_start_script(
    owner_state: &mut crate::conn::TargetOwnerState,
    target_id: Option<&str>,
    script: &DocumentStartScript,
) -> RecordedDocumentStartScript {
    if owner_state.next_document_start_script_id == 0
        && !owner_state.document_start_scripts.is_empty()
    {
        let max_existing_id = owner_state
            .document_start_scripts
            .iter()
            .filter_map(|(identifier, _)| identifier.parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        owner_state.next_document_start_script_id = max_existing_id;
        if let Some((identifier, existing)) =
            owner_state
                .document_start_scripts
                .iter_mut()
                .find(|(_, existing)| {
                    existing.source == script.source && existing.world_name == script.world_name
                })
        {
            let registry_key = existing.registry_key.clone().unwrap_or_else(|| {
                BrowserContext::target_document_start_script_registry_key(target_id, identifier)
            });
            if existing.registry_key.is_none() {
                existing.registry_key = Some(registry_key.clone());
            }
            let script = existing.with_registry_key(registry_key);
            return RecordedDocumentStartScript {
                identifier: identifier.clone(),
                script,
                inserted: false,
            };
        }
    }
    owner_state.next_document_start_script_id =
        owner_state.next_document_start_script_id.wrapping_add(1);
    let identifier = owner_state.next_document_start_script_id.to_string();
    let script = script.with_registry_key(
        BrowserContext::target_document_start_script_registry_key(target_id, &identifier),
    );
    owner_state
        .document_start_scripts
        .push((identifier.clone(), script.clone()));
    RecordedDocumentStartScript {
        identifier,
        script,
        inserted: true,
    }
}

pub(super) async fn complete_pending_create_isolated_world_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    session_id: Option<&str>,
    mut completed: CompletedCreateIsolatedWorldCommand,
    command_context: &mut CommandDispatchContext,
) -> PageCommandTaskStep {
    match completed.task.phase.clone() {
        CreateIsolatedWorldPhase::InitialTargetNavigation => {
            let navigation_plan = match completed.completed {
                CompletedCreateIsolatedWorldPhase::InitialTargetNavigation(Ok(completed)) => {
                    completed.into_plan()
                }
                CompletedCreateIsolatedWorldPhase::InitialTargetNavigation(Err(message)) => {
                    return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000, message,
                    ));
                }
                CompletedCreateIsolatedWorldPhase::RendererPageCommand(_) => {
                    return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        "Invalid createIsolatedWorld initial navigation completion",
                    ));
                }
                CompletedCreateIsolatedWorldPhase::PreloadListeners(_) => {
                    return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        "Invalid createIsolatedWorld initial navigation continuation",
                    ));
                }
            };
            if let Err(plan) = append_page_command_step_output(
                &mut completed.task.prefix_output,
                PageCommandTaskStep::Complete(navigation_plan),
            ) {
                return PageCommandTaskStep::Complete(plan);
            }
            start_create_isolated_world_frame_or_world_phase(
                conn,
                command_id,
                session_id,
                completed.task,
            )
        }
        CreateIsolatedWorldPhase::RuntimeActivity => {
            if create_isolated_world_renderer_completion_is_stale(conn, session_id, &completed.task)
            {
                return restart_create_isolated_world_after_stale_renderer_completion(
                    conn,
                    command_id,
                    session_id,
                    completed.task,
                );
            }
            let is_child_frame_request = completed.task.params.frame_id != completed.task.target_id;
            let completion = match completed.completed {
                CompletedCreateIsolatedWorldPhase::RendererPageCommand(renderer_completed) => {
                    match *renderer_completed {
                        Ok(completion) => completion,
                        Err(message) => {
                            let error_message = if is_child_frame_request {
                                "NoFrameForGivenId".to_owned()
                            } else {
                                message
                            };
                            return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                                -32000,
                                error_message,
                            ));
                        }
                    }
                }
                CompletedCreateIsolatedWorldPhase::InitialTargetNavigation(_)
                | CompletedCreateIsolatedWorldPhase::PreloadListeners(_) => {
                    return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        "Invalid createIsolatedWorld runtime-activity completion",
                    ));
                }
            };
            let completed_world = {
                let Some(mut page) = conn
                    .runtime_session_owner_slot_mut(session_id)
                    .ok()
                    .and_then(|slot| slot.loaded_page_mut())
                else {
                    return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        "NoDocumentLoaded",
                    ));
                };
                page.finish_create_isolated_world_command_turn(completion)
            };
            let (execution_context_id, output) = match completed_world {
                Ok(completed_world) => completed_world,
                Err(error) => {
                    return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        error.to_string(),
                    ));
                }
            };
            command_context.consume_renderer_command_turn_output(output);
            completed.task.created_execution_context_id = Some(execution_context_id);
            start_create_isolated_world_preload_listeners(
                conn,
                command_id,
                session_id,
                completed.task,
            )
        }
        CreateIsolatedWorldPhase::PreloadListeners => {
            let completed_listeners = match completed.completed {
                CompletedCreateIsolatedWorldPhase::PreloadListeners(completed) => *completed,
                CompletedCreateIsolatedWorldPhase::InitialTargetNavigation(_)
                | CompletedCreateIsolatedWorldPhase::RendererPageCommand(_) => {
                    return PageCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        "Invalid createIsolatedWorld preload-listener completion",
                    ));
                }
            };
            let step = complete_bidi_preload_listener_setup(conn, completed_listeners);
            continue_create_isolated_world_preload_listeners(
                conn,
                command_id,
                session_id,
                completed.task,
                step,
            )
        }
    }
}

fn append_page_command_step_output(
    prefix: &mut CommandOutputPlan,
    step: PageCommandTaskStep,
) -> Result<(), CommandOutputPlan> {
    match step {
        PageCommandTaskStep::Complete(plan) => {
            if plan.command_status().is_some_and(|status| status.is_err()) {
                return Err(plan);
            }
            prefix.extend(plan.into_composite_command_prefix());
            Ok(())
        }
        PageCommandTaskStep::Pending(_) => Err(CommandOutputPlan::error(
            -32000,
            "Invalid createIsolatedWorld nested navigation continuation",
        )),
    }
}

fn start_create_isolated_world_preload_listeners(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    session_id: Option<&str>,
    task: CreateIsolatedWorldCommandTask,
) -> PageCommandTaskStep {
    if !task.has_bidi_channel_argument {
        return finish_create_isolated_world_task(task, Vec::new());
    }
    let Some(execution_context_id) = task.created_execution_context_id else {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "Missing createIsolatedWorld execution context",
        ));
    };
    let step = start_bidi_preload_listener_setup(conn, session_id, execution_context_id);
    continue_create_isolated_world_preload_listeners(conn, command_id, session_id, task, step)
}

fn continue_create_isolated_world_preload_listeners(
    conn: &CdpConnection,
    command_id: Option<u64>,
    session_id: Option<&str>,
    mut task: CreateIsolatedWorldCommandTask,
    step: BidiPreloadListenerSetupStep,
) -> PageCommandTaskStep {
    match step {
        BidiPreloadListenerSetupStep::Pending(pending) => {
            task.phase = CreateIsolatedWorldPhase::PreloadListeners;
            pending_create_isolated_world_command_for_session(
                conn,
                command_id,
                session_id,
                task,
                PendingCreateIsolatedWorldPhase::PreloadListeners(pending),
            )
        }
        BidiPreloadListenerSetupStep::Complete(events) => {
            finish_create_isolated_world_task(task, events)
        }
    }
}

fn finish_create_isolated_world_task(
    mut task: CreateIsolatedWorldCommandTask,
    preload_channel_listener_events: Vec<BackgroundProtocolEvent>,
) -> PageCommandTaskStep {
    let Some(execution_context_id) = task.created_execution_context_id else {
        return PageCommandTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "Missing createIsolatedWorld execution context",
        ));
    };
    push_background_events(&mut task.prefix_output, preload_channel_listener_events);
    task.prefix_output
        .push_result(json!({ "executionContextId": execution_context_id }));
    PageCommandTaskStep::Complete(task.prefix_output)
}

#[cfg(test)]
mod protocol_neutral_tests {
    use crate::devtools_runtime::{
        DevToolsAddPreloadScriptCommand, DevToolsCommand, DevToolsCommandContext,
        DevToolsPreloadScriptSource, DevToolsProtocol,
    };
    use serde_json::json;

    use crate::conn::{CdpConnection, Cmd, DocumentStartScript, TargetOwnerState};

    use super::{
        PageCommandTaskStep, build_cdp_add_preload_script_command,
        build_cdp_remove_preload_script_command, document_start_script_from_add_preload_command,
        start_devtools_preload_command,
    };

    #[test]
    fn cdp_add_preload_script_builds_protocol_neutral_command() {
        let params = json!({
            "source": "globalThis.ready = true;",
            "worldName": "utility",
            "runImmediately": true,
            "includeCommandLineAPI": true
        });
        let cmd = Cmd::for_test(
            Some(41),
            "Page.addScriptToEvaluateOnNewDocument",
            &params,
            Some("SID-preload"),
            r#"{"id":41,"method":"Page.addScriptToEvaluateOnNewDocument"}"#,
        );
        let params = cmd
            .get_params()
            .expect("add preload params should parse")
            .expect("add preload params should be present");

        let command =
            build_cdp_add_preload_script_command(&cmd, Some("TID-preload"), Some("BID-1"), params);

        assert_eq!(command.context.protocol, DevToolsProtocol::Cdp);
        assert_eq!(
            command.context.session_id.as_ref().map(|id| id.as_str()),
            Some("SID-preload")
        );
        assert_eq!(
            command.context.target_id.as_ref().map(|id| id.as_str()),
            Some("TID-preload")
        );
        assert_eq!(
            command
                .context
                .browser_context_id
                .as_ref()
                .map(|id| id.as_str()),
            Some("BID-1")
        );
        assert_eq!(
            command.target_ids.as_ref().map(|ids| {
                ids.iter()
                    .map(crate::devtools_runtime::DevToolsTargetId::as_str)
                    .collect::<Vec<_>>()
            }),
            Some(vec!["TID-preload"])
        );
        assert_eq!(
            command
                .browser_context_ids
                .iter()
                .map(crate::devtools_runtime::DevToolsBrowserContextId::as_str)
                .collect::<Vec<_>>(),
            vec!["BID-1"]
        );
        assert_eq!(command.world_name.as_deref(), Some("utility"));
        assert!(command.run_immediately);
        assert!(command.include_command_line_api);
        assert_eq!(
            command.source,
            DevToolsPreloadScriptSource::RawScript("globalThis.ready = true;".to_owned())
        );
    }

    #[test]
    fn cdp_remove_preload_script_builds_protocol_neutral_command() {
        let params = json!({"identifier": "SCRIPT-1"});
        let cmd = Cmd::for_test(
            Some(42),
            "Page.removeScriptToEvaluateOnNewDocument",
            &params,
            Some("SID-preload"),
            r#"{"id":42,"method":"Page.removeScriptToEvaluateOnNewDocument"}"#,
        );
        let params = cmd
            .get_params()
            .expect("remove preload params should parse")
            .expect("remove preload params should be present");

        let command = build_cdp_remove_preload_script_command(
            &cmd,
            Some("TID-preload"),
            Some("BID-1"),
            params,
        );

        assert_eq!(command.context.protocol, DevToolsProtocol::Cdp);
        assert_eq!(command.script_id.as_str(), "SCRIPT-1");
        assert_eq!(
            command.context.target_id.as_ref().map(|id| id.as_str()),
            Some("TID-preload")
        );
    }

    #[test]
    fn target_preload_remove_prefers_stored_registry_key() {
        let mut scripts = vec![(
            "1".to_owned(),
            DocumentStartScript {
                registry_key: Some("target:legacy-active:1".to_owned()),
                source: "globalThis.ready = true;".to_owned(),
                world_name: None,
                has_bidi_channel_argument: false,
                bidi_channel_handoffs: Vec::new(),
            },
        )];

        let (removed, registry_key) = super::remove_stored_document_start_script_registry_key(
            &mut scripts,
            "1",
            Some("target:TID-current:1".to_owned()),
        );

        assert!(removed);
        assert_eq!(registry_key.as_deref(), Some("target:legacy-active:1"));
        assert!(scripts.is_empty());
    }

    #[test]
    fn target_preload_remove_falls_back_for_legacy_unkeyed_script() {
        let mut scripts = vec![(
            "1".to_owned(),
            DocumentStartScript {
                registry_key: None,
                source: "globalThis.ready = true;".to_owned(),
                world_name: None,
                has_bidi_channel_argument: false,
                bidi_channel_handoffs: Vec::new(),
            },
        )];

        let (removed, registry_key) = super::remove_stored_document_start_script_registry_key(
            &mut scripts,
            "1",
            Some("target:TID-current:1".to_owned()),
        );

        assert!(removed);
        assert_eq!(registry_key.as_deref(), Some("target:TID-current:1"));
        assert!(scripts.is_empty());
    }

    #[test]
    fn target_preload_duplicate_records_key_for_legacy_unkeyed_script() {
        let mut owner_state = TargetOwnerState::default();
        owner_state.document_start_scripts.push((
            "1".to_owned(),
            DocumentStartScript {
                registry_key: None,
                source: "globalThis.ready = true;".to_owned(),
                world_name: Some("utility".to_owned()),
                has_bidi_channel_argument: false,
                bidi_channel_handoffs: Vec::new(),
            },
        ));
        let script = DocumentStartScript {
            registry_key: None,
            source: "globalThis.ready = true;".to_owned(),
            world_name: Some("utility".to_owned()),
            has_bidi_channel_argument: false,
            bidi_channel_handoffs: Vec::new(),
        };

        let recorded =
            super::record_document_start_script(&mut owner_state, Some("TID-current"), &script);

        assert!(!recorded.inserted);
        assert_eq!(recorded.identifier, "1");
        assert_eq!(
            recorded.script.registry_key.as_deref(),
            Some("target:TID-current:1")
        );
        assert_eq!(
            owner_state.document_start_scripts[0]
                .1
                .registry_key
                .as_deref(),
            Some("target:TID-current:1")
        );
    }

    #[test]
    fn target_preload_duplicate_preserves_existing_stored_key() {
        let mut owner_state = TargetOwnerState::default();
        owner_state.document_start_scripts.push((
            "1".to_owned(),
            DocumentStartScript {
                registry_key: Some("target:legacy-active:1".to_owned()),
                source: "globalThis.ready = true;".to_owned(),
                world_name: None,
                has_bidi_channel_argument: false,
                bidi_channel_handoffs: Vec::new(),
            },
        ));
        let script = DocumentStartScript {
            registry_key: None,
            source: "globalThis.ready = true;".to_owned(),
            world_name: None,
            has_bidi_channel_argument: false,
            bidi_channel_handoffs: Vec::new(),
        };

        let recorded =
            super::record_document_start_script(&mut owner_state, Some("TID-current"), &script);

        assert!(!recorded.inserted);
        assert_eq!(
            recorded.script.registry_key.as_deref(),
            Some("target:legacy-active:1")
        );
        assert_eq!(
            owner_state.document_start_scripts[0]
                .1
                .registry_key
                .as_deref(),
            Some("target:legacy-active:1")
        );
    }

    #[test]
    fn bidi_add_preload_script_marks_channel_arguments() {
        let command = DevToolsAddPreloadScriptCommand {
            context: DevToolsCommandContext {
                protocol: DevToolsProtocol::WebDriverBidi,
                session_id: None,
                target_id: None,
                browser_context_id: None,
            },
            source: DevToolsPreloadScriptSource::FunctionDeclaration {
                function_declaration: "(channel) => channel('ready')".to_owned(),
                arguments: vec![json!({
                    "type": "channel",
                    "value": {
                        "channel": "channel_name"
                    }
                })],
            },
            world_name: None,
            target_ids: None,
            browser_context_ids: Vec::new(),
            run_immediately: false,
            include_command_line_api: false,
        };

        let script = document_start_script_from_add_preload_command(&command)
            .expect("BiDi channel preload should lower to a document-start script");

        assert!(script.has_bidi_channel_argument);
        assert_eq!(script.bidi_channel_handoffs.len(), 1);
        assert_eq!(script.bidi_channel_handoffs[0].channel, "channel_name");
        assert!(
            script.bidi_channel_handoffs[0]
                .handoff_id
                .starts_with("__lmBidiPreloadChannel_")
        );
        assert!(!script.bidi_channel_handoffs[0].token.is_empty());
        assert!(script.source.contains("__moliCreateBidiChannelDelegate"));
        assert!(script.source.contains("__moliPutBidiPreloadChannelProxy"));
        assert!(!script.source.contains("__moliBidiPreloadChannelRegistry"));
        assert!(
            !script
                .source
                .contains("__moliEnsureBidiPreloadChannelHandoff")
        );
        assert!(!script.source.contains("Object.create(null)"));
    }

    #[test]
    fn devtools_preload_entry_routes_add_command_to_owner_error() {
        let mut conn = CdpConnection::new();
        let params = json!({"source": "globalThis.ready = true;"});
        let cmd = Cmd::for_test(
            Some(43),
            "Page.addScriptToEvaluateOnNewDocument",
            &params,
            Some("SID-missing"),
            r#"{"id":43,"method":"Page.addScriptToEvaluateOnNewDocument"}"#,
        );
        let params = cmd
            .get_params()
            .expect("add preload params should parse")
            .expect("add preload params should be present");
        let command = build_cdp_add_preload_script_command(&cmd, None, None, params);

        let step = start_devtools_preload_command(
            &mut conn,
            cmd.id,
            cmd.session_id,
            DevToolsCommand::AddPreloadScript(command),
        );

        let PageCommandTaskStep::Complete(plan) = step else {
            panic!("missing preload target should complete through the unified preload entry");
        };
        let mut out = Vec::new();
        plan.emit_into(&mut out, cmd.id, cmd.session_id);
        assert_eq!(out[0]["id"], json!(43));
        assert_eq!(out[0]["error"]["message"], json!("BrowserContextNotLoaded"));
    }
}

#[cfg(test)]
mod create_isolated_world_participant_tests {
    use moli_page_types::BidiPreloadChannelHandoff;
    use serde_json::{Value, json};

    use super::{
        PendingCreateIsolatedWorldCommand, PendingCreateIsolatedWorldPhase,
        complete_pending_create_isolated_world_command, try_start_create_isolated_world_command,
    };
    use crate::conn::{BrowserContext, Cmd, CommandDispatchContext, DocumentStartScript};
    use crate::domains::page::{PageCommandTaskStep, PendingPageCommandKind};
    use crate::domains::runtime::BidiPreloadListenerSetupOperationKind;
    use crate::testing::TestContext;

    async fn context_with_isolated_world_channel_preload() -> TestContext {
        let mut ctx = TestContext::new();
        let mut browser_context = BrowserContext::new("BID-isolated-participant".to_owned());
        browser_context.set_active_target_id("TID-isolated-participant".to_owned());
        ctx.conn.insert_browser_context(browser_context);
        ctx.install_navigation_fixture_for_session_owner(
            "data:text/html,<!doctype html><body>isolated participant</body>",
            None,
        )
        .await;
        ctx.conn
            .with_target_owner_state_for_session_mut(None, |owner_state| {
                owner_state.document_start_scripts.push((
                    "channel-preload".to_owned(),
                    DocumentStartScript {
                        registry_key: None,
                        source: String::new(),
                        world_name: Some("utility".to_owned()),
                        has_bidi_channel_argument: true,
                        bidi_channel_handoffs: vec![BidiPreloadChannelHandoff {
                            handoff_id: "__lmIsolatedParticipant".to_owned(),
                            token: "isolated-participant-token".to_owned(),
                            channel: "isolated-participant-channel".to_owned(),
                            ownership: None,
                            serialization_options: None,
                        }],
                    },
                ));
            })
            .expect("the test target owner should remain resident");
        ctx
    }

    fn start_create_isolated_world(ctx: &mut TestContext) -> PendingCreateIsolatedWorldCommand {
        let params = json!({
            "frameId": "TID-isolated-participant",
            "worldName": "utility",
        });
        let command = Cmd::for_test(
            Some(71),
            "Page.createIsolatedWorld",
            &params,
            None,
            r#"{"id":71,"method":"Page.createIsolatedWorld"}"#,
        );
        take_create_isolated_world_pending(try_start_create_isolated_world_command(
            &mut ctx.conn,
            &command,
        ))
    }

    fn take_create_isolated_world_pending(
        step: PageCommandTaskStep,
    ) -> PendingCreateIsolatedWorldCommand {
        let PageCommandTaskStep::Pending(pending) = step else {
            panic!("createIsolatedWorld should expose its next move-owned participant");
        };
        let PendingPageCommandKind::CreateIsolatedWorld(pending) = *pending.kind else {
            panic!("createIsolatedWorld must remain on its exact Page command task");
        };
        pending
    }

    fn create_isolated_world_response(
        plan: crate::domains::command_output::CommandOutputPlan,
    ) -> Value {
        plan.into_background_events(Some(71), None)
            .into_iter()
            .find(|event| event.protocol_message_id() == Some(71))
            .expect("createIsolatedWorld should produce its command response")
            .into_parts()
            .0
    }

    #[tokio::test]
    async fn create_isolated_world_channel_listener_advances_through_owned_participants() {
        let mut ctx = context_with_isolated_world_channel_preload().await;
        let pending_world = start_create_isolated_world(&mut ctx);
        assert!(matches!(
            &pending_world.pending,
            PendingCreateIsolatedWorldPhase::RendererPageCommand(_)
        ));
        let completed_world = pending_world.wait().await;
        let mut command_context = CommandDispatchContext::default();
        let pending_realms = take_create_isolated_world_pending(
            complete_pending_create_isolated_world_command(
                &mut ctx.conn,
                Some(71),
                None,
                completed_world,
                &mut command_context,
            )
            .await,
        );
        let PendingCreateIsolatedWorldPhase::PreloadListeners(setup) = &pending_realms.pending
        else {
            panic!("realm inventory must stay inside the reusable listener setup participant");
        };
        assert_eq!(
            setup.operation_kind(),
            BidiPreloadListenerSetupOperationKind::RealmInventory
        );

        let completed_realms = pending_realms.wait().await;
        let mut step = complete_pending_create_isolated_world_command(
            &mut ctx.conn,
            Some(71),
            None,
            completed_realms,
            &mut command_context,
        )
        .await;
        let mut saw_listener_participant = false;
        for _ in 0..32 {
            match step {
                PageCommandTaskStep::Pending(pending) => {
                    let PendingPageCommandKind::CreateIsolatedWorld(pending) = *pending.kind else {
                        panic!("preload listener work must remain on createIsolatedWorld");
                    };
                    let PendingCreateIsolatedWorldPhase::PreloadListeners(setup) = &pending.pending
                    else {
                        panic!("listener work must stay inside the reusable setup participant");
                    };
                    saw_listener_participant |= setup.operation_kind()
                        == BidiPreloadListenerSetupOperationKind::ListenerBatch;
                    let completed = pending.wait().await;
                    step = complete_pending_create_isolated_world_command(
                        &mut ctx.conn,
                        Some(71),
                        None,
                        completed,
                        &mut command_context,
                    )
                    .await;
                }
                PageCommandTaskStep::Complete(plan) => {
                    assert!(saw_listener_participant);
                    let response = create_isolated_world_response(plan);
                    assert!(response["result"]["executionContextId"].as_i64().is_some());
                    return;
                }
            }
        }
        panic!("createIsolatedWorld preload listener participant did not terminate");
    }

    #[tokio::test]
    async fn stale_realm_inventory_does_not_enter_replacement_page() {
        let mut ctx = context_with_isolated_world_channel_preload().await;
        let old_attachment = ctx
            .conn
            .current_renderer_agent_attachment_id_for_session_owner(None)
            .expect("the old Page should have an exact renderer attachment");
        let pending_world = start_create_isolated_world(&mut ctx);
        let completed_world = pending_world.wait().await;
        let mut command_context = CommandDispatchContext::default();
        let pending_realms = take_create_isolated_world_pending(
            complete_pending_create_isolated_world_command(
                &mut ctx.conn,
                Some(71),
                None,
                completed_world,
                &mut command_context,
            )
            .await,
        );
        let PendingCreateIsolatedWorldPhase::PreloadListeners(setup) = &pending_realms.pending
        else {
            panic!("realm inventory must stay inside the reusable listener setup participant");
        };
        assert_eq!(
            setup.operation_kind(),
            BidiPreloadListenerSetupOperationKind::RealmInventory
        );
        let completed_realms = pending_realms.wait().await;

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

        let PageCommandTaskStep::Complete(plan) = complete_pending_create_isolated_world_command(
            &mut ctx.conn,
            Some(71),
            None,
            completed_realms,
            &mut command_context,
        )
        .await
        else {
            panic!("stale realm inventory must not start work on the replacement Page");
        };
        let response = create_isolated_world_response(plan);
        assert!(response["result"]["executionContextId"].as_i64().is_some());
        assert_eq!(
            ctx.conn
                .current_renderer_agent_attachment_id_for_session_owner(None),
            Some(replacement_attachment)
        );
        assert!(!ctx.conn.has_pending_inspector_awaits());
    }
}
