use std::collections::{HashMap, HashSet};

use crate::{
    browser_host::{
        BrowserContextHandle, BrowserContextId, BrowserTargetId, BrowserTargetSessionStorageAccess,
    },
    page::RendererPageLifetimeOwner,
    runtime::NavigationEngine,
};

use super::{
    BrowserContextEngineHandoff, BrowserContextRegistrationMetadata, BrowserNavigationOwner,
    BrowserPageOwnerKey, BrowserPageResidenceRegistryError, BrowserSelectedTargetEngineDisposition,
    BrowserTargetEngineHandoffOutcome, BrowserTargetEngineOwnerMismatch,
    BrowserTargetEngineRegistry, BrowserTargetTopologyProjection,
};

/// Frontend projection of the currently selected BrowserContext and engine.
///
/// The context identity is only a concurrency/invariant guard. Browser Core
/// remains authoritative for selection. The engine disposition temporarily
/// comes from the physical Page payload projection because engine residence
/// has not fully moved into Core yet. The exact Page-slot capability is
/// already registered by Core independently of that payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserContextSelectionProjection {
    browser_context_id: Option<BrowserContextId>,
    target_engine: BrowserSelectedTargetEngineDisposition,
}

impl BrowserContextSelectionProjection {
    pub fn new(
        browser_context_id: Option<String>,
        target_engine: BrowserSelectedTargetEngineDisposition,
    ) -> Self {
        Self {
            browser_context_id: browser_context_id.map(BrowserContextId::new),
            target_engine,
        }
    }

    pub fn browser_context_id(&self) -> Option<&str> {
        self.browser_context_id
            .as_ref()
            .map(BrowserContextId::as_str)
    }

    pub fn target_engine(&self) -> &BrowserSelectedTargetEngineDisposition {
        &self.target_engine
    }
}

/// Result of registering one exact BrowserContext identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserContextRegistration {
    browser_context_id: BrowserContextId,
    selected: bool,
    engine_outcome: Option<BrowserTargetEngineHandoffOutcome>,
    target_session_storage_accesses: Vec<(BrowserTargetId, BrowserTargetSessionStorageAccess)>,
}

impl BrowserContextRegistration {
    pub fn browser_context_id(&self) -> &str {
        self.browser_context_id.as_str()
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    pub fn engine_outcome(&self) -> Option<BrowserTargetEngineHandoffOutcome> {
        self.engine_outcome
    }

    pub fn target_session_storage_access(
        &self,
        target_id: &str,
    ) -> Option<&BrowserTargetSessionStorageAccess> {
        self.target_session_storage_accesses
            .iter()
            .find(|(candidate, _)| candidate.as_str() == target_id)
            .map(|(_, access)| access)
    }

    pub fn target_session_storage_accesses(
        &self,
    ) -> impl Iterator<Item = (&str, &BrowserTargetSessionStorageAccess)> {
        self.target_session_storage_accesses
            .iter()
            .map(|(target_id, access)| (target_id.as_str(), access))
    }
}

/// Result of selecting one already registered BrowserContext.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserContextActivation {
    previous_browser_context_id: BrowserContextId,
    browser_context_id: BrowserContextId,
    changed: bool,
    engine_outcome: Option<BrowserTargetEngineHandoffOutcome>,
}

impl BrowserContextActivation {
    pub fn previous_browser_context_id(&self) -> &str {
        self.previous_browser_context_id.as_str()
    }

    pub fn browser_context_id(&self) -> &str {
        self.browser_context_id.as_str()
    }

    pub fn changed(&self) -> bool {
        self.changed
    }

    pub fn engine_outcome(&self) -> Option<BrowserTargetEngineHandoffOutcome> {
        self.engine_outcome
    }
}

/// Exact, revision-bound authorization to remove one BrowserContext.
///
/// A selected context's successor is chosen by Core when the permit is
/// prepared. Protocol may inspect that exact physical projection to construct
/// a replacement engine, but cannot choose a different successor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserContextRemovalPermit {
    revision: u64,
    browser_context_id: BrowserContextId,
    browser_context_handle: BrowserContextHandle,
    was_selected: bool,
    successor_browser_context_id: Option<BrowserContextId>,
}

/// Exact Core-owned claim that one BrowserContext is being disposed.
///
/// The reservation does not remove Target/Page topology. It prevents new
/// work from entering the exact Context while its already-owned cleanup is
/// advanced as Browser Host participants. A public id reused by a later
/// Context cannot satisfy this capability.
#[derive(Debug)]
pub struct BrowserContextDisposalReservation {
    browser_context_handle: BrowserContextHandle,
}

impl BrowserContextDisposalReservation {
    pub fn browser_context_handle(&self) -> &BrowserContextHandle {
        &self.browser_context_handle
    }

    pub fn browser_context_id(&self) -> &str {
        self.browser_context_handle.browser_context_id()
    }
}

impl BrowserContextRemovalPermit {
    pub fn browser_context_id(&self) -> &str {
        self.browser_context_id.as_str()
    }

    pub fn browser_context_handle(&self) -> &BrowserContextHandle {
        &self.browser_context_handle
    }

    pub fn was_selected(&self) -> bool {
        self.was_selected
    }

    pub fn successor_browser_context_id(&self) -> Option<&str> {
        self.successor_browser_context_id
            .as_ref()
            .map(BrowserContextId::as_str)
    }
}

