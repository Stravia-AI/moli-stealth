use moli_core::{
    browser_host::{
        BrowserNavigationFailure, BrowserNavigationTraceContext, BrowserNavigationTraceEvent,
        BrowserNavigationTraceSource, BrowserPageOwnerKey,
    },
    page::ScriptNetworkOutputItem,
};
use moli_page_types::BrowserActionId;

use crate::{
    devtools_runtime::DevToolsCommandContext,
    domains::network::TargetNetworkBacklogPreparedDelivery,
};

use super::{
    CdpConnection, CommittedRendererDocumentBinding, DevToolsDocumentNavigationState,
    DocumentNavigationToken,
};

/// Migration adapter from frontend/session routing to Browser Core's
/// authoritative cross-document request registry.
///
/// A CDP session is resolved to a browser Target exactly once. Pending and
/// committed request identity then live in `BrowserNavigationOwner`; protocol
/// slots retain only renderer attachment and lifecycle projections.
impl CdpConnection {
    fn document_navigation_owner_for_session(
        &self,
        session_id: Option<&str>,
    ) -> Option<BrowserPageOwnerKey> {
        self.target_page_owner_key_for_session(session_id)
    }

    fn document_navigation_owner_for_target_id(
        &self,
        target_id: &str,
    ) -> Option<BrowserPageOwnerKey> {
        let browser_context = self.browser_contexts().find(|browser_context| {
            browser_context.is_active_target(target_id)
                || browser_context.background_target(target_id).is_some()
        })?;
        Some(BrowserPageOwnerKey::new(
            browser_context.id.clone(),
            target_id.to_owned(),
        ))
    }

    pub(crate) fn prepare_navigation_trace_context_for_session_owner(
        &self,
        session_id: Option<&str>,
        origin: BrowserNavigationTraceSource,
        source_document: Option<moli_core::RendererDocumentLifecycleIdentity>,
    ) -> Option<BrowserNavigationTraceContext> {
        if !moli_trace::browser_owner_trace_enabled() {
            return None;
        }
        let owner = self.document_navigation_owner_for_session(session_id)?;
        let context = self
            .browser_host_state
            .navigation_owner()
            .owner_navigation_trace_context(
                &owner,
                BrowserActionId::allocate(),
                origin,
                source_document,
            )?;
        context.emit(BrowserNavigationTraceEvent::new(
            "browser_action_published",
            origin,
            match origin {
                BrowserNavigationTraceSource::FrontendCommand => "frontend-command",
                BrowserNavigationTraceSource::RendererIntent => "renderer-output",
                BrowserNavigationTraceSource::Network => "network",
                BrowserNavigationTraceSource::Lifecycle => "lifecycle",
            },
            "browser-owner-inbox",
        ));
        Some(context)
    }

    pub(crate) fn document_navigation_trace_context(
        &self,
        token: &DocumentNavigationToken,
    ) -> Option<BrowserNavigationTraceContext> {
        let owner = self.document_navigation_owner_for_target_id(token.target_id())?;
        self.browser_host_state
            .navigation_owner()
            .document_navigation_trace_context(&owner, token)
    }

    pub(crate) fn current_page_residence_for_document_navigation(
        &self,
        token: &DocumentNavigationToken,
    ) -> Option<moli_core::browser_host::PageResidenceIdentity> {
        let owner = self.document_navigation_owner_for_target_id(token.target_id())?;
        if !self
            .browser_host_state
            .navigation_owner()
            .accepts_committed_document_navigation(&owner, token)
        {
            return None;
        }
        self.browser_host_state
            .navigation_owner()
            .capture_page_residence(owner.browser_context_id(), owner.target_id())
    }

    pub(crate) fn allocate_frontend_projection_trace_sequence(&mut self) -> Option<u64> {
        moli_trace::browser_owner_trace_enabled()
            .then(|| self.scheduler_state.allocate_frontend_projection_sequence())
    }

    pub(crate) fn accepts_pending_document_navigation_token(
        &self,
        token: &DocumentNavigationToken,
    ) -> bool {
        let Some(owner) = self.document_navigation_owner_for_target_id(token.target_id()) else {
            return false;
        };
        self.browser_host_state
            .navigation_owner()
            .accepts_pending_document_navigation(&owner, token)
    }

