use super::cookie_manager_surface;
use super::state::TargetPageAbsenceReason;
use super::{
    BackgroundProtocolEvent, BrowserContext, BrowserContextCookieBackendConnectionState,
    BrowserContextCookieGetFreshnessStatus, BrowserContextCookieSetReadinessStatus,
    BrowserContextDefaultCookieWriteUrlSource, BrowserContextDocumentCookieCacheLookupResult,
    BrowserContextFirstCookieRequest, BrowserContextReservedSiteDataOwnerState,
    BrowserContextSiteDataManagerOwnerState, BrowserContextStructuredCookieCommandVerdict,
    BrowserContextStructuredCookieWriteBackendStatus,
    BrowserContextStructuredCookieWriteReadinessStatus, CdpConnection, CdpInitialStoragePartition,
    CdpSessionRoute, CommandDispatchContext, CommandResponseFlushContext, CompletedDownloadBody,
    CompletedDownloadBodyArtifact, InitialDocumentPageInstallResult, NavigationBackgroundEvent,
    NavigationDispatchState, NavigationResultProjection, ServiceWorkerTargetState,
    SharedWorkerTargetState, build_event,
};
use crate::devtools_runtime::{
    AutomationEvent, DevToolsFrameId, DevToolsLoaderId, DevToolsTargetFilterEntry,
    DevToolsTargetId, NavigationFrameEvent, NavigationFrameEventKind,
};
use crate::domains::network::{
    FailedNavigationDocumentPolicy, FailedNavigationResponseMode,
    MaterializedDownloadDocumentProgress, MaterializedFailedDocumentProgress,
    MaterializedNavigationLoadOutcome, empty_main_document_progress_gate_for_test,
};
use crate::domains::page::MaterializedNavigationCompletion;
use crate::testing::TestContext;
use moli_cookie_jar::{
    BrowserCookieFacadeContextOverrides, BrowserCookieFacadeOverrides, CookieSiteDataClearScope,
    CookieSiteDataOperation, CookieSiteDataOperationPreviewReport, CookieSiteDataScope,
    CookieSiteDataSummary, CookieStorageClearTarget, StoredCookie, StoredCookieSameSite,
    StoredCookieSetRejectionReason, StoredCookieSourceScheme,
};
use moli_core::{
    LayoutPolicy, OptionalResourceFetchMask,
    browser_host::{
        BrowserDownloadArtifactOutcome, BrowserDownloadBehavior, BrowserDownloadPolicyUpdate,
        BrowserFact, BrowserHostState, BrowserNavigationFailure, BrowserTargetTerminationKind,
    },
    page::RendererServiceWorkerVersionStatus,
    runtime::{NavigationEngine, NavigationRuntimeConfig},
};
use moli_fetch::{FetchCancelHandle, FetchConfig};
use moli_shared_worker::SharedWorkerInstanceId;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use url::Url;

mod cookie_surfaces;
mod message;
mod resource_runtime;
mod site_data;

fn active_layout_policy(conn: &CdpConnection) -> LayoutPolicy {
    conn.browser_host_state
        .navigation_owner()
        .clone_active_navigation_engine()
        .layout_policy()
}

#[test]
fn idle_navigation_engine_reset_preserves_mock_layout_policy() {
    let mut conn = CdpConnection::new_with_initial_storage_partition_and_runtime_config(
        CdpInitialStoragePartition::memory(),
        NavigationRuntimeConfig::new(
            FetchConfig::default(),
            OptionalResourceFetchMask::NONE,
            true,
            LayoutPolicy::Mock,
        ),
    );

    assert_eq!(active_layout_policy(&conn), LayoutPolicy::Mock);
    let reset = conn.release_idle_navigation_engine_memory_if_idle();

    assert!(reset.reset);
    assert_eq!(active_layout_policy(&conn), LayoutPolicy::Mock);
}

#[tokio::test]
async fn browser_context_install_and_removal_preserve_mock_layout_policy() {
    let mut conn = CdpConnection::new_with_initial_storage_partition_and_runtime_config(
        CdpInitialStoragePartition::memory(),
        NavigationRuntimeConfig::new(
            FetchConfig::default(),
            OptionalResourceFetchMask::NONE,
            true,
            LayoutPolicy::Mock,
        ),
    );
    conn.insert_browser_context(BrowserContext::new("CTX-layout".to_owned()));

    assert_eq!(active_layout_policy(&conn), LayoutPolicy::Mock);
    let removed = conn
        .remove_browser_context_by_id_restoring_active_async("CTX-layout", None)
        .await;

    assert!(removed.is_ok());
    assert_eq!(active_layout_policy(&conn), LayoutPolicy::Mock);
}

#[test]
fn browser_host_registry_outlives_protocol_adapter_state() {
    let fetch_config = FetchConfig::default();
    let browser_host_state = BrowserHostState::new(
        NavigationEngine::new_with_fetch_config_and_resource_loading(
            fetch_config.clone(),
            OptionalResourceFetchMask::NONE,
            true,
        ),
    );
    let mut conn = CdpConnection::new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state.clone(),
        CdpInitialStoragePartition::memory(),
    );
    conn.install_default_browser_target();
    let target_id = conn.default_target_id().to_owned();
    let browser_instance_id = browser_host_state.navigation_owner().browser_instance_id();
    let browser_context = conn
        .browser_context
        .as_ref()
        .expect("default physical BrowserContext projection");
    let browser_context_handle = browser_context.browser_context_handle().clone();
    let renderer_runtime_id = browser_context.renderer_runtime().id();
    assert!(
        !browser_context.carries_renderer_runtime_registration_owner_for_test(),
        "a committed frontend projection must not retain the unique runtime root"
    );
    assert_eq!(
        browser_host_state
            .renderer_runtime_owner_access(&browser_context_handle)
            .expect("Browser Host runtime root")
            .runtime()
            .id(),
        renderer_runtime_id
    );

    drop(conn);

    let browser_owner = browser_host_state.navigation_owner();
    assert_eq!(browser_owner.browser_instance_id(), browser_instance_id);
    assert_eq!(browser_owner.browser_context_count(), 1);
    assert_eq!(browser_owner.target_count(), 1);
    assert!(browser_owner.has_target(&target_id));
    drop(browser_owner);
    assert_eq!(
        browser_host_state
            .renderer_runtime_owner_access(&browser_context_handle)
            .expect("frontend teardown must leave the Browser Host runtime root live")
            .runtime()
            .id(),
        renderer_runtime_id
    );
}

#[tokio::test]
async fn browser_host_renderer_page_lifetime_outlives_protocol_adapter_state() {
    let browser_host_state = BrowserHostState::new(NavigationEngine::new());
    let mut conn = CdpConnection::new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state.clone(),
        CdpInitialStoragePartition::memory(),
    );
    conn.install_default_browser_target();
    let owner = conn
        .target_page_owner_key_for_session(None)
        .expect("default Target owner");
    let initial_document_owner = conn
        .initial_document_page_owner_for_session(None)
        .expect("default Target initial Document owner");
    let mut navigation = conn
        .load_navigation_via_runtime_async("data:text/html,<title>host-owned</title>")
        .await
        .expect("initial renderer navigation");
    if let Some(engine) = navigation.navigation_engine.take() {
        conn.adopt_navigation_engine_for_current_owner(engine)
            .expect("initial renderer engine should remain Browser-owned");
    }
    let page = navigation.page;
    let renderer_page_id = page.page_id();
    let page_creation_artifacts = navigation.page_creation_artifacts;

    assert_eq!(
        conn.install_initial_loaded_page_for_page_owner_async(
            &initial_document_owner,
            page,
            page_creation_artifacts,
        )
        .await
        .expect("initial Page installation should commit"),
        InitialDocumentPageInstallResult::Installed
    );
    assert_eq!(
        browser_host_state
            .navigation_owner()
            .renderer_page_id_for_owner(&owner),
        Some(renderer_page_id)
    );

    drop(conn);

    assert_eq!(
        browser_host_state
            .navigation_owner()
            .renderer_page_id_for_owner(&owner),
        Some(renderer_page_id),
        "dropping the Protocol adapter must not retire the Browser-owned renderer Page"
    );

    let request = browser_host_state
        .navigation_owner()
        .capture_target_termination(&owner, BrowserTargetTerminationKind::Close)
        .expect("Browser Host should still own the Target after frontend teardown");
    let permit = browser_host_state
        .navigation_owner()
        .prepare_target_termination(request)
        .expect("retained Browser Target should still prepare termination");
    let mut termination = browser_host_state
        .commit_target_termination(permit)
        .expect("retained Browser Target should still commit termination");
    let retired_owner = termination
        .take_retired_renderer_page_owner()
        .expect("Browser Host termination should return its renderer Page owner");
    assert_eq!(retired_owner.page_id(), renderer_page_id);
    retired_owner
        .close_async()
        .await
        .expect("retained renderer Page should close after frontend teardown");
}

