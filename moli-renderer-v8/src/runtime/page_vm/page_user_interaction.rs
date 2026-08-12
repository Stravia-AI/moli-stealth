use crate::page_task_queue::{
    PageUserInteractionBodyEffect, PageUserInteractionTargetEffect, PageUserInteractionTurnAction,
    PageUserInteractionTurnOutcome, RendererPageUserInteractionOwner,
    RendererPageUserInteractionTask, RendererPageUserInteractionTaskId,
    RendererPageUserInteractionTaskKind,
};

use super::{
    AuthorizedCurrentWindowDocumentTask, IntoPageTaskCompletion, PageTaskCompletion, PageVm,
};

impl IntoPageTaskCompletion for PageUserInteractionTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageUserInteractionTargetEffect::AppliedToCurrentOwner => {
                PageTaskCompletion::CallbackCompletion
            }
            PageUserInteractionTargetEffect::NotAppliedToCurrentOwner => {
                PageTaskCompletion::CheckpointOnly
            }
            PageUserInteractionTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

/// Proof that the Page arbiter matched the PageVm namespace, exact Document,
/// Host pending slot, and task kind before V8 execution.
pub(crate) type AuthorizedCurrentPageUserInteractionTask =
    AuthorizedCurrentWindowDocumentTask<RendererPageUserInteractionTask>;

impl PageVm {
    fn current_page_user_interaction_owner(
        &self,
        task_id: RendererPageUserInteractionTaskId,
    ) -> Option<(
        RendererPageUserInteractionOwner,
        RendererPageUserInteractionTaskKind,
    )> {
        self.vm().current_pending_user_interaction_owner(
            task_id,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_user_interaction_turn(
        &mut self,
        task: RendererPageUserInteractionTask,
    ) -> anyhow::Result<PageUserInteractionTurnOutcome> {
        let owner = task.owner();
        let task_id = task.task_id();
        let kind = task.kind();
        let current = self.current_page_user_interaction_owner(task_id);
        let target_effect =
            match self.authorize_current_window_document_task(task, owner, kind, current) {
                Ok(authorization) => {
                    match self
                        .vm_mut()
                        .apply_current_user_interaction_task_body(authorization)?
                    {
                        PageUserInteractionBodyEffect::Applied => {
                            PageUserInteractionTargetEffect::AppliedToCurrentOwner
                        }
                        PageUserInteractionBodyEffect::NotApplied => {
                            PageUserInteractionTargetEffect::NotAppliedToCurrentOwner
                        }
                    }
                }
                Err(stale) => {
                    if stale.may_discard_local_payload() {
                        self.vm_mut().discard_stale_user_interaction_task(task_id);
                    }
                    let current_owner = stale.current_owner();
                    tracing::debug!(
                        ?owner,
                        ?current_owner,
                        ?task_id,
                        ?kind,
                        "discarded stale exact-Document user-interaction task"
                    );
                    PageUserInteractionTargetEffect::DiscardedStaleOwner { current_owner }
                }
            };
        let action = PageUserInteractionTurnAction {
            owner,
            task_id,
            kind,
            target_effect,
        };
        Ok(PageUserInteractionTurnOutcome::new(action))
    }
}
