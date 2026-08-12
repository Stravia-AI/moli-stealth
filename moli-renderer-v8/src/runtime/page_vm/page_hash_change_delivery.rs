use crate::page_task_queue::{
    PageHashChangeDeliveryTargetEffect, PageHashChangeDeliveryTurnAction,
    PageHashChangeDeliveryTurnOutcome, RendererPageHashChangeDeliveryOwner,
    RendererPageHashChangeDeliveryTask,
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl IntoPageTaskCompletion for PageHashChangeDeliveryTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageHashChangeDeliveryTargetEffect::DispatchedToCurrentOwner => {
                PageTaskCompletion::CallbackCompletion
            }
            PageHashChangeDeliveryTargetEffect::CurrentOwnerHadNoEventTarget => {
                PageTaskCompletion::CheckpointOnly
            }
            PageHashChangeDeliveryTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

/// Proof that the Page arbiter matched both the PageVm namespace and exact
/// recipient LocalDOMWindow before V8 event construction.
pub(crate) struct AuthorizedCurrentPageHashChangeDelivery(RendererPageHashChangeDeliveryTask);

impl AuthorizedCurrentPageHashChangeDelivery {
    fn new(task: RendererPageHashChangeDeliveryTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageHashChangeDeliveryTask {
        self.0
    }
}

impl PageVm {
    fn current_page_hash_change_delivery_owner(
        &self,
        expected: RendererPageHashChangeDeliveryOwner,
    ) -> Option<RendererPageHashChangeDeliveryOwner> {
        self.vm().current_hash_change_delivery_owner(
            expected,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_hash_change_delivery_turn(
        &mut self,
        task: RendererPageHashChangeDeliveryTask,
    ) -> anyhow::Result<PageHashChangeDeliveryTurnOutcome> {
        let owner = task.owner();
        let current_owner = self.current_page_hash_change_delivery_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            if self.vm_mut().apply_current_hash_change_delivery_body(
                AuthorizedCurrentPageHashChangeDelivery::new(task),
            )? {
                PageHashChangeDeliveryTargetEffect::DispatchedToCurrentOwner
            } else {
                PageHashChangeDeliveryTargetEffect::CurrentOwnerHadNoEventTarget
            }
        } else {
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-owner hashchange delivery"
            );
            PageHashChangeDeliveryTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageHashChangeDeliveryTurnAction {
            owner,
            target_effect,
        };
        Ok(PageHashChangeDeliveryTurnOutcome::new(action))
    }
}
