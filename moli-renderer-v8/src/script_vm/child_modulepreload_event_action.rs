use crate::document_runtime::DomHandle;
use crate::frame_owner_model::{
    FrameDocumentModulepreloadEventAction, FrameDocumentModulepreloadEventActionHooks,
    FrameDocumentModulepreloadEventActionRunner, FrameDocumentModulepreloadTerminalOutcome,
    FrameDocumentOwner, FrameDocumentTaskOwner, FrameRealmId,
};

use super::ScriptVm;

impl ScriptVm {
    pub(crate) fn current_child_modulepreload_event_action_owner(
        &self,
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
    ) -> Option<FrameDocumentTaskOwner> {
        self._context_host
            .borrow()
            .current_child_module_route_task_owner(owner, realm_id)
    }

    pub(crate) fn current_child_modulepreload_event_action_is_runnable(
        &self,
        owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
    ) -> bool {
        self._context_host
            .borrow()
            .current_child_modulepreload_event_action_is_runnable(owner, realm_id)
    }

    pub(crate) fn apply_current_child_modulepreload_event_action(
        &mut self,
        authorization: crate::runtime::AuthorizedCurrentChildModulepreloadEventAction,
    ) -> FrameDocumentModulepreloadTerminalOutcome {
        FrameDocumentModulepreloadEventActionRunner::new(ScriptVmChildModulepreloadEventHooks {
            vm: self,
        })
        .run_event_action(authorization.into_action())
    }

    #[cfg(test)]
    /// Executes only the typed modulepreload event body.
    ///
    /// Low-level ScriptVm tests use this to inspect realm admission and domain
    /// state. Page-level checkpoint and callback follow-up assertions must run
    /// through the selected Page task dispatcher instead.
    pub(crate) fn run_child_modulepreload_event_action_body_for_test(&mut self) -> bool {
        let source = self
            ._page_task_residence_for_executor_test
            .as_ref()
            .expect("child modulepreload executor fixture must retain its production Page source")
            .task_sources();
        let Some(task) = source.take_child_modulepreload_event_action_for_executor_test(|owner| {
            self.current_child_modulepreload_event_action_owner(
                owner.document_owner().document_owner(),
                owner.realm_id(),
            ) != Some(owner.document_owner())
                || self.current_child_modulepreload_event_action_is_runnable(
                    owner.document_owner(),
                    owner.realm_id(),
                )
        }) else {
            return false;
        };
        let owner = task.owner();
        let action = task.into_action();
        if self.current_child_modulepreload_event_action_owner(
            owner.document_owner().document_owner(),
            owner.realm_id(),
        ) == Some(owner.document_owner())
        {
            let _ = self.apply_current_child_modulepreload_event_action(
                crate::runtime::AuthorizedCurrentChildModulepreloadEventAction::new_for_executor_test(
                    action,
                ),
            );
        }
        true
    }
}

struct ScriptVmChildModulepreloadEventHooks<'vm> {
    vm: &'vm mut ScriptVm,
}

impl FrameDocumentModulepreloadEventActionHooks for ScriptVmChildModulepreloadEventHooks<'_> {
    fn dispatch_modulepreload_event(
        &mut self,
        owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
        link_handle: DomHandle,
        successful: bool,
    ) -> Result<(), String> {
        super::child_document_event::ChildDocumentEventOwner::new(self.vm)
            .dispatch_modulepreload_link_handle_event(owner, realm_id, link_handle, successful)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    fn record_modulepreload_event_dispatch_failed(
        &mut self,
        action: &FrameDocumentModulepreloadEventAction,
        error: &str,
    ) {
        let url = action.key().map(|key| key.url());
        tracing::warn!(
            owner = ?action.owner(),
            realm_id = ?action.realm_id(),
            ?url,
            link_handle = ?action.link_handle(),
            successful = action.successful(),
            error,
            "child modulepreload event action dispatch failed"
        );
    }
}