#[test]
fn browser_host_policy_is_shared_across_frontend_adapters() {
    let mut fetch_config = FetchConfig::default();
    fetch_config.set_user_agent("BaseHostPolicy/1.0");
    let browser_host_state = BrowserHostState::new(
        NavigationEngine::new_with_fetch_config_and_resource_loading(
            fetch_config,
            OptionalResourceFetchMask::NONE,
            true,
        ),
    );
    let mut first = CdpConnection::new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state.clone(),
        CdpInitialStoragePartition::memory(),
    );
    let second = CdpConnection::new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state.clone(),
        CdpInitialStoragePartition::memory(),
    );

    first.set_global_browser_identity_override_from_user_agent(Some(
        "SharedHostPolicy/2.0".to_owned(),
    ));
    first.set_global_extra_headers(vec![("x-host-policy".to_owned(), "shared".to_owned())]);
    first.set_global_cache_disabled(true);
    first.set_global_network_conditions(Some(super::EmulatedNetworkConditions::offline()));
    first.set_global_geolocation_override(Some(super::EmulatedGeolocationOverrideState::Position(
        super::EmulatedGeolocationOverride {
            latitude: 1.0,
            longitude: 2.0,
            accuracy: 3.0,
            altitude: None,
            altitude_accuracy: None,
            heading: None,
            speed: None,
        },
    )));
    let mut bounds = first.browser_host_policy_snapshot().window_bounds().clone();
    bounds.width = Some(1440);
    first.apply_browser_host_policy_update(
        moli_core::browser_host::BrowserHostPolicyUpdate::ReplaceWindowBounds(bounds),
    );
    first.apply_browser_host_policy_update(
        moli_core::browser_host::BrowserHostPolicyUpdate::ReplacePermissionOverrides(vec![
            super::PermissionOverride {
                permission: json!({ "name": "geolocation" }),
                setting: "denied".to_owned(),
                origin: Some("https://example.test".to_owned()),
                embedded_origin: None,
                browser_context_id: None,
            },
        ]),
    );
    drop(first);

    let policy = second.browser_host_policy_snapshot();
    assert_eq!(second.user_agent(), "SharedHostPolicy/2.0");
    assert!(policy.global_cache_disabled());
    assert_eq!(
        policy.global_extra_headers(),
        &[("x-host-policy".to_owned(), "shared".to_owned())]
    );
    assert_eq!(policy.window_bounds().width, Some(1440));
    assert_eq!(policy.permission_overrides().len(), 1);

    let context = second.new_browser_context("BID-policy-reader".to_owned());
    assert!(context.network_policy.cache_disabled());
    assert_eq!(
        context.global_extra_headers,
        vec![("x-host-policy".to_owned(), "shared".to_owned())]
    );
    assert!(context.effective_active_network_offline());
    assert_eq!(
        context
            .effective_active_geolocation_override()
            .and_then(|state| state.position().cloned())
            .map(|position| (position.latitude, position.longitude)),
        Some((1.0, 2.0))
    );
    assert_eq!(
        second
            .effective_permission_overrides_for_browser_context_id("BID-policy-reader")
            .len(),
        1
    );
}

#[test]
fn browser_download_state_is_shared_and_frontend_subscriptions_are_not() {
    let browser_host_state = BrowserHostState::new(NavigationEngine::new());
    let mut first = CdpConnection::new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state.clone(),
        CdpInitialStoragePartition::memory(),
    );
    let second = CdpConnection::new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state,
        CdpInitialStoragePartition::memory(),
    );

    first.apply_browser_download_policy_update(BrowserDownloadPolicyUpdate::SetGlobal {
        behavior: BrowserDownloadBehavior::AllowAndName,
        download_path: Some("/tmp/shared-browser-downloads".to_owned()),
    });
    first.set_automation_download_events_enabled_for_browser_context(None, true);
    first.set_browser_download_events_enabled_for_session(None, true);
    let cancel_handle = FetchCancelHandle::new();
    let registry = first.browser_download_registry();
    registry.register_active("shared-active".to_owned(), cancel_handle.clone());
    let artifact_path = PathBuf::from("/tmp/shared-browser-download-artifact");
    registry.record_completed("shared-complete", artifact_path.clone());

    drop(first);

    let policy = second.effective_browser_download_policy(None);
    assert_eq!(policy.behavior(), BrowserDownloadBehavior::AllowAndName);
    assert_eq!(
        policy.download_path(),
        Some("/tmp/shared-browser-downloads")
    );
    assert!(
        !second.automation_download_events_enabled_for_browser_context(None),
        "automation event projection must remain local to its frontend"
    );
    assert!(
        second
            .browser_download_event_session_ids_for_test()
            .is_empty(),
        "wire subscription generations must remain local to each frontend"
    );
    assert_eq!(second.cancel_download("shared-active"), Ok(()));
    assert!(cancel_handle.is_cancelled());
    assert_eq!(
        second
            .browser_download_registry()
            .artifact("shared-complete"),
        BrowserDownloadArtifactOutcome::Ready(artifact_path)
    );
}

#[test]
fn browser_host_identity_namespace_is_shared_across_frontend_adapters() {
    let browser_host_state = BrowserHostState::new(NavigationEngine::new());
    let mut first = CdpConnection::new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state.clone(),
        CdpInitialStoragePartition::memory(),
    );
    let mut second = CdpConnection::new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state,
        CdpInitialStoragePartition::memory(),
    );

    assert_eq!(first.gen_bc_id(), "BID-1");
    assert_eq!(second.gen_user_browser_context_id(), "user-context-2");
    assert_eq!(first.gen_target_id(), "TID-1");
    assert_eq!(second.gen_target_id(), "TID-2");
    assert_eq!(
        first.browser_host_state.allocate_browser_command_id().get(),
        1
    );
    assert_eq!(
        second
            .browser_host_state
            .allocate_browser_command_id()
            .get(),
        2
    );
}

#[test]
fn browser_network_request_namespace_is_shared_across_frontend_adapters() {
    let browser_host_state = BrowserHostState::new(NavigationEngine::new());
    let first = CdpConnection::new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state.clone(),
        CdpInitialStoragePartition::memory(),
    );
    let second = CdpConnection::new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state,
        CdpInitialStoragePartition::memory(),
    );

    assert_eq!(
        first
            .browser_host_state
            .network_artifacts()
            .allocate_request_id(),
        "REQ-1"
    );
    assert_eq!(
        second
            .browser_host_state
            .network_artifacts()
            .allocate_request_id(),
        "REQ-2",
        "frontend construction must not create a private request-id namespace"
    );
}

#[test]
fn global_io_stream_ids_cross_u32_max_without_reuse() {
    let mut conn = CdpConnection::new();

    conn.next_global_io_stream_id = u32::MAX as u64;
    let handle = conn.open_global_io_stream(b"payload".to_vec());

    assert_eq!(handle, "BROWSER-STREAM-4294967296");
    assert!(conn.global_io_streams.contains_key(&handle));
}

#[test]
#[should_panic(expected = "global IO stream id space exhausted")]
fn global_io_stream_id_allocator_rejects_u64_exhaustion() {
    let mut conn = CdpConnection::new();
    conn.next_global_io_stream_id = u64::MAX;

    let _ = conn.open_global_io_stream(Vec::new());
}

#[test]
#[should_panic(expected = "internal Runtime command id space exhausted")]
fn internal_runtime_command_id_allocator_rejects_u64_exhaustion() {
    let mut conn = CdpConnection::new();
    conn.next_internal_runtime_command_id = u64::MAX;

    let _ = conn.next_internal_runtime_command_id();
}

