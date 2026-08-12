use moli_owner_queue::{OwnerReadyTaskRoute, OwnerReadyTaskSource, OwnerTaskReadySignal};

use crate::{
    native_bridge::WindowExecutionContextIdentity,
    resource_ready::{ReadyPageTask, RendererPageTaskReadyMetadata},
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
    types::MessagePortId,
};

use super::RendererOwnerWakeSender;

/// Exact Page-side attachment that owned a MessagePort when delivery became
/// runnable.
///
/// A port can move between Window realms without changing its registry id. The
/// root token rejects a retired PageVm, while the execution-context identity
/// prevents an old wake from delivering through a wrapper installed by a later
/// transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageMessagePortDeliveryOwner {
    root_document: RendererDocumentToken,
    execution_context: WindowExecutionContextIdentity,
}

impl RendererPageMessagePortDeliveryOwner {
    pub(crate) const fn new(
        root_document: RendererDocumentToken,
        execution_context: WindowExecutionContextIdentity,
    ) -> Self {
        Self {
            root_document,
            execution_context,
        }
    }

    pub(crate) const fn execution_context(self) -> WindowExecutionContextIdentity {
        self.execution_context
    }
}

/// One concrete MessagePort delivery opportunity selected by the Page
/// scheduler. The structured-clone payload remains in the shared port registry.
#[derive(Debug)]
pub(crate) struct RendererPageMessagePortDeliveryTask {
    owner: RendererPageMessagePortDeliveryOwner,
    port_id: MessagePortId,
}

impl RendererPageMessagePortDeliveryTask {
    fn new(owner: RendererPageMessagePortDeliveryOwner, port_id: MessagePortId) -> Self {
        Self { owner, port_id }
    }

    pub(crate) const fn owner(&self) -> RendererPageMessagePortDeliveryOwner {
        self.owner
    }

    pub(crate) const fn port_id(&self) -> MessagePortId {
        self.port_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageMessagePortDeliveryRouteClosed;

#[derive(Clone, Debug)]
pub(crate) struct RendererPageMessagePortDeliveryRoute {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageMessagePortDeliveryTask>,
        RendererPageMessagePortDeliveryReadySignal,
    >,
}

impl RendererPageMessagePortDeliveryRoute {
    pub(crate) fn sender(
        &self,
        root_document: RendererDocumentToken,
    ) -> RendererPageMessagePortDeliverySender {
        RendererPageMessagePortDeliverySender {
            task_route: self.task_route.clone(),
            root_document,
        }
    }

    fn same_route_as(&self, source: &RendererPageMessagePortDeliverySource) -> bool {
        self.task_route.same_source_as(&source.source)
    }
}

/// PageVm-stamped route used to bind a port attachment to its accepting Window
/// realm.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageMessagePortDeliverySender {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageMessagePortDeliveryTask>,
        RendererPageMessagePortDeliveryReadySignal,
    >,
    root_document: RendererDocumentToken,
}

impl RendererPageMessagePortDeliverySender {
    pub(crate) fn bind_execution_context(
        &self,
        execution_context: WindowExecutionContextIdentity,
    ) -> RendererPageMessagePortDeliveryProducer {
        RendererPageMessagePortDeliveryProducer {
            task_route: self.task_route.clone(),
            owner: RendererPageMessagePortDeliveryOwner::new(self.root_document, execution_context),
        }
    }
}

/// Exact owner capability stored by one page-side MessagePort attachment.
///
/// Transfer replaces this producer in the registry. Already queued tasks keep
/// the old owner and are harmlessly ignored by the Page arbiter.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageMessagePortDeliveryProducer {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageMessagePortDeliveryTask>,
        RendererPageMessagePortDeliveryReadySignal,
    >,
    owner: RendererPageMessagePortDeliveryOwner,
}