/// Result of committing one BrowserContext removal.
#[derive(Debug)]
pub struct BrowserContextRemoval {
    browser_context_id: BrowserContextId,
    selected_browser_context_id: Option<BrowserContextId>,
    engine_outcome: Option<BrowserTargetEngineHandoffOutcome>,
    retired_renderer_page_owners: Vec<RendererPageLifetimeOwner>,
}

impl BrowserContextRemoval {
    pub fn browser_context_id(&self) -> &str {
        self.browser_context_id.as_str()
    }

    pub fn selected_browser_context_id(&self) -> Option<&str> {
        self.selected_browser_context_id
            .as_ref()
            .map(BrowserContextId::as_str)
    }

    pub fn engine_outcome(&self) -> Option<BrowserTargetEngineHandoffOutcome> {
        self.engine_outcome
    }

    /// Takes any renderer Pages retired with residual Target runtime state.
    /// A normal disposal has already terminated every Page Target, so this is
    /// empty unless migration projection drift left a registered Page behind.
    pub fn take_retired_renderer_page_owners(&mut self) -> Vec<RendererPageLifetimeOwner> {
        std::mem::take(&mut self.retired_renderer_page_owners)
    }
}

/// BrowserContext topology or engine projection failed exact identity checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserContextRegistryError {
    PageResidence(BrowserPageResidenceRegistryError),
    DuplicateBrowserContext(BrowserContextId),
    UnknownBrowserContext(BrowserContextId),
    BrowserContextHandleIdMismatch {
        expected: BrowserContextId,
        projected: BrowserContextId,
    },
    BrowserContextHandleNotStaged(BrowserContextId),
    BrowserContextHandleNotLive(BrowserContextId),
    BrowserContextHandleProjectionMismatch(BrowserContextId),
    BrowserContextDisposing(BrowserContextId),
    BrowserContextDisposalReservationMismatch(BrowserContextId),
    SelectionProjectionMismatch {
        authoritative: Option<BrowserContextId>,
        projected: Option<BrowserContextId>,
    },
    EngineProjectionContextMismatch {
        projected: Option<BrowserContextId>,
        engine_owner: BrowserPageOwnerKey,
    },
    SelectedTargetProjectionMismatch {
        projected: Option<BrowserPageOwnerKey>,
        requested: Option<BrowserPageOwnerKey>,
    },
    TargetTopologyProjectionContextMismatch {
        expected: BrowserContextId,
        projected: BrowserContextId,
    },
    DuplicateTargetTopologyContext(BrowserContextId),
    DuplicateProjectedTarget(BrowserTargetId),
    DuplicateBrowserTarget(BrowserTargetId),
    ActiveTargetCreationMetadataWithoutActiveTarget(BrowserContextId),
    TargetSessionStorageMetadataWithoutTarget {
        browser_context_id: BrowserContextId,
        target_id: BrowserTargetId,
    },
    TargetHandleNotStaged(BrowserTargetId),
    TargetTopologyOwnerMissing {
        browser_context_id: BrowserContextId,
        target_id: BrowserTargetId,
    },
    TargetTopologyOwnerContextMismatch {
        browser_context_id: BrowserContextId,
        target_id: BrowserTargetId,
        actual_browser_context_id: BrowserContextId,
    },
    TargetHandleNotLive(BrowserTargetId),
    EngineOwnerMismatch(BrowserTargetEngineOwnerMismatch),
    StaleRemovalPermit {
        permit_revision: u64,
        current_revision: u64,
    },
    RemovalSuccessorRequired(BrowserContextId),
    UnexpectedRemovalSuccessor(BrowserContextId),
}

