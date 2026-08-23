use crate::runtime::NavigationEngine;

mod context_creation;
mod context_registry;
mod document_build;
mod document_lifecycle_registry;
mod engine_registry;
mod facts;
mod history;
mod history_registry;
mod initial_document;
mod initial_document_registry;
mod navigation_trace;
mod page_registry;
mod page_replacement;
mod page_runtime;
mod page_transition;
mod request;
mod request_registry;
mod target_creation;
mod target_metadata;
mod target_registry;
mod target_runtime_registry;
mod target_snapshot;
mod target_termination;
mod target_transaction;
mod types;

use history_registry::BrowserNavigationHistoryRegistry;
use initial_document_registry::BrowserInitialEmptyDocumentRegistry;
use page_registry::BrowserPageResidenceRegistry;
use request_registry::BrowserDocumentNavigationRegistry;
use target_registry::BrowserTargetRegistry;
use target_runtime_registry::BrowserTargetRuntimeRegistry;

use super::fact_journal::BrowserFactJournal;
use context_registry::BrowserContextRegistry;
use document_lifecycle_registry::BrowserDocumentLifecycleRegistry;
use engine_registry::{BrowserContextEngineHandoff, BrowserTargetEngineRegistry};

pub use context_creation::BrowserContextRegistrationMetadata;
pub use context_registry::{
    BrowserContextActivation, BrowserContextDisposalReservation, BrowserContextRegistration,
    BrowserContextRegistryError, BrowserContextRemoval, BrowserContextRemovalPermit,
    BrowserContextSelectionProjection,
};
pub use engine_registry::{
    BrowserSelectedTargetEngineDisposition, BrowserTargetEngineAdoptionError,
    BrowserTargetEngineContextMismatch, BrowserTargetEngineHandoff,
    BrowserTargetEngineHandoffOutcome, BrowserTargetEngineOwnerMismatch,
    BrowserTargetEngineResidence,
};
pub use history::{
    BrowserHistoryTraversalDestination, BrowserHistoryTraversalResolution,
    BrowserHistoryTraversalResolutionError, BrowserNavigationHistory,
    BrowserNavigationHistoryEntry, BrowserNavigationHistoryPageSnapshot,
    BrowserNavigationHistorySeed, BrowserNavigationHistoryUpdate,
    BrowserSameDocumentHistoryUpdateError,
};
pub use history_registry::{
    BrowserExactHistoryTraversalResolutionError, BrowserSameDocumentNavigationCommitError,
};
pub use initial_document::{
    BrowserInitialEmptyDocumentCreator, BrowserInitialEmptyDocumentSeed,
    BrowserInitialEmptyDocumentSnapshot,
};
pub use navigation_trace::{
    BrowserInstanceId, BrowserNavigationTraceContext, BrowserNavigationTraceEvent,
    BrowserNavigationTraceSource,
};
pub use page_registry::BrowserPageResidenceRegistryError;
pub use page_replacement::{
    BrowserPageReplacement, BrowserPageReplacementCommitError, BrowserPageReplacementPermit,
};
pub use page_transition::{
    BrowserPageResidenceTransition, BrowserPageResidenceTransitionCommitError,
    BrowserPageResidenceTransitionKind, BrowserPageResidenceTransitionPermit,
};
pub use request::{
    BrowserDocumentNavigation, BrowserNavigationFailure, BrowserNavigationRequestId,
};
pub use target_creation::BrowserTargetCreationMetadata;
pub use target_metadata::BrowserTargetMetadataTransition;
pub use target_registry::{
    BrowserTargetRegistryError, BrowserTargetResidence, BrowserTargetSlotProjection,
    BrowserTargetTopologyProjection,
};
pub use target_snapshot::{
    BrowserContextTargetSnapshot, BrowserTargetStateSnapshot, BrowserTopLevelTargetSnapshot,
};
pub use target_termination::{
    BrowserTargetTermination, BrowserTargetTerminationCommitError, BrowserTargetTerminationKind,
    BrowserTargetTerminationPermit, BrowserTargetTerminationRequest,
};
pub use target_transaction::{BrowserTargetActivation, BrowserTargetRegistration};
pub use types::{BrowserPageFetchConfiguration, BrowserPageOwnerKey};

