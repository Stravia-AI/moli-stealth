use std::sync::Arc;
use url::Url;

use crate::conn::{
    CdpConnection, CommittedRendererAgentAttachment, DocumentNavigationToken,
    LoadedNavigationRendererAttachmentCommit, NavigationDispatchState,
    TargetLoadedNavigationCommitState,
};
use crate::domains::command_output::CommandOutputBuffer;
use moli_core::{
    RendererOutputFence,
    page::{
        Page, PermissionOverrideRegistration, RendererMainDocumentCommit, RendererRuntimeRealmInfo,
    },
};

pub(super) enum LoadedPageRestoreStart {
    Pending(Box<PendingLoadedPageRestore>),
    Ready(CompletedLoadedPageRestore),
    Rejected,
}

pub(super) struct PendingLoadedPageRestore {
    page: Page,
    commit_state: TargetLoadedNavigationCommitState,
    permission_overrides: Vec<PermissionOverrideRegistration>,
    renderer_attachment_commit: LoadedNavigationRendererAttachmentCommit,
    preload_channel_realms: Vec<RendererRuntimeRealmInfo>,
    final_url: Url,
    target_url: Url,
    main_document_commit: Arc<RendererMainDocumentCommit>,
    timing_started: Option<std::time::Instant>,
}

pub(super) enum CompletedLoadedPageRestore {
    Configured(Box<ReadyLoadedPageRestore>),
    MissingCommitState,
    Failed {
        failure: LoadedPageRestoreFailure,
        runtime_output_predecessor: Option<RendererOutputFence>,
    },
}

pub(super) struct ReadyLoadedPageRestore {
    pub(super) page: Page,
    pub(super) commit_state: TargetLoadedNavigationCommitState,
    pub(super) renderer_attachment_commit: LoadedNavigationRendererAttachmentCommit,
    pub(super) runtime_output_predecessor: Option<RendererOutputFence>,
    pub(super) preload_channel_realms: Vec<RendererRuntimeRealmInfo>,
    pub(super) target_url: Url,
    pub(super) main_document_commit: Arc<RendererMainDocumentCommit>,
    pub(super) timing_started: Option<std::time::Instant>,
}

pub(super) enum LoadedPageRestoreFailure {
    Runtime(String),
    Fetch(String),
    Permissions(String),
}

