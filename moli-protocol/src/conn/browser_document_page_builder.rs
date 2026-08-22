use moli_core::{
    browser_host::BrowserNavigationOwner,
    page::RendererMainDocumentCommit,
    runtime::{
        BuiltDocumentPage, CommittedDocumentResourceSource, ExternalRawDocumentBodyStream,
        NavigationEngine, PageVmInitStage, PreparedDocumentPage, RendererPageReservationToken,
        RendererReplyBoundary, RendererReservedServiceWorkerClient,
    },
};
use url::Url;

use super::TargetNavigationLoadInputs;

/// Protocol-side adapter for document construction. Active work calls the
/// Browser Core owner facade; detached background jobs use their standalone
/// engine. The adapter never extracts an engine from `BrowserNavigationOwner`.
#[allow(clippy::too_many_arguments)]
pub(crate) trait BrowserDocumentPageBuilder {
    async fn prepare_protocol_streaming_raw_page(
        &mut self,
        page_reservation: RendererPageReservationToken,
        load_inputs: &TargetNavigationLoadInputs,
        requested_url: Url,
        final_url: Url,
        redirected: bool,
        redirect_count: usize,
        response_status: u16,
        response_headers: Vec<(String, String)>,
        raw_body: ExternalRawDocumentBodyStream,
        stage: PageVmInitStage,
        reply_boundary: RendererReplyBoundary,
        resource_source: CommittedDocumentResourceSource,
        reserved_service_worker_client: Option<RendererReservedServiceWorkerClient>,
        main_document_commit: Option<RendererMainDocumentCommit>,
    ) -> anyhow::Result<PreparedDocumentPage>;

    async fn prepare_protocol_document_page(
        &mut self,
        page_reservation: RendererPageReservationToken,
        load_inputs: &TargetNavigationLoadInputs,
        requested_url: Url,
        final_url: Url,
        redirected: bool,
        redirect_count: usize,
        response_status: u16,
        response_headers: Vec<(String, String)>,
        response_body: String,
        resource_source: CommittedDocumentResourceSource,
        main_document_commit: Option<RendererMainDocumentCommit>,
    ) -> anyhow::Result<PreparedDocumentPage>;

    async fn build_protocol_html_page(
        &mut self,
        load_inputs: &TargetNavigationLoadInputs,
        requested_url: Url,
        final_url: Url,
        redirected: bool,
        redirect_count: usize,
        response_status: u16,
        response_headers: Vec<(String, String)>,
        response_body: String,
        main_document_commit: Option<RendererMainDocumentCommit>,
    ) -> anyhow::Result<BuiltDocumentPage>;
}

