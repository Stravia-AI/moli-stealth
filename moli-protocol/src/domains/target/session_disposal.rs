use crate::conn::{
    BackgroundProtocolEvent, CdpConnection, TargetBindingCleanupAction, TargetBindingCleanupPlan,
    TargetEventPlan, TargetSessionDetachCleanupPlan,
};

struct PreparedTargetSessionDisposal {
    binding: TargetBindingCleanupPlan,
    detachment: TargetSessionDetachCleanupPlan,
}

impl PreparedTargetSessionDisposal {
    fn prepare(
        conn: &CdpConnection,
        detachment: TargetSessionDetachCleanupPlan,
    ) -> anyhow::Result<Self> {
        let session_id = detachment.session_id();
        let route = conn
            .session_route(Some(session_id))
            .ok_or_else(|| anyhow::anyhow!("InvalidSessionId"))?;
        let binding = TargetBindingCleanupPlan::from_route(session_id, &route)
            .ok_or_else(|| anyhow::anyhow!("InvalidSessionId"))?;
        let action_target_id = match binding.action() {
            TargetBindingCleanupAction::PageTarget { target_id, .. }
            | TargetBindingCleanupAction::SharedWorkerTarget { target_id }
            | TargetBindingCleanupAction::DedicatedWorkerTarget { target_id }
            | TargetBindingCleanupAction::ServiceWorkerTarget { target_id } => target_id,
            TargetBindingCleanupAction::TabTarget { tab_target_id } => tab_target_id,
        };
        if action_target_id != detachment.target_id() {
            anyhow::bail!("UnknownTargetId");
        }
        Ok(Self {
            binding,
            detachment,
        })
    }
}

pub(super) struct TargetSessionDisposalOutcome {
    event_plan: TargetEventPlan,
    renderer_output_predecessor: Option<moli_core::RendererOutputFence>,
}

impl TargetSessionDisposalOutcome {
    pub(super) fn into_parts(self) -> (TargetEventPlan, Option<moli_core::RendererOutputFence>) {
        (self.event_plan, self.renderer_output_predecessor)
    }
}

pub(super) async fn dispose_target_session_async(
    conn: &mut CdpConnection,
    background_events: &mut Vec<BackgroundProtocolEvent>,
    protocol_events: &mut Vec<BackgroundProtocolEvent>,
    cleanup_plan: TargetSessionDetachCleanupPlan,
) -> anyhow::Result<TargetSessionDisposalOutcome> {
    let prepared = PreparedTargetSessionDisposal::prepare(conn, cleanup_plan)?;
    let session_id = prepared.binding.session_id().to_owned();

    // Tracing may own isolate tasks that must finish before another session can
    // start a trace. Wait for that handler before retiring the session route.
    conn.cancel_tracing_for_session_owner_async(Some(&session_id))
        .await;

    let mut renderer_output_predecessor = None;
    match prepared.binding.action() {
        TargetBindingCleanupAction::PageTarget { .. } => {
            renderer_output_predecessor = dispose_page_session_runtime_state_async(
                conn,
                background_events,
                protocol_events,
                &session_id,
            )
            .await?;
        }
        TargetBindingCleanupAction::SharedWorkerTarget { .. }
        | TargetBindingCleanupAction::DedicatedWorkerTarget { .. }
        | TargetBindingCleanupAction::ServiceWorkerTarget { .. } => {
            dispose_worker_session_runtime_state_async(
                conn,
                background_events,
                protocol_events,
                &prepared.binding,
            )
            .await?;
        }
        TargetBindingCleanupAction::TabTarget { .. } => {}
    }

    conn.dispose_target_binding_async(&prepared.binding).await?;
    let event_plan = conn.commit_target_session_detachment_event_plan(prepared.detachment);
    Ok(TargetSessionDisposalOutcome {
        event_plan,
        renderer_output_predecessor,
    })
}

