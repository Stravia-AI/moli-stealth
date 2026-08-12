use crate::{
    page_task_queue::{
        PageSharedWorkerClientEventTargetEffect, PageSharedWorkerClientEventTurnAction,
        PageSharedWorkerClientEventTurnOutcome, RendererPageSharedWorkerClientEventOwner,
        RendererPageSharedWorkerClientEventTask,
    },
    script_vm::{SharedWorkerClientEventBodyEffect, SharedWorkerErrorDispatchEffect},
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl From<SharedWorkerClientEventBodyEffect> for PageSharedWorkerClientEventTargetEffect {
    fn from(effect: SharedWorkerClientEventBodyEffect) -> Self {
        match effect {
            SharedWorkerClientEventBodyEffect::EndpointClosed => Self::EndpointClosedByCurrentOwner,
            SharedWorkerClientEventBodyEffect::Error {
                endpoint_disposition,
                dispatch_effect,
            } => match dispatch_effect {
                SharedWorkerErrorDispatchEffect::CallbackDispatched => {
                    Self::ErrorCallbackDispatchedToCurrentOwner {
                        endpoint_disposition,
                    }
                }
                SharedWorkerErrorDispatchEffect::CurrentTargetHadNoCallback => {
                    Self::CurrentOwnerErrorHadNoCallback {
                        endpoint_disposition,
                    }
                }
            },
        }
    }
}

impl IntoPageTaskCompletion for PageSharedWorkerClientEventTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageSharedWorkerClientEventTargetEffect::ErrorCallbackDispatchedToCurrentOwner {
                ..
            } => PageTaskCompletion::CallbackCompletion,
            PageSharedWorkerClientEventTargetEffect::EndpointClosedByCurrentOwner
            | PageSharedWorkerClientEventTargetEffect::CurrentOwnerErrorHadNoCallback { .. } => {
                PageTaskCompletion::CheckpointOnly
            }
            PageSharedWorkerClientEventTargetEffect::DiscardedStaleOwner { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

/// Proof that the Page arbiter matched a selected event against the exact
/// SharedWorker wrapper and its current root Document/Window realm.
pub(crate) struct AuthorizedCurrentPageSharedWorkerClientEvent(
    RendererPageSharedWorkerClientEventTask,
);

impl AuthorizedCurrentPageSharedWorkerClientEvent {
    fn new(task: RendererPageSharedWorkerClientEventTask) -> Self {
        Self(task)
    }

    pub(crate) fn into_task(self) -> RendererPageSharedWorkerClientEventTask {
        self.0
    }
}

impl PageVm {
    fn current_page_shared_worker_client_event_owner(
        &self,
        client_id: moli_shared_worker::SharedWorkerClientId,
    ) -> Option<RendererPageSharedWorkerClientEventOwner> {
        let execution_context = self
            .vm()
            .current_shared_worker_client_event_identity(client_id)?;
        Some(RendererPageSharedWorkerClientEventOwner::new(
            self.document_lifecycle.identity().document,
            execution_context,
            client_id,
        ))
    }

    pub(in crate::runtime) fn apply_selected_page_shared_worker_client_event_turn(
        &mut self,
        task: RendererPageSharedWorkerClientEventTask,
    ) -> anyhow::Result<PageSharedWorkerClientEventTurnOutcome> {
        let owner = task.owner();
        let event_kind = task.event_kind();
        let current_owner = self.current_page_shared_worker_client_event_owner(owner.client_id());
        let target_effect = if current_owner == Some(owner) {
            self.vm_mut()
                .apply_current_shared_worker_client_event_body(
                    AuthorizedCurrentPageSharedWorkerClientEvent::new(task),
                )?
                .into()
        } else {
            tracing::debug!(
                ?owner,
                ?current_owner,
                "ignored stale exact-owner SharedWorker client event"
            );
            PageSharedWorkerClientEventTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageSharedWorkerClientEventTurnAction {
            owner,
            event_kind,
            target_effect,
        };
        Ok(PageSharedWorkerClientEventTurnOutcome::new(action))
    }
}
