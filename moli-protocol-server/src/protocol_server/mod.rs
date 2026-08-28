use std::{net::SocketAddr, sync::Arc};

use anyhow::Result;
use anyhow::{Context, anyhow};
use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, header},
    middleware::{self, Next},
    response::Response,
    routing::{any, get, post, put},
    serve::ListenerExt,
};
use moli_cookie_jar::StoredCookie;
use moli_core::{
    LayoutPolicy, OptionalResourceFetchMask,
    browser_host::BrowserTargetIdAllocator,
    runtime::{NavigationRuntimeConfig, storage_partition::StoragePartitionState},
};
use moli_fetch::FetchConfig;
#[cfg(test)]
use moli_protocol::DEFAULT_CDP_TAB_TARGET_ID;
use moli_protocol::{CdpInitialStoragePartition, DEFAULT_CDP_PAGE_TARGET_ID};
use tokio::net::TcpListener;
use tracing::info;

pub use crate::config::ServerConfig;

mod cdp;
mod cdp_agent_host;
mod cdp_owner;
mod cdp_socket;
mod devtools_host_service;
mod protocol_local_executor;
mod tcp_options;
mod webdriver_bidi;
mod webdriver_classic;
mod webdriver_files;

use cdp_agent_host::SharedCdpAgentHostDirectory;
use cdp_owner::SharedCdpOwnerRegistry;
use webdriver_bidi::SharedBidiSessionRegistry;
use webdriver_classic::SharedClassicSessionRegistry;

const DEFAULT_BROWSER_ID: &str = "moli-browser";
const DEFAULT_TARGET_ID: &str = DEFAULT_CDP_PAGE_TARGET_ID;
#[cfg(test)]
const DEFAULT_TAB_TARGET_ID: &str = DEFAULT_CDP_TAB_TARGET_ID;
const DEFAULT_TARGET_URL: &str = "about:blank";

#[derive(Debug, Clone)]
pub struct ProtocolServer {
    config: ServerConfig,
    storage_partition: Arc<StoragePartitionState>,
    navigation_runtime_config: NavigationRuntimeConfig,
}

impl ProtocolServer {
    pub fn new(config: ServerConfig) -> Self {
        Self::new_with_initial_cookies(config, Vec::new())
    }

    pub fn new_with_initial_cookies(
        config: ServerConfig,
        initial_cookies: Vec<StoredCookie>,
    ) -> Self {
        let storage_partition = Arc::new(
            StoragePartitionState::open(None).expect("in-memory storage partition should open"),
        );
        storage_partition
            .import_cookies(initial_cookies)
            .expect("in-memory cookie import should succeed");
        Self::new_with_storage_partition(config, storage_partition)
    }

    pub fn new_with_storage_partition(
        config: ServerConfig,
        storage_partition: Arc<StoragePartitionState>,
    ) -> Self {
        Self::new_with_storage_partition_fetch_config_and_resource_loading(
            config,
            storage_partition,
            FetchConfig::default(),
            OptionalResourceFetchMask::NONE,
            true,
        )
    }

    pub fn new_with_storage_partition_fetch_config_and_resource_loading(
        config: ServerConfig,
        storage_partition: Arc<StoragePartitionState>,
        fetch_config: FetchConfig,
        optional_resource_fetch_mask: OptionalResourceFetchMask,
        subframe_loading_enabled: bool,
    ) -> Self {
        Self::new_with_storage_partition_and_runtime_config(
            config,
            storage_partition,
            NavigationRuntimeConfig::new(
                fetch_config,
                optional_resource_fetch_mask,
                subframe_loading_enabled,
                LayoutPolicy::default(),
            ),
        )
    }

    pub fn new_with_storage_partition_and_runtime_config(
        config: ServerConfig,
        storage_partition: Arc<StoragePartitionState>,
        mut navigation_runtime_config: NavigationRuntimeConfig,
    ) -> Self {
        if navigation_runtime_config
            .fetch_config()
            .http_cache_dir()
            .is_none()
            && let Some(http_cache_root) = storage_partition.http_cache_root()
        {
            navigation_runtime_config
                .fetch_config_mut()
                .set_http_cache_dir(Some(http_cache_root.display().to_string()));
        }
        Self {
            config,
            storage_partition,
            navigation_runtime_config,
        }
    }