#[test]
fn replace_root_target_discovery_is_noop_when_already_enabled() {
    let mut conn = CdpConnection::new();
    let filter = vec![DevToolsTargetFilterEntry {
        exclude: false,
        target_type: Some("service_worker".to_owned()),
    }];
    conn.set_target_discovery_for_owner_from_devtools_filter(None, Some(filter.clone()));

    let previous = conn.replace_root_target_discovery_enabled(true);

    assert!(previous);
    assert_eq!(conn.target_discovery_filter_for_owner(None), Some(filter));
}

#[test]
fn command_response_flush_permit_is_unique_and_scoped_to_its_context() {
    let mut conn = CdpConnection::new();

    let (first_permit, first_context) = conn.begin_command_response_flush_permit();
    let first_receiver = first_context
        .receiver()
        .expect("first command should install a response flush observer");

    let (second_permit, second_context) = conn.begin_command_response_flush_permit();
    let second_receiver = second_context
        .receiver()
        .expect("second command should install a response flush observer");

    second_permit.finish();

    assert!(
        !*first_receiver.borrow(),
        "finishing a later command permit must not release observers of an earlier command"
    );
    assert!(
        *second_receiver.borrow(),
        "finishing a command permit should release observers of that same command"
    );

    first_permit.finish();
    assert!(
        *first_receiver.borrow(),
        "the earlier command should remain releasable by its unique permit"
    );
}

#[test]
fn dropping_command_response_flush_permit_cancels_its_observers() {
    let mut conn = CdpConnection::new();
    let (permit, context) = conn.begin_command_response_flush_permit();
    let receiver = context
        .receiver()
        .expect("command should install a response flush observer");

    drop(permit);

    assert!(
        receiver.has_changed().is_err(),
        "dropping the unique permit must close the command-scoped observation"
    );
    assert!(
        !*receiver.borrow(),
        "canceling a command must not falsely publish that its response was flushed"
    );
}

#[test]
fn command_response_flush_permit_runs_deferred_release_exactly_once() {
    let mut conn = CdpConnection::new();
    let (permit, context) = conn.begin_command_response_flush_permit();
    let releases = Arc::new(AtomicUsize::new(0));
    let release_counter = releases.clone();
    context.defer_until_response_flush(move || {
        release_counter.fetch_add(1, Ordering::SeqCst);
    });

    assert_eq!(releases.load(Ordering::SeqCst), 0);
    permit.finish();
    assert_eq!(
        releases.load(Ordering::SeqCst),
        1,
        "the unique permit must release one continuation exactly once"
    );
}

#[test]
fn abandoned_command_response_flush_permit_releases_fail_open() {
    let mut conn = CdpConnection::new();
    let (permit, context) = conn.begin_command_response_flush_permit();
    let releases = Arc::new(AtomicUsize::new(0));
    let release_counter = releases.clone();
    context.defer_until_response_flush(move || {
        release_counter.fetch_add(1, Ordering::SeqCst);
    });

    drop(context);
    assert_eq!(releases.load(Ordering::SeqCst), 0);
    drop(permit);
    assert_eq!(
        releases.load(Ordering::SeqCst),
        1,
        "dropping the unique permit must not leave renderer work parked"
    );
}

#[test]
fn missing_command_response_flush_context_releases_immediately() {
    let context = CommandResponseFlushContext::default();
    let releases = Arc::new(AtomicUsize::new(0));
    let release_counter = releases.clone();
    context.defer_until_response_flush(move || {
        release_counter.fetch_add(1, Ordering::SeqCst);
    });

    assert_eq!(releases.load(Ordering::SeqCst), 1);
}

#[test]
fn background_navigation_completion_sender_routes_explicit_session_owners() {
    let mut conn = CdpConnection::new();
    let mut active = BrowserContext::new("BID-active".to_owned());
    active.set_active_target_id("TID-active");
    active.attach_active_session("SID-active");
    conn.browser_context = Some(active);

    let mut inactive = BrowserContext::new("BID-inactive".to_owned());
    inactive.set_active_target_id("TID-inactive");
    inactive.attach_active_session("SID-inactive");
    conn.inactive_browser_contexts.push(inactive);

    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    conn.set_background_navigation_completion_sender(sender);

    assert!(
        conn.background_navigation_completion_sender_for_session_owner(Some("SID-active"))
            .is_some(),
        "a command scoped to a concrete target owner can continue navigation work in the background"
    );
    assert!(
        conn.background_navigation_completion_sender_for_session_owner(Some("SID-inactive"))
            .is_some(),
        "inactive-context target owners should also be routable by explicit session id"
    );
}

#[test]
fn none_session_owner_route_override_scope_restores_previous_route_on_drop() {
    let mut conn = CdpConnection::new();
    let previous_route = CdpSessionRoute::ActiveTarget {
        browser_context_id: "BID-active".to_owned(),
        target_id: Some("TID-active".to_owned()),
    };
    let scoped_route = CdpSessionRoute::BackgroundTarget {
        browser_context_id: "BID-background".to_owned(),
        target_id: "TID-background".to_owned(),
    };

    conn.replace_none_session_owner_route_override(Some(previous_route.clone()));
    {
        let mut scope = conn.scoped_none_session_owner_route_override(scoped_route.clone());
        assert_eq!(
            scope.conn_mut().none_session_owner_route_override(),
            Some(scoped_route)
        );
    }

    assert_eq!(
        conn.none_session_owner_route_override(),
        Some(previous_route)
    );
}

#[test]
fn navigation_background_event_queue_drains_current_token() {
    let mut conn = CdpConnection::new();
    let mut browser_context = BrowserContext::new("CTX-nav".to_owned());
    browser_context.set_active_target_id("TID-nav");
    conn.browser_context = Some(browser_context);
    let token = conn
        .start_document_navigation_for_session_owner(None, "LOADER-1".to_owned())
        .expect("active target should produce navigation token");
    let message = build_event(
        "Page.frameStartedLoading",
        json!({ "frameId": "TID-nav" }),
        None,
    );

    conn.enqueue_navigation_background_event(NavigationBackgroundEvent::protocol_message(
        token,
        message.clone(),
    ));

    assert_eq!(conn.drain_navigation_background_events(), vec![message]);
    assert!(conn.drain_navigation_background_events().is_empty());
}

#[test]
fn active_browser_context_installs_its_renderer_runtime_on_engine() {
    let mut conn = CdpConnection::new();
    let browser_context = conn.new_browser_context("CTX-runtime".to_owned());
    let renderer_runtime = browser_context.renderer_runtime();

    conn.insert_browser_context(browser_context);

    assert!(
        conn.browser_host_state
            .navigation_owner()
            .active_browser_context_runtime()
            .shares_state_with(&renderer_runtime)
    );
}

#[test]
fn activating_inactive_browser_context_switches_engine_renderer_runtime() {
    let mut conn = CdpConnection::new();
    let first = conn.new_browser_context("CTX-first".to_owned());
    conn.insert_browser_context(first);
    let second = conn.new_browser_context("CTX-second".to_owned());
    let second_renderer_runtime = second.renderer_runtime();
    conn.insert_browser_context(second);

    assert!(conn.activate_browser_context_by_id("CTX-second"));

    assert!(
        conn.browser_host_state
            .navigation_owner()
            .active_browser_context_runtime()
            .shares_state_with(&second_renderer_runtime)
    );
}

#[tokio::test]
async fn removing_active_browser_context_switches_engine_to_promoted_context() {
    let mut conn = CdpConnection::new();
    let first = conn.new_browser_context("CTX-first".to_owned());
    conn.insert_browser_context(first);
    let second = conn.new_browser_context("CTX-second".to_owned());
    let second_renderer_runtime = second.renderer_runtime();
    conn.insert_browser_context(second);

    let removed = conn
        .remove_browser_context_by_id_restoring_active_async("CTX-first", Some("CTX-first"))
        .await
        .expect("active context should be removable");

    assert_eq!(removed, "CTX-first");
    assert_eq!(
        conn.browser_context.as_ref().map(|bc| bc.id.as_str()),
        Some("CTX-second")
    );
    assert!(
        conn.browser_host_state
            .navigation_owner()
            .active_browser_context_runtime()
            .shares_state_with(&second_renderer_runtime)
    );
    assert_eq!(
        conn.browser_host_state
            .navigation_owner()
            .selected_browser_context_id(),
        Some("CTX-second")
    );
    assert_eq!(
        conn.browser_host_state
            .navigation_owner()
            .browser_context_count(),
        1
    );
}

