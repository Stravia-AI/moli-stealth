use std::collections::VecDeque;

use moli_core::page::{CompletedPageCommand, PendingPageCommand, RendererRuntimeRealmInfo};
use moli_page_types::BidiPreloadChannelHandoff;
use serde_json::{Value, json};

use crate::conn::{
    BackgroundProtocolEvent, BidiChannelListenerResidence, BidiChannelOwnerAction,
    BidiChannelOwnerActionStep, BidiChannelPageOwner, CdpConnection, CommandResponseFlushContext,
    CompletedBidiChannelOwnerAction, CompletedRuntimeProtocolMessageDispatch,
    CompletedRuntimeProtocolMessageNormalization, PendingBidiChannelListener,
    PendingBidiChannelOwnerAction, PendingRuntimeProtocolMessageDispatch,
    PendingRuntimeProtocolMessageNormalization, RuntimeProtocolMessageCompletionStep,
};
use crate::devtools_runtime::{DevToolsRealmId, DevToolsRemoteHandleId, DevToolsTargetId};

use super::dispatcher::{
    bidi_preload_channel_properties_from_handoff, bidi_preload_channel_proxy_handle_source,
};

/// One exact-Page setup for BiDi preload listeners created by a concrete
/// execution context.
///
/// Realm discovery is itself a renderer command, so it is kept in the same
/// move-owned participant chain as proxy materialization, listener startup,
/// and failure cleanup. A Page replacement between any wait and apply turn
/// terminates the setup without following a session id into the successor.
pub(crate) struct PendingBidiPreloadListenerSetup {
    operation: PendingBidiPreloadListenerSetupOperation,
}

enum PendingBidiPreloadListenerSetupOperation {
    RealmInventory(Box<PendingBidiPreloadListenerRealmInventory>),
    ListenerBatch(Box<PendingBidiPreloadListenerBatch>),
}

struct PendingBidiPreloadListenerRealmInventory {
    owner: BidiChannelPageOwner,
    execution_context_id: i64,
    pending: PendingPageCommand,
}

pub(crate) struct CompletedBidiPreloadListenerSetup {
    operation: CompletedBidiPreloadListenerSetupOperation,
}

enum CompletedBidiPreloadListenerSetupOperation {
    RealmInventory(Box<CompletedBidiPreloadListenerRealmInventory>),
    ListenerBatch(Box<CompletedBidiPreloadListenerBatch>),
}

struct CompletedBidiPreloadListenerRealmInventory {
    owner: BidiChannelPageOwner,
    execution_context_id: i64,
    completed: Result<CompletedPageCommand, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) enum BidiPreloadListenerSetupOperationKind {
    RealmInventory,
    ListenerBatch,
}

pub(crate) enum BidiPreloadListenerSetupStep {
    Pending(Box<PendingBidiPreloadListenerSetup>),
    Complete(Vec<BackgroundProtocolEvent>),
}

impl PendingBidiPreloadListenerSetup {
    #[cfg(test)]
    pub(crate) fn operation_kind(&self) -> BidiPreloadListenerSetupOperationKind {
        match self.operation {
            PendingBidiPreloadListenerSetupOperation::RealmInventory(_) => {
                BidiPreloadListenerSetupOperationKind::RealmInventory
            }
            PendingBidiPreloadListenerSetupOperation::ListenerBatch(_) => {
                BidiPreloadListenerSetupOperationKind::ListenerBatch
            }
        }
    }

    pub(crate) async fn wait(self) -> CompletedBidiPreloadListenerSetup {
        let operation = match self.operation {
            PendingBidiPreloadListenerSetupOperation::RealmInventory(pending) => {
                let PendingBidiPreloadListenerRealmInventory {
                    owner,
                    execution_context_id,
                    pending,
                } = *pending;
                CompletedBidiPreloadListenerSetupOperation::RealmInventory(Box::new(
                    CompletedBidiPreloadListenerRealmInventory {
                        owner,
                        execution_context_id,
                        completed: pending.wait().await.map_err(|error| error.to_string()),
                    },
                ))
            }
            PendingBidiPreloadListenerSetupOperation::ListenerBatch(pending) => {
                CompletedBidiPreloadListenerSetupOperation::ListenerBatch(Box::new(
                    (*pending).wait().await,
                ))
            }
        };
        CompletedBidiPreloadListenerSetup { operation }
    }
}

