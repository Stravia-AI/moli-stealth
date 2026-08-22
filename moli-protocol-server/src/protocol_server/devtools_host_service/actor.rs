use std::time::Instant;

use crate::cdp_scheduler::{
    BrowserHostExecutionWake, CdpScheduler, CdpSchedulerEventReceivers,
    DevToolsPageCommandExecution, ProtocolAdapterScheduler,
};

use super::*;
use crate::protocol_server::webdriver_bidi::{BidiSocketActor, BidiSocketActorInput};

struct AttachedBidiFrontend {
    actor: BidiSocketActor,
    session_registry: SharedBidiSessionRegistry,
    detached_tx: Option<oneshot::Sender<()>>,
}

impl AttachedBidiFrontend {
    async fn release(
        &mut self,
        scheduler: &mut CdpScheduler,
        receivers: &mut CdpSchedulerEventReceivers,
    ) {
        self.actor.release_event_sources(scheduler, receivers).await;
        self.actor
            .release_session(&mut self.session_registry.lock());
        if let Some(detached_tx) = self.detached_tx.take() {
            let _ = detached_tx.send(());
        }
    }
}

enum DevToolsHostRequestOutcome {
    Continue,
    AttachedBidi(Box<AttachedBidiFrontend>),
    DetachBidi,
    Shutdown,
}

async fn handle_devtools_host_request(
    scheduler: &mut CdpScheduler,
    receivers: &mut CdpSchedulerEventReceivers,
    request: DevToolsHostServiceRequest,
    mut attached_bidi: Option<&mut AttachedBidiFrontend>,
) -> DevToolsHostRequestOutcome {
    match request {
        DevToolsHostServiceRequest::Execute {
            command,
            timeout,
            pending_navigation_timeout,
            expected_page,
            response_tx,
        } => {
            let execution = execute_devtools_command_with_pending_navigation_wait(
                scheduler,
                receivers,
                *command,
                timeout,
                pending_navigation_timeout,
                expected_page.as_ref(),
            )
            .await;
            let DevToolsPageCommandExecution {
                execution,
                page_residence,
            } = execution;
            let result = execution.result;
            let keep_attached = if let Some(attached) = attached_bidi.as_mut() {
                attached
                    .actor
                    .send_or_route_protocol_output(
                        scheduler,
                        receivers,
                        execution.protocol_output,
                        None,
                    )
                    .await
            } else {
                true
            };
            let _ = response_tx.send(DevToolsHostCommandExecution {
                result,
                page_residence,
            });
            if keep_attached {
                DevToolsHostRequestOutcome::Continue
            } else {
                DevToolsHostRequestOutcome::DetachBidi
            }
        }
        DevToolsHostServiceRequest::WaitForDocumentLifecycle {
            context,
            milestone,
            timeout,
            response_tx,
        } => {
            let execution = scheduler
                .wait_for_devtools_context_document_lifecycle(
                    receivers, &context, milestone, timeout,
                )
                .await;
            let result = execution.result.map(|_| ());
            let keep_attached = if let Some(attached) = attached_bidi.as_mut() {
                attached
                    .actor
                    .send_or_route_protocol_output(
                        scheduler,
                        receivers,
                        execution.protocol_output,
                        None,
                    )
                    .await
            } else {
                true
            };
            let _ = response_tx.send(result);
            if keep_attached {
                DevToolsHostRequestOutcome::Continue
            } else {
                DevToolsHostRequestOutcome::DetachBidi
            }
        }
        DevToolsHostServiceRequest::SetJavaScriptDialogHandlerEnabled {
            enabled,
            response_tx,
        } => {
            let result = scheduler
                .set_automation_javascript_dialog_handler_enabled(enabled)
                .then_some(())
                .ok_or_else(|| {
                    DevToolsError::new(
                        DevToolsErrorKind::NoSuchTarget,
                        "DevTools Host has no browser context",
                    )
                });
            let _ = response_tx.send(result);
            DevToolsHostRequestOutcome::Continue
        }
        DevToolsHostServiceRequest::AttachBidi {
            socket,
            web_socket_url,
            session,
            session_registry,
            response_tx,
        } => {
            if attached_bidi.is_some() {
                let _ = response_tx.send(None);
                return DevToolsHostRequestOutcome::Continue;
            }
            let mut actor = BidiSocketActor::new(*socket, web_socket_url);
            match session {
                BidiFrontendSession::Standalone => {}
                BidiFrontendSession::Existing {
                    session_id,
                    file_prompt_handler,
                } => {
                    let attached = {
                        let mut registry = session_registry.lock();
                        actor.attach_existing_session(session_id, &mut registry)
                    };
                    if !attached {
                        let _ = response_tx.send(None);
                        return DevToolsHostRequestOutcome::Continue;
                    }
                    actor.set_file_prompt_handler_for_script_commands(
                        file_prompt_handler.as_deref(),
                    );
                }
            }
            actor.install_frontend_scheduler_hooks(scheduler);
            let (detached_tx, detached_rx) = oneshot::channel();
            let _ = response_tx.send(Some(AttachedBidiFrontendLifetime { detached_rx }));
            DevToolsHostRequestOutcome::AttachedBidi(Box::new(AttachedBidiFrontend {
                actor,
                session_registry,
                detached_tx: Some(detached_tx),
            }))
        }
        DevToolsHostServiceRequest::Shutdown { response_tx } => {
            let _ = response_tx.send(());
            DevToolsHostRequestOutcome::Shutdown
        }
    }
}