    #[cfg(test)]
    pub(crate) fn has_pending_document_navigation_for_target_id(&self, target_id: &str) -> bool {
        let Some(owner) = self.document_navigation_owner_for_target_id(target_id) else {
            return false;
        };
        self.browser_host_state
            .navigation_owner()
            .has_pending_document_navigation(&owner)
    }

    pub fn has_pending_document_navigation_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> bool {
        let Some(owner) = self.document_navigation_owner_for_session(session_id) else {
            return false;
        };
        self.browser_host_state
            .navigation_owner()
            .has_pending_document_navigation(&owner)
    }

    fn document_navigation_state_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> DevToolsDocumentNavigationState {
        if self
            .document_navigation_owner_for_session(session_id)
            .is_none()
        {
            return DevToolsDocumentNavigationState::Unavailable;
        }
        if self.has_pending_document_navigation_for_session_owner(session_id) {
            return DevToolsDocumentNavigationState::PendingNavigation;
        }
        // The initial empty Document has a real loader identity before any
        // cross-document request exists. Frame-tree lookup combines Browser
        // Core's committed request with that protocol bootstrap metadata.
        match self.target_session_owner_frame_tree_loader_id(session_id) {
            Some(loader_id) => DevToolsDocumentNavigationState::Committed { loader_id },
            None => DevToolsDocumentNavigationState::AwaitingCommit,
        }
    }

    /// Resolves Document readiness through the exact target captured by a
    /// protocol-neutral command context.
    pub fn devtools_context_document_navigation_state(
        &mut self,
        context: &DevToolsCommandContext,
    ) -> DevToolsDocumentNavigationState {
        if let Some(target_id) = context.target_id.as_ref() {
            let Some(route) = self.target_session_route_for_target_id(target_id.as_str()) else {
                return DevToolsDocumentNavigationState::Unavailable;
            };
            let mut route_scope = self.scoped_none_session_owner_route_override(route);
            return route_scope
                .conn_mut()
                .document_navigation_state_for_session_owner(None);
        }
        self.document_navigation_state_for_session_owner(
            context
                .session_id
                .as_ref()
                .map(|session_id| session_id.as_str()),
        )
    }

    pub(crate) fn accepts_pending_document_navigation_for_session_owner(
        &self,
        session_id: Option<&str>,
        token: &DocumentNavigationToken,
    ) -> bool {
        let Some(owner) = self.document_navigation_owner_for_session(session_id) else {
            return false;
        };
        owner.target_id() == token.target_id()
            && self
                .browser_host_state
                .navigation_owner()
                .accepts_pending_document_navigation(&owner, token)
    }

    pub(crate) fn accepts_document_body_completion_for_session_owner(
        &self,
        session_id: Option<&str>,
        token: &DocumentNavigationToken,
    ) -> bool {
        let Some(owner) = self.document_navigation_owner_for_session(session_id) else {
            return false;
        };
        owner.target_id() == token.target_id()
            && self
                .browser_host_state
                .navigation_owner()
                .accepts_document_body_completion(&owner, token)
    }

    pub(crate) fn accepts_committed_document_navigation_for_session_owner(
        &self,
        session_id: Option<&str>,
        token: &DocumentNavigationToken,
    ) -> bool {
        let Some(owner) = self.document_navigation_owner_for_session(session_id) else {
            return false;
        };
        owner.target_id() == token.target_id()
            && self
                .browser_host_state
                .navigation_owner()
                .accepts_committed_document_navigation(&owner, token)
    }

    pub(crate) fn ensure_document_accessible_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Result<(), String> {
        if self.has_pending_document_navigation_for_session_owner(session_id) {
            return Err("Navigation is changing the document".to_owned());
        }
        Ok(())
    }

    pub(crate) fn current_document_loader_id_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<String> {
        let owner = self.document_navigation_owner_for_session(session_id)?;
        self.browser_host_state
            .navigation_owner()
            .current_document_loader_id(&owner)
    }

    pub(crate) fn committed_document_loader_id_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<String> {
        let owner = self.document_navigation_owner_for_session(session_id)?;
        self.browser_host_state
            .navigation_owner()
            .committed_document_loader_id(&owner)
    }

