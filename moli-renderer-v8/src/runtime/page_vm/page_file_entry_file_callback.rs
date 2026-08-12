use crate::page_task_queue::{
    PageFileEntryFileCallbackTargetEffect, PageFileEntryFileCallbackTurnAction,
    PageFileEntryFileCallbackTurnOutcome, RendererPageFileEntryFileCallbackOwner,
    RendererPageFileEntryFileCallbackTask, RendererPageFileEntryFileCallbackTaskId,
    RendererPageFileEntryFileCallbackTaskKind,
};

use super::{
    AuthorizedCurrentWindowDocumentTask, IntoPageTaskCompletion, PageTaskCompletion, PageVm,
};

impl IntoPageTaskCompletion for PageFileEntryFileCallbackTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageFileEntryFileCallbackTargetEffect::CallbackInvokedForCurrentOwner => {
                PageTaskCompletion::CallbackCompletion
            }
            PageFileEntryFileCallbackTargetEffect::CurrentOwnerCallbackRetired => {
                PageTaskCompletion::CheckpointOnly
            }
            PageFileEntryFileCallbackTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

pub(crate) type AuthorizedCurrentPageFileEntryFileCallback =
    AuthorizedCurrentWindowDocumentTask<RendererPageFileEntryFileCallbackTask>;

impl PageVm {
    fn current_page_file_entry_file_callback_owner(
        &self,
        task_id: RendererPageFileEntryFileCallbackTaskId,
    ) -> Option<(
        RendererPageFileEntryFileCallbackOwner,
        RendererPageFileEntryFileCallbackTaskKind,
    )> {
        self.vm().current_pending_file_entry_file_callback_owner(
            task_id,
            self.document_lifecycle.identity().document,
        )
    }

    pub(in crate::runtime) fn apply_selected_page_file_entry_file_callback_turn(
        &mut self,
        task: RendererPageFileEntryFileCallbackTask,
    ) -> anyhow::Result<PageFileEntryFileCallbackTurnOutcome> {
        let owner = task.owner();
        let task_id = task.task_id();
        let kind = task.kind();
        let current = self.current_page_file_entry_file_callback_owner(task_id);
        let target_effect =
            match self.authorize_current_window_document_task(task, owner, kind, current) {
                Ok(authorization) => self
                    .vm_mut()
                    .apply_current_file_entry_file_callback_body(authorization)?,
                Err(stale) => {
                    if stale.may_discard_local_payload() {
                        self.vm_mut()
                            .discard_stale_file_entry_file_callback_task(task_id);
                    }
                    let current_owner = stale.current_owner();
                    tracing::debug!(
                        ?owner,
                        ?current_owner,
                        ?task_id,
                        ?kind,
                        "discarded stale exact-Document FileSystemFileEntry.file callback"
                    );
                    PageFileEntryFileCallbackTargetEffect::DiscardedStaleOwner { current_owner }
                }
            };
        let action = PageFileEntryFileCallbackTurnAction {
            owner,
            task_id,
            kind,
            target_effect,
        };
        Ok(PageFileEntryFileCallbackTurnOutcome::new(action))
    }
}
