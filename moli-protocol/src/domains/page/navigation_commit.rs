use url::Url;

use crate::conn::{
    CdpConnection, CommandDispatchContext, CommittedRendererAgentAttachment,
    DocumentNavigationToken, NavigationDispatchState,
};
use crate::domains::activity::{
    MainDocumentDownloadNavigationActivity, MainDocumentNavigationActivity,
};
use crate::domains::command_output::CommandOutputBuffer;
use crate::domains::network::{
    MaterializedDownloadDocumentProgress, MaterializedLoadedDocumentProgress,
};
use moli_core::{
    RendererOutputFence,
    browser_host::{BrowserNavigationFailure, BrowserPageOwnerKey},
    page::{
        RendererDocumentLifecycleEvent, RendererDocumentLifecycleEventKind,
        RendererDocumentLifecycleMilestone, RendererPageCreationArtifacts,
        RendererPendingDownloadActivation,
    },
    runtime::NavigationEngine,
};

use super::loaded_page_install::{
    CompletedLoadedPageInstall, CompletedLoadedPagePostInstall, LoadedPageInstallOutcome,
    LoadedPageInstallStart, LoadedPagePostInstallStart, PendingLoadedPageInstall,
    PendingLoadedPagePostInstall, complete_loaded_navigation_page_post_install,
    start_loaded_navigation_page_install, start_loaded_navigation_page_post_install,
};
use super::loaded_page_restore::{
    CompletedLoadedPageRestore, LoadedPageRestoreStart, PendingLoadedPageRestore,
    start_loaded_navigation_page_restore,
};

pub(super) enum LoadedNavigationCommitStart {
    Pending(Box<PendingLoadedNavigationCommit>),
    Ready(Box<CompletedLoadedNavigationCommit>),
    Rejected,
}

pub(super) struct PendingLoadedNavigationCommit {
    continuation: LoadedNavigationCommitContinuation,
    restore: PendingLoadedPageRestore,
}

pub(super) struct CompletedLoadedNavigationCommit {
    continuation: LoadedNavigationCommitContinuation,
    restore: CompletedLoadedPageRestore,
}

pub(super) enum LoadedNavigationCommitApplyStart {
    Pending(Box<PendingLoadedNavigationPageDisposal>),
    Ready(Box<CompletedLoadedNavigationPageDisposal>),
    Rejected,
}

pub(super) struct PendingLoadedNavigationPageDisposal {
    continuation: LoadedNavigationCommitContinuation,
    install: PendingLoadedPageInstall,
}

pub(super) struct CompletedLoadedNavigationPageDisposal {
    continuation: LoadedNavigationCommitContinuation,
    install: CompletedLoadedPageInstall,
}

pub(super) enum LoadedNavigationPostDisposalStart {
    Pending(Box<PendingLoadedNavigationPreloadListeners>),
    Ready(Option<BrowserPageOwnerKey>),
}

pub(super) struct PendingLoadedNavigationPreloadListeners {
    continuation: LoadedNavigationCommitContinuation,
    post_install: PendingLoadedPagePostInstall,
}

pub(super) struct CompletedLoadedNavigationPreloadListeners {
    continuation: LoadedNavigationCommitContinuation,
    post_install: CompletedLoadedPagePostInstall,
}

struct LoadedNavigationCommitContinuation {
    token: DocumentNavigationToken,
    navigation_activity: MainDocumentNavigationActivity,
    pending_download: Option<RendererPendingDownloadActivation>,
    page_creation_artifacts: RendererPageCreationArtifacts,
    deferred_initial_renderer_document_lifecycle_events: Vec<RendererDocumentLifecycleEvent>,
    final_url: Url,
    response_headers: Vec<(String, String)>,
    response_from_cache: bool,
    main_document_body: Option<crate::conn::CapturedBody>,
    is_network_error_page: bool,
    renderer_output_predecessor: Option<RendererOutputFence>,
    navigation_engine: Option<NavigationEngine>,
    engine_adoption_error: Option<String>,
}

