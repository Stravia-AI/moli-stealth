use moli_core::browser_host::{
    BrowserAuxiliaryNavigationKind, BrowserCommandId, BrowserContextHandle,
    BrowserHistoryTraversalDestination, BrowserHostHandle, BrowserNavigateCommandOutcome,
    BrowserNavigationTraceEvent, BrowserNavigationTraceSource, BrowserOwnerInput,
    BrowserOwnerInputKind, BrowserPausedNavigationDecision,
};
use serde_json::Value;
use tokio::sync::oneshot;

use super::{
    CdpConnection, CommandDispatchContext, PausedDocumentTransfer, PendingFetchAuthNavigation,
    PendingFetchNavigation, TargetPageResidenceIdentity,
};

use crate::domains::command_output::{BrowserNavigateCommandProjection, CommandOutputPlan};

pub(crate) struct PreparedBrowserOwnerNavigationCommand {
    command_id: Option<u64>,
    command_session_id: Option<String>,
    allow_background_navigation: bool,
    result_payload: Value,
    command_context: CommandDispatchContext,
    reply: oneshot::Sender<CompletedBrowserOwnerNavigationCommand>,
}

impl PreparedBrowserOwnerNavigationCommand {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Option<u64>,
        Option<String>,
        bool,
        Value,
        CommandDispatchContext,
        oneshot::Sender<CompletedBrowserOwnerNavigationCommand>,
    ) {
        (
            self.command_id,
            self.command_session_id,
            self.allow_background_navigation,
            self.result_payload,
            self.command_context,
            self.reply,
        )
    }
}

pub(crate) struct CompletedBrowserOwnerNavigationCommand {
    outcome: Option<BrowserNavigateCommandOutcome>,
    projection: BrowserNavigateCommandProjection,
    command_context: CommandDispatchContext,
}

impl CompletedBrowserOwnerNavigationCommand {
    pub(crate) fn new(
        outcome: Option<BrowserNavigateCommandOutcome>,
        projection: BrowserNavigateCommandProjection,
        command_context: CommandDispatchContext,
    ) -> Self {
        Self {
            outcome,
            projection,
            command_context,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        Option<BrowserNavigateCommandOutcome>,
        BrowserNavigateCommandProjection,
        CommandDispatchContext,
    ) {
        (self.outcome, self.projection, self.command_context)
    }
}

pub(crate) struct PendingBrowserOwnerNavigationCommand {
    reply: oneshot::Receiver<CompletedBrowserOwnerNavigationCommand>,
}

/// Protocol-owned continuation for a frontend command that depends on the
/// Target's initial creation URL being committed first.
///
/// Core carries only the opaque `BrowserCommandId`; the enclosing
/// `Page.createIsolatedWorld` task and its CDP route never enter Browser Host.
pub(crate) struct PreparedBrowserOwnerInitialTargetNavigationCommand {
    reply: oneshot::Sender<CompletedBrowserOwnerInitialTargetNavigationCommand>,
}

impl PreparedBrowserOwnerInitialTargetNavigationCommand {
    pub(crate) fn into_reply(
        self,
    ) -> oneshot::Sender<CompletedBrowserOwnerInitialTargetNavigationCommand> {
        self.reply
    }
}

pub(crate) struct CompletedBrowserOwnerInitialTargetNavigationCommand {
    plan: CommandOutputPlan,
}

impl CompletedBrowserOwnerInitialTargetNavigationCommand {
    pub(crate) fn new(plan: CommandOutputPlan) -> Self {
        Self { plan }
    }

    pub(crate) fn into_plan(self) -> CommandOutputPlan {
        self.plan
    }
}

pub(crate) struct PendingBrowserOwnerInitialTargetNavigationCommand {
    reply: oneshot::Receiver<CompletedBrowserOwnerInitialTargetNavigationCommand>,
}

pub(crate) struct PreparedBrowserOwnerStopLoadingCommand {
    command_context: CommandDispatchContext,
    reply: oneshot::Sender<CompletedBrowserOwnerStopLoadingCommand>,
}

impl PreparedBrowserOwnerStopLoadingCommand {
    pub(crate) fn into_parts(
        self,
    ) -> (
        CommandDispatchContext,
        oneshot::Sender<CompletedBrowserOwnerStopLoadingCommand>,
    ) {
        (self.command_context, self.reply)
    }
}

pub(crate) struct CompletedBrowserOwnerStopLoadingCommand {
    plan: CommandOutputPlan,
    command_context: CommandDispatchContext,
}

impl CompletedBrowserOwnerStopLoadingCommand {
    pub(crate) fn new(plan: CommandOutputPlan, command_context: CommandDispatchContext) -> Self {
        Self {
            plan,
            command_context,
        }
    }

    pub(crate) fn into_parts(self) -> (CommandOutputPlan, CommandDispatchContext) {
        (self.plan, self.command_context)
    }
}

pub(crate) struct PendingBrowserOwnerStopLoadingCommand {
    reply: oneshot::Receiver<CompletedBrowserOwnerStopLoadingCommand>,
}

pub(crate) struct PreparedBrowserOwnerContextDisposalCommand {
    prefix_events: Vec<super::BackgroundProtocolEvent>,
    command_context: CommandDispatchContext,
    reply: oneshot::Sender<CompletedBrowserOwnerContextDisposalCommand>,
}

impl PreparedBrowserOwnerContextDisposalCommand {
    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<super::BackgroundProtocolEvent>,
        CommandDispatchContext,
        oneshot::Sender<CompletedBrowserOwnerContextDisposalCommand>,
    ) {
        (self.prefix_events, self.command_context, self.reply)
    }
}

pub(crate) struct CompletedBrowserOwnerContextDisposalCommand {
    plan: CommandOutputPlan,
    command_context: CommandDispatchContext,
}

impl CompletedBrowserOwnerContextDisposalCommand {
    pub(crate) fn new(plan: CommandOutputPlan, command_context: CommandDispatchContext) -> Self {
        Self {
            plan,
            command_context,
        }
    }

    pub(crate) fn into_parts(self) -> (CommandOutputPlan, CommandDispatchContext) {
        (self.plan, self.command_context)
    }
}

pub(crate) struct PendingBrowserOwnerContextDisposalCommand {
    reply: oneshot::Receiver<CompletedBrowserOwnerContextDisposalCommand>,
}

