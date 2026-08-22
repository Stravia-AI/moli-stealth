use crate::conn::{
    CdpConnection, CompletedRendererCallReplayBatch, DocumentNavigationToken,
    NavigationDispatchState, PendingRendererCallReplayBatch,
};
use crate::domains::command_output::CommandOutputBuffer;

/// One exact renderer participant still required before a Document navigation
/// can leave the Browser navigation gate.
pub(super) struct PendingNavigationTail {
    navigation_session_id: Option<String>,
    loader_id: String,
    replay: PendingRendererCallReplayBatch,
}

/// Move-owned completion of one navigation-tail renderer participant.
pub(super) struct CompletedNavigationTail {
    navigation_session_id: Option<String>,
    loader_id: String,
    replay: CompletedRendererCallReplayBatch,
}

pub(super) enum NavigationTailStep {
    Pending(Box<PendingNavigationTail>),
    Complete,
}

impl PendingNavigationTail {
    pub(super) async fn wait(self) -> CompletedNavigationTail {
        CompletedNavigationTail {
            navigation_session_id: self.navigation_session_id,
            loader_id: self.loader_id,
            replay: self.replay.wait().await,
        }
    }
}

/// Finishes the renderer's old-Document bookkeeping and starts the first
/// replayable Inspector command on the replacement Document.
///
/// Everything before the returned participant is synchronous. In particular,
/// the participant owns its Page command future and does not borrow
/// `CdpConnection` while the replacement renderer runs it.
pub(super) fn start_materialized_navigation_tail(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    token: &DocumentNavigationToken,
    state: &NavigationDispatchState,
) -> NavigationTailStep {
    let navigation_session_id = state.navigate_session_id.clone();
    let primary_protocol_session_id = conn
        .runtime_session_owner_primary_session_id(navigation_session_id.as_deref())
        .or_else(|| navigation_session_id.clone());
    let (routed_renderer_output, renderer_call_replacements) = conn
        .finish_renderer_document_navigation_for_session_owner(
            navigation_session_id.as_deref(),
            token,
        )
        .map(|finish| (finish.released_output, finish.renderer_call_replacements))
        .unwrap_or_default();
    if !routed_renderer_output.is_empty() {
        let mut background_events = Vec::new();
        crate::domains::runtime::push_routed_renderer_runtime_inspector_message_batch_background_events(
            conn,
            &mut background_events,
            routed_renderer_output,
            primary_protocol_session_id.as_deref(),
        );
        out.extend_background_events_after_messages(background_events);
    }
    if let Some(renderer_call_replacements) = renderer_call_replacements {
        let (new_attachment_id, terminations, replays) = renderer_call_replacements.into_parts();
        conn.terminate_prepared_renderer_calls_after_navigation(
            terminations,
            "Inspected target navigated or closed",
        );
        let (events, replay) =
            conn.start_prepared_renderer_calls_after_navigation(replays, new_attachment_id);
        out.extend_background_events_after_messages(events);
        if let Some(replay) = replay {
            return NavigationTailStep::Pending(Box::new(PendingNavigationTail {
                navigation_session_id,
                loader_id: state.loader_id.clone(),
                replay,
            }));
        }
    }
    clear_completed_navigation_tail(conn, navigation_session_id.as_deref(), &state.loader_id);
    NavigationTailStep::Complete
}

/// Applies one replay completion and either starts the next exact replay or
/// closes the navigation gate after the batch is terminal.
pub(super) async fn complete_materialized_navigation_tail_async(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    completed: CompletedNavigationTail,
) -> NavigationTailStep {
    let CompletedNavigationTail {
        navigation_session_id,
        loader_id,
        replay,
    } = completed;
    let (events, next) = conn
        .complete_prepared_renderer_call_after_navigation(replay)
        .await;
    out.extend_background_events_after_messages(events);
    if let Some(replay) = next {
        NavigationTailStep::Pending(Box::new(PendingNavigationTail {
            navigation_session_id,
            loader_id,
            replay,
        }))
    } else {
        clear_completed_navigation_tail(conn, navigation_session_id.as_deref(), &loader_id);
        NavigationTailStep::Complete
    }
}

/// Compatibility drain for callers that do not yet expose a Browser Host
/// participant boundary. Response-commit-ready direct and background
/// navigation completion uses `start_materialized_navigation_tail` and resumes
/// one participant at a time instead of entering this loop.
pub(super) async fn finish_materialized_navigation_tail_async(
    conn: &mut CdpConnection,
    out: &mut CommandOutputBuffer,
    token: &DocumentNavigationToken,
    state: &NavigationDispatchState,
) {
    let mut step = start_materialized_navigation_tail(conn, out, token, state);
    loop {
        match step {
            NavigationTailStep::Complete => return,
            NavigationTailStep::Pending(pending) => {
                let completed = pending.wait().await;
                step = complete_materialized_navigation_tail_async(conn, out, completed).await;
            }
        }
    }
}

fn clear_completed_navigation_tail(
    conn: &mut CdpConnection,
    navigation_session_id: Option<&str>,
    loader_id: &str,
) {
    conn.clear_document_navigation_protocol_tail_for_session_owner_if_loader_matches(
        navigation_session_id,
        loader_id,
    );
}