impl PendingLoadedNavigationCommit {
    pub(super) async fn wait(self) -> CompletedLoadedNavigationCommit {
        CompletedLoadedNavigationCommit {
            continuation: self.continuation,
            restore: self.restore.wait().await,
        }
    }
}

impl LoadedNavigationCommitApplyStart {
    pub(super) fn committed_owner(&self) -> Option<&BrowserPageOwnerKey> {
        match self {
            Self::Pending(pending) => pending.committed_owner(),
            Self::Ready(completed) => completed.committed_owner(),
            Self::Rejected => None,
        }
    }
}

impl PendingLoadedNavigationPageDisposal {
    pub(super) fn committed_owner(&self) -> Option<&BrowserPageOwnerKey> {
        self.install.committed_owner()
    }

    pub(super) async fn wait(self) -> CompletedLoadedNavigationPageDisposal {
        CompletedLoadedNavigationPageDisposal {
            continuation: self.continuation,
            install: self.install.wait().await,
        }
    }
}

impl CompletedLoadedNavigationPageDisposal {
    pub(super) fn committed_owner(&self) -> Option<&BrowserPageOwnerKey> {
        self.install.committed_owner()
    }
}

impl PendingLoadedNavigationPreloadListeners {
    pub(super) async fn wait(self) -> CompletedLoadedNavigationPreloadListeners {
        CompletedLoadedNavigationPreloadListeners {
            continuation: self.continuation,
            post_install: self.post_install.wait().await,
        }
    }
}

pub(super) async fn commit_loaded_navigation_async(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    token: &DocumentNavigationToken,
    state: NavigationDispatchState,
    navigation: MaterializedLoadedDocumentProgress,
    committed_renderer_attachment: Option<CommittedRendererAgentAttachment>,
    command_context: &mut CommandDispatchContext,
) -> Option<BrowserPageOwnerKey> {
    let navigation_session_id = state.navigate_session_id.clone();
    let completed = match start_loaded_navigation_commit(
        conn,
        out,
        token.clone(),
        state,
        navigation,
        committed_renderer_attachment,
    ) {
        LoadedNavigationCommitStart::Pending(pending) => pending.wait().await,
        LoadedNavigationCommitStart::Ready(completed) => *completed,
        LoadedNavigationCommitStart::Rejected => {
            let _ = conn.fail_document_navigation_for_session_owner_if_matches(
                navigation_session_id.as_deref(),
                token,
                BrowserNavigationFailure::Commit {
                    error_text: "loaded navigation restore was rejected".to_owned(),
                },
            );
            return None;
        }
    };
    complete_loaded_navigation_commit_async(conn, out, completed, command_context).await
}

pub(super) fn start_loaded_navigation_commit(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    token: DocumentNavigationToken,
    state: NavigationDispatchState,
    navigation: MaterializedLoadedDocumentProgress,
    committed_renderer_attachment: Option<CommittedRendererAgentAttachment>,
) -> LoadedNavigationCommitStart {
    let MaterializedLoadedDocumentProgress {
        page,
        pending_download,
        page_creation_artifacts,
        final_url,
        response_headers,
        response_from_cache,
        main_document_body,
        initial_runtime_realms,
        renderer_output_predecessor,
        main_document_commit,
        progress_gate,
        navigation_engine,
        network_error_page,
    } = navigation;
    let target_url = network_error_page
        .as_ref()
        .map(|error_page| error_page.unreachable_url().clone())
        .unwrap_or_else(|| final_url.clone());
    let Some(main_document_commit) = main_document_commit else {
        let error = "loaded navigation is missing its frozen main Document commit identity";
        if state.navigate_id.is_some() {
            out.push_error_after_messages(-32000, error);
        } else {
            tracing::warn!(
                session_id = state.navigate_session_id.as_deref(),
                loader_id = state.loader_id,
                "{error} after early Page.navigate result"
            );
        }
        return LoadedNavigationCommitStart::Rejected;
    };
    let is_network_error_page = network_error_page.is_some();
    let (page_creation_artifacts, deferred_initial_renderer_document_lifecycle_events) =
        split_renderer_page_creation_lifecycle_at_load_boundary(page_creation_artifacts);
    let mut navigation_activity = MainDocumentNavigationActivity::new(
        state,
        final_url.clone(),
        progress_gate,
        Some(token.clone()),
    );
    if let Some(error_page) = network_error_page.as_ref() {
        navigation_activity =
            navigation_activity.with_network_error_page_result(error_page.error_text().to_owned());
    }
    let restore = start_loaded_navigation_page_restore(
        conn,
        &token,
        navigation_activity.state(),
        page,
        &final_url,
        target_url,
        main_document_commit,
        initial_runtime_realms,
        committed_renderer_attachment,
    );
    let continuation = LoadedNavigationCommitContinuation {
        token,
        navigation_activity,
        pending_download,
        page_creation_artifacts,
        deferred_initial_renderer_document_lifecycle_events,
        final_url,
        response_headers,
        response_from_cache,
        main_document_body,
        is_network_error_page,
        renderer_output_predecessor,
        navigation_engine,
        engine_adoption_error: None,
    };
    match restore {
        LoadedPageRestoreStart::Pending(restore) => {
            LoadedNavigationCommitStart::Pending(Box::new(PendingLoadedNavigationCommit {
                continuation,
                restore: *restore,
            }))
        }
        LoadedPageRestoreStart::Ready(restore) => {
            LoadedNavigationCommitStart::Ready(Box::new(CompletedLoadedNavigationCommit {
                continuation,
                restore,
            }))
        }
        LoadedPageRestoreStart::Rejected => LoadedNavigationCommitStart::Rejected,
    }
}