pub(crate) struct PreparedBrowserOwnerPausedNavigationDecisionCommand {
    pending_navigation: BrowserOwnerPausedNavigationSidecar,
    command_context: CommandDispatchContext,
    reply: oneshot::Sender<CompletedBrowserOwnerPausedNavigationDecisionCommand>,
}

/// Protocol-owned payload paired with one Core paused-navigation decision.
///
/// Fetch request ids, response bodies, auth response bodies and frontend
/// routing remain in this sidecar. Browser Host serializes only the exact Page
/// and neutral decision; the selected turn later consumes the matching sidecar
/// variant.
pub(crate) enum BrowserOwnerPausedNavigationSidecar {
    Request(PendingFetchNavigation),
    Response(Box<PausedDocumentTransfer>),
    Auth(PendingFetchAuthNavigation),
}

impl PreparedBrowserOwnerPausedNavigationDecisionCommand {
    pub(crate) fn into_parts(
        self,
    ) -> (
        BrowserOwnerPausedNavigationSidecar,
        CommandDispatchContext,
        oneshot::Sender<CompletedBrowserOwnerPausedNavigationDecisionCommand>,
    ) {
        (self.pending_navigation, self.command_context, self.reply)
    }
}

pub(crate) struct CompletedBrowserOwnerPausedNavigationDecisionCommand {
    plan: CommandOutputPlan,
    command_context: CommandDispatchContext,
}

impl CompletedBrowserOwnerPausedNavigationDecisionCommand {
    pub(crate) fn new(plan: CommandOutputPlan, command_context: CommandDispatchContext) -> Self {
        Self {
            plan,
            command_context,
        }
    }

    pub(crate) fn into_parts(self) -> (CommandOutputPlan, CommandDispatchContext) {
        (self.plan, self.command_context)
    }
}

pub(crate) struct PendingBrowserOwnerPausedNavigationDecisionCommand {
    reply: oneshot::Receiver<CompletedBrowserOwnerPausedNavigationDecisionCommand>,
}

pub(crate) enum PreparedBrowserOwnerCommand {
    Navigation(PreparedBrowserOwnerNavigationCommand),
    StopLoading(PreparedBrowserOwnerStopLoadingCommand),
    ContextDisposal(PreparedBrowserOwnerContextDisposalCommand),
    InitialTargetNavigation(PreparedBrowserOwnerInitialTargetNavigationCommand),
    PausedNavigationDecision(Box<PreparedBrowserOwnerPausedNavigationDecisionCommand>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreparedBrowserOwnerCommandKind {
    Navigation,
    StopLoading,
    ContextDisposal,
    InitialTargetNavigation,
    PausedNavigationDecision,
}

impl PreparedBrowserOwnerCommand {
    fn kind(&self) -> PreparedBrowserOwnerCommandKind {
        match self {
            Self::Navigation(_) => PreparedBrowserOwnerCommandKind::Navigation,
            Self::StopLoading(_) => PreparedBrowserOwnerCommandKind::StopLoading,
            Self::ContextDisposal(_) => PreparedBrowserOwnerCommandKind::ContextDisposal,
            Self::InitialTargetNavigation(_) => {
                PreparedBrowserOwnerCommandKind::InitialTargetNavigation
            }
            Self::PausedNavigationDecision(_) => {
                PreparedBrowserOwnerCommandKind::PausedNavigationDecision
            }
        }
    }
}

impl PendingBrowserOwnerPausedNavigationDecisionCommand {
    pub(crate) async fn wait(
        self,
    ) -> Result<CompletedBrowserOwnerPausedNavigationDecisionCommand, String> {
        self.reply
            .await
            .map_err(|_| "BrowserOwnerPausedNavigationDecisionCompletionCanceled".to_owned())
    }
}

pub(crate) struct BrowserOwnerPausedNavigationDecisionPublicationFailure {
    error: BrowserOwnerInputPublicationError,
    pending_navigation: Option<Box<BrowserOwnerPausedNavigationSidecar>>,
}

impl BrowserOwnerPausedNavigationDecisionPublicationFailure {
    pub(crate) fn into_parts(
        self,
    ) -> (
        BrowserOwnerInputPublicationError,
        Option<BrowserOwnerPausedNavigationSidecar>,
    ) {
        (self.error, self.pending_navigation.map(|pending| *pending))
    }
}

impl PendingBrowserOwnerStopLoadingCommand {
    pub(crate) async fn wait(self) -> Result<CompletedBrowserOwnerStopLoadingCommand, String> {
        self.reply
            .await
            .map_err(|_| "BrowserOwnerStopLoadingCompletionCanceled".to_owned())
    }
}

impl PendingBrowserOwnerContextDisposalCommand {
    pub(crate) async fn wait(self) -> Result<CompletedBrowserOwnerContextDisposalCommand, String> {
        self.reply
            .await
            .map_err(|_| "BrowserOwnerContextDisposalCompletionCanceled".to_owned())
    }
}

impl PendingBrowserOwnerNavigationCommand {
    pub(crate) async fn wait(self) -> Result<CompletedBrowserOwnerNavigationCommand, String> {
        self.reply
            .await
            .map_err(|_| "BrowserOwnerNavigationCompletionCanceled".to_owned())
    }
}

impl PendingBrowserOwnerInitialTargetNavigationCommand {
    pub(crate) async fn wait(
        self,
    ) -> Result<CompletedBrowserOwnerInitialTargetNavigationCommand, String> {
        self.reply
            .await
            .map_err(|_| "BrowserOwnerInitialTargetNavigationCompletionCanceled".to_owned())
    }
}

/// Why a protocol-neutral input could not enter Browser Host.
///
/// This is an application-composition failure, not a protocol scheduler event;
/// callers may diagnose it but must not recreate a fallback owner queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrowserOwnerInputPublicationError {
    HostNotInstalled { kind: BrowserOwnerInputKind },
    HostStopped { kind: BrowserOwnerInputKind },
}

impl std::fmt::Display for BrowserOwnerInputPublicationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostNotInstalled { kind } => {
                write!(formatter, "Browser Host is not installed for {kind:?}")
            }
            Self::HostStopped { kind } => {
                write!(formatter, "Browser Host stopped before accepting {kind:?}")
            }
        }
    }
}

impl std::error::Error for BrowserOwnerInputPublicationError {}

impl CdpConnection {
    fn allocate_browser_owner_command_id(&mut self) -> BrowserCommandId {
        loop {
            let command_id = self.browser_host_state.allocate_browser_command_id();
            if !self
                .prepared_browser_owner_commands
                .contains_key(&command_id)
            {
                return command_id;
            }
        }
    }

