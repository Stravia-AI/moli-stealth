use moli_core::browser_host::{
    BrowserNavigateCommandError, BrowserNavigateCommandErrorKind, BrowserNavigateCommandOutcome,
    BrowserNavigateCommandResult, BrowserTargetId,
};
use moli_page_types::{DevToolsSessionKey, FrontendCommandId};
use serde_json::{Value, json};

use super::{
    CommandOutput, CommandOutputPlan, CommandResponseOutput, CommandResponseResult,
    RendererOutputBoundary,
};

/// Protocol-only information needed to put one detached Browser navigation
/// outcome back at its exact command-output position.
///
/// The sidecar contains no Browser execution authority. It retains only wire
/// routing, extension fields, error decoration and renderer ordering state.
#[derive(Debug)]
pub(crate) struct BrowserNavigateCommandProjection {
    plan: CommandOutputPlan,
    response: Option<BrowserNavigateResponseProjection>,
}

#[derive(Debug)]
struct BrowserNavigateResponseProjection {
    output_index: usize,
    before_renderer_boundary: bool,
    route: CommandResponseRoute,
    shape: BrowserNavigateResponseProjectionShape,
}

/// One protocol-neutral Browser navigation outcome plus the frontend-only
/// correlation needed to project it when its shared output FIFO is consumed.
///
/// The producer reaches the navigation response boundary independently of a
/// frontend poll. Command id, session routing and CDP result decoration stay
/// in this Protocol sidecar rather than entering the Core outcome.
#[derive(Debug)]
pub(crate) struct BrowserNavigateCommandOutcomeDelivery {
    frontend_command_id: FrontendCommandId,
    session_key: DevToolsSessionKey,
    outcome: BrowserNavigateCommandOutcome,
    projection: BrowserNavigateCommandProjection,
}

#[derive(Clone, Copy, Debug)]
enum CommandResponseRoute {
    InheritSession,
    WithoutSession,
}

#[derive(Debug)]
enum BrowserNavigateResponseProjectionShape {
    Success {
        extension_fields: serde_json::Map<String, Value>,
        target_id_field: bool,
        loader_id_field: bool,
        error_text_field: bool,
        is_download_field: bool,
    },
    Error {
        code: i32,
        data: Option<Value>,
    },
}

