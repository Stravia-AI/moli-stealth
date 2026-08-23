use moli_core::{
    browser_host::{
        BrowserPageOwnerKey, BrowserSelectedTargetEngineDisposition,
        BrowserTargetEngineAdoptionError, BrowserTargetEngineResidence, PageResidenceIdentity,
    },
    page::{
        CompletedDevToolsIoCommandDispatch, CompletedPageCommand, PendingDevToolsIoCommandDispatch,
        PendingPageCommand,
    },
    runtime::NavigationEngine,
};

use crate::devtools_runtime::{DevToolsError, DevToolsErrorKind};

#[cfg(test)]
use moli_core::browser_host::BrowserTargetRegistryError;

use super::{CdpConnection, TargetProjectionError};

#[derive(Debug)]
pub(crate) enum BrowserTargetPromotionError {
    Projection(TargetProjectionError),
    BrowserContextNotLoaded,
    BrowserContextLostAfterProjection,
    Synchronization(String),
}

pub(crate) enum BrowserTargetPromotionStart {
    Complete(bool),
    Pending(PendingBrowserTargetPromotion),
}

pub(crate) struct PendingBrowserTargetPromotion {
    page_owner: PageResidenceIdentity,
    script_execution_disabled: PendingDevToolsIoCommandDispatch,
    commands: Vec<(BrowserTargetPromotionPageCommandKind, PendingPageCommand)>,
    start_error: Option<String>,
}

pub(crate) struct CompletedBrowserTargetPromotion {
    page_owner: PageResidenceIdentity,
    script_execution_disabled: Result<CompletedDevToolsIoCommandDispatch, String>,
    commands: Vec<(
        BrowserTargetPromotionPageCommandKind,
        Result<CompletedPageCommand, String>,
    )>,
    start_error: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum BrowserTargetPromotionPageCommandKind {
    ExtraHttpHeaders,
    NetworkOffline,
    BypassServiceWorker,
    BlockedUrlPatterns,
    CpuThrottlingRate,
    FetchSubresourceInterception,
    SurfaceOverride,
}

impl PendingBrowserTargetPromotion {
    pub(crate) async fn wait(self) -> CompletedBrowserTargetPromotion {
        let script_execution_disabled = self
            .script_execution_disabled
            .wait()
            .await
            .map_err(|error| error.to_string());
        let mut commands = Vec::with_capacity(self.commands.len());
        for (kind, pending) in self.commands {
            commands.push((
                kind,
                pending.wait().await.map_err(|error| error.to_string()),
            ));
        }
        CompletedBrowserTargetPromotion {
            page_owner: self.page_owner,
            script_execution_disabled,
            commands,
            start_error: self.start_error,
        }
    }
}

impl std::fmt::Display for BrowserTargetPromotionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Projection(error) => write!(formatter, "TargetProjectionRejected: {error}"),
            Self::BrowserContextNotLoaded => formatter.write_str("BrowserContextNotLoaded"),
            Self::BrowserContextLostAfterProjection => {
                formatter.write_str("BrowserContextLostAfterTargetProjection")
            }
            Self::Synchronization(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for BrowserTargetPromotionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Projection(error) => Some(error),
            _ => None,
        }
    }
}

impl From<TargetProjectionError> for BrowserTargetPromotionError {
    fn from(error: TargetProjectionError) -> Self {
        Self::Projection(error)
    }
}

impl From<BrowserTargetPromotionError> for DevToolsError {
    fn from(error: BrowserTargetPromotionError) -> Self {
        Self::new(DevToolsErrorKind::Internal, error.to_string())
    }
}

/// Migration adapter between the physical Target/Page projection and Browser
/// Core's target-keyed NavigationEngine registry.
///
/// This module may inspect protocol-resident active/background Page slots to
/// choose `Retain` versus `Discard`, but every engine exchange is committed by
/// Browser Core with protocol-neutral `{context, target}` identity. It must not
/// consult CDP session attachment when authorizing a handoff.
impl CdpConnection {
    pub(super) fn selected_target_engine_disposition(
        &self,
    ) -> BrowserSelectedTargetEngineDisposition {
        let Some(browser_context) = self.browser_context.as_ref() else {
            return BrowserSelectedTargetEngineDisposition::Unbound;
        };
        let Some(target_id) = browser_context.active_target_id() else {
            return BrowserSelectedTargetEngineDisposition::Unbound;
        };
        let owner = BrowserPageOwnerKey::new(browser_context.id.clone(), target_id.to_owned());
        if browser_context.has_loaded_page() {
            BrowserSelectedTargetEngineDisposition::Retain(owner)
        } else {
            BrowserSelectedTargetEngineDisposition::Discard(owner)
        }
    }

