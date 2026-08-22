use crate::{
    conn::{
        CdpConnection, CdpSessionRoute, DevToolsCommandDispatchOutcome,
        DevToolsCommandExecutionOutput,
    },
    devtools_runtime::{DevToolsCommand, DevToolsCommandContext, DevToolsCommandResult},
};

use super::{
    CompletedFetchCommandDispatch, FetchCommandTaskStep, PendingFetchCommandDispatch,
    complete_pending_devtools_fetch_execution_output, devtools_fetch_execution_output_from_plan,
    devtools_fetch_success_result, fetch_devtools_command_session_ids,
    start_devtools_fetch_command,
};

/// Scheduler-facing task for one typed Fetch terminal decision.
///
/// A main-Document request publishes its decision to Browser Owner. A
/// subresource request keeps its Page participant, but both variants expose
/// the wait to the application scheduler instead of borrowing
/// `CdpConnection` across it.
pub enum DevToolsFetchCommandTaskStep {
    Pending(Box<PendingDevToolsFetchCommand>),
    Complete(Box<DevToolsCommandDispatchOutcome>),
}

/// Move-owned typed Fetch wait with no `&mut CdpConnection` borrow.
#[must_use = "a typed Fetch command must be awaited or explicitly abandoned"]
pub struct PendingDevToolsFetchCommand {
    pending: PendingFetchCommandDispatch,
    owner_route: Option<CdpSessionRoute>,
    success_result: DevToolsCommandResult,
    devtools_context: DevToolsCommandContext,
}

impl PendingDevToolsFetchCommand {
    /// Whether this command owns the same pre-commit renderer publication
    /// gate as its paused top-level navigation.
    pub fn holds_navigation_renderer_publication_gate(&self) -> bool {
        self.pending.holds_navigation_renderer_publication_gate()
    }

    pub async fn wait(self) -> CompletedDevToolsFetchCommand {
        CompletedDevToolsFetchCommand {
            completed: self.pending.wait().await,
            owner_route: self.owner_route,
            success_result: self.success_result,
            devtools_context: self.devtools_context,
        }
    }
}

/// Completed typed Fetch participant ready for frontend-only projection.
#[must_use = "a completed typed Fetch command must be projected"]
pub struct CompletedDevToolsFetchCommand {
    completed: CompletedFetchCommandDispatch,
    owner_route: Option<CdpSessionRoute>,
    success_result: DevToolsCommandResult,
    devtools_context: DevToolsCommandContext,
}

impl CdpConnection {
    /// Starts one typed Fetch terminal decision without hiding its Browser or
    /// renderer participant inside a borrowed connection future.
    ///
    /// Once `Pending` is returned, main-Document execution has no direct
    /// Protocol fallback. The application scheduler must continue servicing
    /// Browser Host turns until the exact task completes.
    pub async fn try_start_devtools_fetch_command_task(
        &mut self,
        command: DevToolsCommand,
    ) -> Result<DevToolsFetchCommandTaskStep, DevToolsCommand> {
        if !is_typed_fetch_terminal_decision(&command) {
            return Err(command);
        }

        let devtools_context = command.context().clone();
        let success_result = devtools_fetch_success_result(&command);
        let (owner_session_id, owner_route) =
            match fetch_devtools_command_session_ids(self, &command) {
                Ok(session_ids) => session_ids,
                Err(error) => {
                    return Ok(DevToolsFetchCommandTaskStep::Complete(Box::new(
                        finish_devtools_fetch_execution_output(
                            self,
                            devtools_context,
                            DevToolsCommandExecutionOutput::new(Err(error)),
                        )
                        .await,
                    )));
                }
            };
        let step = {
            let mut route_scope =
                self.scoped_optional_none_session_owner_route_override(owner_route.clone());
            start_devtools_fetch_command(
                route_scope.conn_mut(),
                None,
                owner_session_id.as_deref(),
                command,
                true,
            )
        };
        match step {
            FetchCommandTaskStep::Complete(plan) => {
                let output = devtools_fetch_execution_output_from_plan(plan, success_result);
                Ok(DevToolsFetchCommandTaskStep::Complete(Box::new(
                    finish_devtools_fetch_execution_output(self, devtools_context, output).await,
                )))
            }
            FetchCommandTaskStep::Pending(pending) => Ok(DevToolsFetchCommandTaskStep::Pending(
                Box::new(PendingDevToolsFetchCommand {
                    pending,
                    owner_route,
                    success_result,
                    devtools_context,
                }),
            )),
        }
    }

    /// Applies one completed typed Fetch task and projects only its frontend
    /// result/effects. Browser execution has already happened in owner turns.
    pub async fn complete_devtools_fetch_command_task(
        &mut self,
        completed: CompletedDevToolsFetchCommand,
    ) -> DevToolsCommandDispatchOutcome {
        let CompletedDevToolsFetchCommand {
            completed,
            owner_route,
            success_result,
            devtools_context,
        } = completed;
        let output = {
            let mut route_scope =
                self.scoped_optional_none_session_owner_route_override(owner_route);
            complete_pending_devtools_fetch_execution_output(
                route_scope.conn_mut(),
                completed,
                success_result,
            )
            .await
        };
        finish_devtools_fetch_execution_output(self, devtools_context, output).await
    }
}

fn is_typed_fetch_terminal_decision(command: &DevToolsCommand) -> bool {
    matches!(
        command,
        DevToolsCommand::ContinueInterceptedRequest(_)
            | DevToolsCommand::ContinueInterceptedResponse(_)
            | DevToolsCommand::ContinueWithAuth(_)
            | DevToolsCommand::FailInterceptedRequest(_)
            | DevToolsCommand::FulfillInterceptedRequest(_)
    )
}

async fn finish_devtools_fetch_execution_output(
    conn: &mut CdpConnection,
    devtools_context: DevToolsCommandContext,
    output: DevToolsCommandExecutionOutput,
) -> DevToolsCommandDispatchOutcome {
    let (result, protocol_events, renderer_output_predecessor) = output.into_parts();
    conn.finish_devtools_command_dispatch(
        devtools_context,
        result,
        protocol_events,
        renderer_output_predecessor,
    )
    .await
}
