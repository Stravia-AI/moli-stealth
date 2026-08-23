use crate::{
    browser_host::{
        BrowserContextId, BrowserPageResidenceHandle, BrowserTargetHandle, BrowserTargetId,
        BrowserTargetSessionStorageAccess, PageResidenceIdentity,
    },
    page::RendererPageLifetimeOwner,
    runtime::NavigationEngine,
};

use super::{
    BrowserContextSelectionProjection, BrowserNavigationOwner, BrowserPageOwnerKey,
    BrowserTargetCreationMetadata, BrowserTargetEngineHandoff, BrowserTargetEngineHandoffOutcome,
    BrowserTargetRegistryError, BrowserTargetResidence, BrowserTargetTopologyProjection,
};

/// Result of registering one top-level Target in an existing BrowserContext.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTargetRegistration {
    owner: BrowserPageOwnerKey,
    handle: BrowserTargetHandle,
    page_residence: BrowserPageResidenceHandle,
    page_residence_identity: PageResidenceIdentity,
    residence: BrowserTargetResidence,
    previous_active_target_id: Option<BrowserTargetId>,
    engine_outcome: Option<BrowserTargetEngineHandoffOutcome>,
    session_storage_access: BrowserTargetSessionStorageAccess,
}

impl BrowserTargetRegistration {
    pub fn owner(&self) -> &BrowserPageOwnerKey {
        &self.owner
    }

    pub fn residence(&self) -> BrowserTargetResidence {
        self.residence
    }

    pub fn handle(&self) -> &BrowserTargetHandle {
        &self.handle
    }

    pub fn page_residence_handle(&self) -> &BrowserPageResidenceHandle {
        &self.page_residence
    }

    /// Exact initial Page-slot identity committed with this Target.
    pub fn page_residence_identity(&self) -> &PageResidenceIdentity {
        &self.page_residence_identity
    }

    pub fn previous_active_target_id(&self) -> Option<&str> {
        self.previous_active_target_id
            .as_ref()
            .map(BrowserTargetId::as_str)
    }

    pub fn engine_outcome(&self) -> Option<BrowserTargetEngineHandoffOutcome> {
        self.engine_outcome
    }

    pub fn session_storage_access(&self) -> &BrowserTargetSessionStorageAccess {
        &self.session_storage_access
    }
}

/// Result of selecting an already registered top-level Target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTargetActivation {
    owner: BrowserPageOwnerKey,
    previous_active_target_id: Option<BrowserTargetId>,
    changed: bool,
    engine_outcome: Option<BrowserTargetEngineHandoffOutcome>,
}

impl BrowserTargetActivation {
    pub fn owner(&self) -> &BrowserPageOwnerKey {
        &self.owner
    }

    pub fn previous_active_target_id(&self) -> Option<&str> {
        self.previous_active_target_id
            .as_ref()
            .map(BrowserTargetId::as_str)
    }

    pub fn changed(&self) -> bool {
        self.changed
    }

    pub fn engine_outcome(&self) -> Option<BrowserTargetEngineHandoffOutcome> {
        self.engine_outcome
    }
}

impl BrowserNavigationOwner {
    pub fn target_session_storage_access(
        &self,
        browser_context_id: &str,
        target_id: &str,
    ) -> Option<BrowserTargetSessionStorageAccess> {
        self.targets
            .session_storage_access(&BrowserPageOwnerKey::new(browser_context_id, target_id))
    }

    fn publish_target_created_after_registration(
        &mut self,
        registration: &BrowserTargetRegistration,
    ) {
        if let Err(error) = self.record_target_created_fact(
            registration.owner(),
            registration.page_residence_identity(),
        ) {
            tracing::error!(
                %error,
                browser_context_id = registration.owner().browser_context_id(),
                target_id = registration.owner().target_id(),
                "failed to publish top-level Target creation Browser fact"
            );
        }
    }

