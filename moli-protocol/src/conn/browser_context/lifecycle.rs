use super::registry_projection::BrowserContextProjectionError;
use super::*;
use moli_core::browser_host::{
    BrowserContextDisposalReservation, BrowserContextHandle, BrowserContextRegistrationMetadata,
    BrowserContextRegistryError,
};
use moli_core::runtime::RendererBrowserContextRuntimeOwner;

impl CdpConnection {
    #[cfg(test)]
    pub(crate) fn adopt_direct_browser_context_fixture_attachments(&mut self) {
        // Older protocol tests still construct physical BrowserContexts by
        // assigning the public fixture fields directly. Once Browser Core
        // became authoritative, adopting only their frontend attachment map
        // left those Contexts/Targets unknown to the engine registry. Route a
        // wholly unregistered fixture through the production registration
        // transaction before installing its legacy session attachments.
        if self.registered_browser_context_count() == 0
            && (self.browser_context.is_some() || !self.inactive_browser_contexts.is_empty())
        {
            let selected = self.browser_context.take();
            let inactive = std::mem::take(&mut self.inactive_browser_contexts);
            if let Some(selected) = selected {
                self.try_insert_browser_context(selected)
                    .expect("direct selected BrowserContext fixture must register in Core");
            }
            for browser_context in inactive {
                self.try_insert_browser_context(browser_context)
                    .expect("direct inactive BrowserContext fixture must register in Core");
            }
        }
        if let Some(browser_context) = self.browser_context.as_mut() {
            browser_context.adopt_background_target_fixture_attachments();
        }
        for browser_context in &mut self.inactive_browser_contexts {
            browser_context.adopt_background_target_fixture_attachments();
        }
    }

    pub(crate) fn begin_browser_context_disposal(
        &mut self,
        browser_context_handle: &BrowserContextHandle,
    ) -> Result<BrowserContextDisposalReservation, BrowserContextRegistryError> {
        self.browser_host_state
            .begin_browser_context_disposal(browser_context_handle)
    }

    pub(crate) fn rollback_browser_context_disposal(
        &mut self,
        reservation: BrowserContextDisposalReservation,
    ) -> bool {
        self.browser_host_state
            .rollback_browser_context_disposal(reservation)
    }

    pub(crate) fn try_activate_browser_context_by_id(
        &mut self,
        browser_context_id: &str,
    ) -> Result<bool, BrowserContextProjectionError> {
        let changed = self.activate_browser_context_projection_by_id(browser_context_id)?;
        if changed {
            self.apply_active_engine_fetch_overrides();
            self.invalidate_resource_runtime();
        }
        Ok(changed)
    }

    pub fn activate_browser_context_by_id(&mut self, browser_context_id: &str) -> bool {
        match self.try_activate_browser_context_by_id(browser_context_id) {
            Ok(_) => true,
            Err(error) => {
                if !error.is_unknown_context() {
                    tracing::warn!(
                        browser_context_id,
                        error = %error,
                        "BrowserContext activation projection was rejected"
                    );
                }
                false
            }
        }
    }

    pub async fn activate_browser_context_by_id_async(&mut self, browser_context_id: &str) -> bool {
        self.activate_browser_context_by_id(browser_context_id)
    }

    pub fn activate_browser_context_for_session(&mut self, session_id: &str) -> bool {
        let Some(route) = self.session_route(Some(session_id)) else {
            return false;
        };
        match route.browser_context_id() {
            Some(browser_context_id) => self.activate_browser_context_by_id(browser_context_id),
            None => true,
        }
    }

    pub async fn activate_browser_context_for_session_async(&mut self, session_id: &str) -> bool {
        self.activate_browser_context_for_session(session_id)
    }

    pub fn activate_browser_context_for_target(&mut self, target_id: &str) -> bool {
        let browser_context_id = self
            .browser_host_state
            .navigation_owner()
            .target_browser_context_id(target_id)
            .map(str::to_owned);
        if let Some(browser_context_id) = browser_context_id {
            return self.activate_browser_context_by_id(&browser_context_id);
        }
        self.activate_matching_browser_context(|bc| {
            bc.has_shared_worker_target(target_id)
                || bc.has_dedicated_worker_target(target_id)
                || bc.has_service_worker_target(target_id)
        })
    }

    pub async fn activate_browser_context_for_target_async(&mut self, target_id: &str) -> bool {
        self.activate_browser_context_for_target(target_id)
    }

    pub(crate) fn try_insert_browser_context(
        &mut self,
        browser_context: BrowserContext,
    ) -> Result<(), BrowserContextProjectionError> {
        self.try_insert_browser_context_with_metadata(
            browser_context,
            BrowserContextRegistrationMetadata::default(),
        )
    }

