use crate::{
    native_bridge::WindowTaskTarget,
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::dom_manipulation::{RendererPageDomManipulationRoute, RendererPageDomManipulationTask};

/// Immutable data captured when one Window queues a `hashchange` event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageHashChangeData {
    old_url: String,
    new_url: String,
}

impl RendererPageHashChangeData {
    pub(crate) fn new(old_url: String, new_url: String) -> Self {
        Self { old_url, new_url }
    }

    pub(crate) fn old_url(&self) -> &str {
        &self.old_url
    }

    pub(crate) fn new_url(&self) -> &str {
        &self.new_url
    }
}

/// PageVm namespace plus the exact LocalDOMWindow that queued the event.
///
/// Like other Window tasks, `hashchange` survives `document.open()` in the
/// same LocalDOMWindow but cannot cross Window replacement or PageVm reuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageHashChangeDeliveryOwner {
    root_document: RendererDocumentToken,
    target: WindowTaskTarget,
}

impl RendererPageHashChangeDeliveryOwner {
    pub(crate) const fn new(
        root_document: RendererDocumentToken,
        target: WindowTaskTarget,
    ) -> Self {
        Self {
            root_document,
            target,
        }
    }

    pub(crate) const fn target(self) -> WindowTaskTarget {
        self.target
    }

    #[cfg(test)]
    pub(crate) const fn root_document(self) -> RendererDocumentToken {
        self.root_document
    }
}

#[derive(Debug)]
pub(crate) struct RendererPageHashChangeDeliveryTask {
    owner: RendererPageHashChangeDeliveryOwner,
    data: RendererPageHashChangeData,
}

impl RendererPageHashChangeDeliveryTask {
    pub(super) fn new(
        owner: RendererPageHashChangeDeliveryOwner,
        data: RendererPageHashChangeData,
    ) -> Self {
        Self { owner, data }
    }

    pub(crate) const fn owner(&self) -> RendererPageHashChangeDeliveryOwner {
        self.owner
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        RendererPageHashChangeDeliveryOwner,
        RendererPageHashChangeData,
    ) {
        (self.owner, self.data)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageHashChangeDeliveryRouteClosed;

#[derive(Clone, Debug)]
pub(crate) struct RendererPageHashChangeDeliverySender {
    route: RendererPageDomManipulationRoute,
    root_document: RendererDocumentToken,
}

impl RendererPageHashChangeDeliverySender {
    pub(super) fn new(
        route: RendererPageDomManipulationRoute,
        root_document: RendererDocumentToken,
    ) -> Self {
        Self {
            route,
            root_document,
        }
    }

    pub(crate) fn send(
        &self,
        target: WindowTaskTarget,
        data: RendererPageHashChangeData,
    ) -> Result<(), RendererPageHashChangeDeliveryRouteClosed> {
        let owner = RendererPageHashChangeDeliveryOwner::new(self.root_document, target);
        self.route
            .send(RendererPageDomManipulationTask::HashChange(
                RendererPageHashChangeDeliveryTask::new(owner, data),
            ))
            .map_err(|_| RendererPageHashChangeDeliveryRouteClosed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageHashChangeDeliveryTargetEffect {
    DispatchedToCurrentOwner,
    CurrentOwnerHadNoEventTarget,
    DiscardedStaleOwner {
        current_owner: Option<RendererPageHashChangeDeliveryOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageHashChangeDeliveryTurnAction {
    pub(crate) owner: RendererPageHashChangeDeliveryOwner,
    pub(crate) target_effect: PageHashChangeDeliveryTargetEffect,
}

pub(crate) type PageHashChangeDeliveryTurnOutcome =
    PageOwnerTurnOutcome<PageHashChangeDeliveryTurnAction>;
