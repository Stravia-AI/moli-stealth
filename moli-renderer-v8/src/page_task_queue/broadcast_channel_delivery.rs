use crate::{
    native_bridge::WindowExecutionContextIdentity,
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
    types::BroadcastChannelId,
};

use super::dom_manipulation::{RendererPageDomManipulationRoute, RendererPageDomManipulationTask};

/// Exact execution owner of one page-side BroadcastChannel delivery.
///
/// The stable queue is Page-owned. The root token prevents a late delivery
/// from a retired PageVm from matching a replacement PageVm whose local owner
/// counters were reused, while the execution-context identity binds delivery
/// to the accepting Window realm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageBroadcastChannelDeliveryOwner {
    root_document: RendererDocumentToken,
    execution_context: WindowExecutionContextIdentity,
}

impl RendererPageBroadcastChannelDeliveryOwner {
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

/// One concrete channel delivery selected by the Page scheduler.
#[derive(Debug)]
pub(crate) struct RendererPageBroadcastChannelDeliveryTask {
    owner: RendererPageBroadcastChannelDeliveryOwner,
    channel_id: BroadcastChannelId,
}

impl RendererPageBroadcastChannelDeliveryTask {
    fn new(
        owner: RendererPageBroadcastChannelDeliveryOwner,
        channel_id: BroadcastChannelId,
    ) -> Self {
        Self { owner, channel_id }
    }

    pub(crate) const fn owner(&self) -> RendererPageBroadcastChannelDeliveryOwner {
        self.owner
    }

    pub(crate) const fn channel_id(&self) -> BroadcastChannelId {
        self.channel_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageBroadcastChannelDeliveryRouteClosed;

/// PageVm-generation-stamped route used to bind individual channel owners.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageBroadcastChannelDeliverySender {
    route: RendererPageDomManipulationRoute,
    root_document: RendererDocumentToken,
}

impl RendererPageBroadcastChannelDeliverySender {
    pub(super) fn new(
        route: RendererPageDomManipulationRoute,
        root_document: RendererDocumentToken,
    ) -> Self {
        Self {
            route,
            root_document,
        }
    }

    pub(crate) fn bind_execution_context(
        &self,
        execution_context: WindowExecutionContextIdentity,
    ) -> RendererPageBroadcastChannelDeliveryProducer {
        RendererPageBroadcastChannelDeliveryProducer {
            route: self.route.clone(),
            owner: RendererPageBroadcastChannelDeliveryOwner::new(
                self.root_document,
                execution_context,
            ),
        }
    }

    #[cfg(test)]
    pub(crate) fn same_route_as(&self, other: &Self) -> bool {
        self.route.same_route_as(&other.route)
    }
}

/// Exact owner capability stored by one BroadcastChannel registry entry.
///
/// The registry supplies only the channel id at wake time; all execution
/// authority was captured synchronously when that channel was constructed.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageBroadcastChannelDeliveryProducer {
    route: RendererPageDomManipulationRoute,
    owner: RendererPageBroadcastChannelDeliveryOwner,
}

impl RendererPageBroadcastChannelDeliveryProducer {
    pub(crate) fn send(
        &self,
        channel_id: BroadcastChannelId,
    ) -> Result<(), RendererPageBroadcastChannelDeliveryRouteClosed> {
        self.route
            .send(RendererPageDomManipulationTask::BroadcastChannel(
                RendererPageBroadcastChannelDeliveryTask::new(self.owner, channel_id),
            ))
            .map_err(|_| RendererPageBroadcastChannelDeliveryRouteClosed)
    }

    #[cfg(test)]
    pub(crate) const fn owner(&self) -> RendererPageBroadcastChannelDeliveryOwner {
        self.owner
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageBroadcastChannelDeliveryDocumentEffect {
    DispatchedToCurrentOwner,
    CurrentOwnerHadNoPendingEvent,
    DiscardedStaleOwner {
        current_owner: Option<RendererPageBroadcastChannelDeliveryOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageBroadcastChannelDeliveryTurnAction {
    pub(crate) owner: RendererPageBroadcastChannelDeliveryOwner,
    pub(crate) channel_id: BroadcastChannelId,
    pub(crate) document_effect: PageBroadcastChannelDeliveryDocumentEffect,
}

pub(crate) type PageBroadcastChannelDeliveryTurnOutcome =
    PageOwnerTurnOutcome<PageBroadcastChannelDeliveryTurnAction>;