macro_rules! impl_browser_document_page_builder {
    ($owner:ty, $prepare_streaming:ident, $prepare_document:ident, $build_html:ident) => {
        impl BrowserDocumentPageBuilder for $owner {
            async fn prepare_protocol_streaming_raw_page(
                &mut self,
                page_reservation: RendererPageReservationToken,
                load_inputs: &TargetNavigationLoadInputs,
                requested_url: Url,
                final_url: Url,
                redirected: bool,
                redirect_count: usize,
                response_status: u16,
                response_headers: Vec<(String, String)>,
                raw_body: ExternalRawDocumentBodyStream,
                stage: PageVmInitStage,
                reply_boundary: RendererReplyBoundary,
                resource_source: CommittedDocumentResourceSource,
                reserved_service_worker_client: Option<RendererReservedServiceWorkerClient>,
                main_document_commit: Option<RendererMainDocumentCommit>,
            ) -> anyhow::Result<PreparedDocumentPage> {
                let (
                    fetch_subresource_interception_enabled,
                    fetch_subresource_interception_resource_type,
                ) = load_inputs.fetch_subresource_interception;
                self.$prepare_streaming(
                    page_reservation,
                    load_inputs.page_storage_handles().into_navigation_storage(),
                    requested_url,
                    final_url.clone(),
                    load_inputs.navigation_initiator_url.clone(),
                    redirected,
                    redirect_count,
                    response_status,
                    response_headers,
                    raw_body,
                    load_inputs.document_start_scripts.clone(),
                    load_inputs.runtime_bindings.clone(),
                    load_inputs
                        .runtime_inspector_session_restore_snapshots
                        .clone(),
                    load_inputs.extra_http_headers.clone(),
                    load_inputs.locale_override.clone(),
                    load_inputs.timezone_override.clone(),
                    load_inputs.script_execution_disabled,
                    load_inputs.bypass_content_security_policy,
                    load_inputs.cpu_throttling_rate,
                    load_inputs.emulated_media.clone(),
                    load_inputs.viewport_surface,
                    load_inputs.network_offline,
                    load_inputs.blocked_url_patterns.clone(),
                    fetch_subresource_interception_enabled,
                    fetch_subresource_interception_resource_type,
                    stage,
                    reply_boundary,
                    load_inputs.root_frame_id.clone(),
                    resource_source,
                    reserved_service_worker_client,
                    main_document_commit,
                )
                .await
            }

            async fn prepare_protocol_document_page(
                &mut self,
                page_reservation: RendererPageReservationToken,
                load_inputs: &TargetNavigationLoadInputs,
                requested_url: Url,
                final_url: Url,
                redirected: bool,
                redirect_count: usize,
                response_status: u16,
                response_headers: Vec<(String, String)>,
                response_body: String,
                resource_source: CommittedDocumentResourceSource,
                main_document_commit: Option<RendererMainDocumentCommit>,
            ) -> anyhow::Result<PreparedDocumentPage> {
                let (
                    fetch_subresource_interception_enabled,
                    fetch_subresource_interception_resource_type,
                ) = load_inputs.fetch_subresource_interception;
                self.$prepare_document(
                    page_reservation,
                    load_inputs.page_storage_handles().into_navigation_storage(),
                    requested_url,
                    final_url.clone(),
                    load_inputs.navigation_initiator_url.clone(),
                    redirected,
                    redirect_count,
                    response_status,
                    response_headers,
                    response_body,
                    load_inputs.document_start_scripts.clone(),
                    load_inputs.runtime_bindings.clone(),
                    load_inputs
                        .runtime_inspector_session_restore_snapshots
                        .clone(),
                    load_inputs.extra_http_headers.clone(),
                    load_inputs.locale_override.clone(),
                    load_inputs.timezone_override.clone(),
                    load_inputs.script_execution_disabled,
                    load_inputs.bypass_content_security_policy,
                    load_inputs.cpu_throttling_rate,
                    load_inputs.emulated_media.clone(),
                    load_inputs.viewport_surface,
                    load_inputs.network_offline,
                    load_inputs.blocked_url_patterns.clone(),
                    fetch_subresource_interception_enabled,
                    fetch_subresource_interception_resource_type,
                    load_inputs.root_frame_id.clone(),
                    resource_source,
                    main_document_commit,
                )
                .await
            }

            async fn build_protocol_html_page(
                &mut self,
                load_inputs: &TargetNavigationLoadInputs,
                requested_url: Url,
                final_url: Url,
                redirected: bool,
                redirect_count: usize,
                response_status: u16,
                response_headers: Vec<(String, String)>,
                response_body: String,
                main_document_commit: Option<RendererMainDocumentCommit>,
            ) -> anyhow::Result<BuiltDocumentPage> {
                let (
                    fetch_subresource_interception_enabled,
                    fetch_subresource_interception_resource_type,
                ) = load_inputs.fetch_subresource_interception;
                self.$build_html(
                    load_inputs.page_storage_handles().into_navigation_storage(),
                    requested_url,
                    final_url.clone(),
                    load_inputs.navigation_initiator_url.clone(),
                    redirected,
                    redirect_count,
                    response_status,
                    response_headers,
                    response_body,
                    load_inputs.document_start_scripts.clone(),
                    load_inputs.runtime_bindings.clone(),
                    load_inputs
                        .runtime_inspector_session_restore_snapshots
                        .clone(),
                    load_inputs.extra_http_headers.clone(),
                    load_inputs.locale_override.clone(),
                    load_inputs.timezone_override.clone(),
                    load_inputs.script_execution_disabled,
                    load_inputs.bypass_content_security_policy,
                    load_inputs.cpu_throttling_rate,
                    load_inputs.emulated_media.clone(),
                    load_inputs.viewport_surface,
                    load_inputs.network_offline,
                    load_inputs.blocked_url_patterns.clone(),
                    fetch_subresource_interception_enabled,
                    fetch_subresource_interception_resource_type,
                    load_inputs.root_frame_id.clone(),
                    main_document_commit,
                )
                .await
            }
        }
    };
}

impl_browser_document_page_builder!(
    NavigationEngine,
    prepare_streaming_raw_page_from_external_body_with_storage_and_inspector_session_restores_async,
    prepare_document_page_from_response_with_storage_and_inspector_session_restores_async,
    build_html_page_from_response_with_storage_and_inspector_session_restores_async
);
impl_browser_document_page_builder!(
    BrowserNavigationOwner,
    prepare_active_streaming_raw_page,
    prepare_active_document_page_from_response,
    build_active_html_page_from_response
);
