use crate::{
    browser_host::{BrowserTargetId, BrowserTargetSessionStorageSeed},
    network::SharedWebStorageStore,
};

use super::BrowserTargetCreationMetadata;

/// Protocol-neutral immutable metadata installed by one BrowserContext
/// registration transaction.
///
/// The active Target is identified by the exact staged topology supplied to
/// that transaction. Keeping its creation metadata separate from the physical
/// topology prevents frontend Page payloads from becoming browser authority.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowserContextRegistrationMetadata {
    active_target_creation: Option<BrowserTargetCreationMetadata>,
    target_session_storage: Vec<(BrowserTargetId, BrowserTargetSessionStorageSeed)>,
}

impl BrowserContextRegistrationMetadata {
    pub fn with_active_target_creation(creation_metadata: BrowserTargetCreationMetadata) -> Self {
        Self {
            active_target_creation: Some(creation_metadata),
            target_session_storage: Vec::new(),
        }
    }

    pub(super) fn active_target_creation(&self) -> Option<&BrowserTargetCreationMetadata> {
        self.active_target_creation.as_ref()
    }

    pub fn with_target_session_storage_store(
        mut self,
        target_id: impl Into<String>,
        store: SharedWebStorageStore,
    ) -> Self {
        let target_id = BrowserTargetId::new(target_id);
        let seed = BrowserTargetSessionStorageSeed::from_store(store);
        if let Some((_, current)) = self
            .target_session_storage
            .iter_mut()
            .find(|(candidate, _)| candidate == &target_id)
        {
            *current = seed;
        } else {
            self.target_session_storage.push((target_id, seed));
        }
        self
    }

    pub(super) fn target_session_storage_store(
        &self,
        target_id: &str,
    ) -> Option<SharedWebStorageStore> {
        self.target_session_storage
            .iter()
            .find(|(candidate, _)| candidate.as_str() == target_id)
            .map(|(_, seed)| seed.store())
    }

    pub(super) fn target_session_storage_target_ids(&self) -> impl Iterator<Item = &str> {
        self.target_session_storage
            .iter()
            .map(|(target_id, _)| target_id.as_str())
    }
}