#[test]
fn unknown_browser_context_cannot_change_core_or_physical_selection() {
    let mut conn = CdpConnection::new();
    let mut first = conn.new_browser_context("CTX-first".to_owned());
    first.set_active_target_id("TID-first");
    conn.insert_browser_context(first);
    let mut second = conn.new_browser_context("CTX-second".to_owned());
    second.set_active_target_id("TID-second");
    conn.insert_browser_context(second);

    assert!(!conn.activate_browser_context_by_id("CTX-missing"));

    assert_eq!(
        conn.browser_context
            .as_ref()
            .map(|context| context.id.as_str()),
        Some("CTX-first")
    );
    assert_eq!(
        conn.inactive_browser_contexts
            .iter()
            .map(|context| context.id.as_str())
            .collect::<Vec<_>>(),
        vec!["CTX-second"]
    );
    assert_eq!(
        conn.browser_host_state
            .navigation_owner()
            .selected_browser_context_id(),
        Some("CTX-first")
    );
    assert_eq!(
        conn.browser_host_state
            .navigation_owner()
            .browser_context_count(),
        2
    );
}

#[test]
fn late_browser_context_removal_permit_cannot_change_physical_selection() {
    use moli_core::browser_host::{
        BrowserContextRegistryError, BrowserContextRuntimeRegistryError,
        BrowserContextSelectionProjection,
    };

    let mut conn = CdpConnection::new();
    let mut first = conn.new_browser_context("CTX-first".to_owned());
    first.set_active_target_id("TID-first");
    conn.insert_browser_context(first);
    let mut second = conn.new_browser_context("CTX-second".to_owned());
    second.set_active_target_id("TID-second");
    conn.insert_browser_context(second);
    let permit = conn
        .browser_host_state
        .navigation_owner()
        .prepare_browser_context_removal("CTX-first")
        .expect("selected context removal should prepare");

    assert!(conn.activate_browser_context_by_id("CTX-second"));
    let projection = BrowserContextSelectionProjection::new(
        Some("CTX-second".to_owned()),
        conn.selected_target_engine_disposition(),
    );
    let error = conn
        .browser_host_state
        .commit_browser_context_removal_with_successor_runtime(
            permit,
            projection,
            NavigationEngine::new,
        )
        .expect_err("late removal permit must be rejected");

    assert!(matches!(
        error,
        BrowserContextRuntimeRegistryError::Context(
            BrowserContextRegistryError::StaleRemovalPermit { .. }
        )
    ));
    assert_eq!(
        conn.browser_context
            .as_ref()
            .map(|context| context.id.as_str()),
        Some("CTX-second")
    );
    assert!(
        conn.inactive_browser_contexts
            .iter()
            .any(|context| context.id == "CTX-first")
    );
    assert_eq!(
        conn.browser_host_state
            .navigation_owner()
            .selected_browser_context_id(),
        Some("CTX-second")
    );
    assert_eq!(
        conn.browser_host_state
            .navigation_owner()
            .browser_context_count(),
        2
    );
}

#[tokio::test]
async fn memory_diagnostics_reports_page_vm_document_isolate_model() {
    let mut conn = CdpConnection::new();
    conn.adopt_navigation_engine_for_current_owner(
        NavigationEngine::new_with_page_vm_document_isolate_for_diagnostics(),
    )
    .expect("diagnostics engine should adopt into the current owner");

    conn.browser_context = Some(BrowserContext::new("BID-shared-diagnostics".to_owned()));
    let first_page = conn
        .load_page_via_runtime_async("data:text/html,<!doctype html><body>first</body>")
        .await
        .expect("first shared diagnostics page should load");
    let second_page = conn
        .load_page_via_runtime_async("data:text/html,<!doctype html><body>second</body>")
        .await
        .expect("second shared diagnostics page should load");
    let browser_context = conn.browser_context.as_mut().expect("browser context");
    browser_context.replace_loaded_page(Some(first_page));
    let mut background = super::BackgroundTarget::with_url(
        "TID-shared-diagnostics-bg".to_owned(),
        Some("SID-shared-diagnostics-bg".to_owned()),
        "data:text/html,<!doctype html><body>second</body>".to_owned(),
    );
    background.replace_loaded_page(Some(second_page));
    browser_context.background_targets.push(background);

    let pending_diagnostics = conn
        .start_moli_diagnostics()
        .expect("moli diagnostics dispatch should start");
    let diagnostics = conn.complete_moli_diagnostics(
        pending_diagnostics
            .wait()
            .await
            .expect("moli diagnostics dispatch should finish"),
    );

    let memory_cache = &diagnostics["connection"]["activeNavigationEngine"]["networkMemoryCache"];
    assert!(
        diagnostics["connection"]["activeNavigationEngine"]["resourceRuntimeId"]
            .as_u64()
            .is_some_and(|runtime_id| runtime_id > 0),
        "a materialized ResourceRequestClient should expose its shared browser resource runtime identity"
    );
    assert_eq!(memory_cache["retainedBytes"], json!(0));
    assert_eq!(memory_cache["retainedBytesLimit"], json!(15 * 1024 * 1024));
    assert_eq!(
        memory_cache["resourceBodyBytesLimit"],
        json!(3 * 1024 * 1024)
    );

    assert_eq!(
        diagnostics["isolateScope"]["documentIsolateModel"],
        json!("page-vm")
    );
    assert_eq!(
        diagnostics["isolateScope"]["loadedDocumentPageCount"],
        json!(2)
    );
    assert_eq!(
        diagnostics["isolateScope"]["estimatedRendererOwnerCount"],
        json!(1)
    );
    assert_eq!(
        diagnostics["isolateScope"]["loadedDocumentRendererOwnerCount"],
        json!(1)
    );
    assert_eq!(
        diagnostics["isolateScope"]["estimatedDocumentIsolateCount"],
        json!(2),
        "page-vm diagnostics should count one document isolate per loaded page"
    );
    assert_eq!(
        diagnostics["isolateScope"]["estimatedWorkerIsolateCount"],
        json!(0)
    );
    assert_eq!(
        diagnostics["isolateScope"]["estimatedLiveV8IsolateCount"],
        json!(2),
        "two loaded PageVMs should report two live document isolates"
    );
    let isolate_accounting = &diagnostics["isolateScope"]["documentIsolateAccounting"];
    assert_eq!(isolate_accounting["scope"], json!("renderer-process"));
    for counter in ["created", "destroyed", "live", "reserved"] {
        assert!(
            isolate_accounting[counter].is_u64(),
            "document isolate accounting should expose numeric {counter}: {diagnostics:?}"
        );
    }
    assert_eq!(
        diagnostics["isolateScope"]["documentContextCount"],
        json!(2),
        "HeapProfiler.moliDiagnostics should aggregate loaded page document contexts"
    );
    assert_eq!(
        diagnostics["isolateScope"]["isolatedWorldContextCount"],
        json!(0)
    );
    assert_eq!(
        diagnostics["isolateScope"]["childDefaultContextCount"],
        json!(0)
    );
}