async fn execute_devtools_command_with_pending_navigation_wait(
    scheduler: &mut CdpScheduler,
    receivers: &mut CdpSchedulerEventReceivers,
    command: DevToolsCommand,
    timeout: Option<Duration>,
    pending_navigation_timeout: Option<Duration>,
    expected_page: Option<&DevToolsPageResidenceIdentity>,
) -> DevToolsPageCommandExecution {
    let mut execution = execute_devtools_command_once(
        scheduler,
        receivers,
        command.clone(),
        timeout,
        expected_page,
    )
    .await;

    let Some(pending_navigation_timeout) = pending_navigation_timeout else {
        return execution;
    };
    let started = Instant::now();
    loop {
        if !result_is_navigation_changing_document(&execution.execution.result) {
            return execution;
        }
        let Some(remaining) = pending_navigation_timeout.checked_sub(started.elapsed()) else {
            execution.execution.result = Err(pending_navigation_timeout_error());
            return execution;
        };
        let mut progress = scheduler
            .complete_ready_owner_and_protocol_residences_for_external_load_wait(
                &mut receivers.browser_host,
            )
            .await;
        if progress.is_empty() {
            let navigation_gate_open = scheduler.has_inflight_background_navigation();
            let input = match tokio::time::timeout(
                remaining,
                receivers.recv_interleaved_input(navigation_gate_open),
            )
            .await
            {
                Ok(Some(input)) => input,
                Ok(None) => {
                    execution.execution.result = Err(DevToolsError::new(
                        DevToolsErrorKind::NoSuchSession,
                        "DevTools Host stopped while waiting for navigation",
                    ));
                    return execution;
                }
                Err(_) => {
                    execution.execution.result = Err(pending_navigation_timeout_error());
                    return execution;
                }
            };
            progress = match scheduler
                .complete_interleaved_scheduler_input(receivers, input)
                .await
            {
                Ok(progress) => progress,
                Err(failure) => {
                    let (progress, error) = failure.into_parts();
                    execution.execution.protocol_output.append(progress);
                    execution.execution.result = Err(error);
                    return execution;
                }
            };
        }
        execution.execution.protocol_output.append(progress);

        let retry_timeout = match timeout {
            Some(timeout) => {
                let Some(remaining) = pending_navigation_timeout.checked_sub(started.elapsed())
                else {
                    execution.execution.result = Err(pending_navigation_timeout_error());
                    return execution;
                };
                Some(timeout.min(remaining))
            }
            None => None,
        };
        let retry = execute_devtools_command_once(
            scheduler,
            receivers,
            command.clone(),
            retry_timeout,
            expected_page,
        )
        .await;
        execution
            .execution
            .protocol_output
            .append(retry.execution.protocol_output);
        execution.execution.result = retry.execution.result;
        execution.page_residence = retry.page_residence;
    }
}

async fn execute_devtools_command_once(
    scheduler: &mut CdpScheduler,
    receivers: &mut CdpSchedulerEventReceivers,
    command: DevToolsCommand,
    timeout: Option<Duration>,
    expected_page: Option<&DevToolsPageResidenceIdentity>,
) -> DevToolsPageCommandExecution {
    scheduler
        .execute_devtools_command_with_external_load_wait_and_page_residence(
            receivers,
            command,
            timeout,
            expected_page,
        )
        .await
}

fn result_is_navigation_changing_document(
    result: &Result<DevToolsCommandResult, DevToolsError>,
) -> bool {
    matches!(
        result,
        Err(error) if error.kind == DevToolsErrorKind::NavigationChangingDocument
    )
}

fn pending_navigation_timeout_error() -> DevToolsError {
    DevToolsError::new(DevToolsErrorKind::Timeout, "navigation wait timed out")
}

