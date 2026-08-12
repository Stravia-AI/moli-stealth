use anyhow::Result;

use crate::{
    page_task_queue::{
        PageWorkerHostBridgeCurrentEffect, PageWorkerHostBridgeTargetEffect,
        PageWorkerHostBridgeTurnAction, PageWorkerHostBridgeTurnOutcome,
        RendererPageWorkerHostBridgeTask,
    },
    runtime::PageOwnerTurnOutcome,
    script_vm::WorkerHostBridgeBodyEffect,
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl From<WorkerHostBridgeBodyEffect> for PageWorkerHostBridgeTargetEffect {
    fn from(effect: WorkerHostBridgeBodyEffect) -> Self {
        match effect {
            WorkerHostBridgeBodyEffect::StateAppliedWithoutPageContext => {
                Self::AppliedToCurrentOwner(
                    PageWorkerHostBridgeCurrentEffect::StateAppliedWithoutPageContext,
                )
            }
            WorkerHostBridgeBodyEffect::StateAppliedInPageContext => Self::AppliedToCurrentOwner(
                PageWorkerHostBridgeCurrentEffect::StateAppliedInPageContext,
            ),
            WorkerHostBridgeBodyEffect::ExactTargetUnavailable => Self::IgnoredStaleTarget,
        }
    }
}

impl IntoPageTaskCompletion for PageWorkerHostBridgeTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect() {
            PageWorkerHostBridgeTargetEffect::AppliedToCurrentOwner(
                PageWorkerHostBridgeCurrentEffect::StateAppliedInPageContext,
            )
            | PageWorkerHostBridgeTargetEffect::AppliedToCurrentOwner(
                PageWorkerHostBridgeCurrentEffect::StateAppliedWithoutPageContext,
            ) => PageTaskCompletion::CheckpointOnly,
            PageWorkerHostBridgeTargetEffect::IgnoredStaleRoot
            | PageWorkerHostBridgeTargetEffect::IgnoredStaleTarget => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

impl PageVm {
    /// Applies one Worker host/control record already selected from the
    /// Page-owned networking FIFO.
    ///
    /// Root-Document authorization happens before the event reaches ScriptVm.
    /// Worker identity remains ScriptVm-owned because DedicatedWorker realm
    /// generations and SharedWorker host registrations live there. A stale
    /// record is consumed without producing protocol-visible activity.
    pub(in crate::runtime) fn apply_selected_page_worker_host_bridge_turn(
        &mut self,
        task: RendererPageWorkerHostBridgeTask,
    ) -> Result<PageWorkerHostBridgeTurnOutcome> {
        let owner = task.owner();
        let target_effect = if owner.root_document() != self.document_lifecycle.identity().document
        {
            PageWorkerHostBridgeTargetEffect::IgnoredStaleRoot
        } else {
            self.vm_mut()
                .apply_current_worker_host_bridge_event_body(task.into_event())?
                .into()
        };
        let action = PageWorkerHostBridgeTurnAction::new(owner, target_effect);
        Ok(PageOwnerTurnOutcome::new(action))
    }
}
