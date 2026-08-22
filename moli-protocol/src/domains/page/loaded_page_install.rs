use url::Url;

use crate::conn::{
    CdpConnection, CommandDispatchContext, DocumentNavigationToken, LoadedPageReplacementOutcome,
    LoadedPageReplacementStart, NavigationDispatchState, PendingLoadedPageReplacement,
    TargetLoadedNavigationCommitState,
};
use crate::domains::command_output::CommandOutputBuffer;
use crate::domains::runtime::{
    BidiPreloadListenerBatchStep, CompletedBidiPreloadListenerBatch,
    PendingBidiPreloadListenerBatch, complete_bidi_preload_listener_batch,
    start_bidi_preload_listener_batch,
};
use moli_core::{browser_host::BrowserPageOwnerKey, page::RendererRuntimeRealmInfo};

use super::loaded_page_restore::{
    CompletedLoadedPageRestore, ReadyLoadedPageRestore, report_loaded_page_restore_failure,
};

#[derive(Default)]
pub(super) struct LoadedPageInstallOutcome {
    pub(super) owner: Option<BrowserPageOwnerKey>,
}

pub(super) enum LoadedPageInstallStart {
    Pending(Box<PendingLoadedPageInstall>),
    Ready(CompletedLoadedPageInstall),
    Failed,
}

pub(super) struct PendingLoadedPageInstall {
    continuation: LoadedPageInstallContinuation,
    replacement: PendingLoadedPageReplacement,
}

pub(super) enum CompletedLoadedPageInstall {
    MissingCommitState,
    Replacement(Box<CompletedLoadedPageReplacement>),
}

pub(super) struct CompletedLoadedPageReplacement {
    continuation: LoadedPageInstallContinuation,
    outcome: LoadedPageReplacementOutcome,
}

struct LoadedPageInstallContinuation {
    commit_state: TargetLoadedNavigationCommitState,
    preload_channel_realms: Vec<RendererRuntimeRealmInfo>,
    final_url: Url,
    timing_started: Option<std::time::Instant>,
    page_commit_started: Option<std::time::Instant>,
}

pub(super) enum LoadedPagePostInstallStart {
    Pending(Box<PendingLoadedPagePostInstall>),
    Ready(Option<LoadedPageInstallOutcome>),
}

pub(super) struct PendingLoadedPagePostInstall {
    continuation: LoadedPagePostInstallContinuation,
    pending: PendingBidiPreloadListenerBatch,
}

pub(super) struct CompletedLoadedPagePostInstall {
    continuation: LoadedPagePostInstallContinuation,
    completed: CompletedBidiPreloadListenerBatch,
}

struct LoadedPagePostInstallContinuation {
    outcome: LoadedPageInstallOutcome,
    final_url: Url,
    timing_started: Option<std::time::Instant>,
}

impl LoadedPageInstallStart {
    pub(super) fn committed_owner(&self) -> Option<&BrowserPageOwnerKey> {
        match self {
            Self::Pending(pending) => pending.committed_owner(),
            Self::Ready(completed) => completed.committed_owner(),
            Self::Failed => None,
        }
    }
}

impl PendingLoadedPageInstall {
    pub(super) fn committed_owner(&self) -> Option<&BrowserPageOwnerKey> {
        self.replacement.committed_owner()
    }

    pub(super) async fn wait(self) -> CompletedLoadedPageInstall {
        CompletedLoadedPageInstall::Replacement(Box::new(CompletedLoadedPageReplacement {
            continuation: self.continuation,
            outcome: self.replacement.wait().await,
        }))
    }
}

impl CompletedLoadedPageInstall {
    pub(super) fn committed_owner(&self) -> Option<&BrowserPageOwnerKey> {
        match self {
            Self::MissingCommitState => None,
            Self::Replacement(replacement) => replacement.outcome.committed_owner(),
        }
    }
}

