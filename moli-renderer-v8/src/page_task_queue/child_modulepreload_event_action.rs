use moli_owner_queue::{OwnerReadyTaskRoute, OwnerReadyTaskSource, OwnerTaskReadySignal};

use crate::{
    frame_owner_model::{
        FrameDocumentModulepreloadEventAction, FrameDocumentModulepreloadTerminalOutcome,
        FrameDocumentTaskOwner, FrameRealmId,
    },
    resource_ready::{ReadyPageTask, RendererPageTaskReadyMetadata},
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::RendererOwnerWakeSender;

/// Stable Page namespace plus the exact child Document/realm that owns one
/// modulepreload load/error event action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageChildModulepreloadEventActionOwner {
    root_document: RendererDocumentToken,
    document_owner: FrameDocumentTaskOwner,
    realm_id: FrameRealmId,
}

impl RendererPageChildModulepreloadEventActionOwner {
    pub(crate) const fn new(
        root_document: RendererDocumentToken,
        document_owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
    ) -> Self {
        Self {
            root_document,
            document_owner,
            realm_id,
        }
    }

    pub(crate) const fn root_document(self) -> RendererDocumentToken {
        self.root_document
    }

    pub(crate) const fn document_owner(self) -> FrameDocumentTaskOwner {
        self.document_owner
    }

    pub(crate) const fn realm_id(self) -> FrameRealmId {
        self.realm_id
    }
}

#[derive(Debug)]
pub(crate) struct RendererPageChildModulepreloadEventActionTask {
    owner: RendererPageChildModulepreloadEventActionOwner,
    action: FrameDocumentModulepreloadEventAction,
}

impl RendererPageChildModulepreloadEventActionTask {
    fn new(
        root_document: RendererDocumentToken,
        action: FrameDocumentModulepreloadEventAction,
    ) -> Self {
        let owner = RendererPageChildModulepreloadEventActionOwner::new(
            root_document,
            action.owner(),
            action.realm_id(),
        );
        Self { owner, action }
    }

    pub(crate) const fn owner(&self) -> RendererPageChildModulepreloadEventActionOwner {
        self.owner
    }

    pub(crate) fn into_action(self) -> FrameDocumentModulepreloadEventAction {
        self.action
    }
}

#[derive(Debug)]
pub(crate) struct RendererPageChildModulepreloadEventActionRouteClosed(
    Box<FrameDocumentModulepreloadEventAction>,
);

impl RendererPageChildModulepreloadEventActionRouteClosed {
    pub(crate) fn into_action(self) -> FrameDocumentModulepreloadEventAction {
        *self.0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RendererPageChildModulepreloadEventActionRoute {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageChildModulepreloadEventActionTask>,
        ChildModulepreloadEventActionReadySignal,
    >,
}

impl RendererPageChildModulepreloadEventActionRoute {
    pub(crate) fn sender(
        &self,
        root_document: RendererDocumentToken,
    ) -> RendererPageChildModulepreloadEventActionSender {
        RendererPageChildModulepreloadEventActionSender {
            task_route: self.task_route.clone(),
            root_document,
        }
    }

    fn same_route_as(&self, source: &RendererPageChildModulepreloadEventActionSource) -> bool {
        self.task_route.same_source_as(&source.source)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RendererPageChildModulepreloadEventActionSender {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageChildModulepreloadEventActionTask>,
        ChildModulepreloadEventActionReadySignal,
    >,
    root_document: RendererDocumentToken,
}

impl RendererPageChildModulepreloadEventActionSender {
    pub(crate) fn send(
        &self,
        action: FrameDocumentModulepreloadEventAction,
    ) -> Result<(), RendererPageChildModulepreloadEventActionRouteClosed> {
        self.task_route
            .send_and_signal_if_newly_ready(ReadyPageTask::new(
                RendererPageChildModulepreloadEventActionTask::new(self.root_document, action),
            ))
            .map_err(|error| {
                let (_, task) = error.0.into_parts();
                RendererPageChildModulepreloadEventActionRouteClosed(Box::new(task.into_action()))
            })
    }
}

#[derive(Clone, Debug)]
struct ChildModulepreloadEventActionReadySignal {
    owner_wake: RendererOwnerWakeSender,
}

impl OwnerTaskReadySignal for ChildModulepreloadEventActionReadySignal {
    fn signal_ready(&self) {
        self.owner_wake.signal_child_modulepreload_event_action();
    }
}

/// Unique Page-lifetime consumer for child modulepreload event actions.
#[derive(Debug)]
pub(crate) struct RendererPageChildModulepreloadEventActionSource {
    source: OwnerReadyTaskSource<
        ReadyPageTask<RendererPageChildModulepreloadEventActionTask>,
        ChildModulepreloadEventActionReadySignal,
    >,
}

impl RendererPageChildModulepreloadEventActionSource {
    pub(crate) fn new(owner_wake: RendererOwnerWakeSender) -> Self {
        Self {
            source: OwnerReadyTaskSource::new(ChildModulepreloadEventActionReadySignal {
                owner_wake,
            }),
        }
    }

    pub(crate) fn route(&self) -> RendererPageChildModulepreloadEventActionRoute {
        RendererPageChildModulepreloadEventActionRoute {
            task_route: self.source.route(),
        }
    }

    pub(crate) fn next_ready_metadata(&mut self) -> Option<RendererPageTaskReadyMetadata> {
        self.source.front().map(ReadyPageTask::metadata)
    }

    pub(crate) fn next_ready_owner(
        &mut self,
    ) -> Option<RendererPageChildModulepreloadEventActionOwner> {
        self.source.front().map(|ready| ready.value().owner())
    }

    pub(crate) fn pop_front(
        &mut self,
    ) -> Option<(
        RendererPageTaskReadyMetadata,
        RendererPageChildModulepreloadEventActionTask,
    )> {
        self.source.pop_front().map(ReadyPageTask::into_parts)
    }

    pub(crate) fn has_ready_task(&mut self) -> bool {
        !self.source.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.source.clear_local();
    }

    pub(crate) fn route_matches(
        &self,
        route: &RendererPageChildModulepreloadEventActionRoute,
    ) -> bool {
        route.same_route_as(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageChildModulepreloadEventActionTargetEffect {
    AppliedToCurrentOwner {
        outcome: FrameDocumentModulepreloadTerminalOutcome,
    },
    DiscardedStaleOwner {
        current_owner: Option<RendererPageChildModulepreloadEventActionOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageChildModulepreloadEventActionTurnAction {
    pub(crate) owner: RendererPageChildModulepreloadEventActionOwner,
    pub(crate) target_effect: PageChildModulepreloadEventActionTargetEffect,
}

pub(crate) type PageChildModulepreloadEventActionTurnOutcome =
    PageOwnerTurnOutcome<PageChildModulepreloadEventActionTurnAction>;
