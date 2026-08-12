use crate::page_task_queue::{
    PageTextTrackDefaultModeTargetEffect, PageTextTrackDefaultModeTurnAction,
    PageTextTrackDefaultModeTurnOutcome, RendererPageTextTrackDefaultModeOwner,
    RendererPageTextTrackDefaultModeTask, RendererPageTextTrackDefaultModeTaskId,
    RendererPageTextTrackDefaultModeTaskKind,
};

use super::{AuthorizedCurrentWindowDocumentTask, PageVm};

pub(crate) type AuthorizedCurrentPageTextTrackDefaultMode =
    AuthorizedCurrentWindowDocumentTask<RendererPageTextTrackDefaultModeTask>;

impl PageVm {
    fn current_page_text_track_default_mode_owner(
        &self,
        task_id: RendererPageTextTrackDefaultModeTaskId,
    ) -> Option<(
        RendererPageTextTrackDefaultModeOwner,
        RendererPageTextTrackDefaultModeTaskKind,
    )> {
        self.vm().current_pending_text_track_default_mode_owner(
            task_id,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_text_track_default_mode_turn(
        &mut self,
        task: RendererPageTextTrackDefaultModeTask,
    ) -> anyhow::Result<PageTextTrackDefaultModeTurnOutcome> {
        let owner = task.owner();
        let task_id = task.task_id();
        let kind = task.kind();
        let current = self.current_page_text_track_default_mode_owner(task_id);
        let target_effect =
            match self.authorize_current_window_document_task(task, owner, kind, current) {
                Ok(authorization) => {
                    if self
                        .vm_mut()
                        .apply_current_text_track_default_mode_body(authorization)?
                    {
                        PageTextTrackDefaultModeTargetEffect::AppliedToCurrentOwner
                    } else {
                        PageTextTrackDefaultModeTargetEffect::CurrentOwnerNoLongerEligible
                    }
                }
                Err(stale) => {
                    if stale.may_discard_local_payload() {
                        self.vm_mut()
                            .discard_stale_text_track_default_mode_task(task_id);
                    }
                    let current_owner = stale.current_owner();
                    tracing::debug!(
                        ?owner,
                        ?current_owner,
                        ?task_id,
                        ?kind,
                        "discarded stale exact-Document text-track default-mode task"
                    );
                    PageTextTrackDefaultModeTargetEffect::DiscardedStaleOwner { current_owner }
                }
            };
        let action = PageTextTrackDefaultModeTurnAction {
            owner,
            task_id,
            kind,
            target_effect,
        };
        Ok(PageTextTrackDefaultModeTurnOutcome::new(action))
    }
}
