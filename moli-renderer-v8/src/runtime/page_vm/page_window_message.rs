use crate::page_task_queue::{
    PageWindowMessageTargetEffect, PageWindowMessageTurnAction, PageWindowMessageTurnOutcome,
    RendererPageWindowMessageOwner, RendererPageWindowMessageTask,
};

use super::PageVm;

/// Proof that the Page arbiter matched both the root PageVm namespace and the
/// LocalWindow target before the V8 executor touched the local payload.
pub(crate) struct AuthorizedCurrentPageWindowMessage(RendererPageWindowMessageTask);

impl AuthorizedCurrentPageWindowMessage {
    fn new(task: RendererPageWindowMessageTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageWindowMessageTask {
        self.0
    }
}

impl PageVm {
    fn current_page_window_message_owner(
        &self,
        expected: RendererPageWindowMessageOwner,
    ) -> Option<RendererPageWindowMessageOwner> {
        self.vm()
            .current_window_task_target(expected.target())
            .map(|target| {
                RendererPageWindowMessageOwner::new(
                    self.document_lifecycle.identity().document,
                    target,
                )
            })
    }

    pub(in crate::runtime::page_vm) fn page_window_message_is_eligible_for_owner_turn(
        &mut self,
        expected: RendererPageWindowMessageOwner,
        task_id: crate::page_task_queue::RendererPageWindowMessageTaskId,
    ) -> bool {
        let current = self.current_page_window_message_owner(expected);
        current != Some(expected)
            || (self
                .vm()
                .window_message_task_is_materialized(expected, task_id))
    }

    pub(in crate::runtime) fn apply_selected_page_window_message_turn(
        &mut self,
        task: RendererPageWindowMessageTask,
    ) -> anyhow::Result<PageWindowMessageTurnOutcome> {
        let owner = task.owner();
        let task_id = task.task_id();
        let current_owner = self.current_page_window_message_owner(owner);
        let target_effect = if current_owner == Some(owner) {
            match self.vm_mut().apply_current_window_message_task_body(
                AuthorizedCurrentPageWindowMessage::new(task),
            )? {
                crate::window_host::WindowMessageTaskRunResult::Completed => {
                    PageWindowMessageTargetEffect::AppliedToCurrentOwner
                }
                crate::window_host::WindowMessageTaskRunResult::Idle => {
                    PageWindowMessageTargetEffect::CurrentOwnerHadNoPendingMessage
                }
                crate::window_host::WindowMessageTaskRunResult::Blocked => {
                    anyhow::bail!(
                        "scheduler selected a Window.postMessage task before its target materialized"
                    )
                }
            }
        } else {
            // A task from an older PageVm has no payload in the current Host;
            // touching its local id could discard a newly reused id. Same-
            // PageVm LocalWindow retirement, however, may still leave an
            // undelivered payload that this exact id is responsible to clean.
            if owner.root_document() == self.document_lifecycle.identity().document {
                self.vm_mut().discard_stale_window_message_task(task_id);
            }
            tracing::debug!(
                ?owner,
                ?current_owner,
                ?task_id,
                "discarded stale exact-owner Window.postMessage task"
            );
            PageWindowMessageTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageWindowMessageTurnAction {
            owner,
            task_id,
            target_effect,
        };
        Ok(PageWindowMessageTurnOutcome::new(action))
    }
}