    fn selected_target_engine_owner_key(&self) -> Option<BrowserPageOwnerKey> {
        let browser_context = self.browser_context.as_ref()?;
        Some(BrowserPageOwnerKey::new(
            browser_context.id.clone(),
            browser_context.active_target_id()?.to_owned(),
        ))
    }

    /// Transitional helper for engine-producing operations that do not yet
    /// carry a neutral Target key. Browser Core resolves its authoritative
    /// selected Target (or exact unbound state) and commits the adoption.
    pub(crate) fn adopt_navigation_engine_for_current_owner(
        &mut self,
        engine: NavigationEngine,
    ) -> Result<Option<BrowserTargetEngineResidence>, BrowserTargetEngineAdoptionError> {
        let projected_owner = self.selected_target_engine_owner_key();
        self.browser_host_state
            .navigation_owner_mut()
            .adopt_selected_target_engine_or_unbound(projected_owner, engine)
    }

    pub(crate) fn apply_scheduler_senders_to_navigation_engine(&self, engine: &NavigationEngine) {
        self.browser_host_state
            .navigation_owner()
            .configure_detached_engine(engine);
    }

    #[cfg(test)]
    pub(crate) fn navigation_engine_with_user_agent_marker_for_test(
        &self,
        marker: &str,
    ) -> NavigationEngine {
        let mut fetch_config = self
            .browser_host_state
            .navigation_owner()
            .active_fetch_config()
            .clone();
        fetch_config.set_user_agent(marker);
        self.browser_host_state
            .navigation_owner()
            .new_engine_sharing_active_renderer_owner(fetch_config)
            .expect("test navigation engine should share the active renderer owner")
    }

    #[cfg(test)]
    pub(crate) fn active_navigation_engine_user_agent_for_test(&self) -> String {
        self.browser_host_state
            .navigation_owner()
            .active_fetch_config()
            .user_agent()
            .to_owned()
    }

    pub(crate) fn retain_background_navigation_engine(
        &mut self,
        browser_context_id: String,
        target_id: String,
        engine: NavigationEngine,
    ) -> Result<(), String> {
        let owner_access = self
            .browser_context_by_id(&browser_context_id)
            .ok_or_else(|| {
                format!(
                    "cannot retain navigation engine for missing BrowserContext `{browser_context_id}`"
                )
            })?
            .renderer_runtime_owner_access();
        if !engine
            .browser_context_runtime()
            .shares_state_with(&owner_access.runtime())
        {
            return Err(format!(
                "navigation engine renderer context does not match BrowserContext `{browser_context_id}`"
            ));
        }
        self.browser_host_state
            .navigation_owner_mut()
            .adopt_target_engine(
                BrowserPageOwnerKey::new(browser_context_id, target_id),
                BrowserTargetEngineResidence::Retained,
                engine,
            )
            .map_err(|error| error.to_string())?;
        owner_access.reap_retired_resource_runtimes();
        Ok(())
    }

    pub(crate) fn adopt_loaded_navigation_engine_for_target_owner(
        &mut self,
        owner: BrowserPageOwnerKey,
        engine: NavigationEngine,
    ) -> Result<BrowserTargetEngineResidence, BrowserTargetEngineAdoptionError> {
        self.browser_host_state
            .navigation_owner_mut()
            .adopt_registered_target_engine(owner, engine)
    }

