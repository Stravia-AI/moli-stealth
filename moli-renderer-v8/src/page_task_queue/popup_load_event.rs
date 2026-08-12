use crate::{
    native_bridge::LightweightPopupNavigationTaskToken,
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::dom_manipulation::{RendererPageDomManipulationRoute, RendererPageDomManipulationTask};

/// PageVm namespace plus the exact popup Document navigation whose `load`
/// event became ready.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPagePopupLoadEventOwner {
    root_document: RendererDocumentToken,
    target: LightweightPopupNavigationTaskToken,
}

impl RendererPagePopupLoadEventOwner {
    pub(crate) const fn new(
        root_document: RendererDocumentToken,
        target: LightweightPopupNavigationTaskToken,
    ) -> Self {
        Self {
            root_document,
            target,
        }
    }

    pub(crate) const fn root_document(self) -> RendererDocumentToken {
        self.root_document
    }

    pub(crate) const fn target(self) -> LightweightPopupNavigationTaskToken {
        self.target
    }
}

#[derive(Debug)]
pub(crate) struct RendererPagePopupLoadEventTask {
    owner: RendererPagePopupLoadEventOwner,
}

impl RendererPagePopupLoadEventTask {
    fn new(owner: RendererPagePopupLoadEventOwner) -> Self {
        Self { owner }
    }

    pub(crate) const fn owner(&self) -> RendererPagePopupLoadEventOwner {
        self.owner
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPagePopupLoadEventRouteClosed;

#[derive(Clone, Debug)]
pub(crate) struct RendererPagePopupLoadEventSender {
    route: RendererPageDomManipulationRoute,
    root_document: RendererDocumentToken,
}

impl RendererPagePopupLoadEventSender {
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
        target: LightweightPopupNavigationTaskToken,
    ) -> Result<(), RendererPagePopupLoadEventRouteClosed> {
        let owner = RendererPagePopupLoadEventOwner::new(self.root_document, target);
        self.route
            .send(RendererPageDomManipulationTask::PopupLoadEvent(
                RendererPagePopupLoadEventTask::new(owner),
            ))
            .map_err(|_| RendererPagePopupLoadEventRouteClosed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PagePopupLoadEventTargetEffect {
    DispatchedToCurrentOwner,
    DiscardedStaleOwner {
        current_owner: Option<RendererPagePopupLoadEventOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PagePopupLoadEventTurnAction {
    pub(crate) owner: RendererPagePopupLoadEventOwner,
    pub(crate) target_effect: PagePopupLoadEventTargetEffect,
}

pub(crate) type PagePopupLoadEventTurnOutcome = PageOwnerTurnOutcome<PagePopupLoadEventTurnAction>;
