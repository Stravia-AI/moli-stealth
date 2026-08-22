use std::sync::OnceLock;

use moli_core::browser_host::BrowserTargetSessionStorageAccess;
#[cfg(test)]
use moli_core::network::deep_clone_shared_web_storage_store;
use moli_core::network::{SharedWebStorageStore, new_shared_web_storage_store};

#[derive(Clone, Default)]
pub(crate) struct TargetSessionStorageNamespace {
    candidate_store: OnceLock<SharedWebStorageStore>,
    browser_access: Option<BrowserTargetSessionStorageAccess>,
}

impl std::fmt::Debug for TargetSessionStorageNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetSessionStorageNamespace")
            .field(
                "candidate_initialized",
                &self.candidate_store.get().is_some(),
            )
            .field(
                "browser_owned",
                &self
                    .browser_access
                    .as_ref()
                    .is_some_and(|access| access.is_live()),
            )
            .finish()
    }
}

impl TargetSessionStorageNamespace {
    #[cfg(test)]
    pub(crate) fn from_store(store: SharedWebStorageStore) -> Self {
        Self {
            candidate_store: OnceLock::from(store),
            browser_access: None,
        }
    }

    pub(crate) fn store(&self) -> &SharedWebStorageStore {
        if let Some(access) = self.browser_access.as_ref() {
            return access.store();
        }
        self.candidate_store
            .get_or_init(new_shared_web_storage_store)
    }

    #[cfg(test)]
    pub(crate) fn deep_clone(&self) -> Self {
        Self::from_store(deep_clone_shared_web_storage_store(self.store()))
    }

    pub(crate) fn bind_browser_access(&mut self, access: BrowserTargetSessionStorageAccess) {
        let _ = self.candidate_store.take();
        self.browser_access = Some(access);
    }
}