impl RendererPageMessagePortDeliveryProducer {
    pub(crate) fn send(
        &self,
        port_id: MessagePortId,
    ) -> Result<(), RendererPageMessagePortDeliveryRouteClosed> {
        self.task_route
            .send_and_signal_if_newly_ready(ReadyPageTask::new(
                RendererPageMessagePortDeliveryTask::new(self.owner, port_id),
            ))
            .map_err(|_| RendererPageMessagePortDeliveryRouteClosed)
    }

    #[cfg(test)]
    pub(crate) const fn owner(&self) -> RendererPageMessagePortDeliveryOwner {
        self.owner
    }
}

#[derive(Clone, Debug)]
struct RendererPageMessagePortDeliveryReadySignal {
    owner_wake: RendererOwnerWakeSender,
}

impl OwnerTaskReadySignal for RendererPageMessagePortDeliveryReadySignal {
    fn signal_ready(&self) {
        self.owner_wake.signal_message_port_delivery();
    }
}

/// Unique Page-lifetime consumer for page-side MessagePort delivery tasks.
#[derive(Debug)]
pub(crate) struct RendererPageMessagePortDeliverySource {
    source: OwnerReadyTaskSource<
        ReadyPageTask<RendererPageMessagePortDeliveryTask>,
        RendererPageMessagePortDeliveryReadySignal,
    >,
}

impl RendererPageMessagePortDeliverySource {
    pub(crate) fn new(owner_wake: RendererOwnerWakeSender) -> Self {
        Self {
            source: OwnerReadyTaskSource::new(RendererPageMessagePortDeliveryReadySignal {
                owner_wake,
            }),
        }
    }

    pub(crate) fn route(&self) -> RendererPageMessagePortDeliveryRoute {
        RendererPageMessagePortDeliveryRoute {
            task_route: self.source.route(),
        }
    }

    pub(crate) fn next_ready_metadata(&mut self) -> Option<RendererPageTaskReadyMetadata> {
        self.source.front().map(ReadyPageTask::metadata)
    }

    pub(crate) fn next_ready_owner(&mut self) -> Option<RendererPageMessagePortDeliveryOwner> {
        self.source.front().map(|ready| ready.value().owner())
    }

    pub(crate) fn next_ready_port_id(&mut self) -> Option<MessagePortId> {
        self.source.front().map(|ready| ready.value().port_id())
    }

    pub(crate) fn pop_front(
        &mut self,
    ) -> Option<(
        RendererPageTaskReadyMetadata,
        RendererPageMessagePortDeliveryTask,
    )> {
        self.source.pop_front().map(ReadyPageTask::into_parts)
    }

    pub(crate) fn has_ready_task(&mut self) -> bool {
        !self.source.is_empty()
    }

    pub(crate) fn has_ready_task_for(
        &mut self,
        owner: RendererPageMessagePortDeliveryOwner,
        port_id: MessagePortId,
    ) -> bool {
        self.source.has_matching_task(|ready| {
            let task = ready.value();
            task.owner() == owner && task.port_id() == port_id
        })
    }

    pub(crate) fn clear(&mut self) {
        self.source.clear_local();
    }

    pub(crate) fn route_matches(&self, route: &RendererPageMessagePortDeliveryRoute) -> bool {
        route.same_route_as(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageMessagePortDeliveryTargetEffect {
    ConsumedByCurrentOwner {
        callback_dispatched: bool,
    },
    CurrentOwnerHadNoReadyEvent,
    IgnoredStaleOwner {
        current_owner: Option<RendererPageMessagePortDeliveryOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageMessagePortDeliveryTurnAction {
    pub(crate) owner: RendererPageMessagePortDeliveryOwner,
    pub(crate) port_id: MessagePortId,
    pub(crate) target_effect: PageMessagePortDeliveryTargetEffect,
}

pub(crate) type PageMessagePortDeliveryTurnOutcome =
    PageOwnerTurnOutcome<PageMessagePortDeliveryTurnAction>;
