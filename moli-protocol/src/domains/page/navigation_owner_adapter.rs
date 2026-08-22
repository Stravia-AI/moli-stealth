use moli_core::browser_host::{
    BrowserHistoryTraversalDestination, BrowserHistoryTraversalResult,
    BrowserNavigateCommandOutcome, BrowserNavigateCommandResult,
};
use url::Url;

use crate::{
    conn::{
        CdpConnection, CommandDispatchContext, CompletedBrowserOwnerNavigationCommand,
        PendingBrowserOwnerNavigationCommand,
    },
    devtools_runtime::{
        DevToolsCommand, DevToolsCommandContext, DevToolsCommandResult, DevToolsError,
        DevToolsErrorKind, DevToolsFrameId, DevToolsHistoryTraversalDestination, DevToolsLoaderId,
        DevToolsNavigateResult, DevToolsProtocol, DevToolsTraverseHistoryResult,
        webdriver_bidi_navigation_id_from_loader_id,
    },
};

use super::{LOADER_ID, navigation};

/// Start result for a direct DevTools top-level navigation admitted by Browser
/// Owner.
///
/// `Err(DevToolsCommand)` from the start method means the command is not a
/// top-level Browser action (currently a child-frame navigation or a command
/// kind not migrated to this adapter). It may continue through its owning
/// Page/renderer path. Once this type is returned, there is no direct Browser
/// execution fallback.
pub enum DevToolsBrowserOwnerNavigationCommandTaskStep {
    Pending(Box<PendingDevToolsBrowserOwnerNavigationCommand>),
    Complete(Box<crate::conn::DevToolsCommandDispatchOutcome>),
}

/// Frontend-only projection retained while Browser Host owns execution.
#[derive(Clone, Copy, Debug)]
enum DirectNavigationResultProjection {
    Navigate { protocol: DevToolsProtocol },
    Reload { protocol: DevToolsProtocol },
    TraverseHistory,
}

impl DirectNavigationResultProjection {
    fn protocol(self) -> DevToolsProtocol {
        match self {
            Self::Navigate { protocol } | Self::Reload { protocol } => protocol,
            Self::TraverseHistory => DevToolsProtocol::Cdp,
        }
    }

    fn exposes_frame(self) -> bool {
        matches!(self, Self::Navigate { .. })
    }
}

/// Move-owned wait for the protocol-neutral Browser command result.
///
/// This value contains no `&mut CdpConnection`; the application scheduler can
/// keep polling Browser Host turns while the command is pending.
#[must_use = "a Browser Owner navigation result must be awaited or explicitly abandoned"]
pub struct PendingDevToolsBrowserOwnerNavigationCommand {
    pending: PendingBrowserOwnerNavigationCommand,
    result_projection: DirectNavigationResultProjection,
    devtools_context: DevToolsCommandContext,
}

impl PendingDevToolsBrowserOwnerNavigationCommand {
    pub async fn wait(self) -> CompletedDevToolsBrowserOwnerNavigationCommand {
        CompletedDevToolsBrowserOwnerNavigationCommand {
            completed: self.pending.wait().await,
            result_projection: self.result_projection,
            devtools_context: self.devtools_context,
        }
    }
}

/// Exact Browser Owner result ready for frontend projection.
#[must_use = "a completed Browser Owner navigation must be projected"]
pub struct CompletedDevToolsBrowserOwnerNavigationCommand {
    completed: Result<CompletedBrowserOwnerNavigationCommand, String>,
    result_projection: DirectNavigationResultProjection,
    devtools_context: DevToolsCommandContext,
}

