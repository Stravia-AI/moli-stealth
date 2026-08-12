use crate::page_task_queue::{
    PageChildNavigationCommitTargetEffect, PageChildNavigationCommitTurnAction,
    PageChildNavigationCommitTurnOutcome, RendererPageChildNavigationCommitOwner,
    RendererPageChildNavigationCommitTask,
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl IntoPageTaskCompletion for PageChildNavigationCommitTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageChildNavigationCommitTargetEffect::AppliedToCurrentOwner => {
                // A current navigation commit owns an ordinary task-end
                // checkpoint even when it only mutates host-side child state.
                // Its concrete child follow-ups were already published by the
                // body and must not be synchronously drained here.
                PageTaskCompletion::CheckpointOnly
            }
            PageChildNavigationCommitTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

/// Proof that the Page arbiter matched the resident PageVm, child scheduler
/// lane and exact navigation-load generation before entering V8.
pub(crate) struct AuthorizedCurrentPageChildNavigationCommit(RendererPageChildNavigationCommitTask);

impl AuthorizedCurrentPageChildNavigationCommit {
    fn new(task: RendererPageChildNavigationCommitTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageChildNavigationCommitTask {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn new_for_executor_test(task: RendererPageChildNavigationCommitTask) -> Self {
        Self(task)
    }
}

impl PageVm {
    fn current_page_child_navigation_commit_owner(
        &self,
        expected: RendererPageChildNavigationCommitOwner,
    ) -> Option<RendererPageChildNavigationCommitOwner> {
        if expected.root_document() != self.document_lifecycle.identity().document {
            return None;
        }
        self.vm().current_child_navigation_commit_owner(
            expected.commit().child_handle,
            expected.root_document(),
        )
    }

    pub(in crate::runtime) fn apply_selected_page_child_navigation_commit_turn(
        &mut self,
        task: RendererPageChildNavigationCommitTask,
    ) -> anyhow::Result<PageChildNavigationCommitTurnOutcome> {
        let owner = task.owner();
        let current_owner = self.current_page_child_navigation_commit_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            self.vm_mut().apply_current_child_navigation_commit_body(
                AuthorizedCurrentPageChildNavigationCommit::new(task),
            )?;
            PageChildNavigationCommitTargetEffect::AppliedToCurrentOwner
        } else {
            if owner.root_document() == self.document_lifecycle.identity().document {
                self.vm_mut().discard_stale_child_navigation_commit(task);
            }
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-generation child navigation commit"
            );
            PageChildNavigationCommitTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageChildNavigationCommitTurnAction {
            owner,
            target_effect,
        };
        Ok(PageChildNavigationCommitTurnOutcome::new(action))
    }

    #[cfg(test)]
    /// Run only the exact-owner body for low-level domain fixtures.
    ///
    /// Complete HTML-task tests must use `PageSelectedTaskTestSelector` so the
    /// production dispatcher also submits the task-end checkpoint.
    pub(in crate::runtime) fn run_child_navigation_commit_body_for_test(
        &mut self,
    ) -> anyhow::Result<Option<PageChildNavigationCommitTurnOutcome>> {
        let sources = self.page_task_executor_sources_for_test();
        let Some(task) = sources.take_scheduler_task_for_executor_test(|descriptor| {
            matches!(
                descriptor,
                crate::page_task_queue::RendererPageReadyDescriptor::NavigationAndTraversal {
                    head: crate::page_task_queue::RendererPageNavigationAndTraversalHead::ChildNavigationCommit { .. },
                    ..
                }
            )
        }) else {
            return Ok(None);
        };
        let crate::page_task_queue::RendererPageSchedulerTask::NavigationAndTraversal(
            crate::page_task_queue::RendererPageNavigationAndTraversalTask::ChildNavigationCommit(
                task,
            ),
        ) = task
        else {
            unreachable!("child-navigation descriptor must dequeue its own family task")
        };
        self.apply_selected_page_child_navigation_commit_turn(task)
            .map(Some)
    }
}
