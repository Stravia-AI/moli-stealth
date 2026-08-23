use crate::browser_host::{
    BrowserContextHandle, BrowserPageOwnerKey, BrowserTargetHandle, PageResidenceIdentity,
};

use super::{
    BrowserContextRegistryError, BrowserInstanceId, BrowserNavigationOwner,
    BrowserPageResidenceRegistryError, BrowserTargetRegistryError, BrowserTargetResidence,
};

/// One exact top-level Target captured from Browser Core current state.
///
/// Metadata visible only to a frontend (title, URL, opener and attachment)
/// deliberately remains outside this value. The handles make a delayed
/// consumer unable to address a later Context, Target or Page slot that
/// reused the same public ids.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTargetStateSnapshot {
    browser_instance_id: BrowserInstanceId,
    browser_context: BrowserContextHandle,
    target: BrowserTargetHandle,
    page_residence: PageResidenceIdentity,
    residence: BrowserTargetResidence,
}

impl BrowserTargetStateSnapshot {
    pub fn browser_instance_id(&self) -> BrowserInstanceId {
        self.browser_instance_id
    }

    pub fn browser_context_id(&self) -> &str {
        self.browser_context.browser_context_id()
    }

    pub fn browser_context_handle(&self) -> &BrowserContextHandle {
        &self.browser_context
    }

    pub fn target_id(&self) -> &str {
        self.target.target_id()
    }

    pub fn target_handle(&self) -> &BrowserTargetHandle {
        &self.target
    }

    pub fn page_residence(&self) -> &PageResidenceIdentity {
        &self.page_residence
    }

    pub fn residence(&self) -> BrowserTargetResidence {
        self.residence
    }
}

/// One exact BrowserContext and its ordered top-level Target current state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserContextTargetSnapshot {
    browser_instance_id: BrowserInstanceId,
    browser_context: BrowserContextHandle,
    selected: bool,
    targets: Vec<BrowserTargetStateSnapshot>,
}

impl BrowserContextTargetSnapshot {
    pub fn browser_instance_id(&self) -> BrowserInstanceId {
        self.browser_instance_id
    }

    pub fn browser_context_id(&self) -> &str {
        self.browser_context.browser_context_id()
    }

    pub fn browser_context_handle(&self) -> &BrowserContextHandle {
        &self.browser_context
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    pub fn targets(&self) -> &[BrowserTargetStateSnapshot] {
        &self.targets
    }

    pub fn target(&self, target_id: &str) -> Option<&BrowserTargetStateSnapshot> {
        self.targets
            .iter()
            .find(|target| target.target_id() == target_id)
    }
}

/// Browser-owned current-state resnapshot for all live top-level Targets.
///
/// Contexts are ordered selected-first followed by Core's inactive order.
/// Targets are ordered active-first followed by Core's background order. This
/// is a resnapshot, not a replay of Target-created occurrences.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTopLevelTargetSnapshot {
    browser_instance_id: BrowserInstanceId,
    contexts: Vec<BrowserContextTargetSnapshot>,
}

impl BrowserTopLevelTargetSnapshot {
    pub fn browser_instance_id(&self) -> BrowserInstanceId {
        self.browser_instance_id
    }

    pub fn contexts(&self) -> &[BrowserContextTargetSnapshot] {
        &self.contexts
    }

    pub fn context(&self, browser_context_id: &str) -> Option<&BrowserContextTargetSnapshot> {
        self.contexts
            .iter()
            .find(|context| context.browser_context_id() == browser_context_id)
    }

    pub fn target(&self, target_id: &str) -> Option<&BrowserTargetStateSnapshot> {
        self.contexts
            .iter()
            .find_map(|context| context.target(target_id))
    }
}