impl CompletedBidiPreloadListenerSetup {
    pub(crate) fn renderer_output_predecessor(&self) -> Option<moli_core::RendererOutputFence> {
        match &self.operation {
            CompletedBidiPreloadListenerSetupOperation::RealmInventory(completed) => completed
                .completed
                .as_ref()
                .ok()
                .and_then(CompletedPageCommand::renderer_output_predecessor),
            CompletedBidiPreloadListenerSetupOperation::ListenerBatch(_) => None,
        }
    }
}

pub(crate) fn start_bidi_preload_listener_setup(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    execution_context_id: i64,
) -> BidiPreloadListenerSetupStep {
    if conn
        .target_owner_bidi_channel_preload_handoffs_for_session(session_id)
        .is_empty()
    {
        return BidiPreloadListenerSetupStep::Complete(Vec::new());
    }
    let Some(owner) = BidiChannelPageOwner::capture(conn, session_id) else {
        return BidiPreloadListenerSetupStep::Complete(Vec::new());
    };
    let owner_session_id = owner.session_id().map(str::to_owned);
    let pending = {
        let mut route_scope = owner.enter(conn);
        let conn = route_scope.conn_mut();
        if !owner.is_current(conn) {
            return BidiPreloadListenerSetupStep::Complete(Vec::new());
        }
        conn.runtime_session_owner_slot_mut(owner_session_id.as_deref())
            .and_then(|slot| {
                slot.loaded_page_mut()
                    .ok_or_else(|| "NoDocumentLoaded".to_owned())
            })
            .and_then(|page| {
                page.start_runtime_realm_inventory()
                    .map_err(|error| error.to_string())
            })
    };
    match pending {
        Ok(pending) => {
            BidiPreloadListenerSetupStep::Pending(Box::new(PendingBidiPreloadListenerSetup {
                operation: PendingBidiPreloadListenerSetupOperation::RealmInventory(Box::new(
                    PendingBidiPreloadListenerRealmInventory {
                        owner,
                        execution_context_id,
                        pending,
                    },
                )),
            }))
        }
        Err(error) => {
            tracing::debug!(%error, execution_context_id, "failed to start BiDi preload listener realm inventory");
            BidiPreloadListenerSetupStep::Complete(Vec::new())
        }
    }
}

pub(crate) fn complete_bidi_preload_listener_setup(
    conn: &mut CdpConnection,
    completed: CompletedBidiPreloadListenerSetup,
) -> BidiPreloadListenerSetupStep {
    match completed.operation {
        CompletedBidiPreloadListenerSetupOperation::RealmInventory(completed_inventory) => {
            let CompletedBidiPreloadListenerRealmInventory {
                owner,
                execution_context_id,
                completed,
            } = *completed_inventory;
            let completed = match completed {
                Ok(completed) => completed,
                Err(error) => {
                    tracing::debug!(%error, execution_context_id, "BiDi preload listener realm inventory failed");
                    return BidiPreloadListenerSetupStep::Complete(Vec::new());
                }
            };
            let owner_session_id = owner.session_id().map(str::to_owned);
            let realms = {
                let mut route_scope = owner.enter(conn);
                let conn = route_scope.conn_mut();
                if !owner.is_current(conn) {
                    return BidiPreloadListenerSetupStep::Complete(Vec::new());
                }
                conn.runtime_session_owner_slot_mut(owner_session_id.as_deref())
                    .and_then(|slot| {
                        slot.loaded_page_mut()
                            .ok_or_else(|| "NoDocumentLoaded".to_owned())
                    })
                    .and_then(|mut page| {
                        page.finish_runtime_realm_inventory(completed)
                            .map_err(|error| error.to_string())
                    })
            };
            let realms = match realms {
                Ok(realms) => realms
                    .into_iter()
                    .filter(|realm| realm.context_id == execution_context_id)
                    .collect(),
                Err(error) => {
                    tracing::debug!(%error, execution_context_id, "failed to finish BiDi preload listener realm inventory");
                    return BidiPreloadListenerSetupStep::Complete(Vec::new());
                }
            };
            let batch_step = {
                let mut route_scope = owner.enter(conn);
                start_bidi_preload_listener_batch(
                    route_scope.conn_mut(),
                    owner_session_id.as_deref(),
                    realms,
                )
            };
            bidi_preload_listener_setup_from_batch_step(batch_step)
        }
        CompletedBidiPreloadListenerSetupOperation::ListenerBatch(completed) => {
            let step = complete_bidi_preload_listener_batch(conn, *completed);
            bidi_preload_listener_setup_from_batch_step(step)
        }
    }
}

