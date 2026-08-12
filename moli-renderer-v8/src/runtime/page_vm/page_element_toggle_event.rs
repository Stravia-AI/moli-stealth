use crate::page_task_queue::{
    PageElementToggleEventTargetEffect, PageElementToggleEventTurnAction,
    PageElementToggleEventTurnOutcome, RendererPageElementToggleEventKind,
    RendererPageElementToggleEventOwner, RendererPageElementToggleEventTask,
    RendererPageElementToggleEventTaskId,
};

use super::{
    AuthorizedCurrentWindowDocumentTask, IntoPageTaskCompletion, PageTaskCompletion, PageVm,
};

impl IntoPageTaskCompletion for PageElementToggleEventTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageElementToggleEventTargetEffect::DispatchedToCurrentOwner => {
                PageTaskCompletion::CallbackCompletion
            }
            PageElementToggleEventTargetEffect::CurrentOwnerHadNoEventTarget => {
                PageTaskCompletion::CheckpointOnly
            }
            PageElementToggleEventTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

/// Proof that the Page arbiter matched the PageVm namespace, exact Document,
/// Host coalescing slot, and task kind before the V8 executor received data.
pub(crate) type AuthorizedCurrentPageElementToggleEvent =
    AuthorizedCurrentWindowDocumentTask<RendererPageElementToggleEventTask>;

impl PageVm {
    fn current_page_element_toggle_event_owner(
        &self,
        task_id: RendererPageElementToggleEventTaskId,
    ) -> Option<(
        RendererPageElementToggleEventOwner,
        RendererPageElementToggleEventKind,
    )> {
        self.vm().current_pending_element_toggle_event_owner(
            task_id,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_element_toggle_event_turn(
        &mut self,
        task: RendererPageElementToggleEventTask,
    ) -> anyhow::Result<PageElementToggleEventTurnOutcome> {
        let owner = task.owner();
        let task_id = task.task_id();
        let kind = task.kind();
        let current = self.current_page_element_toggle_event_owner(task_id);
        let target_effect =
            match self.authorize_current_window_document_task(task, owner, kind, current) {
                Ok(authorization) => {
                    if self
                        .vm_mut()
                        .apply_current_element_toggle_event_body(authorization)?
                    {
                        PageElementToggleEventTargetEffect::DispatchedToCurrentOwner
                    } else {
                        PageElementToggleEventTargetEffect::CurrentOwnerHadNoEventTarget
                    }
                }
                Err(stale) => {
                    if stale.may_discard_local_payload() {
                        self.vm_mut()
                            .discard_stale_element_toggle_event_task(task_id);
                    }
                    let current_owner = stale.current_owner();
                    tracing::debug!(
                        ?owner,
                        ?current_owner,
                        ?task_id,
                        ?kind,
                        "discarded stale exact-Document element toggle event"
                    );
                    PageElementToggleEventTargetEffect::DiscardedStaleOwner { current_owner }
                }
            };
        let action = PageElementToggleEventTurnAction {
            owner,
            task_id,
            kind,
            target_effect,
        };
        Ok(PageElementToggleEventTurnOutcome::new(action))
    }
}