    pub(crate) fn try_insert_browser_context_with_metadata(
        &mut self,
        mut browser_context: BrowserContext,
        registration_metadata: BrowserContextRegistrationMetadata,
    ) -> Result<(), BrowserContextProjectionError> {
        #[cfg(test)]
        let fixture_browser_context_id = browser_context.id.clone();
        browser_context
            .adopt_browser_network_artifact_store(self.browser_host_state.network_artifacts());
        browser_context
            .renderer_runtime()
            .set_service_worker_pause_on_start_for_devtools(
                self.service_worker_pause_on_start_for_devtools(),
            );
        browser_context
            .renderer_runtime()
            .set_dedicated_worker_pause_on_start_for_devtools(
                self.dedicated_worker_pause_on_start_for_devtools(),
            );
        let became_active = self.register_browser_context_projection_with_metadata(
            browser_context,
            registration_metadata,
        )?;
        #[cfg(test)]
        self.browser_context_by_id_mut(&fixture_browser_context_id)
            .expect("newly registered test BrowserContext must remain projected")
            .adopt_background_target_fixture_attachments();
        if became_active {
            self.apply_active_engine_fetch_overrides();
        }
        Ok(())
    }

    pub fn insert_browser_context(&mut self, browser_context: BrowserContext) {
        let browser_context_id = browser_context.id.clone();
        if let Err(error) = self.try_insert_browser_context(browser_context) {
            tracing::warn!(
                browser_context_id,
                error = %error,
                "BrowserContext registration projection was rejected"
            );
        }
    }

    #[cfg(test)]
    pub(crate) async fn remove_browser_context_by_id_restoring_active_async(
        &mut self,
        browser_context_id: &str,
        restore_browser_context_id: Option<&str>,
    ) -> Result<String, BrowserContextProjectionError> {
        let removal = self.remove_browser_context_projection_by_id(browser_context_id)?;
        let (
            browser_context,
            selection_changed,
            retired_renderer_page_owners,
            mut renderer_runtime_owner,
        ) = removal.into_parts();
        let removed_browser_context_id = browser_context.id.clone();
        renderer_runtime_owner.terminate_renderer_producers_for_owner_shutdown();
        if selection_changed {
            self.invalidate_resource_runtime();
        }
        self.restore_preferred_browser_context_async(
            restore_browser_context_id,
            browser_context_id,
        )
        .await;
        if selection_changed {
            self.apply_active_engine_fetch_overrides();
        }
        for owner in retired_renderer_page_owners {
            let _ = owner.close_async().await;
        }
        drop(browser_context);
        renderer_runtime_owner.shutdown_network_and_join();
        Ok(removed_browser_context_id)
    }

    /// Commits the final physical removal for an exact Browser Host disposal.
    ///
    /// Unlike the legacy frontend wrapper, this path never restores a
    /// Context selection captured before participant waits. Core chooses the
    /// successor in this terminal turn and the physical projection follows
    /// that current decision.
    pub(crate) fn remove_browser_context_for_disposal(
        &mut self,
        reservation: &BrowserContextDisposalReservation,
    ) -> Result<
        (
            BrowserContext,
            Vec<moli_core::page::RendererPageLifetimeOwner>,
            RendererBrowserContextRuntimeOwner,
        ),
        BrowserContextProjectionError,
    > {
        let removal = self.remove_browser_context_projection_for_disposal(reservation)?;
        let (
            browser_context,
            selection_changed,
            retired_renderer_page_owners,
            renderer_runtime_owner,
        ) = removal.into_parts();
        if selection_changed {
            self.invalidate_resource_runtime();
            self.apply_active_engine_fetch_overrides();
        }
        Ok((
            browser_context,
            retired_renderer_page_owners,
            renderer_runtime_owner,
        ))
    }

    pub(crate) fn refresh_active_browser_context_loader(&mut self) {
        self.apply_active_engine_fetch_overrides();
        self.invalidate_resource_runtime();
    }

    fn activate_matching_browser_context<F>(&mut self, mut matches: F) -> bool
    where
        F: FnMut(&BrowserContext) -> bool,
    {
        let browser_context_id = if let Some(browser_context) = self
            .browser_context
            .as_ref()
            .filter(|browser_context| matches(browser_context))
        {
            browser_context.id.clone()
        } else {
            let Some(browser_context) = self
                .inactive_browser_contexts
                .iter()
                .find(|browser_context| matches(browser_context))
            else {
                return false;
            };
            browser_context.id.clone()
        };
        self.activate_browser_context_by_id(&browser_context_id)
    }

    #[cfg(test)]
    async fn restore_preferred_browser_context_async(
        &mut self,
        restore_browser_context_id: Option<&str>,
        removed_browser_context_id: &str,
    ) {
        let Some(restore_browser_context_id) = restore_browser_context_id else {
            return;
        };
        if restore_browser_context_id == removed_browser_context_id {
            return;
        }
        if self
            .browser_context
            .as_ref()
            .is_some_and(|bc| bc.id == restore_browser_context_id)
        {
            return;
        }
        let _ = self
            .activate_browser_context_by_id_async(restore_browser_context_id)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physically_matched_unregistered_context_is_rejected_without_panicking() {
        let mut connection = CdpConnection::new();
        connection.browser_context = Some(BrowserContext::new("physical-only".to_owned()));

        assert!(!connection.activate_matching_browser_context(|_| true));

        assert_eq!(
            connection
                .browser_context
                .as_ref()
                .map(|context| context.id.as_str()),
            Some("physical-only")
        );
        assert_eq!(connection.registered_browser_context_count(), 0);
        assert_eq!(
            connection
                .browser_host_state
                .navigation_owner()
                .selected_browser_context_id(),
            None
        );
    }
}