    fn take_prepared_browser_owner_command(
        &mut self,
        command_id: BrowserCommandId,
        expected_kind: PreparedBrowserOwnerCommandKind,
    ) -> Option<PreparedBrowserOwnerCommand> {
        let prepared = self.prepared_browser_owner_commands.remove(&command_id)?;
        let actual_kind = prepared.kind();
        if actual_kind == expected_kind {
            return Some(prepared);
        }

        let previous = self
            .prepared_browser_owner_commands
            .insert(command_id, prepared);
        debug_assert!(previous.is_none());
        tracing::error!(
            browser_command_id = command_id.get(),
            ?expected_kind,
            ?actual_kind,
            "prepared Browser Owner command kind did not match selected Host input"
        );
        None
    }

    /// Publishes one frontend-originated top-level navigation.
    ///
    /// CDP response payload and completion routing remain in Protocol. Core
    /// receives only an opaque correlation id, exact Page residence and
    /// protocol-neutral navigation parameters. Same-Document classification
    /// happens only after the actor selects this exact Page. There is
    /// deliberately no direct-execution fallback when Browser Host is
    /// unavailable.
    pub(crate) fn publish_browser_owner_navigate_command(
        &mut self,
        frontend_command_id: Option<u64>,
        frontend_session_id: Option<&str>,
        page_owner: TargetPageResidenceIdentity,
        url: String,
        referrer: Option<String>,
        allow_background_navigation: bool,
        result_payload: Value,
        command_context: CommandDispatchContext,
    ) -> Result<PendingBrowserOwnerNavigationCommand, BrowserOwnerInputPublicationError> {
        let command_id = self.allocate_browser_owner_command_id();
        let input = BrowserOwnerInput::frontend_navigate(command_id, page_owner, url, referrer);
        self.publish_browser_owner_frontend_navigation_command(
            command_id,
            frontend_command_id,
            frontend_session_id,
            input,
            allow_background_navigation,
            result_payload,
            command_context,
        )
    }

    /// Publishes one frontend-originated top-level reload.
    ///
    /// The exact Page, not its mutable URL, crosses the frontend boundary.
    /// Browser Owner resolves the current URL and starts the reload only after
    /// selecting this command from its FIFO mailbox.
    pub(crate) fn publish_browser_owner_reload_command(
        &mut self,
        frontend_command_id: Option<u64>,
        frontend_session_id: Option<&str>,
        page_owner: TargetPageResidenceIdentity,
        ignore_cache: bool,
        script_to_evaluate_on_load: Option<String>,
        allow_background_navigation: bool,
        result_payload: Value,
        command_context: CommandDispatchContext,
    ) -> Result<PendingBrowserOwnerNavigationCommand, BrowserOwnerInputPublicationError> {
        let command_id = self.allocate_browser_owner_command_id();
        let input = BrowserOwnerInput::frontend_reload(
            command_id,
            page_owner,
            ignore_cache,
            script_to_evaluate_on_load,
        );
        self.publish_browser_owner_frontend_navigation_command(
            command_id,
            frontend_command_id,
            frontend_session_id,
            input,
            allow_background_navigation,
            result_payload,
            command_context,
        )
    }

    /// Publishes one frontend-originated joint session-history traversal.
    ///
    /// The frontend contributes only a browser destination and exact Page.
    /// Browser Core resolves a relative delta against its current cursor, then
    /// resolves the URL and Document-sequence relationship after this input
    /// wins its Browser Host turn.
    pub(crate) fn publish_browser_owner_history_traversal_command(
        &mut self,
        frontend_command_id: Option<u64>,
        frontend_session_id: Option<&str>,
        page_owner: TargetPageResidenceIdentity,
        destination: BrowserHistoryTraversalDestination,
        allow_background_navigation: bool,
        result_payload: Value,
        command_context: CommandDispatchContext,
    ) -> Result<PendingBrowserOwnerNavigationCommand, BrowserOwnerInputPublicationError> {
        let command_id = self.allocate_browser_owner_command_id();
        let input =
            BrowserOwnerInput::frontend_history_traversal(command_id, page_owner, destination);
        self.publish_browser_owner_frontend_navigation_command(
            command_id,
            frontend_command_id,
            frontend_session_id,
            input,
            allow_background_navigation,
            result_payload,
            command_context,
        )
    }

    fn publish_browser_owner_frontend_navigation_command(
        &mut self,
        command_id: BrowserCommandId,
        frontend_command_id: Option<u64>,
        frontend_session_id: Option<&str>,
        input: BrowserOwnerInput,
        allow_background_navigation: bool,
        result_payload: Value,
        command_context: CommandDispatchContext,
    ) -> Result<PendingBrowserOwnerNavigationCommand, BrowserOwnerInputPublicationError> {
        let (reply, pending_reply) = oneshot::channel();
        self.prepared_browser_owner_commands.insert(
            command_id,
            PreparedBrowserOwnerCommand::Navigation(PreparedBrowserOwnerNavigationCommand {
                // Direct WebDriver commands do not always have a wire command
                // id. A foreground Page participant still needs an opaque id
                // so the shared navigation pipeline emits its terminal result
                // plan. The BrowserCommandId is correlation only and never
                // enters Core execution semantics or frontend projection.
                command_id: frontend_command_id
                    .or_else(|| (!allow_background_navigation).then_some(command_id.get())),
                command_session_id: frontend_session_id.map(str::to_owned),
                allow_background_navigation,
                result_payload,
                command_context,
                reply,
            }),
        );
        let kind = input.kind();
        let publication = if let Some(browser_host) = self.browser_host_handle.as_ref() {
            browser_host.publish(input).map_err(|error| {
                BrowserOwnerInputPublicationError::HostStopped { kind: error.kind() }
            })
        } else {
            Err(BrowserOwnerInputPublicationError::HostNotInstalled { kind })
        };
        if let Err(error) = publication {
            let _ = self.take_prepared_browser_owner_navigation_command(command_id);
            return Err(error);
        }
        if moli_trace::cdp_runtime_trace_enabled() {
            tracing::info!(
                target: "moli_cdp_runtime",
                stage = "browser_owner_input_published",
                kind = ?kind,
                browser_command_id = command_id.get(),
            );
        }
        Ok(PendingBrowserOwnerNavigationCommand {
            reply: pending_reply,
        })
    }

