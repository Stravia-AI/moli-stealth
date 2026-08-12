use crate::page_task_queue::{
    PageImageLoadEventStalePayloadEffect, PageImageLoadEventTargetEffect,
    PageImageLoadEventTurnAction, PageImageLoadEventTurnOutcome, RendererPageImageLoadEventKind,
    RendererPageImageLoadEventOwner, RendererPageImageLoadEventTask,
    RendererPageImageLoadEventTaskId,
};

use super::{
    AuthorizedCurrentWindowDocumentTask, IntoPageTaskCompletion, PageTaskCompletion, PageVm,
};

impl IntoPageTaskCompletion for PageImageLoadEventTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageImageLoadEventTargetEffect::DispatchedToCurrentOwner
            | PageImageLoadEventTargetEffect::SettledCurrentOwnerWithoutEvent
            | PageImageLoadEventTargetEffect::DiscardedStaleOwner {
                stale_payload_effect:
                    PageImageLoadEventStalePayloadEffect::SettledExactPayloadAndProcessedDecodeRequests,
                ..
            } => PageTaskCompletion::CallbackCompletion,
            PageImageLoadEventTargetEffect::DiscardedStaleOwner {
                stale_payload_effect:
                    PageImageLoadEventStalePayloadEffect::ForeignPageVmStatePreserved
                    | PageImageLoadEventStalePayloadEffect::NoSettledExactPayload,
                ..
            } => PageTaskCompletion::NoCompletion,
        }
    }
}

pub(crate) type AuthorizedCurrentPageImageLoadEvent =
    AuthorizedCurrentWindowDocumentTask<RendererPageImageLoadEventTask>;

impl PageVm {
    fn current_page_image_load_event_owner(
        &self,
        task_id: RendererPageImageLoadEventTaskId,
    ) -> Option<(
        RendererPageImageLoadEventOwner,
        RendererPageImageLoadEventKind,
    )> {
        self.vm().current_pending_image_load_event_owner(
            task_id,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_image_load_event_turn(
        &mut self,
        task: RendererPageImageLoadEventTask,
    ) -> anyhow::Result<PageImageLoadEventTurnOutcome> {
        let owner = task.owner();
        let task_id = task.task_id();
        let kind = task.kind();
        let current = self.current_page_image_load_event_owner(task_id);
        let target_effect = match self
            .authorize_current_window_document_task(task, owner, kind, current)
        {
            Ok(authorization) => self
                .vm_mut()
                .apply_current_image_load_event_body(authorization)?,
            Err(stale) => {
                let stale_payload_effect = if stale.may_discard_local_payload() {
                    if self
                        .vm_mut()
                        .discard_stale_image_load_event_task_body(task_id)?
                    {
                        PageImageLoadEventStalePayloadEffect::SettledExactPayloadAndProcessedDecodeRequests
                    } else {
                        PageImageLoadEventStalePayloadEffect::NoSettledExactPayload
                    }
                } else {
                    PageImageLoadEventStalePayloadEffect::ForeignPageVmStatePreserved
                };
                let current_owner = stale.current_owner();
                tracing::debug!(
                    ?owner,
                    ?current_owner,
                    ?task_id,
                    ?kind,
                    "discarded stale exact-Document image load event task"
                );
                PageImageLoadEventTargetEffect::DiscardedStaleOwner {
                    current_owner,
                    stale_payload_effect,
                }
            }
        };
        let action = PageImageLoadEventTurnAction {
            owner,
            task_id,
            kind,
            target_effect,
        };
        Ok(PageImageLoadEventTurnOutcome::new(action))
    }
}
