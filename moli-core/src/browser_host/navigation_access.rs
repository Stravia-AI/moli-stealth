use std::{sync::Arc, time::Duration};

use crate::{
    RendererOutputTransportSender,
    network::ResourceRequestClient,
    page::{
        CompletedPageCommand, Page, PendingPageCommand, RendererCommandTurnOutput,
        RendererDocumentLifecycleEvent, RendererPageLifetimeOwner, SameDocumentHistoryUpdate,
    },
    runtime::{NavigationEngine, NavigationResourceStorageHandles},
};

use super::{
    BrowserContextActivation, BrowserContextDisposalReservation, BrowserContextHandle,
    BrowserContextRegistryError, BrowserContextSelectionProjection, BrowserDocumentNavigation,
    BrowserExactHistoryTraversalResolutionError, BrowserFactEnvelope, BrowserFactPublishError,
    BrowserHistoryTraversalDestination, BrowserHistoryTraversalResolution,
    BrowserHistoryTraversalResolutionError, BrowserHostState, BrowserInitialEmptyDocumentSeed,
    BrowserNavigationFailure, BrowserNavigationHistoryEntry, BrowserNavigationHistoryPageSnapshot,
    BrowserNavigationHistorySeed, BrowserNavigationTraceContext, BrowserPageFetchConfiguration,
    BrowserPageOwnerKey, BrowserPageReplacement, BrowserPageReplacementCommitError,
    BrowserPageReplacementPermit, BrowserPageResidenceTransition,
    BrowserPageResidenceTransitionCommitError, BrowserPageResidenceTransitionPermit,
    BrowserPageRuntimeOwner, BrowserSameDocumentNavigationCommitError, BrowserTargetActivation,
    BrowserTargetCreationMetadata, BrowserTargetEngineAdoptionError,
    BrowserTargetEngineOwnerMismatch, BrowserTargetEngineResidence, BrowserTargetRegistration,
    BrowserTargetRegistryError, BrowserTargetTermination, BrowserTargetTerminationCommitError,
    BrowserTargetTerminationPermit, BrowserTargetTopologyProjection, PageResidenceIdentity,
};

/// Narrow mutation entry points for authoritative Browser navigation state.
///
/// Protocol callers can request one complete owner operation, but cannot hold
/// a mutable borrow of the underlying registry aggregate or combine arbitrary
/// mutations outside this boundary.
impl BrowserHostState {
    pub fn activate_browser_context<F>(
        &self,
        browser_context_id: &str,
        projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserContextActivation, BrowserContextRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.navigation_owner_mut().activate_browser_context(
            browser_context_id,
            projection,
            create_replacement,
        )
    }

    pub fn begin_browser_context_disposal(
        &self,
        browser_context_handle: &BrowserContextHandle,
    ) -> Result<BrowserContextDisposalReservation, BrowserContextRegistryError> {
        self.navigation_owner_mut()
            .begin_browser_context_disposal(browser_context_handle)
    }

    pub fn rollback_browser_context_disposal(
        &self,
        reservation: BrowserContextDisposalReservation,
    ) -> bool {
        self.navigation_owner_mut()
            .rollback_browser_context_disposal(reservation)
    }

