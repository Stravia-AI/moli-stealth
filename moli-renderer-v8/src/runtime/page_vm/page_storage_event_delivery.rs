use crate::page_task_queue::{
    PageStorageEventDeliveryTargetEffect, PageStorageEventDeliveryTurnAction,
    PageStorageEventDeliveryTurnOutcome, RendererPageStorageEventDeliveryOwner,
    RendererPageStorageEventDeliveryTask,
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl IntoPageTaskCompletion for PageStorageEventDeliveryTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageStorageEventDeliveryTargetEffect::DispatchedToCurrentOwner => {
                PageTaskCompletion::CallbackCompletion
            }
            PageStorageEventDeliveryTargetEffect::CurrentOwnerHadNoEventTarget => {
                PageTaskCompletion::CheckpointOnly
            }
            PageStorageEventDeliveryTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

/// Proof that the Page arbiter matched both the PageVm namespace and the exact
/// recipient LocalDOMWindow before the V8 executor received event data.
pub(crate) struct AuthorizedCurrentPageStorageEventDelivery(RendererPageStorageEventDeliveryTask);

impl AuthorizedCurrentPageStorageEventDelivery {
    fn new(task: RendererPageStorageEventDeliveryTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageStorageEventDeliveryTask {
        self.0
    }
}

impl PageVm {
    fn current_page_storage_event_delivery_owner(
        &self,
        expected: RendererPageStorageEventDeliveryOwner,
    ) -> Option<RendererPageStorageEventDeliveryOwner> {
        self.vm().current_storage_event_delivery_owner(
            expected,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_storage_event_delivery_turn(
        &mut self,
        task: RendererPageStorageEventDeliveryTask,
    ) -> anyhow::Result<PageStorageEventDeliveryTurnOutcome> {
        let owner = task.owner();
        let current_owner = self.current_page_storage_event_delivery_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            if self.vm_mut().apply_current_storage_event_delivery_body(
                AuthorizedCurrentPageStorageEventDelivery::new(task),
            )? {
                PageStorageEventDeliveryTargetEffect::DispatchedToCurrentOwner
            } else {
                PageStorageEventDeliveryTargetEffect::CurrentOwnerHadNoEventTarget
            }
        } else {
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-owner StorageEvent delivery"
            );
            PageStorageEventDeliveryTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageStorageEventDeliveryTurnAction {
            owner,
            target_effect,
        };
        Ok(PageStorageEventDeliveryTurnOutcome::new(action))
    }
}
