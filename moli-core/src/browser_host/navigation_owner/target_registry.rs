use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::browser_host::{
    BrowserContextId, BrowserPageResidenceHandle, BrowserTargetHandle, BrowserTargetId,
    BrowserTargetSessionStorageAccess,
};
use crate::network::SharedWebStorageStore;

use super::{
    BrowserContextRegistryError, BrowserNavigationOwner, BrowserPageOwnerKey,
    BrowserPageResidenceRegistryError, BrowserTargetEngineContextMismatch,
    BrowserTargetEngineOwnerMismatch,
};

mod transaction;

/// Exact capabilities carried by one physical top-level Target slot.
///
/// Browser Core owns both registrations. Protocol migration storage carries
/// clones so a raw Target id or an unrelated Page slot cannot authorize an
/// owner transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTargetSlotProjection {
    target: BrowserTargetHandle,
    page_residence: BrowserPageResidenceHandle,
}

impl BrowserTargetSlotProjection {
    pub fn new(target: BrowserTargetHandle, page_residence: BrowserPageResidenceHandle) -> Self {
        Self {
            target,
            page_residence,
        }
    }

    pub fn target_id(&self) -> &str {
        self.target.target_id()
    }

    pub fn target_handle(&self) -> &BrowserTargetHandle {
        &self.target
    }

    pub fn page_residence_handle(&self) -> &BrowserPageResidenceHandle {
        &self.page_residence
    }
}

/// Physical active/background slots supplied only as an exact projection
/// guard while Target/Page payload storage remains outside Browser Core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTargetTopologyProjection {
    browser_context_id: BrowserContextId,
    active_target: Option<BrowserTargetSlotProjection>,
    background_targets: Vec<BrowserTargetSlotProjection>,
}

impl BrowserTargetTopologyProjection {
    pub fn new(
        browser_context_id: impl Into<String>,
        active_target: Option<BrowserTargetSlotProjection>,
        background_targets: impl IntoIterator<Item = BrowserTargetSlotProjection>,
    ) -> Self {
        Self {
            browser_context_id: BrowserContextId::new(browser_context_id),
            active_target,
            background_targets: background_targets.into_iter().collect(),
        }
    }

    pub fn browser_context_id(&self) -> &str {
        self.browser_context_id.as_str()
    }

    pub fn active_target_id(&self) -> Option<&str> {
        self.active_target
            .as_ref()
            .map(BrowserTargetSlotProjection::target_id)
    }

    pub fn background_target_ids(&self) -> impl Iterator<Item = &str> {
        self.background_targets
            .iter()
            .map(BrowserTargetSlotProjection::target_id)
    }

    pub(super) fn slots(&self) -> impl Iterator<Item = &BrowserTargetSlotProjection> {
        self.active_target
            .iter()
            .chain(self.background_targets.iter())
    }
}

/// Authoritative residence of one live top-level browser Target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserTargetResidence {
    Active,
    Background,
}

/// Target topology, context selection, or engine identity failed an exact
/// Browser Owner transaction guard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserTargetRegistryError {
    BrowserContext(BrowserContextRegistryError),
    PageResidence(BrowserPageResidenceRegistryError),
    EngineOwner(BrowserTargetEngineOwnerMismatch),
    EngineContext(BrowserTargetEngineContextMismatch),
    UnknownBrowserContext(BrowserContextId),
    DuplicateBrowserContextProjection(BrowserContextId),
    ProjectionContextMismatch {
        expected: BrowserContextId,
        projected: BrowserContextId,
    },
    DuplicateProjectedTarget(BrowserTargetId),
    DuplicateTarget(BrowserTargetId),
    UnknownTarget(BrowserTargetId),
    TargetHandleNotStaged(BrowserTargetId),
    TargetHandleNotLive(BrowserTargetId),
    TargetHandleProjectionMismatch(BrowserTargetId),
    TargetContextMismatch {
        target_id: BrowserTargetId,
        expected: BrowserContextId,
        actual: BrowserContextId,
    },
    TopologyProjectionMismatch {
        browser_context_id: BrowserContextId,
        authoritative_active: Option<BrowserTargetId>,
        authoritative_background: Vec<BrowserTargetId>,
        projected_active: Option<BrowserTargetId>,
        projected_background: Vec<BrowserTargetId>,
    },
    SelectedBrowserContextRequired {
        requested: BrowserContextId,
        selected: Option<BrowserContextId>,
    },
    TargetIsNotActive(BrowserPageOwnerKey),
    TargetIsNotBackground(BrowserPageOwnerKey),
    TargetHasCommittedRendererPage(BrowserPageOwnerKey),
    TargetTopologyOwnerMismatch(BrowserPageOwnerKey),
}

