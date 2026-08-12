use std::{collections::VecDeque, time::Instant};

use moli_owner_queue::{OwnerReadyTaskRoute, OwnerReadyTaskSource, OwnerTaskReadySignal};

use crate::{
    frame_owner_model::FrameDocumentTaskOwner,
    resource_ready::{ReadyPageTask, RendererPageTaskReadyMetadata},
    runtime::{PageOwnerTurnOutcome, RendererDocumentToken},
};

use super::{
    PageOwnedInternalLoadingTask, PageOwnedInternalLoadingTaskEffect, RendererOwnerWakeSender,
};

/// PageVm namespace plus the exact main Document that produced one HTML
/// internal-loading task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageInternalLoadingOwner {
    root_document: RendererDocumentToken,
    document_owner: FrameDocumentTaskOwner,
}

impl RendererPageInternalLoadingOwner {
    pub(crate) const fn new(
        root_document: RendererDocumentToken,
        document_owner: FrameDocumentTaskOwner,
    ) -> Self {
        Self {
            root_document,
            document_owner,
        }
    }
}

/// One concrete task from the HTML internal-loading task source.
#[derive(Debug)]
pub(crate) struct RendererPageInternalLoadingTask {
    owner: RendererPageInternalLoadingOwner,
    task: PageOwnedInternalLoadingTask,
}

impl RendererPageInternalLoadingTask {
    fn new(root_document: RendererDocumentToken, task: PageOwnedInternalLoadingTask) -> Self {
        let owner = RendererPageInternalLoadingOwner::new(root_document, task.document_owner());
        Self { owner, task }
    }

    pub(crate) const fn owner(&self) -> RendererPageInternalLoadingOwner {
        self.owner
    }

