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

/// Read-only Browser Core snapshot of one Target's initial empty Document.
///
/// The record deliberately survives `exited`: history and diagnostics still
/// need the Target-creation metadata after a successor Document commits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserInitialEmptyDocumentSnapshot {
    target_id: BrowserTargetId,
    loader_id: String,
    seed: BrowserInitialEmptyDocumentSeed,
    materialized: bool,
    exited: bool,
    pending_cross_document_navigation: bool,
}

impl BrowserInitialEmptyDocumentSnapshot {
    pub(super) fn new(target_id: impl Into<String>, seed: BrowserInitialEmptyDocumentSeed) -> Self {
        let target_id = BrowserTargetId::new(target_id);
        let loader_id = format!("LID-INITIAL-{}", target_id.as_str());
        Self {
            target_id,
            loader_id,
            seed,
            materialized: false,
            exited: false,
            pending_cross_document_navigation: false,
        }
    }

    pub fn target_id(&self) -> &str {
        self.target_id.as_str()
    }

    pub fn initial_url(&self) -> &str {
        self.seed.initial_url()
    }

    pub fn loader_id(&self) -> &str {
        &self.loader_id
    }

    pub fn creator(&self) -> Option<&BrowserInitialEmptyDocumentCreator> {
        self.seed.creator()
    }

    pub fn storage_key(&self) -> Option<&MoliStorageKey> {
        self.seed.storage_key()
    }

    pub fn materialized(&self) -> bool {
        self.materialized
    }

    pub fn exited(&self) -> bool {
        self.exited
    }

    pub fn pending_cross_document_navigation(&self) -> bool {
        self.pending_cross_document_navigation
    }

    pub fn is_on_initial_empty_document(&self) -> bool {
        !self.exited
    }

    pub(super) fn history_seed(&self) -> BrowserNavigationHistorySeed {
        BrowserNavigationHistorySeed::initial_empty_document(self.initial_url())
    }

    pub(super) fn mark_materialized(&mut self) {
        if !self.exited {
            self.materialized = true;
        }
    }

    pub(super) fn rollback_materialized(&mut self) -> bool {
        if self.exited || !self.materialized {
            return false;
        }
        self.materialized = false;
        true
    }

    pub(super) fn mark_pending_cross_document_navigation(&mut self) {
        if !self.exited {
            self.pending_cross_document_navigation = true;
        }
    }

    pub(super) fn clear_pending_cross_document_navigation(&mut self) {
        self.pending_cross_document_navigation = false;
    }

    pub(super) fn mark_exited(&mut self) {
        self.exited = true;
        self.pending_cross_document_navigation = false;
    }
}