#[tokio::test]
async fn replacing_or_retiring_a_loaded_page_advances_its_residence_generation() {
    let mut conn = CdpConnection::new();
    conn.browser_context = Some(BrowserContext::new("BID-page-generation".to_owned()));
    let first_page = conn
        .load_page_via_runtime_async("data:text/html,<body>first</body>")
        .await
        .expect("first Page");
    let second_page = conn
        .load_page_via_runtime_async("data:text/html,<body>second</body>")
        .await
        .expect("second Page");
    let context = conn.browser_context.as_mut().unwrap();
    let initial_generation = context.active_target.runtime_slot.loaded_page_generation();

    assert!(context.replace_loaded_page(Some(first_page)).is_none());
    let first_generation = context.active_target.runtime_slot.loaded_page_generation();
    assert_eq!(first_generation, initial_generation.wrapping_add(1));

    let first = context
        .replace_loaded_page(Some(second_page))
        .expect("first Page should be replaced");
    let second_generation = context.active_target.runtime_slot.loaded_page_generation();
    assert_eq!(second_generation, first_generation.wrapping_add(1));
    let _ = first.close_async().await;

    let second = context
        .clear_loaded_page_with_reason(TargetPageAbsenceReason::TargetClosed)
        .expect("second Page should be retired");
    assert_eq!(
        context.active_target.runtime_slot.loaded_page_generation(),
        second_generation.wrapping_add(1)
    );
    let _ = second.close_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn moli_diagnostics_preserves_runtime_observable_diagnostics() {
    let mut ctx = TestContext::new();
    let page_url = "data:text/html,<body>diagnostics capture</body>";
    let mut browser_context = BrowserContext::new("BID-diagnostics-capture".to_owned());
    browser_context.set_active_target_id("TID-diagnostics-capture");
    browser_context.attach_active_session("SID-diagnostics-capture");
    browser_context.set_target_url("about:blank".to_owned());
    ctx.conn.insert_browser_context(browser_context);
    ctx.install_navigation_fixture_for_session_owner(page_url, Some("SID-diagnostics-capture"))
        .await;

    ctx.process_async(json!({
        "id": 44_100,
        "method": "Runtime.enable",
        "sessionId": "SID-diagnostics-capture",
    }))
    .await;
    let enable_response = ctx.take_response_by_id(44_100);
    assert_eq!(enable_response["result"], json!({}));
    ctx.sent.clear();

    ctx.conn
        .evaluate_runtime_expression_for_session_owner_async(
            Some("SID-diagnostics-capture"),
            "console.log('survives diagnostics')",
        )
        .await
        .expect("console expression should evaluate");

    let pending_diagnostics = ctx
        .conn
        .start_moli_diagnostics()
        .expect("moli diagnostics dispatch should start");
    let _ = ctx.conn.complete_moli_diagnostics(
        pending_diagnostics
            .wait()
            .await
            .expect("moli diagnostics dispatch should finish"),
    );

    let first_snapshot = ctx
        .conn
        .page_diagnostics_snapshot_for_session_owner_async(Some("SID-diagnostics-capture"))
        .await
        .expect("runtime observable diagnostics should remain readable after diagnostics");
    assert_eq!(first_snapshot.diagnostics.pending_inspector_messages, 0);
    let first_source = first_snapshot
        .runtime_observable_source()
        .expect("console evaluation should update read-only observable diagnostics")
        .clone();

    let second_snapshot = ctx
        .conn
        .page_diagnostics_snapshot_for_session_owner_async(Some("SID-diagnostics-capture"))
        .await
        .expect("a second read-only diagnostics snapshot should complete");
    assert_eq!(
        second_snapshot.runtime_observable_source(),
        Some(&first_source),
        "Moli diagnostics must not mutate the renderer's read-only observable summary"
    );
}

#[tokio::test]
async fn memory_diagnostics_ignores_empty_retained_renderer_owner_for_document_isolates() {
    let mut conn = CdpConnection::new();
    conn.adopt_navigation_engine_for_current_owner(
        NavigationEngine::new_with_page_vm_document_isolate_for_diagnostics(),
    )
    .expect("diagnostics engine should adopt into the current owner");

    conn.browser_context = Some(BrowserContext::new("BID-doc-owner-diagnostics".to_owned()));
    let first_page = conn
        .load_page_via_runtime_async("data:text/html,<!doctype html><body>first</body>")
        .await
        .expect("first shared diagnostics page should load");
    let second_page = conn
        .load_page_via_runtime_async("data:text/html,<!doctype html><body>second</body>")
        .await
        .expect("second shared diagnostics page should load");
    let browser_context = conn.browser_context.as_mut().expect("browser context");
    browser_context.replace_loaded_page(Some(first_page));
    let mut background = super::BackgroundTarget::with_url(
        "TID-doc-owner-diagnostics-bg".to_owned(),
        Some("SID-doc-owner-diagnostics-bg".to_owned()),
        "data:text/html,<!doctype html><body>second</body>".to_owned(),
    );
    background.replace_loaded_page(Some(second_page));
    browser_context.background_targets.push(background);

    let retained_context = BrowserContext::new("BID-empty-retained".to_owned());
    let retained = NavigationEngine::new_with_fetch_config_and_browser_context_access(
        FetchConfig::default(),
        retained_context.renderer_runtime_owner_access(),
        conn.browser_host_state
            .navigation_owner()
            .active_optional_resource_fetch_mask(),
        conn.browser_host_state
            .navigation_owner()
            .active_subframe_loading_enabled(),
    )
    .expect("retained diagnostics context owner should be live");
    assert!(
        !conn
            .browser_host_state
            .navigation_owner()
            .detached_engine_shares_active_renderer_owner(&retained),
        "test setup must retain a distinct renderer owner without a loaded document"
    );
    conn.inactive_browser_contexts.push(retained_context);
    conn.retain_background_navigation_engine(
        "BID-empty-retained".to_owned(),
        "TID-empty-retained".to_owned(),
        retained,
    )
    .expect("diagnostics engine must match its retained BrowserContext");

    let diagnostics = conn.moli_memory_diagnostics();

    assert_eq!(
        diagnostics["isolateScope"]["loadedDocumentPageCount"],
        json!(2)
    );
    assert_eq!(
        diagnostics["isolateScope"]["retainedBackgroundNavigationEngineRendererOwnerCount"],
        json!(1)
    );
    assert_eq!(
        diagnostics["isolateScope"]["estimatedRendererOwnerCount"],
        json!(2),
        "the empty retained engine still contributes renderer owner fixed cost"
    );
    assert_eq!(
        diagnostics["isolateScope"]["loadedDocumentRendererOwnerCount"],
        json!(1),
        "the two loaded pages still share one renderer owner"
    );
    assert_eq!(
        diagnostics["isolateScope"]["estimatedDocumentIsolateCount"],
        json!(2),
        "each loaded PageVM contributes one document isolate"
    );
    assert_eq!(
        diagnostics["isolateScope"]["estimatedLiveV8IsolateCount"],
        json!(2),
        "empty retained renderer owners contribute fixed owner cost but no extra live V8 isolate"
    );
}

#[tokio::test]
async fn memory_diagnostics_sync_counts_dedicated_worker_from_cached_page_snapshot() {
    let mut conn = CdpConnection::new();
    conn.browser_context = Some(BrowserContext::new("BID-sync-dedicated-worker".to_owned()));
    let mut page = conn
        .load_page_via_runtime_async("data:text/html,<!doctype html><body>worker</body>")
        .await
        .expect("sync diagnostics dedicated-worker page should load");
    let start_worker_response = page
        .evaluate_runtime_expression_async(
            r#"
(() => {
  globalThis.__lmSyncDiagnosticsWorkerReady = false;
  const source = "postMessage('ready'); setInterval(() => {}, 1000);";
  const worker = new Worker("data:text/javascript," + encodeURIComponent(source));
  worker.onmessage = () => { globalThis.__lmSyncDiagnosticsWorkerReady = true; };
  globalThis.__lmSyncDiagnosticsWorker = worker;
  return "started";
})()
"#,
        )
        .await
        .expect("dedicated worker should start");
    assert_eq!(
        start_worker_response["value"],
        json!("started"),
        "dedicated worker should be scheduled before sync diagnostics: {start_worker_response:?}"
    );

    let browser_context = conn.browser_context.as_mut().expect("browser context");
    browser_context.set_active_target_id("TID-sync-dedicated-worker");
    browser_context.replace_loaded_page(Some(page));

    for _ in 0..64 {
        let ready_response = conn
            .browser_context
            .as_mut()
            .and_then(|context| context.active_target.runtime_slot.loaded_page_mut())
            .expect("loaded sync diagnostics page")
            .evaluate_runtime_expression_async("globalThis.__lmSyncDiagnosticsWorkerReady === true")
            .await
            .expect("worker ready probe should evaluate");
        if ready_response["value"] != json!(true) {
            continue;
        }

        let diagnostics = conn.moli_memory_diagnostics();
        assert_eq!(
            diagnostics["isolateScope"]["estimatedDocumentIsolateCount"],
            json!(1)
        );
        assert_eq!(
            diagnostics["isolateScope"]["estimatedWorkerIsolateCount"],
            json!(1),
            "sync diagnostics must include page-owned dedicated worker isolates: {diagnostics:?}"
        );
        assert_eq!(
            diagnostics["isolateScope"]["estimatedLiveV8IsolateCount"],
            json!(2),
            "sync diagnostics live V8 total should be one document isolate plus one dedicated worker isolate: {diagnostics:?}"
        );
        return;
    }

    panic!("dedicated worker did not report ready before sync diagnostics assertion");
}

#[tokio::test]
async fn memory_diagnostics_counts_different_browser_context_document_isolates_separately() {
    let mut conn = CdpConnection::new();

    let mut first_context = conn.new_browser_context("BID-doc-owner-first".to_owned());
    first_context.set_active_target_id("TID-doc-owner-first");
    conn.insert_browser_context(first_context);

    let first_page = conn
        .load_page_via_runtime_async("data:text/html,<!doctype html><body>first-context</body>")
        .await
        .expect("first browser-context diagnostics page should load");
    conn.browser_context
        .as_mut()
        .expect("first browser context should be active")
        .replace_loaded_page(Some(first_page));

    let mut second_context = conn.new_browser_context("BID-doc-owner-second".to_owned());
    second_context.set_active_target_id("TID-doc-owner-second");
    conn.insert_browser_context(second_context);
    assert!(conn.activate_browser_context_by_id("BID-doc-owner-second"));

    let second_page = conn
        .load_page_via_runtime_async("data:text/html,<!doctype html><body>second-context</body>")
        .await
        .expect("second browser-context diagnostics page should load");
    conn.browser_context
        .as_mut()
        .expect("second browser context should be active")
        .replace_loaded_page(Some(second_page));

    let pending_diagnostics = conn
        .start_moli_diagnostics()
        .expect("moli diagnostics dispatch should start");
    let diagnostics = conn.complete_moli_diagnostics(
        pending_diagnostics
            .wait()
            .await
            .expect("moli diagnostics dispatch should finish"),
    );

    assert_eq!(
        diagnostics["isolateScope"]["loadedDocumentPageCount"],
        json!(2)
    );
    assert_eq!(
        diagnostics["isolateScope"]["retainedBackgroundNavigationEngineRendererOwnerCount"],
        json!(1),
        "switching browser contexts should retain the first loaded target's renderer owner fixed cost"
    );
    assert_eq!(
        diagnostics["isolateScope"]["loadedDocumentRendererOwnerCount"],
        json!(2),
        "different browser contexts must not collapse their document pages onto one renderer owner"
    );
    assert_eq!(
        diagnostics["isolateScope"]["estimatedDocumentIsolateCount"],
        json!(2),
        "different loaded PageVMs should report separate document isolates"
    );
    assert_eq!(
        diagnostics["isolateScope"]["estimatedLiveV8IsolateCount"],
        json!(2),
        "two live document isolates without workers should report two live V8 isolates"
    );
    assert_eq!(
        diagnostics["isolateScope"]["documentContextCount"],
        json!(2),
        "diagnostics should snapshot both browser-context document pages"
    );
}

#[test]
fn memory_diagnostics_counts_shared_retained_background_engine_by_renderer_owner() {
    let mut conn = CdpConnection::new();
    let browser_context = BrowserContext::new("BID-shared-diagnostics".to_owned());
    conn.insert_browser_context(browser_context);

    let retained = conn
        .browser_host_state
        .navigation_owner()
        .new_engine_sharing_active_renderer_owner(FetchConfig::default())
        .expect("retained diagnostics wrapper should share a live context owner");
    assert!(
        conn.browser_host_state
            .navigation_owner()
            .detached_engine_shares_active_renderer_owner(&retained),
        "test setup must retain a NavigationEngine wrapper that shares the active renderer owner"
    );
    conn.retain_background_navigation_engine(
        "BID-shared-diagnostics".to_owned(),
        "TID-shared-diagnostics-bg".to_owned(),
        retained,
    )
    .expect("diagnostics wrapper must match its retained BrowserContext");

    let diagnostics = conn.moli_memory_diagnostics();

    assert_eq!(
        diagnostics["connection"]["retainedBackgroundNavigationEngineCount"],
        json!(1),
        "the CDP connection still retains a background NavigationEngine wrapper"
    );
    assert_eq!(
        diagnostics["isolateScope"]["retainedBackgroundNavigationEngineRendererOwnerCount"],
        json!(0),
        "a retained background NavigationEngine that shares the active renderer owner must not count as another renderer owner"
    );
    assert_eq!(
        diagnostics["isolateScope"]["estimatedRendererOwnerCount"],
        json!(1)
    );
}

#[test]
fn retained_background_engine_rejects_a_foreign_browser_context_route() {
    let mut conn = CdpConnection::new();
    conn.browser_context = Some(BrowserContext::new("BID-retain-route".to_owned()));
    let foreign_context = BrowserContext::new("BID-retain-route-foreign".to_owned());
    let foreign_engine = NavigationEngine::new_with_fetch_config_and_browser_context_access(
        FetchConfig::default(),
        foreign_context.renderer_runtime_owner_access(),
        conn.browser_host_state
            .navigation_owner()
            .active_optional_resource_fetch_mask(),
        conn.browser_host_state
            .navigation_owner()
            .active_subframe_loading_enabled(),
    )
    .expect("foreign context owner should be live during the regression");

    let error = conn
        .retain_background_navigation_engine(
            "BID-retain-route".to_owned(),
            "TID-retain-route".to_owned(),
            foreign_engine,
        )
        .expect_err("a retained route must reject an engine from another BrowserContext");

    assert!(error.contains("does not match BrowserContext `BID-retain-route`"));
    assert_eq!(
        conn.browser_host_state
            .navigation_owner()
            .retained_background_engine_count(),
        0
    );
}

#[tokio::test]
async fn memory_diagnostics_splits_pending_inspector_await_counts_by_target_owner() {
    let mut conn = CdpConnection::new();
    conn.browser_context = Some(BrowserContext::new(
        "BID-pending-await-diagnostics".to_owned(),
    ));

    let active_page = conn
        .load_page_via_runtime_async("data:text/html,<!doctype html><body>active</body>")
        .await
        .expect("active diagnostics page should load");
    let background_page = conn
        .load_page_via_runtime_async("data:text/html,<!doctype html><body>background</body>")
        .await
        .expect("background diagnostics page should load");

    let browser_context = conn.browser_context.as_mut().expect("browser context");
    browser_context.set_active_target_id("TID-pending-await-active");
    browser_context.attach_active_session("SID-pending-await-active");
    browser_context.replace_loaded_page(Some(active_page));
    browser_context
        .devtools_session_state_mut()
        .register_pending_inspector_await(10_001, Some("SID-pending-await-active"), None);
    browser_context
        .devtools_session_state_mut()
        .register_pending_inspector_await(
            10_002,
            Some("SID-pending-await-active"),
            Some("active-group"),
        );

    let mut background = super::BackgroundTarget::with_url(
        "TID-pending-await-bg".to_owned(),
        Some("SID-pending-await-bg".to_owned()),
        "data:text/html,<!doctype html><body>background</body>".to_owned(),
    );
    background.replace_loaded_page(Some(background_page));
    browser_context.background_targets.push(background);
    browser_context.adopt_background_target_fixture_attachments();
    browser_context
        .primary_devtools_session_state_for_target_mut("TID-pending-await-bg")
        .expect("background Target frontend state")
        .register_pending_inspector_await(20_001, Some("SID-pending-await-bg"), None);

    let shared_worker_instance_id = SharedWorkerInstanceId::from_u64(30_001);
    let mut shared_worker_target = SharedWorkerTargetState::new(
        moli_core::RendererOwnerLocalHostId::new_for_testing(1),
        shared_worker_instance_id,
        "TID-pending-await-sw".to_owned(),
        Some("TID-pending-await-active".to_owned()),
        "https://example.test/sw.js".to_owned(),
        "diagnostics-sw".to_owned(),
    );
    shared_worker_target.attach_session("SID-pending-await-sw".to_owned());
    shared_worker_target.register_pending_inspector_await(
        "SID-pending-await-sw",
        30_001,
        Some("SID-pending-await-sw"),
        None,
    );
    browser_context
        .shared_worker_targets
        .insert(shared_worker_instance_id, shared_worker_target);

    let service_worker_version_id = 40_001;
    let mut service_worker_target = ServiceWorkerTargetState::new(
        40_000,
        service_worker_version_id,
        "TID-pending-await-service-worker".to_owned(),
        "https://example.test/service-worker.js".to_owned(),
        "https://example.test/".to_owned(),
        RendererServiceWorkerVersionStatus::Activated,
        None,
    );
    service_worker_target.attach_session("SID-pending-await-service-worker".to_owned());
    service_worker_target.register_pending_inspector_await(
        "SID-pending-await-service-worker",
        40_001,
        Some("SID-pending-await-service-worker"),
        None,
    );
    browser_context
        .service_worker_targets
        .insert(service_worker_version_id, service_worker_target);

    let diagnostics = conn.moli_memory_diagnostics();

    assert_eq!(
        diagnostics["isolateScope"]["pendingInspectorAwaitCount"],
        json!(5)
    );
    assert_eq!(
        diagnostics["isolateScope"]["pageTargetPendingInspectorAwaitCount"],
        json!(3)
    );
    assert_eq!(
        diagnostics["isolateScope"]["pageTargetWithPendingInspectorAwaitCount"],
        json!(2)
    );
    assert_eq!(
        diagnostics["isolateScope"]["sharedWorkerTargetPendingInspectorAwaitCount"],
        json!(1)
    );
    assert_eq!(
        diagnostics["isolateScope"]["sharedWorkerTargetWithPendingInspectorAwaitCount"],
        json!(1)
    );
    assert_eq!(
        diagnostics["isolateScope"]["serviceWorkerTargetPendingInspectorAwaitCount"],
        json!(1)
    );
    assert_eq!(
        diagnostics["isolateScope"]["serviceWorkerTargetWithPendingInspectorAwaitCount"],
        json!(1)
    );
    assert_eq!(
        diagnostics["activeBrowserContext"]["pendingInspectorAwaitCount"],
        json!(5)
    );
    assert_eq!(
        diagnostics["activeBrowserContext"]["pageTargetPendingInspectorAwaitCount"],
        json!(3)
    );
    assert_eq!(
        diagnostics["activeBrowserContext"]["sharedWorkerTargetPendingInspectorAwaitCount"],
        json!(1)
    );
    assert_eq!(
        diagnostics["activeBrowserContext"]["serviceWorkerTargetPendingInspectorAwaitCount"],
        json!(1)
    );
    assert_eq!(
        diagnostics["activeBrowserContext"]["serviceWorkerTargetWithPendingInspectorAwaitCount"],
        json!(1)
    );
    assert_eq!(
        diagnostics["activeBrowserContext"]["runtimeSession"]["pendingInspectorAwaitCount"],
        json!(2)
    );
    assert_eq!(
        diagnostics["activeBrowserContext"]["runtimeSession"]["primaryPendingInspectorAwaitCount"],
        json!(2)
    );
    assert!(
        diagnostics["activeBrowserContext"]["targetParking"]
            .get("pendingInspectorAwaitCount")
            .is_none(),
        "frontend await diagnostics must no longer be attributed to physical parking storage"
    );
}

#[test]
fn navigation_background_event_queue_drops_stale_token() {
    let mut conn = CdpConnection::new();
    let mut browser_context = BrowserContext::new("CTX-nav".to_owned());
    browser_context.set_active_target_id("TID-nav");
    conn.browser_context = Some(browser_context);
    let stale = conn
        .start_document_navigation_for_session_owner(None, "LOADER-1".to_owned())
        .expect("active target should produce stale token");
    let current = conn
        .start_document_navigation_for_session_owner(None, "LOADER-2".to_owned())
        .expect("active target should produce current token");
    let stale_message = build_event(
        "Page.frameStartedLoading",
        json!({ "frameId": "TID-nav", "loaderId": "LOADER-1" }),
        None,
    );
    let current_message = build_event(
        "Page.frameStartedLoading",
        json!({ "frameId": "TID-nav", "loaderId": "LOADER-2" }),
        None,
    );

    conn.enqueue_navigation_background_event(NavigationBackgroundEvent::protocol_message(
        stale,
        stale_message,
    ));
    conn.enqueue_navigation_background_event(NavigationBackgroundEvent::protocol_message(
        current,
        current_message.clone(),
    ));

    assert_eq!(
        conn.drain_navigation_background_events(),
        vec![current_message]
    );
}

#[test]
fn navigation_background_event_queue_preserves_order_for_current_token() {
    let mut conn = CdpConnection::new();
    let mut browser_context = BrowserContext::new("CTX-nav-order".to_owned());
    browser_context.set_active_target_id("TID-nav-order");
    conn.browser_context = Some(browser_context);
    let stale = conn
        .start_document_navigation_for_session_owner(None, "LOADER-1".to_owned())
        .expect("active target should produce stale navigation token");
    let current = conn
        .start_document_navigation_for_session_owner(None, "LOADER-2".to_owned())
        .expect("active target should produce current navigation token");

    let stale_message = build_event(
        "Page.frameStartedLoading",
        json!({ "frameId": "TID-nav-order", "loaderId": "LOADER-1" }),
        None,
    );
    let current_first_message = build_event(
        "Page.frameStartedLoading",
        json!({ "frameId": "TID-nav-order", "loaderId": "LOADER-2", "step": 1 }),
        None,
    );
    let current_second_message = build_event(
        "Page.frameStoppedLoading",
        json!({ "frameId": "TID-nav-order", "loaderId": "LOADER-2", "step": 2 }),
        None,
    );

    conn.enqueue_navigation_background_event(NavigationBackgroundEvent::protocol_message(
        stale,
        stale_message,
    ));
    conn.enqueue_navigation_background_event(NavigationBackgroundEvent::protocol_message(
        current.clone(),
        current_first_message.clone(),
    ));
    conn.enqueue_navigation_background_event(NavigationBackgroundEvent::protocol_message(
        current,
        current_second_message.clone(),
    ));

    assert_eq!(
        conn.drain_navigation_background_events(),
        vec![current_first_message, current_second_message]
    );
}

#[test]
fn navigation_background_event_sender_preserves_typed_sidecar_for_current_token() {
    let mut conn = CdpConnection::new();
    let (tx, mut rx) = crate::conn::browser_background_output_channel();
    conn.set_background_event_sender(tx);
    let mut browser_context = BrowserContext::new("CTX-nav-typed".to_owned());
    browser_context.set_active_target_id("TID-nav-typed");
    conn.browser_context = Some(browser_context);
    let current = conn
        .start_document_navigation_for_session_owner(None, "LOADER-typed".to_owned())
        .expect("active target should produce current navigation token");
    let message = build_event(
        "Page.frameStartedNavigating",
        json!({
            "frameId": "TID-nav-typed",
            "loaderId": "LOADER-typed",
            "url": "https://example.test/",
            "navigationType": "differentDocument"
        }),
        None,
    );
    let automation_event = AutomationEvent::NavigationFrame(NavigationFrameEvent {
        target_id: DevToolsTargetId::from("TID-nav-typed"),
        frame_id: DevToolsFrameId::from("TID-nav-typed"),
        parent_frame_id: None,
        loader_id: Some(DevToolsLoaderId::from("LOADER-typed")),
        url: "https://example.test/".to_owned(),
        kind: NavigationFrameEventKind::StartedNavigating,
        frame_name: None,
        security_origin: None,
        secure_context_type: None,
    });

    conn.send_navigation_background_protocol_event(
        current,
        BackgroundProtocolEvent::immediate_automation_event(
            message.clone(),
            automation_event.clone(),
        ),
    );

    let background_event = rx
        .try_recv()
        .expect("current navigation event should flush to background sender");
    let (actual_message, actual_automation_event) = background_event.into_parts();
    assert_eq!(actual_message, message);
    assert_eq!(actual_automation_event, Some(automation_event));
}

#[tokio::test]
async fn materialized_navigation_completion_drops_stale_token() {
    let mut conn = CdpConnection::new();
    let mut browser_context = BrowserContext::new("CTX-nav".to_owned());
    browser_context.set_active_target_id("TID-nav");
    conn.insert_browser_context(browser_context);
    let stale = conn
        .start_document_navigation_for_session_owner(None, "LOADER-1".to_owned())
        .expect("active target should produce stale token");
    let current = conn
        .start_document_navigation_for_session_owner(None, "LOADER-2".to_owned())
        .expect("active target should produce current token");
    let state =
        materialized_navigation_test_state(Some(7), "LOADER-1", "https://example.test/stale");
    let navigation =
        MaterializedNavigationLoadOutcome::Failed(MaterializedFailedDocumentProgress {
            error_text: "stale navigation should not emit".to_owned(),
            document_policy: FailedNavigationDocumentPolicy::InvalidateCommittedDocument,
            response_mode: FailedNavigationResponseMode::ProtocolError,
            progress_gate: empty_main_document_progress_gate_for_test(),
        });

    let mut out = Vec::new();
    let mut command_context = CommandDispatchContext::default();
    conn.drain_materialized_navigation_completion_into(
        &mut out,
        MaterializedNavigationCompletion::new(stale.clone(), state, navigation),
        &mut command_context,
    )
    .await;

    assert_eq!(out.len(), 1, "stale completion must emit terminal reply");
    let reply = &out[0];
    assert_eq!(reply["id"], serde_json::json!(7));
    assert_eq!(reply["error"]["code"], serde_json::json!(-32000));
    assert_eq!(
        reply["error"]["message"],
        serde_json::json!("Navigation aborted")
    );
    assert!(
        reply.get("method").is_none(),
        "stale completion must emit a command reply, not an event"
    );
    let facts = conn.browser_fact_snapshot_for_test();
    assert!(facts.iter().any(|fact| {
        matches!(
            fact.fact(),
            BrowserFact::NavigationAccepted {
                navigation,
                superseded_navigation: Some(superseded),
            } if navigation == &current && superseded == &stale
        )
    }));
    assert!(
        !facts.iter().any(|fact| {
            matches!(
                fact.fact(),
                BrowserFact::NavigationFailed { navigation, .. } if navigation == &stale
            )
        }),
        "the successor admission owns the old request terminal without a second occurrence"
    );
}

#[tokio::test]
async fn materialized_navigation_completion_drops_stale_token_without_navigate_id() {
    let mut conn = CdpConnection::new();
    let mut browser_context = BrowserContext::new("CTX-nav-none".to_owned());
    browser_context.set_active_target_id("TID-nav-none");
    conn.insert_browser_context(browser_context);
    let stale = conn
        .start_document_navigation_for_session_owner(None, "LOADER-1".to_owned())
        .expect("active target should produce stale navigation token");
    let _ = conn
        .start_document_navigation_for_session_owner(None, "LOADER-2".to_owned())
        .expect("active target should produce current navigation token");
    let state =
        materialized_navigation_test_state(None, "LOADER-1", "https://example.test/stale-no-id");
    let navigation =
        MaterializedNavigationLoadOutcome::Failed(MaterializedFailedDocumentProgress {
            error_text: "stale navigation should not emit without a navigate id".to_owned(),
            document_policy: FailedNavigationDocumentPolicy::InvalidateCommittedDocument,
            response_mode: FailedNavigationResponseMode::ProtocolError,
            progress_gate: empty_main_document_progress_gate_for_test(),
        });

    let mut out = Vec::new();
    let mut command_context = CommandDispatchContext::default();
    conn.drain_materialized_navigation_completion_into(
        &mut out,
        MaterializedNavigationCompletion::new(stale, state, navigation),
        &mut command_context,
    )
    .await;

    assert!(
        out.is_empty(),
        "stale completion without navigate id must not emit protocol output"
    );
}

#[tokio::test]
async fn materialized_navigation_completion_drains_current_token() {
    let mut conn = CdpConnection::new();
    let mut browser_context = BrowserContext::new("CTX-nav".to_owned());
    browser_context.set_active_target_id("TID-nav");
    conn.insert_browser_context(browser_context);
    let current = conn
        .start_document_navigation_for_session_owner(None, "LOADER-1".to_owned())
        .expect("active target should produce current token");
    let previous_page = conn
        .target_page_residence_identity_for_session(None)
        .expect("current Page residence");
    let state =
        materialized_navigation_test_state(Some(8), "LOADER-1", "https://example.test/current");
    let navigation =
        MaterializedNavigationLoadOutcome::Failed(MaterializedFailedDocumentProgress {
            error_text: "current navigation should emit".to_owned(),
            document_policy: FailedNavigationDocumentPolicy::InvalidateCommittedDocument,
            response_mode: FailedNavigationResponseMode::ProtocolError,
            progress_gate: empty_main_document_progress_gate_for_test(),
        });

    let mut out = Vec::new();
    let mut command_context = CommandDispatchContext::default();
    conn.drain_materialized_navigation_completion_into(
        &mut out,
        MaterializedNavigationCompletion::new(current.clone(), state, navigation),
        &mut command_context,
    )
    .await;

    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["id"], json!(8));
    assert_eq!(out[0]["error"]["code"], json!(-32000));
    assert_eq!(
        out[0]["error"]["message"],
        json!("current navigation should emit")
    );
    let facts = conn.browser_fact_snapshot_for_test();
    let failed = facts
        .iter()
        .find(|fact| {
            matches!(
                fact.fact(),
                BrowserFact::NavigationFailed { navigation, .. } if navigation == &current
            )
        })
        .expect("network failure should publish the exact request terminal");
    assert_eq!(
        failed.fact(),
        &BrowserFact::NavigationFailed {
            navigation: current,
            failure: BrowserNavigationFailure::Network {
                error_text: "current navigation should emit".to_owned(),
            },
            previous_page: Some(previous_page),
        }
    );
}

