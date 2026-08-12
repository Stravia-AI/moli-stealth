use anyhow::Result;

use crate::{
    frame_owner_model::FrameDocumentModuleDependencyFetchTask,
    page_task_queue::{
        PageChildModuleDependencyFetchStartTargetEffect,
        PageChildModuleDependencyFetchStartTurnAction,
        PageChildModuleDependencyFetchStartTurnOutcome,
        RendererPageChildModuleDependencyFetchStartOwner,
        RendererPageChildModuleDependencyFetchStartTask,
    },
    types::ChildModuleFetchNetworkAttribution,
};

use super::PageVm;

/// Proof that the Page arbiter matched the stable root namespace and complete
/// child/document/realm target before admitting a dependency fetch start.
pub(crate) struct AuthorizedCurrentChildModuleDependencyFetchStart {
    target: crate::frame_owner_model::ChildDocumentModuleFetchTarget,
    task: FrameDocumentModuleDependencyFetchTask,
    network_attribution: ChildModuleFetchNetworkAttribution,
}

impl AuthorizedCurrentChildModuleDependencyFetchStart {
    fn new(
        target: crate::frame_owner_model::ChildDocumentModuleFetchTarget,
        task: FrameDocumentModuleDependencyFetchTask,
        network_attribution: ChildModuleFetchNetworkAttribution,
    ) -> Self {
        Self {
            target,
            task,
            network_attribution,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        crate::frame_owner_model::ChildDocumentModuleFetchTarget,
        FrameDocumentModuleDependencyFetchTask,
        ChildModuleFetchNetworkAttribution,
    ) {
        (self.target, self.task, self.network_attribution)
    }
}

impl PageVm {
    pub(in crate::runtime) fn apply_selected_page_child_module_dependency_fetch_start_turn(
        &mut self,
        start: RendererPageChildModuleDependencyFetchStartTask,
    ) -> Result<PageChildModuleDependencyFetchStartTurnOutcome> {
        let owner = start.owner();
        let request_url = start.task().fetch_request().source_url().clone();
        let current_snapshot = self.vm().capture_current_child_module_fetch_producer(
            owner.target().child_handle(),
            request_url,
        );
        let root_document = self.document_lifecycle.identity().document;
        let current_owner = current_snapshot.as_ref().map(|(target, _)| {
            RendererPageChildModuleDependencyFetchStartOwner::new(root_document, *target)
        });
        let target_effect = if current_owner == Some(owner) {
            let (_, network_attribution) = current_snapshot
                .expect("matching exact owner must retain its atomic producer snapshot");
            let outcome = self
                .vm_mut()
                .apply_current_child_module_dependency_fetch_start(
                    AuthorizedCurrentChildModuleDependencyFetchStart::new(
                        owner.target(),
                        start.into_task(),
                        network_attribution,
                    ),
                );
            PageChildModuleDependencyFetchStartTargetEffect::AppliedToCurrentOwner { outcome }
        } else {
            tracing::debug!(
                ?owner,
                ?current_owner,
                "discarded stale exact-owner child module dependency fetch start"
            );
            PageChildModuleDependencyFetchStartTargetEffect::DiscardedStaleOwner { current_owner }
        };
        let action = PageChildModuleDependencyFetchStartTurnAction {
            owner,
            target_effect,
        };
        Ok(PageChildModuleDependencyFetchStartTurnOutcome::new(action))
    }

    #[cfg(test)]
    /// Apply one exact dependency-start body without submitting the selected
    /// Page task's completion. Complete task tests must use the shared exact
    /// selector and production dispatcher.
    pub(in crate::runtime) fn run_child_module_dependency_fetch_start_body_for_test(
        &mut self,
    ) -> Result<Option<PageChildModuleDependencyFetchStartTurnOutcome>> {
        let task_sources = self.page_task_executor_sources_for_test();
        let Some(task) = task_sources.take_child_module_dependency_fetch_start_for_executor_test()
        else {
            return Ok(None);
        };
        self.apply_selected_page_child_module_dependency_fetch_start_turn(task)
            .map(Some)
    }
}
