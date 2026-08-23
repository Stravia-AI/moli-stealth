#[cfg(test)]
use moli_core::browser_host::{BrowserInitialEmptyDocumentSeed, BrowserTargetRegistryError};
use moli_core::browser_host::{BrowserInitialEmptyDocumentSnapshot, BrowserPageOwnerKey};

use super::CdpConnection;

/// Projection adapters from frontend/session routing to Browser Core's
/// initial-empty-Document authority.
///
/// Production creation metadata is installed by the Core Target or
/// BrowserContext registration transaction. The physical BrowserContext is
/// consulted here only for Page presence; metadata, loader identity, and
/// lifecycle flags are never mirrored into protocol parking state.
impl CdpConnection {
    #[cfg(test)]
    pub(crate) fn register_target_initial_empty_document_for_test(
        &mut self,
        owner: &BrowserPageOwnerKey,
        seed: BrowserInitialEmptyDocumentSeed,
    ) -> Result<(), BrowserTargetRegistryError> {
        self.browser_host_state
            .navigation_owner_mut()
            .register_target_initial_empty_document(owner, seed)
    }

    pub(crate) fn target_initial_empty_document_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<BrowserInitialEmptyDocumentSnapshot> {
        let owner = self.target_page_owner_key_for_session(session_id)?;
        self.browser_host_state
            .navigation_owner()
            .target_initial_empty_document(&owner)
    }

    #[cfg(test)]
    pub(crate) fn target_initial_empty_document_for_owner(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<BrowserInitialEmptyDocumentSnapshot> {
        self.browser_host_state
            .navigation_owner()
            .target_initial_empty_document(owner)
    }

    pub(crate) fn mark_target_initial_empty_document_exited_for_session_owner(
        &mut self,
        session_id: Option<&str>,
    ) -> bool {
        let Some(owner) = self.target_page_owner_key_for_session(session_id) else {
            return false;
        };
        self.browser_host_state
            .navigation_owner_mut()
            .mark_target_initial_empty_document_exited(&owner);
        true
    }

    pub(crate) fn can_install_target_initial_empty_document_page(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> bool {
        if !self
            .browser_host_state
            .navigation_owner()
            .can_install_target_initial_empty_document_page(owner)
        {
            return false;
        }
        let Some(browser_context) = self.browser_context_by_id(owner.browser_context_id()) else {
            return false;
        };
        if browser_context.active_target_id() == Some(owner.target_id()) {
            return !browser_context.has_loaded_page();
        }
        browser_context
            .background_target(owner.target_id())
            .is_some_and(|target| !target.has_loaded_page())
    }

    pub(crate) fn assert_target_materialized_initial_empty_document_has_page(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> Result<(), String> {
        let materialized_current = self
            .browser_host_state
            .navigation_owner()
            .target_initial_empty_document(owner)
            .is_some_and(|state| state.is_on_initial_empty_document() && state.materialized());
        if !materialized_current {
            return Ok(());
        }
        let has_loaded_page = self
            .browser_context_by_id(owner.browser_context_id())
            .is_some_and(|browser_context| {
                if browser_context.active_target_id() == Some(owner.target_id()) {
                    return browser_context.has_loaded_page();
                }
                browser_context
                    .background_target(owner.target_id())
                    .is_some_and(|target| target.has_loaded_page())
            });
        if has_loaded_page {
            return Ok(());
        }
        Err(format!(
            "TargetInitialEmptyDocumentMissingPage: target {} has materialized current initial empty document without loaded Page",
            owner.target_id()
        ))
    }
}
