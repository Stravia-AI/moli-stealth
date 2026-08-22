use std::collections::HashSet;

use moli_core::browser_host::{
    BrowserContextDisposalReservation, BrowserContextId, BrowserContextRegistrationMetadata,
    BrowserContextRegistryError, BrowserContextRemovalPermit, BrowserContextRuntimeRegistryError,
    BrowserContextSelectionProjection,
};
use moli_core::page::RendererPageLifetimeOwner;
use moli_core::runtime::RendererBrowserContextRuntimeOwner;

use crate::devtools_runtime::{DevToolsError, DevToolsErrorKind};

use super::{BrowserContext, BrowserEngineReplacementInputs, CdpConnection};

pub(super) struct ProjectedBrowserContextRemoval {
    pub(super) browser_context: BrowserContext,
    pub(super) selection_changed: bool,
    retired_renderer_page_owners: Vec<RendererPageLifetimeOwner>,
    renderer_runtime_owner: RendererBrowserContextRuntimeOwner,
}

impl ProjectedBrowserContextRemoval {
    pub(super) fn into_parts(
        self,
    ) -> (
        BrowserContext,
        bool,
        Vec<RendererPageLifetimeOwner>,
        RendererBrowserContextRuntimeOwner,
    ) {
        (
            self.browser_context,
            self.selection_changed,
            self.retired_renderer_page_owners,
            self.renderer_runtime_owner,
        )
    }
}

/// A Core BrowserContext transaction or its same-turn physical projection was
/// rejected before the two registries could diverge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BrowserContextProjectionError {
    Core(BrowserContextRegistryError),
    Runtime(BrowserContextRuntimeRegistryError),
    PhysicalContextCountMismatch {
        authoritative: usize,
        projected: usize,
    },
    PhysicalSelectionMismatch {
        authoritative: Option<String>,
        projected: Option<String>,
    },
    DuplicatePhysicalContext(String),
    PhysicalContextMissingFromCore(String),
    PhysicalContextMissing(String),
    PhysicalRuntimeOwnerMissing(String),
    UnexpectedUnchangedActivation(String),
}

impl BrowserContextProjectionError {
    pub(crate) fn is_unknown_context(&self) -> bool {
        matches!(
            self,
            Self::Core(BrowserContextRegistryError::UnknownBrowserContext(_))
                | Self::Runtime(BrowserContextRuntimeRegistryError::Context(
                    BrowserContextRegistryError::UnknownBrowserContext(_)
                ))
        )
    }
}

impl std::fmt::Display for BrowserContextProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Core(error) => error.fmt(formatter),
            Self::Runtime(error) => error.fmt(formatter),
            Self::PhysicalContextCountMismatch {
                authoritative,
                projected,
            } => write!(
                formatter,
                "Browser Core has {authoritative} BrowserContexts but the physical projection has {projected}"
            ),
            Self::PhysicalSelectionMismatch {
                authoritative,
                projected,
            } => write!(
                formatter,
                "authoritative selected BrowserContext {authoritative:?} does not match physical selection {projected:?}"
            ),
            Self::DuplicatePhysicalContext(browser_context_id) => write!(
                formatter,
                "physical BrowserContext projection contains duplicate identity {browser_context_id:?}"
            ),
            Self::PhysicalContextMissingFromCore(browser_context_id) => write!(
                formatter,
                "physical BrowserContext {browser_context_id:?} is not registered in Browser Core"
            ),
            Self::PhysicalContextMissing(browser_context_id) => write!(
                formatter,
                "Core-known BrowserContext {browser_context_id:?} has no exact physical projection"
            ),
            Self::PhysicalRuntimeOwnerMissing(browser_context_id) => write!(
                formatter,
                "new physical BrowserContext {browser_context_id:?} has no renderer runtime owner to register"
            ),
            Self::UnexpectedUnchangedActivation(browser_context_id) => write!(
                formatter,
                "Core reported unchanged activation for physically inactive BrowserContext {browser_context_id:?}"
            ),
        }
    }
}

impl std::error::Error for BrowserContextProjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core(error) => Some(error),
            Self::Runtime(error) => Some(error),
            _ => None,
        }
    }
}

impl From<BrowserContextRegistryError> for BrowserContextProjectionError {
    fn from(error: BrowserContextRegistryError) -> Self {
        Self::Core(error)
    }
}

impl From<BrowserContextRuntimeRegistryError> for BrowserContextProjectionError {
    fn from(error: BrowserContextRuntimeRegistryError) -> Self {
        match error {
            BrowserContextRuntimeRegistryError::Context(error) => Self::Core(error),
            error => Self::Runtime(error),
        }
    }
}