    pub(crate) fn take_prepared_browser_owner_navigation_command(
        &mut self,
        command_id: BrowserCommandId,
    ) -> Option<PreparedBrowserOwnerNavigationCommand> {
        let prepared = self.take_prepared_browser_owner_command(
            command_id,
            PreparedBrowserOwnerCommandKind::Navigation,
        )?;
        let PreparedBrowserOwnerCommand::Navigation(prepared) = prepared else {
            unreachable!("prepared command kind was validated")
        };
        Some(prepared)
    }

    /// Publishes one target-slot-scoped stop-loading command.
    ///
    /// Frontend correlation remains in the outer Page command task. Browser
    /// Core receives only an opaque command id and the captured Page-slot
    /// capability; the Host resolves the slot's current Document after FIFO
    /// selection and never falls back to direct Protocol execution.
    pub(crate) fn publish_browser_owner_stop_loading_command(
        &mut self,
        page_owner: TargetPageResidenceIdentity,
        command_context: CommandDispatchContext,
    ) -> Result<PendingBrowserOwnerStopLoadingCommand, BrowserOwnerInputPublicationError> {
        let command_id = self.allocate_browser_owner_command_id();
        let input = BrowserOwnerInput::frontend_stop_loading(command_id, page_owner);
        let (reply, pending_reply) = oneshot::channel();
        self.prepared_browser_owner_commands.insert(
            command_id,
            PreparedBrowserOwnerCommand::StopLoading(PreparedBrowserOwnerStopLoadingCommand {
                command_context,
                reply,
            }),
        );
        let kind = input.kind();
        let publication = if let Some(browser_host) = self.browser_host_handle.as_ref() {
            browser_host.publish(input).map_err(|error| {
                BrowserOwnerInputPublicationError::HostStopped { kind: error.kind() }
            })
        } else {
            Err(BrowserOwnerInputPublicationError::HostNotInstalled { kind })
        };
        if let Err(error) = publication {
            let _ = self.take_prepared_browser_owner_stop_loading_command(command_id);
            return Err(error);
        }
        if moli_trace::cdp_runtime_trace_enabled() {
            tracing::info!(
                target: "moli_cdp_runtime",
                stage = "browser_owner_input_published",
                kind = ?kind,
                browser_command_id = command_id.get(),
            );
        }
        Ok(PendingBrowserOwnerStopLoadingCommand {
            reply: pending_reply,
        })
    }

    pub(crate) fn take_prepared_browser_owner_stop_loading_command(
        &mut self,
        command_id: BrowserCommandId,
    ) -> Option<PreparedBrowserOwnerStopLoadingCommand> {
        let prepared = self.take_prepared_browser_owner_command(
            command_id,
            PreparedBrowserOwnerCommandKind::StopLoading,
        )?;
        let PreparedBrowserOwnerCommand::StopLoading(prepared) = prepared else {
            unreachable!("prepared command kind was validated")
        };
        Some(prepared)
    }

    /// Publishes disposal of one exact BrowserContext instance.
    ///
    /// Protocol event routing and the completion channel remain in this
    /// sidecar. Browser Host receives only the opaque command id and Core
    /// capability, so frontend disconnect cannot cancel accepted cleanup.
    pub(crate) fn publish_browser_owner_context_disposal_command(
        &mut self,
        browser_context_handle: BrowserContextHandle,
        prefix_events: Vec<super::BackgroundProtocolEvent>,
        command_context: CommandDispatchContext,
    ) -> Result<PendingBrowserOwnerContextDisposalCommand, BrowserOwnerInputPublicationError> {
        let command_id = self.allocate_browser_owner_command_id();
        let input =
            BrowserOwnerInput::frontend_dispose_browser_context(command_id, browser_context_handle);
        let (reply, pending_reply) = oneshot::channel();
        self.prepared_browser_owner_commands.insert(
            command_id,
            PreparedBrowserOwnerCommand::ContextDisposal(
                PreparedBrowserOwnerContextDisposalCommand {
                    prefix_events,
                    command_context,
                    reply,
                },
            ),
        );
        let kind = input.kind();
        let publication = if let Some(browser_host) = self.browser_host_handle.as_ref() {
            browser_host.publish(input).map_err(|error| {
                BrowserOwnerInputPublicationError::HostStopped { kind: error.kind() }
            })
        } else {
            Err(BrowserOwnerInputPublicationError::HostNotInstalled { kind })
        };
        if let Err(error) = publication {
            let _ = self.take_prepared_browser_owner_context_disposal_command(command_id);
            return Err(error);
        }
        if moli_trace::cdp_runtime_trace_enabled() {
            tracing::info!(
                target: "moli_cdp_runtime",
                stage = "browser_owner_input_published",
                kind = ?kind,
                browser_command_id = command_id.get(),
            );
        }
        Ok(PendingBrowserOwnerContextDisposalCommand {
            reply: pending_reply,
        })
    }

    pub(crate) fn take_prepared_browser_owner_context_disposal_command(
        &mut self,
        command_id: BrowserCommandId,
    ) -> Option<PreparedBrowserOwnerContextDisposalCommand> {
        let prepared = self.take_prepared_browser_owner_command(
            command_id,
            PreparedBrowserOwnerCommandKind::ContextDisposal,
        )?;
        let PreparedBrowserOwnerCommand::ContextDisposal(prepared) = prepared else {
            unreachable!("prepared command kind was validated")
        };
        Some(prepared)
    }

    /// Publishes one exact Page-scoped decision for a paused top-level
    /// navigation.
    ///
    /// The move-owned paused request remains in the Protocol sidecar. A failed
    /// publication returns it to the caller so the request can be restored;
    /// this boundary never falls back to direct frontend execution.
    pub(crate) fn publish_browser_owner_paused_navigation_decision(
        &mut self,
        page_owner: TargetPageResidenceIdentity,
        pending_navigation: PendingFetchNavigation,
        decision: BrowserPausedNavigationDecision,
        command_context: CommandDispatchContext,
    ) -> Result<
        PendingBrowserOwnerPausedNavigationDecisionCommand,
        BrowserOwnerPausedNavigationDecisionPublicationFailure,
    > {
        self.publish_browser_owner_paused_navigation_decision_with_sidecar(
            page_owner,
            BrowserOwnerPausedNavigationSidecar::Request(pending_navigation),
            decision,
            command_context,
        )
    }

