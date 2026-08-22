use moli_core::browser_host::{
    BrowserPageResidenceHandle, BrowserTargetCreationMetadata, BrowserTargetHandle,
    BrowserTargetRegistration, BrowserTargetResidence, BrowserTargetSessionStorageAccess,
};
use moli_core::page::RendererPageLifetimeOwner;

use crate::conn::{BackgroundTarget, BrowserTargetCreatedFactProjection};

use super::target_projection_error::TargetProjectionError;
use super::{BrowserContext, BrowserEngineReplacementInputs, CdpConnection};

#[derive(Debug)]
pub(in crate::conn) struct ProjectedTargetActivation {
    synchronize_loaded_page: bool,
}

/// Physical projection paired with the exact Browser topology occurrence that
/// authorizes a live frontend Target-created notification.
#[derive(Debug)]
pub(crate) struct ProjectedTargetRegistration {
    browser_fact: Option<BrowserTargetCreatedFactProjection>,
}

impl ProjectedTargetRegistration {
    pub(crate) fn into_browser_fact(self) -> Option<BrowserTargetCreatedFactProjection> {
        self.browser_fact
    }
}

impl ProjectedTargetActivation {
    pub(in crate::conn) fn synchronize_loaded_page(&self) -> bool {
        self.synchronize_loaded_page
    }
}

/// Same-turn physical projection for Browser Core-owned top-level Target
/// topology. DevTools session synchronization is deliberately not performed
/// here; callers may await it only after this projection has completed.
impl CdpConnection {
    #[cfg(test)]
    pub(crate) fn register_background_target_projection(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
        project: impl FnOnce(
            &mut BrowserContext,
            BrowserTargetHandle,
            BrowserPageResidenceHandle,
            BrowserTargetSessionStorageAccess,
        ),
    ) -> Result<(), TargetProjectionError> {
        self.register_background_target_with_creation_metadata_projection(
            browser_context_id,
            target_id,
            BrowserTargetCreationMetadata::default(),
            project,
        )
        .map(|_| ())
    }

    pub(crate) fn register_background_target_with_creation_metadata_projection(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
        creation_metadata: BrowserTargetCreationMetadata,
        project: impl FnOnce(
            &mut BrowserContext,
            BrowserTargetHandle,
            BrowserPageResidenceHandle,
            BrowserTargetSessionStorageAccess,
        ),
    ) -> Result<ProjectedTargetRegistration, TargetProjectionError> {
        self.validate_browser_target_topology_projection()?;
        let mut staged =
            self.take_physical_browser_context_for_target_projection(browser_context_id)?;
        let topology = Self::browser_target_topology_projection(&staged.browser_context);
        let registration = {
            let mut browser_owner = self.browser_host_state.navigation_owner_mut();
            browser_owner.register_background_target_with_creation_metadata(
                browser_context_id,
                target_id,
                creation_metadata,
                topology,
            )
        };
        let registration = match registration {
            Ok(registration) => registration,
            Err(error) => {
                self.restore_physical_browser_context_after_target_projection(staged);
                return Err(TargetProjectionError::Core(error));
            }
        };
        debug_assert_eq!(
            registration.owner().browser_context_id(),
            browser_context_id
        );
        debug_assert_eq!(registration.owner().target_id(), target_id);
        debug_assert_eq!(registration.residence(), BrowserTargetResidence::Background);
        project(
            &mut staged.browser_context,
            registration.handle().clone(),
            registration.page_residence_handle().clone(),
            registration.session_storage_access().clone(),
        );
        self.restore_physical_browser_context_after_target_projection(staged);
        self.debug_assert_browser_target_topology_projection();
        let browser_fact = self.claim_target_created_fact_for_registration(&registration);
        Ok(ProjectedTargetRegistration { browser_fact })
    }

    #[cfg(test)]
    pub(crate) fn register_active_target_projection(
        &mut self,
        target_id: &str,
        project: impl FnOnce(
            &mut BrowserContext,
            BrowserTargetHandle,
            BrowserPageResidenceHandle,
            BrowserTargetSessionStorageAccess,
        ),
    ) -> Result<(), TargetProjectionError> {
        self.register_active_target_with_creation_metadata_projection(
            target_id,
            BrowserTargetCreationMetadata::default(),
            project,
        )
        .map(|_| ())
    }

