use std::collections::HashMap;

use super::{
    BrowserPageOwnerKey, document_lifecycle_registry::BrowserDocumentLifecycleRecord,
    engine_registry::BrowserPageOwner, history_registry::BrowserTargetNavigationHistory,
    initial_document::BrowserInitialEmptyDocumentRecord, page_registry::BrowserPageResidenceRecord,
    request_registry::BrowserTargetDocumentNavigationState,
    target_termination::BrowserTargetTerminationState,
};

/// All runtime state owned by one exact Browser Target.
///
/// Module-specific registries remain the transaction boundaries, but their
/// storage is aggregated here so Target retirement has one exact record to
/// remove instead of a collection of independently keyed maps.
#[derive(Default)]
pub(super) struct BrowserTargetRuntimeRecord {
    pub(super) engine: Option<BrowserPageOwner>,
    pub(super) page_residence: Option<BrowserPageResidenceRecord>,
    pub(super) document_lifecycle: Option<BrowserDocumentLifecycleRecord>,
    pub(super) initial_empty_document: Option<BrowserInitialEmptyDocumentRecord>,
    pub(super) navigation_history: Option<BrowserTargetNavigationHistory>,
    pub(super) document_navigation: Option<BrowserTargetDocumentNavigationState>,
    pub(super) termination: Option<BrowserTargetTerminationState>,
}

impl BrowserTargetRuntimeRecord {
    fn is_empty(&self) -> bool {
        self.engine.is_none()
            && self.page_residence.is_none()
            && self.document_lifecycle.is_none()
            && self.initial_empty_document.is_none()
            && self.navigation_history.is_none()
            && self.document_navigation.is_none()
            && self.termination.is_none()
    }
}

#[derive(Default)]
pub(super) struct BrowserTargetRuntimeRegistry {
    pub(super) entries: HashMap<BrowserPageOwnerKey, BrowserTargetRuntimeRecord>,
}

impl BrowserTargetRuntimeRegistry {
    pub(super) fn remove(
        &mut self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<BrowserTargetRuntimeRecord> {
        self.entries.remove(owner)
    }

    pub(super) fn prune_empty(&mut self) {
        self.entries.retain(|_, record| !record.is_empty());
    }
}
