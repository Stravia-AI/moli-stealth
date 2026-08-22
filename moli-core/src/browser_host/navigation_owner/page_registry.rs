use std::collections::HashMap;

use crate::browser_host::page_residence::BrowserPageResidenceAdvanceError;
use crate::browser_host::{
    BrowserContextId, BrowserPageResidenceHandle, BrowserPageRuntimeAccess,
    BrowserPageRuntimeOwner, PageResidenceIdentity,
};
use crate::page::RendererPageLifetimeOwner;

use super::{BrowserNavigationOwner, BrowserPageOwnerKey, BrowserTargetTopologyProjection};

/// Exact Page-slot registration or physical projection mismatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserPageResidenceRegistryError {
    DuplicateTarget(BrowserPageOwnerKey),
    UnknownTarget(BrowserPageOwnerKey),
    TargetNotLive(BrowserPageOwnerKey),
    ProjectionMismatch(BrowserPageOwnerKey),
    DuplicateProjectedHandle {
        first: BrowserPageOwnerKey,
        duplicate: BrowserPageOwnerKey,
    },
    ContextProjectionMismatch(BrowserContextId),
    StaleTransition {
        owner: BrowserPageOwnerKey,
        expected_generation: u64,
        current_generation: u64,
    },
    GenerationExhausted(BrowserPageOwnerKey),
    RuntimeOwnerMismatch(BrowserPageOwnerKey),
}