fn bidi_preload_listener_setup_from_batch_step(
    step: BidiPreloadListenerBatchStep,
) -> BidiPreloadListenerSetupStep {
    match step {
        BidiPreloadListenerBatchStep::Pending(pending) => {
            BidiPreloadListenerSetupStep::Pending(Box::new(PendingBidiPreloadListenerSetup {
                operation: PendingBidiPreloadListenerSetupOperation::ListenerBatch(pending),
            }))
        }
        BidiPreloadListenerBatchStep::Complete(events) => {
            BidiPreloadListenerSetupStep::Complete(events)
        }
    }
}

struct BidiPreloadListenerRealm {
    execution_context_id: i64,
    realm_id: DevToolsRealmId,
    listener_target_id: DevToolsTargetId,
}

struct BidiPreloadListenerJob {
    realm: BidiPreloadListenerRealm,
    handoff: BidiPreloadChannelHandoff,
}

struct BidiPreloadListenerBatchContinuation {
    owner: BidiChannelPageOwner,
    jobs: VecDeque<BidiPreloadListenerJob>,
    background_events: Vec<BackgroundProtocolEvent>,
}

/// One move-owned renderer wait in a loaded-Document BiDi preload-listener
/// batch.
///
/// A batch deliberately advances one command at a time. Completion returns to
/// the Browser Owner apply turn, revalidates the exact Page generation, and
/// only then starts the next proxy/listener/cleanup operation.
pub(crate) struct PendingBidiPreloadListenerBatch {
    continuation: BidiPreloadListenerBatchContinuation,
    operation: PendingBidiPreloadListenerOperation,
}

enum PendingBidiPreloadListenerOperation {
    TakeProxy {
        job: BidiPreloadListenerJob,
        command_id: u64,
        object_group: String,
        pending: PendingRuntimeProtocolMessageDispatch,
    },
    NormalizeProxy {
        job: BidiPreloadListenerJob,
        command_id: u64,
        object_group: String,
        pending: Box<PendingRuntimeProtocolMessageNormalization>,
    },
    OwnerAction(PendingBidiChannelOwnerAction),
}

pub(crate) struct CompletedBidiPreloadListenerBatch {
    continuation: BidiPreloadListenerBatchContinuation,
    operation: CompletedBidiPreloadListenerOperation,
}

enum CompletedBidiPreloadListenerOperation {
    TakeProxy {
        job: BidiPreloadListenerJob,
        command_id: u64,
        object_group: String,
        completed: Result<CompletedRuntimeProtocolMessageDispatch, String>,
    },
    NormalizeProxy {
        job: BidiPreloadListenerJob,
        command_id: u64,
        object_group: String,
        completed: Box<CompletedRuntimeProtocolMessageNormalization>,
    },
    OwnerAction(CompletedBidiChannelOwnerAction),
}

pub(crate) enum BidiPreloadListenerBatchStep {
    Pending(Box<PendingBidiPreloadListenerBatch>),
    Complete(Vec<BackgroundProtocolEvent>),
}

impl PendingBidiPreloadListenerBatch {
    pub(crate) async fn wait(self) -> CompletedBidiPreloadListenerBatch {
        let operation = match self.operation {
            PendingBidiPreloadListenerOperation::TakeProxy {
                job,
                command_id,
                object_group,
                pending,
            } => CompletedBidiPreloadListenerOperation::TakeProxy {
                job,
                command_id,
                object_group,
                completed: pending.wait().await,
            },
            PendingBidiPreloadListenerOperation::NormalizeProxy {
                job,
                command_id,
                object_group,
                pending,
            } => CompletedBidiPreloadListenerOperation::NormalizeProxy {
                job,
                command_id,
                object_group,
                completed: Box::new((*pending).wait().await),
            },
            PendingBidiPreloadListenerOperation::OwnerAction(pending) => {
                CompletedBidiPreloadListenerOperation::OwnerAction(pending.wait().await)
            }
        };
        CompletedBidiPreloadListenerBatch {
            continuation: self.continuation,
            operation,
        }
    }
}