impl BrowserNavigationOwner {
    /// Captures all live top-level Browser Target identities in one owner turn.
    pub fn snapshot_top_level_targets(
        &self,
    ) -> Result<BrowserTopLevelTargetSnapshot, BrowserTargetRegistryError> {
        let mut contexts = Vec::with_capacity(self.browser_context_count());
        for browser_context_id in self
            .browser_contexts
            .ordered_context_ids()
            .cloned()
            .collect::<Vec<_>>()
        {
            let Some(browser_context) = self.browser_contexts.handle(&browser_context_id).cloned()
            else {
                return Err(
                    BrowserContextRegistryError::UnknownBrowserContext(browser_context_id).into(),
                );
            };
            if !browser_context.is_live() {
                return Err(BrowserContextRegistryError::BrowserContextHandleNotLive(
                    browser_context_id,
                )
                .into());
            }

            let mut targets = Vec::new();
            for (target_id, residence) in self
                .targets
                .ordered_targets_for_context(&browser_context_id)?
            {
                let Some(target) = self.targets.handle_for_target(&target_id).cloned() else {
                    return Err(BrowserTargetRegistryError::UnknownTarget(target_id));
                };
                let Some(actual_context_id) = self.targets.context_for_target(&target_id) else {
                    return Err(BrowserTargetRegistryError::UnknownTarget(target_id));
                };
                if actual_context_id != &browser_context_id {
                    return Err(BrowserTargetRegistryError::TargetContextMismatch {
                        target_id,
                        expected: browser_context_id.clone(),
                        actual: actual_context_id.clone(),
                    });
                }
                if !target.is_live() {
                    return Err(BrowserTargetRegistryError::TargetHandleNotLive(target_id));
                }
                let owner =
                    BrowserPageOwnerKey::new(browser_context_id.as_str(), target.target_id());
                let page_residence = self
                    .page_residences
                    .identity(&self.target_runtimes, &owner)
                    .ok_or_else(|| {
                        BrowserPageResidenceRegistryError::UnknownTarget(owner.clone())
                    })?;
                targets.push(BrowserTargetStateSnapshot {
                    browser_instance_id: self.browser_instance_id,
                    browser_context: browser_context.clone(),
                    target,
                    page_residence,
                    residence,
                });
            }
            contexts.push(BrowserContextTargetSnapshot {
                browser_instance_id: self.browser_instance_id,
                browser_context,
                selected: self.browser_contexts.selected() == Some(&browser_context_id),
                targets,
            });
        }
        Ok(BrowserTopLevelTargetSnapshot {
            browser_instance_id: self.browser_instance_id,
            contexts,
        })
    }

    /// Checks whether a whole resnapshot originated from this Browser
    /// instance. This remains meaningful when the snapshot has no Context or
    /// Target entries whose exact handles could otherwise prove provenance.
    pub fn browser_top_level_target_snapshot_is_from_current_browser(
        &self,
        snapshot: &BrowserTopLevelTargetSnapshot,
    ) -> bool {
        snapshot.browser_instance_id == self.browser_instance_id
    }

    /// Checks whether a delayed Context snapshot still addresses the same
    /// Browser instance and exact live Context. Selection may have changed.
    pub fn browser_context_target_snapshot_is_current(
        &self,
        snapshot: &BrowserContextTargetSnapshot,
    ) -> bool {
        snapshot.browser_instance_id == self.browser_instance_id
            && self
                .browser_contexts
                .handle_is_current(&snapshot.browser_context)
    }

    /// Checks whether a delayed Target snapshot still addresses the same live
    /// Context, Target and Page slot. Document generation and active/background
    /// residence may advance without invalidating the stable slot identity.
    pub fn browser_target_state_snapshot_is_current(
        &self,
        snapshot: &BrowserTargetStateSnapshot,
    ) -> bool {
        if snapshot.browser_instance_id != self.browser_instance_id
            || !self
                .browser_contexts
                .handle_is_current(&snapshot.browser_context)
            || !self.targets.handle_is_current(&snapshot.target)
        {
            return false;
        }
        let owner = BrowserPageOwnerKey::new(snapshot.browser_context_id(), snapshot.target_id());
        self.targets.validate_target_owner(&owner).is_ok_and(|_| {
            self.page_owner_key_for_same_slot(&snapshot.page_residence)
                .as_ref()
                == Some(&owner)
        })
    }
}

#[cfg(test)]
mod tests;