    pub async fn serve(&self) -> Result<()> {
        let listener = TcpListener::bind(self.config.bind_target())
            .await
            .with_context(|| {
                anyhow!(
                    "failed to bind protocol server to {}:{}",
                    self.config.host,
                    self.config.port
                )
            })?;
        let addr = listener
            .local_addr()
            .context("failed to read bound protocol socket address")?;

        info!(%addr, "protocol server listening");

        let app_state = AppState::new_with_storage_partition_and_runtime_config(
            addr,
            self.storage_partition.clone(),
            self.navigation_runtime_config.clone(),
        )?;

        let cdp_owner_registry = app_state.cdp_owner_registry.clone();
        let app = build_router(app_state);

        let listener = listener.tap_io(|tcp_stream| {
            tcp_options::configure_accepted_protocol_stream(tcp_stream);
        });
        let result = axum::serve(listener, app).await;
        cdp_owner_registry.shutdown().await;
        result.context("protocol server failed")
    }
}

fn build_router(app_state: AppState) -> Router {
    Router::new()
        .route("/status", get(webdriver_classic::webdriver_classic_status))
        .route("/status/", get(webdriver_classic::webdriver_classic_status))
        .route("/json", any(cdp::json_list))
        .route("/json/list", any(cdp::json_list))
        .route("/json/list/", any(cdp::json_list))
        .route("/json/version", get(cdp::json_version))
        .route("/json/version/", get(cdp::json_version))
        .route("/json/protocol", get(cdp::json_protocol))
        .route("/json/protocol/", get(cdp::json_protocol))
        .route("/json/new", put(cdp::json_new_target))
        .route("/json/new/", put(cdp::json_new_target))
        .route("/json/activate/{target_id}", get(cdp::json_activate_target))
        .route(
            "/json/activate/{target_id}/",
            get(cdp::json_activate_target),
        )
        .route("/json/close/{target_id}", get(cdp::json_close_target))
        .route("/json/close/{target_id}/", get(cdp::json_close_target))
        .route(
            "/devtools/browser/{browser_id}",
            get(cdp::ws_browser_upgrade_handler),
        )
        .route(
            "/devtools/browser/{browser_id}/",
            get(cdp::ws_browser_upgrade_handler),
        )
        .route(
            "/devtools/page/{target_id}",
            get(cdp::ws_target_upgrade_handler),
        )
        .route(
            "/devtools/page/{target_id}/",
            get(cdp::ws_target_upgrade_handler),
        )
        .route(
            "/session",
            get(webdriver_bidi::ws_bidi_session_upgrade_handler)
                .post(webdriver_classic::webdriver_classic_new_session),
        )
        .route(
            "/session/",
            get(webdriver_bidi::ws_bidi_session_upgrade_handler)
                .post(webdriver_classic::webdriver_classic_new_session),
        )
        .route(
            "/session/{session_id}",
            get(webdriver_bidi::ws_bidi_existing_session_upgrade_handler)
                .delete(webdriver_classic::webdriver_classic_delete_session),
        )
        .route(
            "/session/{session_id}/",
            get(webdriver_bidi::ws_bidi_existing_session_upgrade_handler)
                .delete(webdriver_classic::webdriver_classic_delete_session),
        )
        .route(
            "/session/{session_id}/url",
            get(webdriver_classic::webdriver_classic_get_url)
                .post(webdriver_classic::webdriver_classic_navigate),
        )
        .route(
            "/session/{session_id}/url/",
            get(webdriver_classic::webdriver_classic_get_url)
                .post(webdriver_classic::webdriver_classic_navigate),
        )
        .route(
            "/session/{session_id}/title",
            get(webdriver_classic::webdriver_classic_get_title),
        )
        .route(
            "/session/{session_id}/title/",
            get(webdriver_classic::webdriver_classic_get_title),
        )
        .route(
            "/session/{session_id}/timeouts",
            get(webdriver_classic::webdriver_classic_get_timeouts)
                .post(webdriver_classic::webdriver_classic_set_timeouts),
        )
        .route(
            "/session/{session_id}/timeouts/",
            get(webdriver_classic::webdriver_classic_get_timeouts)
                .post(webdriver_classic::webdriver_classic_set_timeouts),
        )
        .route(
            "/session/{session_id}/source",
            get(webdriver_classic::webdriver_classic_get_source),
        )
        .route(
            "/session/{session_id}/source/",
            get(webdriver_classic::webdriver_classic_get_source),
        )
        .route(
            "/session/{session_id}/screenshot",
            get(webdriver_classic::webdriver_classic_take_screenshot),
        )
        .route(
            "/session/{session_id}/screenshot/",
            get(webdriver_classic::webdriver_classic_take_screenshot),
        )
        .route(
            "/session/{session_id}/print",
            post(webdriver_classic::webdriver_classic_print_page),
        )
        .route(
            "/session/{session_id}/print/",
            post(webdriver_classic::webdriver_classic_print_page),
        )
        .route(
            "/session/{session_id}/file",
            post(webdriver_classic::webdriver_classic_upload_file),
        )
        .route(
            "/session/{session_id}/file/",
            post(webdriver_classic::webdriver_classic_upload_file),
        )
        .route(
            "/session/{session_id}/se/file",
            post(webdriver_classic::webdriver_classic_upload_file),
        )
        .route(
            "/session/{session_id}/se/file/",
            post(webdriver_classic::webdriver_classic_upload_file),
        )
        .route(
            "/session/{session_id}/se/files",
            get(webdriver_classic::webdriver_classic_get_downloadable_files)
                .post(webdriver_classic::webdriver_classic_download_file)
                .delete(webdriver_classic::webdriver_classic_delete_downloadable_files),
        )
        .route(
            "/session/{session_id}/se/files/",
            get(webdriver_classic::webdriver_classic_get_downloadable_files)
                .post(webdriver_classic::webdriver_classic_download_file)
                .delete(webdriver_classic::webdriver_classic_delete_downloadable_files),
        )
        .route(
            "/session/{session_id}/moli/service-workers",
            get(webdriver_classic::webdriver_classic_get_service_workers),
        )
        .route(
            "/session/{session_id}/moli/service-workers/",
            get(webdriver_classic::webdriver_classic_get_service_workers),
        )
        .route(
            "/session/{session_id}/window",
            get(webdriver_classic::webdriver_classic_get_window)
                .post(webdriver_classic::webdriver_classic_switch_window)
                .delete(webdriver_classic::webdriver_classic_close_window),
        )
        .route(
            "/session/{session_id}/window/",
            get(webdriver_classic::webdriver_classic_get_window)
                .post(webdriver_classic::webdriver_classic_switch_window)
                .delete(webdriver_classic::webdriver_classic_close_window),
        )
        .route(
            "/session/{session_id}/window/handles",
            get(webdriver_classic::webdriver_classic_get_window_handles),
        )
        .route(
            "/session/{session_id}/window/handles/",
            get(webdriver_classic::webdriver_classic_get_window_handles),
        )
        .route(
            "/session/{session_id}/window/rect",
            get(webdriver_classic::webdriver_classic_get_window_rect)
                .post(webdriver_classic::webdriver_classic_set_window_rect),
        )
        .route(
            "/session/{session_id}/window/rect/",
            get(webdriver_classic::webdriver_classic_get_window_rect)
                .post(webdriver_classic::webdriver_classic_set_window_rect),
        )
        .route(
            "/session/{session_id}/window/maximize",
            post(webdriver_classic::webdriver_classic_maximize_window),
        )
        .route(
            "/session/{session_id}/window/maximize/",
            post(webdriver_classic::webdriver_classic_maximize_window),
        )
        .route(
            "/session/{session_id}/window/minimize",
            post(webdriver_classic::webdriver_classic_minimize_window),
        )
        .route(
            "/session/{session_id}/window/minimize/",
            post(webdriver_classic::webdriver_classic_minimize_window),
        )
        .route(
            "/session/{session_id}/window/fullscreen",
            post(webdriver_classic::webdriver_classic_fullscreen_window),
        )
        .route(
            "/session/{session_id}/window/fullscreen/",
            post(webdriver_classic::webdriver_classic_fullscreen_window),
        )
        .route(
            "/session/{session_id}/window/new",
            post(webdriver_classic::webdriver_classic_new_window),
        )
        .route(
            "/session/{session_id}/window/new/",
            post(webdriver_classic::webdriver_classic_new_window),
        )
        .route(
            "/session/{session_id}/frame",
            post(webdriver_classic::webdriver_classic_switch_frame),
        )
        .route(
            "/session/{session_id}/frame/",
            post(webdriver_classic::webdriver_classic_switch_frame),
        )
        .route(
            "/session/{session_id}/frame/parent",
            post(webdriver_classic::webdriver_classic_switch_parent_frame),
        )
        .route(
            "/session/{session_id}/frame/parent/",
            post(webdriver_classic::webdriver_classic_switch_parent_frame),
        )
        .route(
            "/session/{session_id}/alert/text",
            get(webdriver_classic::webdriver_classic_get_alert_text)
                .post(webdriver_classic::webdriver_classic_send_alert_text),
        )
        .route(
            "/session/{session_id}/alert/text/",
            get(webdriver_classic::webdriver_classic_get_alert_text)
                .post(webdriver_classic::webdriver_classic_send_alert_text),
        )
        .route(
            "/session/{session_id}/alert/accept",
            post(webdriver_classic::webdriver_classic_accept_alert),
        )
        .route(
            "/session/{session_id}/alert/accept/",
            post(webdriver_classic::webdriver_classic_accept_alert),
        )
        .route(
            "/session/{session_id}/alert/dismiss",
            post(webdriver_classic::webdriver_classic_dismiss_alert),
        )
        .route(
            "/session/{session_id}/alert/dismiss/",
            post(webdriver_classic::webdriver_classic_dismiss_alert),
        )
        .route(
            "/session/{session_id}/refresh",
            post(webdriver_classic::webdriver_classic_refresh),
        )
        .route(
            "/session/{session_id}/refresh/",
            post(webdriver_classic::webdriver_classic_refresh),
        )
        .route(
            "/session/{session_id}/back",
            post(webdriver_classic::webdriver_classic_back),
        )
        .route(
            "/session/{session_id}/back/",
            post(webdriver_classic::webdriver_classic_back),
        )
        .route(
            "/session/{session_id}/forward",
            post(webdriver_classic::webdriver_classic_forward),
        )
        .route(
            "/session/{session_id}/forward/",
            post(webdriver_classic::webdriver_classic_forward),
        )
        .route(
            "/session/{session_id}/execute/sync",
            post(webdriver_classic::webdriver_classic_execute_sync),
        )
        .route(
            "/session/{session_id}/execute/sync/",
            post(webdriver_classic::webdriver_classic_execute_sync),
        )
        .route(
            "/session/{session_id}/execute/async",
            post(webdriver_classic::webdriver_classic_execute_async),
        )
        .route(
            "/session/{session_id}/execute/async/",
            post(webdriver_classic::webdriver_classic_execute_async),
        )
        .route(
            "/session/{session_id}/element",
            post(webdriver_classic::webdriver_classic_find_element),
        )
        .route(
            "/session/{session_id}/element/",
            post(webdriver_classic::webdriver_classic_find_element),
        )
        .route(
            "/session/{session_id}/elements",
            post(webdriver_classic::webdriver_classic_find_elements),
        )
        .route(
            "/session/{session_id}/elements/",
            post(webdriver_classic::webdriver_classic_find_elements),
        )
        .route(
            "/session/{session_id}/element/active",
            get(webdriver_classic::webdriver_classic_get_active_element),
        )
        .route(
            "/session/{session_id}/element/active/",
            get(webdriver_classic::webdriver_classic_get_active_element),
        )
        .route(
            "/session/{session_id}/element/{element_id}/element",
            post(webdriver_classic::webdriver_classic_find_child_element),
        )
        .route(
            "/session/{session_id}/element/{element_id}/element/",
            post(webdriver_classic::webdriver_classic_find_child_element),
        )
        .route(
            "/session/{session_id}/element/{element_id}/elements",
            post(webdriver_classic::webdriver_classic_find_child_elements),
        )
        .route(
            "/session/{session_id}/element/{element_id}/elements/",
            post(webdriver_classic::webdriver_classic_find_child_elements),
        )
        .route(
            "/session/{session_id}/element/{element_id}/shadow",
            get(webdriver_classic::webdriver_classic_get_element_shadow_root),
        )
        .route(
            "/session/{session_id}/element/{element_id}/shadow/",
            get(webdriver_classic::webdriver_classic_get_element_shadow_root),
        )
        .route(
            "/session/{session_id}/shadow/{shadow_root_id}/element",
            post(webdriver_classic::webdriver_classic_find_shadow_element),
        )
        .route(
            "/session/{session_id}/shadow/{shadow_root_id}/element/",
            post(webdriver_classic::webdriver_classic_find_shadow_element),
        )
        .route(
            "/session/{session_id}/shadow/{shadow_root_id}/elements",
            post(webdriver_classic::webdriver_classic_find_shadow_elements),
        )
        .route(
            "/session/{session_id}/shadow/{shadow_root_id}/elements/",
            post(webdriver_classic::webdriver_classic_find_shadow_elements),
        )
        .route(
            "/session/{session_id}/element/{element_id}/attribute/{name}",
            get(webdriver_classic::webdriver_classic_get_element_attribute),
        )
        .route(
            "/session/{session_id}/element/{element_id}/attribute/{name}/",
            get(webdriver_classic::webdriver_classic_get_element_attribute),
        )
        .route(
            "/session/{session_id}/element/{element_id}/text",
            get(webdriver_classic::webdriver_classic_get_element_text),
        )
        .route(
            "/session/{session_id}/element/{element_id}/text/",
            get(webdriver_classic::webdriver_classic_get_element_text),
        )
        .route(
            "/session/{session_id}/element/{element_id}/name",
            get(webdriver_classic::webdriver_classic_get_element_tag_name),
        )
        .route(
            "/session/{session_id}/element/{element_id}/name/",
            get(webdriver_classic::webdriver_classic_get_element_tag_name),
        )
        .route(
            "/session/{session_id}/element/{element_id}/equals/{other_element_id}",
            get(webdriver_classic::webdriver_classic_element_equals),
        )
        .route(
            "/session/{session_id}/element/{element_id}/equals/{other_element_id}/",
            get(webdriver_classic::webdriver_classic_element_equals),
        )
        .route(
            "/session/{session_id}/element/{element_id}/enabled",
            get(webdriver_classic::webdriver_classic_is_element_enabled),
        )
        .route(
            "/session/{session_id}/element/{element_id}/enabled/",
            get(webdriver_classic::webdriver_classic_is_element_enabled),
        )
        .route(
            "/session/{session_id}/element/{element_id}/displayed",
            get(webdriver_classic::webdriver_classic_is_element_displayed),
        )
        .route(
            "/session/{session_id}/element/{element_id}/displayed/",
            get(webdriver_classic::webdriver_classic_is_element_displayed),
        )
        .route(
            "/session/{session_id}/element/{element_id}/selected",
            get(webdriver_classic::webdriver_classic_is_element_selected),
        )
        .route(
            "/session/{session_id}/element/{element_id}/selected/",
            get(webdriver_classic::webdriver_classic_is_element_selected),
        )
        .route(
            "/session/{session_id}/element/{element_id}/rect",
            get(webdriver_classic::webdriver_classic_get_element_rect),
        )
        .route(
            "/session/{session_id}/element/{element_id}/rect/",
            get(webdriver_classic::webdriver_classic_get_element_rect),
        )
        .route(
            "/session/{session_id}/element/{element_id}/screenshot",
            get(webdriver_classic::webdriver_classic_take_element_screenshot),
        )
        .route(
            "/session/{session_id}/element/{element_id}/screenshot/",
            get(webdriver_classic::webdriver_classic_take_element_screenshot),
        )
        .route(
            "/session/{session_id}/element/{element_id}/css/{property_name}",
            get(webdriver_classic::webdriver_classic_get_element_css_value),
        )
        .route(
            "/session/{session_id}/element/{element_id}/css/{property_name}/",
            get(webdriver_classic::webdriver_classic_get_element_css_value),
        )
        .route(
            "/session/{session_id}/element/{element_id}/computedlabel",
            get(webdriver_classic::webdriver_classic_get_element_computed_label),
        )
        .route(
            "/session/{session_id}/element/{element_id}/computedlabel/",
            get(webdriver_classic::webdriver_classic_get_element_computed_label),
        )
        .route(
            "/session/{session_id}/element/{element_id}/computedrole",
            get(webdriver_classic::webdriver_classic_get_element_computed_role),
        )
        .route(
            "/session/{session_id}/element/{element_id}/computedrole/",
            get(webdriver_classic::webdriver_classic_get_element_computed_role),
        )
        .route(
            "/session/{session_id}/element/{element_id}/property/{name}",
            get(webdriver_classic::webdriver_classic_get_element_property),
        )
        .route(
            "/session/{session_id}/element/{element_id}/property/{name}/",
            get(webdriver_classic::webdriver_classic_get_element_property),
        )
        .route(
            "/session/{session_id}/element/{element_id}/clear",
            post(webdriver_classic::webdriver_classic_clear_element),
        )
        .route(
            "/session/{session_id}/element/{element_id}/clear/",
            post(webdriver_classic::webdriver_classic_clear_element),
        )
        .route(
            "/session/{session_id}/element/{element_id}/click",
            post(webdriver_classic::webdriver_classic_click_element),
        )
        .route(
            "/session/{session_id}/element/{element_id}/click/",
            post(webdriver_classic::webdriver_classic_click_element),
        )
        .route(
            "/session/{session_id}/element/{element_id}/value",
            post(webdriver_classic::webdriver_classic_send_keys_to_element),
        )
        .route(
            "/session/{session_id}/element/{element_id}/value/",
            post(webdriver_classic::webdriver_classic_send_keys_to_element),
        )
        .route(
            "/session/{session_id}/actions",
            post(webdriver_classic::webdriver_classic_perform_actions)
                .delete(webdriver_classic::webdriver_classic_release_actions),
        )
        .route(
            "/session/{session_id}/actions/",
            post(webdriver_classic::webdriver_classic_perform_actions)
                .delete(webdriver_classic::webdriver_classic_release_actions),
        )
        .route(
            "/session/{session_id}/cookie",
            get(webdriver_classic::webdriver_classic_get_cookies)
                .post(webdriver_classic::webdriver_classic_add_cookie)
                .delete(webdriver_classic::webdriver_classic_delete_all_cookies),
        )
        .route(
            "/session/{session_id}/cookie/",
            get(webdriver_classic::webdriver_classic_get_cookies)
                .post(webdriver_classic::webdriver_classic_add_cookie)
                .delete(webdriver_classic::webdriver_classic_delete_all_cookies),
        )
        .route(
            "/session/{session_id}/cookie/{name}",
            get(webdriver_classic::webdriver_classic_get_named_cookie)
                .delete(webdriver_classic::webdriver_classic_delete_cookie),
        )
        .route(
            "/session/{session_id}/cookie/{name}/",
            get(webdriver_classic::webdriver_classic_get_named_cookie)
                .delete(webdriver_classic::webdriver_classic_delete_cookie),
        )
        .with_state(app_state)
        .layer(middleware::from_fn(
            classic_webdriver_response_headers_middleware,
        ))
}