pub(crate) fn start_bidi_preload_listener_batch(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    realms: Vec<RendererRuntimeRealmInfo>,
) -> BidiPreloadListenerBatchStep {
    let handoffs = conn.target_owner_bidi_channel_preload_handoffs_for_session(session_id);
    if handoffs.is_empty() {
        return BidiPreloadListenerBatchStep::Complete(Vec::new());
    }
    let target_id = conn
        .target_owner_identity_for_session(session_id)
        .and_then(|(_, target_id)| target_id)
        .map(DevToolsTargetId::from);
    let route = conn.session_route(session_id).or_else(|| {
        target_id
            .as_ref()
            .and_then(|target_id| conn.target_session_route_for_target_id(target_id.as_str()))
    });
    let Some(route) = route else {
        return BidiPreloadListenerBatchStep::Complete(Vec::new());
    };
    let owner = {
        let mut route_scope = conn.scoped_none_session_owner_route_override(route);
        BidiChannelPageOwner::capture(route_scope.conn_mut(), session_id)
    };
    let Some(owner) = owner else {
        return BidiPreloadListenerBatchStep::Complete(Vec::new());
    };

    let mut seen_context_ids = Vec::new();
    let mut jobs = VecDeque::new();
    for realm in realms {
        if seen_context_ids.contains(&realm.context_id) {
            continue;
        }
        let Some(native_realm_id) = realm.realm_id.filter(|realm_id| !realm_id.is_empty()) else {
            continue;
        };
        let listener_target_id = realm
            .frame_id
            .filter(|frame_id| !frame_id.is_empty())
            .map(DevToolsTargetId::from)
            .or_else(|| target_id.clone());
        let Some(listener_target_id) = listener_target_id else {
            continue;
        };
        let realm_owner_id = target_id
            .as_ref()
            .map(DevToolsTargetId::as_str)
            .unwrap_or_else(|| listener_target_id.as_str());
        let realm = BidiPreloadListenerRealm {
            execution_context_id: realm.context_id,
            realm_id: DevToolsRealmId::from(format!("{realm_owner_id}:{native_realm_id}")),
            listener_target_id,
        };
        seen_context_ids.push(realm.execution_context_id);
        for handoff in &handoffs {
            jobs.push_back(BidiPreloadListenerJob {
                realm: BidiPreloadListenerRealm {
                    execution_context_id: realm.execution_context_id,
                    realm_id: realm.realm_id.clone(),
                    listener_target_id: realm.listener_target_id.clone(),
                },
                handoff: handoff.clone(),
            });
        }
    }
    start_next_bidi_preload_listener_job(
        conn,
        BidiPreloadListenerBatchContinuation {
            owner,
            jobs,
            background_events: Vec::new(),
        },
    )
}

pub(crate) fn complete_bidi_preload_listener_batch(
    conn: &mut CdpConnection,
    completed: CompletedBidiPreloadListenerBatch,
) -> BidiPreloadListenerBatchStep {
    let CompletedBidiPreloadListenerBatch {
        continuation,
        operation,
    } = completed;
    match operation {
        CompletedBidiPreloadListenerOperation::OwnerAction(completed) => {
            let step = conn.complete_bidi_channel_owner_action(completed);
            continue_after_bidi_channel_owner_action(conn, continuation, step)
        }
        CompletedBidiPreloadListenerOperation::TakeProxy {
            job,
            command_id,
            object_group,
            completed,
        } => {
            if !bidi_preload_batch_owner_is_current(conn, &continuation.owner) {
                return BidiPreloadListenerBatchStep::Complete(continuation.background_events);
            }
            let completed = match completed {
                Ok(completed) => completed,
                Err(error) => {
                    return continue_after_bidi_preload_proxy_response(
                        conn,
                        continuation,
                        job,
                        object_group,
                        Err(error),
                    );
                }
            };
            let step = {
                let mut route_scope = continuation.owner.enter(conn);
                route_scope
                    .conn_mut()
                    .start_runtime_protocol_message_completion(completed)
            };
            continue_after_bidi_preload_proxy_normalization(
                conn,
                continuation,
                job,
                command_id,
                object_group,
                step,
            )
        }
        CompletedBidiPreloadListenerOperation::NormalizeProxy {
            job,
            command_id,
            object_group,
            completed,
        } => {
            if !bidi_preload_batch_owner_is_current(conn, &continuation.owner) {
                return BidiPreloadListenerBatchStep::Complete(continuation.background_events);
            }
            let step = {
                let mut route_scope = continuation.owner.enter(conn);
                route_scope
                    .conn_mut()
                    .complete_runtime_protocol_message_normalization(*completed)
            };
            continue_after_bidi_preload_proxy_normalization(
                conn,
                continuation,
                job,
                command_id,
                object_group,
                step,
            )
        }
    }
}

