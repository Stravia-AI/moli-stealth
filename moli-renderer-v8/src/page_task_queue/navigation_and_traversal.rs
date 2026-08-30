use moli_owner_queue::{OwnerReadyTaskRoute, OwnerReadyTaskSource};

use crate::{
    resource_ready::{ReadyPageTask, RendererPageTaskReadyMetadata},
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::{
    RendererOwnerWakeSender, RendererOwnerWakeSource, RendererPageTaskReadySignal,
    child_navigation_commit::{
        PageChildNavigationCommitTurnAction, RendererPageChildNavigationCommitOwner,
        RendererPageChildNavigationCommitSender, RendererPageChildNavigationCommitTask,
    },
    history_traversal::{
        PageHistoryTraversalTurnAction, RendererPageHistoryTraversalOwner,
        RendererPageHistoryTraversalSender, RendererPageHistoryTraversalTask,
        RendererPageHistoryTraversalTaskId, RendererPageHistoryTraversalTaskKind,
    },
    navigation_api_task::{
        PageNavigationApiTaskTurnAction, RendererPageNavigationApiTask,
        RendererPageNavigationApiTaskId, RendererPageNavigationApiTaskKind,
        RendererPageNavigationApiTaskOwner, RendererPageNavigationApiTaskSender,
    },
};

/// Exact snapshot of the head of the HTML navigation-and-traversal task source.
///
/// This is deliberately not named an owner: the payload-specific key and kind
/// are included so PageVm can check whether the Host-local V8 payload is still
/// current before the unique consumer removes the stable task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RendererPageNavigationAndTraversalHead {
    ChildNavigationCommit {
        owner: RendererPageChildNavigationCommitOwner,
    },
    HistoryTraversal {
        owner: RendererPageHistoryTraversalOwner,
        task_id: RendererPageHistoryTraversalTaskId,
        kind: RendererPageHistoryTraversalTaskKind,
    },
    NavigationApi {
        owner: RendererPageNavigationApiTaskOwner,
        task_id: RendererPageNavigationApiTaskId,
        kind: RendererPageNavigationApiTaskKind,
    },
}

/// One concrete operation sharing the HTML navigation-and-traversal source.
#[derive(Debug)]
pub(crate) enum RendererPageNavigationAndTraversalTask {
    ChildNavigationCommit(RendererPageChildNavigationCommitTask),
    HistoryTraversal(RendererPageHistoryTraversalTask),
    NavigationApi(RendererPageNavigationApiTask),
}

impl RendererPageNavigationAndTraversalTask {
    pub(crate) const fn head(&self) -> RendererPageNavigationAndTraversalHead {
        match self {
            Self::ChildNavigationCommit(task) => {
                RendererPageNavigationAndTraversalHead::ChildNavigationCommit {
                    owner: task.owner(),
                }
            }
            Self::HistoryTraversal(task) => {
                RendererPageNavigationAndTraversalHead::HistoryTraversal {
                    owner: task.owner(),
                    task_id: task.task_id(),
                    kind: task.kind(),
                }
            }
            Self::NavigationApi(task) => RendererPageNavigationAndTraversalHead::NavigationApi {
                owner: task.owner(),
                task_id: task.task_id(),
                kind: task.kind(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageNavigationAndTraversalTurnAction {
    ChildNavigationCommit(PageChildNavigationCommitTurnAction),
    HistoryTraversal(PageHistoryTraversalTurnAction),
    NavigationApi(PageNavigationApiTaskTurnAction),
}

pub(crate) type PageNavigationAndTraversalTurnOutcome =
    PageOwnerTurnOutcome<PageNavigationAndTraversalTurnAction>;

/// PageVm-stamped producer capability for the shared HTML task source.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageNavigationAndTraversalSender {
    route: RendererPageNavigationAndTraversalRoute,
    root_document: RendererDocumentToken,
}

impl RendererPageNavigationAndTraversalSender {
    pub(super) fn new(
        route: RendererPageNavigationAndTraversalRoute,
        root_document: RendererDocumentToken,
    ) -> Self {
        Self {
            route,
            root_document,
        }
    }

    pub(crate) fn history_traversal(&self) -> RendererPageHistoryTraversalSender {
        RendererPageHistoryTraversalSender::new(self.route.clone(), self.root_document)
    }

    pub(crate) fn child_navigation_commit(&self) -> RendererPageChildNavigationCommitSender {
        RendererPageChildNavigationCommitSender::new(self.route.clone(), self.root_document)
    }

    pub(crate) fn navigation_api_task(&self) -> RendererPageNavigationApiTaskSender {
        RendererPageNavigationApiTaskSender::new(self.route.clone(), self.root_document)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RendererPageNavigationAndTraversalRoute {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageNavigationAndTraversalTask>,
        RendererPageTaskReadySignal,
    >,
}

impl RendererPageNavigationAndTraversalRoute {
    pub(super) fn send(
        &self,
        task: RendererPageNavigationAndTraversalTask,
    ) -> Result<(), RendererPageNavigationAndTraversalRouteClosed> {
        self.task_route
            .send_and_signal_if_newly_ready(ReadyPageTask::new(task))
            .map_err(|_| RendererPageNavigationAndTraversalRouteClosed)
    }

    fn same_source_as(&self, source: &RendererPageNavigationAndTraversalSource) -> bool {
        self.task_route.same_source_as(&source.source)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RendererPageNavigationAndTraversalRouteClosed;

/// Unique Page-lifetime consumer for the HTML navigation-and-traversal source.
#[derive(Debug)]
pub(crate) struct RendererPageNavigationAndTraversalSource {
    source: OwnerReadyTaskSource<
        ReadyPageTask<RendererPageNavigationAndTraversalTask>,
        RendererPageTaskReadySignal,
    >,
}

impl RendererPageNavigationAndTraversalSource {
    pub(crate) fn new(owner_wake: RendererOwnerWakeSender) -> Self {
        Self {
            source: OwnerReadyTaskSource::new(RendererPageTaskReadySignal::new(
                owner_wake,
                RendererOwnerWakeSource::NavigationAndTraversalTask,
            )),
        }
    }

    pub(crate) fn route(&self) -> RendererPageNavigationAndTraversalRoute {
        RendererPageNavigationAndTraversalRoute {
            task_route: self.source.route(),
        }
    }

    pub(crate) fn next_ready_metadata(&mut self) -> Option<RendererPageTaskReadyMetadata> {
        self.source.front().map(ReadyPageTask::metadata)
    }

    pub(crate) fn next_ready_head(&mut self) -> Option<RendererPageNavigationAndTraversalHead> {
        self.source.front().map(|ready| ready.value().head())
    }

    pub(crate) fn pop_front(
        &mut self,
    ) -> Option<(
        RendererPageTaskReadyMetadata,
        RendererPageNavigationAndTraversalTask,
    )> {
        self.source.pop_front().map(ReadyPageTask::into_parts)
    }

    pub(crate) fn has_ready_task(&mut self) -> bool {
        !self.source.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.source.clear_local();
    }

    pub(crate) fn route_matches(&self, route: &RendererPageNavigationAndTraversalRoute) -> bool {
        route.same_source_as(self)
    }
}
