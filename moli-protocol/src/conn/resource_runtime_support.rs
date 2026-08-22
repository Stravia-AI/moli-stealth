use moli_core::network::{
    BrowserResourceRuntime, BrowserResourceRuntimeOwner, ResourceRequestClient,
};
use moli_core::page::{CompletedPageCommand, PendingPageCommand};

use super::{
    BrowserContext, CdpConnection, TargetNavigationLoadInputs,
    state::BrowserContextResourceStorageHandles,
};

impl CdpConnection {
    pub(crate) fn invalidate_resource_runtime(&mut self) {
        self.browser_host_state
            .navigation_owner_mut()
            .reset_active_resource_runtime_without_loaded_page();
    }

    pub(crate) fn resource_storage_handles(&self) -> BrowserContextResourceStorageHandles {
        self.browser_context
            .as_ref()
            .map(BrowserContext::resource_storage_handles)
            .unwrap_or_else(|| self.initial_storage_partition.resource_storage_handles())
    }

    pub(crate) fn ensure_resource_request_client(
        &mut self,
    ) -> Result<ResourceRequestClient, String> {
        self.apply_active_engine_fetch_overrides();
        let storage = self.resource_storage_handles();
        self.browser_host_state
            .navigation_owner_mut()
            .ensure_active_resource_runtime_ready(storage.into_navigation_storage())
            .map_err(|error| format!("failed to initialize resource runtime: {error}"))?;
        self.browser_host_state
            .navigation_owner()
            .active_resource_request_client()
            .ok_or_else(|| "resource request client unavailable".to_owned())
    }

    pub(crate) fn ensure_resource_request_client_for_navigation_load_inputs(
        &mut self,
        load_inputs: &TargetNavigationLoadInputs,
    ) -> Result<ResourceRequestClient, String> {
        if !self.navigation_load_inputs_use_primary_engine(load_inputs) {
            let mut engine = self.background_navigation_engine_for_load_inputs(load_inputs);
            let storage = load_inputs.resource_storage_handles();
            engine
                .ensure_resource_runtime_ready_for_navigation_storage(
                    storage.into_navigation_storage(),
                )
                .map_err(|error| format!("failed to initialize resource runtime: {error}"))?;
            let request_client = engine
                .resource_request_client()
                .ok_or_else(|| "resource request client unavailable".to_owned())?;
            if let (Some(browser_context_id), Some(target_id)) = (
                load_inputs.browser_context_id.clone(),
                load_inputs.root_frame_id.clone(),
            ) {
                self.retain_background_navigation_engine(browser_context_id, target_id, engine)?;
            }
            return Ok(request_client);
        }
        self.apply_navigation_load_input_engine_fetch_overrides(load_inputs);
        let storage = load_inputs.resource_storage_handles();
        self.browser_host_state
            .navigation_owner_mut()
            .ensure_active_resource_runtime_ready(storage.into_navigation_storage())
            .map_err(|error| format!("failed to initialize resource runtime: {error}"))?;
        self.browser_host_state
            .navigation_owner()
            .active_resource_request_client()
            .ok_or_else(|| "resource request client unavailable".to_owned())
    }

    fn navigation_load_inputs_use_primary_engine(
        &self,
        load_inputs: &TargetNavigationLoadInputs,
    ) -> bool {
        let Some(browser_context_id) = load_inputs.browser_context_id.as_deref() else {
            return self
                .browser_host_state
                .navigation_owner()
                .active_runtime_shares_state_with(&load_inputs.renderer_runtime);
        };
        self.browser_context.as_ref().is_some_and(|context| {
            context.id == browser_context_id
                && context.active_target_id() == load_inputs.root_frame_id.as_deref()
                && self
                    .browser_host_state
                    .navigation_owner()
                    .active_runtime_shares_state_with(&load_inputs.renderer_runtime)
        })
    }

    fn apply_navigation_load_input_engine_fetch_overrides(
        &mut self,
        load_inputs: &TargetNavigationLoadInputs,
    ) {
        let policy = self.browser_host_network_policy_snapshot();
        let browser_identity = load_inputs
            .browser_identity_override
            .clone()
            .or_else(|| policy.global_browser_identity_override().cloned())
            .unwrap_or_else(|| policy.base_browser_identity().clone());
        let http_proxy = load_inputs
            .http_proxy_override
            .clone()
            .or_else(|| policy.base_http_proxy().map(str::to_owned));
        let http_no_proxy = load_inputs
            .http_no_proxy_override
            .clone()
            .or_else(|| policy.base_http_no_proxy().map(str::to_owned));
        let tls_verify_host = load_inputs
            .tls_verify_host_override
            .unwrap_or(policy.base_tls_verify_host());
        self.browser_host_state
            .navigation_owner_mut()
            .configure_active_fetch(moli_core::browser_host::BrowserPageFetchConfiguration {
                browser_identity,
                http_proxy,
                http_no_proxy,
                tls_verify_host,
                bypass_service_worker: load_inputs.bypass_service_worker,
            });
    }