impl std::fmt::Display for BrowserTargetRegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BrowserContext(error) => error.fmt(formatter),
            Self::PageResidence(error) => error.fmt(formatter),
            Self::EngineOwner(error) => error.fmt(formatter),
            Self::EngineContext(error) => error.fmt(formatter),
            Self::UnknownBrowserContext(id) => write!(
                formatter,
                "BrowserContext {:?} has no Target topology",
                id.as_str()
            ),
            Self::DuplicateBrowserContextProjection(id) => write!(
                formatter,
                "BrowserContext {:?} already has a Target topology",
                id.as_str()
            ),
            Self::ProjectionContextMismatch {
                expected,
                projected,
            } => write!(
                formatter,
                "Target topology projection for BrowserContext {:?} was supplied for {:?}",
                expected.as_str(),
                projected.as_str()
            ),
            Self::DuplicateProjectedTarget(id) => write!(
                formatter,
                "Target {:?} occurs more than once in one physical topology projection",
                id.as_str()
            ),
            Self::DuplicateTarget(id) => {
                write!(formatter, "Target {:?} is already registered", id.as_str())
            }
            Self::UnknownTarget(id) => {
                write!(formatter, "Target {:?} is not registered", id.as_str())
            }
            Self::TargetHandleNotStaged(id) => write!(
                formatter,
                "Target {:?} handle is no longer staged for registration",
                id.as_str()
            ),
            Self::TargetHandleNotLive(id) => write!(
                formatter,
                "Target {:?} handle is no longer live for retirement",
                id.as_str()
            ),
            Self::TargetHandleProjectionMismatch(id) => write!(
                formatter,
                "physical Target handle for {:?} is not the live Browser Core instance",
                id.as_str()
            ),
            Self::TargetContextMismatch {
                target_id,
                expected,
                actual,
            } => write!(
                formatter,
                "Target {:?} belongs to BrowserContext {:?}, not {:?}",
                target_id.as_str(),
                actual.as_str(),
                expected.as_str()
            ),
            Self::TopologyProjectionMismatch {
                browser_context_id,
                authoritative_active,
                authoritative_background,
                projected_active,
                projected_background,
            } => write!(
                formatter,
                "authoritative Target topology for BrowserContext {:?} ({:?}, {:?}) does not match physical projection ({:?}, {:?})",
                browser_context_id.as_str(),
                authoritative_active.as_ref().map(BrowserTargetId::as_str),
                authoritative_background
                    .iter()
                    .map(BrowserTargetId::as_str)
                    .collect::<Vec<_>>(),
                projected_active.as_ref().map(BrowserTargetId::as_str),
                projected_background
                    .iter()
                    .map(BrowserTargetId::as_str)
                    .collect::<Vec<_>>()
            ),
            Self::SelectedBrowserContextRequired {
                requested,
                selected,
            } => write!(
                formatter,
                "Target activation requires selected BrowserContext {:?}, current selection is {:?}",
                requested.as_str(),
                selected.as_ref().map(BrowserContextId::as_str)
            ),
            Self::TargetIsNotActive(owner) => write!(
                formatter,
                "Target {:?} is not the active Target in BrowserContext {:?}",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::TargetIsNotBackground(owner) => write!(
                formatter,
                "Target {:?} is not a background Target in BrowserContext {:?}",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::TargetHasCommittedRendererPage(owner) => write!(
                formatter,
                "Target {:?} in BrowserContext {:?} has a committed renderer Page and requires typed termination",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::TargetTopologyOwnerMismatch(owner) => write!(
                formatter,
                "Target {:?} reverse owner is absent from BrowserContext {:?} topology",
                owner.target_id(),
                owner.browser_context_id()
            ),
        }
    }
}

impl std::error::Error for BrowserTargetRegistryError {}

impl From<BrowserContextRegistryError> for BrowserTargetRegistryError {
    fn from(error: BrowserContextRegistryError) -> Self {
        Self::BrowserContext(error)
    }
}

impl From<BrowserPageResidenceRegistryError> for BrowserTargetRegistryError {
    fn from(error: BrowserPageResidenceRegistryError) -> Self {
        Self::PageResidence(error)
    }
}

impl From<BrowserTargetEngineOwnerMismatch> for BrowserTargetRegistryError {
    fn from(error: BrowserTargetEngineOwnerMismatch) -> Self {
        Self::EngineOwner(error)
    }
}

