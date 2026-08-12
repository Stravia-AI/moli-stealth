use crate::page_task_queue::{
    PagePopupLoadEventTargetEffect, PagePopupLoadEventTurnAction, PagePopupLoadEventTurnOutcome,
    RendererPagePopupLoadEventOwner, RendererPagePopupLoadEventTask,
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl IntoPageTaskCompletion for PagePopupLoadEventTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PagePopupLoadEventTargetEffect::DispatchedToCurrentOwner => {
                PageTaskCompletion::CallbackCompletion
            }
            PagePopupLoadEventTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

/// Proof that the Page arbiter matched the PageVm namespace and the exact
/// lightweight-popup Document navigation before entering V8.
pub(crate) struct AuthorizedCurrentPagePopupLoadEvent(RendererPagePopupLoadEventTask);

impl AuthorizedCurrentPagePopupLoadEvent {
    fn new(task: RendererPagePopupLoadEventTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPagePopupLoadEventTask {
        self.0
    }
}

impl PageVm {
    fn current_page_popup_load_event_owner(
        &self,
        expected: RendererPagePopupLoadEventOwner,
    ) -> Option<RendererPagePopupLoadEventOwner> {
        self.vm().current_popup_load_event_owner(
            expected.target(),
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_popup_load_event_turn(
        &mut self,
        task: RendererPagePopupLoadEventTask,
    ) -> anyhow::Result<PagePopupLoadEventTurnOutcome> {
        let owner = task.owner();
        let current_owner = self.current_page_popup_load_event_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            self.vm_mut().apply_current_popup_load_event_body(
                AuthorizedCurrentPagePopupLoadEvent::new(task),
            )?;
            PagePopupLoadEventTargetEffect::DispatchedToCurrentOwner
        } else {
            if owner.root_document() == self.document_lifecycle.identity().document {
                self.vm_mut()
                    .discard_stale_popup_load_event_task(owner.target());
            }
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-Document popup load event"
            );
            PagePopupLoadEventTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PagePopupLoadEventTurnAction {
            owner,
            target_effect,
        };
        Ok(PagePopupLoadEventTurnOutcome::new(action))
    }
}
