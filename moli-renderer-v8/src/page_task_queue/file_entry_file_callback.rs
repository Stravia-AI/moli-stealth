use crate::{
    native_bridge::WindowDocumentTaskTarget,
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::{
    RendererPageWindowDocumentTask, RendererPageWindowDocumentTaskOwner,
    dom_manipulation::{RendererPageDomManipulationRoute, RendererPageDomManipulationTask},
};

/// Host-local key for one pending `FileSystemFileEntry.file()` callback.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RendererPageFileEntryFileCallbackTaskId(u64);

impl RendererPageFileEntryFileCallbackTaskId {
    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// The current in-memory FileEntry implementation can only settle success.
///
/// Keep the operation typed even with one variant: the optional error
/// callback is still converted synchronously by Web IDL, but no fabricated
/// asynchronous error is published when the entry already owns its File.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RendererPageFileEntryFileCallbackTaskKind {
    Success,
}

pub(crate) type RendererPageFileEntryFileCallbackOwner = RendererPageWindowDocumentTaskOwner;
pub(crate) type RendererPageFileEntryFileCallbackTask = RendererPageWindowDocumentTask<
    RendererPageFileEntryFileCallbackTaskId,
    RendererPageFileEntryFileCallbackTaskKind,
>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageFileEntryFileCallbackRouteClosed;

/// PageVm-stamped producer derived from the shared DOM-manipulation route.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageFileEntryFileCallbackSender {
    route: RendererPageDomManipulationRoute,
    root_document: RendererDocumentToken,
}

impl RendererPageFileEntryFileCallbackSender {
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
        task_id: RendererPageFileEntryFileCallbackTaskId,
        kind: RendererPageFileEntryFileCallbackTaskKind,
    ) -> Result<(), RendererPageFileEntryFileCallbackRouteClosed> {
        self.route
            .send(RendererPageDomManipulationTask::FileEntryFileCallback(
                RendererPageFileEntryFileCallbackTask::new(
                    RendererPageFileEntryFileCallbackOwner::new(self.root_document, target),
                    task_id,
                    kind,
                ),
            ))
            .map_err(|_| RendererPageFileEntryFileCallbackRouteClosed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageFileEntryFileCallbackTargetEffect {
    CallbackInvokedForCurrentOwner,
    CurrentOwnerCallbackRetired,
    DiscardedStaleOwner {
        current_owner: Option<RendererPageFileEntryFileCallbackOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageFileEntryFileCallbackTurnAction {
    pub(crate) owner: RendererPageFileEntryFileCallbackOwner,
    pub(crate) task_id: RendererPageFileEntryFileCallbackTaskId,
    pub(crate) kind: RendererPageFileEntryFileCallbackTaskKind,
    pub(crate) target_effect: PageFileEntryFileCallbackTargetEffect,
}

pub(crate) type PageFileEntryFileCallbackTurnOutcome =
    PageOwnerTurnOutcome<PageFileEntryFileCallbackTurnAction>;
