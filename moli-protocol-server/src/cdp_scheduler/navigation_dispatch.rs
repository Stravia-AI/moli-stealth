use std::{
    future::{self, Future},
    time::Duration,
};

use moli_protocol::{
    CompletedDevToolsBrowserOwnerNavigationCommand, DevToolsBrowserOwnerNavigationCommandTaskStep,
    PendingDevToolsBrowserOwnerNavigationCommand,
    devtools_runtime::{DevToolsCommand, DevToolsError, DevToolsErrorKind},
};
use tokio::time::{Instant as TokioInstant, sleep_until};

use super::{
    CdpScheduler, CdpSchedulerEventReceivers, DevToolsCommandExecution, ProtocolOutputSequence,
};

pub(super) fn devtools_command_uses_browser_owner_dispatch(command: &DevToolsCommand) -> bool {
    matches!(
        command,
        DevToolsCommand::Navigate(_)
            | DevToolsCommand::Reload(_)
            | DevToolsCommand::TraverseHistory(_)
    )
}

impl CdpScheduler {
    /// Executes one direct frontend navigation while continuing to service the
    /// independently-owned Browser Host lane.
    ///
    /// A child-frame navigation is returned by Protocol as not Browser-owned
    /// and keeps its existing Page/renderer path. Once a top-level command is
    /// published, only Host turns and exact participant completions can advance
    /// it; this loop merely waits for the neutral result and projects outputs.
    pub(super) async fn execute_devtools_browser_owner_navigation_with_interleaved_progress(
        &mut self,
        receivers: &mut CdpSchedulerEventReceivers,
        command: DevToolsCommand,
        timeout: Option<Duration>,
        background_command_id: Option<u64>,
    ) -> DevToolsCommandExecution {
        let navigation_wait = super::devtools_navigation_wait(&command);
        let start = self
            .host_adapter
            .try_start_devtools_browser_owner_navigation_command(command, background_command_id)
            .await;
        match start {
            Err(command) => {
                return self
                    .execute_non_browser_owner_navigation_with_timeout(
                        receivers,
                        command,
                        timeout,
                        background_command_id,
                    )
                    .await;
            }
            Ok(DevToolsBrowserOwnerNavigationCommandTaskStep::Complete(outcome)) => {
                return self
                    .finish_devtools_command_dispatch_outcome_with_protocol_messages(
                        Some(receivers),
                        navigation_wait,
                        *outcome,
                        false,
                    )
                    .await;
            }
            Ok(DevToolsBrowserOwnerNavigationCommandTaskStep::Pending(pending)) => {
                self.wait_for_devtools_browser_owner_navigation(
                    receivers,
                    *pending,
                    navigation_wait,
                    timeout.map(|timeout| TokioInstant::now() + timeout),
                )
                .await
            }
        }
    }

    async fn execute_non_browser_owner_navigation_with_timeout(
        &mut self,
        receivers: &mut CdpSchedulerEventReceivers,
        command: DevToolsCommand,
        timeout: Option<Duration>,
        background_command_id: Option<u64>,
    ) -> DevToolsCommandExecution {
        let execution = self.execute_devtools_command_with_protocol_messages_inner(
            Some(receivers),
            command,
            false,
            background_command_id,
        );
        let Some(timeout) = timeout else {
            return execution.await;
        };
        match tokio::time::timeout(timeout, execution).await {
            Ok(execution) => execution,
            Err(_) => navigation_timeout_execution(ProtocolOutputSequence::empty()),
        }
    }

    async fn wait_for_devtools_browser_owner_navigation(
        &mut self,
        receivers: &mut CdpSchedulerEventReceivers,
        pending: PendingDevToolsBrowserOwnerNavigationCommand,
        navigation_wait: Option<moli_protocol::devtools_runtime::DevToolsNavigationWait>,
        deadline: Option<TokioInstant>,
    ) -> DevToolsCommandExecution {
        let mut protocol_output = ProtocolOutputSequence::empty();
        let mut completion = Box::pin(pending.wait());
        loop {
            // A terminal Browser Host apply sends the neutral frontend result
            // synchronously. Observe that exact result before selecting
            // post-command protocol work published by the same apply turn;
            // otherwise a deferred load action can overtake the response and
            // make its renderer publication look already observed.
            let ready_completion = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(match completion.as_mut().poll(cx) {
                    std::task::Poll::Ready(completed) => Some(completed),
                    std::task::Poll::Pending => None,
                })
            })
            .await;
            if let Some(completed) = ready_completion {
                return self
                    .finish_devtools_browser_owner_navigation_completion(
                        receivers,
                        completed,
                        navigation_wait,
                        protocol_output,
                    )
                    .await;
            }