    pub(crate) fn loaded_page_mut_for_protocol_access(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<moli_core::browser_host::BrowserPageRuntimeLease, String> {
        self.ensure_document_accessible_for_session_owner(session_id)?;
        self.loaded_page_mut_for_interruptible_protocol_access(session_id)
    }

    /// Returns the exact Page that remains attached while a cross-Document
    /// navigation is suspended.
    ///
    /// Only commands classified as renderer-interruptible at the parsed CDP
    /// boundary may use this access path. Ordinary protocol commands must use
    /// [`Self::loaded_page_mut_for_protocol_access`] so they wait for the
    /// replacement attachment instead of entering the old renderer.
    pub(crate) fn loaded_page_mut_for_interruptible_protocol_access(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<moli_core::browser_host::BrowserPageRuntimeLease, String> {
        self.runtime_session_owner_slot_mut(session_id)?
            .loaded_page_mut()
            .ok_or_else(|| "NoDocumentLoaded".to_owned())
    }

    #[cfg(test)]
    pub(crate) fn start_document_navigation_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        loader_id: String,
    ) -> Option<DocumentNavigationToken> {
        let navigation = self
            .start_document_navigation_for_session_owner_with_trace(session_id, loader_id, None)?;
        if let Err(error) = self.take_navigation_admission_fact(&navigation) {
            tracing::error!(
                %error,
                target_id = navigation.target_id(),
                loader_id = navigation.loader_id(),
                "test/embedding navigation admission lost its exact Browser fact"
            );
        }
        Some(navigation)
    }

    pub(crate) fn start_document_navigation_for_session_owner_with_trace(
        &mut self,
        session_id: Option<&str>,
        loader_id: String,
        trace: Option<BrowserNavigationTraceContext>,
    ) -> Option<DocumentNavigationToken> {
        let owner = self.document_navigation_owner_for_session(session_id)?;
        // Validate the frontend projection before mutating Browser Core. Once
        // accepted, the actor owns both mutations in this turn.
        self.runtime_session_owner_slot(session_id).ok()?;
        let token = self
            .browser_host_state
            .try_start_document_navigation_with_trace(&owner, loader_id, trace)?;
        self.runtime_session_owner_slot_mut(session_id)
            .expect("validated target runtime slot")
            .begin_document_navigation_protocol_state(token.clone());
        Some(token)
    }

    #[cfg(test)]
    pub(crate) fn commit_document_navigation_for_session_owner_if_matches(
        &mut self,
        session_id: Option<&str>,
        token: &DocumentNavigationToken,
    ) {
        let Some(owner) = self.document_navigation_owner_for_session(session_id) else {
            return;
        };
        if owner.target_id() == token.target_id() {
            let _ = self
                .browser_host_state
                .commit_document_navigation_if_matches(&owner, token);
        }
    }

    pub(crate) fn fail_document_navigation_for_session_owner_if_matches(
        &mut self,
        session_id: Option<&str>,
        token: &DocumentNavigationToken,
        failure: BrowserNavigationFailure,
    ) -> bool {
        let Some(owner) = self.document_navigation_owner_for_session(session_id) else {
            return false;
        };
        if owner.target_id() != token.target_id()
            || !self.browser_host_state.fail_document_navigation_if_matches(
                &owner,
                token,
                failure.clone(),
            )
        {
            return false;
        }
        match self.take_navigation_failure_fact(token, &failure) {
            Ok(_) => true,
            Err(error) => {
                tracing::error!(
                    %error,
                    target_id = token.target_id(),
                    loader_id = token.loader_id(),
                    "navigation terminal committed without an exact frontend Browser fact"
                );
                false
            }
        }
    }

    pub(crate) fn convert_document_navigation_to_download_for_session_owner_if_matches(
        &mut self,
        session_id: Option<&str>,
        token: &DocumentNavigationToken,
    ) -> bool {
        let Some(owner) = self.document_navigation_owner_for_session(session_id) else {
            return false;
        };
        if owner.target_id() != token.target_id()
            || !self
                .browser_host_state
                .convert_document_navigation_to_download_if_matches(&owner, token)
        {
            return false;
        }
        match self.take_navigation_download_fact(token) {
            Ok(_) => true,
            Err(error) => {
                tracing::error!(
                    %error,
                    target_id = token.target_id(),
                    loader_id = token.loader_id(),
                    "download conversion committed without an exact frontend Browser fact"
                );
                false
            }
        }
    }

    /// Retires only the protocol/renderer tail projection after Browser Core
    /// has already committed the exact navigation terminal. A delayed tail is
    /// deliberately unable to clear or otherwise mutate a successor request.
    pub(crate) fn clear_document_navigation_protocol_tail_for_session_owner_if_loader_matches(
        &mut self,
        session_id: Option<&str>,
        loader_id: &str,
    ) {
        let _ = self
            .runtime_session_owner_slot_mut(session_id)
            .map(|slot| slot.clear_pending_renderer_page_if_loader_matches(loader_id));
        self.discard_uncommitted_main_document_resource_for_session_owner(session_id, loader_id);
    }

    /// Returns a lifecycle projection only while its optional navigation token
    /// still names Browser Core's exact committed request.
    pub(crate) fn committed_renderer_document_binding_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<&CommittedRendererDocumentBinding> {
        let owner = self.document_navigation_owner_for_session(session_id)?;
        let binding = self
            .runtime_session_owner_slot(session_id)
            .ok()?
            .committed_renderer_document_binding()?;
        let is_current = binding.navigation.as_ref().is_none_or(|navigation| {
            self.browser_host_state
                .navigation_owner()
                .accepts_committed_document_navigation(&owner, navigation)
        });
        is_current.then_some(binding)
    }

    pub(crate) fn renderer_document_binding_is_current_for_session_owner(
        &self,
        session_id: Option<&str>,
        binding: &CommittedRendererDocumentBinding,
    ) -> bool {
        self.committed_renderer_document_binding_for_session_owner(session_id) == Some(binding)
    }

    pub(crate) fn ingest_renderer_page_network_output_item_and_prepare_live_delivery_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        source_renderer_page: Option<super::RendererPageResidenceIdentity>,
        source_document: moli_core::RendererDocumentLifecycleIdentity,
        item: &ScriptNetworkOutputItem,
    ) -> Option<TargetNetworkBacklogPreparedDelivery> {
        // The exact renderer Page/Document pair below may name either the
        // current Document or a replacement's retained network-only route.
        // Requiring the current Browser navigation token here would discard
        // the old Document's terminal facts as soon as its successor commits.
        let primary_session_id = self.runtime_session_owner_primary_session_id(session_id);
        let mut request_id_allocator = self.browser_host_state.network_artifacts();
        self.runtime_session_owner_slot_mut(session_id)
            .ok()
            .and_then(|slot| {
                slot.ingest_renderer_network_output_item_and_prepare_live_delivery(
                    source_renderer_page,
                    source_document,
                    item,
                    session_id,
                    primary_session_id.as_deref(),
                    None,
                    &mut request_id_allocator,
                )
            })
    }