    pub fn register_background_target(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
        topology_projection: BrowserTargetTopologyProjection,
    ) -> Result<BrowserTargetRegistration, BrowserTargetRegistryError> {
        self.register_background_target_with_creation_metadata(
            browser_context_id,
            target_id,
            BrowserTargetCreationMetadata::default(),
            topology_projection,
        )
    }

    pub fn register_background_target_with_creation_metadata(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
        creation_metadata: BrowserTargetCreationMetadata,
        topology_projection: BrowserTargetTopologyProjection,
    ) -> Result<BrowserTargetRegistration, BrowserTargetRegistryError> {
        let browser_context_id = BrowserContextId::new(browser_context_id);
        let target_id = BrowserTargetId::new(target_id);
        if !self.browser_contexts.contains(&browser_context_id) {
            return Err(BrowserTargetRegistryError::UnknownBrowserContext(
                browser_context_id,
            ));
        }
        if !self
            .browser_contexts
            .accepts_owner_work(&browser_context_id)
        {
            return Err(super::BrowserContextRegistryError::BrowserContextDisposing(
                browser_context_id,
            )
            .into());
        }
        self.targets
            .validate_projection(&browser_context_id, &topology_projection)?;
        self.page_residences.validate_projection(
            &self.target_runtimes,
            &browser_context_id,
            &topology_projection,
        )?;
        let registration = self.targets.begin_background_registration(
            &browser_context_id,
            &target_id,
            creation_metadata.session_storage_store(),
        )?;
        let handle = registration.handle().clone();
        let session_storage_access = registration.session_storage_access().clone();
        let owner = BrowserPageOwnerKey::new(browser_context_id.as_str(), target_id.as_str());
        let page_registration = match self
            .page_residences
            .begin_target_registration(&mut self.target_runtimes, owner.clone())
        {
            Ok(registration) => registration,
            Err(error) => {
                let rolled_back = self.targets.rollback_target_registration(registration);
                debug_assert!(
                    rolled_back,
                    "same-turn Page registration rejection must restore background Target registration"
                );
                return Err(error.into());
            }
        };
        self.install_target_creation_metadata(&owner, &creation_metadata);
        let page_residence = self
            .page_residences
            .commit_target_registration(&mut self.target_runtimes, page_registration);
        self.targets.commit_target_registration(registration);
        let page_residence_identity = page_residence.identity(
            owner.browser_context_id().to_owned(),
            Some(owner.target_id().to_owned()),
        );
        let registration = BrowserTargetRegistration {
            owner,
            handle,
            page_residence,
            page_residence_identity,
            residence: BrowserTargetResidence::Background,
            previous_active_target_id: None,
            engine_outcome: None,
            session_storage_access,
        };
        self.publish_target_created_after_registration(&registration);
        Ok(registration)
    }

    pub fn register_active_target<F>(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
        topology_projection: BrowserTargetTopologyProjection,
        selection_projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserTargetRegistration, BrowserTargetRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.register_active_target_with_creation_metadata(
            browser_context_id,
            target_id,
            BrowserTargetCreationMetadata::default(),
            topology_projection,
            selection_projection,
            create_replacement,
        )
    }

