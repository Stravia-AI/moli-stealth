use moli_owner_queue::{OwnerReadyTaskRoute, OwnerReadyTaskSource, OwnerTaskReadySignal};

use crate::{
    frame_owner_model::{
        FrameDocumentModuleScriptTerminalBatchTask, FrameDocumentModuleScriptTerminalOutcome,
        FrameDocumentTaskOwner, FrameRealmId,
    },
    resource_ready::{ReadyPageTask, RendererPageTaskReadyMetadata},
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::RendererOwnerWakeSender;

/// Stable Page namespace plus the exact child Document/realm that owns one
/// module-map terminal fanout action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageChildModuleScriptTerminalOwner {
    root_document: RendererDocumentToken,
    document_owner: FrameDocumentTaskOwner,
    realm_id: FrameRealmId,
}

impl RendererPageChildModuleScriptTerminalOwner {
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

/// One module-map terminal fanout captured at its producing child realm.
///
/// A batch remains one Page action because every item is a client notification
/// for the same module-map terminal. The runner may enqueue later task-source
/// work, but it never executes those follow-ups in this turn.
#[derive(Debug)]
pub(crate) struct RendererPageChildModuleScriptTerminalTask {
    owner: RendererPageChildModuleScriptTerminalOwner,
    terminal: FrameDocumentModuleScriptTerminalBatchTask,
}

impl RendererPageChildModuleScriptTerminalTask {
    fn new(
        root_document: RendererDocumentToken,
        terminal: FrameDocumentModuleScriptTerminalBatchTask,
    ) -> Self {
        let owner = RendererPageChildModuleScriptTerminalOwner::new(
            root_document,
            terminal.owner(),
            terminal.realm_id(),
        );
        Self { owner, terminal }
    }

    pub(crate) const fn owner(&self) -> RendererPageChildModuleScriptTerminalOwner {
        self.owner
    }

    pub(crate) fn into_terminal(self) -> FrameDocumentModuleScriptTerminalBatchTask {
        self.terminal
    }
}

#[derive(Debug)]
pub(crate) struct RendererPageChildModuleScriptTerminalRouteClosed(
    Box<FrameDocumentModuleScriptTerminalBatchTask>,
);

impl RendererPageChildModuleScriptTerminalRouteClosed {
    pub(crate) fn into_terminal(self) -> FrameDocumentModuleScriptTerminalBatchTask {
        *self.0
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RendererPageChildModuleScriptTerminalRoute {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageChildModuleScriptTerminalTask>,
        ChildModuleScriptTerminalReadySignal,
    >,
}

impl RendererPageChildModuleScriptTerminalRoute {
    pub(crate) fn sender(
        &self,
        root_document: RendererDocumentToken,
    ) -> RendererPageChildModuleScriptTerminalSender {
        RendererPageChildModuleScriptTerminalSender {
            task_route: self.task_route.clone(),
            root_document,
        }
    }

    fn same_route_as(&self, source: &RendererPageChildModuleScriptTerminalSource) -> bool {
        self.task_route.same_source_as(&source.source)
    }
}

/// Document-stamped producer for child module terminal fanout actions.
///
/// Closing the stable Page route rejects the original terminal. There is no
/// fallback to the deleted child-frame pump.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageChildModuleScriptTerminalSender {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageChildModuleScriptTerminalTask>,
        ChildModuleScriptTerminalReadySignal,
    >,
    root_document: RendererDocumentToken,
}

impl RendererPageChildModuleScriptTerminalSender {
    pub(crate) fn send(
        &self,
        terminal: FrameDocumentModuleScriptTerminalBatchTask,
    ) -> Result<(), RendererPageChildModuleScriptTerminalRouteClosed> {
        self.task_route
            .send_and_signal_if_newly_ready(ReadyPageTask::new(
                RendererPageChildModuleScriptTerminalTask::new(self.root_document, terminal),
            ))
            .map_err(|error| {
                let (_, task) = error.0.into_parts();
                RendererPageChildModuleScriptTerminalRouteClosed(Box::new(task.into_terminal()))
            })
    }
}

#[derive(Clone, Debug)]
struct ChildModuleScriptTerminalReadySignal {
    owner_wake: RendererOwnerWakeSender,
}

impl OwnerTaskReadySignal for ChildModuleScriptTerminalReadySignal {
    fn signal_ready(&self) {
        self.owner_wake.signal_child_module_script_terminal();
    }
}

/// Unique Page-lifetime consumer for child module terminal fanout actions.
#[derive(Debug)]
pub(crate) struct RendererPageChildModuleScriptTerminalSource {
    source: OwnerReadyTaskSource<
        ReadyPageTask<RendererPageChildModuleScriptTerminalTask>,
        ChildModuleScriptTerminalReadySignal,
    >,
}

impl RendererPageChildModuleScriptTerminalSource {
    pub(crate) fn new(owner_wake: RendererOwnerWakeSender) -> Self {
        Self {
            source: OwnerReadyTaskSource::new(ChildModuleScriptTerminalReadySignal { owner_wake }),
        }
    }

    pub(crate) fn route(&self) -> RendererPageChildModuleScriptTerminalRoute {
        RendererPageChildModuleScriptTerminalRoute {
            task_route: self.source.route(),
        }
    }

    pub(crate) fn next_ready_metadata(&mut self) -> Option<RendererPageTaskReadyMetadata> {
        self.source.front().map(ReadyPageTask::metadata)
    }

    pub(crate) fn next_ready_owner(
        &mut self,
    ) -> Option<RendererPageChildModuleScriptTerminalOwner> {
        self.source.front().map(|ready| ready.value().owner())
    }

    pub(crate) fn pop_front(
        &mut self,
    ) -> Option<(
        RendererPageTaskReadyMetadata,
        RendererPageChildModuleScriptTerminalTask,
    )> {
        self.source.pop_front().map(ReadyPageTask::into_parts)
    }

    pub(crate) fn has_ready_task(&mut self) -> bool {
        !self.source.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.source.clear_local();
    }

    pub(crate) fn route_matches(&self, route: &RendererPageChildModuleScriptTerminalRoute) -> bool {
        route.same_route_as(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageChildModuleScriptTerminalTargetEffect {
    AppliedToCurrentOwner {
        outcome: FrameDocumentModuleScriptTerminalOutcome,
    },
    DiscardedStaleOwner {
        current_owner: Option<RendererPageChildModuleScriptTerminalOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageChildModuleScriptTerminalTurnAction {
    pub(crate) owner: RendererPageChildModuleScriptTerminalOwner,
    pub(crate) target_effect: PageChildModuleScriptTerminalTargetEffect,
}

pub(crate) type PageChildModuleScriptTerminalTurnOutcome =
    PageOwnerTurnOutcome<PageChildModuleScriptTerminalTurnAction>;
