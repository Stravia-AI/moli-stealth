use crate::page_task_queue::{
    PageNavigationApiTaskTargetEffect, PageNavigationApiTaskTurnAction,
    PageNavigationApiTaskTurnOutcome, RendererPageNavigationApiTask,
    RendererPageNavigationApiTaskId, RendererPageNavigationApiTaskKind,
    RendererPageNavigationApiTaskOwner,
};
use crate::script_vm::NavigationApiTaskBodyApplied;

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl From<NavigationApiTaskBodyApplied> for PageNavigationApiTaskTargetEffect {
    fn from(_: NavigationApiTaskBodyApplied) -> Self {
        Self::FinishResultAppliedToCurrentOwner
    }
}

impl IntoPageTaskCompletion for PageNavigationApiTaskTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageNavigationApiTaskTargetEffect::FinishResultAppliedToCurrentOwner => {
                // The success pass may enter event listeners, and both that
                // pass and the finished Promise reactions can publish child
                // or runtime-script work belonging to this selected task.
                PageTaskCompletion::CallbackCompletion
            }
            PageNavigationApiTaskTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

pub(crate) struct AuthorizedCurrentPageNavigationApiTask(RendererPageNavigationApiTask);

impl AuthorizedCurrentPageNavigationApiTask {
    fn new(task: RendererPageNavigationApiTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageNavigationApiTask {
        self.0
    }
}

impl PageVm {
    fn current_page_navigation_api_task_owner(
        &self,
        task_id: RendererPageNavigationApiTaskId,
    ) -> Option<(
        RendererPageNavigationApiTaskOwner,
        RendererPageNavigationApiTaskKind,
    )> {
        self.vm().current_pending_navigation_api_task_owner(
            task_id,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_navigation_api_task_turn(
        &mut self,
        task: RendererPageNavigationApiTask,
    ) -> anyhow::Result<PageNavigationApiTaskTurnOutcome> {
        let owner = task.owner();
        let task_id = task.task_id();
        let kind = task.kind();
        let current = self.current_page_navigation_api_task_owner(task_id);
        let target_effect = if current == Some((owner, kind)) {
            self.vm_mut()
                .apply_current_navigation_api_task_body(
                    AuthorizedCurrentPageNavigationApiTask::new(task),
                )
                .map(Into::into)?
        } else {
            if owner.root_document() == self.document_lifecycle.identity().document {
                self.vm_mut().discard_stale_navigation_api_task(task_id);
            }
            tracing::debug!(
                ?owner,
                ?current,
                ?task_id,
                ?kind,
                "discarded stale exact-owner Navigation API task"
            );
            PageNavigationApiTaskTargetEffect::DiscardedStaleOwner {
                current_owner: current.map(|(owner, _)| owner),
            }
        };
        let action = PageNavigationApiTaskTurnAction {
            owner,
            task_id,
            kind,
            target_effect,
        };
        Ok(PageNavigationApiTaskTurnOutcome::new(action))
    }
}
