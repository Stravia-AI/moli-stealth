use crate::page_task_queue::{
    PageConnectedStyleEventTargetEffect, PageConnectedStyleEventTurnAction,
    PageConnectedStyleEventTurnOutcome, PageStylesheetNetworkingTargetEffect,
    PageStylesheetNetworkingTurnAction, PageStylesheetNetworkingTurnOutcome,
    RendererPageConnectedStyleEventTask, RendererPageStylesheetNetworkingTask,
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl IntoPageTaskCompletion for PageConnectedStyleEventTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageConnectedStyleEventTargetEffect::DispatchedToCurrentOwner { .. } => {
                PageTaskCompletion::CallbackCompletion
            }
            PageConnectedStyleEventTargetEffect::CurrentOwnerHadNoEvent { .. } => {
                PageTaskCompletion::CheckpointOnly
            }
            PageConnectedStyleEventTargetEffect::DiscardedStaleOwner => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

impl IntoPageTaskCompletion for PageStylesheetNetworkingTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            // A stylesheet fetch terminal is a real Networking task even
            // though applying its CSS/state does not itself enter V8. The
            // selected-task dispatcher therefore owns the ordinary task-end
            // checkpoint, but not callback-style runtime reconciliation. A
            // later link/style load or error event remains a separate task.
            PageStylesheetNetworkingTargetEffect::AppliedToCurrentOwner => {
                PageTaskCompletion::CheckpointOnly
            }
            // Retired stylesheet state may still record its historical
            // network result, but it is not a task of the replacement agent
            // and must not checkpoint that agent's microtask queue.
            PageStylesheetNetworkingTargetEffect::RecordedForStaleOwner => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

impl PageVm {
    pub(in crate::runtime) fn apply_selected_page_stylesheet_networking_turn(
        &mut self,
        task: RendererPageStylesheetNetworkingTask,
    ) -> anyhow::Result<PageStylesheetNetworkingTurnOutcome> {
        let owner = task.owner();
        let root_document = self.document_lifecycle.identity().document;
        let target_effect = self
            .vm_mut()
            .apply_page_stylesheet_networking_task(root_document, task);
        Ok(PageStylesheetNetworkingTurnOutcome::new(
            PageStylesheetNetworkingTurnAction {
                owner,
                target_effect,
            },
        ))
    }

    pub(in crate::runtime) fn apply_selected_page_connected_style_event_turn(
        &mut self,
        task: RendererPageConnectedStyleEventTask,
    ) -> anyhow::Result<PageConnectedStyleEventTurnOutcome> {
        let owner = task.owner();
        let root_document = self.document_lifecycle.identity().document;
        let target_effect = self
            .vm_mut()
            .apply_page_connected_style_event_task_body(root_document, task);
        let action = PageConnectedStyleEventTurnAction {
            owner,
            target_effect,
        };
        Ok(PageConnectedStyleEventTurnOutcome::new(action))
    }
}
