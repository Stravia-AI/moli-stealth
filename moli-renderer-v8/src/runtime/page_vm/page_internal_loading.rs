use crate::page_task_queue::{
    PageInternalLoadingTargetEffect, PageInternalLoadingTurnAction, PageInternalLoadingTurnOutcome,
    RendererPageInternalLoadingOwner, RendererPageInternalLoadingTask,
};

use super::PageVm;

impl PageVm {
    pub(in crate::runtime) fn current_page_internal_loading_owner(
        &self,
    ) -> Option<RendererPageInternalLoadingOwner> {
        self.vm()
            .current_main_document_task_owner()
            .map(|document_owner| {
                RendererPageInternalLoadingOwner::new(
                    self.document_lifecycle.identity().document,
                    document_owner,
                )
            })
    }

    pub(in crate::runtime) fn apply_selected_page_internal_loading_turn(
        &mut self,
        task: RendererPageInternalLoadingTask,
    ) -> PageInternalLoadingTurnOutcome {
        let owner = task.owner();
        let current_owner = self.current_page_internal_loading_owner();
        let target_effect = if current_owner == Some(owner) {
            PageInternalLoadingTargetEffect::AppliedToCurrentOwner {
                effect: self
                    .vm_mut()
                    .run_page_owned_internal_loading_task(task.into_task()),
            }
        } else {
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-Document internal-loading task"
            );
            PageInternalLoadingTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageInternalLoadingTurnAction {
            owner,
            target_effect,
        };
        PageInternalLoadingTurnOutcome::new(action)
    }

    /// Executes only the authorized internal-loading body.
    ///
    /// Complete HTML-task tests must use
    /// `PageSelectedTaskTestSelector::InternalLoading` so the production
    /// dispatcher, rather than the fixture, owns task-end checkpoint policy.
    #[cfg(test)]
    pub(in crate::runtime) fn run_internal_loading_body_for_test(
        &mut self,
    ) -> Option<PageInternalLoadingTurnOutcome> {
        let task_sources = self.page_task_executor_sources_for_test();
        let task = task_sources.take_internal_loading_for_executor_test()?;
        Some(self.apply_selected_page_internal_loading_turn(task))
    }

    #[cfg(test)]
    pub(in crate::runtime) fn next_internal_loading_deadline_for_test(
        &self,
    ) -> Option<std::time::Instant> {
        self.page_task_executor_sources_for_test()
            .next_internal_loading_deadline(self.current_page_internal_loading_owner())
    }
}