fn continue_after_bidi_preload_proxy_normalization(
    conn: &mut CdpConnection,
    continuation: BidiPreloadListenerBatchContinuation,
    job: BidiPreloadListenerJob,
    command_id: u64,
    object_group: String,
    step: RuntimeProtocolMessageCompletionStep,
) -> BidiPreloadListenerBatchStep {
    match step {
        RuntimeProtocolMessageCompletionStep::Pending(pending) => {
            BidiPreloadListenerBatchStep::Pending(Box::new(PendingBidiPreloadListenerBatch {
                continuation,
                operation: PendingBidiPreloadListenerOperation::NormalizeProxy {
                    job,
                    command_id,
                    object_group,
                    pending,
                },
            }))
        }
        RuntimeProtocolMessageCompletionStep::Complete(result) => {
            let response = (*result).and_then(|output| {
                finish_bidi_preload_proxy_dispatch(conn, &continuation.owner, command_id, output)
            });
            continue_after_bidi_preload_proxy_response(
                conn,
                continuation,
                job,
                object_group,
                response,
            )
        }
    }
}

fn continue_after_bidi_preload_proxy_response(
    conn: &mut CdpConnection,
    continuation: BidiPreloadListenerBatchContinuation,
    job: BidiPreloadListenerJob,
    object_group: String,
    response: Result<Value, String>,
) -> BidiPreloadListenerBatchStep {
    let proxy_handle = match response {
        Ok(response) => {
            let session_id = continuation.owner.session_id().map(str::to_owned);
            let mut route_scope = continuation.owner.enter(conn);
            let conn = route_scope.conn_mut();
            conn.register_runtime_remote_object_ids_from_value_for_session_owner_with_group(
                session_id.as_deref(),
                &response,
                &object_group,
            );
            let handle = response
                .pointer("/result/result/objectId")
                .and_then(Value::as_str)
                .map(|object_id| DevToolsRemoteHandleId::from(object_id.to_owned()));
            if let Some(handle) = handle.as_ref() {
                conn.register_runtime_remote_object_ids_for_session_owner_with_realm(
                    session_id.as_deref(),
                    vec![handle.as_str().to_owned()],
                    job.realm.realm_id.as_str(),
                );
            }
            handle
        }
        Err(error) => {
            tracing::debug!(
                %error,
                handoff_id = %job.handoff.handoff_id,
                channel = %job.handoff.channel,
                execution_context_id = job.realm.execution_context_id,
                "failed to materialize BiDi preload channel proxy handle"
            );
            None
        }
    };
    let Some(proxy_handle) = proxy_handle else {
        let owner = continuation.owner.clone();
        let step = conn.start_bidi_channel_owner_action(
            BidiChannelOwnerAction::release_object_group(owner, object_group),
        );
        return continue_after_bidi_channel_owner_action(conn, continuation, step);
    };
    let properties = match bidi_preload_channel_properties_from_handoff(&job.handoff) {
        Ok(properties) => properties,
        Err(error) => {
            tracing::debug!(
                %error,
                handoff_id = %job.handoff.handoff_id,
                channel = %job.handoff.channel,
                execution_context_id = job.realm.execution_context_id,
                "skipping invalid BiDi preload channel handoff"
            );
            let owner = continuation.owner.clone();
            let step = conn.start_bidi_channel_owner_action(
                BidiChannelOwnerAction::release_object_group(owner, object_group),
            );
            return continue_after_bidi_channel_owner_action(conn, continuation, step);
        }
    };
    let Some(listener) = PendingBidiChannelListener::new(
        Some(job.realm.listener_target_id),
        Some(job.realm.realm_id),
        proxy_handle,
        object_group.clone(),
        properties,
    ) else {
        let owner = continuation.owner.clone();
        let step = conn.start_bidi_channel_owner_action(
            BidiChannelOwnerAction::release_object_group(owner, object_group),
        );
        return continue_after_bidi_channel_owner_action(conn, continuation, step);
    };
    let residence = BidiChannelListenerResidence::new(continuation.owner.clone(), listener);
    let step =
        conn.start_bidi_channel_owner_action(BidiChannelOwnerAction::start_listener(residence));
    continue_after_bidi_channel_owner_action(conn, continuation, step)
}