    pub(crate) fn publish_browser_owner_paused_navigation_auth_decision(
        &mut self,
        page_owner: TargetPageResidenceIdentity,
        pending_navigation: PendingFetchAuthNavigation,
        decision: BrowserPausedNavigationDecision,
        command_context: CommandDispatchContext,
    ) -> Result<
        PendingBrowserOwnerPausedNavigationDecisionCommand,
        BrowserOwnerPausedNavigationDecisionPublicationFailure,
    > {
        self.publish_browser_owner_paused_navigation_decision_with_sidecar(
            page_owner,
            BrowserOwnerPausedNavigationSidecar::Auth(pending_navigation),
            decision,
            command_context,
        )
    }

    pub(crate) fn publish_browser_owner_paused_navigation_response_decision(
        &mut self,
        page_owner: TargetPageResidenceIdentity,
        pending_response: PausedDocumentTransfer,
        decision: BrowserPausedNavigationDecision,
        command_context: CommandDispatchContext,
    ) -> Result<
        PendingBrowserOwnerPausedNavigationDecisionCommand,
        BrowserOwnerPausedNavigationDecisionPublicationFailure,
    > {
        self.publish_browser_owner_paused_navigation_decision_with_sidecar(
            page_owner,
            BrowserOwnerPausedNavigationSidecar::Response(Box::new(pending_response)),
            decision,
            command_context,
        )
    }

    fn publish_browser_owner_paused_navigation_decision_with_sidecar(
        &mut self,
        page_owner: TargetPageResidenceIdentity,
        pending_navigation: BrowserOwnerPausedNavigationSidecar,
        decision: BrowserPausedNavigationDecision,
        command_context: CommandDispatchContext,
    ) -> Result<
        PendingBrowserOwnerPausedNavigationDecisionCommand,
        BrowserOwnerPausedNavigationDecisionPublicationFailure,
    > {
        let command_id = self.allocate_browser_owner_command_id();
        let input = BrowserOwnerInput::frontend_paused_navigation_decision(
            command_id, page_owner, decision,
        );
        let (reply, pending_reply) = oneshot::channel();
        self.prepared_browser_owner_commands.insert(
            command_id,
            PreparedBrowserOwnerCommand::PausedNavigationDecision(Box::new(
                PreparedBrowserOwnerPausedNavigationDecisionCommand {
                    pending_navigation,
                    command_context,
                    reply,
                },
            )),
        );
        let kind = input.kind();
        let publication = if let Some(browser_host) = self.browser_host_handle.as_ref() {
            browser_host.publish(input).map_err(|error| {
                BrowserOwnerInputPublicationError::HostStopped { kind: error.kind() }
            })
        } else {
            Err(BrowserOwnerInputPublicationError::HostNotInstalled { kind })
        };
        if let Err(error) = publication {
            let pending_navigation = self
                .take_prepared_browser_owner_paused_navigation_decision_command(command_id)
                .map(|prepared| prepared.into_parts().0);
            if pending_navigation.is_none() {
                tracing::error!(
                    browser_command_id = command_id.get(),
                    "paused navigation decision sidecar disappeared during failed publication"
                );
            }
            return Err(BrowserOwnerPausedNavigationDecisionPublicationFailure {
                error,
                pending_navigation: pending_navigation.map(Box::new),
            });
        }
        if moli_trace::cdp_runtime_trace_enabled() {
            tracing::info!(
                target: "moli_cdp_runtime",
                stage = "browser_owner_input_published",
                kind = ?kind,
                browser_command_id = command_id.get(),
            );
        }
        Ok(PendingBrowserOwnerPausedNavigationDecisionCommand {
            reply: pending_reply,
        })
    }

    pub(crate) fn take_prepared_browser_owner_paused_navigation_decision_command(
        &mut self,
        command_id: BrowserCommandId,
    ) -> Option<PreparedBrowserOwnerPausedNavigationDecisionCommand> {
        let prepared = self.take_prepared_browser_owner_command(
            command_id,
            PreparedBrowserOwnerCommandKind::PausedNavigationDecision,
        )?;
        let PreparedBrowserOwnerCommand::PausedNavigationDecision(prepared) = prepared else {
            unreachable!("prepared command kind was validated")
        };
        Some(*prepared)
    }

    /// Publishes an already-prepared protocol-neutral Browser action.
    ///
    /// Callers may enforce an exact renderer/protocol predecessor before this
    /// boundary, but they cannot execute the action directly if Host is absent.
    pub(crate) fn publish_browser_owner_input(
        &self,
        input: BrowserOwnerInput,
    ) -> Result<(), BrowserOwnerInputPublicationError> {
        let kind = input.kind();
        let publication = if let Some(browser_host) = self.browser_host_handle.as_ref() {
            browser_host.publish(input).map_err(|error| {
                BrowserOwnerInputPublicationError::HostStopped { kind: error.kind() }
            })
        } else {
            Err(BrowserOwnerInputPublicationError::HostNotInstalled { kind })
        };
        if publication.is_ok() && moli_trace::cdp_runtime_trace_enabled() {
            tracing::info!(
                target: "moli_cdp_runtime",
                stage = "browser_owner_input_published",
                kind = ?kind,
            );
        }
        publication
    }

    /// Publishes navigation for an already-resolved auxiliary Target.
    ///
    /// `Ok(false)` means the Target no longer owns a live Page at capture
    /// time. A successful publication freezes Core's exact Page residence;
    /// later replacement is rejected by the Host executor instead of being
    /// followed through a mutable target/session route.
    pub(crate) fn publish_renderer_auxiliary_navigation(
        &self,
        browser_context_id: &str,
        target_id: &str,
        url: String,
        kind: BrowserAuxiliaryNavigationKind,
    ) -> Result<bool, BrowserOwnerInputPublicationError> {
        let Some(page_owner) = self
            .browser_host_state
            .navigation_owner()
            .capture_page_residence(browser_context_id, target_id)
        else {
            return Ok(false);
        };
        self.publish_browser_owner_input(BrowserOwnerInput::renderer_auxiliary_navigation(
            page_owner, url, kind,
        ))?;
        Ok(true)
    }