pub(super) async fn complete_loaded_navigation_commit_async(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    completed: CompletedLoadedNavigationCommit,
    command_context: &mut CommandDispatchContext,
) -> Option<BrowserPageOwnerKey> {
    let completed =
        match start_completed_loaded_navigation_commit(conn, out, completed, command_context) {
            LoadedNavigationCommitApplyStart::Pending(pending) => pending.wait().await,
            LoadedNavigationCommitApplyStart::Ready(completed) => *completed,
            LoadedNavigationCommitApplyStart::Rejected => return None,
        };
    complete_loaded_navigation_commit_after_page_disposal_async(conn, out, completed).await
}

pub(super) fn start_completed_loaded_navigation_commit(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    completed: CompletedLoadedNavigationCommit,
    command_context: &mut CommandDispatchContext,
) -> LoadedNavigationCommitApplyStart {
    let CompletedLoadedNavigationCommit {
        mut continuation,
        restore,
    } = completed;
    let install = start_loaded_navigation_page_install(
        conn,
        out,
        &continuation.token,
        continuation.navigation_activity.state(),
        &continuation.final_url,
        restore,
        command_context,
    );
    if install.committed_owner().is_none() {
        let _ = conn.fail_document_navigation_for_session_owner_if_matches(
            continuation
                .navigation_activity
                .state()
                .navigate_session_id
                .as_deref(),
            &continuation.token,
            BrowserNavigationFailure::Commit {
                error_text: "loaded navigation Page install did not commit".to_owned(),
            },
        );
    }
    if let Some(owner) = install.committed_owner().cloned()
        && let Some(engine) = continuation.navigation_engine.take()
    {
        continuation.engine_adoption_error = conn
            .adopt_loaded_navigation_engine_for_target_owner(owner, engine)
            .err()
            .map(|error| format!("failed to adopt loaded Page engine: {error}"));
    }
    match install {
        LoadedPageInstallStart::Pending(install) => LoadedNavigationCommitApplyStart::Pending(
            Box::new(PendingLoadedNavigationPageDisposal {
                continuation,
                install: *install,
            }),
        ),
        LoadedPageInstallStart::Ready(install) => LoadedNavigationCommitApplyStart::Ready(
            Box::new(CompletedLoadedNavigationPageDisposal {
                continuation,
                install,
            }),
        ),
        LoadedPageInstallStart::Failed => LoadedNavigationCommitApplyStart::Rejected,
    }
}

