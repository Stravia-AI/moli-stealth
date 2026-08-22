use std::{future, future::Future, time::Duration};

use moli_protocol::{
    CompletedDevToolsBrowserOwnerContextDisposalCommand,
    DevToolsBrowserOwnerContextDisposalCommandTaskStep,
    PendingDevToolsBrowserOwnerContextDisposalCommand,
    devtools_runtime::{DevToolsCommand, DevToolsError, DevToolsErrorKind},
};
use tokio::time::{Instant as TokioInstant, sleep_until};

use super::{
    CdpScheduler, CdpSchedulerEventReceivers, DevToolsCommandExecution, ProtocolOutputSequence,
};

pub(super) fn devtools_command_uses_browser_owner_context_disposal(
    command: &DevToolsCommand,
) -> bool {
    matches!(command, DevToolsCommand::RemoveBrowserContext(_))
}

impl CdpScheduler {
    /// Waits for a typed Context-disposal result while continuing to select
    /// Browser Host turns and independent protocol ingress.
    pub(super) async fn execute_devtools_browser_owner_context_disposal_with_interleaved_progress(
        &mut self,
        receivers: &mut CdpSchedulerEventReceivers,
        command: DevToolsCommand,
        timeout: Option<Duration>,
    ) -> DevToolsCommandExecution {
        let start = self
            .host_adapter
            .try_start_devtools_browser_owner_context_disposal_command(command)
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
            Ok(DevToolsBrowserOwnerContextDisposalCommandTaskStep::Complete(outcome)) => {
                return self
                    .finish_devtools_command_dispatch_outcome_with_protocol_messages(
                        Some(receivers),
                        None,
                        *outcome,
                        false,
                    )
                    .await;
            }
            Ok(DevToolsBrowserOwnerContextDisposalCommandTaskStep::Pending(pending)) => {
                self.wait_for_devtools_browser_owner_context_disposal(
                    receivers,
                    *pending,
                    timeout.map(|timeout| TokioInstant::now() + timeout),
                )
                .await
            }
        }
    }

    async fn wait_for_devtools_browser_owner_context_disposal(
        &mut self,
        receivers: &mut CdpSchedulerEventReceivers,
        pending: PendingDevToolsBrowserOwnerContextDisposalCommand,
        deadline: Option<TokioInstant>,
    ) -> DevToolsCommandExecution {
        let mut protocol_output = ProtocolOutputSequence::empty();
        let mut completion = Box::pin(pending.wait());
        loop {
            let ready_completion = std::future::poll_fn(|cx| {
                std::task::Poll::Ready(match completion.as_mut().poll(cx) {
                    std::task::Poll::Ready(completed) => Some(completed),
                    std::task::Poll::Pending => None,
                })
            })
            .await;
            if let Some(completed) = ready_completion {
                return self
                    .finish_devtools_browser_owner_context_disposal_completion(
                        receivers,
                        completed,
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

            let mut timed_out = false;
            tokio::select! {
                biased;
                completed = &mut completion => {
                    return self
                        .finish_devtools_browser_owner_context_disposal_completion(
                            receivers,
                            completed,
                            protocol_output,
                        )
                        .await;
                }
                _ = wait_until_context_disposal_deadline(deadline) => {
                    timed_out = true;
                }
                maybe_input = receivers.recv_interleaved_input(false) => {
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
                // Dropping only the frontend receiver is intentional. The
                // accepted Host task retains its reservation and continues;
                // its terminal projection is settled as detached output.
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

    async fn finish_devtools_browser_owner_context_disposal_completion(
        &mut self,
        receivers: &mut CdpSchedulerEventReceivers,
        completed: CompletedDevToolsBrowserOwnerContextDisposalCommand,
        mut protocol_output: ProtocolOutputSequence,
    ) -> DevToolsCommandExecution {
        let outcome = self
            .host_adapter
            .complete_devtools_browser_owner_context_disposal_command(completed)
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

async fn wait_until_context_disposal_deadline(deadline: Option<TokioInstant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use moli_protocol::devtools_runtime::{
        DevToolsBrowserContextId, DevToolsCommandContext, DevToolsProtocol,
        DevToolsRemoveBrowserContextCommand,
    };

    use super::*;

    #[test]
    fn remove_browser_context_uses_interleaved_owner_dispatch() {
        let command = DevToolsCommand::RemoveBrowserContext(DevToolsRemoveBrowserContextCommand {
            context: DevToolsCommandContext {
                protocol: DevToolsProtocol::WebDriverBidi,
                session_id: None,
                target_id: None,
                browser_context_id: None,
            },
            browser_context_id: DevToolsBrowserContextId::from("BID-owner"),
        });

        assert!(devtools_command_uses_browser_owner_context_disposal(
            &command
        ));
    }
}