pub(super) fn start_loaded_navigation_page_restore(
    conn: &mut CdpConnection,
    token: &DocumentNavigationToken,
    state: &NavigationDispatchState,
    page: Page,
    final_url: &Url,
    target_url: Url,
    main_document_commit: Arc<RendererMainDocumentCommit>,
    initial_runtime_realms: Vec<RendererRuntimeRealmInfo>,
    committed_renderer_attachment: Option<CommittedRendererAgentAttachment>,
) -> LoadedPageRestoreStart {
    let timing_enabled = moli_trace::cdp_nav_timing_enabled();
    let timing_started = timing_enabled.then(std::time::Instant::now);
    if timing_enabled {
        tracing::info!(
            target: "moli_cdp_nav_timing",
            url = %final_url,
            stage = "restore_commit_start",
        );
    }
    let prepared_configuration_committed = committed_renderer_attachment.is_some();
    let mut page = page;
    let page_agent_token = page.renderer_devtools_agent_token();
    let renderer_attachment_commit = match committed_renderer_attachment {
        Some(transaction) => {
            if token != transaction.navigation()
                || transaction.current().agent_token() != page_agent_token
                || conn.current_renderer_agent_attachment_id_for_session_owner(
                    state.navigate_session_id.as_deref(),
                ) != Some(transaction.current().id())
            {
                tracing::warn!(
                    session_id = state.navigate_session_id.as_deref(),
                    "prepared navigation Page does not match its committed renderer attachment"
                );
                return LoadedPageRestoreStart::Rejected;
            }
            page.bind_renderer_agent_attachment(transaction.current().id());
            LoadedNavigationRendererAttachmentCommit::AlreadyCommitted(transaction)
        }
        None => {
            match conn.prepare_renderer_agent_candidate_for_session_owner(
                state.navigate_session_id.as_deref(),
                token,
                &mut page,
            ) {
                Ok(candidate) => LoadedNavigationRendererAttachmentCommit::Prepare(Some(candidate)),
                Err(error) => {
                    tracing::debug!(
                        %error,
                        session_id = state.navigate_session_id.as_deref(),
                        loader_id = token.loader_id(),
                        "dropping superseded renderer navigation candidate before commit"
                    );
                    return LoadedPageRestoreStart::Rejected;
                }
            }
        }
    };
    let Some(commit_state) = conn
        .prepare_loaded_navigation_commit_for_session_owner(state.navigate_session_id.as_deref())
    else {
        return LoadedPageRestoreStart::Ready(CompletedLoadedPageRestore::MissingCommitState);
    };
    let permission_overrides = conn
        .effective_permission_overrides_for_browser_context_id(&commit_state.browser_context_id);
    let preload_channel_realms = dedupe_preload_channel_realms(initial_runtime_realms);
    // This inventory resolves BiDi listener context IDs. Live CDP
    // context-created facts still travel only through renderer output.
    if prepared_configuration_committed {
        for stage in [
            "restore_commit_runtime_restored",
            "restore_commit_fetch_restored",
            "restore_commit_permissions_restored",
        ] {
            emit_loaded_page_restore_timing(
                final_url,
                stage,
                timing_enabled.then(std::time::Instant::now),
                timing_started.as_ref(),
            );
        }
        LoadedPageRestoreStart::Ready(CompletedLoadedPageRestore::Configured(Box::new(
            ReadyLoadedPageRestore {
                page,
                commit_state,
                renderer_attachment_commit,
                runtime_output_predecessor: None,
                preload_channel_realms,
                target_url,
                main_document_commit,
                timing_started,
            },
        )))
    } else {
        LoadedPageRestoreStart::Pending(Box::new(PendingLoadedPageRestore {
            page,
            commit_state,
            permission_overrides,
            renderer_attachment_commit,
            preload_channel_realms,
            final_url: final_url.clone(),
            target_url,
            main_document_commit,
            timing_started,
        }))
    }
}

impl PendingLoadedPageRestore {
    pub(super) async fn wait(mut self) -> CompletedLoadedPageRestore {
        let restore_started = self.timing_started.is_some().then(std::time::Instant::now);
        let runtime_output_predecessor = match self
            .page
            .restore_runtime_protocol_state_async(
                self.commit_state
                    .renderer_runtime_inspector_session_id
                    .clone(),
                &self
                    .commit_state
                    .runtime_inspector_session_restore_snapshots,
                &self.commit_state.isolated_worlds,
                &self.commit_state.stored_runtime_bindings,
                &self.commit_state.session_runtime_bindings,
                self.commit_state.runtime_frontend_enabled,
            )
            .await
        {
            Ok(predecessor) => predecessor,
            Err(error) => {
                return CompletedLoadedPageRestore::Failed {
                    failure: LoadedPageRestoreFailure::Runtime(error.to_string()),
                    runtime_output_predecessor: None,
                };
            }
        };
        emit_loaded_page_restore_timing(
            &self.final_url,
            "restore_commit_runtime_restored",
            restore_started,
            self.timing_started.as_ref(),
        );

        let (fetch_subresource_enabled, fetch_subresource_resource_type) =
            self.commit_state.fetch_subresource_config;
        let fetch_restore_started = self.timing_started.is_some().then(std::time::Instant::now);
        if (fetch_subresource_enabled || fetch_subresource_resource_type.is_some())
            && let Err(error) = self
                .page
                .set_fetch_subresource_interception_async(
                    fetch_subresource_enabled,
                    fetch_subresource_resource_type,
                )
                .await
        {
            return CompletedLoadedPageRestore::Failed {
                failure: LoadedPageRestoreFailure::Fetch(error.to_string()),
                runtime_output_predecessor,
            };
        }
        emit_loaded_page_restore_timing(
            &self.final_url,
            "restore_commit_fetch_restored",
            fetch_restore_started,
            self.timing_started.as_ref(),
        );

        let permission_started = self.timing_started.is_some().then(std::time::Instant::now);
        if !self.permission_overrides.is_empty()
            && let Err(error) = self
                .page
                .set_permission_overrides_async(&self.permission_overrides)
                .await
        {
            return CompletedLoadedPageRestore::Failed {
                failure: LoadedPageRestoreFailure::Permissions(error.to_string()),
                runtime_output_predecessor,
            };
        }
        emit_loaded_page_restore_timing(
            &self.final_url,
            "restore_commit_permissions_restored",
            permission_started,
            self.timing_started.as_ref(),
        );

        CompletedLoadedPageRestore::Configured(Box::new(ReadyLoadedPageRestore {
            page: self.page,
            commit_state: self.commit_state,
            renderer_attachment_commit: self.renderer_attachment_commit,
            runtime_output_predecessor,
            preload_channel_realms: self.preload_channel_realms,
            target_url: self.target_url,
            main_document_commit: self.main_document_commit,
            timing_started: self.timing_started,
        }))
    }
}