impl CommandOutputPlan {
    /// Separates a Browser navigation command outcome from its CDP projection.
    ///
    /// A background navigation may have no response in this terminal plan: its
    /// response is still produced by the migration-period early-result path.
    /// That state is represented by `None` instead of manufacturing a Browser
    /// completion before the response boundary has actually been reached.
    pub(crate) fn into_browser_navigate_outcome_and_projection(
        self,
        requested_url: &str,
    ) -> (
        Option<BrowserNavigateCommandOutcome>,
        BrowserNavigateCommandProjection,
    ) {
        let CommandOutputPlan {
            outputs,
            post_response_events,
            renderer_output_predecessor,
            renderer_output_boundary,
        } = self;
        let boundary_index = renderer_output_boundary
            .as_ref()
            .map(|boundary| boundary.output_index);
        let mut retained_before_boundary = 0usize;
        let mut retained_outputs = Vec::with_capacity(outputs.len());
        let mut outcome = None;
        let mut response_projection = None;
        let mut produced_multiple_responses = false;

        for (index, output) in outputs.into_iter().enumerate() {
            let (route, response) = match output {
                CommandOutput::Command(response) => {
                    (CommandResponseRoute::InheritSession, response)
                }
                CommandOutput::CommandWithoutSession(response) => {
                    (CommandResponseRoute::WithoutSession, response)
                }
                CommandOutput::OwnerEvent(_) | CommandOutput::BackgroundEvent(_) => {
                    if boundary_index.is_some_and(|boundary| index < boundary) {
                        retained_before_boundary = retained_before_boundary.saturating_add(1);
                    }
                    retained_outputs.push(output);
                    continue;
                }
            };

            if response_projection.is_some() {
                produced_multiple_responses = true;
                continue;
            }

            let output_index = retained_outputs.len();
            let before_renderer_boundary = boundary_index.is_some_and(|boundary| index < boundary);
            match detach_browser_navigate_response(response, requested_url) {
                Ok((detached_outcome, shape)) => {
                    outcome = Some(detached_outcome);
                    response_projection = Some(BrowserNavigateResponseProjection {
                        output_index,
                        before_renderer_boundary,
                        route,
                        shape,
                    });
                }
                Err(message) => {
                    outcome = Some(BrowserNavigateCommandOutcome::Rejected(
                        BrowserNavigateCommandError::new(
                            BrowserNavigateCommandErrorKind::Failed,
                            message,
                        ),
                    ));
                    response_projection = Some(BrowserNavigateResponseProjection {
                        output_index,
                        before_renderer_boundary,
                        route,
                        shape: BrowserNavigateResponseProjectionShape::Error {
                            code: -32000,
                            data: None,
                        },
                    });
                }
            }
        }

        if produced_multiple_responses {
            tracing::error!("Browser navigation terminal plan produced multiple command responses");
            outcome = Some(BrowserNavigateCommandOutcome::Rejected(
                BrowserNavigateCommandError::new(
                    BrowserNavigateCommandErrorKind::Failed,
                    "BrowserNavigateProducedMultipleResponses",
                ),
            ));
            if let Some(response) = response_projection.as_mut() {
                response.shape = BrowserNavigateResponseProjectionShape::Error {
                    code: -32000,
                    data: None,
                };
            }
        }

        let plan = Self {
            outputs: retained_outputs,
            post_response_events,
            renderer_output_predecessor,
            renderer_output_boundary: renderer_output_boundary.map(|boundary| {
                Box::new(RendererOutputBoundary {
                    output_index: retained_before_boundary,
                    cursor: boundary.cursor,
                })
            }),
        };
        (
            outcome,
            BrowserNavigateCommandProjection {
                plan,
                response: response_projection,
            },
        )
    }

    /// Converts a deferred navigation response into an ordinary background
    /// output plan while retaining its protocol-neutral Browser outcome on the
    /// exact command-response envelope.
    pub(crate) fn into_browser_navigate_background_event_plan(
        self,
        requested_url: &str,
        command_id: Option<u64>,
        session_id: Option<&str>,
    ) -> Self {
        let (outcome, projection) =
            self.into_browser_navigate_outcome_and_projection(requested_url);
        let outcome_sidecar = outcome.clone();
        let mut plan = projection
            .project(outcome)
            .into_background_event_plan(command_id, session_id);
        let Some(outcome) = outcome_sidecar else {
            return plan;
        };

        let mut outcome = Some(outcome);
        for output in &mut plan.outputs {
            let CommandOutput::BackgroundEvent(event) = output else {
                continue;
            };
            let Some((event_command_id, _, _)) = event.command_response_payload_ref() else {
                continue;
            };
            if event_command_id != command_id {
                continue;
            }
            let Some(outcome) = outcome.take() else {
                break;
            };
            let attached = event.attach_browser_navigate_command_outcome(outcome);
            debug_assert!(
                attached,
                "typed command response must accept navigation outcome"
            );
            break;
        }
        if outcome.is_some() {
            tracing::error!(
                command_id,
                "deferred Browser navigation outcome had no matching response envelope"
            );
        }
        plan
    }
}

impl BrowserNavigateCommandProjection {
    /// Rebuilds the frontend command response without changing the relative
    /// position of Browser events or the renderer insertion boundary.
    pub(crate) fn project(
        self,
        outcome: Option<BrowserNavigateCommandOutcome>,
    ) -> CommandOutputPlan {
        let (mut plan, projected_response) = self.into_projected_response(outcome);
        if let Some((output_index, before_renderer_boundary, route, response)) = projected_response
        {
            insert_projected_command_response(
                &mut plan,
                output_index,
                before_renderer_boundary,
                route,
                response,
            );
        }
        plan
    }