    /// Session routing is retained only as a test/migration input adapter. It
    /// resolves once to neutral browser identity; the core operation neither
    /// sees nor stores the frontend session.
    #[cfg(test)]
    pub(crate) fn adopt_loaded_navigation_engine_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        engine: NavigationEngine,
    ) {
        let Some(owner) = self.target_page_owner_key_for_session(session_id) else {
            self.adopt_navigation_engine_for_current_owner(engine)
                .expect("test current engine owner should remain exact");
            return;
        };
        self.adopt_loaded_navigation_engine_for_target_owner(owner, engine)
            .expect("test navigation engine owner must remain physically resident");
    }

    pub(crate) async fn promote_background_target_to_active_for_connection_async(
        &mut self,
        target_id: &str,
    ) -> Result<bool, BrowserTargetPromotionError> {
        match self.start_promote_background_target_to_active_for_connection(target_id)? {
            BrowserTargetPromotionStart::Complete(promoted) => Ok(promoted),
            BrowserTargetPromotionStart::Pending(pending) => {
                self.finish_promote_background_target_to_active_for_connection(pending.wait().await)
            }
        }
    }

    pub(crate) fn start_promote_background_target_to_active_for_connection(
        &mut self,
        target_id: &str,
    ) -> Result<BrowserTargetPromotionStart, BrowserTargetPromotionError> {
        let Some(browser_context) = self.browser_context.as_ref() else {
            return Err(BrowserTargetPromotionError::BrowserContextNotLoaded);
        };
        if browser_context.active_target_id() == Some(target_id) {
            return Ok(BrowserTargetPromotionStart::Complete(true));
        }
        if browser_context.background_target(target_id).is_none() {
            return Ok(BrowserTargetPromotionStart::Complete(false));
        }
        let projected = self.activate_target_projection(target_id)?;
        if projected.synchronize_loaded_page()
            && let Some(pending) = self.start_active_target_promotion_page_synchronization()?
        {
            return Ok(BrowserTargetPromotionStart::Pending(pending));
        }
        self.refresh_active_browser_context_loader();
        Ok(BrowserTargetPromotionStart::Complete(true))
    }

    pub(crate) fn finish_promote_background_target_to_active_for_connection(
        &mut self,
        completed: CompletedBrowserTargetPromotion,
    ) -> Result<bool, BrowserTargetPromotionError> {
        let promoted_context_is_selected = self
            .browser_host_state
            .navigation_owner()
            .selected_browser_context_id()
            == Some(completed.page_owner.browser_context_id());
        let owner_route = self
            .target_page_owner_route_if_current(&completed.page_owner)
            .ok_or_else(|| {
                BrowserTargetPromotionError::Synchronization(
                    "promoted Target Page was replaced during state synchronization".to_owned(),
                )
            })?;
        let mut route_scope = self.scoped_none_session_owner_route_override(owner_route);
        let conn = route_scope.conn_mut();
        let mut page = conn
            .runtime_session_owner_slot_mut(None)
            .map_err(BrowserTargetPromotionError::Synchronization)?
            .loaded_page_mut()
            .ok_or_else(|| {
                BrowserTargetPromotionError::Synchronization(
                    "promoted Target lost its loaded Page during state synchronization".to_owned(),
                )
            })?;
        let mut first_error = completed.start_error;
        match completed.script_execution_disabled {
            Ok(CompletedDevToolsIoCommandDispatch::Dispatched)
            | Ok(CompletedDevToolsIoCommandDispatch::SessionResponse {
                response_succeeded: true,
                ..
            }) => {}
            Ok(CompletedDevToolsIoCommandDispatch::SessionResponse {
                response_succeeded: false,
                ..
            }) => {
                if first_error.is_none() {
                    first_error = Some(
                        "promoted Target script-execution policy IO command returned an error"
                            .to_owned(),
                    );
                }
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(format!(
                        "promoted Target script-execution policy IO command failed: {error}"
                    ));
                }
            }
        }
        for (kind, completion) in completed.commands {
            let completion = match completion {
                Ok(completion) => completion,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(format!(
                            "promoted Target {kind:?} command failed before apply: {error}"
                        ));
                    }
                    continue;
                }
            };
            let result = match kind {
                BrowserTargetPromotionPageCommandKind::ExtraHttpHeaders => {
                    page.finish_set_extra_http_headers(completion)
                }
                BrowserTargetPromotionPageCommandKind::NetworkOffline => {
                    page.finish_set_network_offline(completion)
                }
                BrowserTargetPromotionPageCommandKind::BypassServiceWorker => {
                    page.finish_set_bypass_service_worker(completion)
                }
                BrowserTargetPromotionPageCommandKind::BlockedUrlPatterns => {
                    page.finish_set_blocked_url_patterns(completion)
                }
                BrowserTargetPromotionPageCommandKind::CpuThrottlingRate => {
                    page.finish_set_cpu_throttling_rate(completion)
                }
                BrowserTargetPromotionPageCommandKind::FetchSubresourceInterception => {
                    page.finish_set_fetch_subresource_interception(completion)
                }
                BrowserTargetPromotionPageCommandKind::SurfaceOverride => {
                    page.finish_run_page_surface_override_script(completion)
                }
            };
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(format!(
                    "failed to finish promoted Target {kind:?}: {error}"
                ));
            }
        }
        drop(route_scope);
        if let Some(error) = first_error {
            return Err(BrowserTargetPromotionError::Synchronization(error));
        }
        if promoted_context_is_selected {
            self.refresh_active_browser_context_loader();
        }
        Ok(true)
    }

    fn start_active_target_promotion_page_synchronization(
        &mut self,
    ) -> Result<Option<PendingBrowserTargetPromotion>, BrowserTargetPromotionError> {
        let Some(browser_context) = self.browser_context.as_ref() else {
            return Err(BrowserTargetPromotionError::BrowserContextLostAfterProjection);
        };
        let Some(target_id) = browser_context.active_target_id() else {
            return Err(BrowserTargetPromotionError::BrowserContextLostAfterProjection);
        };
        if !browser_context.active_target.runtime_slot.has_loaded_page() {
            return Ok(None);
        }
        let page_owner = self
            .browser_host_state
            .navigation_owner()
            .capture_page_residence(&browser_context.id, target_id)
            .ok_or_else(|| {
                BrowserTargetPromotionError::Synchronization(
                    "promoted Target has no exact Page residence".to_owned(),
                )
            })?;

        let browser_context = self
            .browser_context
            .as_mut()
            .ok_or(BrowserTargetPromotionError::BrowserContextLostAfterProjection)?;
        let effective_headers = browser_context.effective_extra_headers();
        let network_offline = browser_context.network_policy.network_offline();
        let bypass_service_worker = browser_context.network_policy.bypass_service_worker();
        let blocked_url_patterns = browser_context
            .network_policy
            .blocked_url_patterns()
            .to_vec();
        let script_execution_disabled = browser_context.script_execution_disabled;
        let cpu_throttling_rate = browser_context.cpu_throttling_rate;
        let (fetch_subresource_enabled, fetch_subresource_resource_type) = browser_context
            .active_target
            .fetch_owner
            .subresource_interception_config();
        let surface_override = browser_context
            .generated_surface_override_script_for_active_target()
            .map(|script| script.source);
        let page = browser_context
            .active_target
            .runtime_slot
            .loaded_page_mut()
            .ok_or_else(|| {
                BrowserTargetPromotionError::Synchronization(
                    "promoted Target has no loaded Page to synchronize".to_owned(),
                )
            })?;
        let script_execution_disabled =
            page.start_set_script_execution_disabled_from_io(script_execution_disabled);
        let mut pending = Vec::with_capacity(6 + usize::from(surface_override.is_some()));
        macro_rules! start_promotion_command {
            ($kind:expr, $command:expr) => {
                match $command {
                    Ok(command) => pending.push(($kind, command)),
                    Err(error) => {
                        let error = format!("failed to start promoted Target {:?}: {error}", $kind);
                        if pending.is_empty() {
                            return Err(BrowserTargetPromotionError::Synchronization(error));
                        }
                        return Ok(Some(PendingBrowserTargetPromotion {
                            page_owner,
                            script_execution_disabled,
                            commands: pending,
                            start_error: Some(error),
                        }));
                    }
                }
            };
        }
        start_promotion_command!(
            BrowserTargetPromotionPageCommandKind::ExtraHttpHeaders,
            page.start_set_extra_http_headers(&effective_headers)
        );
        start_promotion_command!(
            BrowserTargetPromotionPageCommandKind::NetworkOffline,
            page.start_set_network_offline(network_offline)
        );
        start_promotion_command!(
            BrowserTargetPromotionPageCommandKind::BypassServiceWorker,
            page.start_set_bypass_service_worker(bypass_service_worker)
        );
        start_promotion_command!(
            BrowserTargetPromotionPageCommandKind::BlockedUrlPatterns,
            page.start_set_blocked_url_patterns(&blocked_url_patterns)
        );
        start_promotion_command!(
            BrowserTargetPromotionPageCommandKind::CpuThrottlingRate,
            page.start_set_cpu_throttling_rate(cpu_throttling_rate)
        );
        start_promotion_command!(
            BrowserTargetPromotionPageCommandKind::FetchSubresourceInterception,
            page.start_set_fetch_subresource_interception(
                fetch_subresource_enabled,
                fetch_subresource_resource_type,
            )
        );
        if let Some(source) = surface_override {
            start_promotion_command!(
                BrowserTargetPromotionPageCommandKind::SurfaceOverride,
                page.start_run_page_surface_override_script(&source)
            );
        }
        Ok(Some(PendingBrowserTargetPromotion {
            page_owner,
            script_execution_disabled,
            commands: pending,
            start_error: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::BackgroundTarget;

    #[test]
    fn exact_engine_adoption_does_not_require_frontend_session() {
        let mut conn = CdpConnection::new();
        let mut context = conn.new_browser_context("context-1".to_owned());
        context.set_active_target_id("target-a");
        conn.insert_browser_context(context);
        let owner = BrowserPageOwnerKey::new("context-1", "target-a");

        assert_eq!(
            conn.adopt_loaded_navigation_engine_for_target_owner(
                owner.clone(),
                NavigationEngine::new()
            ),
            Ok(BrowserTargetEngineResidence::Selected)
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .selected_target_engine_owner(),
            Some(&owner)
        );
    }

    #[test]
    fn physical_background_residence_parks_engine_by_target_not_session() {
        let mut conn = CdpConnection::new();
        let mut context = conn.new_browser_context("context-1".to_owned());
        context.set_active_target_id("target-a");
        context.background_targets.push(BackgroundTarget::with_url(
            "target-b".to_owned(),
            None,
            "about:blank".to_owned(),
        ));
        conn.insert_browser_context(context);
        let background = BrowserPageOwnerKey::new("context-1", "target-b");

        assert_eq!(
            conn.adopt_loaded_navigation_engine_for_target_owner(
                background.clone(),
                NavigationEngine::new()
            ),
            Ok(BrowserTargetEngineResidence::Retained)
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .retained_background_engine_keys()
                .collect::<Vec<_>>(),
            vec![&background]
        );
    }

    #[test]
    fn physical_selected_target_divergence_rejects_engine_without_rebinding_core() {
        let mut conn = CdpConnection::new();
        let mut context = conn.new_browser_context("context-1".to_owned());
        context.set_active_target_id("target-a");
        conn.insert_browser_context(context);
        let target_a = BrowserPageOwnerKey::new("context-1", "target-a");
        let target_b = BrowserPageOwnerKey::new("context-1", "target-b");
        let selected_renderer_owner = conn
            .browser_host_state
            .navigation_owner()
            .active_renderer_owner_id_for_diagnostics();
        conn.browser_context
            .as_mut()
            .expect("physical BrowserContext")
            .set_active_target_id("target-b");

        let error = conn
            .adopt_navigation_engine_for_current_owner(
                NavigationEngine::new_with_page_vm_document_isolate_for_diagnostics(),
            )
            .expect_err("physical/Core selected Target divergence must reject adoption");

        assert_eq!(
            error,
            BrowserTargetEngineAdoptionError::SelectedTargetProjectionMismatch {
                authoritative: Some(target_a.clone()),
                projected: Some(target_b),
            }
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .selected_target_engine_owner(),
            Some(&target_a)
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .active_renderer_owner_id_for_diagnostics(),
            selected_renderer_owner,
            "rejected projection must preserve the selected engine"
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .retained_background_engine_count(),
            0
        );
    }

    #[test]
    fn unknown_loaded_target_rejects_engine_without_mutating_registry() {
        let mut conn = CdpConnection::new();
        let selected_renderer_owner = conn
            .browser_host_state
            .navigation_owner()
            .active_renderer_owner_id_for_diagnostics();
        let missing = BrowserPageOwnerKey::new("missing-context", "missing-target");

        let error = conn
            .adopt_loaded_navigation_engine_for_target_owner(
                missing.clone(),
                NavigationEngine::new_with_page_vm_document_isolate_for_diagnostics(),
            )
            .expect_err("unknown exact Target must reject engine adoption");

        assert_eq!(
            error,
            BrowserTargetEngineAdoptionError::Target(BrowserTargetRegistryError::UnknownTarget(
                moli_core::browser_host::BrowserTargetId::new("missing-target")
            ))
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .selected_target_engine_owner(),
            None
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .active_renderer_owner_id_for_diagnostics(),
            selected_renderer_owner
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .retained_background_engine_count(),
            0
        );
    }

    #[test]
    fn topology_rejects_engine_owner_divergence_before_idle_reset() {
        let mut conn = CdpConnection::new();
        let divergent = BrowserPageOwnerKey::new("missing-context", "missing-target");
        let selected_renderer_owner = conn
            .browser_host_state
            .navigation_owner()
            .active_renderer_owner_id_for_diagnostics();
        let error = conn
            .browser_host_state
            .navigation_owner_mut()
            .adopt_target_engine(
                divergent.clone(),
                BrowserTargetEngineResidence::Selected,
                NavigationEngine::new_with_page_vm_document_isolate_for_diagnostics(),
            )
            .expect_err("selected engine identity must come from Browser topology");
        assert_eq!(error.selected(), None);
        assert_eq!(error.requested(), Some(&divergent));
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .active_renderer_owner_id_for_diagnostics(),
            selected_renderer_owner,
            "rejected injection must preserve the unbound engine"
        );

        let result = conn.release_idle_navigation_engine_memory_if_idle();

        assert!(result.reset);
        assert_eq!(result.reason, "idle-engine-replaced");
        assert_eq!(result.loaded_browser_context_count, 0);
        assert_eq!(result.live_target_browser_context_count, 0);
        assert_eq!(result.retained_background_navigation_engine_count, 0);
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .selected_target_engine_owner(),
            None
        );
    }

    #[tokio::test]
    async fn loaded_target_handoff_parks_and_restores_exact_core_engine_owner() {
        let mut conn = CdpConnection::new();
        let mut context = conn.new_browser_context("context-1".to_owned());
        context.set_active_target_id("target-a");
        conn.insert_browser_context(context);
        let page_a = conn
            .load_page_via_runtime_async("data:text/html,<title>A</title>")
            .await
            .expect("target A Page should load");
        conn.browser_context
            .as_mut()
            .expect("active context")
            .replace_loaded_page(Some(page_a));
        let background_target =
            BackgroundTarget::with_url("target-b".to_owned(), None, "about:blank".to_owned());
        conn.register_background_target_projection(
            "context-1",
            "target-b",
            move |context, target_handle, page_residence, session_storage_access| {
                let mut background_target = background_target;
                background_target.replace_target_handle(target_handle);
                background_target.replace_page_residence_handle(page_residence);
                background_target.bind_session_storage_access(session_storage_access);
                context.background_targets.push(background_target);
            },
        )
        .expect("background Target projection should register");
        let target_a = BrowserPageOwnerKey::new("context-1", "target-a");
        let target_b = BrowserPageOwnerKey::new("context-1", "target-b");
        assert_eq!(
            conn.adopt_loaded_navigation_engine_for_target_owner(
                target_b.clone(),
                NavigationEngine::new()
            ),
            Ok(BrowserTargetEngineResidence::Retained)
        );

        assert!(
            conn.promote_background_target_to_active_for_connection_async("target-b")
                .await
                .expect("target B promotion should run")
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .selected_target_engine_owner(),
            Some(&target_b)
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .retained_background_engine_keys()
                .collect::<Vec<_>>(),
            vec![&target_a]
        );

        assert!(
            conn.promote_background_target_to_active_for_connection_async("target-a")
                .await
                .expect("target A promotion should run")
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .selected_target_engine_owner(),
            Some(&target_a)
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .retained_background_engine_count(),
            0
        );
    }

    #[tokio::test]
    async fn unknown_target_promotion_does_not_rebind_the_selected_core_engine() {
        let mut conn = CdpConnection::new();
        let mut context = conn.new_browser_context("context-1".to_owned());
        context.set_active_target_id("target-a");
        conn.insert_browser_context(context);
        let target_a = BrowserPageOwnerKey::new("context-1", "target-a");
        assert_eq!(
            conn.adopt_loaded_navigation_engine_for_target_owner(
                target_a.clone(),
                NavigationEngine::new_with_page_vm_document_isolate_for_diagnostics(),
            ),
            Ok(BrowserTargetEngineResidence::Selected)
        );
        let renderer_owner = conn
            .browser_host_state
            .navigation_owner()
            .active_renderer_owner_id_for_diagnostics();

        assert!(
            !conn
                .promote_background_target_to_active_for_connection_async("missing-target")
                .await
                .expect("unknown Target lookup should not fail")
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .selected_target_engine_owner(),
            Some(&target_a)
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .active_renderer_owner_id_for_diagnostics(),
            renderer_owner
        );
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .retained_background_engine_count(),
            0
        );
    }
}
