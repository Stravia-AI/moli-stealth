use moli_core::page::{CompletedPageCommand, PendingPageCommand, RendererPageDiagnosticsSnapshot};
use std::time::Instant;

use super::{BrowserContext, CdpConnection, CdpSessionRoute, TargetRuntimeSlot};

pub(crate) struct PendingChildFrameLifecycleWork {
    session_id: Option<String>,
    pending: PendingPageCommand,
}

pub(crate) struct CompletedChildFrameLifecycleWork {
    session_id: Option<String>,
    completion: CompletedPageCommand,
}

impl PendingChildFrameLifecycleWork {
    pub(crate) async fn wait(self) -> Result<CompletedChildFrameLifecycleWork, String> {
        let completion = self
            .pending
            .wait()
            .await
            .map_err(|error| error.to_string())?;
        Ok(CompletedChildFrameLifecycleWork {
            session_id: self.session_id,
            completion,
        })
    }
}

fn browser_context_by_id_mut_from_parts<'a>(
    browser_context: &'a mut Option<BrowserContext>,
    inactive_browser_contexts: &'a mut [BrowserContext],
    browser_context_id: &str,
) -> Option<&'a mut BrowserContext> {
    if browser_context
        .as_ref()
        .is_some_and(|bc| bc.id == browser_context_id)
    {
        return browser_context.as_mut();
    }
    inactive_browser_contexts
        .iter_mut()
        .find(|bc| bc.id == browser_context_id)
}

impl CdpConnection {
    fn activity_source_runtime_slot_mut(
        &mut self,
        session_id: Option<&str>,
    ) -> Option<&mut TargetRuntimeSlot> {
        let route = match session_id {
            Some(_) => self.session_route(session_id)?,
            None => self
                .none_session_owner_route_override()
                .unwrap_or(CdpSessionRoute::Browser),
        };
        let browser_context = match route {
            CdpSessionRoute::Browser => self.browser_context.as_mut()?,
            CdpSessionRoute::ActiveTarget {
                browser_context_id, ..
            } => browser_context_by_id_mut_from_parts(
                &mut self.browser_context,
                &mut self.inactive_browser_contexts,
                &browser_context_id,
            )?,
            CdpSessionRoute::AuxiliaryTarget {
                browser_context_id,
                target_id,
            } => {
                let browser_context = browser_context_by_id_mut_from_parts(
                    &mut self.browser_context,
                    &mut self.inactive_browser_contexts,
                    &browser_context_id,
                )?;
                if let Some(index) = browser_context
                    .background_targets
                    .iter()
                    .position(|target| target.is_target(&target_id))
                {
                    return Some(&mut browser_context.background_targets[index].runtime_slot);
                }
                browser_context
            }
            CdpSessionRoute::BackgroundTarget {
                browser_context_id,
                target_id,
            } => {
                let browser_context = browser_context_by_id_mut_from_parts(
                    &mut self.browser_context,
                    &mut self.inactive_browser_contexts,
                    &browser_context_id,
                )?;
                let target = browser_context.background_target_mut(&target_id)?;
                return Some(&mut target.runtime_slot);
            }
            CdpSessionRoute::TabTarget { .. }
            | CdpSessionRoute::SharedWorkerTarget { .. }
            | CdpSessionRoute::DedicatedWorkerTarget { .. }
            | CdpSessionRoute::ServiceWorkerTarget { .. } => return None,
        };
        Some(&mut browser_context.active_target.runtime_slot)
    }