impl std::fmt::Display for BrowserPageResidenceRegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateTarget(owner) => write!(
                formatter,
                "Page residence for Target {:?} in BrowserContext {:?} is already registered",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::UnknownTarget(owner) => write!(
                formatter,
                "Target {:?} in BrowserContext {:?} has no Page residence",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::TargetNotLive(owner) => write!(
                formatter,
                "Target {:?} in BrowserContext {:?} has only a staged Page residence",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::ProjectionMismatch(owner) => write!(
                formatter,
                "physical Page residence for Target {:?} in BrowserContext {:?} is not the Browser Core instance",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::DuplicateProjectedHandle { first, duplicate } => write!(
                formatter,
                "Targets {:?} and {:?} project the same physical Page residence",
                first.target_id(),
                duplicate.target_id()
            ),
            Self::ContextProjectionMismatch(browser_context_id) => write!(
                formatter,
                "BrowserContext {:?} Page residence count does not match its Target projection",
                browser_context_id.as_str()
            ),
            Self::StaleTransition {
                owner,
                expected_generation,
                current_generation,
            } => write!(
                formatter,
                "Page residence for Target {:?} in BrowserContext {:?} advanced from generation {expected_generation} to {current_generation} before commit",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::GenerationExhausted(owner) => write!(
                formatter,
                "Page residence generation for Target {:?} in BrowserContext {:?} is exhausted",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::RuntimeOwnerMismatch(owner) => write!(
                formatter,
                "Page runtime payload and renderer lifetime owner for Target {:?} in BrowserContext {:?} do not name the same renderer Page",
                owner.target_id(),
                owner.browser_context_id()
            ),
        }
    }
}

impl std::error::Error for BrowserPageResidenceRegistryError {}

/// Authoritative Target-to-Page-slot capability registry.
///
/// Browser Core stores the strong slot capability from Target registration
/// until Target retirement and, once a concrete Page commits, the unique
/// renderer Page lifetime owner and mutable Page command/cache payload.
/// Protocol migration storage carries an exact slot clone plus a non-owning,
/// invalidatable Page access; it cannot register, replace, or retire either
/// Browser-owned lifetime by dropping that projection.
#[derive(Default)]
pub(super) struct BrowserPageResidenceRegistry {
    entries: HashMap<BrowserPageOwnerKey, BrowserPageResidenceRecord>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserPageResidenceRegistrationState {
    Staged,
    Live,
}

struct BrowserPageResidenceRecord {
    handle: BrowserPageResidenceHandle,
    registration_state: BrowserPageResidenceRegistrationState,
    renderer_page_owner: Option<RendererPageLifetimeOwner>,
    page_runtime_owner: Option<BrowserPageRuntimeOwner>,
}

impl BrowserPageResidenceRecord {
    fn staged(handle: BrowserPageResidenceHandle) -> Self {
        Self {
            handle,
            registration_state: BrowserPageResidenceRegistrationState::Staged,
            renderer_page_owner: None,
            page_runtime_owner: None,
        }
    }

    fn is_staged_exact(&self, handle: &BrowserPageResidenceHandle) -> bool {
        self.registration_state == BrowserPageResidenceRegistrationState::Staged
            && self.handle.same_instance(handle)
    }

    fn is_live(&self) -> bool {
        self.registration_state == BrowserPageResidenceRegistrationState::Live
    }

    fn publish_if_staged_exact(&mut self, handle: &BrowserPageResidenceHandle) -> bool {
        if !self.is_staged_exact(handle) {
            return false;
        }
        self.registration_state = BrowserPageResidenceRegistrationState::Live;
        true
    }
}

impl BrowserPageResidenceRegistry {
    pub(super) fn validate_context_registration(
        &self,
        browser_context_id: &BrowserContextId,
        projection: &BrowserTargetTopologyProjection,
    ) -> Result<(), BrowserPageResidenceRegistryError> {
        let mut projected = Vec::<(BrowserPageOwnerKey, BrowserPageResidenceHandle)>::new();
        for slot in projection.slots() {
            let owner = BrowserPageOwnerKey::new(browser_context_id.as_str(), slot.target_id());
            if self.entries.contains_key(&owner) {
                return Err(BrowserPageResidenceRegistryError::DuplicateTarget(owner));
            }
            if let Some((first, _)) = self
                .entries
                .iter()
                .find(|(_, record)| record.handle.same_instance(slot.page_residence_handle()))
            {
                return Err(
                    BrowserPageResidenceRegistryError::DuplicateProjectedHandle {
                        first: first.clone(),
                        duplicate: owner,
                    },
                );
            }
            if let Some((first, _)) = projected
                .iter()
                .find(|(_, handle)| handle.same_instance(slot.page_residence_handle()))
            {
                return Err(
                    BrowserPageResidenceRegistryError::DuplicateProjectedHandle {
                        first: first.clone(),
                        duplicate: owner,
                    },
                );
            }
            projected.push((owner, slot.page_residence_handle().clone()));
        }
        Ok(())
    }

    pub(super) fn validate_projection(
        &self,
        browser_context_id: &BrowserContextId,
        projection: &BrowserTargetTopologyProjection,
    ) -> Result<(), BrowserPageResidenceRegistryError> {
        let authoritative_count = self
            .entries
            .iter()
            .filter(|(owner, record)| {
                owner.browser_context_id() == browser_context_id.as_str() && record.is_live()
            })
            .count();
        if authoritative_count != projection.slots().count() {
            return Err(
                BrowserPageResidenceRegistryError::ContextProjectionMismatch(
                    browser_context_id.clone(),
                ),
            );
        }
        for slot in projection.slots() {
            let owner = BrowserPageOwnerKey::new(browser_context_id.as_str(), slot.target_id());
            let Some(authoritative) = self.entries.get(&owner) else {
                return Err(BrowserPageResidenceRegistryError::UnknownTarget(owner));
            };
            if !authoritative.is_live() {
                return Err(BrowserPageResidenceRegistryError::TargetNotLive(owner));
            }
            if !authoritative
                .handle
                .same_instance(slot.page_residence_handle())
            {
                return Err(BrowserPageResidenceRegistryError::ProjectionMismatch(owner));
            }
        }
        Ok(())
    }

    fn live_handle(
        &self,
        key: &BrowserPageOwnerKey,
    ) -> Result<&BrowserPageResidenceHandle, BrowserPageResidenceRegistryError> {
        let Some(record) = self.entries.get(key) else {
            return Err(BrowserPageResidenceRegistryError::UnknownTarget(
                key.clone(),
            ));
        };
        if !record.is_live() {
            return Err(BrowserPageResidenceRegistryError::TargetNotLive(
                key.clone(),
            ));
        }
        Ok(&record.handle)
    }

    fn resolve(&self, expected: &PageResidenceIdentity) -> Option<BrowserPageOwnerKey> {
        let target_id = expected.target_id()?;
        let key = BrowserPageOwnerKey::new(expected.browser_context_id(), target_id);
        self.live_handle(&key)
            .is_ok_and(|handle| handle.is_current(expected))
            .then_some(key)
    }

    fn resolve_slot(&self, expected: &PageResidenceIdentity) -> Option<BrowserPageOwnerKey> {
        let target_id = expected.target_id()?;
        let key = BrowserPageOwnerKey::new(expected.browser_context_id(), target_id);
        self.live_handle(&key)
            .is_ok_and(|handle| handle.owns_identity_instance(expected))
            .then_some(key)
    }

    pub(super) fn identity(&self, key: &BrowserPageOwnerKey) -> Option<PageResidenceIdentity> {
        self.live_handle(key).ok().map(|handle| {
            handle.identity(
                key.browser_context_id().to_owned(),
                Some(key.target_id().to_owned()),
            )
        })
    }

    fn prepare_exact_transition(&self, key: &BrowserPageOwnerKey) -> Option<PageResidenceIdentity> {
        self.identity(key)
    }

    pub(super) fn prepare_replacement(
        &self,
        key: &BrowserPageOwnerKey,
    ) -> Option<PageResidenceIdentity> {
        self.prepare_exact_transition(key)
    }

    pub(super) fn capture_termination(
        &self,
        key: &BrowserPageOwnerKey,
    ) -> Option<PageResidenceIdentity> {
        self.prepare_exact_transition(key)
    }

    pub(super) fn accepts_transition(
        &self,
        key: &BrowserPageOwnerKey,
        expected: &PageResidenceIdentity,
    ) -> bool {
        self.live_handle(key)
            .is_ok_and(|handle| handle.is_current(expected))
    }

    fn commit_replacement_with_page_owners(
        &mut self,
        key: &BrowserPageOwnerKey,
        expected: &PageResidenceIdentity,
        successor_renderer_owner: &mut Option<RendererPageLifetimeOwner>,
        successor_runtime_owner: &mut Option<BrowserPageRuntimeOwner>,
    ) -> Result<
        (
            PageResidenceIdentity,
            Option<RendererPageLifetimeOwner>,
            Option<BrowserPageRuntimeAccess>,
        ),
        BrowserPageResidenceRegistryError,
    > {
        let Some(record) = self.entries.get_mut(key) else {
            return Err(BrowserPageResidenceRegistryError::UnknownTarget(
                key.clone(),
            ));
        };
        if !record.is_live() {
            return Err(BrowserPageResidenceRegistryError::TargetNotLive(
                key.clone(),
            ));
        }
        if successor_renderer_owner
            .as_ref()
            .zip(successor_runtime_owner.as_ref())
            .is_some_and(|(renderer, runtime)| runtime.page_id() != renderer.page_id())
            || successor_renderer_owner.is_some() != successor_runtime_owner.is_some()
        {
            return Err(BrowserPageResidenceRegistryError::RuntimeOwnerMismatch(
                key.clone(),
            ));
        }
        record
            .handle
            .try_advance_generation_if_current(expected)
            .map_err(|error| match error {
                BrowserPageResidenceAdvanceError::InstanceMismatch => {
                    BrowserPageResidenceRegistryError::ProjectionMismatch(key.clone())
                }
                BrowserPageResidenceAdvanceError::StaleGeneration { current_generation } => {
                    BrowserPageResidenceRegistryError::StaleTransition {
                        owner: key.clone(),
                        expected_generation: expected.loaded_page_generation(),
                        current_generation,
                    }
                }
                BrowserPageResidenceAdvanceError::GenerationExhausted => {
                    BrowserPageResidenceRegistryError::GenerationExhausted(key.clone())
                }
            })?;
        let current = record.handle.identity(
            key.browser_context_id().to_owned(),
            Some(key.target_id().to_owned()),
        );
        let successor_access = successor_runtime_owner
            .as_ref()
            .map(BrowserPageRuntimeOwner::access);
        let retired_renderer = std::mem::replace(
            &mut record.renderer_page_owner,
            successor_renderer_owner.take(),
        );
        let retired_runtime = std::mem::replace(
            &mut record.page_runtime_owner,
            successor_runtime_owner.take(),
        );
        // Invalidates every stale Protocol access in the same owner turn. The
        // separately returned renderer lifetime owner performs acknowledged
        // asynchronous teardown without keeping the Page payload addressable.
        drop(retired_runtime);
        Ok((current, retired_renderer, successor_access))
    }

    pub(super) fn commit_transition_with_page_owners(
        &mut self,
        key: &BrowserPageOwnerKey,
        expected: &PageResidenceIdentity,
        successor_renderer_owner: &mut Option<RendererPageLifetimeOwner>,
        successor_runtime_owner: &mut Option<BrowserPageRuntimeOwner>,
    ) -> Result<
        (
            PageResidenceIdentity,
            Option<RendererPageLifetimeOwner>,
            Option<BrowserPageRuntimeAccess>,
        ),
        BrowserPageResidenceRegistryError,
    > {
        self.commit_replacement_with_page_owners(
            key,
            expected,
            successor_renderer_owner,
            successor_runtime_owner,
        )
    }

    pub(super) fn commit_termination(
        &mut self,
        key: &BrowserPageOwnerKey,
        expected: &PageResidenceIdentity,
        remove_target: bool,
    ) -> Result<
        (PageResidenceIdentity, Option<RendererPageLifetimeOwner>),
        BrowserPageResidenceRegistryError,
    > {
        let mut no_successor_renderer = None;
        let mut no_successor_runtime = None;
        let (terminal, retired, no_successor_access) = self.commit_replacement_with_page_owners(
            key,
            expected,
            &mut no_successor_renderer,
            &mut no_successor_runtime,
        )?;
        debug_assert!(no_successor_access.is_none());
        if remove_target {
            let removed = self.entries.remove(key);
            debug_assert!(
                removed.is_some(),
                "same-turn terminal Page commit must remove its exact registry entry"
            );
        }
        Ok((terminal, retired))
    }

    pub(super) fn forget_target(
        &mut self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<RendererPageLifetimeOwner> {
        let record = self.entries.remove(owner)?;
        let BrowserPageResidenceRecord {
            renderer_page_owner,
            page_runtime_owner,
            ..
        } = record;
        drop(page_runtime_owner);
        renderer_page_owner
    }

    pub(super) fn handle_for_target(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<BrowserPageResidenceHandle> {
        self.live_handle(owner).ok().cloned()
    }

    pub(super) fn renderer_page_id_for_target(&self, owner: &BrowserPageOwnerKey) -> Option<u64> {
        self.entries
            .get(owner)
            .filter(|record| record.is_live())
            .and_then(|record| record.renderer_page_owner.as_ref())
            .map(RendererPageLifetimeOwner::page_id)
    }

    pub(super) fn handle_is_current(
        &self,
        owner: &BrowserPageOwnerKey,
        handle: &BrowserPageResidenceHandle,
    ) -> bool {
        self.live_handle(owner)
            .is_ok_and(|current| current.same_instance(handle))
    }
}

mod transaction;

impl BrowserNavigationOwner {
    /// Captures the exact Browser Core-owned Page residence for one Target.
    pub fn capture_page_residence(
        &self,
        browser_context_id: impl Into<String>,
        target_id: impl Into<String>,
    ) -> Option<PageResidenceIdentity> {
        self.page_residences
            .identity(&BrowserPageOwnerKey::new(browser_context_id, target_id))
    }

    /// Resolves an exact live Page residence to its protocol-neutral owner key.
    pub fn page_owner_key_if_current(
        &self,
        expected: &PageResidenceIdentity,
    ) -> Option<BrowserPageOwnerKey> {
        self.page_residences.resolve(expected)
    }

    /// Resolves the live Target that still owns the same physical Page slot.
    ///
    /// A successful cross-Document navigation advances the slot generation,
    /// so terminal fact projection cannot require the pre-navigation
    /// generation to remain current. The stable slot instance still prevents
    /// a removed/recreated Target with the same public id from matching.
    pub fn page_owner_key_for_same_slot(
        &self,
        expected: &PageResidenceIdentity,
    ) -> Option<BrowserPageOwnerKey> {
        self.page_residences.resolve_slot(expected)
    }

    /// Returns the registered Page capability for diagnostics/projection.
    pub fn page_residence_handle(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<BrowserPageResidenceHandle> {
        self.page_residences.handle_for_target(owner)
    }

    /// Verifies that a physical slot carries the exact Core-owned Page handle.
    pub fn page_residence_handle_is_current(
        &self,
        owner: &BrowserPageOwnerKey,
        handle: &BrowserPageResidenceHandle,
    ) -> bool {
        self.page_residences.handle_is_current(owner, handle)
    }

    /// Returns the physical renderer Page id owned by this exact Browser Page
    /// residence. Frontend projections deliberately do not affect it.
    pub fn renderer_page_id_for_owner(&self, owner: &BrowserPageOwnerKey) -> Option<u64> {
        self.page_residences.renderer_page_id_for_target(owner)
    }
}

#[cfg(test)]
mod tests;
