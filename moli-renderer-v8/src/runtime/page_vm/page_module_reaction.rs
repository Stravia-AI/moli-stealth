use crate::page_task_queue::{
    PageModuleReactionApplication, PageModuleReactionFollowup, PageModuleReactionTargetEffect,
    PageModuleReactionTurnAction, PageModuleReactionTurnOutcome, RendererPageModuleReactionOwner,
    RendererPageModuleReactionTask,
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

/// Proof that the Page arbiter matched a selected reaction against the root
/// PageVm namespace and its exact Document/realm target.
pub(crate) struct AuthorizedCurrentPageModuleReaction(RendererPageModuleReactionTask);

impl AuthorizedCurrentPageModuleReaction {
    fn new(task: RendererPageModuleReactionTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageModuleReactionTask {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn new_for_executor_test(task: RendererPageModuleReactionTask) -> Self {
        Self(task)
    }
}

impl IntoPageTaskCompletion for PageModuleReactionTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect() {
            PageModuleReactionTargetEffect::AppliedToCurrentOwner(_) => {
                PageTaskCompletion::CheckpointOnly
            }
            PageModuleReactionTargetEffect::DiscardedMissingReaction
            | PageModuleReactionTargetEffect::IgnoredStaleOwner => PageTaskCompletion::NoCompletion,
        }
    }
}

impl PageVm {
    fn current_page_module_reaction_owner(
        &self,
        expected: RendererPageModuleReactionOwner,
    ) -> Option<RendererPageModuleReactionOwner> {
        if expected.root_document() != self.document_lifecycle.identity().document
            || !self
                .vm()
                .module_reaction_target_is_current(expected.target())
        {
            return None;
        }
        Some(expected)
    }

    pub(in crate::runtime) fn apply_selected_page_module_reaction_turn(
        &mut self,
        task: RendererPageModuleReactionTask,
    ) -> anyhow::Result<PageModuleReactionTurnOutcome> {
        let owner = task.owner();
        let current_owner = self.current_page_module_reaction_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            match self.vm_mut().apply_current_page_module_reaction(
                AuthorizedCurrentPageModuleReaction::new(task),
            )? {
                PageModuleReactionApplication::Applied {
                    current_effect,
                    followup,
                } => {
                    match followup {
                        PageModuleReactionFollowup::None => {}
                        PageModuleReactionFollowup::MainParserOwnedEvaluations {
                            ready_action_count,
                        } => {
                            let queued = self.vm_mut().enqueue_parser_owned_module_continuation();
                            debug_assert!(
                                queued,
                                "{ready_action_count} current parser evaluation action(s) must enter the typed main-Document source"
                            );
                        }
                        PageModuleReactionFollowup::RuntimeOwnedModuleContinuation => {
                            let queued = self.vm_mut().enqueue_runtime_owned_module_continuation();
                            debug_assert!(
                                queued,
                                "current runtime module reaction must retain a current main Document"
                            );
                        }
                    }
                    PageModuleReactionTargetEffect::AppliedToCurrentOwner(current_effect)
                }
                PageModuleReactionApplication::NoPendingReaction => {
                    PageModuleReactionTargetEffect::DiscardedMissingReaction
                }
            }
        } else {
            if owner.root_document() == self.document_lifecycle.identity().document {
                self.vm_mut()
                    .discard_stale_page_module_reaction(&task.into_event());
            }
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-owner module reaction"
            );
            PageModuleReactionTargetEffect::IgnoredStaleOwner
        };
        let action = PageModuleReactionTurnAction::new(owner, target_effect);
        Ok(PageModuleReactionTurnOutcome::new(action))
    }
}