fn start_next_bidi_preload_listener_job(
    conn: &mut CdpConnection,
    mut continuation: BidiPreloadListenerBatchContinuation,
) -> BidiPreloadListenerBatchStep {
    loop {
        if !bidi_preload_batch_owner_is_current(conn, &continuation.owner) {
            return BidiPreloadListenerBatchStep::Complete(continuation.background_events);
        }
        let Some(job) = continuation.jobs.pop_front() else {
            return BidiPreloadListenerBatchStep::Complete(continuation.background_events);
        };
        let command_id = conn.next_internal_runtime_command_id();
        let object_group = conn.next_bidi_channel_object_group();
        let raw_json = json!({
            "id": command_id,
            "method": "Runtime.callFunctionOn",
            "params": {
                "functionDeclaration": bidi_preload_channel_proxy_handle_source(),
                "arguments": [
                    { "value": job.handoff.handoff_id },
                    { "value": job.handoff.token },
                ],
                "awaitPromise": false,
                "returnByValue": false,
                "executionContextId": job.realm.execution_context_id,
                "objectGroup": object_group,
            }
        })
        .to_string();
        let session_id = continuation.owner.session_id().map(str::to_owned);
        let pending = {
            let mut route_scope = continuation.owner.enter(conn);
            route_scope
                .conn_mut()
                .start_runtime_protocol_message_for_session_owner(session_id.as_deref(), raw_json)
        };
        match pending {
            Ok(pending) => {
                return BidiPreloadListenerBatchStep::Pending(Box::new(
                    PendingBidiPreloadListenerBatch {
                        continuation,
                        operation: PendingBidiPreloadListenerOperation::TakeProxy {
                            job,
                            command_id,
                            object_group,
                            pending,
                        },
                    },
                ));
            }
            Err(error) => {
                tracing::debug!(
                    %error,
                    handoff_id = %job.handoff.handoff_id,
                    channel = %job.handoff.channel,
                    execution_context_id = job.realm.execution_context_id,
                    "failed to start BiDi preload channel proxy materialization"
                );
                match conn.start_bidi_channel_owner_action(
                    BidiChannelOwnerAction::release_object_group(
                        continuation.owner.clone(),
                        object_group,
                    ),
                ) {
                    BidiChannelOwnerActionStep::Pending(pending) => {
                        return BidiPreloadListenerBatchStep::Pending(Box::new(
                            PendingBidiPreloadListenerBatch {
                                continuation,
                                operation: PendingBidiPreloadListenerOperation::OwnerAction(
                                    *pending,
                                ),
                            },
                        ));
                    }
                    BidiChannelOwnerActionStep::Complete(events) => {
                        continuation.background_events.extend(events);
                    }
                }
            }
        }
    }
}

fn continue_after_bidi_channel_owner_action(
    conn: &mut CdpConnection,
    mut continuation: BidiPreloadListenerBatchContinuation,
    step: BidiChannelOwnerActionStep,
) -> BidiPreloadListenerBatchStep {
    match step {
        BidiChannelOwnerActionStep::Pending(pending) => {
            BidiPreloadListenerBatchStep::Pending(Box::new(PendingBidiPreloadListenerBatch {
                continuation,
                operation: PendingBidiPreloadListenerOperation::OwnerAction(*pending),
            }))
        }
        BidiChannelOwnerActionStep::Complete(events) => {
            continuation.background_events.extend(events);
            start_next_bidi_preload_listener_job(conn, continuation)
        }
    }
}

fn bidi_preload_batch_owner_is_current(
    conn: &mut CdpConnection,
    owner: &BidiChannelPageOwner,
) -> bool {
    let mut route_scope = owner.enter(conn);
    owner.is_current(route_scope.conn_mut())
}