    pub(crate) fn into_task(self) -> PageOwnedInternalLoadingTask {
        self.task
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageInternalLoadingRouteClosed;

#[derive(Clone, Debug)]
pub(crate) struct RendererPageInternalLoadingRoute {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageInternalLoadingTask>,
        InternalLoadingReadySignal,
    >,
}

impl RendererPageInternalLoadingRoute {
    pub(crate) fn sender(
        &self,
        root_document: RendererDocumentToken,
    ) -> RendererPageInternalLoadingSender {
        RendererPageInternalLoadingSender {
            task_route: self.task_route.clone(),
            root_document,
        }
    }

    fn same_route_as(&self, source: &RendererPageInternalLoadingSource) -> bool {
        self.task_route.same_source_as(&source.source)
    }
}

/// PageVm-stamped producer for concrete internal-loading tasks.
#[derive(Clone, Debug)]
pub(crate) struct RendererPageInternalLoadingSender {
    task_route: OwnerReadyTaskRoute<
        ReadyPageTask<RendererPageInternalLoadingTask>,
        InternalLoadingReadySignal,
    >,
    root_document: RendererDocumentToken,
}

impl RendererPageInternalLoadingSender {
    /// Posts one cancellable delayed task to the internal-loading source.
    ///
    /// A newer scheduled refresh for the same exact Document replaces the
    /// older one, matching Blink's one-task `HttpRefreshScheduler` handle.
    pub(crate) fn schedule_at(
        &self,
        task: PageOwnedInternalLoadingTask,
        ready_at: Instant,
    ) -> Result<(), RendererPageInternalLoadingRouteClosed> {
        self.task_route
            .send_and_signal_if_newly_ready(ReadyPageTask::at(
                RendererPageInternalLoadingTask::new(self.root_document, task),
                ready_at,
            ))
            .map_err(|_| RendererPageInternalLoadingRouteClosed)
    }
}

#[derive(Clone, Debug)]
struct InternalLoadingReadySignal {
    owner_wake: RendererOwnerWakeSender,
}

impl OwnerTaskReadySignal for InternalLoadingReadySignal {
    fn signal_ready(&self) {
        self.owner_wake.signal_internal_loading_task();
    }
}

/// Unique Page-lifetime consumer for HTML internal-loading tasks.
#[derive(Debug)]
pub(crate) struct RendererPageInternalLoadingSource {
    source: OwnerReadyTaskSource<
        ReadyPageTask<RendererPageInternalLoadingTask>,
        InternalLoadingReadySignal,
    >,
}

impl RendererPageInternalLoadingSource {
    pub(crate) fn new(owner_wake: RendererOwnerWakeSender) -> Self {
        Self {
            source: OwnerReadyTaskSource::new(InternalLoadingReadySignal { owner_wake }),
        }
    }

    pub(crate) fn route(&self) -> RendererPageInternalLoadingRoute {
        RendererPageInternalLoadingRoute {
            task_route: self.source.route(),
        }
    }

    pub(crate) fn next_ready_metadata(&mut self) -> Option<RendererPageTaskReadyMetadata> {
        self.source.with_tasks_mut(|tasks| {
            normalize_scheduled_tasks(tasks);
            tasks
                .front()
                .filter(|task| task.ready_at <= Instant::now())
                .map(ReadyPageTask::metadata)
        })
    }

    pub(crate) fn next_ready_owner(&mut self) -> Option<RendererPageInternalLoadingOwner> {
        self.source.with_tasks_mut(|tasks| {
            normalize_scheduled_tasks(tasks);
            tasks
                .front()
                .filter(|task| task.ready_at <= Instant::now())
                .map(|ready| ready.value().owner())
        })
    }

    pub(crate) fn pop_front(
        &mut self,
    ) -> Option<(
        RendererPageTaskReadyMetadata,
        RendererPageInternalLoadingTask,
    )> {
        self.source.with_tasks_mut(|tasks| {
            normalize_scheduled_tasks(tasks);
            if tasks.front()?.ready_at > Instant::now() {
                return None;
            }
            tasks.pop_front().map(ReadyPageTask::into_parts)
        })
    }

    pub(crate) fn has_ready_task(&mut self) -> bool {
        self.source.with_tasks_mut(|tasks| {
            normalize_scheduled_tasks(tasks);
            tasks
                .front()
                .is_some_and(|task| task.ready_at <= Instant::now())
        })
    }

    /// Returns the next delayed internal-loading deadline still owned by the
    /// live exact Document. Future work from a replaced Document is canceled
    /// before it can keep the Page deadline index resident.
    pub(crate) fn next_deadline_for_owner(
        &mut self,
        current_owner: Option<RendererPageInternalLoadingOwner>,
    ) -> Option<Instant> {
        self.source.with_tasks_mut(|tasks| {
            normalize_scheduled_tasks(tasks);
            tasks.retain(|task| Some(task.value().owner()) == current_owner);
            tasks.front().map(|task| task.ready_at)
        })
    }

    #[cfg(debug_assertions)]
    pub(crate) fn local_deadline_for_owner(
        &self,
        current_owner: Option<RendererPageInternalLoadingOwner>,
    ) -> Option<Instant> {
        self.source.with_local_tasks(|tasks| {
            tasks
                .iter()
                .filter(|task| Some(task.value().owner()) == current_owner)
                .map(|task| task.ready_at)
                .min()
        })
    }

    pub(crate) fn clear(&mut self) {
        self.source.clear_local();
    }

    pub(crate) fn route_matches(&self, route: &RendererPageInternalLoadingRoute) -> bool {
        route.same_route_as(self)
    }
}

fn normalize_scheduled_tasks(tasks: &mut VecDeque<ReadyPageTask<RendererPageInternalLoadingTask>>) {
    if tasks.len() < 2 {
        return;
    }
    let mut normalized = VecDeque::with_capacity(tasks.len());
    while let Some(task) = tasks.pop_front() {
        let owner = task.value().owner();
        normalized.retain(|queued: &ReadyPageTask<RendererPageInternalLoadingTask>| {
            queued.value().owner() != owner
        });
        normalized.push_back(task);
    }
    normalized
        .make_contiguous()
        .sort_by_key(|task| (task.ready_at, task.order));
    *tasks = normalized;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageInternalLoadingTargetEffect {
    /// The exact task was authorized against the current root Document and its
    /// body ran. Activated and suppressed bodies are both real selected tasks.
    AppliedToCurrentOwner {
        effect: PageOwnedInternalLoadingTaskEffect,
    },
    /// The queued task belongs to a detached/replaced Document. Chromium
    /// cancels the corresponding `kInternalLoading` task when the old Document
    /// detaches; Moli may still dequeue its stable payload, but must not
    /// enter or checkpoint the replacement realm.
    DiscardedStaleOwner {
        current_owner: Option<RendererPageInternalLoadingOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageInternalLoadingTurnAction {
    pub(crate) owner: RendererPageInternalLoadingOwner,
    pub(crate) target_effect: PageInternalLoadingTargetEffect,
}

pub(crate) type PageInternalLoadingTurnOutcome =
    PageOwnerTurnOutcome<PageInternalLoadingTurnAction>;