impl CdpConnection {
    /// Attempts to admit one direct DevTools top-level navigation command to
    /// the shared Browser Owner FIFO.
    ///
    /// The frontend freezes only the exact Page residence. Current URL lookup,
    /// same-Document classification and physical execution happen after the
    /// Browser Host actor selects the input. Child-frame navigation deliberately
    /// remains a Page/renderer action and is returned to the caller unchanged.
    pub async fn try_start_devtools_browser_owner_navigation_command(
        &mut self,
        command: DevToolsCommand,
        background_command_id: Option<u64>,
    ) -> Result<DevToolsBrowserOwnerNavigationCommandTaskStep, DevToolsCommand> {
        if !matches!(
            command,
            DevToolsCommand::Navigate(_)
                | DevToolsCommand::Reload(_)
                | DevToolsCommand::TraverseHistory(_)
        ) {
            return Err(command);
        }
        let allow_background_navigation = matches!(
            &command,
            DevToolsCommand::Navigate(command) if command.wait == crate::devtools_runtime::DevToolsNavigationWait::None
        ) || matches!(
            &command,
            DevToolsCommand::Reload(command) if command.wait == crate::devtools_runtime::DevToolsNavigationWait::None
        ) || matches!(
            &command,
            DevToolsCommand::TraverseHistory(command) if command.wait == crate::devtools_runtime::DevToolsNavigationWait::None
        );
        if matches!(
            &command,
            DevToolsCommand::Navigate(command)
                if navigation::child_frame_navigation_target_id(self, command).is_some()
        ) {
            return Err(command);
        }

        let devtools_context = command.context().clone();
        let route = match navigation::devtools_navigation_target_route(self, &command) {
            Ok(route) => route,
            Err(error) => {
                return Ok(self
                    .complete_direct_navigation_start_error(devtools_context, error)
                    .await);
            }
        };
        let page_owner = {
            let mut route_scope = self.scoped_none_session_owner_route_override(route);
            route_scope
                .conn_mut()
                .target_page_residence_identity_for_session(None)
        };
        let Some(page_owner) = page_owner else {
            return Ok(self
                .complete_direct_navigation_start_error(
                    devtools_context,
                    DevToolsError::new(DevToolsErrorKind::NoSuchTarget, "TargetNotLoaded"),
                )
                .await);
        };

        let protocol = devtools_context.protocol;
        let target_id = page_owner.target_id().map(str::to_owned);
        // A wait:none command receives its typed result from the exact Host
        // acceptance turn below. Do not also attach a migration-period
        // background command id to the detached load, or its later completion
        // could manufacture a second frontend response.
        let owner_frontend_command_id = (!allow_background_navigation)
            .then_some(background_command_id)
            .flatten();
        let publication = match command {
            DevToolsCommand::Navigate(command) => {
                if Url::parse(&command.url).is_err() {
                    return Ok(self
                        .complete_direct_navigation_start_error(
                            devtools_context,
                            DevToolsError::new(
                                DevToolsErrorKind::Internal,
                                "Invalid navigation URL",
                            ),
                        )
                        .await);
                }
                let result_payload = navigation::cdp_navigate_result_payload(
                    None,
                    target_id.as_deref(),
                    Some(LOADER_ID),
                    &command.url,
                );
                self.publish_browser_owner_navigate_command(
                    owner_frontend_command_id,
                    None,
                    page_owner,
                    command.url,
                    command.referrer,
                    allow_background_navigation,
                    result_payload,
                    CommandDispatchContext::default(),
                )
                .map(|pending| {
                    (
                        pending,
                        DirectNavigationResultProjection::Navigate { protocol },
                    )
                })
            }
            DevToolsCommand::Reload(command) => {
                // The internal payload carries a frame field only so the
                // existing navigation pipeline refreshes the exact loader id.
                // Reload's typed frontend result still omits the frame below.
                let result_payload = navigation::cdp_navigate_result_payload(
                    None,
                    target_id.as_deref(),
                    Some(LOADER_ID),
                    "",
                );
                self.publish_browser_owner_reload_command(
                    owner_frontend_command_id,
                    None,
                    page_owner,
                    command.ignore_cache,
                    command.script_to_evaluate_on_load,
                    allow_background_navigation,
                    result_payload,
                    CommandDispatchContext::default(),
                )
                .map(|pending| {
                    (
                        pending,
                        DirectNavigationResultProjection::Reload { protocol },
                    )
                })
            }
            DevToolsCommand::TraverseHistory(command) => {
                let destination = match command.destination {
                    DevToolsHistoryTraversalDestination::Entry { entry_id, .. } => {
                        BrowserHistoryTraversalDestination::Entry(entry_id)
                    }
                    DevToolsHistoryTraversalDestination::Delta(delta) => {
                        BrowserHistoryTraversalDestination::Delta(delta)
                    }
                };
                self.publish_browser_owner_history_traversal_command(
                    owner_frontend_command_id,
                    None,
                    page_owner,
                    destination,
                    allow_background_navigation,
                    serde_json::json!({}),
                    CommandDispatchContext::default(),
                )
                .map(|pending| (pending, DirectNavigationResultProjection::TraverseHistory))
            }
            command => return Err(command),
        };

        match publication {
            Ok((pending, result_projection)) => {
                Ok(DevToolsBrowserOwnerNavigationCommandTaskStep::Pending(
                    Box::new(PendingDevToolsBrowserOwnerNavigationCommand {
                        pending,
                        result_projection,
                        devtools_context,
                    }),
                ))
            }
            Err(error) => Ok(self
                .complete_direct_navigation_start_error(
                    devtools_context,
                    DevToolsError::new(DevToolsErrorKind::Internal, error.to_string()),
                )
                .await),
        }
    }

    async fn complete_direct_navigation_start_error(
        &mut self,
        devtools_context: DevToolsCommandContext,
        error: DevToolsError,
    ) -> DevToolsBrowserOwnerNavigationCommandTaskStep {
        DevToolsBrowserOwnerNavigationCommandTaskStep::Complete(Box::new(
            self.finish_devtools_command_dispatch(devtools_context, Err(error), Vec::new(), None)
                .await,
        ))
    }