impl std::fmt::Display for BrowserContextRegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PageResidence(error) => error.fmt(formatter),
            Self::DuplicateBrowserContext(id) => {
                write!(
                    formatter,
                    "BrowserContext {:?} is already registered",
                    id.as_str()
                )
            }
            Self::UnknownBrowserContext(id) => {
                write!(
                    formatter,
                    "BrowserContext {:?} is not registered",
                    id.as_str()
                )
            }
            Self::BrowserContextHandleIdMismatch {
                expected,
                projected,
            } => write!(
                formatter,
                "BrowserContext {:?} received an exact handle for {:?}",
                expected.as_str(),
                projected.as_str()
            ),
            Self::BrowserContextHandleNotStaged(id) => write!(
                formatter,
                "BrowserContext handle for {:?} was already activated or retired before registration",
                id.as_str()
            ),
            Self::BrowserContextHandleNotLive(id) => write!(
                formatter,
                "BrowserContext handle for {:?} is no longer live for removal",
                id.as_str()
            ),
            Self::BrowserContextHandleProjectionMismatch(id) => write!(
                formatter,
                "physical BrowserContext handle for {:?} is not the live Browser Core instance",
                id.as_str()
            ),
            Self::BrowserContextDisposing(id) => write!(
                formatter,
                "BrowserContext {:?} is already being disposed",
                id.as_str()
            ),
            Self::BrowserContextDisposalReservationMismatch(id) => write!(
                formatter,
                "BrowserContext {:?} disposal reservation is no longer the Core-owned claim",
                id.as_str()
            ),
            Self::SelectionProjectionMismatch {
                authoritative,
                projected,
            } => write!(
                formatter,
                "authoritative selected BrowserContext {:?} does not match projected selection {:?}",
                authoritative.as_ref().map(BrowserContextId::as_str),
                projected.as_ref().map(BrowserContextId::as_str)
            ),
            Self::EngineProjectionContextMismatch {
                projected,
                engine_owner,
            } => write!(
                formatter,
                "projected BrowserContext {:?} does not own projected target engine {:?}",
                projected.as_ref().map(BrowserContextId::as_str),
                engine_owner
            ),
            Self::SelectedTargetProjectionMismatch {
                projected,
                requested,
            } => write!(
                formatter,
                "selected BrowserContext target projection {:?} does not match requested selected target {:?}",
                projected, requested
            ),
            Self::TargetTopologyProjectionContextMismatch {
                expected,
                projected,
            } => write!(
                formatter,
                "BrowserContext {:?} received Target topology projection for {:?}",
                expected.as_str(),
                projected.as_str()
            ),
            Self::DuplicateTargetTopologyContext(id) => write!(
                formatter,
                "BrowserContext {:?} already has a Target topology",
                id.as_str()
            ),
            Self::DuplicateProjectedTarget(id) => write!(
                formatter,
                "Target {:?} occurs more than once in one BrowserContext projection",
                id.as_str()
            ),
            Self::DuplicateBrowserTarget(id) => write!(
                formatter,
                "Target {:?} is already registered in another BrowserContext topology",
                id.as_str()
            ),
            Self::ActiveTargetCreationMetadataWithoutActiveTarget(id) => write!(
                formatter,
                "BrowserContext {:?} cannot install active Target creation metadata without an active Target",
                id.as_str()
            ),
            Self::TargetSessionStorageMetadataWithoutTarget {
                browser_context_id,
                target_id,
            } => write!(
                formatter,
                "BrowserContext {:?} cannot install sessionStorage metadata for absent Target {:?}",
                browser_context_id.as_str(),
                target_id.as_str()
            ),
            Self::TargetHandleNotStaged(id) => write!(
                formatter,
                "Target handle for {:?} was already activated or retired before BrowserContext registration",
                id.as_str()
            ),
            Self::TargetTopologyOwnerMissing {
                browser_context_id,
                target_id,
            } => write!(
                formatter,
                "BrowserContext {:?} Target topology has no reverse owner for {:?}",
                browser_context_id.as_str(),
                target_id.as_str()
            ),
            Self::TargetTopologyOwnerContextMismatch {
                browser_context_id,
                target_id,
                actual_browser_context_id,
            } => write!(
                formatter,
                "BrowserContext {:?} Target {:?} reverse owner belongs to {:?}",
                browser_context_id.as_str(),
                target_id.as_str(),
                actual_browser_context_id.as_str()
            ),
            Self::TargetHandleNotLive(id) => write!(
                formatter,
                "Target handle for {:?} is no longer live for BrowserContext removal",
                id.as_str()
            ),
            Self::EngineOwnerMismatch(error) => error.fmt(formatter),
            Self::StaleRemovalPermit {
                permit_revision,
                current_revision,
            } => write!(
                formatter,
                "BrowserContext removal permit revision {permit_revision} is stale at revision {current_revision}"
            ),
            Self::RemovalSuccessorRequired(id) => write!(
                formatter,
                "removing selected BrowserContext {:?} requires its Core-selected successor",
                id.as_str()
            ),
            Self::UnexpectedRemovalSuccessor(id) => write!(
                formatter,
                "BrowserContext {:?} removal does not accept a successor",
                id.as_str()
            ),
        }
    }
}

impl std::error::Error for BrowserContextRegistryError {}

impl From<BrowserPageResidenceRegistryError> for BrowserContextRegistryError {
    fn from(error: BrowserPageResidenceRegistryError) -> Self {
        Self::PageResidence(error)
    }
}

impl From<BrowserTargetEngineOwnerMismatch> for BrowserContextRegistryError {
    fn from(error: BrowserTargetEngineOwnerMismatch) -> Self {
        Self::EngineOwnerMismatch(error)
    }
}

/// Authoritative selected/inactive BrowserContext identity registry.
///
/// This registry deliberately stores no DevTools session, domain policy,
/// physical Page, or storage payload. Those are separate migration units.
#[derive(Default)]
pub(super) struct BrowserContextRegistry {
    selected: Option<BrowserContextId>,
    inactive: Vec<BrowserContextId>,
    handles: HashMap<BrowserContextId, BrowserContextHandle>,
    disposing: HashSet<BrowserContextHandle>,
    revision: u64,
}

impl BrowserContextRegistry {
    pub(super) fn contains(&self, browser_context_id: &BrowserContextId) -> bool {
        self.handles.contains_key(browser_context_id)
    }

    pub(super) fn handle(
        &self,
        browser_context_id: &BrowserContextId,
    ) -> Option<&BrowserContextHandle> {
        self.handles.get(browser_context_id)
    }

    pub(super) fn handle_is_current(&self, handle: &BrowserContextHandle) -> bool {
        self.handles
            .get(&BrowserContextId::new(handle.browser_context_id()))
            .is_some_and(|current| current.same_instance(handle) && current.is_live())
    }

    pub(super) fn accepts_owner_work(&self, browser_context_id: &BrowserContextId) -> bool {
        self.handles
            .get(browser_context_id)
            .is_some_and(|handle| handle.is_live() && !self.disposing.contains(handle))
    }

