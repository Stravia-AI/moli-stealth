use std::{collections::HashMap, fmt};

use crate::runtime::{
    RendererBrowserContextRuntimeOwner, RendererBrowserContextRuntimeOwnerAccess,
};

use super::{BrowserContextHandle, BrowserContextId, BrowserContextRegistryError};

/// Browser Host rejected a physical BrowserContext runtime-root transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserContextRuntimeRegistryError {
    Context(BrowserContextRegistryError),
    DuplicateRuntimeRoot(BrowserContextId),
    MissingRuntimeRoot(BrowserContextId),
}

impl fmt::Display for BrowserContextRuntimeRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Context(error) => error.fmt(formatter),
            Self::DuplicateRuntimeRoot(browser_context_id) => write!(
                formatter,
                "BrowserContext {:?} already has an exact renderer runtime root",
                browser_context_id.as_str()
            ),
            Self::MissingRuntimeRoot(browser_context_id) => write!(
                formatter,
                "BrowserContext {:?} has no exact renderer runtime root",
                browser_context_id.as_str()
            ),
        }
    }
}

impl std::error::Error for BrowserContextRuntimeRegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Context(error) => Some(error),
            Self::DuplicateRuntimeRoot(_) | Self::MissingRuntimeRoot(_) => None,
        }
    }
}

impl From<BrowserContextRegistryError> for BrowserContextRuntimeRegistryError {
    fn from(error: BrowserContextRegistryError) -> Self {
        Self::Context(error)
    }
}

/// Unique renderer/network roots keyed by exact BrowserContext instance.
///
/// This registry deliberately owns no selection or Target topology. Its
/// transaction helpers compose with `BrowserNavigationOwner` synchronously so
/// the two registries either publish together or restore the exact root.
#[derive(Default)]
pub(super) struct BrowserContextRuntimeRegistry {
    roots: HashMap<BrowserContextHandle, RendererBrowserContextRuntimeOwner>,
}

impl BrowserContextRuntimeRegistry {
    pub(super) fn register<T>(
        &mut self,
        browser_context_handle: BrowserContextHandle,
        renderer_runtime_owner: RendererBrowserContextRuntimeOwner,
        commit: impl FnOnce(
            RendererBrowserContextRuntimeOwnerAccess,
        ) -> Result<T, BrowserContextRegistryError>,
    ) -> Result<T, BrowserContextRuntimeRegistryError> {
        if self.roots.contains_key(&browser_context_handle) {
            return Err(BrowserContextRuntimeRegistryError::DuplicateRuntimeRoot(
                BrowserContextId::new(browser_context_handle.browser_context_id()),
            ));
        }
        let renderer_runtime_access = renderer_runtime_owner.owner_access();
        let committed = commit(renderer_runtime_access)?;
        self.roots
            .insert(browser_context_handle, renderer_runtime_owner);
        Ok(committed)
    }

    pub(super) fn remove<T>(
        &mut self,
        browser_context_handle: BrowserContextHandle,
        commit: impl FnOnce() -> Result<T, BrowserContextRegistryError>,
    ) -> Result<(T, RendererBrowserContextRuntimeOwner), BrowserContextRuntimeRegistryError> {
        let Some(renderer_runtime_owner) = self.roots.remove(&browser_context_handle) else {
            return Err(BrowserContextRuntimeRegistryError::MissingRuntimeRoot(
                BrowserContextId::new(browser_context_handle.browser_context_id()),
            ));
        };
        match commit() {
            Ok(committed) => Ok((committed, renderer_runtime_owner)),
            Err(error) => {
                self.roots
                    .insert(browser_context_handle, renderer_runtime_owner);
                Err(error.into())
            }
        }
    }

    pub(super) fn owner_access(
        &self,
        browser_context_handle: &BrowserContextHandle,
    ) -> Option<RendererBrowserContextRuntimeOwnerAccess> {
        if !browser_context_handle.is_live() {
            return None;
        }
        self.roots
            .get(browser_context_handle)
            .map(RendererBrowserContextRuntimeOwner::owner_access)
    }

    pub(super) fn terminate_renderer_producers(&mut self) {
        for root in self.roots.values_mut() {
            root.terminate_renderer_producers_for_owner_shutdown();
        }
    }

    pub(super) fn shutdown_network_and_join(&mut self) {
        for root in self.roots.values_mut() {
            root.shutdown_network_and_join();
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.roots.len()
    }
}
