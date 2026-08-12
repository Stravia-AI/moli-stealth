use crate::{
    frame_owner_model::FrameDocumentDynamicImportTerminalPreparedAction,
    page_task_queue::{
        PageDynamicImportOwnerActionDocumentEffect, PageDynamicImportOwnerActionTurnAction,
        PageDynamicImportOwnerActionTurnOutcome, RendererPageDynamicImportOwnerActionOwner,
        RendererPageDynamicImportOwnerActionTask,
    },
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

/// Proof that the Page arbiter matched the stable root-Document namespace and
/// the complete child Document/realm target before executing an owner action.
pub(crate) struct AuthorizedCurrentChildDynamicImportOwnerAction(
    FrameDocumentDynamicImportTerminalPreparedAction,
);

impl AuthorizedCurrentChildDynamicImportOwnerAction {
    fn new(action: FrameDocumentDynamicImportTerminalPreparedAction) -> Self {
        Self(action)
    }

    pub(crate) fn into_action(self) -> FrameDocumentDynamicImportTerminalPreparedAction {
        self.0
    }
}

impl IntoPageTaskCompletion for PageDynamicImportOwnerActionTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.document_effect {
            // Every current entry is one selected internal script-
            // continuation task, including variants that only update owner
            // state or schedule a fetch. Promise settlement, when present,
            // deliberately left its user reactions for this checkpoint. The
            // old path did not perform callback-style child/runtime follow-up,
            // so this remains CheckpointOnly rather than CallbackCompletion.
            PageDynamicImportOwnerActionDocumentEffect::AppliedToCurrentOwner { .. } => {
                PageTaskCompletion::CheckpointOnly
            }
            // An old root/child/realm ticket did not execute a current task
            // body and must not enter replacement V8 just to manufacture a
            // checkpoint.
            PageDynamicImportOwnerActionDocumentEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

impl PageVm {
    fn current_page_dynamic_import_owner_action_owner(
        &self,
        expected: RendererPageDynamicImportOwnerActionOwner,
    ) -> Option<RendererPageDynamicImportOwnerActionOwner> {
        let root_document = self.document_lifecycle.identity().document;
        self.vm()
            .current_child_dynamic_import_task_owner(
                expected.task_owner().document_owner(),
                expected.realm_id(),
            )
            .map(|task_owner| {
                RendererPageDynamicImportOwnerActionOwner::new(
                    root_document,
                    task_owner,
                    expected.realm_id(),
                )
            })
    }

    pub(in crate::runtime) fn apply_selected_page_dynamic_import_owner_action_turn(
        &mut self,
        task: RendererPageDynamicImportOwnerActionTask,
    ) -> PageDynamicImportOwnerActionTurnOutcome {
        let owner = task.owner();
        let current_owner = self.current_page_dynamic_import_owner_action_owner(owner);
        let document_effect = if current_owner == Some(owner) {
            let outcome = self
                .vm_mut()
                .apply_current_child_dynamic_import_owner_action(
                    AuthorizedCurrentChildDynamicImportOwnerAction::new(task.into_action()),
                );
            PageDynamicImportOwnerActionDocumentEffect::AppliedToCurrentOwner { outcome }
        } else {
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-owner child dynamic-import owner action"
            );
            PageDynamicImportOwnerActionDocumentEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageDynamicImportOwnerActionTurnAction {
            owner,
            document_effect,
        };
        PageDynamicImportOwnerActionTurnOutcome::new(action)
    }

    /// Test-only body executor for assertions about the owner-action domain
    /// transition itself.
    ///
    /// This deliberately does not submit the selected task's checkpoint.
    /// Page behavior tests must normally use the exact production dispatcher
    /// through `PageSelectedTaskTestSelector::DynamicImportOwnerAction`.
    #[cfg(test)]
    pub(in crate::runtime) fn run_page_dynamic_import_owner_action_body_for_test(
        &mut self,
    ) -> Option<PageDynamicImportOwnerActionTurnOutcome> {
        let source = self
            .page_task_executor_sources_for_test()
            .dynamic_import_owner_action();
        let candidate = source.next_ready_metadata()?;
        let (actual, task) = source
            .pop_front()
            .expect("selected standalone dynamic-import action must remain queued");
        debug_assert_eq!(actual, candidate);
        Some(self.apply_selected_page_dynamic_import_owner_action_turn(task))
    }
}