/// Commits Browser Core and physical Page residence synchronously, then
/// returns the retired or rejected Page as a move-owned disposal participant.
pub(super) fn start_loaded_navigation_page_install(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    token: &DocumentNavigationToken,
    state: &NavigationDispatchState,
    final_url: &Url,
    restore: CompletedLoadedPageRestore,
    command_context: &mut CommandDispatchContext,
) -> LoadedPageInstallStart {
    let ReadyLoadedPageRestore {
        mut page,
        commit_state,
        renderer_attachment_commit,
        runtime_output_predecessor,
        preload_channel_realms,
        target_url,
        main_document_commit,
        timing_started,
    } = match restore {
        CompletedLoadedPageRestore::Configured(restore) => *restore,
        CompletedLoadedPageRestore::MissingCommitState => {
            return LoadedPageInstallStart::Ready(CompletedLoadedPageInstall::MissingCommitState);
        }
        CompletedLoadedPageRestore::Failed {
            failure,
            runtime_output_predecessor,
        } => {
            if let Some(predecessor) = runtime_output_predecessor {
                command_context.set_renderer_output_predecessor(predecessor);
            }
            report_loaded_page_restore_failure(out, state, failure);
            return LoadedPageInstallStart::Failed;
        }
    };
    if let Some(predecessor) = runtime_output_predecessor {
        command_context.set_renderer_output_predecessor(predecessor);
    }
    // `DocumentCommit` page creation hands Browser Host the committed Page
    // before renderer parser/DCL/load continuation is admitted. Preserve the
    // frontend response boundary while transferring the Page lifetime into
    // Browser Core: the continuation is protocol causality, not Browser Page
    // authority, and must therefore be detached before the Core replacement.
    if let Some(continuation) = page.take_committed_document_post_response_continuation() {
        command_context
            .response_flush()
            .defer_until_response_flush(move || continuation.release());
    }
    let page_commit_started = timing_started.is_some().then(std::time::Instant::now);
    let replacement = conn.start_loaded_page_replacement_for_session_owner(
        state.navigate_session_id.as_deref(),
        token,
        page,
        &target_url,
        &main_document_commit,
        renderer_attachment_commit,
    );
    let continuation = LoadedPageInstallContinuation {
        commit_state,
        preload_channel_realms,
        final_url: final_url.clone(),
        timing_started,
        page_commit_started,
    };
    match replacement {
        LoadedPageReplacementStart::Pending(replacement) => {
            LoadedPageInstallStart::Pending(Box::new(PendingLoadedPageInstall {
                continuation,
                replacement,
            }))
        }
        LoadedPageReplacementStart::Ready(outcome) => LoadedPageInstallStart::Ready(
            CompletedLoadedPageInstall::Replacement(Box::new(CompletedLoadedPageReplacement {
                continuation,
                outcome,
            })),
        ),
    }
}