pub(super) async fn complete_loaded_navigation_commit_after_page_disposal_async(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    completed: CompletedLoadedNavigationPageDisposal,
) -> Option<BrowserPageOwnerKey> {
    let mut step = start_loaded_navigation_commit_after_page_disposal(conn, out, completed);
    loop {
        match step {
            LoadedNavigationPostDisposalStart::Pending(pending) => {
                let completed = (*pending).wait().await;
                step = complete_loaded_navigation_preload_listeners(conn, out, completed);
            }
            LoadedNavigationPostDisposalStart::Ready(owner) => return owner,
        }
    }
}

pub(super) fn start_loaded_navigation_commit_after_page_disposal(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    completed: CompletedLoadedNavigationPageDisposal,
) -> LoadedNavigationPostDisposalStart {
    let CompletedLoadedNavigationPageDisposal {
        continuation,
        install,
    } = completed;
    let post_install = start_loaded_navigation_page_post_install(
        conn,
        out,
        continuation.navigation_activity.state(),
        install,
    );
    continue_loaded_navigation_after_page_post_install(conn, out, continuation, post_install)
}

pub(super) fn complete_loaded_navigation_preload_listeners(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    completed: CompletedLoadedNavigationPreloadListeners,
) -> LoadedNavigationPostDisposalStart {
    let CompletedLoadedNavigationPreloadListeners {
        continuation,
        post_install,
    } = completed;
    let post_install = complete_loaded_navigation_page_post_install(conn, out, post_install);
    continue_loaded_navigation_after_page_post_install(conn, out, continuation, post_install)
}

fn continue_loaded_navigation_after_page_post_install(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    continuation: LoadedNavigationCommitContinuation,
    post_install: LoadedPagePostInstallStart,
) -> LoadedNavigationPostDisposalStart {
    match post_install {
        LoadedPagePostInstallStart::Pending(post_install) => {
            LoadedNavigationPostDisposalStart::Pending(Box::new(
                PendingLoadedNavigationPreloadListeners {
                    continuation,
                    post_install: *post_install,
                },
            ))
        }
        LoadedPagePostInstallStart::Ready(Some(commit)) => {
            LoadedNavigationPostDisposalStart::Ready(
                finish_loaded_navigation_commit_after_page_post_install(
                    conn,
                    out,
                    continuation,
                    commit,
                ),
            )
        }
        LoadedPagePostInstallStart::Ready(None) => LoadedNavigationPostDisposalStart::Ready(None),
    }
}