impl From<BrowserTargetEngineContextMismatch> for BrowserTargetRegistryError {
    fn from(error: BrowserTargetEngineContextMismatch) -> Self {
        Self::EngineContext(error)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct BrowserContextTargets {
    active: Option<BrowserTargetId>,
    background: Vec<BrowserTargetId>,
}

#[derive(Clone, Debug)]
struct BrowserTargetRecord {
    browser_context_id: BrowserContextId,
    handle: BrowserTargetHandle,
    session_storage_store: SharedWebStorageStore,
}

impl PartialEq for BrowserTargetRecord {
    fn eq(&self, other: &Self) -> bool {
        self.browser_context_id == other.browser_context_id
            && self.handle == other.handle
            && Arc::ptr_eq(&self.session_storage_store, &other.session_storage_store)
    }
}

impl Eq for BrowserTargetRecord {}

/// Authoritative top-level Target identity, context membership, and
/// active/background topology.
///
/// Physical Page payload, renderer, DevTools session, opener metadata, and
/// storage payloads deliberately remain outside this registry during Phase 2.
/// The exact Page-slot capability is owned by the sibling `page_registry`.
#[derive(Default)]
pub(super) struct BrowserTargetRegistry {
    contexts: HashMap<BrowserContextId, BrowserContextTargets>,
    owners: HashMap<BrowserTargetId, BrowserTargetRecord>,
}

impl BrowserTargetRegistry {
    pub(super) fn session_storage_access(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<BrowserTargetSessionStorageAccess> {
        let record = self.owners.get(&BrowserTargetId::new(owner.target_id()))?;
        if record.browser_context_id.as_str() != owner.browser_context_id() {
            return None;
        }
        Some(BrowserTargetSessionStorageAccess::new(
            record.handle.clone(),
            record.session_storage_store.clone(),
        ))
    }

    pub(super) fn validate_context_registration(
        &self,
        browser_context_id: &BrowserContextId,
        projection: &BrowserTargetTopologyProjection,
    ) -> Result<(), BrowserContextRegistryError> {
        if projection.browser_context_id != *browser_context_id {
            return Err(
                BrowserContextRegistryError::TargetTopologyProjectionContextMismatch {
                    expected: browser_context_id.clone(),
                    projected: projection.browser_context_id.clone(),
                },
            );
        }
        if self.contexts.contains_key(browser_context_id) {
            return Err(BrowserContextRegistryError::DuplicateTargetTopologyContext(
                browser_context_id.clone(),
            ));
        }
        let mut projected = HashSet::new();
        for slot in projection.slots() {
            let handle = slot.target_handle();
            let target_id = BrowserTargetId::new(slot.target_id());
            if !projected.insert(target_id.clone()) {
                return Err(BrowserContextRegistryError::DuplicateProjectedTarget(
                    target_id,
                ));
            }
            if !handle.is_staged() {
                return Err(BrowserContextRegistryError::TargetHandleNotStaged(
                    target_id,
                ));
            }
            if self.owners.contains_key(&target_id) {
                return Err(BrowserContextRegistryError::DuplicateBrowserTarget(
                    target_id,
                ));
            }
        }
        Ok(())
    }

    pub(super) fn validate_projection(
        &self,
        browser_context_id: &BrowserContextId,
        projection: &BrowserTargetTopologyProjection,
    ) -> Result<(), BrowserTargetRegistryError> {
        if projection.browser_context_id != *browser_context_id {
            return Err(BrowserTargetRegistryError::ProjectionContextMismatch {
                expected: browser_context_id.clone(),
                projected: projection.browser_context_id.clone(),
            });
        }
        let Some(topology) = self.contexts.get(browser_context_id) else {
            return Err(BrowserTargetRegistryError::UnknownBrowserContext(
                browser_context_id.clone(),
            ));
        };
        let projected_active = projection.active_target_id().map(BrowserTargetId::new);
        let projected_background = projection
            .background_target_ids()
            .map(BrowserTargetId::new)
            .collect::<Vec<_>>();
        if topology.active != projected_active || topology.background != projected_background {
            return Err(BrowserTargetRegistryError::TopologyProjectionMismatch {
                browser_context_id: browser_context_id.clone(),
                authoritative_active: topology.active.clone(),
                authoritative_background: topology.background.clone(),
                projected_active,
                projected_background,
            });
        }
        for slot in projection.slots() {
            let handle = slot.target_handle();
            let target_id = BrowserTargetId::new(slot.target_id());
            let Some(record) = self.owners.get(&target_id) else {
                return Err(BrowserTargetRegistryError::UnknownTarget(target_id));
            };
            if record.browser_context_id != *browser_context_id
                || !record.handle.same_instance(handle)
                || !handle.is_live()
            {
                return Err(BrowserTargetRegistryError::TargetHandleProjectionMismatch(
                    target_id,
                ));
            }
        }
        Ok(())
    }

    pub(super) fn validate_new_target(
        &self,
        browser_context_id: &BrowserContextId,
        target_id: &BrowserTargetId,
    ) -> Result<(), BrowserTargetRegistryError> {
        if !self.contexts.contains_key(browser_context_id) {
            return Err(BrowserTargetRegistryError::UnknownBrowserContext(
                browser_context_id.clone(),
            ));
        }
        if self.owners.contains_key(target_id) {
            return Err(BrowserTargetRegistryError::DuplicateTarget(
                target_id.clone(),
            ));
        }
        Ok(())
    }

    pub(super) fn validate_target_owner(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> Result<BrowserTargetResidence, BrowserTargetRegistryError> {
        let target_id = BrowserTargetId::new(owner.target_id());
        let browser_context_id = BrowserContextId::new(owner.browser_context_id());
        let Some(actual) = self.owners.get(&target_id) else {
            return Err(BrowserTargetRegistryError::UnknownTarget(target_id));
        };
        if actual.browser_context_id != browser_context_id {
            return Err(BrowserTargetRegistryError::TargetContextMismatch {
                target_id,
                expected: browser_context_id,
                actual: actual.browser_context_id.clone(),
            });
        }
        if !actual.handle.is_live() {
            return Err(BrowserTargetRegistryError::TargetHandleProjectionMismatch(
                target_id,
            ));
        }
        let Some(topology) = self.contexts.get(&actual.browser_context_id) else {
            return Err(BrowserTargetRegistryError::UnknownBrowserContext(
                actual.browser_context_id.clone(),
            ));
        };
        if topology.active.as_ref() == Some(&target_id) {
            return Ok(BrowserTargetResidence::Active);
        }
        if topology.background.contains(&target_id) {
            return Ok(BrowserTargetResidence::Background);
        }
        Err(BrowserTargetRegistryError::TargetTopologyOwnerMismatch(
            owner.clone(),
        ))
    }

    pub(super) fn active_target(
        &self,
        browser_context_id: &BrowserContextId,
    ) -> Option<&BrowserTargetId> {
        self.contexts
            .get(browser_context_id)
            .and_then(|topology| topology.active.as_ref())
    }

    pub(super) fn active_target_for_registered_context(
        &self,
        browser_context_id: &BrowserContextId,
    ) -> Result<Option<&BrowserTargetId>, BrowserTargetRegistryError> {
        self.contexts
            .get(browser_context_id)
            .map(|topology| topology.active.as_ref())
            .ok_or_else(|| {
                BrowserTargetRegistryError::UnknownBrowserContext(browser_context_id.clone())
            })
    }

    pub(super) fn context_for_target(
        &self,
        target_id: &BrowserTargetId,
    ) -> Option<&BrowserContextId> {
        self.owners
            .get(target_id)
            .map(|record| &record.browser_context_id)
    }

    pub(super) fn handle_for_target(
        &self,
        target_id: &BrowserTargetId,
    ) -> Option<&BrowserTargetHandle> {
        self.owners.get(target_id).map(|record| &record.handle)
    }

    pub(super) fn handle_is_current(&self, handle: &BrowserTargetHandle) -> bool {
        self.owners
            .get(&BrowserTargetId::new(handle.target_id()))
            .is_some_and(|record| record.handle.same_instance(handle) && handle.is_live())
    }

    pub(super) fn target_count(&self) -> usize {
        self.owners.len()
    }

    pub(super) fn context_target_count(&self, browser_context_id: &BrowserContextId) -> usize {
        self.contexts.get(browser_context_id).map_or(0, |topology| {
            usize::from(topology.active.is_some()) + topology.background.len()
        })
    }

    pub(super) fn ordered_targets_for_context(
        &self,
        browser_context_id: &BrowserContextId,
    ) -> Result<Vec<(BrowserTargetId, BrowserTargetResidence)>, BrowserTargetRegistryError> {
        let Some(topology) = self.contexts.get(browser_context_id) else {
            return Err(BrowserTargetRegistryError::UnknownBrowserContext(
                browser_context_id.clone(),
            ));
        };
        Ok(topology
            .active
            .iter()
            .cloned()
            .map(|target_id| (target_id, BrowserTargetResidence::Active))
            .chain(
                topology
                    .background
                    .iter()
                    .cloned()
                    .map(|target_id| (target_id, BrowserTargetResidence::Background)),
            )
            .collect())
    }
}

impl BrowserNavigationOwner {
    /// Returns the exact live Target capability for one public Target id.
    pub fn target_handle(&self, target_id: &str) -> Option<BrowserTargetHandle> {
        self.targets
            .handle_for_target(&BrowserTargetId::new(target_id))
            .cloned()
    }

    /// Checks both public Target id and stable Target instance capability.
    pub fn target_handle_is_current(&self, handle: &BrowserTargetHandle) -> bool {
        self.targets.handle_is_current(handle)
    }
}

#[cfg(test)]
mod tests;