pub(super) async fn run_devtools_host_service(
    mut rx: mpsc::UnboundedReceiver<DevToolsHostServiceRequest>,
    initial_storage_partition: CdpInitialStoragePartition,
    navigation_runtime_config: NavigationRuntimeConfig,
) {
    let (mut scheduler, mut receivers) = CdpScheduler::new_with_initial_state_runtime_config(
        initial_storage_partition,
        navigation_runtime_config,
    );
    let mut attached_bidi: Option<AttachedBidiFrontend> = None;
    let mut adapter_scheduler = ProtocolAdapterScheduler::<()>::default();
    loop {
        if adapter_scheduler.load_projection_precedes_browser_owner(&scheduler) {
            let Some(input) = adapter_scheduler
                .recv_load_projection_predecessor_input(&scheduler)
                .await
            else {
                break;
            };
            let keep_attached = if let Some(attached) = attached_bidi.as_mut() {
                attached
                    .actor
                    .handle_adapter_scheduler_input(
                        &mut adapter_scheduler,
                        &mut scheduler,
                        &mut receivers,
                        input,
                    )
                    .await
            } else {
                let _ = adapter_scheduler
                    .advance_input(&mut scheduler, input, || ())
                    .await;
                true
            };
            if !keep_attached && let Some(mut attached) = attached_bidi.take() {
                attached.release(&mut scheduler, &mut receivers).await;
            }
            continue;
        }
        if attached_bidi.is_some() {
            let mut detach_bidi = false;
            let mut shutdown_requested = false;
            {
                let attached = attached_bidi.as_mut().expect("attached BiDi frontend");
                let page_javascript_blocked = scheduler.has_pending_javascript_dialog();
                let navigation_gate_open = receivers
                    .renderer_navigation_gate_open(scheduler.has_inflight_background_navigation());
                adapter_scheduler.schedule_turn_if_needed(&scheduler, page_javascript_blocked);
                tokio::select! {
                    biased;
                    wake = receivers.browser_host.recv_wake() => {
                        let output = match wake {
                            BrowserHostExecutionWake::TurnSelected => {
                                scheduler.complete_next_browser_owner_input(
                                    &mut receivers.browser_host,
                                )
                            }
                            BrowserHostExecutionWake::ParticipantCompleted(completed) => {
                                scheduler
                                    .complete_browser_host_participant(
                                        &mut receivers.browser_host,
                                        *completed,
                                    )
                                    .await
                            }
                            BrowserHostExecutionWake::DetachedNavigationCompleted(completed) => {
                                scheduler
                                    .complete_detached_devtools_browser_owner_navigation(
                                        &mut receivers,
                                        *completed,
                                    )
                                    .await
                            }
                            BrowserHostExecutionWake::Closed => break,
                        };
                        if !attached.actor.send_or_route_protocol_output(
                            &mut scheduler,
                            &mut receivers,
                            output,
                            None,
                        ).await {
                            detach_bidi = true;
                        }
                    }
                    completion = receivers.background_navigation_completion_rx.recv() => {
                        let Some(completion) = completion else {
                            break;
                        };
                        if !attached.actor.handle_background_navigation_completion(
                            &mut scheduler,
                            &mut receivers,
                            completion,
                        ).await {
                            detach_bidi = true;
                        }
                    }
                    event = receivers.background_event_rx.recv() => {
                        let Some(event) = event else {
                            break;
                        };
                        let output = scheduler.route_background_event_around_inflight_navigation(event);
                        if !attached.actor.send_or_route_protocol_output(
                            &mut scheduler,
                            &mut receivers,
                            output,
                            None,
                        ).await {
                            detach_bidi = true;
                        }
                    }
                    publication = receivers.renderer_publication_rx.recv(), if !page_javascript_blocked && !navigation_gate_open => {
                        let Some(publication) = publication else {
                            break;
                        };
                        if !attached.actor.handle_renderer_publication(
                            &mut adapter_scheduler,
                            &mut scheduler,
                            &mut receivers,
                            publication,
                        ).await {
                            detach_bidi = true;
                        }
                    }
                    actor_input = attached.actor.recv_attached_input(
                        &mut adapter_scheduler,
                        page_javascript_blocked,
                    ) => {
                        match actor_input {
                            BidiSocketActorInput::Socket(Some(message)) => {
                                if !attached.actor.handle_socket_message(
                                    &mut scheduler,
                                    &mut receivers,
                                    &attached.session_registry,
                                    message,
                                ).await {
                                    detach_bidi = true;
                                }
                            }
                            BidiSocketActorInput::Socket(None) => {
                                detach_bidi = true;
                            }
                            BidiSocketActorInput::AdapterScheduler(input) => {
                                if !attached.actor.handle_adapter_scheduler_input(
                                    &mut adapter_scheduler,
                                    &mut scheduler,
                                    &mut receivers,
                                    input,
                                ).await {
                                    detach_bidi = true;
                                }
                            }
                            BidiSocketActorInput::RuntimeResponseReady(Some(response)) => {
                                if !attached.actor.handle_runtime_response_ready(
                                    &mut scheduler,
                                    &mut receivers,
                                    *response,
                                ).await {
                                    detach_bidi = true;
                                }
                            }
                            BidiSocketActorInput::RuntimeResponseReady(None) => {
                                detach_bidi = true;
                            }
                        }
                    }
                    request = rx.recv() => {
                        let Some(request) = request else {
                            break;
                        };
                        match handle_devtools_host_request(
                            &mut scheduler,
                            &mut receivers,
                            request,
                            Some(attached),
                        ).await {
                            DevToolsHostRequestOutcome::Continue => {}
                            DevToolsHostRequestOutcome::AttachedBidi(mut duplicate) => {
                                duplicate.release(&mut scheduler, &mut receivers).await;
                            }
                            DevToolsHostRequestOutcome::DetachBidi => {
                                detach_bidi = true;
                            }
                            DevToolsHostRequestOutcome::Shutdown => {
                                attached.release(&mut scheduler, &mut receivers).await;
                                shutdown_requested = true;
                            }
                        }
                    }
                }
            }
            if shutdown_requested {
                return;
            }
            if detach_bidi && let Some(mut attached) = attached_bidi.take() {
                attached.release(&mut scheduler, &mut receivers).await;
            }
        } else {
            let page_javascript_blocked = scheduler.has_pending_javascript_dialog();
            let navigation_gate_open = receivers
                .renderer_navigation_gate_open(scheduler.has_inflight_background_navigation());
            adapter_scheduler.schedule_turn_if_needed(&scheduler, page_javascript_blocked);
            tokio::select! {
                biased;
                wake = receivers.browser_host.recv_wake() => {
                    let _ = match wake {
                        BrowserHostExecutionWake::TurnSelected => {
                            scheduler.complete_next_browser_owner_input(
                                &mut receivers.browser_host,
                            )
                        }
                        BrowserHostExecutionWake::ParticipantCompleted(completed) => {
                            scheduler
                                .complete_browser_host_participant(
                                    &mut receivers.browser_host,
                                    *completed,
                                )
                                .await
                        }
                        BrowserHostExecutionWake::DetachedNavigationCompleted(completed) => {
                            scheduler
                                .complete_detached_devtools_browser_owner_navigation(
                                    &mut receivers,
                                    *completed,
                                )
                                .await
                        }
                        BrowserHostExecutionWake::Closed => break,
                    };
                }
                completion = receivers.background_navigation_completion_rx.recv() => {
                    let Some(completion) = completion else {
                        break;
                    };
                    if scheduler
                        .drain_background_navigation_completion_with_progress_barrier(
                            completion,
                            &mut receivers,
                        )
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                event = receivers.background_event_rx.recv() => {
                    let Some(event) = event else {
                        break;
                    };
                    let _ = scheduler.route_background_event_around_inflight_navigation(event);
                }
                publication = receivers.renderer_publication_rx.recv(), if !page_javascript_blocked && !navigation_gate_open => {
                    let Some(publication) = publication else {
                        break;
                    };
                    let _ = adapter_scheduler
                        .ingest_renderer_publication(&mut scheduler, publication)
                        .await;
                }
                input = adapter_scheduler.recv_input(), if !page_javascript_blocked => {
                    let _ = adapter_scheduler
                        .advance_input(&mut scheduler, input, || ())
                        .await;
                }
                request = rx.recv() => {
                    let Some(request) = request else {
                        break;
                    };
                    match handle_devtools_host_request(
                        &mut scheduler,
                        &mut receivers,
                        request,
                        None,
                    ).await {
                        DevToolsHostRequestOutcome::Continue => {}
                        DevToolsHostRequestOutcome::AttachedBidi(attached) => {
                            attached_bidi = Some(*attached);
                        }
                        DevToolsHostRequestOutcome::DetachBidi => {}
                        DevToolsHostRequestOutcome::Shutdown => return,
                    }
                    if attached_bidi.is_none() {
                        ingest_ready_renderer_publications(
                            &mut adapter_scheduler,
                            &mut scheduler,
                            &mut receivers,
                        ).await;
                    }
                }
            }
        }
    }
    if let Some(mut attached) = attached_bidi.take() {
        attached.release(&mut scheduler, &mut receivers).await;
    }
}

async fn ingest_ready_renderer_publications(
    adapter_scheduler: &mut ProtocolAdapterScheduler<()>,
    scheduler: &mut CdpScheduler,
    receivers: &mut CdpSchedulerEventReceivers,
) {
    while let Ok(publication) = receivers.renderer_publication_rx.try_recv() {
        let _ = adapter_scheduler
            .ingest_renderer_publication(scheduler, publication)
            .await;
    }
}