    pub(crate) fn register_active_target_with_creation_metadata_projection(
        &mut self,
        target_id: &str,
        creation_metadata: BrowserTargetCreationMetadata,
        project: impl FnOnce(
            &mut BrowserContext,
            BrowserTargetHandle,
            BrowserPageResidenceHandle,
            BrowserTargetSessionStorageAccess,
        ),
    ) -> Result<ProjectedTargetRegistration, TargetProjectionError> {
        self.validate_browser_target_topology_projection()?;
        let Some(browser_context) = self.browser_context.as_ref() else {
            return Err(TargetProjectionError::PhysicalBrowserContextMissing(
                "<selected>".to_owned(),
            ));
        };
        let browser_context_id = browser_context.id.clone();
        let topology = Self::browser_target_topology_projection(browser_context);
        let renderer_runtime = browser_context.renderer_runtime_owner_access();
        let selection = self.selected_browser_context_projection();
        let replacement_inputs = BrowserEngineReplacementInputs::capture(self);
        let mut staged =
            self.take_physical_browser_context_for_target_projection(&browser_context_id)?;
        let registration = {
            let mut browser_owner = self.browser_host_state.navigation_owner_mut();
            browser_owner.register_active_target_with_creation_metadata(
                &browser_context_id,
                target_id,
                creation_metadata,
                topology,
                selection,
                || replacement_inputs.create_engine(renderer_runtime),
            )
        };
        let registration = match registration {
            Ok(registration) => registration,
            Err(error) => {
                self.restore_physical_browser_context_after_target_projection(staged);
                return Err(TargetProjectionError::Core(error));
            }
        };
        debug_assert_eq!(
            registration.owner().browser_context_id(),
            browser_context_id
        );
        debug_assert_eq!(registration.owner().target_id(), target_id);
        debug_assert_eq!(registration.residence(), BrowserTargetResidence::Active);
        project(
            &mut staged.browser_context,
            registration.handle().clone(),
            registration.page_residence_handle().clone(),
            registration.session_storage_access().clone(),
        );
        self.restore_physical_browser_context_after_target_projection(staged);
        self.debug_assert_browser_target_topology_projection();
        let browser_fact = self.claim_target_created_fact_for_registration(&registration);
        Ok(ProjectedTargetRegistration { browser_fact })
    }

    #[cfg(test)]
    pub(crate) fn replace_active_target_projection(
        &mut self,
        expected_target_id: &str,
        replacement_target_id: &str,
        project: impl FnOnce(
            &mut BrowserContext,
            BrowserTargetHandle,
            BrowserPageResidenceHandle,
            BrowserTargetSessionStorageAccess,
        ),
    ) -> Result<(), TargetProjectionError> {
        self.replace_active_target_with_creation_metadata_projection(
            expected_target_id,
            replacement_target_id,
            BrowserTargetCreationMetadata::default(),
            project,
        )
        .map(|_| ())
    }

    pub(crate) fn replace_active_target_with_creation_metadata_projection(
        &mut self,
        expected_target_id: &str,
        replacement_target_id: &str,
        creation_metadata: BrowserTargetCreationMetadata,
        project: impl FnOnce(
            &mut BrowserContext,
            BrowserTargetHandle,
            BrowserPageResidenceHandle,
            BrowserTargetSessionStorageAccess,
        ),
    ) -> Result<ProjectedTargetRegistration, TargetProjectionError> {
        self.validate_browser_target_topology_projection()?;
        let Some(browser_context) = self.browser_context.as_ref() else {
            return Err(TargetProjectionError::PhysicalBrowserContextMissing(
                "<selected>".to_owned(),
            ));
        };
        let browser_context_id = browser_context.id.clone();
        let topology = Self::browser_target_topology_projection(browser_context);
        let renderer_runtime = browser_context.renderer_runtime_owner_access();
        let selection = self.selected_browser_context_projection();
        let replacement_inputs = BrowserEngineReplacementInputs::capture(self);
        let mut staged =
            self.take_physical_browser_context_for_target_projection(&browser_context_id)?;
        let registration = {
            let mut browser_owner = self.browser_host_state.navigation_owner_mut();
            browser_owner.replace_active_target_with_creation_metadata(
                &browser_context_id,
                expected_target_id,
                replacement_target_id,
                creation_metadata,
                topology,
                selection,
                || replacement_inputs.create_engine(renderer_runtime),
            )
        };
        let registration = match registration {
            Ok(registration) => registration,
            Err(error) => {
                self.restore_physical_browser_context_after_target_projection(staged);
                return Err(TargetProjectionError::Core(error));
            }
        };
        debug_assert_eq!(
            registration.owner().browser_context_id(),
            browser_context_id
        );
        debug_assert_eq!(registration.owner().target_id(), replacement_target_id);
        debug_assert_eq!(registration.residence(), BrowserTargetResidence::Active);
        debug_assert_eq!(
            registration.previous_active_target_id(),
            Some(expected_target_id)
        );
        project(
            &mut staged.browser_context,
            registration.handle().clone(),
            registration.page_residence_handle().clone(),
            registration.session_storage_access().clone(),
        );
        self.restore_physical_browser_context_after_target_projection(staged);
        self.debug_assert_browser_target_topology_projection();
        let browser_fact = self.claim_target_created_fact_for_registration(&registration);
        Ok(ProjectedTargetRegistration { browser_fact })
    }

