use crate::page_task_queue::{
    PageBroadcastChannelDeliveryDocumentEffect, PageBroadcastChannelDeliveryTurnAction,
    PageBroadcastChannelDeliveryTurnOutcome, RendererPageBroadcastChannelDeliveryOwner,
    RendererPageBroadcastChannelDeliveryTask,
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl IntoPageTaskCompletion for PageBroadcastChannelDeliveryTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.document_effect {
            PageBroadcastChannelDeliveryDocumentEffect::DispatchedToCurrentOwner => {
                PageTaskCompletion::CallbackCompletion
            }
            PageBroadcastChannelDeliveryDocumentEffect::CurrentOwnerHadNoPendingEvent => {
                PageTaskCompletion::CheckpointOnly
            }
            PageBroadcastChannelDeliveryDocumentEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

/// Proof that the Page arbiter matched both the PageVm root namespace and the
/// exact accepting Window realm before V8 dispatch.
pub(crate) struct AuthorizedCurrentBroadcastChannelDelivery(
    RendererPageBroadcastChannelDeliveryTask,
);

impl AuthorizedCurrentBroadcastChannelDelivery {
    fn new(task: RendererPageBroadcastChannelDeliveryTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageBroadcastChannelDeliveryTask {
        self.0
    }
}

impl PageVm {
    fn current_page_broadcast_channel_delivery_owner(
        &self,
        expected: RendererPageBroadcastChannelDeliveryOwner,
    ) -> Option<RendererPageBroadcastChannelDeliveryOwner> {
        self.vm()
            .broadcast_channel_delivery_owner_is_current(expected)
            .then(|| {
                RendererPageBroadcastChannelDeliveryOwner::new(
                    self.document_lifecycle.identity().document,
                    expected.execution_context(),
                )
            })
    }

    pub(in crate::runtime) fn apply_selected_page_broadcast_channel_delivery_turn(
        &mut self,
        task: RendererPageBroadcastChannelDeliveryTask,
    ) -> anyhow::Result<PageBroadcastChannelDeliveryTurnOutcome> {
        let owner = task.owner();
        let channel_id = task.channel_id();
        let current_owner = self.current_page_broadcast_channel_delivery_owner(owner);
        let document_effect = if current_owner == Some(owner) {
            if self
                .vm_mut()
                .apply_current_broadcast_channel_delivery_body(
                    AuthorizedCurrentBroadcastChannelDelivery::new(task),
                )?
            {
                PageBroadcastChannelDeliveryDocumentEffect::DispatchedToCurrentOwner
            } else {
                PageBroadcastChannelDeliveryDocumentEffect::CurrentOwnerHadNoPendingEvent
            }
        } else {
            self.vm_mut()
                .discard_stale_broadcast_channel_delivery(channel_id);
            tracing::debug!(
                ?owner,
                ?current_owner,
                channel_id,
                "discarded stale exact-owner BroadcastChannel delivery"
            );
            PageBroadcastChannelDeliveryDocumentEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageBroadcastChannelDeliveryTurnAction {
            owner,
            channel_id,
            document_effect,
        };
        Ok(PageBroadcastChannelDeliveryTurnOutcome::new(action))
    }
}