    pub fn register_background_target_with_creation_metadata(
        &self,
        browser_context_id: &str,
        target_id: &str,
        creation_metadata: BrowserTargetCreationMetadata,
        topology_projection: BrowserTargetTopologyProjection,
    ) -> Result<BrowserTargetRegistration, BrowserTargetRegistryError> {
        self.navigation_owner_mut()
            .register_background_target_with_creation_metadata(
                browser_context_id,
                target_id,
                creation_metadata,
                topology_projection,
            )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn register_active_target_with_creation_metadata<F>(
        &self,
        browser_context_id: &str,
        target_id: &str,
        creation_metadata: BrowserTargetCreationMetadata,
        topology_projection: BrowserTargetTopologyProjection,
        selection_projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserTargetRegistration, BrowserTargetRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.navigation_owner_mut()
            .register_active_target_with_creation_metadata(
                browser_context_id,
                target_id,
                creation_metadata,
                topology_projection,
                selection_projection,
                create_replacement,
            )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn replace_active_target_with_creation_metadata<F>(
        &self,
        browser_context_id: &str,
        expected_target_id: &str,
        replacement_target_id: &str,
        creation_metadata: BrowserTargetCreationMetadata,
        topology_projection: BrowserTargetTopologyProjection,
        selection_projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserTargetRegistration, BrowserTargetRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.navigation_owner_mut()
            .replace_active_target_with_creation_metadata(
                browser_context_id,
                expected_target_id,
                replacement_target_id,
                creation_metadata,
                topology_projection,
                selection_projection,
                create_replacement,
            )
    }

    pub fn activate_target<F>(
        &self,
        browser_context_id: &str,
        target_id: &str,
        topology_projection: BrowserTargetTopologyProjection,
        selection_projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserTargetActivation, BrowserTargetRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.navigation_owner_mut().activate_target(
            browser_context_id,
            target_id,
            topology_projection,
            selection_projection,
            create_replacement,
        )
    }

    pub fn rollback_staged_background_target(
        &self,
        browser_context_id: &str,
        target_id: &str,
        topology_projection: BrowserTargetTopologyProjection,
    ) -> Result<Option<RendererPageLifetimeOwner>, BrowserTargetRegistryError> {
        self.navigation_owner_mut()
            .rollback_staged_background_target(browser_context_id, target_id, topology_projection)
    }

    pub fn commit_initial_document_page_materialization(
        &self,
        permit: BrowserPageResidenceTransitionPermit,
        renderer_page_owner: &mut Option<RendererPageLifetimeOwner>,
        page_runtime_owner: &mut Option<BrowserPageRuntimeOwner>,
    ) -> Result<BrowserPageResidenceTransition, BrowserPageResidenceTransitionCommitError> {
        self.navigation_owner_mut()
            .commit_initial_document_page_materialization(
                permit,
                renderer_page_owner,
                page_runtime_owner,
            )
    }

    pub fn commit_failed_navigation_page_discard(
        &self,
        permit: BrowserPageResidenceTransitionPermit,
    ) -> Result<BrowserPageResidenceTransition, BrowserPageResidenceTransitionCommitError> {
        self.navigation_owner_mut()
            .commit_failed_navigation_page_discard(permit)
    }

    pub fn commit_loaded_page_replacement(
        &self,
        permit: BrowserPageReplacementPermit,
        history_page: BrowserNavigationHistoryPageSnapshot,
        renderer_page_owner: &mut Option<RendererPageLifetimeOwner>,
        page_runtime_owner: &mut Option<BrowserPageRuntimeOwner>,
    ) -> Result<BrowserPageReplacement, BrowserPageReplacementCommitError> {
        self.navigation_owner_mut().commit_loaded_page_replacement(
            permit,
            history_page,
            renderer_page_owner,
            page_runtime_owner,
        )
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn commit_page_residence_transition_without_renderer_owner_for_testing(
        &self,
        permit: BrowserPageResidenceTransitionPermit,
    ) -> Result<BrowserPageResidenceTransition, BrowserPageResidenceTransitionCommitError> {
        self.navigation_owner_mut()
            .commit_page_residence_transition_without_renderer_owner_for_testing(permit)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn commit_loaded_page_replacement_without_renderer_owner_for_testing(
        &self,
        permit: BrowserPageReplacementPermit,
        history_page: BrowserNavigationHistoryPageSnapshot,
    ) -> Result<BrowserPageReplacement, BrowserPageReplacementCommitError> {
        self.navigation_owner_mut()
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(permit, history_page)
    }

    pub fn try_start_document_navigation_with_trace(
        &self,
        key: &BrowserPageOwnerKey,
        loader_id: String,
        trace: Option<BrowserNavigationTraceContext>,
    ) -> Option<BrowserDocumentNavigation> {
        self.navigation_owner_mut()
            .try_start_document_navigation_with_trace(key, loader_id, trace)
    }

    pub fn commit_document_navigation_if_matches(
        &self,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        self.navigation_owner_mut()
            .commit_document_navigation_if_matches(key, navigation)
    }

    pub fn fail_document_navigation_if_matches(
        &self,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
        failure: BrowserNavigationFailure,
    ) -> bool {
        self.navigation_owner_mut()
            .fail_document_navigation_if_matches(key, navigation, failure)
    }

    pub fn convert_document_navigation_to_download_if_matches(
        &self,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        self.navigation_owner_mut()
            .convert_document_navigation_to_download_if_matches(key, navigation)
    }

    pub fn record_document_lifecycle_facts(
        &self,
        expected_page: &PageResidenceIdentity,
        events: &[RendererDocumentLifecycleEvent],
    ) -> Result<Vec<Arc<BrowserFactEnvelope>>, BrowserFactPublishError> {
        self.navigation_owner_mut()
            .record_document_lifecycle_facts(expected_page, events)
    }

    pub fn register_target_initial_empty_document(
        &self,
        owner: &BrowserPageOwnerKey,
        seed: BrowserInitialEmptyDocumentSeed,
    ) -> Result<(), BrowserTargetRegistryError> {
        self.navigation_owner_mut()
            .register_target_initial_empty_document(owner, seed)
    }

    pub fn mark_target_initial_empty_document_exited(&self, owner: &BrowserPageOwnerKey) {
        self.navigation_owner_mut()
            .mark_target_initial_empty_document_exited(owner);
    }

    pub fn commit_target_termination(
        &self,
        permit: BrowserTargetTerminationPermit,
    ) -> Result<BrowserTargetTermination, BrowserTargetTerminationCommitError> {
        self.navigation_owner_mut()
            .commit_target_termination(permit)
    }

    pub fn configure_active_fetch(&self, configuration: BrowserPageFetchConfiguration) {
        self.navigation_owner_mut()
            .configure_active_fetch(configuration);
    }

    pub fn ensure_active_resource_runtime_ready(
        &self,
        storage: NavigationResourceStorageHandles,
    ) -> anyhow::Result<()> {
        self.navigation_owner_mut()
            .ensure_active_resource_runtime_ready(storage)
    }

    pub fn rebuild_active_resource_request_client(
        &self,
        storage: NavigationResourceStorageHandles,
    ) -> anyhow::Result<ResourceRequestClient> {
        self.navigation_owner_mut()
            .rebuild_active_resource_request_client(storage)
    }

    pub fn ensure_active_cookie_store(
        &self,
        storage: NavigationResourceStorageHandles,
    ) -> anyhow::Result<moli_cookie_jar::SharedBrowserCookieStore> {
        self.navigation_owner_mut()
            .ensure_active_cookie_store(storage)
    }

    pub fn reset_active_resource_runtime_without_loaded_page(&self) {
        self.navigation_owner_mut()
            .reset_active_resource_runtime_without_loaded_page();
    }

    pub fn start_active_page_child_frame_lifecycle_work(
        &self,
        storage: NavigationResourceStorageHandles,
        page: &Page,
        timeout: Duration,
    ) -> anyhow::Result<PendingPageCommand> {
        self.navigation_owner_mut()
            .start_active_page_child_frame_lifecycle_work(storage, page, timeout)
    }

    pub fn complete_active_page_child_frame_lifecycle_work(
        &self,
        page: &mut Page,
        completion: CompletedPageCommand,
    ) -> anyhow::Result<(bool, RendererCommandTurnOutput)> {
        self.navigation_owner_mut()
            .complete_active_page_child_frame_lifecycle_work(page, completion)
    }

    pub fn set_renderer_output_transport_sender(&self, sender: RendererOutputTransportSender) {
        self.navigation_owner_mut()
            .set_renderer_output_transport_sender(sender);
    }

    pub fn adopt_target_engine(
        &self,
        owner: BrowserPageOwnerKey,
        residence: BrowserTargetEngineResidence,
        engine: NavigationEngine,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        self.navigation_owner_mut()
            .adopt_target_engine(owner, residence, engine)
    }

    pub fn adopt_registered_target_engine(
        &self,
        owner: BrowserPageOwnerKey,
        engine: NavigationEngine,
    ) -> Result<BrowserTargetEngineResidence, BrowserTargetEngineAdoptionError> {
        self.navigation_owner_mut()
            .adopt_registered_target_engine(owner, engine)
    }

    pub fn adopt_selected_target_engine_or_unbound(
        &self,
        projected_owner: Option<BrowserPageOwnerKey>,
        engine: NavigationEngine,
    ) -> Result<Option<BrowserTargetEngineResidence>, BrowserTargetEngineAdoptionError> {
        self.navigation_owner_mut()
            .adopt_selected_target_engine_or_unbound(projected_owner, engine)
    }

    pub fn navigation_history_snapshot(
        &self,
        key: &BrowserPageOwnerKey,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
    ) -> (usize, Vec<BrowserNavigationHistoryEntry>) {
        self.navigation_owner_mut()
            .navigation_history_snapshot(key, fallback_page_seed)
    }

    pub fn resolve_navigation_history_traversal(
        &self,
        key: &BrowserPageOwnerKey,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
        destination: BrowserHistoryTraversalDestination,
    ) -> Result<BrowserHistoryTraversalResolution, BrowserHistoryTraversalResolutionError> {
        self.navigation_owner_mut()
            .resolve_navigation_history_traversal(key, fallback_page_seed, destination)
    }

    pub fn resolve_exact_navigation_history_traversal(
        &self,
        expected_page: &PageResidenceIdentity,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
        destination: BrowserHistoryTraversalDestination,
    ) -> Result<BrowserHistoryTraversalResolution, BrowserExactHistoryTraversalResolutionError>
    {
        self.navigation_owner_mut()
            .resolve_exact_navigation_history_traversal(
                expected_page,
                fallback_page_seed,
                destination,
            )
    }

    pub fn reset_navigation_history(
        &self,
        key: &BrowserPageOwnerKey,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
    ) -> bool {
        self.navigation_owner_mut()
            .reset_navigation_history(key, fallback_page_seed)
    }

    pub fn can_reset_navigation_history(
        &self,
        key: &BrowserPageOwnerKey,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
    ) -> bool {
        self.navigation_owner_mut()
            .can_reset_navigation_history(key, fallback_page_seed)
    }

    pub fn mark_next_navigation_history_replace_current(&self, key: &BrowserPageOwnerKey) {
        self.navigation_owner_mut()
            .mark_next_navigation_history_replace_current(key);
    }

    pub fn mark_next_navigation_history_replace_initial_empty_document(
        &self,
        key: &BrowserPageOwnerKey,
    ) {
        self.navigation_owner_mut()
            .mark_next_navigation_history_replace_initial_empty_document(key);
    }

    pub fn mark_next_navigation_history_traverse_to_entry(
        &self,
        key: &BrowserPageOwnerKey,
        entry_id: i32,
    ) {
        self.navigation_owner_mut()
            .mark_next_navigation_history_traverse_to_entry(key, entry_id);
    }

    pub fn clear_pending_navigation_history_update(&self, key: &BrowserPageOwnerKey) {
        self.navigation_owner_mut()
            .clear_pending_navigation_history_update(key);
    }

    pub fn record_loaded_page_navigation_history(
        &self,
        key: &BrowserPageOwnerKey,
        page: BrowserNavigationHistoryPageSnapshot,
    ) {
        self.navigation_owner_mut()
            .record_loaded_page_navigation_history(key, page);
    }

    pub fn update_current_document_title(
        &self,
        expected_page: &PageResidenceIdentity,
        title: String,
    ) -> Option<bool> {
        self.navigation_owner_mut()
            .update_current_document_title(expected_page, title)
    }

    pub fn commit_same_document_navigation_history(
        &self,
        expected_page: &PageResidenceIdentity,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
        url: String,
        title: String,
        update: SameDocumentHistoryUpdate,
    ) -> Result<(), BrowserSameDocumentNavigationCommitError> {
        self.navigation_owner_mut()
            .commit_same_document_navigation_history(
                expected_page,
                fallback_page_seed,
                url,
                title,
                update,
            )
    }
}