#[tokio::test]
async fn materialized_download_retires_request_without_replacing_current_page() {
    let mut conn = CdpConnection::new();
    let mut browser_context = BrowserContext::new("CTX-download".to_owned());
    browser_context.set_active_target_id("TID-nav");
    conn.insert_browser_context(browser_context);
    let navigation = conn
        .start_document_navigation_for_session_owner(None, "LOADER-download".to_owned())
        .expect("active target should produce download token");
    let current_page = conn
        .target_page_residence_identity_for_session(None)
        .expect("current Page residence");
    let state = materialized_navigation_test_state(
        Some(9),
        "LOADER-download",
        "https://example.test/download",
    );
    let download =
        MaterializedNavigationLoadOutcome::Download(MaterializedDownloadDocumentProgress {
            final_url: Url::parse("https://example.test/download").expect("download URL"),
            progress_gate: empty_main_document_progress_gate_for_test(),
            body_artifact: CompletedDownloadBodyArtifact::from_body(
                CompletedDownloadBody::Buffered(Vec::new()),
                Vec::new(),
            ),
        });

    let mut out = Vec::new();
    let mut command_context = CommandDispatchContext::default();
    conn.drain_materialized_navigation_completion_into(
        &mut out,
        MaterializedNavigationCompletion::new(navigation.clone(), state, download),
        &mut command_context,
    )
    .await;

    assert!(!conn.has_pending_document_navigation_for_session_owner(None));
    assert_eq!(
        conn.target_page_residence_identity_for_session(None)
            .as_ref(),
        Some(&current_page)
    );
    let facts = conn.browser_fact_snapshot_for_test();
    assert!(facts.iter().any(|fact| {
        fact.page_residence() == &current_page
            && fact.fact()
                == &BrowserFact::NavigationConvertedToDownload {
                    navigation: navigation.clone(),
                }
    }));
}

fn materialized_navigation_test_state(
    navigate_id: Option<u64>,
    loader_id: &str,
    requested_url: &str,
) -> NavigationDispatchState {
    NavigationDispatchState {
        navigate_id,
        navigate_session_id: None,
        result_projection: NavigationResultProjection::Cdp(
            json!({ "frameId": "TID-nav", "loaderId": loader_id }),
        ),
        frame_id: "TID-nav".to_owned(),
        session_id: None,
        request_id: Some(loader_id.to_owned()),
        loader_id: loader_id.to_owned(),
        request_announced: true,
        requested_url: Url::parse(requested_url).unwrap(),
        request_method: "GET".to_owned(),
        request_body: None,
        request_body_bytes: None,
        request_headers: Vec::new(),
        request_load_policy: crate::conn::NavigationRequestLoadPolicy::DocumentInitiated,
        timestamp: 0.0,
        source_document_security: Default::default(),
    }
}

fn site_summary(
    name: &str,
    cookie_count: usize,
    persistent_cookie_count: usize,
) -> CookieSiteDataSummary {
    CookieSiteDataSummary::new(name.to_owned(), cookie_count, persistent_cookie_count)
}