    pub(crate) fn build_registered_browser_resource_runtime_for_navigation_load_inputs(
        &self,
        load_inputs: &TargetNavigationLoadInputs,
    ) -> Result<BrowserResourceRuntime, String> {
        let policy = self.browser_host_network_policy_snapshot();
        let mut fetch_config = self.fetch_config().clone();
        let browser_identity = load_inputs
            .browser_identity_override
            .clone()
            .or_else(|| policy.global_browser_identity_override().cloned())
            .unwrap_or_else(|| policy.base_browser_identity().clone());
        fetch_config.set_browser_identity(browser_identity);
        fetch_config.set_http_proxy(
            load_inputs
                .http_proxy_override
                .clone()
                .or_else(|| policy.base_http_proxy().map(str::to_owned)),
        );
        fetch_config.set_http_no_proxy(
            load_inputs
                .http_no_proxy_override
                .clone()
                .or_else(|| policy.base_http_no_proxy().map(str::to_owned)),
        );
        fetch_config.set_tls_verify_host(
            load_inputs
                .tls_verify_host_override
                .unwrap_or(policy.base_tls_verify_host()),
        );
        let storage = load_inputs.resource_storage_handles();
        load_inputs
            .renderer_runtime
            .replace_owned(BrowserResourceRuntimeOwner::new(
                &fetch_config,
                storage.cookie_store,
            ))
            .map_err(|error| format!("browser context resource owner unavailable: {error}"))
    }

    #[cfg(test)]
    pub(crate) fn ensure_cookie_store(
        &mut self,
    ) -> Result<moli_cookie_jar::SharedBrowserCookieStore, String> {
        self.apply_active_engine_fetch_overrides();
        let storage = self.resource_storage_handles();
        self.browser_host_state
            .navigation_owner_mut()
            .ensure_active_cookie_store(storage.into_navigation_storage())
            .map_err(|error| format!("failed to initialize loader: {error}"))
    }

    #[cfg(test)]
    pub(crate) async fn reset_resource_runtime_async(&mut self) {
        let mut engine = self
            .browser_host_state
            .navigation_owner()
            .clone_active_navigation_engine();
        {
            let mut page = self
                .browser_context
                .as_mut()
                .and_then(|bc| bc.active_target.runtime_slot.loaded_page_mut());
            engine
                .reset_resource_runtime_async(page.as_deref_mut())
                .await;
        }
        self.adopt_navigation_engine_for_current_owner(engine)
            .expect("test resource-runtime reset must preserve the current engine owner");
    }

    pub(crate) async fn rebuild_resource_runtime_for_loaded_page_async(&mut self) {
        let storage = self.resource_storage_handles();
        let mut engine = self
            .browser_host_state
            .navigation_owner()
            .clone_active_navigation_engine();
        let rebuild_result = {
            let mut page = self
                .browser_context
                .as_mut()
                .and_then(|bc| bc.active_target.runtime_slot.loaded_page_mut());
            engine
                .rebuild_resource_runtime_for_page_with_storage_async(
                    storage.into_navigation_storage(),
                    page.as_deref_mut(),
                )
                .await
        };
        if rebuild_result.is_err() {
            let mut page = self
                .browser_context
                .as_mut()
                .and_then(|bc| bc.active_target.runtime_slot.loaded_page_mut());
            engine
                .reset_resource_runtime_async(page.as_deref_mut())
                .await;
        }
        if let Err(error) = self.adopt_navigation_engine_for_current_owner(engine) {
            tracing::warn!(
                %error,
                "resource-runtime rebuild engine adoption rejected by Browser Owner"
            );
        }
    }

    pub(crate) fn start_rebuild_resource_runtime_for_session_owner(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<Option<PendingPageCommand>, String> {
        let load_inputs = self.navigation_load_inputs_for_session_owner(session_id);
        let storage = load_inputs.resource_storage_handles();
        let request_client = if self.navigation_load_inputs_use_primary_engine(&load_inputs) {
            self.apply_navigation_load_input_engine_fetch_overrides(&load_inputs);
            self.browser_host_state
                .navigation_owner_mut()
                .rebuild_active_resource_request_client(storage.into_navigation_storage())
                .map_err(|error| format!("failed to rebuild resource runtime: {error}"))?
        } else {
            let mut engine = self.background_navigation_engine_for_load_inputs(&load_inputs);
            let request_client = engine
                .rebuild_resource_request_client_for_navigation_storage(
                    storage.into_navigation_storage(),
                )
                .map_err(|error| format!("failed to rebuild resource runtime: {error}"))?;
            let browser_context_id = load_inputs.browser_context_id.clone().ok_or_else(|| {
                "background resource rebuild has no BrowserContext owner".to_owned()
            })?;
            let target_id = load_inputs
                .root_frame_id
                .clone()
                .ok_or_else(|| "background resource rebuild has no target owner".to_owned())?;
            self.retain_background_navigation_engine(browser_context_id, target_id, engine)?;
            request_client
        };
        let Some(page) = self.resource_runtime_apply_page_for_session_owner(session_id) else {
            return Ok(None);
        };
        page.start_replace_browser_resource_runtime(&request_client.browser_resource_runtime())
            .map(Some)
            .map_err(|error| format!("failed to update page resource runtime: {error}"))
    }

    pub(crate) fn finish_rebuild_resource_runtime_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        completion: CompletedPageCommand,
    ) -> Result<(), String> {
        let Some(mut page) = self.resource_runtime_apply_page_for_session_owner(session_id) else {
            return Ok(());
        };
        page.finish_replace_browser_resource_runtime(completion)
            .map_err(|error| format!("failed to update page resource runtime: {error}"))
    }

    fn resource_runtime_apply_page_for_session_owner(
        &mut self,
        session_id: Option<&str>,
    ) -> Option<moli_core::browser_host::BrowserPageRuntimeLease> {
        if matches!(
            self.session_route(session_id),
            Some(super::CdpSessionRoute::Browser)
        ) {
            return self
                .browser_context
                .as_mut()
                .and_then(|bc| bc.active_target.runtime_slot.loaded_page_mut());
        }
        self.loaded_page_mut_for_protocol_access(session_id).ok()
    }
}
