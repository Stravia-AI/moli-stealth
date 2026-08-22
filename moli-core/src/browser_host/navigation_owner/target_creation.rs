use crate::{browser_host::BrowserTargetSessionStorageSeed, network::SharedWebStorageStore};

use super::{BrowserInitialEmptyDocumentSeed, BrowserNavigationOwner, BrowserPageOwnerKey};

/// Protocol-neutral immutable inputs captured by one Browser Target creation
/// transaction.
///
/// Keeping this separate from Target topology makes future opener/sandbox
/// metadata additions explicit without growing the registry transaction into
/// a frontend payload container.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowserTargetCreationMetadata {
    initial_empty_document: Option<BrowserInitialEmptyDocumentSeed>,
    session_storage: BrowserTargetSessionStorageSeed,
}

impl BrowserTargetCreationMetadata {
    pub fn with_initial_empty_document(seed: BrowserInitialEmptyDocumentSeed) -> Self {
        Self {
            initial_empty_document: Some(seed),
            ..Self::default()
        }
    }

    pub fn with_session_storage_store(mut self, store: SharedWebStorageStore) -> Self {
        self.session_storage = BrowserTargetSessionStorageSeed::from_store(store);
        self
    }

    pub(super) fn initial_empty_document(&self) -> Option<&BrowserInitialEmptyDocumentSeed> {
        self.initial_empty_document.as_ref()
    }

    pub(super) fn session_storage_store(&self) -> SharedWebStorageStore {
        self.session_storage.store()
    }
}

impl BrowserNavigationOwner {
    /// Installs infallible creation metadata after every fallible participant
    /// has accepted a Target-bearing registration transaction and before its
    /// lifecycle handle is published live.
    pub(super) fn install_target_creation_metadata(
        &mut self,
        owner: &BrowserPageOwnerKey,
        creation_metadata: &BrowserTargetCreationMetadata,
    ) {
        if let Some(seed) = creation_metadata.initial_empty_document().cloned() {
            self.initial_empty_documents.begin(owner, seed);
        }
    }
}