    pub(crate) fn start_child_frame_lifecycle_work_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        timeout: std::time::Duration,
    ) -> Result<PendingChildFrameLifecycleWork, String> {
        let storage = self
            .navigation_load_inputs_for_session_owner(session_id)
            .resource_storage_handles();
        let browser_host_state = self.browser_host_state.clone();
        let Some(slot) = self.activity_source_runtime_slot_mut(session_id) else {
            return Err("NoDocumentLoaded".to_owned());
        };
        let Some(page) = slot.loaded_page() else {
            return Err("NoDocumentLoaded".to_owned());
        };
        let pending = browser_host_state
            .navigation_owner_mut()
            .start_active_page_child_frame_lifecycle_work(
                storage.into_navigation_storage(),
                &page,
                timeout,
            )
            .map_err(|error| error.to_string())?;
        Ok(PendingChildFrameLifecycleWork {
            session_id: session_id.map(str::to_owned),
            pending,
        })
    }

    pub(crate) fn complete_child_frame_lifecycle_work_command_turn_for_session_owner(
        &mut self,
        pending: CompletedChildFrameLifecycleWork,
    ) -> Result<(bool, moli_core::page::RendererCommandTurnOutput), String> {
        let session_id = pending.session_id.as_deref();
        let browser_host_state = self.browser_host_state.clone();
        let Some(slot) = self.activity_source_runtime_slot_mut(session_id) else {
            return Err("NoDocumentLoaded".to_owned());
        };
        let Some(mut page) = slot.loaded_page_mut() else {
            return Err("NoDocumentLoaded".to_owned());
        };
        let completed = browser_host_state
            .navigation_owner_mut()
            .complete_active_page_child_frame_lifecycle_work(&mut page, pending.completion)
            .map_err(|error| error.to_string())?;
        let _ = slot.ingest_owner_page_observable_output_updates();
        Ok(completed)
    }

    #[cfg(test)]
    pub(crate) fn complete_child_frame_lifecycle_work_for_session_owner(
        &mut self,
        pending: CompletedChildFrameLifecycleWork,
    ) -> Result<bool, String> {
        self.complete_child_frame_lifecycle_work_command_turn_for_session_owner(pending)
            .map(|(completed, _output)| completed)
    }

    pub async fn page_diagnostics_snapshot_for_session_owner_async(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<RendererPageDiagnosticsSnapshot, String> {
        let trace_started = moli_trace::cdp_runtime_trace_enabled().then(Instant::now);
        trace_activity_source_stage(
            "conn_page_diagnostics_snapshot_start",
            session_id,
            trace_started,
        );
        let Some(slot) = self.activity_source_runtime_slot_mut(session_id) else {
            trace_activity_source_stage(
                "conn_page_diagnostics_snapshot_missing_owner",
                session_id,
                trace_started,
            );
            return Ok(RendererPageDiagnosticsSnapshot::default());
        };
        let Some(mut page) = slot.loaded_page_mut() else {
            trace_activity_source_stage(
                "conn_page_diagnostics_snapshot_missing_page",
                session_id,
                trace_started,
            );
            return Ok(RendererPageDiagnosticsSnapshot::default());
        };
        let renderer_started = moli_trace::cdp_runtime_trace_enabled().then(Instant::now);
        let snapshot = page
            .page_diagnostics_snapshot_async()
            .await
            .map_err(|error| error.to_string())?;
        trace_activity_source_stage(
            "conn_page_diagnostics_snapshot_renderer_done",
            session_id,
            renderer_started,
        );
        let ingest_started = moli_trace::cdp_runtime_trace_enabled().then(Instant::now);
        let ingested = slot.ingest_owner_page_observable_output_updates();
        trace_activity_source_stage_with_bool(
            "conn_page_diagnostics_snapshot_ingest_done",
            session_id,
            ingest_started,
            ingested,
        );
        trace_activity_source_stage(
            "conn_page_diagnostics_snapshot_done",
            session_id,
            trace_started,
        );
        Ok(snapshot)
    }
}

fn trace_activity_source_stage(
    stage: &'static str,
    session_id: Option<&str>,
    started: Option<Instant>,
) {
    if let Some(started) = started {
        tracing::info!(
            target: "moli_cdp_runtime",
            stage = stage,
            session_id = ?session_id,
            elapsed_us = %started.elapsed().as_micros(),
        );
    }
}

fn trace_activity_source_stage_with_bool(
    stage: &'static str,
    session_id: Option<&str>,
    started: Option<Instant>,
    value: bool,
) {
    if let Some(started) = started {
        tracing::info!(
            target: "moli_cdp_runtime",
            stage = stage,
            session_id = ?session_id,
            value,
            elapsed_us = %started.elapsed().as_micros(),
        );
    }
}
