use anyhow::Result;
use moli_fetch::{BrowserNavigationRequestKind, FetchConfig, NetworkFetchResult};
use url::Url;

use crate::{
    OptionalResourceFetchMask,
    network::ResourceRequestClient,
    page::{
        CompletedPageCommand, NavigationResponse, Page, PendingPageCommand,
        SubresourceAuthCredentials,
    },
    runtime::{
        NavigationEngine, NavigationResourceStorageHandles, NavigationRuntimeConfig,
        RendererBrowserContextRuntime, RendererBrowserContextRuntimeOwnerAccess,
        RendererPageReservationToken,
    },
};

use super::{BrowserNavigationOwner, BrowserPageFetchConfiguration};

impl BrowserNavigationOwner {
    pub fn active_fetch_config(&self) -> &FetchConfig {
        self.active_engine().fetch_config()
    }

    pub fn active_runtime_config(&self) -> NavigationRuntimeConfig {
        self.active_engine().runtime_config()
    }

    pub fn active_optional_resource_fetch_mask(&self) -> OptionalResourceFetchMask {
        self.active_engine().optional_resource_fetch_mask()
    }

    pub fn active_subframe_loading_enabled(&self) -> bool {
        self.active_engine().subframe_loading_enabled()
    }

    pub fn active_image_fetch_enabled(&self) -> bool {
        self.active_engine().image_fetch_enabled()
    }

    pub fn active_browser_context_runtime(&self) -> RendererBrowserContextRuntime {
        self.active_engine().browser_context_runtime()
    }

    pub fn active_browser_context_owner_access(&self) -> RendererBrowserContextRuntimeOwnerAccess {
        self.active_engine().browser_context_owner_access()
    }

    /// Clones the selected engine into move-owned async work.
    ///
    /// The clone shares the renderer owner but does not borrow Browser Host
    /// state while network or renderer work is pending. A caller that intends
    /// engine-local configuration changes to become authoritative must hand it
    /// back through an exact Core adoption boundary; an operation-only clone
    /// may be dropped while the selected engine keeps the renderer alive.
    pub fn clone_active_navigation_engine(&self) -> NavigationEngine {
        self.active_engine().clone()
    }

    pub fn active_runtime_shares_state_with(
        &self,
        runtime: &RendererBrowserContextRuntimeOwnerAccess,
    ) -> bool {
        self.active_engine()
            .browser_context_runtime()
            .shares_state_with(&runtime.runtime())
    }

    pub fn new_engine_sharing_active_renderer_owner(
        &self,
        fetch_config: FetchConfig,
    ) -> Result<NavigationEngine> {
        let mut runtime_config = self.active_runtime_config();
        *runtime_config.fetch_config_mut() = fetch_config;
        NavigationEngine::new_with_runtime_config_and_shared_renderer_owner(
            runtime_config,
            self.active_engine(),
        )
    }

    pub fn new_engine_for_browser_context_runtime(
        &self,
        fetch_config: FetchConfig,
        runtime: RendererBrowserContextRuntimeOwnerAccess,
    ) -> Result<NavigationEngine> {
        let mut runtime_config = self.active_runtime_config();
        *runtime_config.fetch_config_mut() = fetch_config;
        NavigationEngine::new_with_runtime_config_and_browser_context_access(
            runtime_config,
            runtime,
        )
    }

    pub fn detached_engine_shares_active_renderer_owner(&self, engine: &NavigationEngine) -> bool {
        engine.shares_renderer_owner_with(self.active_engine())
    }

    pub fn configure_active_fetch(&mut self, configuration: BrowserPageFetchConfiguration) {
        let engine = self.active_engine_mut();
        engine.set_browser_identity_override(configuration.browser_identity);
        engine.set_http_proxy_override(configuration.http_proxy);
        engine.set_http_no_proxy_override(configuration.http_no_proxy);
        engine.set_tls_verify_host(configuration.tls_verify_host);
        engine.set_bypass_service_worker(configuration.bypass_service_worker);
    }

    pub fn set_active_bypass_service_worker(&mut self, bypass: bool) {
        self.active_engine_mut().set_bypass_service_worker(bypass);
    }

    pub fn active_resource_request_client(&self) -> Option<ResourceRequestClient> {
        self.active_engine().resource_request_client()
    }

