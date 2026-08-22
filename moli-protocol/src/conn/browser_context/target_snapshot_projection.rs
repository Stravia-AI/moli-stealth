use moli_core::browser_host::{
    BrowserContextTargetSnapshot, BrowserPageOwnerKey, BrowserTargetStateSnapshot,
    BrowserTopLevelTargetSnapshot,
};

use crate::devtools_runtime::{DevToolsError, DevToolsTargetInfo};

use super::{BrowserContext, CdpConnection, TargetProjectionError};

/// Frontend projection for Browser Core-owned top-level Target current state.
///
/// Core decides which exact Context/Target/Page slots exist and their order.
/// This migration boundary joins those identities with physical metadata and
/// renderer-owned worker targets; it cannot add a physical page that Core did
/// not snapshot.
impl CdpConnection {
    pub(crate) fn browser_context_target_snapshot_is_current(
        &self,
        snapshot: &BrowserContextTargetSnapshot,
    ) -> bool {
        self.browser_host_state
            .navigation_owner()
            .browser_context_target_snapshot_is_current(snapshot)
    }

    pub(crate) fn browser_target_state_snapshot_is_current(
        &self,
        snapshot: &BrowserTargetStateSnapshot,
    ) -> bool {
        self.browser_host_state
            .navigation_owner()
            .browser_target_state_snapshot_is_current(snapshot)
    }

    pub(crate) fn capture_browser_top_level_target_snapshot(
        &self,
    ) -> Result<BrowserTopLevelTargetSnapshot, DevToolsError> {
        self.validate_browser_context_topology_projection()
            .map_err(DevToolsError::from)?;
        self.validate_browser_target_topology_projection()
            .map_err(DevToolsError::from)?;
        self.browser_host_state
            .navigation_owner()
            .snapshot_top_level_targets()
            .map_err(TargetProjectionError::Core)
            .map_err(DevToolsError::from)
    }

    fn physical_browser_context_for_snapshot(
        &self,
        snapshot: &BrowserContextTargetSnapshot,
    ) -> Result<&BrowserContext, TargetProjectionError> {
        if !self
            .browser_host_state
            .navigation_owner()
            .browser_context_target_snapshot_is_current(snapshot)
        {
            return Err(TargetProjectionError::StaleBrowserContextTargetSnapshot(
                snapshot.browser_context_id().to_owned(),
            ));
        }
        let browser_context = self
            .browser_context_by_id(snapshot.browser_context_id())
            .ok_or_else(|| {
                TargetProjectionError::PhysicalBrowserContextMissing(
                    snapshot.browser_context_id().to_owned(),
                )
            })?;
        if browser_context.browser_context_handle() != snapshot.browser_context_handle() {
            return Err(TargetProjectionError::PhysicalBrowserContextHandleMismatch(
                snapshot.browser_context_id().to_owned(),
            ));
        }
        Ok(browser_context)
    }

    pub(crate) fn project_top_level_target_snapshot(
        &self,
        snapshot: &BrowserTargetStateSnapshot,
    ) -> Result<DevToolsTargetInfo, DevToolsError> {
        if !self
            .browser_host_state
            .navigation_owner()
            .browser_target_state_snapshot_is_current(snapshot)
        {
            return Err(DevToolsError::from(
                TargetProjectionError::StaleTopLevelTargetSnapshot {
                    browser_context_id: snapshot.browser_context_id().to_owned(),
                    target_id: snapshot.target_id().to_owned(),
                },
            ));
        }
        let browser_context = self
            .browser_context_by_id(snapshot.browser_context_id())
            .ok_or_else(|| {
                DevToolsError::from(TargetProjectionError::PhysicalBrowserContextMissing(
                    snapshot.browser_context_id().to_owned(),
                ))
            })?;
        if browser_context.browser_context_handle() != snapshot.browser_context_handle() {
            return Err(DevToolsError::from(
                TargetProjectionError::PhysicalBrowserContextHandleMismatch(
                    snapshot.browser_context_id().to_owned(),
                ),
            ));
        }
        let target_id = snapshot.target_id();
        let physical_slot = if browser_context.active_target_id() == Some(target_id) {
            browser_context.active_target_handle().map(|target_handle| {
                (
                    target_handle,
                    browser_context
                        .active_target
                        .runtime_slot
                        .page_residence_handle(),
                )
            })
        } else {
            browser_context.background_target(target_id).map(|target| {
                (
                    target.target_handle(),
                    target.runtime_slot().page_residence_handle(),
                )
            })
        };
        let Some((physical_target_handle, physical_page_residence)) = physical_slot else {
            return Err(DevToolsError::from(
                TargetProjectionError::PhysicalTargetMissing {
                    browser_context_id: snapshot.browser_context_id().to_owned(),
                    target_id: target_id.to_owned(),
                },
            ));
        };
        if physical_target_handle != snapshot.target_handle() {
            return Err(DevToolsError::from(
                TargetProjectionError::PhysicalTargetHandleMismatch {
                    browser_context_id: snapshot.browser_context_id().to_owned(),
                    target_id: target_id.to_owned(),
                },
            ));
        }
        let owner = BrowserPageOwnerKey::new(snapshot.browser_context_id(), target_id);
        let physical_page = physical_page_residence.identity(
            snapshot.browser_context_id().to_owned(),
            Some(target_id.to_owned()),
        );
        if !self
            .browser_host_state
            .navigation_owner()
            .page_residence_handle_is_current(&owner, physical_page_residence)
            || !snapshot
                .page_residence()
                .same_residence_instance(&physical_page)
        {
            return Err(DevToolsError::from(
                TargetProjectionError::PhysicalPageResidenceMismatch {
                    browser_context_id: snapshot.browser_context_id().to_owned(),
                    target_id: target_id.to_owned(),
                },
            ));
        }
        browser_context
            .devtools_target_info(target_id)
            .ok_or_else(|| {
                DevToolsError::from(TargetProjectionError::PhysicalTargetMissing {
                    browser_context_id: snapshot.browser_context_id().to_owned(),
                    target_id: target_id.to_owned(),
                })
            })
    }

    pub(crate) fn project_devtools_target_infos_from_browser_snapshot(
        &self,
        snapshot: &BrowserTopLevelTargetSnapshot,
    ) -> Result<Vec<DevToolsTargetInfo>, DevToolsError> {
        if !self
            .browser_host_state
            .navigation_owner()
            .browser_top_level_target_snapshot_is_from_current_browser(snapshot)
        {
            return Err(DevToolsError::from(
                TargetProjectionError::ForeignBrowserTopLevelTargetSnapshot,
            ));
        }
        let mut infos = Vec::new();
        for context_snapshot in snapshot.contexts() {
            for target_snapshot in context_snapshot.targets() {
                let mut target_info = self.project_top_level_target_snapshot(target_snapshot)?;
                target_info.moli_popup_id = self
                    .browser_context_by_id(context_snapshot.browser_context_id())
                    .and_then(|context| context.target_popup_id(target_snapshot.target_id()));
                infos.push(target_info);
            }
            let browser_context = self
                .physical_browser_context_for_snapshot(context_snapshot)
                .map_err(DevToolsError::from)?;
            infos.extend(browser_context.devtools_worker_target_infos());
        }
        Ok(infos)
    }
}

#[cfg(test)]
mod tests;