            let ready_output = self
                .complete_ready_protocol_residences_after_command()
                .await;
            if !ready_output.is_empty() {
                protocol_output.append(ready_output);
                continue;
            }

            // This foreground command owns the same commit-before-renderer
            // ingress invariant as a background navigation. Do not dequeue
            // the new Page's renderer stream before the terminal Browser Host
            // projection installs its exact root-Document binding and exposes
            // the stream's release boundary.
            let navigation_gate_open = true;
            let mut timed_out = false;
            tokio::select! {
                biased;
                completed = &mut completion => {
                    return self
                        .finish_devtools_browser_owner_navigation_completion(
                            receivers,
                            completed,
                            navigation_wait,
                            protocol_output,
                        )
                        .await;
                }
                _ = wait_until_navigation_deadline(deadline) => {
                    timed_out = true;
                }
                maybe_input = receivers.recv_interleaved_input(navigation_gate_open) => {
                    let Some(input) = maybe_input else {
                        return DevToolsCommandExecution {
                            result: Err(DevToolsError::new(
                                DevToolsErrorKind::Internal,
                                "SchedulerInputClosed",
                            )),
                            protocol_output,
                        };
                    };
                    // Once a concrete input is dequeued, finish its projection
                    // before racing the frontend completion or timeout again.
                    match self.complete_interleaved_scheduler_input(receivers, input).await {
                        Ok(output) => protocol_output.append(output),
                        Err(failure) => {
                            let (output, error) = failure.into_parts();
                            protocol_output.append(output);
                            return DevToolsCommandExecution {
                                result: Err(error),
                                protocol_output,
                            };
                        }
                    }
                }
            }
            if timed_out {
                receivers
                    .browser_host
                    .detach_navigation_completion(completion);
                return navigation_timeout_execution(protocol_output);
            }
        }
    }

    async fn finish_devtools_browser_owner_navigation_completion(
        &mut self,
        receivers: &mut CdpSchedulerEventReceivers,
        completed: CompletedDevToolsBrowserOwnerNavigationCommand,
        navigation_wait: Option<moli_protocol::devtools_runtime::DevToolsNavigationWait>,
        mut protocol_output: ProtocolOutputSequence,
    ) -> DevToolsCommandExecution {
        let outcome = self
            .host_adapter
            .complete_devtools_browser_owner_navigation_command(completed)
            .await;
        let mut execution = self
            .finish_devtools_command_dispatch_outcome_with_protocol_messages(
                Some(receivers),
                navigation_wait,
                outcome,
                false,
            )
            .await;
        protocol_output.append(execution.protocol_output);
        execution.protocol_output = protocol_output;
        execution
    }
}

async fn wait_until_navigation_deadline(deadline: Option<TokioInstant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => future::pending().await,
    }
}

fn navigation_timeout_execution(
    protocol_output: ProtocolOutputSequence,
) -> DevToolsCommandExecution {
    DevToolsCommandExecution {
        result: Err(DevToolsError::new(
            DevToolsErrorKind::Timeout,
            "script timed out",
        )),
        protocol_output,
    }
}

#[cfg(test)]
mod tests {
    use moli_protocol::devtools_runtime::{
        DevToolsCommand, DevToolsCommandContext, DevToolsHistoryTraversalDestination,
        DevToolsNavigationWait, DevToolsProtocol, DevToolsTargetId, DevToolsTraverseHistoryCommand,
    };

    use super::devtools_command_uses_browser_owner_dispatch;

    #[test]
    fn history_delta_enters_browser_owner_dispatch() {
        let command = DevToolsCommand::TraverseHistory(DevToolsTraverseHistoryCommand {
            context: DevToolsCommandContext {
                protocol: DevToolsProtocol::WebDriverBidi,
                session_id: None,
                target_id: Some(DevToolsTargetId::from("target-history")),
                browser_context_id: None,
            },
            destination: DevToolsHistoryTraversalDestination::Delta(-1),
            wait: DevToolsNavigationWait::Load,
        });

        assert!(devtools_command_uses_browser_owner_dispatch(&command));
    }
}