fn finish_bidi_preload_proxy_dispatch(
    conn: &mut CdpConnection,
    owner: &BidiChannelPageOwner,
    command_id: u64,
    output: Option<moli_core::page::RendererCommandTurnOutput>,
) -> Result<Value, String> {
    let session_id = owner.session_id().map(str::to_owned);
    let Some(output) = output else {
        return Err("BiDi preload proxy command was canceled".to_owned());
    };
    let mut events = Vec::new();
    let mut post_response_events = Vec::new();
    let response_flush = CommandResponseFlushContext::default();
    {
        let mut route_scope = owner.enter(conn);
        route_scope
            .conn_mut()
            .route_normalized_renderer_command_turn_output_into(
                output,
                Some(command_id),
                session_id.as_deref(),
                &response_flush,
                &mut events,
                &mut post_response_events,
            );
    }
    events.extend(post_response_events);
    let mut response = None;
    for event in events {
        if event.protocol_message_id() == Some(command_id) && response.is_none() {
            response = Some(event.into_parts().0);
        }
    }
    let response = response.ok_or_else(|| "MissingDevToolsCommandResult".to_owned())?;
    if let Some(error) = response.get("error") {
        return Err(error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("BiDi preload proxy command failed")
            .to_owned());
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use moli_core::page::RendererRuntimeRealmInfo;
    use moli_page_types::{BidiPreloadChannelHandoff, DocumentStartScript};
    use serde_json::json;

    use super::{
        BidiPreloadListenerBatchStep, complete_bidi_preload_listener_batch,
        start_bidi_preload_listener_batch,
    };
    use crate::testing::TestContext;

    #[tokio::test(flavor = "multi_thread")]
    async fn preload_listener_batch_completion_rejects_replacement_generation_before_apply() {
        let mut ctx = TestContext::new();
        tokio::task::LocalSet::new()
            .run_until(async {
                ctx.process_async(json!({
                    "id": 1,
                    "method": "Target.createTarget",
                    "params": { "url": "about:blank" }
                }))
                .await;
                ctx.conn
                    .browser_context
                    .as_mut()
                    .expect("test BrowserContext")
                    .record_default_document_start_script(&DocumentStartScript {
                        registry_key: None,
                        source: String::new(),
                        world_name: None,
                        has_bidi_channel_argument: true,
                        bidi_channel_handoffs: vec![BidiPreloadChannelHandoff {
                            handoff_id: "__lmStalePreloadHandoff".to_owned(),
                            token: "stale-token".to_owned(),
                            channel: "stale-channel".to_owned(),
                            ownership: None,
                            serialization_options: None,
                        }],
                    });
                let realm = ctx
                    .conn
                    .runtime_realm_inventory_for_session_owner_async(None)
                    .await
                    .expect("loaded Page should expose a runtime realm")
                    .into_iter()
                    .find(|realm| realm.context_id.is_some() && realm.realm_id.is_some())
                    .expect("default runtime realm");
                let target_id = realm
                    .target_id
                    .as_ref()
                    .expect("test realm target")
                    .as_str()
                    .to_owned();
                let global_realm_id = realm
                    .realm_id
                    .as_ref()
                    .expect("test realm id")
                    .as_str();
                let native_realm_id = global_realm_id
                    .strip_prefix(&format!("{target_id}:"))
                    .unwrap_or(global_realm_id)
                    .to_owned();
                let realms = vec![RendererRuntimeRealmInfo {
                    context_id: realm.context_id.expect("test context id"),
                    realm_id: Some(native_realm_id),
                    frame_id: realm.frame_id.map(|frame_id| frame_id.into_string()),
                    origin: realm.origin.unwrap_or_default(),
                    name: realm.name.unwrap_or_default(),
                    is_default: realm.is_default.unwrap_or(true),
                    context_type: realm.context_type.unwrap_or_else(|| "default".to_owned()),
                    grant_universal_access: realm.grant_universal_access,
                }];
                let BidiPreloadListenerBatchStep::Pending(pending) =
                    start_bidi_preload_listener_batch(&mut ctx.conn, None, realms)
                else {
                    panic!("a real Page proxy lookup must become a participant");
                };
                let completed = (*pending).wait().await;
                let BidiPreloadListenerBatchStep::Pending(pending) =
                    complete_bidi_preload_listener_batch(&mut ctx.conn, completed)
                else {
                    panic!("proxy output normalization must become its own Page participant");
                };
                let completed = (*pending).wait().await;

                let slot = ctx
                    .conn
                    .runtime_session_owner_slot_mut(None)
                    .expect("test runtime slot");
                slot.set_loaded_page_generation(slot.loaded_page_generation() + 1);

                let BidiPreloadListenerBatchStep::Complete(events) =
                    complete_bidi_preload_listener_batch(&mut ctx.conn, completed)
                else {
                    panic!("a stale completion must not enter the replacement Page");
                };
                assert!(events.is_empty());
                assert!(
                    ctx.conn.take_scheduler_events().is_empty(),
                    "stale preload work must not publish a listener or cleanup against the replacement Page"
                );
            })
            .await;
    }
}