    pub fn register_active_target_with_creation_metadata<F>(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
        creation_metadata: BrowserTargetCreationMetadata,
        topology_projection: BrowserTargetTopologyProjection,
        selection_projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserTargetRegistration, BrowserTargetRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        let browser_context_id = BrowserContextId::new(browser_context_id);
        let target_id = BrowserTargetId::new(target_id);
        self.validate_selected_target_transaction_context(
            &browser_context_id,
            &topology_projection,
            &selection_projection,
        )?;
        let owner = BrowserPageOwnerKey::new(browser_context_id.as_str(), target_id.as_str());
        let handoff = BrowserTargetEngineHandoff::new(
            selection_projection.target_engine().clone(),
            owner.clone(),
        )?;
        let selected_engine_owner = self.selected_target_engine_owner().cloned();
        let registration = self.targets.begin_active_registration(
            &browser_context_id,
            &target_id,
            creation_metadata.session_storage_store(),
        )?;
        let handle = registration.handle().clone();
        let session_storage_access = registration.session_storage_access().clone();
        let previous_active_target_id = registration.previous_active_target_id().cloned();
        let page_registration = match self
            .page_residences
            .begin_target_registration(&mut self.target_runtimes, owner.clone())
        {
            Ok(page_registration) => page_registration,
            Err(error) => {
                let rolled_back = self.targets.rollback_target_registration(registration);
                debug_assert!(
                    rolled_back,
                    "same-turn Page registration rejection must restore active Target registration"
                );
                return Err(error.into());
            }
        };
        let engine_outcome = match self.target_engines.handoff_target_engine(
            &mut self.target_runtimes,
            selected_engine_owner.as_ref(),
            handoff,
            create_replacement,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                let page_rolled_back = self
                    .page_residences
                    .rollback_target_registration(&mut self.target_runtimes, page_registration);
                let rolled_back = self.targets.rollback_target_registration(registration);
                debug_assert!(
                    page_rolled_back,
                    "same-turn active Target engine rejection must restore Page registration"
                );
                debug_assert!(
                    rolled_back,
                    "same-turn active Target registration rejection must restore topology"
                );
                return Err(error.into());
            }
        };
        self.install_target_creation_metadata(&owner, &creation_metadata);
        let page_residence = self
            .page_residences
            .commit_target_registration(&mut self.target_runtimes, page_registration);
        self.targets.commit_target_registration(registration);
        let page_residence_identity = page_residence.identity(
            owner.browser_context_id().to_owned(),
            Some(owner.target_id().to_owned()),
        );
        let registration = BrowserTargetRegistration {
            owner,
            handle,
            page_residence,
            page_residence_identity,
            residence: BrowserTargetResidence::Active,
            previous_active_target_id,
            engine_outcome: Some(engine_outcome),
            session_storage_access,
        };
        self.publish_target_created_after_registration(&registration);
        Ok(registration)
    }

    /// Replaces one exact active staging Target without demoting it. This is
    /// used when the browser's bootstrap placeholder becomes the first real
    /// top-level Target; ordinary live Target creation must use registration
    /// or activation instead.
    pub fn replace_active_target<F>(
        &mut self,
        browser_context_id: &str,
        expected_target_id: &str,
        replacement_target_id: &str,
        topology_projection: BrowserTargetTopologyProjection,
        selection_projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserTargetRegistration, BrowserTargetRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.replace_active_target_with_creation_metadata(
            browser_context_id,
            expected_target_id,
            replacement_target_id,
            BrowserTargetCreationMetadata::default(),
            topology_projection,
            selection_projection,
            create_replacement,
        )
    }

    pub fn replace_active_target_with_creation_metadata<F>(
        &mut self,
        browser_context_id: &str,
        expected_target_id: &str,
        replacement_target_id: &str,
        creation_metadata: BrowserTargetCreationMetadata,
        topology_projection: BrowserTargetTopologyProjection,
        selection_projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserTargetRegistration, BrowserTargetRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        let browser_context_id = BrowserContextId::new(browser_context_id);
        let expected_target_id = BrowserTargetId::new(expected_target_id);
        let replacement_target_id = BrowserTargetId::new(replacement_target_id);
        self.validate_selected_target_transaction_context(
            &browser_context_id,
            &topology_projection,
            &selection_projection,
        )?;
        let expected_owner =
            BrowserPageOwnerKey::new(browser_context_id.as_str(), expected_target_id.as_str());
        if self
            .page_residences
            .renderer_page_id_for_target(&self.target_runtimes, &expected_owner)
            .is_some()
        {
            return Err(BrowserTargetRegistryError::TargetHasCommittedRendererPage(
                expected_owner,
            ));
        }
        let owner =
            BrowserPageOwnerKey::new(browser_context_id.as_str(), replacement_target_id.as_str());
        let handoff = BrowserTargetEngineHandoff::new(
            selection_projection.target_engine().clone(),
            owner.clone(),
        )?;
        let selected_engine_owner = self.selected_target_engine_owner().cloned();
        let replacement = self.targets.begin_active_replacement(
            &browser_context_id,
            &expected_target_id,
            &replacement_target_id,
            creation_metadata.session_storage_store(),
        )?;
        let handle = replacement.replacement_handle().clone();
        let session_storage_access = replacement.replacement_session_storage_access().clone();
        let page_registration = match self
            .page_residences
            .begin_target_registration(&mut self.target_runtimes, owner.clone())
        {
            Ok(page_registration) => page_registration,
            Err(error) => {
                let rolled_back = self.targets.rollback_active_replacement(replacement);
                debug_assert!(
                    rolled_back,
                    "same-turn Page registration rejection must restore active Target replacement"
                );
                return Err(error.into());
            }
        };
        let engine_outcome = match self.target_engines.handoff_target_engine(
            &mut self.target_runtimes,
            selected_engine_owner.as_ref(),
            handoff,
            create_replacement,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                let page_rolled_back = self
                    .page_residences
                    .rollback_target_registration(&mut self.target_runtimes, page_registration);
                let rolled_back = self.targets.rollback_active_replacement(replacement);
                debug_assert!(
                    page_rolled_back,
                    "same-turn active Target replacement rejection must restore Page registration"
                );
                debug_assert!(
                    rolled_back,
                    "same-turn active Target replacement rejection must restore exact source"
                );
                return Err(error.into());
            }
        };
        let retired_renderer_page_owner = self.forget_target_runtime_state(&expected_owner);
        debug_assert!(
            retired_renderer_page_owner.is_none(),
            "prevalidated bootstrap placeholder replacement cannot retire a renderer Page"
        );
        self.install_target_creation_metadata(&owner, &creation_metadata);
        let page_residence = self
            .page_residences
            .commit_target_registration(&mut self.target_runtimes, page_registration);
        self.targets.commit_active_replacement(replacement);
        let page_residence_identity = page_residence.identity(
            owner.browser_context_id().to_owned(),
            Some(owner.target_id().to_owned()),
        );
        let registration = BrowserTargetRegistration {
            owner,
            handle,
            page_residence,
            page_residence_identity,
            residence: BrowserTargetResidence::Active,
            previous_active_target_id: Some(expected_target_id),
            engine_outcome: Some(engine_outcome),
            session_storage_access,
        };
        self.publish_target_created_after_registration(&registration);
        Ok(registration)
    }

    pub fn activate_target<F>(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
        topology_projection: BrowserTargetTopologyProjection,
        selection_projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<BrowserTargetActivation, BrowserTargetRegistryError>
    where
        F: FnOnce() -> NavigationEngine,
    {
        let browser_context_id = BrowserContextId::new(browser_context_id);
        self.validate_selected_target_transaction_context(
            &browser_context_id,
            &topology_projection,
            &selection_projection,
        )?;
        let owner = BrowserPageOwnerKey::new(browser_context_id.as_str(), target_id);
        let residence = self.targets.validate_target_owner(&owner)?;
        if residence == BrowserTargetResidence::Active {
            return Ok(BrowserTargetActivation {
                owner,
                previous_active_target_id: None,
                changed: false,
                engine_outcome: None,
            });
        }

        let handoff = BrowserTargetEngineHandoff::new(
            selection_projection.target_engine().clone(),
            owner.clone(),
        )?;
        let selected_engine_owner = self.selected_target_engine_owner().cloned();
        let activation = self.targets.begin_activation(&owner)?;
        let engine_outcome = match self.target_engines.handoff_target_engine(
            &mut self.target_runtimes,
            selected_engine_owner.as_ref(),
            handoff,
            create_replacement,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                let rolled_back = self.targets.rollback_activation(activation);
                debug_assert!(
                    rolled_back,
                    "same-turn Target activation rejection must restore exact background slot"
                );
                return Err(error.into());
            }
        };
        let previous_active_target_id = self.targets.commit_activation(activation);
        Ok(BrowserTargetActivation {
            owner,
            previous_active_target_id,
            changed: true,
            engine_outcome: Some(engine_outcome),
        })
    }

    /// Rolls back a Target that was registered as background staging but
    /// never became a committed live browsing context. Live Target closure
    /// must use the termination transaction instead.
    pub fn rollback_staged_background_target(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
        topology_projection: BrowserTargetTopologyProjection,
    ) -> Result<Option<RendererPageLifetimeOwner>, BrowserTargetRegistryError> {
        let browser_context_id = BrowserContextId::new(browser_context_id);
        self.targets
            .validate_projection(&browser_context_id, &topology_projection)?;
        self.page_residences.validate_projection(
            &self.target_runtimes,
            &browser_context_id,
            &topology_projection,
        )?;
        let owner = BrowserPageOwnerKey::new(browser_context_id.as_str(), target_id);
        if self.targets.validate_target_owner(&owner)? != BrowserTargetResidence::Background {
            return Err(BrowserTargetRegistryError::TargetIsNotBackground(owner));
        }
        self.targets.remove_target(&owner)?;
        Ok(self.forget_target_runtime_state(&owner))
    }

    fn validate_selected_target_transaction_context(
        &self,
        browser_context_id: &BrowserContextId,
        topology_projection: &BrowserTargetTopologyProjection,
        selection_projection: &BrowserContextSelectionProjection,
    ) -> Result<(), BrowserTargetRegistryError> {
        if !self.browser_contexts.contains(browser_context_id) {
            return Err(BrowserTargetRegistryError::UnknownBrowserContext(
                browser_context_id.clone(),
            ));
        }
        if !self.browser_contexts.accepts_owner_work(browser_context_id) {
            return Err(super::BrowserContextRegistryError::BrowserContextDisposing(
                browser_context_id.clone(),
            )
            .into());
        }
        self.browser_contexts
            .validate_projection(self.selected_target_engine_owner(), selection_projection)?;
        if self.browser_contexts.selected() != Some(browser_context_id) {
            return Err(BrowserTargetRegistryError::SelectedBrowserContextRequired {
                requested: browser_context_id.clone(),
                selected: self.browser_contexts.selected().cloned(),
            });
        }
        self.targets
            .validate_projection(browser_context_id, topology_projection)?;
        self.page_residences.validate_projection(
            &self.target_runtimes,
            browser_context_id,
            topology_projection,
        )?;
        self.browser_contexts.validate_selected_target(
            selection_projection,
            browser_context_id,
            self.targets
                .active_target(browser_context_id)
                .map(BrowserTargetId::as_str),
        )?;
        Ok(())
    }

    pub fn target_browser_context_id(&self, target_id: &str) -> Option<&str> {
        self.targets
            .context_for_target(&BrowserTargetId::new(target_id))
            .map(BrowserContextId::as_str)
    }

    pub fn active_target_id_for_browser_context(&self, browser_context_id: &str) -> Option<&str> {
        self.targets
            .active_target(&BrowserContextId::new(browser_context_id))
            .map(BrowserTargetId::as_str)
    }

    pub fn has_target(&self, target_id: &str) -> bool {
        self.targets
            .context_for_target(&BrowserTargetId::new(target_id))
            .is_some()
    }

    pub fn target_count(&self) -> usize {
        self.targets.target_count()
    }

    pub fn browser_context_target_count(&self, browser_context_id: &str) -> usize {
        self.targets
            .context_target_count(&BrowserContextId::new(browser_context_id))
    }
}

#[cfg(test)]
mod tests;
