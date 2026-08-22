#[cfg(test)]
use super::super::state::TargetPageAbsenceReason;
use super::super::state::{
    CommittedRendererAgentAttachment, PreparedRendererAgentAttachment, TargetPageAttachmentId,
    prepare_renderer_call_replacements_for_devtools_sessions, runtime_bindings_for_renderer,
};
use super::super::{
    BackgroundTarget, BrowserContext, RendererPageResidenceIdentity, TargetRuntimeSlot,
};
use moli_core::{
    browser_host::{
        BrowserPageReplacement, BrowserPageResidenceTransition, BrowserPageRuntimeLease,
        BrowserPageRuntimeOwner,
    },
    page::Page,
};

pub(crate) enum LoadedNavigationRendererAttachmentCommit {
    Prepare(Option<PreparedRendererAgentAttachment>),
    AlreadyCommitted(CommittedRendererAgentAttachment),
}

/// Renderer/protocol participant state prepared before Browser Core commits
/// an authoritative Page replacement.
pub(crate) struct PreparedLoadedNavigationPageCommit {
    page: Page,
    retiring_renderer_page: Option<RendererPageResidenceIdentity>,
}

impl PreparedLoadedNavigationPageCommit {
    pub(crate) fn new(
        page: Page,
        retiring_renderer_page: Option<RendererPageResidenceIdentity>,
    ) -> Self {
        Self {
            page,
            retiring_renderer_page,
        }
    }

    pub(crate) fn retiring_renderer_page(&self) -> Option<RendererPageResidenceIdentity> {
        self.retiring_renderer_page
    }

    pub(crate) fn into_page_runtime_owner(self) -> BrowserPageRuntimeOwner {
        BrowserPageRuntimeOwner::new(self.page)
    }
}

impl BrowserContext {
    async fn close_page_best_effort(page: Page) {
        let _ = page.close_async().await;
    }

    pub(crate) fn loaded_page(&self) -> Option<BrowserPageRuntimeLease> {
        self.active_target.runtime_slot.loaded_page()
    }

    pub(crate) fn has_loaded_page(&self) -> bool {
        self.active_target.runtime_slot.has_loaded_page()
    }

    pub(crate) fn page_attachment_id(&self) -> Option<TargetPageAttachmentId> {
        self.active_target.runtime_slot.page_attachment_id()
    }

    pub(crate) fn clear_active_target_session_scoped_state_fields(&mut self) {
        {
            let (primary, auxiliary) = self.devtools_session_states_mut();
            let retained_runtime_bindings = runtime_bindings_for_renderer(primary, auxiliary);
            primary.page_session_state.page_lifecycle_events = false;
            *primary = Default::default();
            // Auxiliary map membership is the exact attachment route. Reset
            // session-scoped contents without deleting those still-live
            // routes; detach owns structural removal.
            for state in auxiliary.values_mut() {
                *state = Default::default();
            }
            primary.runtime_bindings = retained_runtime_bindings;
            primary.page_session_state.log_enabled = false;
            primary.console_output_session_state.console_enabled = false;
            primary.page_session_state.performance.disable();
            primary.page_session_state.page_bypass_csp_enabled = false;
            primary.page_session_state.page_font_families.clear();
            primary
                .page_session_state
                .page_file_chooser_opened_event_enabled = false;
            primary
                .page_session_state
                .page_intercept_file_chooser_dialog_enabled = false;
        }
        self.active_target
            .runtime_slot
            .disable_primary_network_events();
        self.network_policy.clear_session_scoped_overrides();
        self.tls_verify_host_override = None;
        self.http_proxy_override = None;
        self.http_no_proxy_override = None;
        self.locale_override = None;
        self.timezone_override = None;
        self.network_conditions = None;
        self.geolocation_override = None;
        self.emulated_media = Default::default();
        self.emulated_device_metrics = None;
        self.cpu_throttling_rate = 1.0;
        self.touch_emulation_enabled = false;
        self.emit_touch_events_for_mouse = false;
        self.focus_emulation_enabled = false;
        self.script_execution_disabled = false;
        self.css_enabled = false;
        self.active_target.fetch_owner.reset_config();
        self.clear_pending_fetch_state();
        self.clear_session_scoped_network_observation_artifacts();
        self.active_target
            .runtime_slot
            .request_id_allocator()
            .reset_fetch_navigation_request_counter();
        self.active_target
            .owner_state
            .clear_observable_output_state();
    }