/// Applies the synchronous post-disposal projection and starts the first BiDi
/// preload-listener participant, if the committed Page has channel handoffs.
pub(super) fn start_loaded_navigation_page_post_install(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    state: &NavigationDispatchState,
    install: CompletedLoadedPageInstall,
) -> LoadedPagePostInstallStart {
    let CompletedLoadedPageReplacement {
        continuation:
            LoadedPageInstallContinuation {
                commit_state,
                preload_channel_realms,
                final_url,
                timing_started,
                page_commit_started,
            },
        outcome,
    } = match install {
        CompletedLoadedPageInstall::MissingCommitState => {
            return LoadedPagePostInstallStart::Ready(Some(LoadedPageInstallOutcome::default()));
        }
        CompletedLoadedPageInstall::Replacement(replacement) => *replacement,
    };
    let replacement = match outcome {
        LoadedPageReplacementOutcome::Committed(replacement) => replacement,
        LoadedPageReplacementOutcome::Failed(error) => {
            if state.navigate_id.is_some() {
                out.push_error_after_messages(
                    -32000,
                    format!("failed to collect navigation Inspector output: {error}"),
                );
            } else {
                tracing::warn!(
                    %error,
                    session_id = state.navigate_session_id.as_deref(),
                    "navigation commit failed after early Page.navigate result: Inspector output collection"
                );
            }
            return LoadedPagePostInstallStart::Ready(None);
        }
        LoadedPageReplacementOutcome::Rejected => {
            return LoadedPagePostInstallStart::Ready(None);
        }
    };
    let outcome = LoadedPageInstallOutcome {
        owner: Some(replacement.owner().clone()),
    };
    let worker_retirement_events =
        crate::domains::target::retire_dedicated_worker_targets_for_replaced_page(
            conn,
            replacement.previous_page(),
        );
    out.extend_background_events_after_messages(worker_retirement_events);
    if commit_state.runtime_frontend_enabled {
        let _ = conn.set_renderer_runtime_agent_owns_page_console_api_events_for_session_owner(
            state.navigate_session_id.as_deref(),
            true,
        );
    }
    if let Some(started) = page_commit_started {
        tracing::info!(
            target: "moli_cdp_nav_timing",
            url = %final_url,
            stage = "restore_commit_page_installed",
            phase_ms = started.elapsed().as_millis(),
            elapsed_ms = timing_started
                .as_ref()
                .map(std::time::Instant::elapsed)
                .map(|elapsed| elapsed.as_millis())
                .unwrap_or_default(),
        );
    }
    let preload_channel_realms = if conn.target_owner_has_bidi_channel_preload_script_for_session(
        state.navigate_session_id.as_deref(),
    ) {
        preload_channel_realms
    } else {
        Vec::new()
    };
    let continuation = LoadedPagePostInstallContinuation {
        outcome,
        final_url,
        timing_started,
    };
    match start_bidi_preload_listener_batch(
        conn,
        state.navigate_session_id.as_deref(),
        preload_channel_realms,
    ) {
        BidiPreloadListenerBatchStep::Pending(pending) => {
            LoadedPagePostInstallStart::Pending(Box::new(PendingLoadedPagePostInstall {
                continuation,
                pending: *pending,
            }))
        }
        BidiPreloadListenerBatchStep::Complete(events) => {
            finish_loaded_navigation_page_post_install(out, continuation, events)
        }
    }
}

impl PendingLoadedPagePostInstall {
    pub(super) async fn wait(self) -> CompletedLoadedPagePostInstall {
        CompletedLoadedPagePostInstall {
            continuation: self.continuation,
            completed: self.pending.wait().await,
        }
    }
}

pub(super) fn complete_loaded_navigation_page_post_install(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    completed: CompletedLoadedPagePostInstall,
) -> LoadedPagePostInstallStart {
    let CompletedLoadedPagePostInstall {
        continuation,
        completed,
    } = completed;
    match complete_bidi_preload_listener_batch(conn, completed) {
        BidiPreloadListenerBatchStep::Pending(pending) => {
            LoadedPagePostInstallStart::Pending(Box::new(PendingLoadedPagePostInstall {
                continuation,
                pending: *pending,
            }))
        }
        BidiPreloadListenerBatchStep::Complete(events) => {
            finish_loaded_navigation_page_post_install(out, continuation, events)
        }
    }
}

fn finish_loaded_navigation_page_post_install(
    out: &mut CommandOutputBuffer,
    continuation: LoadedPagePostInstallContinuation,
    events: Vec<crate::conn::BackgroundProtocolEvent>,
) -> LoadedPagePostInstallStart {
    let LoadedPagePostInstallContinuation {
        outcome,
        final_url,
        timing_started,
    } = continuation;
    out.extend_background_events_after_messages(events);
    if let Some(started) = timing_started {
        tracing::info!(
            target: "moli_cdp_nav_timing",
            url = %final_url,
            stage = "restore_commit_done",
            elapsed_ms = started.elapsed().as_millis(),
        );
    }
    LoadedPagePostInstallStart::Ready(Some(outcome))
}