async fn classic_webdriver_response_headers_middleware(
    request: Request<Body>,
    next: Next,
) -> Response {
    let should_apply = is_classic_webdriver_http_path(request.uri().path())
        && !is_websocket_upgrade_request(&request);
    let mut response = next.run(request).await;
    if should_apply && is_classic_webdriver_json_response(&response) {
        let headers = response.headers_mut();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
    }
    response
}

fn is_classic_webdriver_json_response(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split_once(';')
                .map_or(value, |(media_type, _)| media_type)
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
}

fn is_classic_webdriver_http_path(path: &str) -> bool {
    path == "/status"
        || path == "/status/"
        || path == "/session"
        || path == "/session/"
        || path.starts_with("/session/")
}

fn is_websocket_upgrade_request(request: &Request<Body>) -> bool {
    request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"))
}

#[derive(Clone)]
struct AppState {
    browser_ws_url: String,
    page_ws_url: String,
    bidi_ws_url: String,
    bidi_session_registry: SharedBidiSessionRegistry,
    classic_session_registry: SharedClassicSessionRegistry,
    cdp_agent_host_directory: SharedCdpAgentHostDirectory,
    cdp_owner_registry: SharedCdpOwnerRegistry,
    devtools_frontend_url: String,
    storage_partition: Arc<StoragePartitionState>,
    fetch_config: FetchConfig,
    optional_resource_fetch_mask: OptionalResourceFetchMask,
    subframe_loading_enabled: bool,
    layout_policy: LayoutPolicy,
}