    pub(crate) fn clear_active_target_loaded_document_session_state(&mut self) {
        let (primary, auxiliary) = self.devtools_session_states_mut();
        primary
            .page_session_state
            .clear_loaded_document_context_state();
        for state in auxiliary.values_mut() {
            state
                .page_session_state
                .clear_loaded_document_context_state();
        }
    }

    pub(crate) fn clear_active_target_runtime_remote_object_tracking(&mut self) {
        let (primary, auxiliary) = self.devtools_session_states_mut();
        primary.clear_runtime_remote_object_tracking();
        for state in auxiliary.values_mut() {
            state.clear_runtime_remote_object_tracking();
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_loaded_page(&mut self, page: Option<Page>) -> Option<Page> {
        let previous = self.active_target.runtime_slot.replace_loaded_page(page);
        self.ingest_active_target_output_updates();
        self.active_target
            .owner_state
            .clear_loaded_document_context_state();
        self.clear_active_target_loaded_document_session_state();
        previous
    }

    #[cfg(test)]
    pub(crate) fn clear_loaded_page_with_reason(
        &mut self,
        reason: TargetPageAbsenceReason,
    ) -> Option<Page> {
        let previous = self
            .active_target
            .runtime_slot
            .clear_loaded_page_with_reason(reason);
        self.ingest_active_target_output_updates();
        self.active_target
            .owner_state
            .clear_loaded_document_context_state();
        self.clear_active_target_loaded_document_session_state();
        previous
    }

    #[cfg(test)]
    pub(crate) async fn set_loaded_page_async(&mut self, mut page: Page) {
        // BrowserContext owns document-cookie facade overrides for the active
        // browsing context. New pages should inherit the current browser
        // policy surface before any JS observes `document.cookie` or
        // `navigator.cookieEnabled`.
        self.document_cookie_manager_surface
            .apply_to_page_async(&mut page)
            .await;
        let _ = self.replace_loaded_page(Some(page));
    }

    #[cfg(test)]
    pub(crate) fn clear_loaded_page(&mut self) -> bool {
        self.clear_loaded_page_with_reason(TargetPageAbsenceReason::TestFixture)
            .is_some()
    }

    pub(crate) fn ingest_active_target_output_updates(&mut self) -> bool {
        self.active_target
            .runtime_slot
            .ingest_owner_page_observable_output_updates()
    }

    #[cfg(test)]
    async fn close_loaded_page_async(&mut self) -> bool {
        let page = self
            .active_target
            .runtime_slot
            .retire_page_projection_after_browser_owner_forget();
        let had_page = page.is_some();
        if let Some(page) = page {
            Self::close_page_best_effort(page).await;
        }
        had_page
    }

    pub(crate) fn prepare_loaded_navigation_page_commit(
        &mut self,
        mut page: Page,
        renderer_attachment_commit: LoadedNavigationRendererAttachmentCommit,
    ) -> anyhow::Result<PreparedLoadedNavigationPageCommit> {
        let retiring_renderer_page = self
            .active_target
            .runtime_slot
            .loaded_renderer_page_residence();
        let primary_session_id = self.active_session_id_owned();
        let previous_attachment = match renderer_attachment_commit {
            LoadedNavigationRendererAttachmentCommit::Prepare(renderer_agent_candidate) => self
                .active_target
                .runtime_slot
                .commit_loaded_navigation_renderer_attachment(
                    &mut page,
                    renderer_agent_candidate,
                )?,
            LoadedNavigationRendererAttachmentCommit::AlreadyCommitted(transaction) => {
                self.active_target
                    .runtime_slot
                    .bind_page_to_committed_renderer_agent_candidate(&mut page, &transaction)?;
                transaction.previous()
            }
        };
        let new_attachment_id = page
            .renderer_agent_attachment_id()
            .expect("committed navigation Page must have a renderer attachment");
        if let Some(previous_attachment) = previous_attachment
            && previous_attachment.id() != new_attachment_id
        {
            let (primary, auxiliary) = self.devtools_session_states_mut();
            let replacements = prepare_renderer_call_replacements_for_devtools_sessions(
                primary_session_id.as_deref(),
                primary,
                auxiliary,
                previous_attachment.id(),
                new_attachment_id,
            )?;
            self.active_target
                .runtime_slot
                .install_pending_renderer_call_replacements(replacements);
        }
        Ok(PreparedLoadedNavigationPageCommit::new(
            page,
            retiring_renderer_page,
        ))
    }

    pub(crate) fn project_loaded_navigation_page_after_browser_owner_commit(
        &mut self,
        replacement: &BrowserPageReplacement,
        retiring_renderer_page: Option<RendererPageResidenceIdentity>,
    ) {
        self.active_target
            .runtime_slot
            .project_loaded_page_after_browser_owner_commit(replacement, retiring_renderer_page);
        self.ingest_active_target_output_updates();
        self.active_target
            .owner_state
            .clear_loaded_document_context_state();
        self.clear_active_target_loaded_document_session_state();
        self.reset_subresource_network_cursor();
        self.clear_websocket_network_artifacts();
        self.active_target
            .owner_state
            .clear_committed_document_navigation_state();
        self.clear_active_target_runtime_remote_object_tracking();
    }

    pub(crate) fn project_initial_document_page_after_browser_owner_commit(
        &mut self,
        transition: &BrowserPageResidenceTransition,
    ) {
        self.active_target
            .runtime_slot
            .project_initial_document_page_after_browser_owner_commit(transition);
        self.ingest_active_target_output_updates();
        self.active_target
            .owner_state
            .clear_loaded_document_context_state();
        self.clear_active_target_loaded_document_session_state();
    }

    pub(crate) fn project_failed_navigation_page_absence_after_browser_owner_commit(
        &mut self,
        transition: &BrowserPageResidenceTransition,
    ) -> Option<Page> {
        let previous = self
            .active_target
            .runtime_slot
            .project_failed_navigation_page_absence_after_browser_owner_commit(transition);
        self.ingest_active_target_output_updates();
        self.active_target
            .owner_state
            .clear_loaded_document_context_state();
        self.clear_active_target_loaded_document_session_state();
        previous
    }

    pub(crate) async fn clear_active_target_session_scoped_state_async(
        &mut self,
    ) -> Result<(), String> {
        self.clear_active_target_session_scoped_state_fields();
        let emulated_media: moli_core::page::EmulatedMediaOverrides = (&self.emulated_media).into();
        if let Some(mut page) = self.active_target.runtime_slot.loaded_page_mut() {
            page.set_extra_http_headers_async(&[])
                .await
                .map_err(|error| format!("failed to clear page extra headers: {error}"))?;
            page.set_network_offline_async(false)
                .await
                .map_err(|error| format!("failed to clear page offline state: {error}"))?;
            page.set_bypass_service_worker_async(false)
                .await
                .map_err(|error| format!("failed to clear page service worker bypass: {error}"))?;
            page.set_blocked_url_patterns_async(&[])
                .await
                .map_err(|error| format!("failed to clear page blocked URLs: {error}"))?;
            page.set_script_execution_disabled_async(false)
                .await
                .map_err(|error| {
                    format!("failed to clear page script execution disabled state: {error}")
                })?;
            page.set_cpu_throttling_rate_async(1.0)
                .await
                .map_err(|error| format!("failed to clear page CPU throttling rate: {error}"))?;
            page.set_emulated_media_async(&emulated_media)
                .await
                .map_err(|error| format!("failed to clear page emulated media: {error}"))?;
        }
        self.apply_surface_overrides_to_loaded_page_async().await?;
        Ok(())
    }

    pub(crate) fn clear_active_target_session_scoped_state_without_loaded_page(&mut self) {
        self.clear_active_target_session_scoped_state_fields();
    }

    #[cfg(test)]
    pub(crate) async fn reset_active_target_slot_to_empty_async(&mut self) {
        self.clear_active_target_session_scoped_state_fields();
        self.active_target.owner_state.target_crash_state.clear();
        if let Some(target_id) = self.active_target_id_owned() {
            self.forget_target_opener_references_for_target(&target_id);
            self.forget_target_window_names_for_target(&target_id);
            self.forget_target_popup_id_for_target(&target_id);
        }
        self.detach_active_session();
        self.clear_active_target_id();
        self.clear_renderer_document_protocol_state_for_active_target();
        self.close_loaded_page_async().await;
        self.clear_pending_fetch_state();
        self.active_target.owner_state.clear_page_local_state();
        self.restore_raw_cookie_manager_surface_async(Default::default())
            .await;
        self.reset_target_identity_to_new_tab();
        self.reset_target_scoped_network_artifacts();
        self.active_target
            .owner_state
            .clear_observable_output_state();
        self.active_target
            .runtime_slot
            .request_id_allocator()
            .reset_subresource_fetch_request_counter();
    }

    /// Transfers any residual renderer Pages after authoritative whole-
    /// Context removal.
    ///
    /// A correct disposal chain has already closed every registered Target,
    /// so this is normally empty. Returning concrete Pages lets Browser Host
    /// expose real cleanup participants if physical projection drift left a
    /// residual Page, without awaiting an always-ready compatibility drain.
    pub(crate) fn take_residual_pages_for_browser_context_disposal(&mut self) -> Vec<Page> {
        let mut pages = Vec::new();
        if let Some(page) = self
            .active_target
            .runtime_slot
            .retire_page_projection_after_browser_owner_forget()
        {
            pages.push(page);
        }
        for target in &mut self.background_targets {
            if let Some(page) = target
                .runtime_slot
                .retire_page_projection_after_browser_owner_forget()
            {
                pages.push(page);
            }
        }
        pages
    }
}

impl BackgroundTarget {
    pub(crate) fn target_url(&self) -> &str {
        self.target_identity.url()
    }

    pub(crate) fn set_target_url(&mut self, url: String) {
        self.target_identity.set_url(url);
    }

    pub(crate) fn set_target_security_origin(&mut self, security_origin: String) {
        self.target_identity.set_security_origin(security_origin);
    }

    pub(crate) fn set_target_secure_context_type(&mut self, secure_context_type: String) {
        self.target_identity
            .set_secure_context_type(secure_context_type);
    }

    pub(crate) fn target_identity(&self) -> &super::super::TargetIdentityState {
        &self.target_identity
    }

    pub(crate) fn runtime_slot(&self) -> &TargetRuntimeSlot {
        &self.runtime_slot
    }

    pub(crate) fn loaded_page(&self) -> Option<BrowserPageRuntimeLease> {
        self.runtime_slot.loaded_page()
    }

    pub(crate) fn loaded_page_mut(&mut self) -> Option<BrowserPageRuntimeLease> {
        self.runtime_slot.loaded_page_mut()
    }

    pub(crate) fn has_loaded_page(&self) -> bool {
        self.loaded_page().is_some()
    }

    #[cfg(test)]
    pub(crate) fn replace_loaded_page(&mut self, page: Option<Page>) -> Option<Page> {
        let previous = self.runtime_slot.replace_loaded_page(page);
        self.runtime_slot
            .ingest_owner_page_observable_output_updates();
        previous
    }

    pub(crate) fn project_loaded_page_after_browser_owner_commit(
        &mut self,
        replacement: &BrowserPageReplacement,
        retiring_renderer_page: Option<RendererPageResidenceIdentity>,
    ) {
        self.runtime_slot
            .project_loaded_page_after_browser_owner_commit(replacement, retiring_renderer_page);
        self.runtime_slot
            .ingest_owner_page_observable_output_updates();
    }

    pub(crate) fn project_initial_document_page_after_browser_owner_commit(
        &mut self,
        transition: &BrowserPageResidenceTransition,
    ) {
        self.runtime_slot
            .project_initial_document_page_after_browser_owner_commit(transition);
        self.runtime_slot
            .ingest_owner_page_observable_output_updates();
    }

    pub(crate) fn project_failed_navigation_page_absence_after_browser_owner_commit(
        &mut self,
        transition: &BrowserPageResidenceTransition,
    ) -> Option<Page> {
        let previous = self
            .runtime_slot
            .project_failed_navigation_page_absence_after_browser_owner_commit(transition);
        self.runtime_slot
            .ingest_owner_page_observable_output_updates();
        previous
    }

    #[cfg(test)]
    pub(crate) fn page_attachment_id(&self) -> Option<TargetPageAttachmentId> {
        self.runtime_slot.page_attachment_id()
    }

    pub(crate) async fn close_page_async(&mut self) {
        if let Some(page) = self
            .runtime_slot
            .retire_page_projection_after_browser_owner_forget()
        {
            BrowserContext::close_page_best_effort(page).await;
        }
    }
}