    #[cfg(test)]
    pub(crate) fn ingest_renderer_network_output_item_and_prepare_live_delivery_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        source_document: moli_core::RendererDocumentLifecycleIdentity,
        item: &ScriptNetworkOutputItem,
    ) -> Option<TargetNetworkBacklogPreparedDelivery> {
        self.ingest_renderer_page_network_output_item_and_prepare_live_delivery_for_session_owner(
            session_id,
            None,
            source_document,
            item,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_protocol_projection_does_not_clear_browser_request_authority() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let token = conn
            .start_document_navigation_for_session_owner(None, "LOADER-core-owned".to_owned())
            .expect("default target should start a navigation");

        conn.runtime_session_owner_slot_mut(None)
            .expect("default target runtime slot")
            .clear_renderer_document_protocol_state();

        assert!(conn.has_pending_document_navigation_for_session_owner(None));
        assert!(conn.accepts_pending_document_navigation_for_session_owner(None, &token));
    }

    #[test]
    fn delayed_protocol_tail_cannot_clear_a_successor_browser_request() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let first = conn
            .start_document_navigation_for_session_owner(None, "LOADER-first".to_owned())
            .expect("first navigation");
        let second = conn
            .start_document_navigation_for_session_owner(None, "LOADER-second".to_owned())
            .expect("successor navigation");

        conn.clear_document_navigation_protocol_tail_for_session_owner_if_loader_matches(
            None,
            first.loader_id(),
        );

        assert!(!conn.accepts_pending_document_navigation_for_session_owner(None, &first));
        assert!(conn.accepts_pending_document_navigation_for_session_owner(None, &second));
    }
}