    fn claim_target_created_fact_for_registration(
        &mut self,
        registration: &BrowserTargetRegistration,
    ) -> Option<BrowserTargetCreatedFactProjection> {
        match self.take_target_created_fact(registration.page_residence_identity()) {
            Ok(projection) => Some(projection),
            Err(error) => {
                tracing::error!(
                    %error,
                    browser_context_id = registration.owner().browser_context_id(),
                    target_id = registration.owner().target_id(),
                    "top-level Target registration committed without an exact frontend Browser fact"
                );
                None
            }
        }
    }

    pub(in crate::conn) fn activate_target_projection(
        &mut self,
        target_id: &str,
    ) -> Result<ProjectedTargetActivation, TargetProjectionError> {
        self.validate_browser_target_topology_projection()?;
        let Some(browser_context) = self.browser_context.as_ref() else {
            return Err(TargetProjectionError::PhysicalBrowserContextMissing(
                "<selected>".to_owned(),
            ));
        };
        let browser_context_id = browser_context.id.clone();
        let topology = Self::browser_target_topology_projection(browser_context);
        let renderer_runtime = browser_context.renderer_runtime_owner_access();
        let selection = self.selected_browser_context_projection();
        let replacement_inputs = BrowserEngineReplacementInputs::capture(self);
        let mut staged =
            self.take_physical_browser_context_for_target_projection(&browser_context_id)?;
        let physical_target_is_active = staged.browser_context.is_active_target(target_id);
        let synchronize_loaded_page = !staged.browser_context.has_pending_javascript_dialog();
        let staged_target = if physical_target_is_active {
            None
        } else {
            let Some(staged_target) = staged
                .browser_context
                .stage_background_target_slot_projection(target_id)
            else {
                self.restore_physical_browser_context_after_target_projection(staged);
                return Err(TargetProjectionError::PhysicalTargetMissing {
                    browser_context_id,
                    target_id: target_id.to_owned(),
                });
            };
            Some(staged_target)
        };

        let activation = {
            let mut browser_owner = self.browser_host_state.navigation_owner_mut();
            browser_owner.activate_target(
                &browser_context_id,
                target_id,
                topology,
                selection,
                || replacement_inputs.create_engine(renderer_runtime),
            )
        };
        let activation = match activation {
            Ok(activation) => activation,
            Err(error) => {
                if let Some(staged_target) = staged_target {
                    staged
                        .browser_context
                        .restore_staged_background_target_slot_projection(staged_target);
                }
                self.restore_physical_browser_context_after_target_projection(staged);
                return Err(TargetProjectionError::Core(error));
            }
        };
        debug_assert_eq!(activation.owner().browser_context_id(), browser_context_id);
        debug_assert_eq!(activation.owner().target_id(), target_id);

        debug_assert_eq!(
            activation.changed(),
            staged_target.is_some(),
            "validated Core and physical Target residence must produce the same activation outcome"
        );
        if let Some(staged_target) = staged_target {
            staged
                .browser_context
                .project_staged_background_target_to_active_slot_after_browser_owner_commit(
                    staged_target,
                );
        }

        let synchronize_loaded_page = activation.changed() && synchronize_loaded_page;
        self.restore_physical_browser_context_after_target_projection(staged);
        self.debug_assert_browser_target_topology_projection();
        Ok(ProjectedTargetActivation {
            synchronize_loaded_page,
        })
    }

    pub(crate) fn rollback_staged_background_target_projection(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
    ) -> Result<(BackgroundTarget, Option<RendererPageLifetimeOwner>), TargetProjectionError> {
        self.validate_browser_target_topology_projection()?;
        let mut staged =
            self.take_physical_browser_context_for_target_projection(browser_context_id)?;
        let topology = Self::browser_target_topology_projection(&staged.browser_context);
        let Some(index) = staged
            .browser_context
            .background_targets
            .iter()
            .position(|target| target.is_target(target_id))
        else {
            self.restore_physical_browser_context_after_target_projection(staged);
            return Err(TargetProjectionError::PhysicalTargetMissing {
                browser_context_id: browser_context_id.to_owned(),
                target_id: target_id.to_owned(),
            });
        };
        let target = staged.browser_context.background_targets.remove(index);
        let retired_renderer_page_owners = {
            let mut browser_owner = self.browser_host_state.navigation_owner_mut();
            browser_owner.rollback_staged_background_target(browser_context_id, target_id, topology)
        };
        let retired_renderer_page_owners = match retired_renderer_page_owners {
            Ok(owners) => owners,
            Err(error) => {
                staged
                    .browser_context
                    .background_targets
                    .insert(index, target);
                self.restore_physical_browser_context_after_target_projection(staged);
                return Err(TargetProjectionError::Core(error));
            }
        };
        self.restore_physical_browser_context_after_target_projection(staged);
        self.debug_assert_browser_target_topology_projection();
        Ok((target, retired_renderer_page_owners))
    }
}

#[cfg(test)]
mod tests;
