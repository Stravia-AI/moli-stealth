use std::collections::HashSet;

use moli_core::browser_host::{
    BrowserNavigationOwner, BrowserPageOwnerKey, BrowserPageResidenceHandle, BrowserTargetHandle,
    BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
};

use super::target_projection_error::TargetProjectionError;
use super::{BrowserContext, CdpConnection};

/// Exact physical fact projection for the Browser Core-owned top-level Target
/// topology. This module validates identity and capability equality; it never
/// mutates either registry.
impl CdpConnection {
    pub(super) fn browser_target_topology_projection(
        browser_context: &BrowserContext,
    ) -> BrowserTargetTopologyProjection {
        BrowserTargetTopologyProjection::new(
            browser_context.id.clone(),
            browser_context.active_target_handle().map(|target_handle| {
                BrowserTargetSlotProjection::new(
                    target_handle.clone(),
                    browser_context
                        .active_target
                        .runtime_slot
                        .page_residence_handle()
                        .clone(),
                )
            }),
            browser_context.background_targets.iter().map(|target| {
                BrowserTargetSlotProjection::new(
                    target.target_handle().clone(),
                    target.runtime_slot().page_residence_handle().clone(),
                )
            }),
        )
    }

    pub(crate) fn validate_browser_target_topology_projection(
        &self,
    ) -> Result<(), TargetProjectionError> {
        let browser_owner = self.browser_host_state.navigation_owner();
        let mut projected_target_ids = HashSet::new();
        let mut projected_target_count = 0;
        for browser_context in self.browser_contexts() {
            let browser_context_id = browser_context.id.as_str();
            let authoritative_active =
                browser_owner.active_target_id_for_browser_context(browser_context_id);
            let projected_active = browser_context.active_target_id();
            if authoritative_active != projected_active {
                return Err(TargetProjectionError::PhysicalActiveTargetMismatch {
                    browser_context_id: browser_context_id.to_owned(),
                    authoritative: authoritative_active.map(str::to_owned),
                    projected: projected_active.map(str::to_owned),
                });
            }

            let physical_count = usize::from(browser_context.has_active_target())
                + browser_context.background_targets.len();
            let authoritative_count =
                browser_owner.browser_context_target_count(browser_context_id);
            if authoritative_count != physical_count {
                return Err(TargetProjectionError::PhysicalContextTargetCountMismatch {
                    browser_context_id: browser_context_id.to_owned(),
                    authoritative: authoritative_count,
                    projected: physical_count,
                });
            }

            if let Some(target_handle) = browser_context.active_target_handle() {
                Self::validate_physical_target_projection(
                    &browser_owner,
                    browser_context_id,
                    target_handle,
                    browser_context
                        .active_target
                        .runtime_slot
                        .page_residence_handle(),
                )?;
                if !projected_target_ids.insert(target_handle.target_id().to_owned()) {
                    return Err(TargetProjectionError::DuplicatePhysicalTarget(
                        target_handle.target_id().to_owned(),
                    ));
                }
                projected_target_count += 1;
            }
            for target in &browser_context.background_targets {
                Self::validate_physical_target_projection(
                    &browser_owner,
                    browser_context_id,
                    target.target_handle(),
                    target.runtime_slot().page_residence_handle(),
                )?;
                if !projected_target_ids.insert(target.target_id().to_owned()) {
                    return Err(TargetProjectionError::DuplicatePhysicalTarget(
                        target.target_id().to_owned(),
                    ));
                }
                projected_target_count += 1;
            }
        }

        let authoritative_target_count = browser_owner.target_count();
        if authoritative_target_count != projected_target_count {
            return Err(TargetProjectionError::PhysicalTargetCountMismatch {
                authoritative: authoritative_target_count,
                projected: projected_target_count,
            });
        }
        Ok(())
    }

    fn validate_physical_target_projection(
        browser_owner: &BrowserNavigationOwner,
        browser_context_id: &str,
        target_handle: &BrowserTargetHandle,
        page_residence: &BrowserPageResidenceHandle,
    ) -> Result<(), TargetProjectionError> {
        let target_id = target_handle.target_id();
        let authoritative_context = browser_owner.target_browser_context_id(target_id);
        if authoritative_context != Some(browser_context_id) {
            return Err(TargetProjectionError::PhysicalTargetContextMismatch {
                target_id: target_id.to_owned(),
                authoritative: authoritative_context.map(str::to_owned),
                projected: browser_context_id.to_owned(),
            });
        }
        if !browser_owner.target_handle_is_current(target_handle) {
            return Err(TargetProjectionError::PhysicalTargetHandleMismatch {
                browser_context_id: browser_context_id.to_owned(),
                target_id: target_id.to_owned(),
            });
        }
        let page_owner = BrowserPageOwnerKey::new(browser_context_id, target_id);
        if !browser_owner.page_residence_handle_is_current(&page_owner, page_residence) {
            return Err(TargetProjectionError::PhysicalPageResidenceMismatch {
                browser_context_id: browser_context_id.to_owned(),
                target_id: target_id.to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn debug_assert_browser_target_topology_projection(&self) {
        let validation = self.validate_browser_target_topology_projection();
        debug_assert!(
            validation.is_ok(),
            "Browser Core and physical Target topology diverged: {validation:?}"
        );
    }
}
