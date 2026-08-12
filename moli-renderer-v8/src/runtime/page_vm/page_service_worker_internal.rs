//! Browser-context ServiceWorker internal-task authorization and completion.
//!
//! This module intentionally excludes ServiceWorker-to-Window client
//! messages. It authorizes one exact root, delegates body-only execution, and
//! maps the resulting Promise/event/internal-action fact to the unique
//! selected-task completion.

use anyhow::Result;

use crate::{
    page_task_queue::{
        PageServiceWorkerInternalTargetEffect, PageServiceWorkerInternalTurnAction,
        PageServiceWorkerInternalTurnOutcome, RendererPageServiceWorkerInternalTask,
        ServiceWorkerInternalCallbackEffect,
    },
    runtime::PageOwnerTurnOutcome,
    script_vm::{ServiceWorkerInternalBodyCallbackEffect, ServiceWorkerInternalBodyEffect},
};

use super::{IntoPageTaskCompletion, PageTaskCompletion, PageVm};

impl From<ServiceWorkerInternalBodyEffect> for PageServiceWorkerInternalTargetEffect {
    fn from(effect: ServiceWorkerInternalBodyEffect) -> Self {
        match effect {
            ServiceWorkerInternalBodyEffect::PromiseSettled => Self::PromiseSettledAtCurrentRoot,
            ServiceWorkerInternalBodyEffect::EventDispatchPassCompleted { callback_effect } => {
                Self::EventDispatchPassCompletedAtCurrentRoot {
                    callback_effect: match callback_effect {
                        ServiceWorkerInternalBodyCallbackEffect::CallbackBodyDispatched => {
                            ServiceWorkerInternalCallbackEffect::CallbackBodyDispatched
                        }
                        ServiceWorkerInternalBodyCallbackEffect::NoCallbackBodyDispatched => {
                            ServiceWorkerInternalCallbackEffect::NoCallbackBodyDispatched
                        }
                    },
                }
            }
            ServiceWorkerInternalBodyEffect::InternalActionApplied => {
                Self::InternalActionAppliedAtCurrentRoot
            }
            ServiceWorkerInternalBodyEffect::ExactTargetUnavailable => {
                Self::CurrentRootTaskHadNoExactTarget
            }
        }
    }
}

impl IntoPageTaskCompletion for PageServiceWorkerInternalTurnAction {
    fn into_page_task_completion(self) -> PageTaskCompletion {
        match self.target_effect {
            PageServiceWorkerInternalTargetEffect::EventDispatchPassCompletedAtCurrentRoot {
                callback_effect: ServiceWorkerInternalCallbackEffect::CallbackBodyDispatched,
            } => PageTaskCompletion::CallbackCompletion,
            PageServiceWorkerInternalTargetEffect::PromiseSettledAtCurrentRoot
            | PageServiceWorkerInternalTargetEffect::EventDispatchPassCompletedAtCurrentRoot {
                callback_effect: ServiceWorkerInternalCallbackEffect::NoCallbackBodyDispatched,
            }
            | PageServiceWorkerInternalTargetEffect::InternalActionAppliedAtCurrentRoot => {
                PageTaskCompletion::CheckpointOnly
            }
            PageServiceWorkerInternalTargetEffect::CurrentRootTaskHadNoExactTarget
            | PageServiceWorkerInternalTargetEffect::DiscardedStaleRoot { .. } => {
                PageTaskCompletion::NoCompletion
            }
        }
    }
}

/// Proof that the Page arbiter matched the task's exact root Document before
/// any pending request or Window-client state was inspected.
pub(crate) struct AuthorizedCurrentPageServiceWorkerInternalTask {
    task: RendererPageServiceWorkerInternalTask,
}

impl AuthorizedCurrentPageServiceWorkerInternalTask {
    fn new(task: RendererPageServiceWorkerInternalTask) -> Self {
        Self { task }
    }

    pub(crate) fn into_task(self) -> RendererPageServiceWorkerInternalTask {
        self.task
    }
}

impl PageVm {
    /// Apply one browser-context ServiceWorker task selected from the Page's
    /// internal-default source.
    ///
    /// The Page envelope owns root-Document authorization; request ids,
    /// Window-client generations, and event targets remain ScriptVm
    /// authorities.
    pub(in crate::runtime) fn apply_selected_page_service_worker_internal_turn(
        &mut self,
        task: RendererPageServiceWorkerInternalTask,
    ) -> Result<PageServiceWorkerInternalTurnOutcome> {
        let root_document = task.root_document();
        let task_kind = task.kind();
        let current_root = self.document_lifecycle.identity().document;
        let target_effect = if root_document != current_root {
            PageServiceWorkerInternalTargetEffect::DiscardedStaleRoot { current_root }
        } else {
            self.vm_mut()
                .apply_current_service_worker_internal_body(
                    AuthorizedCurrentPageServiceWorkerInternalTask::new(task),
                )?
                .into()
        };
        let action = PageServiceWorkerInternalTurnAction {
            root_document,
            task_kind,
            target_effect,
        };
        Ok(PageOwnerTurnOutcome::new(action))
    }
}
