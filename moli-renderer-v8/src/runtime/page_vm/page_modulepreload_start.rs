use crate::{
    frame_owner_model::FrameDocumentModulepreloadFetchTask,
    page_task_queue::{
        PageModulepreloadStartDocumentEffect, PageModulepreloadStartTurnAction,
        PageModulepreloadStartTurnOutcome, RendererPageModulepreloadStartOwner,
        RendererPageModulepreloadStartTask,
    },
};

use super::PageVm;

/// Proof that the Page arbiter matched the complete root-Document and
/// child/document/realm target before allowing a start task to mutate its
/// document modulator.
pub(crate) struct AuthorizedCurrentChildModulepreloadStartTask(FrameDocumentModulepreloadFetchTask);

impl AuthorizedCurrentChildModulepreloadStartTask {
    fn new(task: FrameDocumentModulepreloadFetchTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> FrameDocumentModulepreloadFetchTask {
        self.0
    }
}

impl PageVm {
    fn current_page_modulepreload_start_owner(
        &self,
        expected: RendererPageModulepreloadStartOwner,
    ) -> Option<RendererPageModulepreloadStartOwner> {
        let root_document = self.document_lifecycle.identity().document;
        self.vm()
            .current_child_document_module_fetch_target(expected.target().child_handle())
            .map(|target| RendererPageModulepreloadStartOwner::new(root_document, target))
    }

    pub(in crate::runtime) fn apply_selected_page_modulepreload_start_turn(
        &mut self,
        start: RendererPageModulepreloadStartTask,
    ) -> PageModulepreloadStartTurnOutcome {
        let owner = start.owner();
        let current_owner = self.current_page_modulepreload_start_owner(owner);
        let document_effect = if current_owner == Some(owner) {
            let outcome = self.vm_mut().apply_current_child_modulepreload_start_task(
                AuthorizedCurrentChildModulepreloadStartTask::new(start.into_task()),
            );
            PageModulepreloadStartDocumentEffect::AppliedToCurrentOwner { outcome }
        } else {
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-owner child modulepreload start task"
            );
            PageModulepreloadStartDocumentEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageModulepreloadStartTurnAction {
            owner,
            document_effect,
        };
        PageModulepreloadStartTurnOutcome::new(action)
    }

    #[cfg(test)]
    /// Apply one exact start body without submitting selected-task completion.
    /// Complete-task tests must use the shared exact selector and production
    /// dispatcher.
    pub(in crate::runtime) fn run_page_modulepreload_start_body_for_test(
        &mut self,
    ) -> Option<PageModulepreloadStartTurnOutcome> {
        let source = self
            .page_task_executor_sources_for_test()
            .modulepreload_start();
        let candidate = source.next_ready_metadata()?;
        let (actual, task) = source
            .pop_front()
            .expect("selected standalone modulepreload start must remain queued");
        debug_assert_eq!(actual, candidate);
        Some(self.apply_selected_page_modulepreload_start_turn(task))
    }
}
