use moli_owner_queue::{OwnerReadyTaskRoute, OwnerReadyTaskSource, OwnerTaskReadySignal};
use moli_shared_worker::SharedWorkerClientId;

use crate::{
    native_bridge::WindowExecutionContextIdentity,
    resource_ready::{ReadyPageTask, RendererPageTaskReadyMetadata},
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
    shared_worker_runtime::{SharedWorkerClientEndpointDisposition, SharedWorkerClientEvent},
};

use super::RendererOwnerWakeSender;

/// Exact Page-side SharedWorker wrapper that owns one client event.
///
/// Browser-context client ids identify the runtime endpoint. The root token
/// prevents a late event from crossing PageVm replacement, while the Window
/// identity prevents an old child, popup, or isolated realm from dispatching
/// through a replacement wrapper.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageSharedWorkerClientEventOwner {
    root_document: RendererDocumentToken,
    execution_context: WindowExecutionContextIdentity,
    client_id: SharedWorkerClientId,
}

impl RendererPageSharedWorkerClientEventOwner {
    pub(crate) const fn new(
        root_document: RendererDocumentToken,
        execution_context: WindowExecutionContextIdentity,
        client_id: SharedWorkerClientId,
    ) -> Self {
        Self {
            root_document,
            execution_context,
            client_id,
        }
    }

    #[cfg(test)]
    pub(crate) const fn root_document(self) -> RendererDocumentToken {
        self.root_document
    }

    pub(crate) const fn execution_context(self) -> WindowExecutionContextIdentity {
        self.execution_context
    }

    pub(crate) const fn client_id(self) -> SharedWorkerClientId {
        self.client_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RendererSharedWorkerClientEventKind {
    Closed,
    Error,
}

fn shared_worker_client_event_kind(
    event: &SharedWorkerClientEvent,
) -> RendererSharedWorkerClientEventKind {
    match event {
        SharedWorkerClientEvent::Closed => RendererSharedWorkerClientEventKind::Closed,
        SharedWorkerClientEvent::Error(_) => RendererSharedWorkerClientEventKind::Error,
    }
}

#[derive(Debug)]
pub(crate) struct RendererPageSharedWorkerClientEventTask {
    owner: RendererPageSharedWorkerClientEventOwner,
    event: SharedWorkerClientEvent,
}

impl RendererPageSharedWorkerClientEventTask {
    fn new(
        owner: RendererPageSharedWorkerClientEventOwner,
        event: SharedWorkerClientEvent,
    ) -> Self {
        Self { owner, event }
    }

    pub(crate) const fn owner(&self) -> RendererPageSharedWorkerClientEventOwner {
        self.owner
    }

    pub(crate) fn event_kind(&self) -> RendererSharedWorkerClientEventKind {
        shared_worker_client_event_kind(&self.event)
    }

    pub(crate) fn into_event(self) -> SharedWorkerClientEvent {
        self.event
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageSharedWorkerClientEventRouteClosed;

#[derive(Clone, Debug)]
pub(crate) struct RendererPageSharedWorkerClientEventRoute {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageSharedWorkerClientEventTask>,
        RendererPageSharedWorkerClientEventReadySignal,
    >,
}

impl RendererPageSharedWorkerClientEventRoute {
    pub(crate) fn sender(
        &self,
        root_document: RendererDocumentToken,
    ) -> RendererPageSharedWorkerClientEventSender {
        RendererPageSharedWorkerClientEventSender {
            task_route: self.task_route.clone(),
            root_document,
        }
    }

    fn same_route_as(&self, source: &RendererPageSharedWorkerClientEventSource) -> bool {
        self.task_route.same_source_as(&source.source)
    }
}

/// PageVm-stamped route. Binding the creating Window produces a realm route;
/// the browser-context runtime binds the allocated client id exactly once.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageSharedWorkerClientEventSender {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageSharedWorkerClientEventTask>,
        RendererPageSharedWorkerClientEventReadySignal,
    >,
    root_document: RendererDocumentToken,
}

impl RendererPageSharedWorkerClientEventSender {
    pub(crate) fn bind_execution_context(
        &self,
        execution_context: WindowExecutionContextIdentity,
    ) -> RendererPageSharedWorkerClientEventRealmSender {
        RendererPageSharedWorkerClientEventRealmSender {
            task_route: self.task_route.clone(),
            root_document: self.root_document,
            execution_context,
        }
    }
}

/// Page/Window-exact capability retained while SharedWorker connect allocates
/// the browser-context client id.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageSharedWorkerClientEventRealmSender {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageSharedWorkerClientEventTask>,
        RendererPageSharedWorkerClientEventReadySignal,
    >,
    root_document: RendererDocumentToken,
    execution_context: WindowExecutionContextIdentity,
}

impl RendererPageSharedWorkerClientEventRealmSender {
    pub(crate) fn bind_client(
        &self,
        client_id: SharedWorkerClientId,
    ) -> RendererPageSharedWorkerClientEventProducer {
        RendererPageSharedWorkerClientEventProducer {
            task_route: self.task_route.clone(),
            owner: RendererPageSharedWorkerClientEventOwner::new(
                self.root_document,
                self.execution_context,
                client_id,
            ),
        }
    }
}

/// Exact owner capability retained by one browser-context SharedWorker client.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageSharedWorkerClientEventProducer {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageSharedWorkerClientEventTask>,
        RendererPageSharedWorkerClientEventReadySignal,
    >,
    owner: RendererPageSharedWorkerClientEventOwner,
}

impl RendererPageSharedWorkerClientEventProducer {
    pub(crate) fn send(
        &self,
        event: SharedWorkerClientEvent,
    ) -> Result<(), RendererPageSharedWorkerClientEventRouteClosed> {
        self.task_route
            .send_and_signal_if_newly_ready(ReadyPageTask::new(
                RendererPageSharedWorkerClientEventTask::new(self.owner, event),
            ))
            .map_err(|_| RendererPageSharedWorkerClientEventRouteClosed)
    }
}

#[derive(Clone, Debug)]
struct RendererPageSharedWorkerClientEventReadySignal {
    owner_wake: RendererOwnerWakeSender,
}

impl OwnerTaskReadySignal for RendererPageSharedWorkerClientEventReadySignal {
    fn signal_ready(&self) {
        self.owner_wake.signal_shared_worker_client_event();
    }
}

/// Unique Page-lifetime consumer for SharedWorker client events.
#[derive(Debug)]
pub(crate) struct RendererPageSharedWorkerClientEventSource {
    source: OwnerReadyTaskSource<
        ReadyPageTask<RendererPageSharedWorkerClientEventTask>,
        RendererPageSharedWorkerClientEventReadySignal,
    >,
}

impl RendererPageSharedWorkerClientEventSource {
    pub(crate) fn new(owner_wake: RendererOwnerWakeSender) -> Self {
        Self {
            source: OwnerReadyTaskSource::new(RendererPageSharedWorkerClientEventReadySignal {
                owner_wake,
            }),
        }
    }

