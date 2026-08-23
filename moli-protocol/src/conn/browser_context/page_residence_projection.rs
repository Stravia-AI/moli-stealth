use moli_core::{
    browser_host::{BrowserNavigationFailure, BrowserPageOwnerKey, BrowserPageRuntimeOwner},
    page::{Page, RendererPageCreationArtifacts},
};
use url::Url;

use crate::conn::{
    CdpConnection, DocumentNavigationToken, InitialDocumentPageInstallResult,
    InitialDocumentPageOwner,
};

use super::page_residence_projection_error::PageResidenceProjectionError;

mod transaction;

/// Same-turn physical Page participant for Core-owned residence transitions.
///
/// Every fallible physical check completes before Core mutation. Projection
/// after a successful Core commit uses only the staged payload and the exact
/// shared Page capability proven by the permit. Transaction staging and exact
/// slot restoration live in the focused `transaction` module; this module only
/// orchestrates owner outcomes and asynchronous Page disposal.
impl CdpConnection {
    pub(crate) async fn install_initial_loaded_page_for_page_owner_async(
        &mut self,
        owner: &InitialDocumentPageOwner,
        page: Page,
        page_creation_artifacts: RendererPageCreationArtifacts,
    ) -> Result<InitialDocumentPageInstallResult, PageResidenceProjectionError> {
        let browser_owner = BrowserPageOwnerKey::new(&owner.browser_context_id, &owner.target_id);
        if let Err(error) = self.validate_browser_target_topology_projection() {
            let _ = page.close_async().await;
            return Err(error.into());
        }
        let Some(permit) = self
            .browser_host_state
            .navigation_owner()
            .prepare_initial_document_page_materialization(&browser_owner)
        else {
            let _ = page.close_async().await;
            return Ok(InitialDocumentPageInstallResult::Stale);
        };
        let Some(loader_id) = self
            .browser_host_state
            .navigation_owner()
            .target_initial_empty_document(&browser_owner)
            .map(|document| document.loader_id())
        else {
            let _ = page.close_async().await;
            return Ok(InitialDocumentPageInstallResult::Stale);
        };
        let staged = match self.stage_physical_page_residence_projection(&permit, true) {
            Ok(staged) => staged,
            Err(error) => {
                let _ = page.close_async().await;
                return Err(error);
            }
        };

        let mut page_runtime_owner = Some(BrowserPageRuntimeOwner::new(page));
        let mut renderer_page_owner = page_runtime_owner
            .as_mut()
            .and_then(BrowserPageRuntimeOwner::take_renderer_lifetime_owner);
        if renderer_page_owner.is_none() {
            staged.restore(self);
            if let Some(page) = page_runtime_owner
                .take()
                .and_then(BrowserPageRuntimeOwner::into_page)
            {
                let _ = page.close_async().await;
            }
            return Err(PageResidenceProjectionError::RendererPageOwnerMissing {
                browser_context_id: browser_owner.browser_context_id().to_owned(),
                target_id: browser_owner.target_id().to_owned(),
            });
        }

        let transition = {
            let mut browser_owner = self.browser_host_state.navigation_owner_mut();
            browser_owner.commit_initial_document_page_materialization(
                permit,
                &mut renderer_page_owner,
                &mut page_runtime_owner,
            )
        };
        let mut transition = match transition {
            Ok(transition) => transition,
            Err(error) => {
                staged.restore(self);
                tracing::debug!(
                    %error,
                    target_id = browser_owner.target_id(),
                    "prepared initial Document Page materialization became stale"
                );
                if let Some(owner) = renderer_page_owner.take() {
                    let detached_owner = match page_runtime_owner.as_mut() {
                        Some(runtime) => runtime.try_restore_renderer_lifetime_owner(owner).err(),
                        None => Some(owner),
                    };
                    if let Some(owner) = detached_owner {
                        let _ = owner.close_async().await;
                    }
                }
                if let Some(page) = page_runtime_owner
                    .take()
                    .and_then(BrowserPageRuntimeOwner::into_page)
                {
                    let _ = page.close_async().await;
                }
                return Ok(InitialDocumentPageInstallResult::Stale);
            }
        };
        staged.project_initial_document_after_browser_owner_commit(
            self,
            &browser_owner,
            page_creation_artifacts,
            loader_id,
            &transition,
        );
        if let Some(retired_owner) = transition.take_retired_renderer_page_owner() {
            tracing::warn!(
                target_id = browser_owner.target_id(),
                "initial Document materialization unexpectedly retired a renderer Page owner"
            );
            let _ = retired_owner.close_async().await;
        }
        debug_assert!(
            self.assert_target_materialized_initial_empty_document_has_page(&browser_owner)
                .is_ok(),
            "committed initial Document materialization must install its staged physical Page"
        );
        Ok(InitialDocumentPageInstallResult::Installed)
    }

    pub(crate) async fn discard_loaded_page_after_failed_navigation_for_session_owner_async(
        &mut self,
        session_id: Option<&str>,
        navigation: &DocumentNavigationToken,
        failure: BrowserNavigationFailure,
        final_url: &Url,
    ) -> Result<Option<()>, PageResidenceProjectionError> {
        let Some(owner) = self.target_page_owner_key_for_session(session_id) else {
            return Ok(None);
        };
        self.validate_browser_target_topology_projection()?;
        let projected_failure = failure.clone();
        let Some(permit) = self
            .browser_host_state
            .navigation_owner()
            .prepare_failed_navigation_page_discard(&owner, navigation, failure)
        else {
            return Ok(None);
        };
        let staged = self.stage_physical_page_residence_projection(&permit, false)?;
        let transition = {
            let mut browser_owner = self.browser_host_state.navigation_owner_mut();
            browser_owner.commit_failed_navigation_page_discard(permit)
        };
        let mut transition = match transition {
            Ok(transition) => transition,
            Err(error) => {
                staged.restore(self);
                tracing::debug!(
                    %error,
                    target_id = owner.target_id(),
                    "prepared failed-navigation Page discard became stale"
                );
                return Ok(None);
            }
        };
        let browser_fact_projected = match self
            .take_navigation_failure_fact(navigation, &projected_failure)
        {
            Ok(_) => true,
            Err(error) => {
                tracing::error!(
                    %error,
                    target_id = owner.target_id(),
                    loader_id = navigation.loader_id(),
                    "failed-navigation Page transition committed without an exact frontend Browser fact"
                );
                false
            }
        };
        staged.project_failed_navigation_after_browser_owner_commit(
            self,
            &owner,
            final_url,
            &transition,
        );
        if let Some(retired_owner) = transition.take_retired_renderer_page_owner() {
            let _ = retired_owner.close_async().await;
        }
        Ok(browser_fact_projected.then_some(()))
    }
}

#[cfg(test)]
mod tests;
