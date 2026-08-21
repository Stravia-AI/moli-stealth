use crate::{
    frame_owner_model::FrameDocumentModulepreloadEventAction,
    page_task_queue::{
        PageChildModulepreloadEventActionTargetEffect, PageChildModulepreloadEventActionTurnAction,
        PageChildModulepreloadEventActionTurnOutcome,
        RendererPageChildModulepreloadEventActionOwner,
        RendererPageChildModulepreloadEventActionTask,
    },
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl IntoPageTaskCompletion for PageChildModulepreloadEventActionTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageChildModulepreloadEventActionTargetEffect::AppliedToCurrentOwner { outcome }
                if outcome.event_was_dispatched() =>
            {
                PageTaskCompletion::CallbackCompletion
            }
            PageChildModulepreloadEventActionTargetEffect::AppliedToCurrentOwner { .. } => {
                // Entering the exact current child realm used to complete an
                // ordinary task checkpoint even when the link target vanished
                // before dispatch. No listener callback exists to justify
                // child/runtime follow-up in that case.
                PageTaskCompletion::CheckpointOnly
            }
            PageChildModulepreloadEventActionTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

/// Proof that the Page arbiter matched the stable root namespace and the exact
/// child Document/realm before entering V8 for a modulepreload event action.
pub(crate) struct AuthorizedCurrentChildModulepreloadEventAction(
    FrameDocumentModulepreloadEventAction,
);

impl AuthorizedCurrentChildModulepreloadEventAction {
    fn new(action: FrameDocumentModulepreloadEventAction) -> Self {
        Self(action)
    }

    pub(crate) fn into_action(self) -> FrameDocumentModulepreloadEventAction {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn new_for_executor_test(action: FrameDocumentModulepreloadEventAction) -> Self {
        Self(action)
    }
}

impl PageVm {
    fn current_page_child_modulepreload_event_action_owner(
        &self,
        expected: RendererPageChildModulepreloadEventActionOwner,
    ) -> Option<RendererPageChildModulepreloadEventActionOwner> {
        if expected.root_document() != self.document_lifecycle.identity().document {
            return None;
        }
        self.vm()
            .current_child_modulepreload_event_action_owner(
                expected.document_owner().document_owner(),
                expected.realm_id(),
            )
            .map(|document_owner| {
                RendererPageChildModulepreloadEventActionOwner::new(
                    expected.root_document(),
                    document_owner,
                    expected.realm_id(),
                )
            })
    }

    pub(in crate::runtime::page_vm) fn page_child_modulepreload_event_action_is_eligible_for_owner_turn(
        &mut self,
        expected: RendererPageChildModulepreloadEventActionOwner,
    ) -> bool {
        if self.current_page_child_modulepreload_event_action_owner(expected) != Some(expected) {
            return true;
        }
        self.vm()
            .current_child_modulepreload_event_action_is_runnable(
                expected.document_owner(),
                expected.realm_id(),
            )
    }

    pub(in crate::runtime) fn apply_selected_page_child_modulepreload_event_action_turn(
        &mut self,
        task: RendererPageChildModulepreloadEventActionTask,
    ) -> PageChildModulepreloadEventActionTurnOutcome {
        let owner = task.owner();
        let current_owner = self.current_page_child_modulepreload_event_action_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            let outcome = self
                .vm_mut()
                .apply_current_child_modulepreload_event_action(
                    AuthorizedCurrentChildModulepreloadEventAction::new(task.into_action()),
                );
            PageChildModulepreloadEventActionTargetEffect::AppliedToCurrentOwner { outcome }
        } else {
            drop(task);
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-owner child modulepreload event action"
            );
            PageChildModulepreloadEventActionTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageChildModulepreloadEventActionTurnAction {
            owner,
            target_effect,
        };
        PageChildModulepreloadEventActionTurnOutcome::new(action)
    }
}