    /// Validates the frontend response shape while leaving the Browser effect
    /// stream detached from that response.
    ///
    /// Direct typed frontends return the neutral result through their own
    /// adapter. Re-inserting and then flattening the CDP response here would
    /// discard the exact renderer insertion boundary carried by `plan`.
    pub(crate) fn project_status_and_browser_effects(
        self,
        outcome: Option<BrowserNavigateCommandOutcome>,
    ) -> (
        Option<Result<(), crate::devtools_runtime::DevToolsError>>,
        CommandOutputPlan,
    ) {
        if self.response.is_none() {
            // A typed wait:none frontend consumes the neutral accepted outcome
            // directly. Its detached background load has Browser effects but
            // deliberately carries no second wire-response projection.
            return (None, self.plan);
        }
        let (plan, projected_response) = self.into_projected_response(outcome);
        let status = projected_response
            .as_ref()
            .map(|(_, _, _, response)| response.status());
        (status, plan)
    }

    /// Drops only the abandoned frontend response while retaining all Browser
    /// side effects and ordering fences.
    pub(crate) fn into_browser_effects_plan(self) -> CommandOutputPlan {
        self.plan
    }

    fn into_projected_response(
        mut self,
        outcome: Option<BrowserNavigateCommandOutcome>,
    ) -> (
        CommandOutputPlan,
        Option<(usize, bool, CommandResponseRoute, CommandResponseOutput)>,
    ) {
        let projected_response = match (self.response.take(), outcome) {
            (None, None) => None,
            (Some(projection), Some(outcome)) => {
                let BrowserNavigateResponseProjection {
                    output_index,
                    before_renderer_boundary,
                    route,
                    shape,
                } = projection;
                let response = project_browser_navigate_response(outcome, shape);
                Some((output_index, before_renderer_boundary, route, response))
            }
            (None, Some(outcome)) => {
                tracing::error!(
                    ?outcome,
                    "Browser navigation outcome has no frontend response projection"
                );
                None
            }
            (Some(projection), None) => {
                tracing::error!(
                    "Browser navigation response projection has no protocol-neutral outcome"
                );
                Some((
                    projection.output_index,
                    projection.before_renderer_boundary,
                    projection.route,
                    CommandResponseOutput::Error {
                        code: -32000,
                        message: "BrowserNavigateOutcomeMissing".to_owned(),
                        data: None,
                    },
                ))
            }
        };
        (self.plan, projected_response)
    }
}

impl BrowserNavigateCommandOutcomeDelivery {
    pub(crate) fn completed(
        command_id: u64,
        session_id: Option<&str>,
        requested_url: &str,
        result_payload: Value,
    ) -> Self {
        let (outcome, projection) = CommandOutputPlan::result(result_payload)
            .into_browser_navigate_outcome_and_projection(requested_url);
        let outcome = outcome.unwrap_or_else(|| {
            tracing::error!(
                "completed Browser navigation response did not produce a neutral outcome"
            );
            BrowserNavigateCommandOutcome::Rejected(BrowserNavigateCommandError::new(
                BrowserNavigateCommandErrorKind::Failed,
                "BrowserNavigateOutcomeMissing",
            ))
        });
        Self {
            frontend_command_id: FrontendCommandId::new(command_id),
            session_key: DevToolsSessionKey::from_wire_session_id(session_id),
            outcome,
            projection,
        }
    }