pub(super) async fn dispose_primary_page_session_preserving_frontend_async(
    conn: &mut CdpConnection,
    background_events: &mut Vec<BackgroundProtocolEvent>,
    protocol_events: &mut Vec<BackgroundProtocolEvent>,
    session_id: &str,
) -> anyhow::Result<Option<moli_core::RendererOutputFence>> {
    let route = conn
        .session_route(Some(session_id))
        .ok_or_else(|| anyhow::anyhow!("InvalidSessionId"))?;
    let binding = TargetBindingCleanupPlan::from_route(session_id, &route)
        .ok_or_else(|| anyhow::anyhow!("InvalidSessionId"))?;
    if !matches!(
        binding.action(),
        TargetBindingCleanupAction::PageTarget {
            session_key: moli_page_types::DevToolsSessionKey::Primary,
            ..
        }
    ) {
        anyhow::bail!("InvalidSessionId");
    }
    conn.cancel_tracing_for_session_owner_async(Some(session_id))
        .await;
    let predecessor = dispose_page_session_runtime_state_async(
        conn,
        background_events,
        protocol_events,
        session_id,
    )
    .await?;
    if !conn.release_primary_target_session_binding_without_event(session_id) {
        anyhow::bail!("InvalidSessionId");
    }
    Ok(predecessor)
}

pub(super) async fn dispose_dedicated_worker_session_after_prepared_state_delta_async(
    conn: &mut CdpConnection,
    background_events: &mut Vec<BackgroundProtocolEvent>,
    protocol_events: &mut Vec<BackgroundProtocolEvent>,
    cleanup_plan: TargetSessionDetachCleanupPlan,
) -> anyhow::Result<TargetSessionDisposalOutcome> {
    let prepared = PreparedTargetSessionDisposal::prepare(conn, cleanup_plan)?;
    if !matches!(
        prepared.binding.action(),
        TargetBindingCleanupAction::DedicatedWorkerTarget { .. }
    ) {
        anyhow::bail!("InvalidSessionId");
    }
    let session_id = prepared.binding.session_id().to_owned();
    conn.cancel_tracing_for_session_owner_async(Some(&session_id))
        .await;
    dispose_worker_session_runtime_state_async(
        conn,
        background_events,
        protocol_events,
        &prepared.binding,
    )
    .await?;
    conn.dispose_target_binding_async(&prepared.binding).await?;
    let event_plan = conn.commit_target_session_detachment_after_prepared_state_delta_event_plan(
        prepared.detachment,
    );
    Ok(TargetSessionDisposalOutcome {
        event_plan,
        renderer_output_predecessor: None,
    })
}

async fn dispose_worker_session_runtime_state_async(
    conn: &mut CdpConnection,
    background_events: &mut Vec<BackgroundProtocolEvent>,
    protocol_events: &mut Vec<BackgroundProtocolEvent>,
    binding: &TargetBindingCleanupPlan,
) -> anyhow::Result<()> {
    let session_id = binding.session_id();
    conn.release_worker_runtime_remote_objects_for_session_best_effort_async(session_id)
        .await;
    match binding.action() {
        TargetBindingCleanupAction::SharedWorkerTarget { target_id } => {
            let renderer_detach = conn
                .browser_context_by_id(binding.browser_context_id())
                .and_then(|browser_context| {
                    browser_context
                        .shared_worker_target(target_id)
                        .map(|target| {
                            (
                                browser_context.renderer_runtime(),
                                target.renderer_instance_id,
                            )
                        })
                });
            conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
                background_events,
                protocol_events,
                Some(session_id),
                "Target detached",
            );
            if let Some((renderer_runtime, instance_id)) = renderer_detach {
                renderer_runtime.detach_shared_worker_runtime_inspector_session(
                    instance_id,
                    Some(session_id.to_owned()),
                );
            }
        }
        TargetBindingCleanupAction::DedicatedWorkerTarget { target_id } => {
            let renderer_detach = conn
                .browser_context_by_id(binding.browser_context_id())
                .and_then(|browser_context| {
                    browser_context
                        .dedicated_worker_target(target_id)
                        .map(|target| {
                            (
                                browser_context.renderer_runtime(),
                                target.renderer_instance_id,
                            )
                        })
                });
            conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
                background_events,
                protocol_events,
                Some(session_id),
                "Target detached",
            );
            if let Some((renderer_runtime, instance_id)) = renderer_detach {
                renderer_runtime.detach_dedicated_worker_runtime_inspector_session(
                    instance_id,
                    Some(session_id.to_owned()),
                );
            }
        }
        TargetBindingCleanupAction::ServiceWorkerTarget { target_id } => {
            let renderer_detach = conn
                .browser_context_by_id(binding.browser_context_id())
                .and_then(|browser_context| {
                    browser_context
                        .service_worker_target(target_id)
                        .map(|target| {
                            (
                                browser_context.renderer_runtime(),
                                target.renderer_version_id,
                            )
                        })
                });
            conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
                background_events,
                protocol_events,
                Some(session_id),
                "Target detached",
            );
            super::set_service_worker_pause_on_start_owner(conn, Some(session_id), false);
            if let Some((renderer_runtime, version_id)) = renderer_detach {
                renderer_runtime.detach_service_worker_runtime_inspector_session(
                    version_id,
                    Some(session_id.to_owned()),
                );
            }
        }
        TargetBindingCleanupAction::PageTarget { .. }
        | TargetBindingCleanupAction::TabTarget { .. } => anyhow::bail!("InvalidSessionId"),
    }
    Ok(())
}

