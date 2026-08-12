use crate::{
    native_bridge::WindowTaskTarget,
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::dom_manipulation::{RendererPageDomManipulationRoute, RendererPageDomManipulationTask};

/// PageVm-local key for one V8-backed view-transition update callback.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RendererPageViewTransitionUpdateTaskId(u64);

impl RendererPageViewTransitionUpdateTaskId {
    pub(crate) const fn first() -> Self {
        Self(1)
    }

    pub(crate) const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Exact LocalDOMWindow that owns one queued update callback.
///
/// Blink posts this callback to the Document's execution context. Binding it
/// to the LocalDOMWindow intentionally preserves it across `document.open()`
/// while rejecting actual Window replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageViewTransitionUpdateOwner {
    root_document: RendererDocumentToken,
    target: WindowTaskTarget,
}

impl RendererPageViewTransitionUpdateOwner {
    pub(crate) const fn new(
        root_document: RendererDocumentToken,
        target: WindowTaskTarget,
    ) -> Self {
        Self {
            root_document,
            target,
        }
    }

    #[cfg(test)]
    pub(crate) const fn root_document(self) -> RendererDocumentToken {
        self.root_document
    }

    pub(crate) const fn target(self) -> WindowTaskTarget {
        self.target
    }
}

#[derive(Debug)]
pub(crate) struct RendererPageViewTransitionUpdateTask {
    owner: RendererPageViewTransitionUpdateOwner,
    task_id: RendererPageViewTransitionUpdateTaskId,
}

impl RendererPageViewTransitionUpdateTask {
    fn new(
        owner: RendererPageViewTransitionUpdateOwner,
        task_id: RendererPageViewTransitionUpdateTaskId,
    ) -> Self {
        Self { owner, task_id }
    }

    pub(crate) const fn owner(&self) -> RendererPageViewTransitionUpdateOwner {
        self.owner
    }

    pub(crate) const fn task_id(&self) -> RendererPageViewTransitionUpdateTaskId {
        self.task_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageViewTransitionUpdateRouteClosed;

#[derive(Clone, Debug)]
pub(crate) struct RendererPageViewTransitionUpdateSender {
    route: RendererPageDomManipulationRoute,
    root_document: RendererDocumentToken,
}

impl RendererPageViewTransitionUpdateSender {
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
        task_id: RendererPageViewTransitionUpdateTaskId,
    ) -> Result<RendererPageViewTransitionUpdateOwner, RendererPageViewTransitionUpdateRouteClosed>
    {
        let owner = RendererPageViewTransitionUpdateOwner::new(self.root_document, target);
        self.route
            .send(RendererPageDomManipulationTask::ViewTransitionUpdate(
                RendererPageViewTransitionUpdateTask::new(owner, task_id),
            ))
            .map_err(|_| RendererPageViewTransitionUpdateRouteClosed)?;
        Ok(owner)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageViewTransitionUpdateTargetEffect {
    /// The exact pending transition was consumed for the current owner.
    ///
    /// A supplied callback may return, throw, or already belong to a retired
    /// Realm; the transition lifecycle converts each case into its own Promise
    /// settlement. This variant deliberately does not claim that JavaScript
    /// callback code necessarily ran.
    ProcessedForCurrentOwner,
    CurrentOwnerHadNoPendingCallback,
    DiscardedStaleOwner {
        current_owner: Option<RendererPageViewTransitionUpdateOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageViewTransitionUpdateTurnAction {
    pub(crate) owner: RendererPageViewTransitionUpdateOwner,
    pub(crate) task_id: RendererPageViewTransitionUpdateTaskId,
    pub(crate) target_effect: PageViewTransitionUpdateTargetEffect,
}

pub(crate) type PageViewTransitionUpdateTurnOutcome =
    PageOwnerTurnOutcome<PageViewTransitionUpdateTurnAction>;