    /// Projects exactly one command response at the frontend-consumption
    /// boundary. This adapter never executes or advances Browser work.
    pub(crate) fn into_background_protocol_event(self) -> crate::conn::BackgroundProtocolEvent {
        let Self {
            frontend_command_id,
            session_key,
            outcome,
            projection,
        } = self;
        let command_id = frontend_command_id.get();
        let session_id = session_key.wire_session_id();
        let outcome_sidecar = outcome.clone();
        let mut events = projection
            .project(Some(outcome))
            .into_background_events(Some(command_id), session_id);
        if events.len() == 1 {
            let event = match events.pop() {
                Some(event) => event,
                None => browser_navigate_projection_failure_event(command_id, session_id),
            };
            return event.bind_browser_navigate_command_outcome(outcome_sidecar);
        }

        tracing::error!(
            event_count = events.len(),
            "Browser navigation outcome projection did not produce exactly one response"
        );
        browser_navigate_projection_failure_event(command_id, session_id)
            .bind_browser_navigate_command_outcome(outcome_sidecar)
    }
}

fn browser_navigate_projection_failure_event(
    command_id: u64,
    session_id: Option<&str>,
) -> crate::conn::BackgroundProtocolEvent {
    crate::conn::BackgroundProtocolEvent::command_error(
        Some(command_id),
        session_id,
        -32000,
        "BrowserNavigateOutcomeProjectionFailed".to_owned(),
        None,
    )
}

fn detach_browser_navigate_response(
    response: CommandResponseOutput,
    requested_url: &str,
) -> Result<
    (
        BrowserNavigateCommandOutcome,
        BrowserNavigateResponseProjectionShape,
    ),
    String,
> {
    match response {
        CommandResponseOutput::Success(result) => {
            let mut extension_fields = match result {
                CommandResponseResult::Empty => serde_json::Map::new(),
                CommandResponseResult::Json(Value::Object(payload)) => payload,
                CommandResponseResult::Json(_) => {
                    return Err("BrowserNavigateResultMustBeAnObject".to_owned());
                }
            };
            let target_id = take_string_projection_field(&mut extension_fields, "frameId")
                .map(BrowserTargetId::new);
            let target_id_field = target_id.is_some();
            let loader_id = take_string_projection_field(&mut extension_fields, "loaderId");
            let loader_id_field = loader_id.is_some();
            let loader_id =
                loader_id.or_else(|| projected_bidi_navigation_loader_id(&extension_fields));
            let error_text = take_string_projection_field(&mut extension_fields, "errorText");
            let error_text_field = error_text.is_some();
            let is_download = take_bool_projection_field(&mut extension_fields, "isDownload");
            let is_download_field = is_download.is_some();
            Ok((
                BrowserNavigateCommandOutcome::Completed(BrowserNavigateCommandResult::new(
                    requested_url,
                    target_id,
                    loader_id,
                    error_text,
                    is_download,
                )),
                BrowserNavigateResponseProjectionShape::Success {
                    extension_fields,
                    target_id_field,
                    loader_id_field,
                    error_text_field,
                    is_download_field,
                },
            ))
        }
        CommandResponseOutput::Error {
            code,
            message,
            data,
        } => Ok((
            BrowserNavigateCommandOutcome::Rejected(BrowserNavigateCommandError::new(
                browser_navigate_error_kind_from_cdp_code(code),
                message,
            )),
            BrowserNavigateResponseProjectionShape::Error { code, data },
        )),
    }
}

fn projected_bidi_navigation_loader_id(fields: &serde_json::Map<String, Value>) -> Option<String> {
    fields
        .get("navigation")
        .and_then(Value::as_str)
        .and_then(|navigation_id| navigation_id.strip_prefix("navigation-"))
        .filter(|loader_id| !loader_id.is_empty())
        .map(str::to_owned)
}

fn take_string_projection_field(
    fields: &mut serde_json::Map<String, Value>,
    name: &str,
) -> Option<String> {
    match fields.remove(name) {
        Some(Value::String(value)) => Some(value),
        Some(value) => {
            fields.insert(name.to_owned(), value);
            None
        }
        None => None,
    }
}