    pub fn ensure_active_resource_runtime_ready(
        &mut self,
        storage: NavigationResourceStorageHandles,
    ) -> Result<()> {
        self.active_engine_mut()
            .ensure_resource_runtime_ready_for_navigation_storage(storage)
    }

    pub fn rebuild_active_resource_request_client(
        &mut self,
        storage: NavigationResourceStorageHandles,
    ) -> Result<ResourceRequestClient> {
        self.active_engine_mut()
            .rebuild_resource_request_client_for_navigation_storage(storage)
    }

    pub fn ensure_active_cookie_store(
        &mut self,
        storage: NavigationResourceStorageHandles,
    ) -> Result<moli_cookie_jar::SharedBrowserCookieStore> {
        self.active_engine_mut()
            .ensure_cookie_store_for_navigation_storage(storage)
    }

    pub async fn reset_active_resource_runtime(&mut self, page: Option<&mut Page>) {
        self.active_engine_mut()
            .reset_resource_runtime_async(page)
            .await;
    }

    pub fn reset_active_resource_runtime_without_loaded_page(&mut self) {
        self.active_engine_mut()
            .reset_resource_runtime_without_loaded_page();
    }

    pub async fn rebuild_active_resource_runtime_for_page(
        &mut self,
        storage: NavigationResourceStorageHandles,
        page: Option<&mut Page>,
    ) -> Result<()> {
        self.active_engine_mut()
            .rebuild_resource_runtime_for_page_with_storage_async(storage, page)
            .await
    }

    pub fn reserve_active_page_for_creation(&self) -> RendererPageReservationToken {
        self.active_engine().reserve_page_for_creation()
    }

    pub fn start_active_page_child_frame_lifecycle_work(
        &mut self,
        storage: NavigationResourceStorageHandles,
        page: &Page,
        timeout: std::time::Duration,
    ) -> Result<PendingPageCommand> {
        self.active_engine_mut()
            .start_page_child_frame_lifecycle_work_with_storage_best_effort(storage, page, timeout)
    }

    pub fn complete_active_page_child_frame_lifecycle_work(
        &mut self,
        page: &mut Page,
        completion: CompletedPageCommand,
    ) -> Result<(bool, crate::page::RendererCommandTurnOutput)> {
        self.active_engine_mut()
            .complete_page_child_frame_lifecycle_work_best_effort(page, completion)
    }

    pub fn ensure_active_navigation_response_status(
        &self,
        raw_url: &str,
        status: u16,
        allow_auth_challenge: bool,
    ) -> Result<()> {
        self.active_engine().ensure_navigation_response_status(
            raw_url,
            status,
            allow_auth_challenge,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn fetch_active_navigation_response(
        &mut self,
        storage: NavigationResourceStorageHandles,
        initiator_url: Option<&Url>,
        browser_navigation_kind: BrowserNavigationRequestKind,
        infer_referrer_from_initiator: bool,
        method: &str,
        raw_url: &str,
        body: Option<String>,
        request_headers: Vec<(String, String)>,
        auth: Option<SubresourceAuthCredentials>,
    ) -> Result<NetworkFetchResult<NavigationResponse>> {
        self.active_engine_mut()
            .fetch_navigation_response_with_storage_async(
                storage,
                initiator_url,
                browser_navigation_kind,
                infer_referrer_from_initiator,
                method,
                raw_url,
                body,
                request_headers,
                auth,
            )
            .await
    }

    pub fn active_renderer_owner_id_for_diagnostics(&self) -> u64 {
        self.active_engine().renderer_owner_id_for_diagnostics()
    }

    pub fn retained_renderer_owner_ids_for_diagnostics(&self) -> impl Iterator<Item = u64> + '_ {
        self.target_engines
            .retained_renderer_owner_ids(&self.target_runtimes, self.selected_target_engine_owner())
    }

    pub fn active_document_isolate_model_for_diagnostics(&self) -> &'static str {
        self.active_engine()
            .document_isolate_model_for_diagnostics()
    }

    pub fn active_document_isolate_accounting_for_diagnostics(
        &self,
    ) -> crate::page::RendererDocumentIsolateAccountingDiagnostics {
        self.active_engine()
            .document_isolate_accounting_for_diagnostics()
    }
}