impl AppState {
    #[cfg(test)]
    fn new_with_storage_partition(
        addr: SocketAddr,
        storage_partition: Arc<StoragePartitionState>,
        fetch_config: FetchConfig,
        optional_resource_fetch_mask: OptionalResourceFetchMask,
        subframe_loading_enabled: bool,
    ) -> anyhow::Result<Self> {
        Self::new_with_storage_partition_and_runtime_config(
            addr,
            storage_partition,
            NavigationRuntimeConfig::new(
                fetch_config,
                optional_resource_fetch_mask,
                subframe_loading_enabled,
                LayoutPolicy::default(),
            ),
        )
    }

    fn new_with_storage_partition_and_runtime_config(
        addr: SocketAddr,
        storage_partition: Arc<StoragePartitionState>,
        navigation_runtime_config: NavigationRuntimeConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self::from_parts(
            addr,
            storage_partition,
            navigation_runtime_config,
        ))
    }

    fn from_parts(
        addr: SocketAddr,
        storage_partition: Arc<StoragePartitionState>,
        navigation_runtime_config: NavigationRuntimeConfig,
    ) -> Self {
        let cdp_agent_host_directory = SharedCdpAgentHostDirectory::default();
        let cdp_target_id_allocator = BrowserTargetIdAllocator::default();
        let cdp_owner_registry = SharedCdpOwnerRegistry::new(
            cdp_agent_host_directory.clone(),
            cdp_target_id_allocator,
            storage_partition.clone(),
            navigation_runtime_config.clone(),
        );
        Self {
            browser_ws_url: format!("ws://{addr}/devtools/browser/{DEFAULT_BROWSER_ID}"),
            page_ws_url: format!("ws://{addr}/devtools/page/{DEFAULT_TARGET_ID}"),
            bidi_ws_url: format!("ws://{addr}/session"),
            bidi_session_registry: SharedBidiSessionRegistry::default(),
            classic_session_registry: SharedClassicSessionRegistry::default(),
            cdp_agent_host_directory,
            cdp_owner_registry,
            devtools_frontend_url: format!(
                "/devtools/inspector.html?ws={addr}/devtools/page/{DEFAULT_TARGET_ID}"
            ),
            storage_partition,
            fetch_config: navigation_runtime_config.fetch_config().clone(),
            optional_resource_fetch_mask: navigation_runtime_config.optional_resource_fetch_mask(),
            subframe_loading_enabled: navigation_runtime_config.subframe_loading_enabled(),
            layout_policy: navigation_runtime_config.layout_policy(),
        }
    }

    fn initial_storage_partition(&self) -> CdpInitialStoragePartition {
        CdpInitialStoragePartition::from_storage_partition(self.storage_partition.as_ref())
    }
}

async fn flush_storage_partition_profile(
    storage_partition: Arc<StoragePartitionState>,
    surface: &'static str,
) {
    match tokio::task::spawn_blocking(move || storage_partition.flush()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(?error, surface, "failed to flush browser profile"),
        Err(error) => tracing::warn!(?error, surface, "browser profile flush task panicked"),
    }
}

#[cfg(test)]
mod tests;
