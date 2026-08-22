use crate::{
    conn::{
        CdpConnection, CommandDispatchContext, CompletedBrowserOwnerContextDisposalCommand,
        PendingBrowserOwnerContextDisposalCommand,
    },
    devtools_runtime::{
        DevToolsCommand, DevToolsCommandContext, DevToolsCommandResult, DevToolsError,
        DevToolsErrorKind, DevToolsProtocol,
    },
};

/// Direct BiDi/Classic frontend start result for one Browser-owned Context
/// disposal. Once pending is returned, no direct Protocol execution fallback
/// exists; the application loop must keep servicing Browser Host turns.
pub enum DevToolsBrowserOwnerContextDisposalCommandTaskStep {
    Pending(Box<PendingDevToolsBrowserOwnerContextDisposalCommand>),
    Complete(Box<crate::conn::DevToolsCommandDispatchOutcome>),
}

#[must_use = "a Browser Owner Context-disposal result must be awaited or explicitly abandoned"]
pub struct PendingDevToolsBrowserOwnerContextDisposalCommand {
    pending: PendingBrowserOwnerContextDisposalCommand,
    devtools_context: DevToolsCommandContext,
}

impl PendingDevToolsBrowserOwnerContextDisposalCommand {
    pub async fn wait(self) -> CompletedDevToolsBrowserOwnerContextDisposalCommand {
        CompletedDevToolsBrowserOwnerContextDisposalCommand {
            completed: self.pending.wait().await,
            devtools_context: self.devtools_context,
        }
    }
}

#[must_use = "a completed Browser Owner Context disposal must be projected"]
pub struct CompletedDevToolsBrowserOwnerContextDisposalCommand {
    completed: Result<CompletedBrowserOwnerContextDisposalCommand, String>,
    devtools_context: DevToolsCommandContext,
}

impl CdpConnection {
    /// Admits a typed `RemoveBrowserContext` command to the shared Browser
    /// Owner FIFO using the exact Context capability current at publication.
    pub async fn try_start_devtools_browser_owner_context_disposal_command(
        &mut self,
        command: DevToolsCommand,
    ) -> Result<DevToolsBrowserOwnerContextDisposalCommandTaskStep, DevToolsCommand> {
        let DevToolsCommand::RemoveBrowserContext(command) = command else {
            return Err(command);
        };
        let devtools_context = command.context;
        let browser_context_id = command.browser_context_id.into_string();
        if browser_context_id == self.default_browser_context_id() {
            return Ok(self
                .complete_direct_context_disposal_start_error(
                    devtools_context,
                    DevToolsError::new(
                        DevToolsErrorKind::InvalidArgument,
                        "DefaultBrowserContextCannotBeRemoved",
                    ),
                )
                .await);
        }
        let Some(browser_context_handle) = self
            .browser_context_by_id(&browser_context_id)
            .map(|context| context.browser_context_handle().clone())
        else {
            return Ok(self
                .complete_direct_context_disposal_start_error(
                    devtools_context,
                    DevToolsError::new(DevToolsErrorKind::NoSuchTarget, "UnknownBrowserContextId"),
                )
                .await);
        };
        let prefix_events = if devtools_context.protocol == DevToolsProtocol::WebDriverBidi {
            super::browser_context::target_destroyed_automation_events_for_browser_context(
                self,
                &browser_context_id,
            )
        } else {
            Vec::new()
        };
        match self.publish_browser_owner_context_disposal_command(
            browser_context_handle,
            prefix_events,
            CommandDispatchContext::default(),
        ) {
            Ok(pending) => Ok(DevToolsBrowserOwnerContextDisposalCommandTaskStep::Pending(
                Box::new(PendingDevToolsBrowserOwnerContextDisposalCommand {
                    pending,
                    devtools_context,
                }),
            )),
            Err(error) => Ok(self
                .complete_direct_context_disposal_start_error(
                    devtools_context,
                    DevToolsError::new(DevToolsErrorKind::Internal, error.to_string()),
                )
                .await),
        }
    }