/// Completes disposal after a worker target has transferred and dropped its
/// per-session state. This path is used only by renderer-owned worker
/// destruction, where the target lifetime itself has already released
/// Inspector objects and pending state before protocol projection runs.
pub(super) async fn dispose_removed_worker_session_async(
    conn: &mut CdpConnection,
    cleanup_plan: TargetSessionDetachCleanupPlan,
) -> anyhow::Result<TargetEventPlan> {
    let prepared = PreparedTargetSessionDisposal::prepare(conn, cleanup_plan)?;
    if !matches!(
        prepared.binding.action(),
        TargetBindingCleanupAction::SharedWorkerTarget { .. }
            | TargetBindingCleanupAction::DedicatedWorkerTarget { .. }
            | TargetBindingCleanupAction::ServiceWorkerTarget { .. }
    ) {
        anyhow::bail!("InvalidSessionId");
    }
    conn.cancel_tracing_for_session_owner_async(Some(prepared.binding.session_id()))
        .await;
    Ok(conn.commit_target_session_detachment_event_plan(prepared.detachment))
}

/// Emergency completion for a DedicatedWorker retirement whose renderer
/// output failed after the target state was already removed. The renderer is
/// unavailable, so only protocol ownership remains to be disposed.
pub(super) fn dispose_removed_dedicated_worker_session_after_failed_retirement(
    conn: &mut CdpConnection,
    cleanup_plan: TargetSessionDetachCleanupPlan,
) -> TargetEventPlan {
    conn.commit_target_session_detachment_after_prepared_state_delta_event_plan(cleanup_plan)
}

async fn dispose_page_session_runtime_state_async(
    conn: &mut CdpConnection,
    background_events: &mut Vec<BackgroundProtocolEvent>,
    protocol_events: &mut Vec<BackgroundProtocolEvent>,
    session_id: &str,
) -> anyhow::Result<Option<moli_core::RendererOutputFence>> {
    let renderer_output_predecessor =
        super::clear_detached_target_fetch_state_background_events_async(
            conn,
            background_events,
            session_id,
        )
        .await;
    conn.fail_pending_inspector_awaits_for_session_owner_background_events_into(
        background_events,
        protocol_events,
        Some(session_id),
        "Target detached",
    );

    // Document-start scripts are renderer-owned resources. Remove them before
    // detaching the Inspector session that carries the cleanup commands.
    conn.remove_document_start_scripts_for_detached_session_async(session_id)
        .await?;
    clear_page_session_target_state_async(conn, session_id).await?;
    if let Err(error) = conn
        .detach_runtime_inspector_session_for_session_owner_async(Some(session_id))
        .await
    {
        tracing::debug!(
            session_id,
            %error,
            "renderer Inspector session was already unavailable during disposal"
        );
    }
    Ok(renderer_output_predecessor)
}

async fn clear_page_session_target_state_async(
    conn: &mut CdpConnection,
    session_id: &str,
) -> anyhow::Result<()> {
    crate::domains::emulation::clear_emulated_media_for_detached_session_async(conn, session_id)
        .await?;
    conn.clear_target_session_overrides_async(session_id)
        .await?;
    Ok(())
}