fn take_bool_projection_field(
    fields: &mut serde_json::Map<String, Value>,
    name: &str,
) -> Option<bool> {
    match fields.remove(name) {
        Some(Value::Bool(value)) => Some(value),
        Some(value) => {
            fields.insert(name.to_owned(), value);
            None
        }
        None => None,
    }
}

fn browser_navigate_error_kind_from_cdp_code(code: i32) -> BrowserNavigateCommandErrorKind {
    match code {
        -32602 => BrowserNavigateCommandErrorKind::InvalidInput,
        -32001 => BrowserNavigateCommandErrorKind::RequesterUnavailable,
        -31998 => BrowserNavigateCommandErrorKind::TargetUnavailable,
        _ => BrowserNavigateCommandErrorKind::Failed,
    }
}

fn project_browser_navigate_response(
    outcome: BrowserNavigateCommandOutcome,
    projection: BrowserNavigateResponseProjectionShape,
) -> CommandResponseOutput {
    match (outcome, projection) {
        (
            BrowserNavigateCommandOutcome::Completed(result),
            BrowserNavigateResponseProjectionShape::Success {
                mut extension_fields,
                target_id_field,
                loader_id_field,
                error_text_field,
                is_download_field,
            },
        ) => {
            if target_id_field && let Some(target_id) = result.target_id() {
                extension_fields.insert("frameId".to_owned(), json!(target_id.as_str()));
            }
            if loader_id_field && let Some(loader_id) = result.loader_id() {
                extension_fields.insert("loaderId".to_owned(), json!(loader_id));
            }
            if error_text_field && let Some(error_text) = result.error_text() {
                extension_fields.insert("errorText".to_owned(), json!(error_text));
            }
            if is_download_field && let Some(is_download) = result.is_download() {
                extension_fields.insert("isDownload".to_owned(), json!(is_download));
            }
            CommandResponseOutput::Success(CommandResponseResult::Json(Value::Object(
                extension_fields,
            )))
        }
        (
            BrowserNavigateCommandOutcome::Rejected(error),
            BrowserNavigateResponseProjectionShape::Error { code, data },
        ) => CommandResponseOutput::Error {
            code,
            message: error.message().to_owned(),
            data,
        },
        (outcome, projection) => {
            tracing::error!(
                ?outcome,
                ?projection,
                "Browser navigation outcome and frontend projection shape diverged"
            );
            CommandResponseOutput::Error {
                code: -32000,
                message: "BrowserNavigateProjectionMismatch".to_owned(),
                data: None,
            }
        }
    }
}

fn insert_projected_command_response(
    plan: &mut CommandOutputPlan,
    requested_index: usize,
    before_renderer_boundary: bool,
    route: CommandResponseRoute,
    response: CommandResponseOutput,
) {
    let output_index = requested_index.min(plan.outputs.len());
    if output_index != requested_index {
        tracing::error!(
            requested_index,
            output_index,
            "Browser navigation response insertion index exceeded its projection sidecar"
        );
    }
    if before_renderer_boundary && let Some(boundary) = plan.renderer_output_boundary.as_mut() {
        boundary.output_index = boundary.output_index.saturating_add(1);
    }
    let output = match route {
        CommandResponseRoute::InheritSession => CommandOutput::Command(response),
        CommandResponseRoute::WithoutSession => CommandOutput::CommandWithoutSession(response),
    };
    plan.outputs.insert(output_index, output);
}

#[cfg(test)]
mod tests {
    use moli_core::{
        PageId, RendererOutputCursor, RendererOutputFence, RendererOutputStreamIdentity,
        browser_host::{BrowserNavigateCommandErrorKind, BrowserNavigateCommandOutcome},
    };
    use serde_json::json;

    use super::*;
    use crate::domains::command_output::protocol_message_background_event;

