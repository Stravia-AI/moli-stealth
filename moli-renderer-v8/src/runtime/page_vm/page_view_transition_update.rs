use crate::page_task_queue::{
    PageViewTransitionUpdateTargetEffect, PageViewTransitionUpdateTurnAction,
    PageViewTransitionUpdateTurnOutcome, RendererPageViewTransitionUpdateOwner,
    RendererPageViewTransitionUpdateTask,
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl IntoPageTaskCompletion for PageViewTransitionUpdateTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageViewTransitionUpdateTargetEffect::ProcessedForCurrentOwner => {
                PageTaskCompletion::CallbackCompletion
            }
            PageViewTransitionUpdateTargetEffect::CurrentOwnerHadNoPendingCallback => {
                PageTaskCompletion::CheckpointOnly
            }
            PageViewTransitionUpdateTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

pub(crate) struct AuthorizedCurrentPageViewTransitionUpdate(RendererPageViewTransitionUpdateTask);

impl AuthorizedCurrentPageViewTransitionUpdate {
    fn new(task: RendererPageViewTransitionUpdateTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageViewTransitionUpdateTask {
        self.0
    }
}

impl PageVm {
    fn current_page_view_transition_update_owner(
        &self,
        expected: RendererPageViewTransitionUpdateOwner,
    ) -> Option<RendererPageViewTransitionUpdateOwner> {
        self.vm().current_view_transition_update_owner(
            expected,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_view_transition_update_turn(
        &mut self,
        task: RendererPageViewTransitionUpdateTask,
    ) -> anyhow::Result<PageViewTransitionUpdateTurnOutcome> {
        let owner = task.owner();
        let task_id = task.task_id();
        let current_owner = self.current_page_view_transition_update_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            if self.vm_mut().apply_current_view_transition_update_body(
                AuthorizedCurrentPageViewTransitionUpdate::new(task),
            )? {
                PageViewTransitionUpdateTargetEffect::ProcessedForCurrentOwner
            } else {
                PageViewTransitionUpdateTargetEffect::CurrentOwnerHadNoPendingCallback
            }
        } else {
            self.vm_mut()
                .discard_stale_view_transition_update(task_id, owner);
            tracing::debug!(
                ?owner,
                ?current_owner,
                ?task_id,
                "discarded stale exact-Window view-transition update callback"
            );
            PageViewTransitionUpdateTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageViewTransitionUpdateTurnAction {
            owner,
            task_id,
            target_effect,
        };
        Ok(PageViewTransitionUpdateTurnOutcome::new(action))
    }
}