    /// Publishes the creation URL replacement for the exact Target addressed
    /// by one current route.
    ///
    /// The initial-empty-Document predicate is owned by Core and is checked
    /// both here (to avoid no-op mailbox traffic) and again after Browser Host
    /// selection. No frontend session, command id, or response route enters
    /// the input itself.
    pub(crate) fn publish_initial_target_navigation_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Result<bool, BrowserOwnerInputPublicationError> {
        let Some(url) = self.runtime_session_owner_target_url(session_id) else {
            return Ok(false);
        };
        let Some(page_owner) = self.target_page_residence_identity_for_session(session_id) else {
            return Ok(false);
        };
        if !self
            .browser_host_state
            .navigation_owner()
            .accepts_initial_target_navigation(&page_owner, &url)
        {
            return Ok(false);
        }
        self.publish_browser_owner_input(BrowserOwnerInput::initial_target_navigation(
            page_owner, url,
        ))?;
        Ok(true)
    }

    /// Publishes the initial-Target navigation required by one dependent
    /// frontend command.
    ///
    /// The Page command waits only on the move-owned reply. Browser Host owns
    /// selection, network/renderer participants and terminal projection. A
    /// no-op result means the exact Page no longer needs this transition; the
    /// dependent command re-resolves its current renderer residence next.
    pub(crate) fn publish_initial_target_navigation_prerequisite_for_session_owner(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<
        Option<PendingBrowserOwnerInitialTargetNavigationCommand>,
        BrowserOwnerInputPublicationError,
    > {
        let Some(url) = self.runtime_session_owner_target_url(session_id) else {
            return Ok(None);
        };
        let Some(page_owner) = self.target_page_residence_identity_for_session(session_id) else {
            return Ok(None);
        };
        if !self
            .browser_host_state
            .navigation_owner()
            .accepts_initial_target_navigation(&page_owner, &url)
        {
            return Ok(None);
        }

        let command_id = self.allocate_browser_owner_command_id();
        let input = BrowserOwnerInput::frontend_ensure_initial_target_navigation(
            command_id, page_owner, url,
        );
        let (reply, pending_reply) = oneshot::channel();
        self.prepared_browser_owner_commands.insert(
            command_id,
            PreparedBrowserOwnerCommand::InitialTargetNavigation(
                PreparedBrowserOwnerInitialTargetNavigationCommand { reply },
            ),
        );
        let kind = input.kind();
        let publication = if let Some(browser_host) = self.browser_host_handle.as_ref() {
            browser_host.publish(input).map_err(|error| {
                BrowserOwnerInputPublicationError::HostStopped { kind: error.kind() }
            })
        } else {
            Err(BrowserOwnerInputPublicationError::HostNotInstalled { kind })
        };
        if let Err(error) = publication {
            let _ = self.take_prepared_browser_owner_initial_target_navigation_command(command_id);
            return Err(error);
        }
        if moli_trace::cdp_runtime_trace_enabled() {
            tracing::info!(
                target: "moli_cdp_runtime",
                stage = "browser_owner_input_published",
                kind = ?kind,
                browser_command_id = command_id.get(),
            );
        }
        Ok(Some(PendingBrowserOwnerInitialTargetNavigationCommand {
            reply: pending_reply,
        }))
    }

    pub(crate) fn take_prepared_browser_owner_initial_target_navigation_command(
        &mut self,
        command_id: BrowserCommandId,
    ) -> Option<PreparedBrowserOwnerInitialTargetNavigationCommand> {
        let prepared = self.take_prepared_browser_owner_command(
            command_id,
            PreparedBrowserOwnerCommandKind::InitialTargetNavigation,
        )?;
        let PreparedBrowserOwnerCommand::InitialTargetNavigation(prepared) = prepared else {
            unreachable!("prepared command kind was validated")
        };
        Some(prepared)
    }

    /// Revalidates a selected initial-Target input against Core authority.
    pub(crate) fn browser_owner_accepts_initial_target_navigation(
        &self,
        page_owner: &TargetPageResidenceIdentity,
        url: &str,
    ) -> bool {
        self.browser_host_state
            .navigation_owner()
            .accepts_initial_target_navigation(page_owner, url)
    }

    /// Installs the application-owned Browser Host endpoint before renderer
    /// output can be admitted.
    ///
    /// Rebinding is a migration-time construction error: inputs already sent
    /// to the previous actor cannot be transferred by Protocol. Production
    /// composition installs exactly once; the debug assertion keeps fixtures
    /// honest without turning external divergence into a release panic.
    pub fn install_browser_host_handle(&mut self, handle: BrowserHostHandle) {
        debug_assert!(
            self.browser_host_handle.is_none(),
            "one CdpConnection cannot publish to multiple Browser Host actors"
        );
        if self.browser_host_handle.is_some() {
            tracing::error!("ignoring duplicate Browser Host handle installation");
            return;
        }
        self.browser_host_handle = Some(handle);
    }

    #[doc(hidden)]
    #[cfg(feature = "test-support")]
    pub fn browser_host_handle_for_test_support(&self) -> Option<BrowserHostHandle> {
        self.browser_host_handle.clone()
    }

    /// Publishes a top-level navigation input already moved into prepared
    /// output directly to Browser Host.
    ///
    /// The prepared value retains its exact Page residence. This boundary
    /// deliberately performs no navigation: only the actor receiver may select
    /// the input on a later owner turn.
    pub(crate) fn publish_prepared_top_level_location_navigation_input(
        &mut self,
        page_owner: TargetPageResidenceIdentity,
        navigation: moli_core::page::RendererDocumentSourcedTopLevelLocationNavigation,
    ) -> Result<(), BrowserOwnerInputPublicationError> {
        let trace = self
            .browser_host_state
            .navigation_owner()
            .renderer_navigation_trace_context(
                &page_owner,
                navigation.browser_action_id(),
                navigation.source_document(),
            );
        let trace_for_publication = trace.clone();
        let input = BrowserOwnerInput::renderer_top_level_location_navigation(
            page_owner, navigation, trace,
        );
        let kind = input.kind();
        let publication = if let Some(browser_host) = self.browser_host_handle.as_ref() {
            browser_host.publish(input).map_err(|error| {
                BrowserOwnerInputPublicationError::HostStopped { kind: error.kind() }
            })
        } else {
            Err(BrowserOwnerInputPublicationError::HostNotInstalled { kind })
        };
        if let Err(error) = publication {
            if let Some(trace) = trace_for_publication.as_ref() {
                trace.emit(BrowserNavigationTraceEvent::new(
                    "browser_owner_rejected",
                    BrowserNavigationTraceSource::RendererIntent,
                    "renderer-output",
                    "browser-host-unavailable",
                ));
            }
            return Err(error);
        }
        if moli_trace::cdp_runtime_trace_enabled() {
            tracing::info!(
                target: "moli_cdp_runtime",
                stage = "browser_owner_input_published",
                kind = ?kind,
            );
        }
        if let Some(trace) = trace_for_publication.as_ref() {
            trace.emit(BrowserNavigationTraceEvent::new(
                "browser_action_published",
                BrowserNavigationTraceSource::RendererIntent,
                "renderer-output",
                "browser-owner-queue",
            ));
        }
        Ok(())
    }

    /// Publishes one exact renderer history traversal already moved into
    /// prepared output.
    ///
    /// The input contains no session or resolved history entry. Only Browser
    /// Host may resolve the delta and start the resulting traversal on a later
    /// owner turn; Host absence never recreates a Protocol scheduler fallback.
    pub(crate) fn publish_prepared_top_level_history_traversal_input(
        &self,
        page_owner: TargetPageResidenceIdentity,
        delta: i64,
    ) -> Result<(), BrowserOwnerInputPublicationError> {
        self.publish_browser_owner_input(BrowserOwnerInput::renderer_top_level_history_traversal(
            page_owner, delta,
        ))
    }
}

#[cfg(test)]
mod tests {
    use moli_core::{
        PageId,
        browser_host::{
            BrowserHostActor, BrowserHostTurn, BrowserHostTurnExecutor, BrowserOwnerInput,
            BrowserOwnerInputKind, PageResidenceIdentity, RendererBrowserIntent,
        },
        page::{
            RendererDocumentLifecycleIdentity, RendererDocumentSourcedTopLevelLocationNavigation,
            RendererDocumentToken, RendererFrameToken, RendererLifecycleEpoch,
        },
    };