fn finish_loaded_navigation_commit_after_page_post_install(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    continuation: LoadedNavigationCommitContinuation,
    commit: LoadedPageInstallOutcome,
) -> Option<BrowserPageOwnerKey> {
    let LoadedNavigationCommitContinuation {
        token,
        mut navigation_activity,
        pending_download,
        page_creation_artifacts,
        mut deferred_initial_renderer_document_lifecycle_events,
        final_url,
        response_headers,
        response_from_cache,
        main_document_body,
        is_network_error_page,
        renderer_output_predecessor,
        navigation_engine,
        engine_adoption_error,
    } = continuation;
    let navigation_session_id = navigation_activity.state().navigate_session_id.clone();
    if !is_network_error_page {
        let _ = conn.commit_main_document_resource_for_session_owner(
            navigation_session_id.as_deref(),
            navigation_activity.state().frame_id.clone(),
            navigation_activity.state().loader_id.clone(),
            final_url.clone(),
            response_headers,
            response_from_cache,
            main_document_body,
        );
    }

    let LoadedPageInstallOutcome {
        owner: committed_owner,
    } = commit;
    let (renderer_document_binding, mut initial_renderer_document_lifecycle_events) = conn
        .bind_renderer_document_lifecycle_for_session_owner(
            navigation_activity.state().navigate_session_id.as_deref(),
            page_creation_artifacts,
            Some(token),
            navigation_activity.state().frame_id.clone(),
            navigation_activity.state().loader_id.clone(),
        );
    let load_visibility_barrier_armed = renderer_document_binding.is_some()
        && conn.begin_renderer_document_load_visibility_barrier_for_session_owner(
            navigation_activity.state().navigate_session_id.as_deref(),
            &navigation_activity.state().loader_id,
        );
    if load_visibility_barrier_armed {
        // The creation artifact owns only the initial handoff prefix. Once
        // that prefix is taken, every later lifecycle fact is frozen directly
        // into the Page output FIFO, even if it is produced before protocol
        // finishes installing this binding. Never read the Page back here:
        // ordered ingress and the commit cursor preserve that handoff.
        let (_, visible_events) = conn.ingest_renderer_document_lifecycle_events_for_session_owner(
            navigation_activity.state().navigate_session_id.as_deref(),
            std::mem::take(&mut deferred_initial_renderer_document_lifecycle_events),
        );
        initial_renderer_document_lifecycle_events.extend(visible_events);
    }
    navigation_activity.defer_initial_renderer_document_lifecycle_events_until_load_boundary(
        deferred_initial_renderer_document_lifecycle_events,
    );
    let late_engine_adoption_error = navigation_engine.and_then(|engine| {
        let Some(owner) = committed_owner.as_ref().cloned() else {
            return Some("loaded Page engine has no committed Browser owner".to_owned());
        };
        conn.adopt_loaded_navigation_engine_for_target_owner(owner, engine)
            .err()
            .map(|error| format!("failed to adopt loaded Page engine: {error}"))
    });
    let engine_adoption_error = engine_adoption_error.or(late_engine_adoption_error);
    if let Some(message) = engine_adoption_error {
        if navigation_activity.state().navigate_id.is_some() {
            navigation_activity.emit_navigation_error_instead_of_result_into_buffer(out, message);
        } else {
            tracing::warn!(
                error = %message,
                session_id = navigation_session_id.as_deref(),
                "navigation engine adoption failed after early Page.navigate result"
            );
        }
    }

    navigation_activity.emit_loaded_navigation_commit(
        conn,
        out,
        pending_download,
        renderer_document_binding,
        initial_renderer_document_lifecycle_events,
        renderer_output_predecessor,
    );
    committed_owner
}

fn split_renderer_page_creation_lifecycle_at_load_boundary(
    mut artifacts: RendererPageCreationArtifacts,
) -> (
    RendererPageCreationArtifacts,
    Vec<RendererDocumentLifecycleEvent>,
) {
    let Some(load_sequence) = artifacts
        .lifecycle_snapshot
        .load
        .as_ref()
        .map(|stamp| stamp.sequence)
    else {
        return (artifacts, Vec::new());
    };

    let mut deferred = Vec::new();
    let mut before_load = Vec::new();
    for event in std::mem::take(&mut artifacts.initial_lifecycle_events) {
        if event.sequence >= load_sequence {
            deferred.push(event);
        } else {
            before_load.push(event);
        }
    }
    artifacts.initial_lifecycle_events = before_load;
    artifacts.lifecycle_snapshot.load = None;
    if artifacts
        .lifecycle_snapshot
        .terminated
        .as_ref()
        .is_some_and(|stamp| stamp.sequence >= load_sequence)
    {
        artifacts.lifecycle_snapshot.terminated = None;
    }

    if !deferred.iter().any(|event| {
        matches!(
            event.kind,
            RendererDocumentLifecycleEventKind::Milestone(RendererDocumentLifecycleMilestone::Load)
        )
    }) {
        tracing::warn!(
            load_sequence,
            "renderer page creation snapshot contained load without its journal event"
        );
    }

    (artifacts, deferred)
}

pub(super) async fn commit_download_navigation_async(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    state: NavigationDispatchState,
    navigation: MaterializedDownloadDocumentProgress,
    command_context: &mut CommandDispatchContext,
) {
    let MaterializedDownloadDocumentProgress {
        final_url,
        progress_gate,
        body_artifact,
    } = navigation;
    let navigation_activity =
        MainDocumentNavigationActivity::new(state, final_url, progress_gate, None);
    let download_activity =
        MainDocumentDownloadNavigationActivity::new(navigation_activity, body_artifact);

    // Keep this boxed for the same reason as the loaded commit tail: the
    // navigation completion future is otherwise large on small test stacks.
    Box::pin(async move {
        download_activity
            .emit_commit_into_buffer_async(conn, out, command_context)
            .await;
    })
    .await;
}