impl From<BrowserContextProjectionError> for DevToolsError {
    fn from(error: BrowserContextProjectionError) -> Self {
        Self::new(
            DevToolsErrorKind::Internal,
            format!("BrowserContextProjectionRejected: {error}"),
        )
    }
}

/// Transitional physical projection for Core-owned BrowserContext topology.
///
/// This module first validates the complete physical identity set. Operations
/// that move or remove an existing payload stage that `Option`/`Vec` change,
/// call Core in the same actor turn, and either publish the matching projection
/// or restore the exact physical slots on typed rejection. It is intentionally
/// separate from command/session lifecycle orchestration so CDP state cannot
/// become part of the authoritative registry transaction.
impl CdpConnection {
    pub(super) fn validate_browser_context_topology_projection(
        &self,
    ) -> Result<(), BrowserContextProjectionError> {
        let projected_contexts = self
            .browser_contexts()
            .map(|browser_context| {
                (
                    browser_context.id.clone(),
                    browser_context.browser_context_handle().clone(),
                )
            })
            .collect::<Vec<_>>();
        let authoritative_count = self.registered_browser_context_count();
        if projected_contexts.len() != authoritative_count {
            return Err(
                BrowserContextProjectionError::PhysicalContextCountMismatch {
                    authoritative: authoritative_count,
                    projected: projected_contexts.len(),
                },
            );
        }
        let projected_selection = self
            .browser_context
            .as_ref()
            .map(|context| context.id.as_str());
        let authoritative_selection = self
            .browser_host_state
            .navigation_owner()
            .selected_browser_context_id()
            .map(str::to_owned);
        if projected_selection != authoritative_selection.as_deref() {
            return Err(BrowserContextProjectionError::PhysicalSelectionMismatch {
                authoritative: authoritative_selection,
                projected: projected_selection.map(str::to_owned),
            });
        }

        let mut unique_ids = HashSet::with_capacity(projected_contexts.len());
        for (browser_context_id, browser_context_handle) in projected_contexts {
            if !unique_ids.insert(browser_context_id.clone()) {
                return Err(BrowserContextProjectionError::DuplicatePhysicalContext(
                    browser_context_id,
                ));
            }
            if !self
                .browser_host_state
                .navigation_owner()
                .has_browser_context(&browser_context_id)
            {
                return Err(
                    BrowserContextProjectionError::PhysicalContextMissingFromCore(
                        browser_context_id,
                    ),
                );
            }
            if !self
                .browser_host_state
                .navigation_owner()
                .browser_context_handle_is_current(&browser_context_handle)
            {
                return Err(
                    BrowserContextRegistryError::BrowserContextHandleProjectionMismatch(
                        BrowserContextId::new(browser_context_id),
                    )
                    .into(),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn debug_assert_browser_context_topology_projection(&self) {
        let validation = self.validate_browser_context_topology_projection();
        debug_assert!(
            validation.is_ok(),
            "Browser Core and physical BrowserContext topology diverged: {validation:?}"
        );
        #[cfg(debug_assertions)]
        self.debug_assert_browser_target_topology_projection();
    }

    pub(super) fn selected_browser_context_projection(&self) -> BrowserContextSelectionProjection {
        BrowserContextSelectionProjection::new(
            self.browser_context
                .as_ref()
                .map(|context| context.id.clone()),
            self.selected_target_engine_disposition(),
        )
    }

    pub(super) fn register_browser_context_projection_with_metadata(
        &mut self,
        mut browser_context: BrowserContext,
        mut registration_metadata: BrowserContextRegistrationMetadata,
    ) -> Result<bool, BrowserContextProjectionError> {
        self.validate_browser_context_topology_projection()?;
        let browser_context_id = browser_context.id.clone();
        let browser_context_handle = browser_context.browser_context_handle().clone();
        let target_topology = Self::browser_target_topology_projection(&browser_context);
        let registered_target_ids = target_topology
            .active_target_id()
            .into_iter()
            .chain(target_topology.background_target_ids())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        for target_id in &registered_target_ids {
            if let Some(store) = browser_context.target_session_storage_store(target_id) {
                registration_metadata = registration_metadata
                    .with_target_session_storage_store(target_id.clone(), store);
            }
        }
        let renderer_runtime_owner = browser_context
            .take_renderer_runtime_owner_for_host_registration()
            .ok_or_else(|| {
                BrowserContextProjectionError::PhysicalRuntimeOwnerMissing(
                    browser_context_id.clone(),
                )
            })?;
        let projection = self.selected_browser_context_projection();
        let replacement_inputs = BrowserEngineReplacementInputs::capture(self);
        let registration = self
            .browser_host_state
            .register_browser_context_with_runtime(
                browser_context_id.clone(),
                browser_context_handle,
                registration_metadata,
                target_topology,
                projection,
                renderer_runtime_owner,
                |renderer_runtime| replacement_inputs.create_engine(renderer_runtime),
            )
            .map_err(BrowserContextProjectionError::from)?;
        debug_assert_eq!(registration.browser_context_id(), browser_context_id);

        for (target_id, access) in registration.target_session_storage_accesses() {
            let bound = browser_context.bind_target_session_storage_access(access.clone());
            debug_assert!(
                bound,
                "prevalidated physical Target {target_id:?} must accept its exact Core sessionStorage access"
            );
        }

        // Context bootstrap/resnapshot is a current-state projection, not a
        // delayed replay of creation occurrences. Consume each exact Core
        // occurrence now; a later Target.setDiscoverTargets enumerates the
        // authoritative live topology independently.
        for target_id in registered_target_ids {
            let Some(page) = self
                .browser_host_state
                .navigation_owner()
                .capture_page_residence(&browser_context_id, &target_id)
            else {
                tracing::error!(
                    browser_context_id,
                    target_id,
                    "BrowserContext registration committed a Target without a Page fact identity"
                );
                continue;
            };
            match self.take_target_created_fact(&page) {
                Ok(projection) => tracing::trace!(
                    browser_fact_sequence = projection.envelope().sequence().get(),
                    browser_context_id,
                    target_id,
                    "consumed bootstrap Target creation fact before discovery resnapshot"
                ),
                Err(error) => tracing::error!(
                    %error,
                    browser_context_id,
                    target_id,
                    "BrowserContext Target committed without an exact frontend creation fact"
                ),
            }
        }

        let selected = if registration.is_selected() {
            self.browser_context = Some(browser_context);
            true
        } else {
            self.inactive_browser_contexts.push(browser_context);
            false
        };
        self.debug_assert_browser_context_topology_projection();
        Ok(selected)
    }

    pub(super) fn activate_browser_context_projection_by_id(
        &mut self,
        browser_context_id: &str,
    ) -> Result<bool, BrowserContextProjectionError> {
        self.validate_browser_context_topology_projection()?;
        if !self
            .browser_host_state
            .navigation_owner()
            .has_browser_context(browser_context_id)
        {
            return Err(
                BrowserContextRegistryError::UnknownBrowserContext(BrowserContextId::new(
                    browser_context_id,
                ))
                .into(),
            );
        }

        if self
            .browser_context
            .as_ref()
            .is_some_and(|context| context.id == browser_context_id)
        {
            let renderer_runtime = self
                .browser_context
                .as_ref()
                .map(BrowserContext::renderer_runtime_owner_access)
                .ok_or_else(|| {
                    BrowserContextProjectionError::PhysicalContextMissing(
                        browser_context_id.to_owned(),
                    )
                })?;
            let projection = self.selected_browser_context_projection();
            let replacement_inputs = BrowserEngineReplacementInputs::capture(self);
            let activation = self
                .browser_host_state
                .navigation_owner_mut()
                .activate_browser_context(browser_context_id, projection, || {
                    replacement_inputs.create_engine(renderer_runtime)
                })
                .map_err(BrowserContextProjectionError::Core)?;
            debug_assert!(!activation.changed());
            debug_assert_eq!(activation.browser_context_id(), browser_context_id);
            self.debug_assert_browser_context_topology_projection();
            return Ok(false);
        }

        let inactive_index = self
            .inactive_browser_contexts
            .iter()
            .position(|context| context.id == browser_context_id)
            .ok_or_else(|| {
                BrowserContextProjectionError::PhysicalContextMissing(browser_context_id.to_owned())
            })?;
        let renderer_runtime =
            self.inactive_browser_contexts[inactive_index].renderer_runtime_owner_access();
        let previous_browser_context_id = self
            .browser_context
            .as_ref()
            .map(|context| context.id.clone())
            .ok_or_else(|| {
                BrowserContextProjectionError::PhysicalContextMissing("<selected>".to_owned())
            })?;
        let projection = self.selected_browser_context_projection();
        let replacement_inputs = BrowserEngineReplacementInputs::capture(self);

        // Stage the physical swap before entering Core. The staged values are
        // restored at their exact slots when Core rejects the projection, so
        // a typed owner error cannot leave Protocol half-switched.
        let candidate = self.inactive_browser_contexts.remove(inactive_index);
        let Some(previous) = self.browser_context.take() else {
            self.inactive_browser_contexts
                .insert(inactive_index, candidate);
            return Err(BrowserContextProjectionError::PhysicalContextMissing(
                previous_browser_context_id,
            ));
        };
        let activation = {
            let mut browser_owner = self.browser_host_state.navigation_owner_mut();
            browser_owner.activate_browser_context(browser_context_id, projection, || {
                replacement_inputs.create_engine(renderer_runtime)
            })
        };
        let activation = match activation {
            Ok(activation) => activation,
            Err(error) => {
                self.browser_context = Some(previous);
                self.inactive_browser_contexts
                    .insert(inactive_index, candidate);
                return Err(BrowserContextProjectionError::Core(error));
            }
        };
        if !activation.changed() {
            self.browser_context = Some(previous);
            self.inactive_browser_contexts
                .insert(inactive_index, candidate);
            return Err(
                BrowserContextProjectionError::UnexpectedUnchangedActivation(
                    browser_context_id.to_owned(),
                ),
            );
        }
        debug_assert_eq!(
            activation.previous_browser_context_id(),
            previous_browser_context_id
        );
        debug_assert_eq!(activation.browser_context_id(), browser_context_id);
        self.browser_context = Some(candidate);
        self.inactive_browser_contexts.push(previous);
        self.debug_assert_browser_context_topology_projection();
        Ok(true)
    }

    #[cfg(test)]
    pub(super) fn remove_browser_context_projection_by_id(
        &mut self,
        browser_context_id: &str,
    ) -> Result<ProjectedBrowserContextRemoval, BrowserContextProjectionError> {
        self.validate_browser_context_topology_projection()?;
        let browser_context_handle = self
            .browser_context_by_id(browser_context_id)
            .ok_or_else(|| {
                BrowserContextProjectionError::PhysicalContextMissing(browser_context_id.to_owned())
            })?
            .browser_context_handle()
            .clone();
        let permit = self
            .browser_host_state
            .navigation_owner()
            .prepare_browser_context_removal_for_handle(&browser_context_handle)
            .map_err(BrowserContextProjectionError::Core)?;
        self.remove_browser_context_projection_with_permit(browser_context_id, permit)
    }

    /// Removes the exact physical Context already reserved by Browser Host.
    ///
    /// Selection and successor are intentionally resolved here, in the final
    /// owner turn. A frontend selection change that occurred while disposal
    /// participants were pending therefore cannot be overwritten by a stale
    /// restore decision captured at admission.
    pub(super) fn remove_browser_context_projection_for_disposal(
        &mut self,
        reservation: &BrowserContextDisposalReservation,
    ) -> Result<ProjectedBrowserContextRemoval, BrowserContextProjectionError> {
        self.validate_browser_context_topology_projection()?;
        let browser_context_id = reservation.browser_context_id();
        let physical_handle = self
            .browser_context_by_id(browser_context_id)
            .ok_or_else(|| {
                BrowserContextProjectionError::PhysicalContextMissing(browser_context_id.to_owned())
            })?
            .browser_context_handle();
        if physical_handle != reservation.browser_context_handle() {
            return Err(BrowserContextProjectionError::Core(
                BrowserContextRegistryError::BrowserContextHandleProjectionMismatch(
                    BrowserContextId::new(browser_context_id),
                ),
            ));
        }
        let permit = self
            .browser_host_state
            .navigation_owner()
            .prepare_browser_context_removal_for_disposal(reservation)
            .map_err(BrowserContextProjectionError::Core)?;
        self.remove_browser_context_projection_with_permit(browser_context_id, permit)
    }

    fn remove_browser_context_projection_with_permit(
        &mut self,
        browser_context_id: &str,
        permit: BrowserContextRemovalPermit,
    ) -> Result<ProjectedBrowserContextRemoval, BrowserContextProjectionError> {
        let projection = self.selected_browser_context_projection();

        if permit.was_selected() {
            if self
                .browser_context
                .as_ref()
                .map(|context| context.id.as_str())
                != Some(browser_context_id)
            {
                return Err(BrowserContextProjectionError::PhysicalSelectionMismatch {
                    authoritative: Some(browser_context_id.to_owned()),
                    projected: self
                        .browser_context
                        .as_ref()
                        .map(|context| context.id.clone()),
                });
            }
            if let Some(successor_browser_context_id) =
                permit.successor_browser_context_id().map(str::to_owned)
            {
                let successor_index = self
                    .inactive_browser_contexts
                    .iter()
                    .position(|context| context.id == successor_browser_context_id)
                    .ok_or_else(|| {
                        BrowserContextProjectionError::PhysicalContextMissing(
                            successor_browser_context_id.clone(),
                        )
                    })?;
                let successor = &self.inactive_browser_contexts[successor_index];
                let successor_renderer_runtime = successor.renderer_runtime_owner_access();
                let replacement_inputs = BrowserEngineReplacementInputs::capture(self);
                let successor = self.inactive_browser_contexts.remove(successor_index);
                let Some(removed) = self.browser_context.take() else {
                    self.inactive_browser_contexts
                        .insert(successor_index, successor);
                    return Err(BrowserContextProjectionError::PhysicalContextMissing(
                        browser_context_id.to_owned(),
                    ));
                };
                let removal = self
                    .browser_host_state
                    .commit_browser_context_removal_with_successor_runtime(
                        permit,
                        projection,
                        || replacement_inputs.create_engine(successor_renderer_runtime),
                    );
                let (mut removal, renderer_runtime_owner) = match removal {
                    Ok(removal) => removal,
                    Err(error) => {
                        self.browser_context = Some(removed);
                        self.inactive_browser_contexts
                            .insert(successor_index, successor);
                        return Err(error.into());
                    }
                };
                debug_assert_eq!(removal.browser_context_id(), browser_context_id);
                debug_assert_eq!(
                    removal.selected_browser_context_id(),
                    Some(successor_browser_context_id.as_str())
                );
                let retired_renderer_page_owners = removal.take_retired_renderer_page_owners();

                self.browser_context = Some(successor);
                self.debug_assert_browser_context_topology_projection();
                return Ok(ProjectedBrowserContextRemoval {
                    browser_context: removed,
                    selection_changed: true,
                    retired_renderer_page_owners,
                    renderer_runtime_owner,
                });
            }

            let Some(browser_context) = self.browser_context.take() else {
                return Err(BrowserContextProjectionError::PhysicalContextMissing(
                    browser_context_id.to_owned(),
                ));
            };
            let replacement_inputs = BrowserEngineReplacementInputs::capture(self);
            let removal = self
                .browser_host_state
                .commit_browser_context_removal_with_runtime(permit, projection, || {
                    replacement_inputs.create_unbound_engine()
                });
            let (mut removal, renderer_runtime_owner) = match removal {
                Ok(removal) => removal,
                Err(error) => {
                    self.browser_context = Some(browser_context);
                    return Err(error.into());
                }
            };
            debug_assert_eq!(removal.browser_context_id(), browser_context_id);
            debug_assert_eq!(removal.selected_browser_context_id(), None);
            let retired_renderer_page_owners = removal.take_retired_renderer_page_owners();
            self.debug_assert_browser_context_topology_projection();
            return Ok(ProjectedBrowserContextRemoval {
                browser_context,
                selection_changed: true,
                retired_renderer_page_owners,
                renderer_runtime_owner,
            });
        }

        let inactive_index = self
            .inactive_browser_contexts
            .iter()
            .position(|context| context.id == browser_context_id)
            .ok_or_else(|| {
                BrowserContextProjectionError::PhysicalContextMissing(browser_context_id.to_owned())
            })?;
        let browser_context = self.inactive_browser_contexts.remove(inactive_index);
        let replacement_inputs = BrowserEngineReplacementInputs::capture(self);
        let removal = self
            .browser_host_state
            .commit_browser_context_removal_with_runtime(permit, projection, || {
                replacement_inputs.create_unbound_engine()
            });
        let (mut removal, renderer_runtime_owner) = match removal {
            Ok(removal) => removal,
            Err(error) => {
                self.inactive_browser_contexts
                    .insert(inactive_index, browser_context);
                return Err(error.into());
            }
        };
        debug_assert_eq!(removal.browser_context_id(), browser_context_id);
        debug_assert_eq!(
            removal.selected_browser_context_id(),
            self.browser_context
                .as_ref()
                .map(|context| context.id.as_str())
        );
        let retired_renderer_page_owners = removal.take_retired_renderer_page_owners();
        self.debug_assert_browser_context_topology_projection();
        Ok(ProjectedBrowserContextRemoval {
            browser_context,
            selection_changed: false,
            retired_renderer_page_owners,
            renderer_runtime_owner,
        })
    }
}

#[cfg(test)]
mod tests;