    /// Projects one terminal Browser Owner result back into the direct
    /// DevTools command surface without advancing Browser work.
    pub async fn complete_devtools_browser_owner_navigation_command(
        &mut self,
        completed: CompletedDevToolsBrowserOwnerNavigationCommand,
    ) -> crate::conn::DevToolsCommandDispatchOutcome {
        let CompletedDevToolsBrowserOwnerNavigationCommand {
            completed,
            result_projection,
            devtools_context,
        } = completed;
        let (result, command_context) = match completed {
            Ok(completed) => {
                let (outcome, wire_projection, mut command_context) = completed.into_parts();
                let mut result = outcome
                    .as_ref()
                    .map(|outcome| {
                        direct_navigation_result_from_browser_outcome(outcome, result_projection)
                    })
                    .unwrap_or_else(|| {
                        Err(DevToolsError::new(
                            DevToolsErrorKind::Internal,
                            // A Fetch request/auth pause is a Browser-effects
                            // boundary, not a terminal navigation outcome.
                            // BiDi keeps the exact frontend correlation alive
                            // and resolves it when the corresponding Fetch
                            // continuation publishes the command result.
                            "MissingDevToolsCommandResult",
                        ))
                    });
                let (status, mut effects) =
                    wire_projection.project_status_and_browser_effects(outcome);
                if let Some(Err(error)) = status {
                    result = Err(error);
                }
                if let Some(predecessor) = effects.take_renderer_output_predecessor() {
                    command_context.set_renderer_output_predecessor(predecessor);
                }
                let (events, boundary, post_renderer_events, post_response_events) =
                    effects.into_renderer_fenced_background_and_post_response_events(None, None);
                command_context.append_renderer_fenced_protocol_events(
                    events,
                    boundary,
                    post_renderer_events,
                );
                let existing_post_response_events = command_context.take_post_response_events();
                command_context.extend_post_response_events(post_response_events);
                command_context.extend_post_response_events(existing_post_response_events);
                (result, command_context)
            }
            Err(message) => (
                Err(DevToolsError::new(DevToolsErrorKind::Internal, message)),
                crate::conn::CommandDispatchContext::default(),
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

fn direct_navigation_result_from_browser_outcome(
    outcome: &BrowserNavigateCommandOutcome,
    projection: DirectNavigationResultProjection,
) -> Result<DevToolsCommandResult, DevToolsError> {
    match outcome {
        BrowserNavigateCommandOutcome::Completed(result) => match projection {
            DirectNavigationResultProjection::TraverseHistory => {
                direct_history_traversal_result(result)
            }
            DirectNavigationResultProjection::Navigate { .. }
            | DirectNavigationResultProjection::Reload { .. } => Ok(
                DevToolsCommandResult::Navigate(direct_navigate_result(result, projection)),
            ),
        },
        BrowserNavigateCommandOutcome::Rejected(error) => Err(DevToolsError::new(
            DevToolsErrorKind::Internal,
            error.message(),
        )),
    }
}

fn direct_history_traversal_result(
    result: &BrowserNavigateCommandResult,
) -> Result<DevToolsCommandResult, DevToolsError> {
    match result.history_traversal() {
        Some(BrowserHistoryTraversalResult::Noop) => Ok(DevToolsCommandResult::Empty),
        Some(BrowserHistoryTraversalResult::SameDocument) => Ok(
            DevToolsCommandResult::TraverseHistory(DevToolsTraverseHistoryResult {
                same_document: true,
            }),
        ),
        Some(BrowserHistoryTraversalResult::CrossDocument) => Ok(
            DevToolsCommandResult::TraverseHistory(DevToolsTraverseHistoryResult {
                same_document: false,
            }),
        ),
        None => Err(DevToolsError::new(
            DevToolsErrorKind::Internal,
            "BrowserHistoryTraversalResultMissing",
        )),
    }
}

fn direct_navigate_result(
    result: &BrowserNavigateCommandResult,
    projection: DirectNavigationResultProjection,
) -> DevToolsNavigateResult {
    let protocol = projection.protocol();
    let navigation_id = (protocol == DevToolsProtocol::WebDriverBidi)
        .then(|| {
            result
                .loader_id()
                .map(webdriver_bidi_navigation_id_from_loader_id)
        })
        .flatten();
    let frame_id = (protocol != DevToolsProtocol::WebDriverBidi && projection.exposes_frame())
        .then(|| {
            result
                .target_id()
                .map(|target_id| DevToolsFrameId::from(target_id.as_str()))
        })
        .flatten();
    let loader_id =
        if protocol == DevToolsProtocol::WebDriverBidi || result.is_download() == Some(true) {
            None
        } else {
            result.loader_id().map(DevToolsLoaderId::from)
        };
    DevToolsNavigateResult {
        navigation_id,
        frame_id,
        loader_id,
        url: result.requested_url().to_owned(),
    }
}