    #[test]
    fn browser_navigate_outcome_round_trips_response_at_renderer_boundary() {
        let mut plan = CommandOutputPlan::default();
        plan.push_background_event(protocol_message_background_event(json!({
            "method": "Page.beforeNavigateResponse",
            "params": {"sequence": 1}
        })));
        plan.push_result(json!({
            "frameId": "FRAME-1",
            "loaderId": "LOADER-1",
            "futureField": {"preserved": true}
        }));
        let stream =
            RendererOutputStreamIdentity::new_page_for_protocol_test(PageId::new_for_testing(91));
        plan.insert_renderer_output_boundary(RendererOutputFence::new_for_test(
            RendererOutputCursor::new_for_test(stream, 7),
        ));
        plan.push_background_event(protocol_message_background_event(json!({
            "method": "Page.afterRendererCommit",
            "params": {"sequence": 2}
        })));

        let (outcome, projection) =
            plan.into_browser_navigate_outcome_and_projection("https://requested.example/");
        let Some(BrowserNavigateCommandOutcome::Completed(result)) = outcome.as_ref() else {
            panic!("expected a completed Browser navigation outcome");
        };
        assert_eq!(result.requested_url(), "https://requested.example/");
        assert_eq!(result.target_id().map(|id| id.as_str()), Some("FRAME-1"));
        assert_eq!(result.loader_id(), Some("LOADER-1"));
        assert_eq!(result.error_text(), None);
        assert_eq!(result.is_download(), None);

        let (before, boundary, after, post_response) = projection
            .project(outcome)
            .into_renderer_fenced_background_and_post_response_events(Some(42), Some("SID-1"));
        let before = before
            .into_iter()
            .map(|event| event.into_protocol_message())
            .collect::<Vec<_>>();
        let after = after
            .into_iter()
            .map(|event| event.into_protocol_message())
            .collect::<Vec<_>>();

        assert_eq!(
            before,
            vec![
                json!({
                    "method": "Page.beforeNavigateResponse",
                    "params": {"sequence": 1}
                }),
                json!({
                    "id": 42,
                    "result": {
                        "frameId": "FRAME-1",
                        "loaderId": "LOADER-1",
                        "futureField": {"preserved": true}
                    },
                    "sessionId": "SID-1"
                }),
            ]
        );
        assert_eq!(boundary.map(|fence| fence.cursor().sequence()), Some(7));
        assert_eq!(
            after,
            vec![json!({
                "method": "Page.afterRendererCommit",
                "params": {"sequence": 2}
            })]
        );
        assert!(post_response.is_empty());
    }

    #[test]
    fn browser_navigate_rejection_round_trips_wire_error_decoration() {
        let mut plan = CommandOutputPlan::default();
        plan.push_error_with_data(
            -31998,
            "NoSuchTarget",
            Some(json!({"targetId": "FRAME-stale"})),
        );

        let (outcome, projection) =
            plan.into_browser_navigate_outcome_and_projection("https://stale.example/");
        let Some(BrowserNavigateCommandOutcome::Rejected(error)) = outcome.as_ref() else {
            panic!("expected a rejected Browser navigation outcome");
        };
        assert_eq!(
            error.kind(),
            BrowserNavigateCommandErrorKind::TargetUnavailable
        );
        assert_eq!(error.message(), "NoSuchTarget");

        let mut messages = Vec::new();
        projection
            .project(outcome)
            .emit_into(&mut messages, Some(43), Some("SID-stale"));
        assert_eq!(
            messages,
            vec![json!({
                "id": 43,
                "error": {
                    "code": -31998,
                    "message": "NoSuchTarget",
                    "data": {"targetId": "FRAME-stale"}
                },
                "sessionId": "SID-stale"
            })]
        );
    }

