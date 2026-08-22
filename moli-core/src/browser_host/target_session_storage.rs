use std::sync::Arc;

use crate::network::{SharedWebStorageStore, new_shared_web_storage_store};

use super::BrowserTargetHandle;

/// Creation-time payload for one top-level Target's session-storage namespace.
///
/// The seed is move-owned by the Target registration transaction. Cloning it
/// only clones the shared candidate store; the exact Target capability is not
/// created until Browser Core accepts the registration.
#[derive(Clone)]
pub(crate) struct BrowserTargetSessionStorageSeed {
    store: SharedWebStorageStore,
}

impl Default for BrowserTargetSessionStorageSeed {
    fn default() -> Self {
        Self {
            store: new_shared_web_storage_store(),
        }
    }
}

impl BrowserTargetSessionStorageSeed {
    pub(crate) fn from_store(store: SharedWebStorageStore) -> Self {
        Self { store }
    }

    pub(super) fn store(&self) -> SharedWebStorageStore {
        self.store.clone()
    }
}

impl std::fmt::Debug for BrowserTargetSessionStorageSeed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserTargetSessionStorageSeed")
            .finish_non_exhaustive()
    }
}

impl PartialEq for BrowserTargetSessionStorageSeed {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.store, &other.store)
    }
}

impl Eq for BrowserTargetSessionStorageSeed {}

/// Non-authoritative access to the session-storage namespace of one exact
/// live top-level Target.
///
/// The Target registry owns the namespace association. This access carries the
/// exact Target instance capability, so a same-public-id replacement cannot
/// authorize the predecessor's namespace. Store clones already captured by an
/// in-flight renderer operation may finish after retirement, but they cannot
/// become the namespace of a new Target instance.
#[derive(Clone)]
pub struct BrowserTargetSessionStorageAccess {
    target: BrowserTargetHandle,
    store: SharedWebStorageStore,
}

impl BrowserTargetSessionStorageAccess {
    pub(super) fn new(target: BrowserTargetHandle, store: SharedWebStorageStore) -> Self {
        Self { target, store }
    }

    pub fn target_handle(&self) -> &BrowserTargetHandle {
        &self.target
    }

    pub fn is_live(&self) -> bool {
        self.target.is_live()
    }

    /// Returns the shared store for an already-authorized exact Target route.
    ///
    /// This method does not itself authorize a command. Callers must first
    /// resolve the exact live Target handle; `is_live` is exposed for access
    /// projection and stale-completion checks.
    pub fn store(&self) -> &SharedWebStorageStore {
        &self.store
    }
}

impl std::fmt::Debug for BrowserTargetSessionStorageAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserTargetSessionStorageAccess")
            .field("target", &self.target)
            .field("live", &self.is_live())
            .finish_non_exhaustive()
    }
}

impl PartialEq for BrowserTargetSessionStorageAccess {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target && Arc::ptr_eq(&self.store, &other.store)
    }
}

impl Eq for BrowserTargetSessionStorageAccess {}
