use crate::page_task_queue::{
    PageOpfsTaskTargetEffect, PageOpfsTaskTurnAction, PageOpfsTaskTurnOutcome,
    RendererPageOpfsTask, RendererPageOpfsTaskOwner,
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

/// Proof that the Page arbiter matched a selected storage completion against
/// the exact root PageVm, Window realm, transport generation, and pending OPFS
/// settlement.
pub(crate) struct AuthorizedCurrentPageOpfsTask(RendererPageOpfsTask);

impl AuthorizedCurrentPageOpfsTask {
    fn new(task: RendererPageOpfsTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageOpfsTask {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn new_for_executor_test(task: RendererPageOpfsTask) -> Self {
        Self(task)
    }
}

impl IntoPageTaskCompletion for PageOpfsTaskTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageOpfsTaskTargetEffect::SettledCurrentOwner => {
                // Resolving or rejecting the OPFS Promise is the storage task
                // body. Promise reactions belong to this task's ordinary
                // event-loop checkpoint; OPFS does not own a generic runtime
                // work drain after that checkpoint.
                PageTaskCompletion::CheckpointOnly
            }
            PageOpfsTaskTargetEffect::IgnoredStaleOwner { .. } => PageTaskCompletion::NoCompletion,
        }
    }
}

impl PageVm {
    fn current_page_opfs_task_owner(
        &self,
        expected: RendererPageOpfsTaskOwner,
    ) -> Option<RendererPageOpfsTaskOwner> {
        let execution_context = self
            .vm()
            .current_pending_opfs_task_execution_context(expected.task())?;
        Some(RendererPageOpfsTaskOwner::new(
            self.document_lifecycle.identity().document,
            execution_context,
            expected.task(),
        ))
    }

    pub(in crate::runtime) fn apply_selected_page_opfs_task_turn(
        &mut self,
        task: RendererPageOpfsTask,
    ) -> anyhow::Result<PageOpfsTaskTurnOutcome> {
        let owner = task.owner();
        let current_owner = self.current_page_opfs_task_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            self.vm_mut()
                .apply_current_opfs_task_body(AuthorizedCurrentPageOpfsTask::new(task))?;
            PageOpfsTaskTargetEffect::SettledCurrentOwner
        } else {
            // A root mismatch means the PageVm-local task id belongs to
            // another namespace and must not touch this pending map. Within
            // the same root, exact owner cleanup is safe and releases a
            // resolver retained by a retired realm.
            if owner.root_document() == self.document_lifecycle.identity().document {
                self.vm_mut().discard_stale_opfs_task(owner);
            }
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-owner OPFS task"
            );
            PageOpfsTaskTargetEffect::IgnoredStaleOwner { current_owner }
        };
        let action = PageOpfsTaskTurnAction {
            owner,
            target_effect,
        };
        Ok(PageOpfsTaskTurnOutcome::new(action))
    }
}
