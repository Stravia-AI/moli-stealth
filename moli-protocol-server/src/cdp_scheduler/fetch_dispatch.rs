use std::{future, future::Future, time::Duration};

use moli_protocol::{
    CompletedDevToolsFetchCommand, DevToolsFetchCommandTaskStep, PendingDevToolsFetchCommand,
    devtools_runtime::{DevToolsCommand, DevToolsError, DevToolsErrorKind},
};
use tokio::time::{Instant as TokioInstant, sleep_until};

use super::{
    CdpScheduler, CdpSchedulerEventReceivers, DevToolsCommandExecution, ProtocolOutputSequence,
};

pub(super) fn devtools_command_uses_interleaved_fetch_dispatch(command: &DevToolsCommand) -> bool {
    matches!(
        command,
        DevToolsCommand::ContinueInterceptedRequest(_)
            | DevToolsCommand::ContinueInterceptedResponse(_)
            | DevToolsCommand::ContinueWithAuth(_)
            | DevToolsCommand::FailInterceptedRequest(_)
            | DevToolsCommand::FulfillInterceptedRequest(_)
    )
}

impl CdpScheduler {
    /// Executes one typed Fetch terminal decision while keeping Browser Host
    /// and independent protocol ingress live.
    ///
    /// Main-Document decisions enter the shared Browser Owner FIFO. A
    /// subresource decision keeps its exact Page participant. Neither variant
    /// borrows the physical connection while its participant is pending.
    pub(super) async fn execute_devtools_fetch_with_interleaved_progress(
        &mut self,
        receivers: &mut CdpSchedulerEventReceivers,
        command: DevToolsCommand,
        timeout: Option<Duration>,
    ) -> DevToolsCommandExecution {
        let start = self
            .host_adapter
            .commands()
            .try_start_devtools_fetch_command_task(command)
            .await;
        match start {
            Err(command) => {
                return self
                    .execute_devtools_command_with_protocol_messages_inner(
                        Some(receivers),
                        command,
                        false,
                        None,
                    )
                    .await;
            }
            Ok(DevToolsFetchCommandTaskStep::Complete(outcome)) => {
                return self
                    .finish_devtools_command_dispatch_outcome_with_protocol_messages(
                        Some(receivers),
                        None,
                        *outcome,
                        false,
                    )
                    .await;
            }
            Ok(DevToolsFetchCommandTaskStep::Pending(pending)) => {
                self.wait_for_devtools_fetch_command(
                    receivers,
                    *pending,
                    timeout.map(|timeout| TokioInstant::now() + timeout),
                )
                .await
            }
        }
    }

    async fn wait_for_devtools_fetch_command(
        &mut self,
        receivers: &mut CdpSchedulerEventReceivers,
        pending: PendingDevToolsFetchCommand,
        deadline: Option<TokioInstant>,
    ) -> DevToolsCommandExecution {
        let navigation_gate_open = pending.holds_navigation_renderer_publication_gate();
        let mut protocol_output = ProtocolOutputSequence::empty();
        let mut completion = Box::pin(pending.wait());
        loop {
            // The Host terminal turn can publish both the original navigation
            // effects and this frontend completion. Observe the exact command
            // completion before selecting unrelated post-command work.
            let ready_completion = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(match completion.as_mut().poll(cx) {
                    std::task::Poll::Ready(completed) => Some(completed),
                    std::task::Poll::Pending => None,
                })
            })
            .await;
            if let Some(completed) = ready_completion {
                return self
                    .finish_devtools_fetch_command_completion(receivers, completed, protocol_output)
                    .await;
            }

            let ready_output = self
                .complete_ready_protocol_residences_after_command()
                .await;
            if !ready_output.is_empty() {
                protocol_output.append(ready_output);
                continue;
            }

            let mut timed_out = false;
            tokio::select! {
                biased;
                completed = &mut completion => {
                    return self
                        .finish_devtools_fetch_command_completion(
                            receivers,
                            completed,
                            protocol_output,
                        )
                        .await;
                }
                _ = wait_until_fetch_deadline(deadline) => {
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
                // This abandons only the caller's typed reply wait; the Host
                // or Page participant was already accepted and is not
                // canceled through its completion receiver. A detached
                // main-Document Host projection is settled by the reply-send
                // fallback when this receiver has gone away.
                drop(completion);
                return DevToolsCommandExecution {
                    result: Err(DevToolsError::new(
                        DevToolsErrorKind::Timeout,
                        "script timed out",
                    )),
                    protocol_output,
                };
            }
        }
    }

    async fn finish_devtools_fetch_command_completion(
        &mut self,
        receivers: &mut CdpSchedulerEventReceivers,
        completed: CompletedDevToolsFetchCommand,
        mut protocol_output: ProtocolOutputSequence,
    ) -> DevToolsCommandExecution {
        let outcome = self
            .host_adapter
            .commands()
            .complete_devtools_fetch_command_task(completed)
            .await;
        let mut execution = self
            .finish_devtools_command_dispatch_outcome_with_protocol_messages(
                Some(receivers),
                None,
                outcome,
                false,
            )
            .await;
        protocol_output.append(execution.protocol_output);
        execution.protocol_output = protocol_output;
        execution
    }
}

async fn wait_until_fetch_deadline(deadline: Option<TokioInstant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use moli_protocol::devtools_runtime::{
        DevToolsCommandContext, DevToolsFailInterceptedRequestCommand, DevToolsProtocol,
        DevToolsRequestId,
    };

    use super::*;

    #[test]
    fn terminal_fetch_decision_uses_interleaved_dispatch() {
        let command =
            DevToolsCommand::FailInterceptedRequest(DevToolsFailInterceptedRequestCommand {
                context: DevToolsCommandContext {
                    protocol: DevToolsProtocol::WebDriverBidi,
                    session_id: None,
                    target_id: None,
                    browser_context_id: None,
                },
                request_id: DevToolsRequestId::from("request-owner"),
                error_text: "Aborted".to_owned(),
            });

        assert!(devtools_command_uses_interleaved_fetch_dispatch(&command));
    }
}