    pub(super) fn is_disposing(&self, browser_context_id: &BrowserContextId) -> bool {
        self.handles
            .get(browser_context_id)
            .is_some_and(|handle| self.disposing.contains(handle))
    }

    fn disposal_reservation_is_current(
        &self,
        reservation: &BrowserContextDisposalReservation,
    ) -> bool {
        self.handles
            .get(&BrowserContextId::new(reservation.browser_context_id()))
            .is_some_and(|current| {
                current.same_instance(&reservation.browser_context_handle)
                    && self.disposing.contains(current)
            })
    }

    pub(super) fn validate_projection(
        &self,
        selected_engine_owner: Option<&BrowserPageOwnerKey>,
        projection: &BrowserContextSelectionProjection,
    ) -> Result<(), BrowserContextRegistryError> {
        if let Some(engine_owner) = projection.target_engine.expected_owner()
            && projection
                .browser_context_id
                .as_ref()
                .map(BrowserContextId::as_str)
                != Some(engine_owner.browser_context_id())
        {
            return Err(
                BrowserContextRegistryError::EngineProjectionContextMismatch {
                    projected: projection.browser_context_id.clone(),
                    engine_owner: engine_owner.clone(),
                },
            );
        }
        if self.selected != projection.browser_context_id {
            return Err(BrowserContextRegistryError::SelectionProjectionMismatch {
                authoritative: self.selected.clone(),
                projected: projection.browser_context_id.clone(),
            });
        }
        BrowserTargetEngineRegistry::validate_current(
            selected_engine_owner,
            &projection.target_engine,
        )?;
        Ok(())
    }

    pub(super) fn validate_selected_target(
        &self,
        projection: &BrowserContextSelectionProjection,
        browser_context_id: &BrowserContextId,
        target_id: Option<&str>,
    ) -> Result<(), BrowserContextRegistryError> {
        let requested = target_id
            .map(|target_id| BrowserPageOwnerKey::new(browser_context_id.as_str(), target_id));
        let projected = projection.target_engine.expected_owner().cloned();
        if projected != requested {
            return Err(
                BrowserContextRegistryError::SelectedTargetProjectionMismatch {
                    projected,
                    requested,
                },
            );
        }
        Ok(())
    }

    fn validate_removal_permit(
        &self,
        permit: &BrowserContextRemovalPermit,
    ) -> Result<(), BrowserContextRegistryError> {
        if permit.revision != self.revision {
            return Err(BrowserContextRegistryError::StaleRemovalPermit {
                permit_revision: permit.revision,
                current_revision: self.revision,
            });
        }
        if !self.contains(&permit.browser_context_id) {
            return Err(BrowserContextRegistryError::UnknownBrowserContext(
                permit.browser_context_id.clone(),
            ));
        }
        let Some(current) = self.handles.get(&permit.browser_context_id) else {
            return Err(BrowserContextRegistryError::UnknownBrowserContext(
                permit.browser_context_id.clone(),
            ));
        };
        if !current.same_instance(&permit.browser_context_handle) {
            return Err(
                BrowserContextRegistryError::BrowserContextHandleProjectionMismatch(
                    permit.browser_context_id.clone(),
                ),
            );
        }
        if !current.is_live() {
            return Err(BrowserContextRegistryError::BrowserContextHandleNotLive(
                permit.browser_context_id.clone(),
            ));
        }
        Ok(())
    }

    pub(super) fn selected(&self) -> Option<&BrowserContextId> {
        self.selected.as_ref()
    }

    pub(super) fn ordered_context_ids(&self) -> impl Iterator<Item = &BrowserContextId> {
        self.selected.iter().chain(self.inactive.iter())
    }

    fn len(&self) -> usize {
        self.handles.len()
    }
}

impl BrowserNavigationOwner {
    pub fn register_browser_context<F>(
        &mut self,
        browser_context_id: impl Into<String>,
        target_topology: BrowserTargetTopologyProjection,
        projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserContextRegistration, BrowserContextRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        let browser_context_id = browser_context_id.into();
        let handle = BrowserContextHandle::staged(browser_context_id.clone());
        self.register_browser_context_with_handle_and_metadata(
            browser_context_id,
            handle,
            BrowserContextRegistrationMetadata::default(),
            target_topology,
            projection,
            create_replacement,
        )
    }

    pub fn register_browser_context_with_metadata<F>(
        &mut self,
        browser_context_id: impl Into<String>,
        registration_metadata: BrowserContextRegistrationMetadata,
        target_topology: BrowserTargetTopologyProjection,
        projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserContextRegistration, BrowserContextRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        let browser_context_id = browser_context_id.into();
        let handle = BrowserContextHandle::staged(browser_context_id.clone());
        self.register_browser_context_with_handle_and_metadata(
            browser_context_id,
            handle,
            registration_metadata,
            target_topology,
            projection,
            create_replacement,
        )
    }

