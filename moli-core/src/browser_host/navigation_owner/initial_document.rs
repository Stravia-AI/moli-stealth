use moli_storage_key::MoliStorageKey;

use crate::browser_host::BrowserTargetId;

use super::BrowserNavigationHistorySeed;

/// Immutable creator security context captured when a top-level auxiliary
/// browsing context is created.
///
/// This is browser metadata. Renderer/Page projections may clone it while
/// constructing the initial Document, but they do not own its lifetime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserInitialEmptyDocumentCreator {
    target_id: BrowserTargetId,
    security_origin: String,
    secure_context_type: String,
}

impl BrowserInitialEmptyDocumentCreator {
    pub fn new(
        target_id: impl Into<String>,
        security_origin: impl Into<String>,
        secure_context_type: impl Into<String>,
    ) -> Self {
        Self {
            target_id: BrowserTargetId::new(target_id),
            security_origin: security_origin.into(),
            secure_context_type: secure_context_type.into(),
        }
    }

    pub fn target_id(&self) -> &str {
        self.target_id.as_str()
    }

    pub fn security_origin(&self) -> &str {
        &self.security_origin
    }

    pub fn secure_context_type(&self) -> &str {
        &self.secure_context_type
    }
}

/// Immutable metadata used to create one Target's initial empty Document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserInitialEmptyDocumentSeed {
    initial_url: String,
    creator: Option<BrowserInitialEmptyDocumentCreator>,
    storage_key: Option<MoliStorageKey>,
}

impl BrowserInitialEmptyDocumentSeed {
    pub fn new(initial_url: impl Into<String>) -> Self {
        Self {
            initial_url: initial_url.into(),
            creator: None,
            storage_key: None,
        }
    }

    pub fn with_creator(mut self, creator: BrowserInitialEmptyDocumentCreator) -> Self {
        self.creator = Some(creator);
        self
    }

    pub fn with_storage_key(mut self, storage_key: MoliStorageKey) -> Self {
        self.storage_key = Some(storage_key);
        self
    }

    pub fn initial_url(&self) -> &str {
        &self.initial_url
    }

    pub fn creator(&self) -> Option<&BrowserInitialEmptyDocumentCreator> {
        self.creator.as_ref()
    }

    pub fn storage_key(&self) -> Option<&MoliStorageKey> {
        self.storage_key.as_ref()
    }

    pub(super) fn is_initial_empty_document(&self) -> bool {
        url::Url::parse(&self.initial_url)
            .ok()
            .is_some_and(|url| url.scheme() == "about" && url.path() == "blank")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserInitialEmptyDocumentLifecycle {
    Unmaterialized,
    Materialized,
    ExitedUnmaterialized,
    ExitedMaterialized,
}

impl BrowserInitialEmptyDocumentLifecycle {
    fn materialized(self) -> bool {
        matches!(self, Self::Materialized | Self::ExitedMaterialized)
    }

    fn exited(self) -> bool {
        matches!(self, Self::ExitedUnmaterialized | Self::ExitedMaterialized)
    }
}

/// Minimal state retained for one Target's initial empty Document.
///
/// Target identity remains in the registry key, loader identity is
/// deterministic, and navigation pending state lives in the authoritative
/// document-navigation registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BrowserInitialEmptyDocumentRecord {
    seed: BrowserInitialEmptyDocumentSeed,
    lifecycle: BrowserInitialEmptyDocumentLifecycle,
}

impl BrowserInitialEmptyDocumentRecord {
    pub(super) fn new(seed: BrowserInitialEmptyDocumentSeed) -> Self {
        Self {
            seed,
            lifecycle: BrowserInitialEmptyDocumentLifecycle::Unmaterialized,
        }
    }

    pub(super) fn snapshot(
        &self,
        target_id: impl Into<String>,
        has_pending_navigation: bool,
    ) -> BrowserInitialEmptyDocumentSnapshot {
        BrowserInitialEmptyDocumentSnapshot {
            target_id: BrowserTargetId::new(target_id),
            seed: self.seed.clone(),
            lifecycle: self.lifecycle,
            pending_cross_document_navigation: self.is_on_initial_empty_document()
                && has_pending_navigation,
        }
    }

    pub(super) fn initial_url(&self) -> &str {
        self.seed.initial_url()
    }

    pub(super) fn is_on_initial_empty_document(&self) -> bool {
        !self.lifecycle.exited()
    }

    pub(super) fn materialized(&self) -> bool {
        self.lifecycle.materialized()
    }

    pub(super) fn history_seed(&self) -> BrowserNavigationHistorySeed {
        BrowserNavigationHistorySeed::initial_empty_document(self.initial_url())
    }

    pub(super) fn mark_materialized(&mut self) {
        if self.lifecycle == BrowserInitialEmptyDocumentLifecycle::Unmaterialized {
            self.lifecycle = BrowserInitialEmptyDocumentLifecycle::Materialized;
        }
    }

    pub(super) fn rollback_materialized(&mut self) -> bool {
        if self.lifecycle != BrowserInitialEmptyDocumentLifecycle::Materialized {
            return false;
        }
        self.lifecycle = BrowserInitialEmptyDocumentLifecycle::Unmaterialized;
        true
    }

    pub(super) fn mark_exited(&mut self) {
        self.lifecycle = match self.lifecycle {
            BrowserInitialEmptyDocumentLifecycle::Unmaterialized
            | BrowserInitialEmptyDocumentLifecycle::ExitedUnmaterialized => {
                BrowserInitialEmptyDocumentLifecycle::ExitedUnmaterialized
            }
            BrowserInitialEmptyDocumentLifecycle::Materialized
            | BrowserInitialEmptyDocumentLifecycle::ExitedMaterialized => {
                BrowserInitialEmptyDocumentLifecycle::ExitedMaterialized
            }
        };
    }
}

/// Read-only projection of one Target's initial empty Document.
///
/// This value is assembled on demand from the registry key, the minimal
/// lifecycle record, and authoritative navigation state. It is never retained
/// in the per-Target registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserInitialEmptyDocumentSnapshot {
    target_id: BrowserTargetId,
    seed: BrowserInitialEmptyDocumentSeed,
    lifecycle: BrowserInitialEmptyDocumentLifecycle,
    pending_cross_document_navigation: bool,
}

impl BrowserInitialEmptyDocumentSnapshot {
    pub fn target_id(&self) -> &str {
        self.target_id.as_str()
    }

    pub fn initial_url(&self) -> &str {
        self.seed.initial_url()
    }

    pub fn loader_id(&self) -> String {
        format!("LID-INITIAL-{}", self.target_id.as_str())
    }

    pub fn creator(&self) -> Option<&BrowserInitialEmptyDocumentCreator> {
        self.seed.creator()
    }

    pub fn storage_key(&self) -> Option<&MoliStorageKey> {
        self.seed.storage_key()
    }

    pub fn materialized(&self) -> bool {
        self.lifecycle.materialized()
    }

    pub fn exited(&self) -> bool {
        self.lifecycle.exited()
    }

    pub fn pending_cross_document_navigation(&self) -> bool {
        self.pending_cross_document_navigation
    }

    pub fn is_on_initial_empty_document(&self) -> bool {
        !self.exited()
    }
}