/// Single strong owner of active and parked browser navigation runtimes.
///
/// Frontends submit protocol-neutral Target/context handoffs; the focused
/// engine registry owns selected/retained identity and replacement. Page
/// runtime, resource, history, termination, and document-build operations live
/// in sibling modules and do not expose the underlying engine to protocol
/// callers.
pub struct BrowserNavigationOwner {
    browser_instance_id: BrowserInstanceId,
    browser_facts: BrowserFactJournal,
    browser_contexts: BrowserContextRegistry,
    targets: BrowserTargetRegistry,
    target_runtimes: BrowserTargetRuntimeRegistry,
    target_engines: BrowserTargetEngineRegistry,
    page_residences: BrowserPageResidenceRegistry,
    document_lifecycles: BrowserDocumentLifecycleRegistry,
    initial_empty_documents: BrowserInitialEmptyDocumentRegistry,
    navigation_histories: BrowserNavigationHistoryRegistry,
    document_navigations: BrowserDocumentNavigationRegistry,
    target_terminations: target_termination::BrowserTargetTerminationRegistry,
}

impl BrowserNavigationOwner {
    pub fn new(active_engine: NavigationEngine) -> Self {
        let browser_instance_id = BrowserInstanceId::allocate();
        Self {
            browser_instance_id,
            browser_facts: BrowserFactJournal::new(browser_instance_id),
            browser_contexts: BrowserContextRegistry::default(),
            targets: BrowserTargetRegistry::default(),
            target_runtimes: BrowserTargetRuntimeRegistry::default(),
            target_engines: BrowserTargetEngineRegistry::new(active_engine),
            page_residences: BrowserPageResidenceRegistry,
            document_lifecycles: BrowserDocumentLifecycleRegistry,
            initial_empty_documents: BrowserInitialEmptyDocumentRegistry,
            navigation_histories: BrowserNavigationHistoryRegistry,
            document_navigations: BrowserDocumentNavigationRegistry,
            target_terminations: target_termination::BrowserTargetTerminationRegistry,
        }
    }

    pub fn browser_instance_id(&self) -> BrowserInstanceId {
        self.browser_instance_id
    }

    /// Discards Page-runtime ownership for a Target that remains alive.
    ///
    /// A failed navigation can retire the current Page without terminating the
    /// Target. The exact Page-slot capability and joint session history
    /// therefore deliberately survive this operation; the shared capability
    /// already exposes the physical slot's successor generation.
    #[cfg(test)]
    pub(super) fn discard_target_page_runtime(&mut self, target_id: &str) {
        let selected_engine_owner = self.selected_target_engine_owner().cloned();
        self.target_engines.discard_target_page_runtime(
            &mut self.target_runtimes,
            selected_engine_owner.as_ref(),
            target_id,
        );
        self.document_navigations
            .forget_target(&mut self.target_runtimes, target_id);
        self.initial_empty_documents
            .mark_exited_for_target(&mut self.target_runtimes, target_id);
    }

    /// Retires every navigation-owner state entry for internal fixture or
    /// staging cleanup. Live Target closure must use the typed termination
    /// transaction so topology and Page lifetime commit together.
    #[cfg(test)]
    pub(super) fn forget_target(&mut self, target_id: &str) {
        if let Some(browser_context_id) =
            self.target_browser_context_id(target_id).map(str::to_owned)
        {
            let owner = BrowserPageOwnerKey::new(browser_context_id, target_id);
            let _ = self.targets.remove_target(&owner);
            let _ = self.forget_target_runtime_state(&owner);
        }
    }

    pub(super) fn forget_target_runtime_state(
        &mut self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<crate::page::RendererPageLifetimeOwner> {
        self.target_runtimes
            .remove(owner)
            .and_then(|runtime| runtime.page_residence)
            .and_then(page_registry::BrowserPageResidenceRecord::into_renderer_page_owner)
    }
}
