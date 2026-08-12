use crate::{
    native_bridge::WindowDocumentTaskTarget,
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::{
    RendererPageWindowDocumentTask, RendererPageWindowDocumentTaskOwner,
    dom_manipulation::{RendererPageDomManipulationRoute, RendererPageDomManipulationTask},
};

/// Host-local key for one pending automatic text-track mode selection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RendererPageTextTrackDefaultModeTaskId(u64);

impl RendererPageTextTrackDefaultModeTaskId {
    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RendererPageTextTrackDefaultModeTaskKind {
    Apply,
}

pub(crate) type RendererPageTextTrackDefaultModeOwner = RendererPageWindowDocumentTaskOwner;
pub(crate) type RendererPageTextTrackDefaultModeTask = RendererPageWindowDocumentTask<
    RendererPageTextTrackDefaultModeTaskId,
    RendererPageTextTrackDefaultModeTaskKind,
>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageTextTrackDefaultModeRouteClosed;

/// PageVm-stamped producer derived from the shared DOM-manipulation route.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageTextTrackDefaultModeSender {
    route: RendererPageDomManipulationRoute,
    root_document: RendererDocumentToken,
}

impl RendererPageTextTrackDefaultModeSender {
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
        target: WindowDocumentTaskTarget,
        task_id: RendererPageTextTrackDefaultModeTaskId,
        kind: RendererPageTextTrackDefaultModeTaskKind,
    ) -> Result<(), RendererPageTextTrackDefaultModeRouteClosed> {
        self.route
            .send(RendererPageDomManipulationTask::TextTrackDefaultMode(
                RendererPageTextTrackDefaultModeTask::new(
                    RendererPageTextTrackDefaultModeOwner::new(self.root_document, target),
                    task_id,
                    kind,
                ),
            ))
            .map_err(|_| RendererPageTextTrackDefaultModeRouteClosed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageTextTrackDefaultModeTargetEffect {
    AppliedToCurrentOwner,
    CurrentOwnerNoLongerEligible,
    DiscardedStaleOwner {
        current_owner: Option<RendererPageTextTrackDefaultModeOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageTextTrackDefaultModeTurnAction {
    pub(crate) owner: RendererPageTextTrackDefaultModeOwner,
    pub(crate) task_id: RendererPageTextTrackDefaultModeTaskId,
    pub(crate) kind: RendererPageTextTrackDefaultModeTaskKind,
    pub(crate) target_effect: PageTextTrackDefaultModeTargetEffect,
}

pub(crate) type PageTextTrackDefaultModeTurnOutcome =
    PageOwnerTurnOutcome<PageTextTrackDefaultModeTurnAction>;