    #[test]
    fn browser_navigate_bidi_projection_detaches_loader_without_changing_wire_shape() {
        let plan = CommandOutputPlan::result(json!({
            "navigation": "navigation-LOADER-BIDI",
            "url": "https://bidi.example/final",
        }));

        let (outcome, projection) =
            plan.into_browser_navigate_outcome_and_projection("https://bidi.example/requested");
        let Some(BrowserNavigateCommandOutcome::Completed(result)) = outcome.as_ref() else {
            panic!("expected a completed Browser navigation outcome");
        };
        assert_eq!(result.requested_url(), "https://bidi.example/requested");
        assert_eq!(result.loader_id(), Some("LOADER-BIDI"));

        let messages = projection
            .project(outcome)
            .into_background_events(Some(45), None)
            .into_iter()
            .map(|event| event.into_protocol_message())
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec![json!({
                "id": 45,
                "result": {
                    "navigation": "navigation-LOADER-BIDI",
                    "url": "https://bidi.example/final",
                },
            })]
        );
    }

    #[test]
    fn deferred_browser_navigate_response_retains_exact_rejection_sidecar() {
        let plan = CommandOutputPlan::error(-32000, "Navigation aborted by Fetch");

        let events = plan
            .into_browser_navigate_background_event_plan(
                "https://blocked.example/",
                Some(46),
                Some("SID-bidi"),
            )
            .into_background_events(Some(99), None);

        assert_eq!(events.len(), 1);
        let (command_id, outcome) = events[0]
            .browser_navigate_command_outcome()
            .expect("deferred response must carry the neutral navigation outcome");
        assert_eq!(command_id, Some(46));
        let BrowserNavigateCommandOutcome::Rejected(error) = outcome else {
            panic!("expected a rejected Browser navigation outcome");
        };
        assert_eq!(error.message(), "Navigation aborted by Fetch");
        assert_eq!(
            events[0].clone().into_protocol_message(),
            json!({
                "id": 46,
                "error": {
                    "code": -32000,
                    "message": "Navigation aborted by Fetch",
                },
                "sessionId": "SID-bidi",
            })
        );
    }

    #[test]
    fn browser_navigate_deferred_response_does_not_invent_an_outcome() {
        let mut plan = CommandOutputPlan::default();
        plan.push_background_event(protocol_message_background_event(json!({
            "method": "Network.requestWillBeSent",
            "params": {"requestId": "REQ-1"}
        })));

        let (outcome, projection) =
            plan.into_browser_navigate_outcome_and_projection("https://background.example/");
        assert!(outcome.is_none());

        let mut messages = Vec::new();
        projection
            .project(outcome)
            .emit_into(&mut messages, Some(44), Some("SID-background"));
        assert_eq!(
            messages,
            vec![json!({
                "method": "Network.requestWillBeSent",
                "params": {"requestId": "REQ-1"}
            })]
        );
    }

    #[test]
    fn typed_wait_none_outcome_keeps_response_free_background_effects() {
        let mut plan = CommandOutputPlan::default();
        plan.push_background_event(protocol_message_background_event(json!({
            "method": "Page.frameStartedNavigating",
            "params": {"frameId": "FRAME-WAIT-NONE"}
        })));
        let (detached_outcome, projection) =
            plan.into_browser_navigate_outcome_and_projection("https://wait-none.example/");
        assert!(detached_outcome.is_none());
        let accepted = Some(BrowserNavigateCommandOutcome::Completed(
            BrowserNavigateCommandResult::new(
                "https://wait-none.example/",
                Some(BrowserTargetId::new("FRAME-WAIT-NONE")),
                Some("LOADER-WAIT-NONE".to_owned()),
                None,
                None,
            ),
        ));

        let (status, effects) =
            projection.project_status_and_browser_effects(accepted.as_ref().cloned());
        assert!(status.is_none());
        let (effect_status, events) = effects.into_command_status_and_background_events();
        assert!(effect_status.is_none());
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].clone().into_protocol_message(),
            json!({
                "method": "Page.frameStartedNavigating",
                "params": {"frameId": "FRAME-WAIT-NONE"}
            })
        );
    }
}
