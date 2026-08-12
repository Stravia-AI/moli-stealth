use crate::{
    frame_owner_model::FrameDocumentModuleScriptTerminalBatchTask,
    page_task_queue::{
        PageChildModuleScriptTerminalTargetEffect, PageChildModuleScriptTerminalTurnAction,
        PageChildModuleScriptTerminalTurnOutcome, RendererPageChildModuleScriptTerminalOwner,
        RendererPageChildModuleScriptTerminalTask,
    },
};

use super::PageVm;

/// Proof that the Page arbiter matched the root PageVm namespace and exact
/// child Document/realm before advancing a module-map terminal fanout.
pub(crate) struct AuthorizedCurrentChildModuleScriptTerminal(
    FrameDocumentModuleScriptTerminalBatchTask,
);

impl AuthorizedCurrentChildModuleScriptTerminal {
    fn new(terminal: FrameDocumentModuleScriptTerminalBatchTask) -> Self {
        Self(terminal)
    }

    pub(crate) fn into_terminal(self) -> FrameDocumentModuleScriptTerminalBatchTask {
        self.0
    }

    #[cfg(test)]
    pub(crate) fn new_for_executor_test(
        terminal: FrameDocumentModuleScriptTerminalBatchTask,
    ) -> Self {
        Self(terminal)
    }
}

impl PageVm {
    fn current_page_child_module_script_terminal_owner(
        &self,
        expected: RendererPageChildModuleScriptTerminalOwner,
    ) -> Option<RendererPageChildModuleScriptTerminalOwner> {
        if expected.root_document() != self.document_lifecycle.identity().document {
            return None;
        }
        self.vm()
            .current_child_module_script_terminal_owner(
                expected.document_owner(),
                expected.realm_id(),
            )
            .map(|document_owner| {
                RendererPageChildModuleScriptTerminalOwner::new(
                    expected.root_document(),
                    document_owner,
                    expected.realm_id(),
                )
            })
    }

    pub(in crate::runtime::page_vm) fn page_child_module_script_terminal_is_eligible_for_owner_turn(
        &mut self,
        expected: RendererPageChildModuleScriptTerminalOwner,
    ) -> bool {
        if self.current_page_child_module_script_terminal_owner(expected) != Some(expected) {
            // Stale heads are always runnable so replacement cannot hide the
            // task that must be discarded.
            return true;
        }
        self.vm()
            .current_child_module_terminal_work_is_runnable(expected.document_owner())
    }

    pub(in crate::runtime) fn apply_selected_page_child_module_script_terminal_turn(
        &mut self,
        task: RendererPageChildModuleScriptTerminalTask,
    ) -> PageChildModuleScriptTerminalTurnOutcome {
        let owner = task.owner();
        let current_owner = self.current_page_child_module_script_terminal_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            let outcome = self.vm_mut().apply_current_child_module_script_terminal(
                AuthorizedCurrentChildModuleScriptTerminal::new(task.into_terminal()),
            );
            PageChildModuleScriptTerminalTargetEffect::AppliedToCurrentOwner { outcome }
        } else {
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-owner child module-script terminal"
            );
            PageChildModuleScriptTerminalTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageChildModuleScriptTerminalTurnAction {
            owner,
            target_effect,
        };
        PageChildModuleScriptTerminalTurnOutcome::new(action)
    }

    #[cfg(test)]
    /// Apply one exact module-terminal body without submitting the selected
    /// Page task's completion. Complete task tests must use the shared exact
    /// selector and production dispatcher.
    pub(in crate::runtime) fn run_child_module_script_terminal_body_for_test(
        &mut self,
    ) -> Option<PageChildModuleScriptTerminalTurnOutcome> {
        let task_sources = self.page_task_executor_sources_for_test();
        let task = task_sources.take_child_module_script_terminal_for_executor_test(|owner| {
            self.page_child_module_script_terminal_is_eligible_for_owner_turn(owner)
        })?;
        Some(self.apply_selected_page_child_module_script_terminal_turn(task))
    }
}
