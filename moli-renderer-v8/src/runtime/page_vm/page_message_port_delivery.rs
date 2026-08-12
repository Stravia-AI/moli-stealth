use crate::{
    context_bootstrap::MessagePortDeliveryRunResult,
    page_task_queue::{
        PageMessagePortDeliveryTargetEffect, PageMessagePortDeliveryTurnAction,
        PageMessagePortDeliveryTurnOutcome, RendererPageMessagePortDeliveryOwner,
        RendererPageMessagePortDeliveryTask,
    },
};

use super::PageVm;

/// Proof that the Page arbiter matched the selected task against the port's
/// current root PageVm and exact Window attachment.
pub(crate) struct AuthorizedCurrentPageMessagePortDelivery(RendererPageMessagePortDeliveryTask);

impl AuthorizedCurrentPageMessagePortDelivery {
    fn new(task: RendererPageMessagePortDeliveryTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageMessagePortDeliveryTask {
        self.0
    }
}

impl PageVm {
    fn current_page_message_port_delivery_owner(
        &self,
        port_id: crate::types::MessagePortId,
    ) -> Option<RendererPageMessagePortDeliveryOwner> {
        let execution_context = self
            .vm()
            .current_message_port_execution_context_identity(port_id)?;
        Some(RendererPageMessagePortDeliveryOwner::new(
            self.document_lifecycle.identity().document,
            execution_context,
        ))
    }

    pub(in crate::runtime) fn apply_selected_page_message_port_delivery_turn(
        &mut self,
        task: RendererPageMessagePortDeliveryTask,
        same_attachment_task_is_ready: bool,
    ) -> anyhow::Result<PageMessagePortDeliveryTurnOutcome> {
        let owner = task.owner();
        let port_id = task.port_id();
        let current_owner = self.current_page_message_port_delivery_owner(port_id);
        let target_effect = if current_owner == Some(owner) {
            match self.vm_mut().apply_current_message_port_delivery_body(
                AuthorizedCurrentPageMessagePortDelivery::new(task),
                same_attachment_task_is_ready,
            )? {
                MessagePortDeliveryRunResult::Consumed {
                    callback_dispatched,
                } => PageMessagePortDeliveryTargetEffect::ConsumedByCurrentOwner {
                    callback_dispatched,
                },
                MessagePortDeliveryRunResult::Idle => {
                    PageMessagePortDeliveryTargetEffect::CurrentOwnerHadNoReadyEvent
                }
            }
        } else {
            // The registry payload belongs to the port's current attachment.
            // A stale wake must never consume or close it.
            tracing::debug!(
                ?owner,
                ?current_owner,
                port_id,
                "ignored stale exact-owner MessagePort delivery task"
            );
            PageMessagePortDeliveryTargetEffect::IgnoredStaleOwner { current_owner }
        };
        let action = PageMessagePortDeliveryTurnAction {
            owner,
            port_id,
            target_effect,
        };
        Ok(PageMessagePortDeliveryTurnOutcome::new(action))
    }
}
