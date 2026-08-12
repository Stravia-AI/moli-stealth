use crate::page_task_queue::{
    PageWebCryptoTaskTargetEffect, PageWebCryptoTaskTurnAction, PageWebCryptoTaskTurnOutcome,
    RendererPageWebCryptoTask, RendererPageWebCryptoTaskOwner,
};

use super::PageVm;

/// Proof that the Page arbiter matched a selected completion against the exact
/// root PageVm, Window realm, and pending Promise entry.
pub(crate) struct AuthorizedCurrentPageWebCryptoTask(RendererPageWebCryptoTask);

impl AuthorizedCurrentPageWebCryptoTask {
    fn new(task: RendererPageWebCryptoTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageWebCryptoTask {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn new_for_executor_test(task: RendererPageWebCryptoTask) -> Self {
        Self(task)
    }
}

impl PageVm {
    fn current_page_webcrypto_task_owner(
        &self,
        expected: RendererPageWebCryptoTaskOwner,
    ) -> Option<RendererPageWebCryptoTaskOwner> {
        let execution_context = self
            .vm()
            .current_pending_webcrypto_task_execution_context(expected.task())?;
        Some(RendererPageWebCryptoTaskOwner::new(
            self.document_lifecycle.identity().document,
            execution_context,
            expected.task(),
        ))
    }

    pub(in crate::runtime) fn apply_selected_page_webcrypto_task_turn(
        &mut self,
        task: RendererPageWebCryptoTask,
    ) -> anyhow::Result<PageWebCryptoTaskTurnOutcome> {
        let owner = task.owner();
        let current_owner = self.current_page_webcrypto_task_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            self.vm_mut()
                .apply_current_webcrypto_task_body(AuthorizedCurrentPageWebCryptoTask::new(task))?;
            PageWebCryptoTaskTargetEffect::SettledCurrentOwner
        } else {
            // A root mismatch means `task_id` belongs to another PageVm
            // namespace and must not be used to touch this PageVm's pending
            // map. Within the same root, exact cleanup is safe and prevents a
            // retired realm from retaining its resolver.
            if owner.root_document() == self.document_lifecycle.identity().document {
                self.vm_mut().discard_stale_webcrypto_task(owner);
            }
            tracing::debug!(
                ?owner,
                ?current_owner,
                "ignored stale exact-owner WebCrypto task"
            );
            PageWebCryptoTaskTargetEffect::IgnoredStaleOwner { current_owner }
        };
        let action = PageWebCryptoTaskTurnAction {
            owner,
            target_effect,
        };
        Ok(PageWebCryptoTaskTurnOutcome::new(action))
    }
}