    use super::*;

    struct RecordingTurnExecutor;

    impl BrowserHostTurnExecutor for RecordingTurnExecutor {
        type Output = BrowserOwnerInput;

        fn execute_browser_host_turn(&mut self, turn: BrowserHostTurn) -> Self::Output {
            turn.into_input()
        }
    }

    fn renderer_navigation(generation: u64) -> RendererDocumentSourcedTopLevelLocationNavigation {
        let page_id = PageId::new_for_testing(31);
        RendererDocumentSourcedTopLevelLocationNavigation::new(
            RendererDocumentLifecycleIdentity {
                frame: RendererFrameToken { page_id },
                document: RendererDocumentToken::new_for_testing(page_id, generation),
                epoch: RendererLifecycleEpoch(generation),
            },
            format!("https://example.test/{generation}"),
        )
    }

    fn page(generation: u64) -> PageResidenceIdentity {
        PageResidenceIdentity::new(
            "context-browser-host-publication".to_owned(),
            Some("target-browser-host-publication".to_owned()),
            generation,
        )
    }

    #[test]
    fn stop_loading_requires_a_live_host_without_direct_protocol_fallback() {
        let mut conn = CdpConnection::new();

        let error = match conn
            .publish_browser_owner_stop_loading_command(page(1), CommandDispatchContext::default())
        {
            Ok(_) => panic!("an unbound Browser Host must reject stop-loading publication"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            BrowserOwnerInputPublicationError::HostNotInstalled {
                kind: BrowserOwnerInputKind::FrontendStopLoading,
            }
        ));
        assert!(conn.prepared_browser_owner_commands.is_empty());
        assert!(
            conn.take_scheduler_events().is_empty(),
            "stop-loading rejection must not recreate a Protocol scheduler fallback"
        );
    }

    #[tokio::test]
    async fn renderer_intent_uses_host_handle_without_protocol_scheduler_fallback() {
        let mut conn = CdpConnection::new();

        assert!(matches!(
            conn.publish_prepared_top_level_location_navigation_input(
                page(1),
                renderer_navigation(1),
            ),
            Err(BrowserOwnerInputPublicationError::HostNotInstalled {
                kind: BrowserOwnerInputKind::RendererTopLevelLocationNavigation,
            })
        ));
        assert!(
            conn.take_scheduler_events().is_empty(),
            "an unbound Browser Host must not recreate the removed Protocol scheduler envelope"
        );

        let (mut actor, handle) = BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(handle);
        conn.publish_prepared_top_level_location_navigation_input(page(2), renderer_navigation(2))
            .expect("bound Browser Host should accept renderer intent");

        let input = actor
            .complete_next_turn(&mut RecordingTurnExecutor)
            .expect("Browser Host actor should own the published input");
        assert_eq!(
            input.kind(),
            BrowserOwnerInputKind::RendererTopLevelLocationNavigation
        );
        let BrowserOwnerInput::RendererIntent(RendererBrowserIntent::TopLevelLocationNavigation(
            input,
        )) = input
        else {
            panic!("expected renderer navigation input");
        };
        assert_eq!(input.page_owner(), &page(2));
        assert!(conn.take_scheduler_events().is_empty());
    }

