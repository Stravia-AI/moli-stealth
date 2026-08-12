use crate::page_task_queue::{
    PageIndexedDbTaskTargetEffect, PageIndexedDbTaskTurnAction, PageIndexedDbTaskTurnOutcome,
    RendererPageIndexedDbTask, RendererPageIndexedDbTaskOwner,
};
use crate::script_vm::IndexedDbTaskBodyEffect;

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

/// Proof that the Page arbiter matched the selected task against both its
/// PageVm namespace and exact Window realm before the V8 executor touched the
/// realm-local IndexedDB queue.
pub(crate) struct AuthorizedCurrentPageIndexedDbTask(RendererPageIndexedDbTask);

impl AuthorizedCurrentPageIndexedDbTask {
    fn new(task: RendererPageIndexedDbTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageIndexedDbTask {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn new_for_executor_test(task: RendererPageIndexedDbTask) -> Self {
        Self(task)
    }
}

impl IntoPageTaskCompletion for PageIndexedDbTaskTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageIndexedDbTaskTargetEffect::AppliedToCurrentOwner
            | PageIndexedDbTaskTargetEffect::FailedCurrentOwner => {
                // IndexedDB request/transaction bodies may dispatch JS
                // callbacks. Their Promise reactions, child work and runtime
                // follow-up belong to the selected task's central callback
                // completion. The checkpoint also runs the Agent-level IDB
                // transaction-deactivation batch after all reactions.
                PageTaskCompletion::CallbackCompletion
            }
            PageIndexedDbTaskTargetEffect::CurrentOwnerHadNoPendingTask => {
                // The current stable scheduler ticket still represents an
                // ordinary task turn, even if its coalesced realm-local body
                // was removed before selection.
                PageTaskCompletion::CheckpointOnly
            }
            PageIndexedDbTaskTargetEffect::IgnoredStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

impl PageVm {
    fn current_page_indexed_db_task_owner(
        &self,
        expected: RendererPageIndexedDbTaskOwner,
    ) -> Option<RendererPageIndexedDbTaskOwner> {
        self.vm()
            .indexed_db_task_owner_is_current(expected)
            .then(|| {
                RendererPageIndexedDbTaskOwner::new(
                    self.document_lifecycle.identity().document,
                    expected.execution_context(),
                )
            })
    }

    pub(in crate::runtime) fn apply_selected_page_indexed_db_task_turn(
        &mut self,
        task: RendererPageIndexedDbTask,
    ) -> anyhow::Result<PageIndexedDbTaskTurnOutcome> {
        let owner = task.owner();
        let kind = task.kind();
        let current_owner = self.current_page_indexed_db_task_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            match self
                .vm_mut()
                .apply_current_indexed_db_task_body(AuthorizedCurrentPageIndexedDbTask::new(task))
            {
                Ok(IndexedDbTaskBodyEffect::Applied) => {
                    PageIndexedDbTaskTargetEffect::AppliedToCurrentOwner
                }
                Ok(IndexedDbTaskBodyEffect::CurrentOwnerHadNoPendingTask) => {
                    PageIndexedDbTaskTargetEffect::CurrentOwnerHadNoPendingTask
                }
                Err(error) => {
                    self.vm_mut().record_runtime_warning(format_args!(
                        "IndexedDB task dispatch failed: {error}"
                    ));
                    PageIndexedDbTaskTargetEffect::FailedCurrentOwner
                }
            }
        } else {
            if owner.root_document() == self.document_lifecycle.identity().document {
                self.vm_mut().discard_stale_indexed_db_task(owner, kind)?;
            }
            tracing::debug!(
                ?owner,
                ?current_owner,
                ?kind,
                "discarded stale exact-owner IndexedDB task"
            );
            PageIndexedDbTaskTargetEffect::IgnoredStaleOwner { current_owner }
        };
        let action = PageIndexedDbTaskTurnAction {
            owner,
            kind,
            target_effect,
        };
        Ok(PageIndexedDbTaskTurnOutcome::new(action))
    }
}