    /// Registers the exact BrowserContext capability carried by the physical
    /// context projection.
    ///
    /// The public id alone is insufficient because typed frontends may reuse
    /// it after disposal. Registration activates this staged handle only when
    /// the Context, Target, Page and engine transaction commits together.
    pub fn register_browser_context_with_handle_and_metadata<F>(
        &mut self,
        browser_context_id: impl Into<String>,
        browser_context_handle: BrowserContextHandle,
        registration_metadata: BrowserContextRegistrationMetadata,
        target_topology: BrowserTargetTopologyProjection,
        projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserContextRegistration, BrowserContextRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        let browser_context_id = BrowserContextId::new(browser_context_id);
        let projected_handle_id =
            BrowserContextId::new(browser_context_handle.browser_context_id());
        if projected_handle_id != browser_context_id {
            return Err(
                BrowserContextRegistryError::BrowserContextHandleIdMismatch {
                    expected: browser_context_id,
                    projected: projected_handle_id,
                },
            );
        }
        if self.browser_contexts.contains(&browser_context_id) {
            return Err(BrowserContextRegistryError::DuplicateBrowserContext(
                browser_context_id,
            ));
        }
        let selected_engine_owner = self.selected_target_engine_owner().cloned();
        self.browser_contexts
            .validate_projection(selected_engine_owner.as_ref(), &projection)?;
        self.targets
            .validate_context_registration(&browser_context_id, &target_topology)?;
        let next = target_topology
            .active_target_id()
            .map(|target_id| BrowserPageOwnerKey::new(browser_context_id.as_str(), target_id));
        let created_target_ids = target_topology
            .active_target_id()
            .into_iter()
            .chain(target_topology.background_target_ids())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if registration_metadata.active_target_creation().is_some() && next.is_none() {
            return Err(
                BrowserContextRegistryError::ActiveTargetCreationMetadataWithoutActiveTarget(
                    browser_context_id,
                ),
            );
        }
        if let Some(target_id) = registration_metadata
            .target_session_storage_target_ids()
            .find(|target_id| {
                !created_target_ids
                    .iter()
                    .any(|candidate| candidate == target_id)
            })
        {
            return Err(
                BrowserContextRegistryError::TargetSessionStorageMetadataWithoutTarget {
                    browser_context_id,
                    target_id: BrowserTargetId::new(target_id),
                },
            );
        }
        if !browser_context_handle.reserve_activation() {
            return Err(BrowserContextRegistryError::BrowserContextHandleNotStaged(
                browser_context_id,
            ));
        }
        let mut target_session_storage_stores = target_topology
            .active_target_id()
            .into_iter()
            .chain(target_topology.background_target_ids())
            .filter_map(|target_id| {
                registration_metadata
                    .target_session_storage_store(target_id)
                    .map(|store| (BrowserTargetId::new(target_id), store))
            })
            .collect::<HashMap<_, _>>();
        if let (Some(active_target_id), Some(creation_metadata)) = (
            target_topology.active_target_id(),
            registration_metadata.active_target_creation(),
        ) {
            target_session_storage_stores
                .entry(BrowserTargetId::new(active_target_id))
                .or_insert_with(|| creation_metadata.session_storage_store());
        }
        let target_registration = self
            .targets
            .begin_context_registration(target_topology.clone(), target_session_storage_stores);
        let target_registration = match target_registration {
            Ok(registration) => registration,
            Err(error) => {
                browser_context_handle.rollback_activation_reservation();
                return Err(error);
            }
        };
        let page_registration = match self.page_residences.begin_context_registration(
            &mut self.target_runtimes,
            &browser_context_id,
            &target_topology,
        ) {
            Ok(registration) => registration,
            Err(error) => {
                let rolled_back = self
                    .targets
                    .rollback_context_registration(target_registration);
                debug_assert!(
                    rolled_back,
                    "same-turn Page registration rejection must restore Target registration"
                );
                browser_context_handle.rollback_activation_reservation();
                return Err(error.into());
            }
        };

        let (selected, engine_outcome) = if self.browser_contexts.selected.is_none() {
            let outcome = match self.target_engines.handoff_browser_context_engine(
                &mut self.target_runtimes,
                selected_engine_owner.as_ref(),
                BrowserContextEngineHandoff::new(projection.target_engine, next.clone()),
                create_replacement,
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    let page_rolled_back = self.page_residences.rollback_context_registration(
                        &mut self.target_runtimes,
                        page_registration,
                    );
                    let rolled_back = self
                        .targets
                        .rollback_context_registration(target_registration);
                    debug_assert!(
                        page_rolled_back,
                        "same-turn BrowserContext engine rejection must restore Page registration"
                    );
                    debug_assert!(
                        rolled_back,
                        "same-turn BrowserContext engine rejection must restore Target registration"
                    );
                    browser_context_handle.rollback_activation_reservation();
                    return Err(error.into());
                }
            };
            self.browser_contexts.selected = Some(browser_context_id.clone());
            (true, Some(outcome))
        } else {
            self.browser_contexts
                .inactive
                .push(browser_context_id.clone());
            (false, None)
        };
        if let (Some(owner), Some(creation_metadata)) = (
            next.as_ref(),
            registration_metadata.active_target_creation(),
        ) {
            self.install_target_creation_metadata(owner, creation_metadata);
        }
        self.page_residences
            .commit_context_registration(&mut self.target_runtimes, page_registration);
        let target_session_storage_accesses = self
            .targets
            .commit_context_registration(target_registration);
        let previous = self
            .browser_contexts
            .handles
            .insert(browser_context_id.clone(), browser_context_handle.clone());
        debug_assert!(previous.is_none());
        browser_context_handle.commit_activation_reservation();
        self.browser_contexts.revision += 1;
        for target_id in created_target_ids {
            let owner = BrowserPageOwnerKey::new(browser_context_id.as_str(), target_id);
            let Some(page) =
                self.capture_page_residence(owner.browser_context_id(), owner.target_id())
            else {
                tracing::error!(
                    browser_context_id = owner.browser_context_id(),
                    target_id = owner.target_id(),
                    "committed BrowserContext Target has no Page residence for creation fact"
                );
                continue;
            };
            if let Err(error) = self.record_target_created_fact(&owner, &page) {
                tracing::error!(
                    %error,
                    browser_context_id = owner.browser_context_id(),
                    target_id = owner.target_id(),
                    "failed to publish BrowserContext Target creation fact"
                );
            }
        }
        Ok(BrowserContextRegistration {
            browser_context_id,
            selected,
            engine_outcome,
            target_session_storage_accesses,
        })
    }

    pub fn activate_browser_context<F>(
        &mut self,
        browser_context_id: &str,
        projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserContextActivation, BrowserContextRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        let browser_context_id = BrowserContextId::new(browser_context_id);
        if !self.browser_contexts.contains(&browser_context_id) {
            return Err(BrowserContextRegistryError::UnknownBrowserContext(
                browser_context_id,
            ));
        }
        if !self
            .browser_contexts
            .accepts_owner_work(&browser_context_id)
        {
            return Err(BrowserContextRegistryError::BrowserContextDisposing(
                browser_context_id,
            ));
        }
        let selected_engine_owner = self.selected_target_engine_owner().cloned();
        self.browser_contexts
            .validate_projection(selected_engine_owner.as_ref(), &projection)?;
        let previous = self
            .browser_contexts
            .selected
            .clone()
            .expect("a registered BrowserContext registry must have a selection");
        if previous == browser_context_id {
            self.browser_contexts.validate_selected_target(
                &projection,
                &browser_context_id,
                self.targets
                    .active_target(&browser_context_id)
                    .map(crate::browser_host::BrowserTargetId::as_str),
            )?;
            return Ok(BrowserContextActivation {
                previous_browser_context_id: previous,
                browser_context_id,
                changed: false,
                engine_outcome: None,
            });
        }

        let index = self
            .browser_contexts
            .inactive
            .iter()
            .position(|candidate| candidate == &browser_context_id)
            .expect("a known non-selected BrowserContext must be inactive");
        let next = self
            .targets
            .active_target(&browser_context_id)
            .map(|target_id| {
                BrowserPageOwnerKey::new(browser_context_id.as_str(), target_id.as_str())
            });
        let engine_outcome = self.target_engines.handoff_browser_context_engine(
            &mut self.target_runtimes,
            selected_engine_owner.as_ref(),
            BrowserContextEngineHandoff::new(projection.target_engine, next),
            create_replacement,
        )?;

        let selected = self.browser_contexts.inactive.swap_remove(index);
        self.browser_contexts.inactive.push(previous.clone());
        self.browser_contexts.selected = Some(selected);
        self.browser_contexts.revision += 1;
        Ok(BrowserContextActivation {
            previous_browser_context_id: previous,
            browser_context_id,
            changed: true,
            engine_outcome: Some(engine_outcome),
        })
    }

    pub fn prepare_browser_context_removal(
        &self,
        browser_context_id: &str,
    ) -> Result<BrowserContextRemovalPermit, BrowserContextRegistryError> {
        let browser_context_id = BrowserContextId::new(browser_context_id);
        let browser_context_handle = self
            .browser_contexts
            .handle(&browser_context_id)
            .cloned()
            .ok_or_else(|| {
                BrowserContextRegistryError::UnknownBrowserContext(browser_context_id.clone())
            })?;
        self.prepare_browser_context_removal_for_handle(&browser_context_handle)
    }

    /// Captures removal authority for one exact BrowserContext instance.
    ///
    /// This is the admission primitive for queued Browser Host disposal: an
    /// old capability cannot authorize a newly registered context that reuses
    /// the same frontend-visible id.
    pub fn prepare_browser_context_removal_for_handle(
        &self,
        browser_context_handle: &BrowserContextHandle,
    ) -> Result<BrowserContextRemovalPermit, BrowserContextRegistryError> {
        let browser_context_id = BrowserContextId::new(browser_context_handle.browser_context_id());
        let Some(current) = self.browser_contexts.handle(&browser_context_id) else {
            return Err(BrowserContextRegistryError::UnknownBrowserContext(
                browser_context_id,
            ));
        };
        if !current.same_instance(browser_context_handle) {
            return Err(
                BrowserContextRegistryError::BrowserContextHandleProjectionMismatch(
                    browser_context_id,
                ),
            );
        }
        if !current.is_live() {
            return Err(BrowserContextRegistryError::BrowserContextHandleNotLive(
                browser_context_id,
            ));
        }
        if self.browser_contexts.disposing.contains(current) {
            return Err(BrowserContextRegistryError::BrowserContextDisposing(
                browser_context_id,
            ));
        }
        Ok(self.browser_context_removal_permit(browser_context_id, browser_context_handle))
    }

    /// Reserves one exact Context for a Browser Host disposal chain.
    ///
    /// This is a logical admission transition only. Existing Target/Page
    /// capabilities remain available to the disposal owner, while ordinary
    /// Context activation, Target registration and Page replacement reject
    /// new work until the reservation commits or rolls back.
    pub fn begin_browser_context_disposal(
        &mut self,
        browser_context_handle: &BrowserContextHandle,
    ) -> Result<BrowserContextDisposalReservation, BrowserContextRegistryError> {
        let browser_context_id = BrowserContextId::new(browser_context_handle.browser_context_id());
        let Some(current) = self.browser_contexts.handle(&browser_context_id) else {
            return Err(BrowserContextRegistryError::UnknownBrowserContext(
                browser_context_id,
            ));
        };
        if !current.same_instance(browser_context_handle) {
            return Err(
                BrowserContextRegistryError::BrowserContextHandleProjectionMismatch(
                    browser_context_id,
                ),
            );
        }
        if !current.is_live() {
            return Err(BrowserContextRegistryError::BrowserContextHandleNotLive(
                browser_context_id,
            ));
        }
        if !self.browser_contexts.disposing.insert(current.clone()) {
            return Err(BrowserContextRegistryError::BrowserContextDisposing(
                browser_context_id,
            ));
        }
        self.browser_contexts.revision += 1;
        Ok(BrowserContextDisposalReservation {
            browser_context_handle: browser_context_handle.clone(),
        })
    }

    /// Releases a disposal claim that failed before logical Context removal.
    pub fn rollback_browser_context_disposal(
        &mut self,
        reservation: BrowserContextDisposalReservation,
    ) -> bool {
        if !self
            .browser_contexts
            .disposal_reservation_is_current(&reservation)
        {
            return false;
        }
        let removed = self
            .browser_contexts
            .disposing
            .remove(&reservation.browser_context_handle);
        debug_assert!(removed);
        self.browser_contexts.revision += 1;
        true
    }

    /// Captures a final removal permit for the same exact disposal claim.
    ///
    /// Selection and successor are resolved at the terminal owner turn, not
    /// when cleanup first starts, so unrelated Context work may continue
    /// without a stale restore overwriting its later selection.
    pub fn prepare_browser_context_removal_for_disposal(
        &self,
        reservation: &BrowserContextDisposalReservation,
    ) -> Result<BrowserContextRemovalPermit, BrowserContextRegistryError> {
        let browser_context_id = BrowserContextId::new(reservation.browser_context_id());
        if !self
            .browser_contexts
            .disposal_reservation_is_current(reservation)
        {
            return Err(
                BrowserContextRegistryError::BrowserContextDisposalReservationMismatch(
                    browser_context_id,
                ),
            );
        }
        Ok(self.browser_context_removal_permit(
            browser_context_id,
            &reservation.browser_context_handle,
        ))
    }

    fn browser_context_removal_permit(
        &self,
        browser_context_id: BrowserContextId,
        browser_context_handle: &BrowserContextHandle,
    ) -> BrowserContextRemovalPermit {
        let was_selected = self.browser_contexts.selected.as_ref() == Some(&browser_context_id);
        let successor_browser_context_id = if was_selected {
            self.browser_contexts.inactive.first().cloned()
        } else {
            None
        };
        BrowserContextRemovalPermit {
            revision: self.browser_contexts.revision,
            browser_context_id,
            browser_context_handle: browser_context_handle.clone(),
            was_selected,
            successor_browser_context_id,
        }
    }

    pub fn commit_browser_context_removal<F>(
        &mut self,
        permit: BrowserContextRemovalPermit,
        projection: BrowserContextSelectionProjection,
        create_unbound_replacement: F,
    ) -> Result<BrowserContextRemoval, BrowserContextRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.browser_contexts.validate_removal_permit(&permit)?;
        let selected_engine_owner = self.selected_target_engine_owner().cloned();
        self.browser_contexts
            .validate_projection(selected_engine_owner.as_ref(), &projection)?;
        if permit.was_selected && permit.successor_browser_context_id.is_some() {
            return Err(BrowserContextRegistryError::RemovalSuccessorRequired(
                permit.browser_context_id,
            ));
        }

        let inactive_index = if permit.was_selected {
            None
        } else {
            Some(
                self.browser_contexts
                    .inactive
                    .iter()
                    .position(|candidate| candidate == &permit.browser_context_id)
                    .ok_or_else(|| {
                        BrowserContextRegistryError::UnknownBrowserContext(
                            permit.browser_context_id.clone(),
                        )
                    })?,
            )
        };
        if !permit.browser_context_handle.reserve_retirement() {
            return Err(BrowserContextRegistryError::BrowserContextHandleNotLive(
                permit.browser_context_id,
            ));
        }
        let target_removal = match self
            .targets
            .begin_context_removal(&permit.browser_context_id)
        {
            Ok(removal) => removal,
            Err(error) => {
                permit
                    .browser_context_handle
                    .rollback_retirement_reservation();
                return Err(error);
            }
        };

        let engine_outcome = if permit.was_selected {
            let outcome = self.target_engines.handoff_browser_context_engine(
                &mut self.target_runtimes,
                selected_engine_owner.as_ref(),
                BrowserContextEngineHandoff::new(projection.target_engine, None),
                create_unbound_replacement,
            )?;
            self.browser_contexts.selected = None;
            Some(outcome)
        } else {
            if let Some(index) = inactive_index {
                self.browser_contexts.inactive.swap_remove(index);
            }
            None
        };
        let removed_target_ids = self.targets.commit_context_removal(target_removal);
        let mut retired_renderer_page_owners = Vec::new();
        for target_id in removed_target_ids {
            let owner =
                BrowserPageOwnerKey::new(permit.browser_context_id.as_str(), target_id.as_str());
            retired_renderer_page_owners.extend(self.forget_target_runtime_state(&owner));
        }
        let removed_handle = self
            .browser_contexts
            .handles
            .remove(&permit.browser_context_id);
        debug_assert!(
            removed_handle
                .as_ref()
                .is_some_and(|handle| handle.same_instance(&permit.browser_context_handle)),
            "BrowserContext removal must retire the exact registered handle"
        );
        self.browser_contexts
            .disposing
            .remove(&permit.browser_context_handle);
        permit
            .browser_context_handle
            .commit_retirement_reservation();
        self.browser_contexts.revision += 1;
        Ok(BrowserContextRemoval {
            browser_context_id: permit.browser_context_id,
            selected_browser_context_id: self.browser_contexts.selected.clone(),
            engine_outcome,
            retired_renderer_page_owners,
        })
    }

    pub fn commit_browser_context_removal_with_successor<F>(
        &mut self,
        permit: BrowserContextRemovalPermit,
        projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserContextRemoval, BrowserContextRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.browser_contexts.validate_removal_permit(&permit)?;
        let selected_engine_owner = self.selected_target_engine_owner().cloned();
        self.browser_contexts
            .validate_projection(selected_engine_owner.as_ref(), &projection)?;
        let Some(successor_browser_context_id) = permit.successor_browser_context_id.clone() else {
            return Err(BrowserContextRegistryError::UnexpectedRemovalSuccessor(
                permit.browser_context_id,
            ));
        };
        if !permit.was_selected {
            return Err(BrowserContextRegistryError::UnexpectedRemovalSuccessor(
                permit.browser_context_id,
            ));
        }
        let successor_index = self
            .browser_contexts
            .inactive
            .iter()
            .position(|candidate| candidate == &successor_browser_context_id)
            .ok_or_else(|| {
                BrowserContextRegistryError::UnknownBrowserContext(
                    successor_browser_context_id.clone(),
                )
            })?;
        let next = self
            .targets
            .active_target(&successor_browser_context_id)
            .map(|target_id| {
                BrowserPageOwnerKey::new(successor_browser_context_id.as_str(), target_id.as_str())
            });
        if !permit.browser_context_handle.reserve_retirement() {
            return Err(BrowserContextRegistryError::BrowserContextHandleNotLive(
                permit.browser_context_id,
            ));
        }
        let target_removal = match self
            .targets
            .begin_context_removal(&permit.browser_context_id)
        {
            Ok(removal) => removal,
            Err(error) => {
                permit
                    .browser_context_handle
                    .rollback_retirement_reservation();
                return Err(error);
            }
        };
        let engine_outcome = match self.target_engines.handoff_browser_context_engine(
            &mut self.target_runtimes,
            selected_engine_owner.as_ref(),
            BrowserContextEngineHandoff::new(projection.target_engine, next),
            create_replacement,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                let rolled_back = self.targets.rollback_context_removal(target_removal);
                debug_assert!(
                    rolled_back,
                    "same-turn BrowserContext engine rejection must restore Target removal"
                );
                permit
                    .browser_context_handle
                    .rollback_retirement_reservation();
                return Err(error.into());
            }
        };
        let removed_target_ids = self.targets.commit_context_removal(target_removal);
        let mut retired_renderer_page_owners = Vec::new();
        for target_id in removed_target_ids {
            let owner =
                BrowserPageOwnerKey::new(permit.browser_context_id.as_str(), target_id.as_str());
            retired_renderer_page_owners.extend(self.forget_target_runtime_state(&owner));
        }
        let removed_handle = self
            .browser_contexts
            .handles
            .remove(&permit.browser_context_id);
        debug_assert!(
            removed_handle
                .as_ref()
                .is_some_and(|handle| handle.same_instance(&permit.browser_context_handle)),
            "BrowserContext removal must retire the exact registered handle"
        );
        self.browser_contexts
            .disposing
            .remove(&permit.browser_context_handle);
        permit
            .browser_context_handle
            .commit_retirement_reservation();

        let selected = self.browser_contexts.inactive.swap_remove(successor_index);
        self.browser_contexts.selected = Some(selected.clone());
        self.browser_contexts.revision += 1;
        Ok(BrowserContextRemoval {
            browser_context_id: permit.browser_context_id,
            selected_browser_context_id: Some(selected),
            engine_outcome: Some(engine_outcome),
            retired_renderer_page_owners,
        })
    }

    pub fn selected_browser_context_id(&self) -> Option<&str> {
        self.browser_contexts
            .selected()
            .map(BrowserContextId::as_str)
    }

    pub fn browser_context_count(&self) -> usize {
        self.browser_contexts.len()
    }

    pub fn has_browser_context(&self, browser_context_id: &str) -> bool {
        self.browser_contexts
            .contains(&BrowserContextId::new(browser_context_id))
    }

    pub fn browser_context_handle(
        &self,
        browser_context_id: &str,
    ) -> Option<&BrowserContextHandle> {
        self.browser_contexts
            .handle(&BrowserContextId::new(browser_context_id))
    }

    pub fn browser_context_handle_is_current(&self, handle: &BrowserContextHandle) -> bool {
        self.browser_contexts.handle_is_current(handle)
    }

    pub fn browser_context_accepts_owner_work(&self, browser_context_id: &str) -> bool {
        self.browser_contexts
            .accepts_owner_work(&BrowserContextId::new(browser_context_id))
    }
}

#[cfg(test)]
mod tests;
