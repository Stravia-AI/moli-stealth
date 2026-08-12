use crate::{
    frame_owner_model::FrameLaneNavigationCommitTask,
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::navigation_and_traversal::{
    RendererPageNavigationAndTraversalRoute, RendererPageNavigationAndTraversalTask,
};

/// Exact PageVm namespace and child scheduler-lane navigation generation.
///
/// `FrameDocumentNavigationLoadBinding::navigation_id` changes whenever a
/// newer navigation replaces an uncommitted request. That makes an old stable
/// task stale without requiring removal from the middle of the Page source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageChildNavigationCommitOwner {
    root_document: RendererDocumentToken,
    commit: FrameLaneNavigationCommitTask,
}

impl RendererPageChildNavigationCommitOwner {
    pub(crate) const fn new(
        root_document: RendererDocumentToken,
        commit: FrameLaneNavigationCommitTask,
    ) -> Self {
        Self {
            root_document,
            commit,
        }
    }

    pub(crate) const fn root_document(self) -> RendererDocumentToken {
        self.root_document
    }

    pub(crate) const fn commit(self) -> FrameLaneNavigationCommitTask {
        self.commit
    }
}

/// One intrinsically-runnable child navigation commit in the shared HTML
/// navigation-and-traversal source.
#[derive(Debug)]
pub(crate) struct RendererPageChildNavigationCommitTask {
    owner: RendererPageChildNavigationCommitOwner,
}

impl RendererPageChildNavigationCommitTask {
    fn new(owner: RendererPageChildNavigationCommitOwner) -> Self {
        Self { owner }
    }

    pub(crate) const fn owner(&self) -> RendererPageChildNavigationCommitOwner {
        self.owner
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageChildNavigationCommitRouteClosed;

/// PageVm-stamped child-navigation producer derived from the existing
/// navigation-and-traversal family capability.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageChildNavigationCommitSender {
    route: RendererPageNavigationAndTraversalRoute,
    root_document: RendererDocumentToken,
}

impl RendererPageChildNavigationCommitSender {
    pub(super) fn new(
        route: RendererPageNavigationAndTraversalRoute,
        root_document: RendererDocumentToken,
    ) -> Self {
        Self {
            route,
            root_document,
        }
    }

    pub(crate) fn send(
        &self,
        commit: FrameLaneNavigationCommitTask,
    ) -> Result<(), RendererPageChildNavigationCommitRouteClosed> {
        self.route
            .send(
                RendererPageNavigationAndTraversalTask::ChildNavigationCommit(
                    RendererPageChildNavigationCommitTask::new(
                        RendererPageChildNavigationCommitOwner::new(self.root_document, commit),
                    ),
                ),
            )
            .map_err(|_| RendererPageChildNavigationCommitRouteClosed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageChildNavigationCommitTargetEffect {
    AppliedToCurrentOwner,
    DiscardedStaleOwner {
        current_owner: Option<RendererPageChildNavigationCommitOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageChildNavigationCommitTurnAction {
    pub(crate) owner: RendererPageChildNavigationCommitOwner,
    pub(crate) target_effect: PageChildNavigationCommitTargetEffect,
}

pub(crate) type PageChildNavigationCommitTurnOutcome =
    PageOwnerTurnOutcome<PageChildNavigationCommitTurnAction>;