fn emit_loaded_page_restore_timing(
    final_url: &Url,
    stage: &'static str,
    phase_started: Option<std::time::Instant>,
    timing_started: Option<&std::time::Instant>,
) {
    let Some(phase_started) = phase_started else {
        return;
    };
    tracing::info!(
        target: "moli_cdp_nav_timing",
        url = %final_url,
        stage,
        phase_ms = phase_started.elapsed().as_millis(),
        elapsed_ms = timing_started
            .map(std::time::Instant::elapsed)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or_default(),
    );
}

pub(super) fn report_loaded_page_restore_failure(
    out: &mut CommandOutputBuffer,
    state: &NavigationDispatchState,
    failure: LoadedPageRestoreFailure,
) {
    match failure {
        LoadedPageRestoreFailure::Runtime(error) => {
            if state.navigate_id.is_some() {
                out.push_error_after_messages(
                    -32000,
                    format!("failed to restore page runtime protocol state: {error}"),
                );
            } else {
                tracing::warn!(
                    %error,
                    session_id = state.navigate_session_id.as_deref(),
                    "navigation commit failed after early Page.navigate result: runtime protocol state restore"
                );
            }
        }
        LoadedPageRestoreFailure::Fetch(error) => {
            if state.navigate_id.is_some() {
                out.push_error_after_messages(
                    -32000,
                    format!("failed to restore page fetch interception state: {error}"),
                );
            } else {
                tracing::warn!(
                    %error,
                    session_id = state.navigate_session_id.as_deref(),
                    "navigation commit failed after early Page.navigate result: fetch interception state restore"
                );
            }
        }
        LoadedPageRestoreFailure::Permissions(error) => {
            if state.navigate_id.is_some() {
                out.push_error_after_messages(
                    -32000,
                    format!("failed to apply page permission overrides: {error}"),
                );
            } else {
                tracing::warn!(
                    %error,
                    session_id = state.navigate_session_id.as_deref(),
                    "navigation commit failed after early Page.navigate result: permission overrides apply"
                );
            }
        }
    }
}

fn runtime_realm_has_native_unique_id(realm: &RendererRuntimeRealmInfo) -> bool {
    realm
        .realm_id
        .as_deref()
        .is_some_and(|realm_id| !realm_id.is_empty())
}

pub(super) fn dedupe_preload_channel_realms(
    realms: Vec<RendererRuntimeRealmInfo>,
) -> Vec<RendererRuntimeRealmInfo> {
    let mut deduped = Vec::new();
    for realm in realms {
        if runtime_realm_has_native_unique_id(&realm)
            && !deduped
                .iter()
                .any(|existing: &RendererRuntimeRealmInfo| existing.context_id == realm.context_id)
        {
            deduped.push(realm);
        }
    }
    deduped
}
