use crate::{
    document_runtime::DomHandle,
    native_bridge::{ImageLoadEventId, WindowDocumentTaskTarget},
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::{
    RendererPageWindowDocumentTask, RendererPageWindowDocumentTaskOwner,
    dom_manipulation::{RendererPageDomManipulationRoute, RendererPageDomManipulationTask},
};

/// Stable task key for one terminal image request sequence.
///
/// The element handle keeps Host lookup O(1); the monotonically allocated
/// sequence prevents a task for an older request from consuming a replacement
/// request on the same element.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RendererPageImageLoadEventTaskId {
    element: DomHandle,
    sequence: ImageLoadEventId,
}

impl RendererPageImageLoadEventTaskId {
    pub(crate) const fn new(element: DomHandle, sequence: ImageLoadEventId) -> Self {
        Self { element, sequence }
    }

    pub(crate) const fn element(self) -> DomHandle {
        self.element
    }

    pub(crate) const fn sequence(self) -> ImageLoadEventId {
        self.sequence
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RendererPageImageLoadEventKind {
    Load,
    Error,
}

pub(crate) type RendererPageImageLoadEventOwner = RendererPageWindowDocumentTaskOwner;
pub(crate) type RendererPageImageLoadEventTask = RendererPageWindowDocumentTask<
    RendererPageImageLoadEventTaskId,
    RendererPageImageLoadEventKind,
>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageImageLoadEventRouteClosed;

/// PageVm-stamped producer derived from the shared DOM-manipulation route.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageImageLoadEventSender {
    route: RendererPageDomManipulationRoute,
    root_document: RendererDocumentToken,
}

impl RendererPageImageLoadEventSender {
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
        task_id: RendererPageImageLoadEventTaskId,
        kind: RendererPageImageLoadEventKind,
    ) -> Result<(), RendererPageImageLoadEventRouteClosed> {
        self.route
            .send(RendererPageDomManipulationTask::ImageLoadEvent(
                RendererPageImageLoadEventTask::new(
                    RendererPageImageLoadEventOwner::new(self.root_document, target),
                    task_id,
                    kind,
                ),
            ))
            .map_err(|_| RendererPageImageLoadEventRouteClosed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageImageLoadEventTargetEffect {
    DispatchedToCurrentOwner,
    SettledCurrentOwnerWithoutEvent,
    DiscardedStaleOwner {
        current_owner: Option<RendererPageImageLoadEventOwner>,
        stale_payload_effect: PageImageLoadEventStalePayloadEffect,
    },
}

/// What the exact-owner arbiter did with a stale image task's Host payload.
///
/// A stale scheduler task is not sufficient authority to enter V8. Only an
/// exact payload that was both owned by the current PageVm namespace and
/// successfully settled may have made `image.decode()` promises ready; that
/// fact requires a selected-task completion. Foreign PageVm state is never
/// retired by the stale task, and a missing or already-retired local payload
/// does not manufacture a checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageImageLoadEventStalePayloadEffect {
    ForeignPageVmStatePreserved,
    NoSettledExactPayload,
    SettledExactPayloadAndProcessedDecodeRequests,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageImageLoadEventTurnAction {
    pub(crate) owner: RendererPageImageLoadEventOwner,
    pub(crate) task_id: RendererPageImageLoadEventTaskId,
    pub(crate) kind: RendererPageImageLoadEventKind,
    pub(crate) target_effect: PageImageLoadEventTargetEffect,
}

pub(crate) type PageImageLoadEventTurnOutcome = PageOwnerTurnOutcome<PageImageLoadEventTurnAction>;
