use crate::page_task_queue::{
    PageTextTrackLoadStalePayloadEffect, PageTextTrackLoadTargetEffect,
    PageTextTrackLoadTurnAction, PageTextTrackLoadTurnOutcome, RendererPageTextTrackLoadOwner,
    RendererPageTextTrackLoadTask, RendererPageTextTrackLoadTaskId,
    RendererPageTextTrackLoadTaskKind,
};

use super::{AuthorizedCurrentWindowDocumentTask, PageVm};

pub(crate) type AuthorizedCurrentPageTextTrackLoad =
    AuthorizedCurrentWindowDocumentTask<RendererPageTextTrackLoadTask>;

impl PageVm {
    fn current_page_text_track_load_owner(
        &self,
        task_id: RendererPageTextTrackLoadTaskId,
    ) -> Option<(
        RendererPageTextTrackLoadOwner,
        RendererPageTextTrackLoadTaskKind,
    )> {
        self.vm().current_pending_text_track_load_owner(
            task_id,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_text_track_load_turn(
        &mut self,
        task: RendererPageTextTrackLoadTask,
    ) -> anyhow::Result<PageTextTrackLoadTurnOutcome> {
        let owner = task.owner();
        let task_id = task.task_id();
        let kind = task.kind();
        let current = self.current_page_text_track_load_owner(task_id);
        let target_effect =
            match self.authorize_current_window_document_task(task, owner, kind, current) {
                Ok(authorization) => {
                    if self
                        .vm_mut()
                        .apply_current_text_track_load_body(authorization)?
                    {
                        PageTextTrackLoadTargetEffect::AppliedToCurrentOwner
                    } else {
                        PageTextTrackLoadTargetEffect::CurrentOwnerNoLongerEligible
                    }
                }
                Err(stale) => {
                    let stale_payload_effect = if stale.may_discard_local_payload() {
                        if self
                            .vm_mut()
                            .discard_stale_text_track_load_task_body(task_id)?
                        {
                            PageTextTrackLoadStalePayloadEffect::DiscardedExactPayload
                        } else {
                            PageTextTrackLoadStalePayloadEffect::NoDiscardedExactPayload
                        }
                    } else {
                        PageTextTrackLoadStalePayloadEffect::ForeignPageVmStatePreserved
                    };
                    let current_owner = stale.current_owner();
                    tracing::debug!(
                        ?owner,
                        ?current_owner,
                        ?task_id,
                        ?kind,
                        "discarded stale exact-Document text-track load task"
                    );
                    PageTextTrackLoadTargetEffect::DiscardedStaleOwner {
                        current_owner,
                        stale_payload_effect,
                    }
                }
            };
        let action = PageTextTrackLoadTurnAction {
            owner,
            task_id,
            kind,
            target_effect,
        };
        Ok(PageTextTrackLoadTurnOutcome::new(action))
    }
}