    #[tokio::test]
    async fn renderer_history_intent_uses_host_handle_without_protocol_scheduler_fallback() {
        let mut conn = CdpConnection::new();

        assert!(matches!(
            conn.publish_prepared_top_level_history_traversal_input(page(3), -1),
            Err(BrowserOwnerInputPublicationError::HostNotInstalled {
                kind: BrowserOwnerInputKind::RendererTopLevelHistoryTraversal,
            })
        ));
        assert!(conn.take_scheduler_events().is_empty());

        let (mut actor, handle) = BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(handle);
        conn.publish_prepared_top_level_history_traversal_input(page(4), 2)
            .expect("bound Browser Host should accept renderer history intent");

        let input = actor
            .complete_next_turn(&mut RecordingTurnExecutor)
            .expect("Browser Host actor should own the renderer history input");
        assert_eq!(
            input.kind(),
            BrowserOwnerInputKind::RendererTopLevelHistoryTraversal
        );
        let BrowserOwnerInput::RendererIntent(RendererBrowserIntent::TopLevelHistoryTraversal(
            input,
        )) = input
        else {
            panic!("expected renderer history traversal input");
        };
        assert_eq!(input.page_owner(), &page(4));
        assert_eq!(input.delta(), 2);
        assert!(conn.take_scheduler_events().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn initial_target_navigation_starts_only_after_browser_host_selection() {
        let mut conn = CdpConnection::new();
        conn.install_default_browser_target();
        let url = "data:text/html,<main>owner initial target</main>";
        conn.browser_context
            .as_mut()
            .expect("default BrowserContext")
            .set_target_url(url.to_owned());

        assert!(matches!(
            conn.publish_initial_target_navigation_for_session_owner(None),
            Err(BrowserOwnerInputPublicationError::HostNotInstalled {
                kind: BrowserOwnerInputKind::InitialTargetNavigation,
            })
        ));
        assert!(
            !conn.has_pending_document_navigation_for_session_owner(None),
            "publication failure must not start navigation through a direct fallback"
        );

        let (mut actor, handle) = BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(handle);
        assert_eq!(
            conn.publish_initial_target_navigation_for_session_owner(None),
            Ok(true)
        );
        assert_eq!(actor.ready_len(), 1);
        assert!(
            !conn.has_pending_document_navigation_for_session_owner(None),
            "mailbox publication alone must not execute the browser action"
        );
        assert!(conn.take_scheduler_events().is_empty());

        let dispatch = actor
            .complete_next_turn(&mut conn)
            .expect("Browser Host should select initial Target input");
        let outcome = conn.finish_browser_host_turn_for_test(dispatch).await;
        let (_events, scheduler_events, _) = outcome.into_protocol_event_parts();
        assert!(
            scheduler_events.iter().all(|event| !matches!(
                event,
                crate::conn::CdpSchedulerEvent::ProtocolWorkPublished { work }
                    if work.kind()
                        != crate::domains::activity::ProtocolSchedulerWorkKind::MainDocumentLoadFactProjection
            )),
            "the actor-selected action may still hand its loaded-Document tail to the existing compatibility participant, but must not publish unrelated frontend work: {scheduler_events:?}"
        );
        assert!(
            conn.runtime_session_owner_slot(None)
                .ok()
                .and_then(|slot| slot.loaded_page())
                .is_some_and(|page| page.final_url().as_str() == url)
        );
        assert!(
            conn.target_initial_empty_document_for_session_owner(None)
                .is_some_and(|document| document.exited()),
            "the actor-selected replacement must exit the exact initial empty Document"
        );
    }

    #[test]
    fn initial_target_prerequisite_rejects_missing_host_without_direct_fallback() {
        let mut conn = CdpConnection::new();
        conn.install_default_browser_target();
        conn.browser_context
            .as_mut()
            .expect("default BrowserContext")
            .set_target_url("https://example.test/prerequisite".to_owned());

        let error =
            match conn.publish_initial_target_navigation_prerequisite_for_session_owner(None) {
                Ok(_) => panic!("an unbound Browser Host must reject the prerequisite"),
                Err(error) => error,
            };
        assert!(matches!(
            error,
            BrowserOwnerInputPublicationError::HostNotInstalled {
                kind: BrowserOwnerInputKind::FrontendEnsureInitialTargetNavigation,
            }
        ));
        assert!(conn.prepared_browser_owner_commands.is_empty());
        assert!(!conn.has_pending_document_navigation_for_session_owner(None));
        assert!(conn.take_scheduler_events().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn initial_target_prerequisite_reply_waits_for_browser_host_terminal() {
        let mut conn = CdpConnection::new();
        conn.install_default_browser_target();
        let url = "data:text/html,<main>owner prerequisite</main>";
        conn.browser_context
            .as_mut()
            .expect("default BrowserContext")
            .set_target_url(url.to_owned());
        let (mut actor, handle) = BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(handle);

        let pending = conn
            .publish_initial_target_navigation_prerequisite_for_session_owner(None)
            .expect("live Browser Host should accept the prerequisite")
            .expect("the creation URL should require initial navigation");
        assert_eq!(actor.ready_len(), 1);
        assert!(
            !conn.has_pending_document_navigation_for_session_owner(None),
            "frontend publication must not start the navigation"
        );

        let mut completion = Box::pin(pending.wait());
        tokio::select! {
            biased;
            _ = &mut completion => {
                panic!("the dependent command must not resume before Browser Host selection");
            }
            _ = std::future::ready(()) => {}
        }

        let dispatch = actor
            .complete_next_turn(&mut conn)
            .expect("Browser Host should select the prerequisite");
        let outcome = conn.finish_browser_host_turn_for_test(dispatch).await;
        let completed = completion
            .await
            .expect("the Host terminal turn should resolve the prerequisite");
        assert!(
            completed
                .into_plan()
                .command_status()
                .is_none_or(|status| status.is_ok())
        );
        assert!(
            outcome.into_protocol_event_parts().0.is_empty(),
            "navigation projection should remain paired with the dependent command sidecar"
        );
        assert!(
            conn.runtime_session_owner_slot(None)
                .ok()
                .and_then(|slot| slot.loaded_page())
                .is_some_and(|page| page.final_url().as_str() == url),
            "the prerequisite must resolve only after the requested initial URL commits"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropped_initial_target_prerequisite_wait_does_not_cancel_browser_action() {
        let mut conn = CdpConnection::new();
        conn.install_default_browser_target();
        let url = "data:text/html,<main>detached prerequisite</main>";
        conn.browser_context
            .as_mut()
            .expect("default BrowserContext")
            .set_target_url(url.to_owned());
        let (mut actor, handle) = BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(handle);

        let pending = conn
            .publish_initial_target_navigation_prerequisite_for_session_owner(None)
            .expect("live Browser Host should accept the prerequisite")
            .expect("the creation URL should require initial navigation");
        drop(pending);

        let dispatch = actor
            .complete_next_turn(&mut conn)
            .expect("Browser Host should retain the accepted action");
        let outcome = conn.finish_browser_host_turn_for_test(dispatch).await;
        let (
            before_renderer_output,
            post_renderer_output,
            renderer_output_boundary,
            post_response_events,
            _,
            _,
        ) = outcome.into_renderer_owner_turn_parts();
        assert!(
            !before_renderer_output.is_empty()
                || !post_renderer_output.is_empty()
                || !post_response_events.is_empty(),
            "a detached dependent command must leave browser-visible navigation effects"
        );
        assert!(
            renderer_output_boundary.is_some(),
            "detached projection must preserve the exact renderer insertion boundary"
        );
        assert!(
            conn.runtime_session_owner_slot(None)
                .ok()
                .and_then(|slot| slot.loaded_page())
                .is_some_and(|page| page.final_url().as_str() == url),
            "dropping the frontend wait must not cancel the Browser action"
        );
        assert!(conn.prepared_browser_owner_commands.is_empty());
    }
}
