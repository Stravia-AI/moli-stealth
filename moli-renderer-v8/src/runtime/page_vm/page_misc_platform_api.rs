use crate::page_task_queue::{
    PageMiscPlatformApiTargetEffect, PageMiscPlatformApiTurnAction, PageMiscPlatformApiTurnOutcome,
    RendererPageMiscPlatformApiOwner, RendererPageMiscPlatformApiTask,
    RendererPageMiscPlatformApiTaskId, RendererPageMiscPlatformApiTaskKind,
};

use super::{
    AuthorizedCurrentWindowDocumentTask, IntoPageTaskCompletion, PageTaskCompletion, PageVm,
};

impl IntoPageTaskCompletion for PageMiscPlatformApiTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageMiscPlatformApiTargetEffect::CallbackInvokedForCurrentOwner => {
                PageTaskCompletion::CallbackCompletion
            }
            PageMiscPlatformApiTargetEffect::CurrentOwnerCallbackRetired => {
                PageTaskCompletion::CheckpointOnly
            }
            PageMiscPlatformApiTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

pub(crate) type AuthorizedCurrentPageMiscPlatformApiTask =
    AuthorizedCurrentWindowDocumentTask<RendererPageMiscPlatformApiTask>;

impl PageVm {
    fn current_page_misc_platform_api_owner(
        &self,
        task_id: RendererPageMiscPlatformApiTaskId,
    ) -> Option<(
        RendererPageMiscPlatformApiOwner,
        RendererPageMiscPlatformApiTaskKind,
    )> {
        self.vm().current_pending_misc_platform_api_owner(
            task_id,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_misc_platform_api_turn(
        &mut self,
        task: RendererPageMiscPlatformApiTask,
    ) -> anyhow::Result<PageMiscPlatformApiTurnOutcome> {
        let owner = task.owner();
        let task_id = task.task_id();
        let kind = task.kind();
        let current = self.current_page_misc_platform_api_owner(task_id);
        let target_effect =
            match self.authorize_current_window_document_task(task, owner, kind, current) {
                Ok(authorization) => self
                    .vm_mut()
                    .apply_current_misc_platform_api_task_body(authorization)?,
                Err(stale) => {
                    if stale.may_discard_local_payload() {
                        self.vm_mut().discard_stale_misc_platform_api_task(task_id);
                    }
                    let current_owner = stale.current_owner();
                    tracing::debug!(
                        ?owner,
                        ?current_owner,
                        ?task_id,
                        ?kind,
                        "discarded stale exact-Document miscellaneous-platform API task"
                    );
                    PageMiscPlatformApiTargetEffect::DiscardedStaleOwner { current_owner }
                }
            };
        let action = PageMiscPlatformApiTurnAction {
            owner,
            task_id,
            kind,
            target_effect,
        };
        Ok(PageMiscPlatformApiTurnOutcome::new(action))
    }
}