    async fn complete_direct_context_disposal_start_error(
        &mut self,
        devtools_context: DevToolsCommandContext,
        error: DevToolsError,
    ) -> DevToolsBrowserOwnerContextDisposalCommandTaskStep {
        DevToolsBrowserOwnerContextDisposalCommandTaskStep::Complete(Box::new(
            self.finish_devtools_command_dispatch(devtools_context, Err(error), Vec::new(), None)
                .await,
        ))
    }

    /// Projects the terminal neutral owner result into the typed frontend.
    /// This method never advances Browser work; it only consumes the sidecar
    /// response and its ordered protocol effects.
    pub async fn complete_devtools_browser_owner_context_disposal_command(
        &mut self,
        completed: CompletedDevToolsBrowserOwnerContextDisposalCommand,
    ) -> crate::conn::DevToolsCommandDispatchOutcome {
        let CompletedDevToolsBrowserOwnerContextDisposalCommand {
            completed,
            devtools_context,
        } = completed;
        let (result, command_context) = match completed {
            Ok(completed) => {
                let (plan, mut command_context) = completed.into_parts();
                let (status, events) = plan.into_command_status_and_background_events();
                command_context.protocol_events_mut().extend(events);
                (
                    status
                        .unwrap_or(Ok(()))
                        .map(|()| DevToolsCommandResult::Empty),
                    command_context,
                )
            }
            Err(message) => (
                Err(DevToolsError::new(DevToolsErrorKind::Internal, message)),
                CommandDispatchContext::default(),
            ),
        };
        self.finish_devtools_command_dispatch_with_projection_context(
            devtools_context,
            result,
            command_context,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use moli_core::browser_host::BrowserHostActor;

    use crate::{
        conn::BrowserContext,
        devtools_runtime::{
            DevToolsBrowserContextId, DevToolsRemoveBrowserContextCommand, DevToolsSessionId,
        },
    };

    use super::*;

    #[tokio::test]
    async fn typed_context_disposal_waits_for_host_selection_and_projects_bidi_prefix() {
        let mut conn = CdpConnection::new();
        conn.install_default_browser_target();
        let mut browser_context = BrowserContext::new("BID-owner-dispose".to_owned());
        browser_context.set_active_target_id("TID-owner-dispose".to_owned());
        browser_context.attach_active_session("SID-owner-dispose-target".to_owned());
        conn.insert_browser_context(browser_context);
        let (mut browser_host, handle) = BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(handle);

        let command = DevToolsCommand::RemoveBrowserContext(DevToolsRemoveBrowserContextCommand {
            context: DevToolsCommandContext {
                protocol: DevToolsProtocol::WebDriverBidi,
                session_id: Some(DevToolsSessionId::from("SID-owner-dispose-command")),
                target_id: None,
                browser_context_id: None,
            },
            browser_context_id: DevToolsBrowserContextId::from("BID-owner-dispose"),
        });
        let step = conn
            .try_start_devtools_browser_owner_context_disposal_command(command)
            .await
            .expect("RemoveBrowserContext must use the owner adapter");
        let DevToolsBrowserOwnerContextDisposalCommandTaskStep::Pending(pending) = step else {
            panic!("known user Context disposal should await Browser Host");
        };
        assert!(conn.has_browser_context_id("BID-owner-dispose"));
        assert_eq!(browser_host.ready_len(), 1);

        let dispatch = browser_host
            .complete_next_turn(&mut conn)
            .expect("queued typed Context disposal turn");
        let host_outcome = conn.finish_browser_host_turn_for_test(dispatch).await;
        assert!(host_outcome.into_parts().0.is_empty());
        let completed = pending.wait().await;
        let outcome = conn
            .complete_devtools_browser_owner_context_disposal_command(completed)
            .await;
        let (result, scheduler_events, protocol_events, predecessor) =
            outcome.into_complete_parts();

        assert!(matches!(result, Ok(DevToolsCommandResult::Empty)));
        assert!(scheduler_events.is_empty());
        assert!(predecessor.is_none());
        assert!(
            protocol_events.iter().cloned().any(|event| matches!(
                event.into_parts().1,
                Some(crate::devtools_runtime::AutomationEvent::TargetDestroyed(_))
            )),
            "BiDi target-destroyed projection must remain ahead of disposal side effects"
        );
        assert!(!conn.has_browser_context_id("BID-owner-dispose"));
    }
}