    pub(crate) fn route(&self) -> RendererPageSharedWorkerClientEventRoute {
        RendererPageSharedWorkerClientEventRoute {
            task_route: self.source.route(),
        }
    }

    pub(crate) fn next_ready_metadata(&mut self) -> Option<RendererPageTaskReadyMetadata> {
        self.source.front().map(ReadyPageTask::metadata)
    }

    pub(crate) fn next_ready_owner(&mut self) -> Option<RendererPageSharedWorkerClientEventOwner> {
        self.source.front().map(|ready| ready.value().owner())
    }

    pub(crate) fn pop_front(
        &mut self,
    ) -> Option<(
        RendererPageTaskReadyMetadata,
        RendererPageSharedWorkerClientEventTask,
    )> {
        self.source.pop_front().map(ReadyPageTask::into_parts)
    }

    pub(crate) fn has_ready_task(&mut self) -> bool {
        !self.source.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.source.clear_local();
    }

    pub(crate) fn route_matches(&self, route: &RendererPageSharedWorkerClientEventRoute) -> bool {
        route.same_route_as(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageSharedWorkerClientEventTargetEffect {
    /// The exact current endpoint consumed a `Closed` control event. No
    /// SharedWorker callback ran, but the selected task still owns its
    /// ordinary checkpoint.
    EndpointClosedByCurrentOwner,
    /// An error listener ran in the exact current wrapper realm.
    ErrorCallbackDispatchedToCurrentOwner {
        endpoint_disposition: SharedWorkerClientEndpointDisposition,
    },
    /// The exact current error was consumed, but no listener matched.
    CurrentOwnerErrorHadNoCallback {
        endpoint_disposition: SharedWorkerClientEndpointDisposition,
    },
    /// The queued event belonged to a retired root Document or Window realm
    /// and was discarded without entering V8.
    DiscardedStaleOwner {
        current_owner: Option<RendererPageSharedWorkerClientEventOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageSharedWorkerClientEventTurnAction {
    pub(crate) owner: RendererPageSharedWorkerClientEventOwner,
    pub(crate) event_kind: RendererSharedWorkerClientEventKind,
    pub(crate) target_effect: PageSharedWorkerClientEventTargetEffect,
}

pub(crate) type PageSharedWorkerClientEventTurnOutcome =
    PageOwnerTurnOutcome<PageSharedWorkerClientEventTurnAction>;
