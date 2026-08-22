use crate::conn::{
    BrowserOwnerPausedNavigationSidecar, CapturedBody, CdpConnection, Cmd, CommandDispatchContext,
    DEFAULT_LOADER_ID, PendingFetchNavigation, TargetPageResidenceIdentity,
    monotonic_timestamp_seconds,
};
use crate::devtools_runtime::{
    DevToolsAuthChallengeAction, DevToolsCommand, DevToolsContinueInterceptedRequestCommand,
    DevToolsContinueInterceptedResponseCommand, DevToolsContinueWithAuthCommand,
    DevToolsFailInterceptedRequestCommand, DevToolsFulfillInterceptedRequestCommand,
    DevToolsProtocol, DevToolsRequestId,
};
use crate::domains::command_output::CommandOutputPlan;
use crate::domains::{activity, network, page};
use moli_core::browser_host::BrowserPausedNavigationDecision;
use moli_core::page::{
    CompletedPageCommand, RendererSyntheticResponseBody, SubresourceResourceType,
};
use moli_url_policy::BrowserUrlScheme;
use url::Url;

use super::helpers::{
    decode_base64_bytes, decode_base64_to_string, response_headers_from_params,
    response_headers_with_presence_from_params,
};
use super::navigation::{
    complete_tokened_materialized_navigation_as_background_events_async,
    load_or_pause_navigation_for_auth_as_background_events_async,
};
use super::navigation_resume::continue_streaming_document_response_in_background;
use super::params::{
    CloseWebSocketParams, ContinueRequestParams, ContinueResponseParams,
    DispatchWebSocketMessageParams, FailRequestParams, FulfillRequestParams,
    WebSocketMessageOpcode,
};
use super::state::{
    PreparedSubresourceCorrelation, action_session_id_for_devtools_context,
    pending_request_action_result_with_id_validation, take_pending_navigation,
    take_pending_subresource_fetch_request_for_action_session,
    take_pending_subresource_response_request_for_action_session,
};
use super::{
    CompletedFetchCommandOperation, FetchCommandOutput, FetchCommandTaskStep,
    PendingFetchCommandDispatch, PendingFetchCommandKind, PendingFetchCommandOperation,
};

const BLOCKED_BY_CLIENT_ERROR_TEXT: &str = "net::ERR_BLOCKED_BY_CLIENT";
const CONTINUE_RESPONSE_AFTER_BODY_TAKEN_ERROR: &str =
    "Unable to continue request as is after body is taken";
const CONTINUE_RESPONSE_PARTIAL_OVERRIDE_ERROR: &str =
    "Cannot override only status or headers, both should be provided";

fn emit_devtools_empty_success(out: &mut FetchCommandOutput) {
    out.push_success();
}

pub(super) fn start_devtools_fetch_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    command: DevToolsCommand,
    admit_main_document_to_browser_owner: bool,
) -> FetchCommandTaskStep {
    match command {
        DevToolsCommand::ContinueInterceptedRequest(command) => {
            start_devtools_continue_intercepted_request_command(
                conn,
                command_id,
                command_session_id,
                &command,
                admit_main_document_to_browser_owner,
            )
        }
        DevToolsCommand::ContinueInterceptedResponse(command) => {
            start_devtools_continue_intercepted_response_command(
                conn,
                command_id,
                command_session_id,
                &command,
                admit_main_document_to_browser_owner,
            )
        }
        DevToolsCommand::ContinueWithAuth(command) => {
            super::auth::start_devtools_continue_with_auth_command(
                conn,
                command_id,
                command_session_id,
                &command,
                admit_main_document_to_browser_owner,
            )
        }
        DevToolsCommand::FailInterceptedRequest(command) => {
            start_devtools_fail_intercepted_request_command(
                conn,
                command_id,
                command_session_id,
                command,
                admit_main_document_to_browser_owner,
            )
        }
        DevToolsCommand::FulfillInterceptedRequest(command) => {
            start_devtools_fulfill_intercepted_request_command(
                conn,
                command_id,
                command_session_id,
                command,
                admit_main_document_to_browser_owner,
            )
        }
        _ => FetchCommandTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "UnsupportedDevToolsCommand",
        )),
    }
}

fn navigation_fail_request_error_text(error_reason: Option<String>) -> String {
    let error_text = error_reason.unwrap_or_else(|| "Fetch request failed".to_owned());
    match error_text.as_str() {
        "BlockedByClient" => BLOCKED_BY_CLIENT_ERROR_TEXT.to_owned(),
        _ => error_text,
    }
}

fn optional_response_code(code: Option<i64>) -> Result<Option<u16>, ()> {
    code.map(u16::try_from).transpose().map_err(|_| ())
}

fn parse_continue_request_url(raw_url: &str) -> Result<Url, ()> {
    let url = Url::parse(raw_url).map_err(|_| ())?;
    match BrowserUrlScheme::from_url(&url) {
        BrowserUrlScheme::Http
        | BrowserUrlScheme::Https
        | BrowserUrlScheme::WebSocket
        | BrowserUrlScheme::SecureWebSocket => Ok(url),
        _ => Err(()),
    }
}

pub(super) enum PendingContinueRequestState {
    BrowserOwnerNavigationDecision,
    CompatibilityNavigation {
        pending: Box<crate::conn::PendingFetchNavigation>,
    },
    SubresourceFetch {
        correlation: PreparedSubresourceCorrelation,
    },
}

pub(super) fn start_continue_request_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> FetchCommandTaskStep {
    let params: ContinueRequestParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let command = match build_cdp_continue_intercepted_request_command(conn, cmd, params) {
        Ok(command) => command,
        Err(()) => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    start_devtools_continue_intercepted_request_command(
        conn,
        cmd.id,
        cmd.session_id,
        &command,
        true,
    )
}

fn build_cdp_continue_intercepted_request_command(
    conn: &CdpConnection,
    cmd: &Cmd<'_>,
    params: ContinueRequestParams,
) -> Result<DevToolsContinueInterceptedRequestCommand, ()> {
    if let Some(url) = params.url.as_deref() {
        parse_continue_request_url(url)?;
    }
    let decoded_post_data = match params.post_data.as_ref().map(AsRef::as_ref) {
        Some(post_data) => match decode_base64_to_string(post_data) {
            Ok(body) => Some(body),
            Err(()) => return Err(()),
        },
        None => None,
    };
    let headers = params.headers.map(|headers| {
        headers
            .into_iter()
            .map(|header| (header.name, header.value))
            .collect::<Vec<_>>()
    });
    let (browser_context_id, target_id) =
        devtools_fetch_owner_identity_for_session(conn, cmd.session_id);
    Ok(DevToolsContinueInterceptedRequestCommand {
        context: cmd.devtools_command_context(target_id.as_deref(), browser_context_id.as_deref()),
        request_id: DevToolsRequestId::from(params.request_id.as_ref().to_owned()),
        url: params.url,
        method: params.method,
        post_data: decoded_post_data,
        headers,
        intercept_response: params.intercept_response.unwrap_or(false),
    })
}

fn parsed_continue_request_url(
    command: &DevToolsContinueInterceptedRequestCommand,
) -> Result<Option<Url>, ()> {
    match command.url.as_deref() {
        Some(url) => parse_continue_request_url(url).map(Some),
        None => Ok(None),
    }
}

fn start_devtools_continue_intercepted_request_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    command: &DevToolsContinueInterceptedRequestCommand,
    admit_to_browser_owner: bool,
) -> FetchCommandTaskStep {
    let parsed_url = match parsed_continue_request_url(command) {
        Ok(parsed_url) => parsed_url,
        Err(()) => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let request_id = command.request_id.as_str().to_owned();
    let action_session_id = action_session_id_for_devtools_context(
        command_session_id,
        command.context.protocol,
        command.context.session_id.as_ref(),
    );
    if let Some(pending) = take_pending_subresource_fetch_request_for_action_session(
        conn,
        command_session_id,
        action_session_id,
        &request_id,
    ) {
        let mut pending = pending;
        if pending
            .request_stage_pause_state()
            .is_some_and(|chain| !chain.remaining_sessions.is_empty())
        {
            pending.apply_request_stage_continue_modifications(
                parsed_url,
                command.method.clone(),
                command.post_data.clone(),
                command.headers.clone(),
            );
            let Some(event) =
                super::subresource::next_chained_subresource_request_pause_event(conn, pending)
            else {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "RequestNotFound",
                ));
            };
            let mut plan = CommandOutputPlan::default();
            plan.push_success();
            plan.push_background_event(event);
            return FetchCommandTaskStep::Complete(plan);
        }
        let (chain_url, chain_method, chain_body, chain_headers) =
            pending.accumulated_request_stage_continue_modifications();
        let parsed_url = parsed_url.or(chain_url);
        let method = command.method.clone().or(chain_method);
        let post_data = if command.post_data.is_some() {
            command.post_data.clone().map(Some)
        } else {
            chain_body
        };
        let headers = command.headers.clone().or(chain_headers);
        pending.request_stage_chain = None;
        let configured_response_stage = conn
            .target_fetch_subresource_interception_snapshot_for_session_owner(command_session_id)
            .is_some_and(|snapshot| {
                snapshot.has_response_stage_candidate(pending.resource_type.into())
            });
        let intercept_response = command.intercept_response || configured_response_stage;
        if let Some(continuation) = pending.detached_parser_script_fetch_continuation() {
            if !continuation.continue_request(parsed_url) {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "RequestNotFound",
                ));
            }
            return FetchCommandTaskStep::Complete(CommandOutputPlan::success());
        }
        let handle_auth_requests =
            conn.target_fetch_handle_auth_requests_for_session_owner(command_session_id);
        let should_register =
            pending.resource_type != SubresourceResourceType::WebSocket || intercept_response;
        let correlation = match if configured_response_stage && !command.intercept_response {
            PreparedSubresourceCorrelation::prepare_deferred_response_stage(
                conn,
                command_session_id,
                &request_id,
                &pending,
            )
        } else {
            PreparedSubresourceCorrelation::prepare(
                conn,
                command_session_id,
                &request_id,
                &pending,
                should_register,
            )
        } {
            Some(correlation) => correlation,
            None => {
                conn.register_pending_subresource_fetch_request_for_session_owner(
                    command_session_id,
                    request_id,
                    pending,
                );
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "RequestNotFound",
                ));
            }
        };
        let pending_page = match conn.loaded_page_mut_for_protocol_access(command_session_id) {
            Ok(page) => page
                .start_continue_pending_subresource_fetch(
                    pending.internal_id,
                    parsed_url,
                    method,
                    post_data,
                    headers,
                    intercept_response,
                    handle_auth_requests,
                )
                .map_err(|error| format!("subresource fetch continue failed: {error}")),
            Err(message) => Err(message.to_owned()),
        };
        let pending_page = match pending_page {
            Ok(pending_page) => pending_page,
            Err(message) => {
                correlation.rollback(conn, command_session_id);
                conn.register_pending_subresource_fetch_request_for_session_owner(
                    command_session_id,
                    request_id,
                    pending,
                );
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(-32000, message));
            }
        };
        return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
            conn,
            command_id,
            command_session_id,
            PendingFetchCommandKind::ContinueRequest {
                state: Box::new(PendingContinueRequestState::SubresourceFetch { correlation }),
            },
            PendingFetchCommandOperation::Page(pending_page),
        ));
    }
    if let Some(pending) =
        take_pending_navigation(conn, command_session_id, action_session_id, &request_id)
    {
        if !admit_to_browser_owner {
            let mut pending = pending;
            apply_continue_request_to_pending_navigation(conn, &mut pending, parsed_url, command);
            return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                conn,
                command_id,
                command_session_id,
                PendingFetchCommandKind::ContinueRequest {
                    state: Box::new(PendingContinueRequestState::CompatibilityNavigation {
                        pending: Box::new(pending),
                    }),
                },
                PendingFetchCommandOperation::Ready,
            ));
        }
        let Some(page_owner) = page_owner_for_pending_navigation(conn, &pending) else {
            let pending_request_id = pending.fetch_request_id.clone();
            if conn
                .register_pending_fetch_navigation_request_for_session_owner(
                    command_session_id,
                    pending,
                )
                .is_none()
            {
                tracing::error!(
                    request_id = %pending_request_id,
                    "failed to restore paused navigation after Page owner capture failure"
                );
            }
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                "BrowserHostPausedNavigationDecisionAdmissionFailed: PageNotFound",
            ));
        };
        let decision = BrowserPausedNavigationDecision::continue_request(
            parsed_url,
            command.method.clone(),
            command.post_data.clone(),
            command.headers.clone(),
            command.intercept_response,
        );
        let publication = conn.publish_browser_owner_paused_navigation_decision(
            page_owner.clone(),
            pending,
            decision,
            CommandDispatchContext::default(),
        );
        return match publication {
            Ok(pending) => FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                conn,
                command_id,
                command_session_id,
                PendingFetchCommandKind::ContinueRequest {
                    state: Box::new(PendingContinueRequestState::BrowserOwnerNavigationDecision),
                },
                PendingFetchCommandOperation::BrowserOwnerPausedNavigationDecision(pending),
            )),
            Err(failure) => {
                let (error, pending) = failure.into_parts();
                if let Some(pending) = pending {
                    match pending {
                        BrowserOwnerPausedNavigationSidecar::Request(pending) => {
                            restore_pending_navigation_after_owner_publication_failure(
                                conn,
                                &page_owner,
                                command_session_id,
                                pending,
                            );
                        }
                        BrowserOwnerPausedNavigationSidecar::Auth(_) => {
                            tracing::error!(
                                "continueRequest publication returned an auth-navigation sidecar"
                            );
                        }
                        BrowserOwnerPausedNavigationSidecar::Response(_) => {
                            tracing::error!(
                                "continueRequest publication returned a response-navigation sidecar"
                            );
                        }
                    }
                }
                FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    format!("BrowserHostPausedNavigationDecisionAdmissionFailed: {error}"),
                ))
            }
        };
    }
    pending_request_action_output_plan(
        conn,
        command_session_id,
        &request_id,
        command.context.protocol == DevToolsProtocol::Cdp,
    )
    .map_or_else(FetchCommandTaskStep::Complete, |()| {
        FetchCommandTaskStep::Complete(CommandOutputPlan::success())
    })
}

pub(super) async fn complete_continue_request_command_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    completed: CompletedFetchCommandOperation,
    state: PendingContinueRequestState,
    out: &mut FetchCommandOutput,
) {
    match state {
        PendingContinueRequestState::BrowserOwnerNavigationDecision => {
            let Some(completed) = completed.into_browser_owner_paused_navigation_decision() else {
                out.push_error(-32000, "Missing Browser Owner completion");
                return;
            };
            match completed {
                Ok(completed) => out.extend_browser_owner_projection_as_command_response(completed),
                Err(error) => out.push_error(-32000, error),
            }
        }
        PendingContinueRequestState::SubresourceFetch { correlation } => {
            let completed = completed.into_page_completion();
            if let Err(error) = finish_continue_subresource_request(conn, session_id, completed) {
                correlation.rollback(conn, session_id);
                out.push_error(-32000, error);
                return;
            }
            emit_devtools_empty_success(out);
        }
        PendingContinueRequestState::CompatibilityNavigation { pending } => {
            emit_devtools_empty_success(out);
            load_or_pause_navigation_for_auth_as_background_events_async(
                conn, out, *pending, None, None,
            )
            .await;
        }
    }
}

fn apply_continue_request_to_pending_navigation(
    conn: &mut CdpConnection,
    pending: &mut PendingFetchNavigation,
    parsed_url: Option<Url>,
    command: &DevToolsContinueInterceptedRequestCommand,
) {
    if command.intercept_response {
        pending.intercept_response = true;
        pending.response_stage_url_match_policy =
            crate::conn::ResponseStageUrlMatchPolicy::AlreadyMatched;
    }
    if let Some(parsed) = parsed_url {
        pending.navigation.requested_url = parsed;
    }
    if let Some(method) = command.method.clone() {
        pending.navigation.request_method = method;
    }
    if let Some(body) = command.post_data.clone() {
        pending.navigation.set_request_body_text(body);
    }
    if let Some(headers) = command.headers.clone() {
        pending.navigation.request_headers = headers;
    }
    pending.request_cookie_report = page::navigation_cookie_access_report(
        conn,
        &pending.navigation.requested_url,
        &pending.navigation.request_method,
        None,
        pending.navigation.request_load_policy,
        None,
    );
}

fn finish_continue_subresource_request(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    completed: Option<Result<CompletedPageCommand, String>>,
) -> Result<(), String> {
    let completion = completed.ok_or_else(|| "Missing renderer completion".to_owned())??;
    let mut page = conn.loaded_page_mut_for_protocol_access(session_id)?;
    page.finish_continue_pending_subresource_fetch(completion)
        .map(|_| ())
        .map_err(|error| format!("subresource fetch continue failed: {error}"))
}

pub(super) enum PendingFailRequestState {
    BrowserOwnerNavigationDecision,
    CompatibilityNavigation {
        pending: Box<crate::conn::PendingFetchNavigation>,
        error_text: String,
    },
    SubresourceFetch {
        pending: Box<crate::conn::PendingSubresourceFetchRequest>,
    },
    SubresourceResponse {
        pending: Box<crate::conn::PendingSubresourceFetchResponseRequest>,
    },
    ResponseTransfer {
        transfer: Box<crate::conn::PausedDocumentTransfer>,
        error_text: String,
    },
}

pub(super) fn start_fail_request_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> FetchCommandTaskStep {
    let params: FailRequestParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let error_text = navigation_fail_request_error_text(params.error_reason);
    let command =
        build_cdp_fail_intercepted_request_command(conn, cmd, params.request_id, error_text);
    start_devtools_fail_intercepted_request_command(conn, cmd.id, cmd.session_id, command, true)
}

fn build_cdp_fail_intercepted_request_command(
    conn: &CdpConnection,
    cmd: &Cmd<'_>,
    request_id: String,
    error_text: String,
) -> DevToolsFailInterceptedRequestCommand {
    let (browser_context_id, target_id) =
        devtools_fetch_owner_identity_for_session(conn, cmd.session_id);
    DevToolsFailInterceptedRequestCommand {
        context: cmd.devtools_command_context(target_id.as_deref(), browser_context_id.as_deref()),
        request_id: DevToolsRequestId::from(request_id),
        error_text,
    }
}

fn start_devtools_fail_intercepted_request_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    command: DevToolsFailInterceptedRequestCommand,
    admit_to_browser_owner: bool,
) -> FetchCommandTaskStep {
    let validate_request_id = command.context.protocol == DevToolsProtocol::Cdp;
    let request_id = command.request_id.into_string();
    let error_text = command.error_text;
    let action_session_id = action_session_id_for_devtools_context(
        command_session_id,
        command.context.protocol,
        command.context.session_id.as_ref(),
    );

    if let Some(pending) =
        take_pending_navigation(conn, command_session_id, action_session_id, &request_id)
    {
        if !admit_to_browser_owner {
            // Direct typed DevTools commands still execute inside one borrowed
            // CdpConnection future. Until that frontend exposes its own
            // scheduler-visible task step, publishing and awaiting a Browser
            // Host reply here would prevent the Host from borrowing the
            // physical executor. Raw/nested CDP uses the owner path below,
            // including id-less notifications.
            return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                conn,
                command_id,
                command_session_id,
                PendingFetchCommandKind::FailRequest {
                    state: Box::new(PendingFailRequestState::CompatibilityNavigation {
                        pending: Box::new(pending),
                        error_text,
                    }),
                },
                PendingFetchCommandOperation::Ready,
            ));
        }
        let Some(page_owner) = page_owner_for_pending_navigation(conn, &pending) else {
            let pending_request_id = pending.fetch_request_id.clone();
            if conn
                .register_pending_fetch_navigation_request_for_session_owner(
                    command_session_id,
                    pending,
                )
                .is_none()
            {
                tracing::error!(
                    request_id = %pending_request_id,
                    "failed to restore paused navigation after Page owner capture failure"
                );
            }
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                "BrowserHostPausedNavigationDecisionAdmissionFailed: PageNotFound",
            ));
        };
        let publication = conn.publish_browser_owner_paused_navigation_decision(
            page_owner.clone(),
            pending,
            BrowserPausedNavigationDecision::fail(error_text),
            CommandDispatchContext::default(),
        );
        return match publication {
            Ok(pending) => FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                conn,
                command_id,
                command_session_id,
                PendingFetchCommandKind::FailRequest {
                    state: Box::new(PendingFailRequestState::BrowserOwnerNavigationDecision),
                },
                PendingFetchCommandOperation::BrowserOwnerPausedNavigationDecision(pending),
            )),
            Err(failure) => {
                let (error, pending) = failure.into_parts();
                if let Some(pending) = pending {
                    match pending {
                        BrowserOwnerPausedNavigationSidecar::Request(pending) => {
                            restore_pending_navigation_after_owner_publication_failure(
                                conn,
                                &page_owner,
                                command_session_id,
                                pending,
                            );
                        }
                        BrowserOwnerPausedNavigationSidecar::Auth(_) => {
                            tracing::error!(
                                "failRequest publication returned an auth-navigation sidecar"
                            );
                        }
                        BrowserOwnerPausedNavigationSidecar::Response(_) => {
                            tracing::error!(
                                "failRequest publication returned a response-navigation sidecar"
                            );
                        }
                    }
                }
                FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    format!("BrowserHostPausedNavigationDecisionAdmissionFailed: {error}"),
                ))
            }
        };
    }

    if let Some(pending) = take_pending_subresource_fetch_request_for_action_session(
        conn,
        command_session_id,
        action_session_id,
        &request_id,
    ) {
        let pending = pending;
        if let Some(continuation) = pending.detached_parser_script_fetch_continuation() {
            if !continuation.fail(error_text.clone()) {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "RequestNotFound",
                ));
            }
            let mut plan = CommandOutputPlan::success();
            let mut events = Vec::new();
            let loader_id = conn
                .current_document_loader_id_for_session_owner(command_session_id)
                .unwrap_or_else(|| DEFAULT_LOADER_ID.to_owned());
            network::emit_loading_failed(
                &mut events,
                command_session_id,
                &pending.network_request_id,
                &pending.frame_id,
                &loader_id,
                monotonic_timestamp_seconds(),
                &error_text,
                pending.resource_type.into(),
            );
            plan.extend_background_events(events);
            return FetchCommandTaskStep::Complete(plan);
        }
        let page = match conn.loaded_page_mut_for_protocol_access(command_session_id) {
            Ok(page) => page,
            Err(message) if message == "NoDocumentLoaded" => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "NoDocumentLoaded",
                ));
            }
            Err(message) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(-32000, message));
            }
        };
        let pending_page = match page
            .start_fail_pending_subresource_fetch(pending.internal_id, error_text.clone())
        {
            Ok(pending_page) => pending_page,
            Err(error) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    format!("subresource fetch fail failed: {error}"),
                ));
            }
        };
        return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
            conn,
            command_id,
            command_session_id,
            PendingFetchCommandKind::FailRequest {
                state: Box::new(PendingFailRequestState::SubresourceFetch {
                    pending: Box::new(pending),
                }),
            },
            PendingFetchCommandOperation::Page(pending_page),
        ));
    }

    if let Some(pending) = take_pending_subresource_response_request_for_action_session(
        conn,
        command_session_id,
        action_session_id,
        &request_id,
    ) {
        let page = match conn.loaded_page_mut_for_protocol_access(command_session_id) {
            Ok(page) => page,
            Err(message) if message == "NoDocumentLoaded" => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "NoDocumentLoaded",
                ));
            }
            Err(message) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(-32000, message));
            }
        };
        let pending_page = match page
            .start_fail_pending_subresource_response(pending.internal_id, error_text.clone())
        {
            Ok(pending_page) => pending_page,
            Err(error) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    format!("subresource response fail failed: {error}"),
                ));
            }
        };
        return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
            conn,
            command_id,
            command_session_id,
            PendingFetchCommandKind::FailRequest {
                state: Box::new(PendingFailRequestState::SubresourceResponse {
                    pending: Box::new(pending),
                }),
            },
            PendingFetchCommandOperation::Page(pending_page),
        ));
    }

    if let Some(transfer) = conn
        .take_pending_fetch_response_transfer_for_terminal_action_for_session_owner(
            command_session_id,
            &request_id,
        )
    {
        if !admit_to_browser_owner {
            return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                conn,
                command_id,
                command_session_id,
                PendingFetchCommandKind::FailRequest {
                    state: Box::new(PendingFailRequestState::ResponseTransfer {
                        transfer: Box::new(transfer),
                        error_text,
                    }),
                },
                PendingFetchCommandOperation::Ready,
            ));
        }
        let Some(page_owner) = page_owner_for_navigation_state(conn, transfer.navigation()) else {
            let restored = conn.register_pending_fetch_response_transfer_for_session_owner(
                command_session_id,
                request_id,
                transfer,
            );
            if !restored {
                tracing::error!(
                    "failed to restore failed response navigation after Page owner capture failure"
                );
            }
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                "BrowserHostPausedNavigationDecisionAdmissionFailed: PageNotFound",
            ));
        };
        let publication = conn.publish_browser_owner_paused_navigation_response_decision(
            page_owner.clone(),
            transfer,
            BrowserPausedNavigationDecision::fail(error_text),
            CommandDispatchContext::default(),
        );
        return match publication {
            Ok(pending) => FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                conn,
                command_id,
                command_session_id,
                PendingFetchCommandKind::FailRequest {
                    state: Box::new(PendingFailRequestState::BrowserOwnerNavigationDecision),
                },
                PendingFetchCommandOperation::BrowserOwnerPausedNavigationDecision(pending),
            )),
            Err(failure) => {
                let (error, pending) = failure.into_parts();
                if let Some(pending) = pending {
                    match pending {
                        BrowserOwnerPausedNavigationSidecar::Response(transfer) => {
                            restore_pending_response_transfer_after_owner_publication_failure(
                                conn,
                                &page_owner,
                                command_session_id,
                                *transfer,
                            );
                        }
                        BrowserOwnerPausedNavigationSidecar::Request(_) => {
                            tracing::error!(
                                "failRequest publication returned a request-navigation sidecar"
                            );
                        }
                        BrowserOwnerPausedNavigationSidecar::Auth(_) => {
                            tracing::error!(
                                "failRequest publication returned an auth-navigation sidecar"
                            );
                        }
                    }
                }
                FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    format!("BrowserHostPausedNavigationDecisionAdmissionFailed: {error}"),
                ))
            }
        };
    }

    pending_request_action_output_plan(conn, command_session_id, &request_id, validate_request_id)
        .map_or_else(FetchCommandTaskStep::Complete, |()| {
            FetchCommandTaskStep::Complete(CommandOutputPlan::success())
        })
}

pub(super) async fn complete_fail_request_command_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    completed: CompletedFetchCommandOperation,
    state: PendingFailRequestState,
    out: &mut FetchCommandOutput,
) {
    match state {
        PendingFailRequestState::BrowserOwnerNavigationDecision => {
            let Some(completed) = completed.into_browser_owner_paused_navigation_decision() else {
                out.push_error(-32000, "Missing Browser Owner completion");
                return;
            };
            match completed {
                Ok(completed) => out.extend_browser_owner_projection_as_command_response(completed),
                Err(error) => out.push_error(-32000, error),
            }
        }
        PendingFailRequestState::CompatibilityNavigation {
            pending,
            error_text,
        } => {
            let pending = *pending;
            emit_devtools_empty_success(out);
            let token = pending.document_navigation_token;
            let navigation_state = pending.navigation;
            let navigation = network::materialize_navigation_load_result(
                conn,
                &navigation_state,
                Err(error_text),
            );
            complete_tokened_materialized_navigation_as_background_events_async(
                conn,
                out,
                token,
                navigation_state,
                navigation,
            )
            .await;
        }
        PendingFailRequestState::SubresourceFetch { pending } => {
            let Some(completed) = completed.into_page_completion() else {
                out.push_error(-32000, "Missing renderer completion");
                return;
            };
            let completion = match completed {
                Ok(completion) => completion,
                Err(error) => {
                    out.push_error(-32000, error);
                    return;
                }
            };
            match conn.loaded_page_mut_for_protocol_access(session_id) {
                Ok(mut page) => {
                    if let Err(error) = page.finish_fail_pending_subresource_fetch(completion) {
                        out.push_error(-32000, format!("subresource fetch fail failed: {error}"));
                        return;
                    }
                }
                Err(message) if message == "NoDocumentLoaded" => {
                    out.push_error(-32000, "NoDocumentLoaded");
                    return;
                }
                Err(message) => {
                    out.push_error(-32000, message);
                    return;
                }
            }
            emit_devtools_empty_success(out);
            let mut events = Vec::new();
            activity::flush_post_subresource_fetch_request_activity_background_events_async(
                conn,
                &mut events,
                session_id,
                &pending,
            )
            .await;
            out.extend_background_events(events);
        }
        PendingFailRequestState::SubresourceResponse { pending } => {
            let Some(completed) = completed.into_page_completion() else {
                out.push_error(-32000, "Missing renderer completion");
                return;
            };
            let completion = match completed {
                Ok(completion) => completion,
                Err(error) => {
                    out.push_error(-32000, error);
                    return;
                }
            };
            match conn.loaded_page_mut_for_protocol_access(session_id) {
                Ok(mut page) => {
                    if let Err(error) = page.finish_fail_pending_subresource_response(completion) {
                        out.push_error(
                            -32000,
                            format!("subresource response fail failed: {error}"),
                        );
                        return;
                    }
                }
                Err(message) if message == "NoDocumentLoaded" => {
                    out.push_error(-32000, "NoDocumentLoaded");
                    return;
                }
                Err(message) => {
                    out.push_error(-32000, message);
                    return;
                }
            }
            emit_devtools_empty_success(out);
            let mut events = Vec::new();
            activity::flush_post_subresource_response_activity_background_events_async(
                conn,
                &mut events,
                session_id,
                &pending,
            )
            .await;
            out.extend_background_events(events);
        }
        PendingFailRequestState::ResponseTransfer {
            transfer,
            error_text,
        } => {
            let transfer = *transfer;
            let (token, navigation_state, navigation) = transfer.fail(error_text);
            let navigation =
                network::materialize_navigation_load_result(conn, &navigation_state, navigation);
            emit_devtools_empty_success(out);
            complete_tokened_materialized_navigation_as_background_events_async(
                conn,
                out,
                token,
                navigation_state,
                navigation,
            )
            .await;
        }
    }
}

fn page_owner_for_pending_navigation(
    conn: &mut CdpConnection,
    pending: &PendingFetchNavigation,
) -> Option<TargetPageResidenceIdentity> {
    page_owner_for_navigation_state(conn, &pending.navigation)
}

fn page_owner_for_navigation_state(
    conn: &mut CdpConnection,
    navigation: &crate::conn::NavigationDispatchState,
) -> Option<TargetPageResidenceIdentity> {
    let route = conn
        .target_session_route_for_target_id(&navigation.frame_id)
        .or_else(|| conn.session_route(navigation.navigate_session_id.as_deref()))?;
    let mut owner_scope = conn.scoped_none_session_owner_route_override(route);
    owner_scope
        .conn_mut()
        .target_page_residence_identity_for_session(None)
}

fn restore_pending_navigation_after_owner_publication_failure(
    conn: &mut CdpConnection,
    page_owner: &TargetPageResidenceIdentity,
    fallback_session_id: Option<&str>,
    pending: PendingFetchNavigation,
) {
    let restored = if let Some(route) = conn.target_page_owner_route_if_current(page_owner) {
        let mut owner_scope = conn.scoped_none_session_owner_route_override(route);
        owner_scope
            .conn_mut()
            .register_pending_fetch_navigation_request_for_session_owner(None, pending)
    } else {
        conn.register_pending_fetch_navigation_request_for_session_owner(
            fallback_session_id,
            pending,
        )
    };
    if restored.is_none() {
        tracing::error!(
            target_id = page_owner.target_id(),
            "failed to restore paused navigation after Browser Host publication failure"
        );
    }
}

fn restore_pending_response_transfer_after_owner_publication_failure(
    conn: &mut CdpConnection,
    page_owner: &TargetPageResidenceIdentity,
    fallback_session_id: Option<&str>,
    transfer: crate::conn::PausedDocumentTransfer,
) {
    let request_id = transfer.fetch_request_id().to_owned();
    let restored = if let Some(route) = conn.target_page_owner_route_if_current(page_owner) {
        let mut owner_scope = conn.scoped_none_session_owner_route_override(route);
        owner_scope
            .conn_mut()
            .register_pending_fetch_response_transfer_for_session_owner(None, request_id, transfer)
    } else {
        conn.register_pending_fetch_response_transfer_for_session_owner(
            fallback_session_id,
            request_id,
            transfer,
        )
    };
    if !restored {
        tracing::error!(
            target_id = page_owner.target_id(),
            "failed to restore response-paused navigation after Browser Host publication failure"
        );
    }
}

pub(super) enum PendingFulfillRequestState {
    BrowserOwnerNavigationDecision,
    SubresourceFetch {
        pending: Box<crate::conn::PendingSubresourceFetchRequest>,
        request_id: String,
        network_request_id: String,
        websocket_socket_id: Option<u64>,
        register_synthetic_websocket: bool,
    },
    SubresourceResponse {
        pending: Box<crate::conn::PendingSubresourceFetchResponseRequest>,
    },
    Navigation {
        pending: Box<crate::conn::PendingFetchNavigation>,
        response_code: u16,
        response_headers: Vec<(String, String)>,
        decoded_body: Option<RendererSyntheticResponseBody>,
    },
    ResponseTransfer {
        transfer: Box<crate::conn::PausedDocumentTransfer>,
        response_code: u16,
        response_headers: Vec<(String, String)>,
        decoded_body: Option<RendererSyntheticResponseBody>,
    },
}

pub(super) enum PendingWebSocketCommandOperation {
    DispatchText,
    DispatchBinary,
    Close,
}

pub(super) fn start_fulfill_request_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> FetchCommandTaskStep {
    let params: FulfillRequestParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let body = match params.body {
        Some(body) => match decode_base64_bytes(&body) {
            Ok(bytes) => Some(bytes),
            Err(()) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32602,
                    "InvalidParams",
                ));
            }
        },
        None => None,
    };
    let response_headers =
        match response_headers_from_params(params.response_headers, params.binary_response_headers)
        {
            Ok(headers) => headers,
            Err(()) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32602,
                    "InvalidParams",
                ));
            }
        };
    let response_code = params.response_code.unwrap_or(200);
    let command = build_cdp_fulfill_intercepted_request_command(
        conn,
        cmd,
        params.request_id,
        response_code,
        response_headers,
        body,
        params.response_phrase,
    );
    start_devtools_fulfill_intercepted_request_command(conn, cmd.id, cmd.session_id, command, true)
}

fn build_cdp_fulfill_intercepted_request_command(
    conn: &CdpConnection,
    cmd: &Cmd<'_>,
    request_id: String,
    response_code: u16,
    response_headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    response_phrase: Option<String>,
) -> DevToolsFulfillInterceptedRequestCommand {
    let (browser_context_id, target_id) =
        devtools_fetch_owner_identity_for_session(conn, cmd.session_id);
    DevToolsFulfillInterceptedRequestCommand {
        context: cmd.devtools_command_context(target_id.as_deref(), browser_context_id.as_deref()),
        request_id: DevToolsRequestId::from(request_id),
        response_code,
        response_headers,
        body,
        response_phrase,
    }
}

fn start_devtools_fulfill_intercepted_request_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    command: DevToolsFulfillInterceptedRequestCommand,
    admit_to_browser_owner: bool,
) -> FetchCommandTaskStep {
    let validate_request_id = command.context.protocol == DevToolsProtocol::Cdp;
    let request_id = command.request_id.into_string();
    let response_code = command.response_code;
    let response_headers = command.response_headers;
    let response_body = command.body;
    let _ = command.response_phrase;

    let action_session_id = action_session_id_for_devtools_context(
        command_session_id,
        command.context.protocol,
        command.context.session_id.as_ref(),
    );
    if let Some(pending) = take_pending_subresource_fetch_request_for_action_session(
        conn,
        command_session_id,
        action_session_id,
        &request_id,
    ) {
        let pending = pending;
        let decoded_body = response_body.map(RendererSyntheticResponseBody::from_bytes);
        if let Some(continuation) = pending.detached_parser_script_fetch_continuation() {
            let body = decoded_body
                .unwrap_or_else(RendererSyntheticResponseBody::empty)
                .into_body_bytes();
            if !continuation.fulfill(response_code, response_headers, body) {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "RequestNotFound",
                ));
            }
            return FetchCommandTaskStep::Complete(CommandOutputPlan::success());
        }
        let network_request_id = pending.network_request_id.clone();
        let websocket_socket_id = pending.websocket_socket_id;
        let register_synthetic_websocket =
            pending.resource_type == SubresourceResourceType::WebSocket && response_code == 101;
        let page = match conn.loaded_page_mut_for_protocol_access(command_session_id) {
            Ok(page) => page,
            Err(message) if message == "NoDocumentLoaded" => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "NoDocumentLoaded",
                ));
            }
            Err(message) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(-32000, message));
            }
        };
        let pending_page = match page.start_fulfill_pending_subresource_fetch(
            pending.internal_id,
            response_code,
            response_headers.clone(),
            decoded_body.unwrap_or_else(RendererSyntheticResponseBody::empty),
        ) {
            Ok(pending_page) => pending_page,
            Err(error) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    format!("subresource fetch fulfill failed: {error}"),
                ));
            }
        };
        return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
            conn,
            command_id,
            command_session_id,
            PendingFetchCommandKind::FulfillRequest {
                state: Box::new(PendingFulfillRequestState::SubresourceFetch {
                    pending: Box::new(pending),
                    request_id,
                    network_request_id,
                    websocket_socket_id,
                    register_synthetic_websocket,
                }),
            },
            PendingFetchCommandOperation::Page(pending_page),
        ));
    }

    if let Some(pending) = take_pending_subresource_response_request_for_action_session(
        conn,
        command_session_id,
        action_session_id,
        &request_id,
    ) {
        let decoded_body = response_body.map(RendererSyntheticResponseBody::from_bytes);
        let page = match conn.loaded_page_mut_for_protocol_access(command_session_id) {
            Ok(page) => page,
            Err(message) if message == "NoDocumentLoaded" => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "NoDocumentLoaded",
                ));
            }
            Err(message) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(-32000, message));
            }
        };
        let pending_page = match page.start_fulfill_pending_subresource_response(
            pending.internal_id,
            response_code,
            response_headers.clone(),
            decoded_body.unwrap_or_else(RendererSyntheticResponseBody::empty),
        ) {
            Ok(pending_page) => pending_page,
            Err(error) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    format!("subresource response fulfill failed: {error}"),
                ));
            }
        };
        return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
            conn,
            command_id,
            command_session_id,
            PendingFetchCommandKind::FulfillRequest {
                state: Box::new(PendingFulfillRequestState::SubresourceResponse {
                    pending: Box::new(pending),
                }),
            },
            PendingFetchCommandOperation::Page(pending_page),
        ));
    }

    if let Some(pending) =
        take_pending_navigation(conn, command_session_id, action_session_id, &request_id)
    {
        if !admit_to_browser_owner {
            return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                conn,
                command_id,
                command_session_id,
                PendingFetchCommandKind::FulfillRequest {
                    state: Box::new(PendingFulfillRequestState::Navigation {
                        pending: Box::new(pending),
                        response_code,
                        response_headers,
                        decoded_body: response_body.map(RendererSyntheticResponseBody::from_bytes),
                    }),
                },
                PendingFetchCommandOperation::Ready,
            ));
        }
        let Some(page_owner) = page_owner_for_pending_navigation(conn, &pending) else {
            let pending_request_id = pending.fetch_request_id.clone();
            if conn
                .register_pending_fetch_navigation_request_for_session_owner(
                    command_session_id,
                    pending,
                )
                .is_none()
            {
                tracing::error!(
                    request_id = %pending_request_id,
                    "failed to restore fulfilled navigation after Page owner capture failure"
                );
            }
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                "BrowserHostPausedNavigationDecisionAdmissionFailed: PageNotFound",
            ));
        };
        let publication = conn.publish_browser_owner_paused_navigation_decision(
            page_owner.clone(),
            pending,
            BrowserPausedNavigationDecision::fulfill(
                response_code,
                response_headers,
                response_body,
            ),
            CommandDispatchContext::default(),
        );
        return match publication {
            Ok(pending) => FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                conn,
                command_id,
                command_session_id,
                PendingFetchCommandKind::FulfillRequest {
                    state: Box::new(PendingFulfillRequestState::BrowserOwnerNavigationDecision),
                },
                PendingFetchCommandOperation::BrowserOwnerPausedNavigationDecision(pending),
            )),
            Err(failure) => {
                let (error, pending) = failure.into_parts();
                if let Some(pending) = pending {
                    match pending {
                        BrowserOwnerPausedNavigationSidecar::Request(pending) => {
                            restore_pending_navigation_after_owner_publication_failure(
                                conn,
                                &page_owner,
                                command_session_id,
                                pending,
                            );
                        }
                        BrowserOwnerPausedNavigationSidecar::Response(_) => {
                            tracing::error!(
                                "fulfillRequest publication returned a response-navigation sidecar"
                            );
                        }
                        BrowserOwnerPausedNavigationSidecar::Auth(_) => {
                            tracing::error!(
                                "fulfillRequest publication returned an auth-navigation sidecar"
                            );
                        }
                    }
                }
                FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    format!("BrowserHostPausedNavigationDecisionAdmissionFailed: {error}"),
                ))
            }
        };
    }

    if let Some(transfer) = conn
        .take_pending_fetch_response_transfer_for_terminal_action_for_session_owner(
            command_session_id,
            &request_id,
        )
    {
        if !admit_to_browser_owner {
            return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                conn,
                command_id,
                command_session_id,
                PendingFetchCommandKind::FulfillRequest {
                    state: Box::new(PendingFulfillRequestState::ResponseTransfer {
                        transfer: Box::new(transfer),
                        response_code,
                        response_headers,
                        decoded_body: response_body.map(RendererSyntheticResponseBody::from_bytes),
                    }),
                },
                PendingFetchCommandOperation::Ready,
            ));
        }
        let Some(page_owner) = page_owner_for_navigation_state(conn, transfer.navigation()) else {
            let restored = conn.register_pending_fetch_response_transfer_for_session_owner(
                command_session_id,
                request_id,
                transfer,
            );
            if !restored {
                tracing::error!(
                    "failed to restore fulfilled response navigation after Page owner capture failure"
                );
            }
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                "BrowserHostPausedNavigationDecisionAdmissionFailed: PageNotFound",
            ));
        };
        let publication = conn.publish_browser_owner_paused_navigation_response_decision(
            page_owner.clone(),
            transfer,
            BrowserPausedNavigationDecision::fulfill(
                response_code,
                response_headers,
                response_body,
            ),
            CommandDispatchContext::default(),
        );
        return match publication {
            Ok(pending) => FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                conn,
                command_id,
                command_session_id,
                PendingFetchCommandKind::FulfillRequest {
                    state: Box::new(PendingFulfillRequestState::BrowserOwnerNavigationDecision),
                },
                PendingFetchCommandOperation::BrowserOwnerPausedNavigationDecision(pending),
            )),
            Err(failure) => {
                let (error, pending) = failure.into_parts();
                if let Some(pending) = pending {
                    match pending {
                        BrowserOwnerPausedNavigationSidecar::Response(transfer) => {
                            restore_pending_response_transfer_after_owner_publication_failure(
                                conn,
                                &page_owner,
                                command_session_id,
                                *transfer,
                            );
                        }
                        BrowserOwnerPausedNavigationSidecar::Request(_) => {
                            tracing::error!(
                                "fulfillRequest publication returned a request-navigation sidecar"
                            );
                        }
                        BrowserOwnerPausedNavigationSidecar::Auth(_) => {
                            tracing::error!(
                                "fulfillRequest publication returned an auth-navigation sidecar"
                            );
                        }
                    }
                }
                FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    format!("BrowserHostPausedNavigationDecisionAdmissionFailed: {error}"),
                ))
            }
        };
    }

    pending_request_action_output_plan(conn, command_session_id, &request_id, validate_request_id)
        .map_or_else(FetchCommandTaskStep::Complete, |()| {
            FetchCommandTaskStep::Complete(CommandOutputPlan::success())
        })
}

pub(super) async fn complete_fulfill_request_command_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    completed: CompletedFetchCommandOperation,
    state: PendingFulfillRequestState,
    out: &mut FetchCommandOutput,
) {
    match state {
        PendingFulfillRequestState::BrowserOwnerNavigationDecision => {
            let Some(completed) = completed.into_browser_owner_paused_navigation_decision() else {
                out.push_error(-32000, "Missing Browser Owner completion");
                return;
            };
            match completed {
                Ok(completed) => out.extend_browser_owner_projection_as_command_response(completed),
                Err(error) => out.push_error(-32000, error),
            }
        }
        PendingFulfillRequestState::SubresourceFetch {
            pending,
            request_id,
            network_request_id,
            websocket_socket_id,
            register_synthetic_websocket,
        } => {
            let completed = completed.into_page_completion();
            let Some(completed) = completed else {
                out.push_error(-32000, "Missing renderer completion");
                return;
            };
            let completion = match completed {
                Ok(completion) => completion,
                Err(error) => {
                    out.push_error(-32000, error);
                    return;
                }
            };
            match conn.loaded_page_mut_for_protocol_access(session_id) {
                Ok(mut page) => {
                    if let Err(error) = page.finish_fulfill_pending_subresource_fetch(completion) {
                        out.push_error(
                            -32000,
                            format!("subresource fetch fulfill failed: {error}"),
                        );
                        return;
                    }
                }
                Err(message) if message == "NoDocumentLoaded" => {
                    out.push_error(-32000, "NoDocumentLoaded");
                    return;
                }
                Err(message) => {
                    out.push_error(-32000, message);
                    return;
                }
            }
            if register_synthetic_websocket && let Some(socket_id) = websocket_socket_id {
                conn.register_synthetic_websocket_request_for_session_owner(
                    session_id,
                    request_id,
                    network_request_id,
                    socket_id,
                );
            }
            emit_devtools_empty_success(out);
            let mut events = Vec::new();
            activity::flush_post_subresource_fetch_request_activity_background_events_async(
                conn,
                &mut events,
                session_id,
                &pending,
            )
            .await;
            out.extend_background_events(events);
        }
        PendingFulfillRequestState::SubresourceResponse { pending } => {
            let completed = completed.into_page_completion();
            let Some(completed) = completed else {
                out.push_error(-32000, "Missing renderer completion");
                return;
            };
            let completion = match completed {
                Ok(completion) => completion,
                Err(error) => {
                    out.push_error(-32000, error);
                    return;
                }
            };
            match conn.loaded_page_mut_for_protocol_access(session_id) {
                Ok(mut page) => {
                    if let Err(error) = page.finish_fulfill_pending_subresource_response(completion)
                    {
                        out.push_error(
                            -32000,
                            format!("subresource response fulfill failed: {error}"),
                        );
                        return;
                    }
                }
                Err(message) if message == "NoDocumentLoaded" => {
                    out.push_error(-32000, "NoDocumentLoaded");
                    return;
                }
                Err(message) => {
                    out.push_error(-32000, message);
                    return;
                }
            }
            emit_devtools_empty_success(out);
            let mut events = Vec::new();
            activity::flush_post_subresource_response_activity_background_events_async(
                conn,
                &mut events,
                session_id,
                &pending,
            )
            .await;
            out.extend_background_events(events);
        }
        PendingFulfillRequestState::Navigation {
            pending,
            response_code,
            response_headers,
            decoded_body,
        } => {
            let pending = *pending;
            emit_devtools_empty_success(out);
            let token = pending.document_navigation_token;
            let body = CapturedBody::from_optional_renderer_synthetic_response_body(decoded_body);
            let navigation_state = pending.navigation;
            let navigation = conn
                .build_navigation_from_buffered_body_source_for_navigation_async(
                    &navigation_state,
                    navigation_state.requested_url.clone(),
                    response_code,
                    response_headers,
                    body,
                    pending.request_cookie_report,
                    Default::default(),
                    network::MainDocumentBodyProgressSource::default(),
                )
                .await;
            let navigation =
                network::materialize_navigation_load_result(conn, &navigation_state, navigation);
            complete_tokened_materialized_navigation_as_background_events_async(
                conn,
                out,
                token,
                navigation_state,
                navigation,
            )
            .await;
        }
        PendingFulfillRequestState::ResponseTransfer {
            transfer,
            response_code,
            response_headers,
            decoded_body,
        } => {
            let transfer = *transfer;
            emit_devtools_empty_success(out);
            let (token, navigation_state, navigation) = transfer
                .fulfill_synthetic_async(
                    conn,
                    response_code,
                    response_headers,
                    CapturedBody::from_optional_renderer_synthetic_response_body(decoded_body),
                )
                .await;
            let navigation =
                network::materialize_navigation_load_result(conn, &navigation_state, navigation);
            complete_tokened_materialized_navigation_as_background_events_async(
                conn,
                out,
                token,
                navigation_state,
                navigation,
            )
            .await;
        }
    }
}

fn pending_request_action_output_plan(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    request_id: &str,
    validate_request_id: bool,
) -> Result<(), CommandOutputPlan> {
    pending_request_action_result_with_id_validation(
        conn,
        session_id,
        request_id,
        validate_request_id,
    )
}

pub(super) fn start_dispatch_websocket_message_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> FetchCommandTaskStep {
    let params: DispatchWebSocketMessageParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let Some(socket_id) =
        conn.synthetic_websocket_socket_id_for_session_owner(cmd.session_id, &params.request_id)
    else {
        return FetchCommandTaskStep::Complete(CommandOutputPlan::error(-32000, "RequestNotFound"));
    };

    let page = match conn.loaded_page_mut_for_protocol_access(cmd.session_id) {
        Ok(page) => page,
        Err(message) if message == "NoDocumentLoaded" => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                "NoDocumentLoaded",
            ));
        }
        Err(message) => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(-32000, message));
        }
    };
    let (operation, pending_page) = match WebSocketMessageOpcode::parse(&params.opcode) {
        Some(WebSocketMessageOpcode::Text) => (
            PendingWebSocketCommandOperation::DispatchText,
            page.start_receive_synthetic_websocket_text(socket_id, params.data),
        ),
        Some(WebSocketMessageOpcode::Binary) => {
            let bytes = match decode_base64_bytes(&params.data) {
                Ok(bytes) => bytes,
                Err(()) => {
                    return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32602,
                        "InvalidParams",
                    ));
                }
            };
            (
                PendingWebSocketCommandOperation::DispatchBinary,
                page.start_receive_synthetic_websocket_binary(socket_id, bytes),
            )
        }
        None => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let pending = match pending_page {
        Ok(pending) => pending,
        Err(error) => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                error.to_string(),
            ));
        }
    };
    FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
        conn,
        cmd.id,
        cmd.session_id,
        PendingFetchCommandKind::DispatchWebSocketMessage { operation },
        PendingFetchCommandOperation::Page(pending),
    ))
}

pub(super) fn start_close_websocket_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> FetchCommandTaskStep {
    let params: CloseWebSocketParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let Some(socket_id) =
        conn.synthetic_websocket_socket_id_for_session_owner(cmd.session_id, &params.request_id)
    else {
        return FetchCommandTaskStep::Complete(CommandOutputPlan::error(-32000, "RequestNotFound"));
    };

    let page = match conn.loaded_page_mut_for_protocol_access(cmd.session_id) {
        Ok(page) => page,
        Err(message) if message == "NoDocumentLoaded" => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                "NoDocumentLoaded",
            ));
        }
        Err(message) => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(-32000, message));
        }
    };
    let pending = match page.start_close_synthetic_websocket_from_server(
        socket_id,
        params.code,
        params.reason,
    ) {
        Ok(pending) => pending,
        Err(error) => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                error.to_string(),
            ));
        }
    };
    FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
        conn,
        cmd.id,
        cmd.session_id,
        PendingFetchCommandKind::CloseWebSocket,
        PendingFetchCommandOperation::Page(pending),
    ))
}

pub(super) fn complete_websocket_page_command(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    completed: Option<Result<CompletedPageCommand, String>>,
    operation: PendingWebSocketCommandOperation,
    out: &mut FetchCommandOutput,
) {
    let Some(completed) = completed else {
        out.push_error(-32000, "Missing renderer completion");
        return;
    };
    let completion = match completed {
        Ok(completion) => completion,
        Err(error) => {
            out.push_error(-32000, error);
            return;
        }
    };
    let result = match conn.loaded_page_mut_for_protocol_access(session_id) {
        Ok(mut page) => match operation {
            PendingWebSocketCommandOperation::DispatchText => {
                page.finish_receive_synthetic_websocket_text(completion)
            }
            PendingWebSocketCommandOperation::DispatchBinary => {
                page.finish_receive_synthetic_websocket_binary(completion)
            }
            PendingWebSocketCommandOperation::Close => {
                page.finish_close_synthetic_websocket_from_server(completion)
            }
        },
        Err(message) if message == "NoDocumentLoaded" => {
            out.push_error(-32000, "NoDocumentLoaded");
            return;
        }
        Err(message) => {
            out.push_error(-32000, message);
            return;
        }
    };
    match result {
        Ok(()) => out.push_success(),
        Err(error) => out.push_error(-32000, error.to_string()),
    }
}

pub(super) enum PendingContinueResponseState {
    BrowserOwnerNavigationDecision,
    ResponseTransfer {
        request_id: String,
        transfer: Box<crate::conn::PausedDocumentTransfer>,
        response_code: Option<u16>,
        response_headers: Vec<(String, String)>,
    },
    SubresourceResponse {
        pending: Box<crate::conn::PendingSubresourceFetchResponseRequest>,
    },
}

pub(super) fn start_continue_response_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> FetchCommandTaskStep {
    let params: ContinueResponseParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let response_code = match optional_response_code(params.response_code) {
        Ok(code) => code,
        Err(()) => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let response_headers = match response_headers_with_presence_from_params(
        params.response_headers,
        params.binary_response_headers,
    ) {
        Ok(headers) => headers,
        Err(()) => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let command = build_cdp_continue_intercepted_response_command(
        conn,
        cmd,
        params.request_id.as_ref(),
        response_code,
        response_headers,
        params.response_phrase,
    );
    start_devtools_continue_intercepted_response_command(
        conn,
        cmd.id,
        cmd.session_id,
        &command,
        true,
    )
}

fn build_cdp_continue_intercepted_response_command(
    conn: &CdpConnection,
    cmd: &Cmd<'_>,
    request_id: &str,
    response_code: Option<u16>,
    response_headers: Option<Vec<(String, String)>>,
    response_phrase: Option<String>,
) -> DevToolsContinueInterceptedResponseCommand {
    let (browser_context_id, target_id) =
        devtools_fetch_owner_identity_for_session(conn, cmd.session_id);
    DevToolsContinueInterceptedResponseCommand {
        context: cmd.devtools_command_context(target_id.as_deref(), browser_context_id.as_deref()),
        request_id: DevToolsRequestId::from(request_id),
        response_code,
        response_headers,
        response_phrase,
        auth_credentials: None,
    }
}

pub(super) fn devtools_fetch_owner_identity_for_session(
    conn: &CdpConnection,
    session_id: Option<&str>,
) -> (Option<String>, Option<String>) {
    conn.target_owner_identity_for_session(session_id)
        .map(|(browser_context_id, target_id)| (Some(browser_context_id), target_id))
        .unwrap_or((None, None))
}

fn start_devtools_continue_intercepted_response_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    command: &DevToolsContinueInterceptedResponseCommand,
    admit_to_browser_owner: bool,
) -> FetchCommandTaskStep {
    let request_id = command.request_id.as_str().to_owned();
    let response_code = command.response_code;
    let response_headers = command.response_headers.clone();
    let action_session_id = action_session_id_for_devtools_context(
        command_session_id,
        command.context.protocol,
        command.context.session_id.as_ref(),
    );
    if let Some(transfer) = conn
        .take_pending_fetch_response_transfer_for_terminal_action_for_session_owner(
            command_session_id,
            &request_id,
        )
    {
        let _ = &command.response_phrase;
        let transfer_response_headers = response_headers.clone().unwrap_or_default();
        if admit_to_browser_owner {
            let Some(page_owner) = page_owner_for_navigation_state(conn, transfer.navigation())
            else {
                let restored = conn.register_pending_fetch_response_transfer_for_session_owner(
                    command_session_id,
                    request_id,
                    transfer,
                );
                if !restored {
                    tracing::error!(
                        "failed to restore paused response navigation after Page owner capture failure"
                    );
                }
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "BrowserHostPausedNavigationDecisionAdmissionFailed: PageNotFound",
                ));
            };
            let decision = BrowserPausedNavigationDecision::continue_response(
                response_code,
                transfer_response_headers.clone(),
            );
            let publication = conn.publish_browser_owner_paused_navigation_response_decision(
                page_owner.clone(),
                transfer,
                decision,
                CommandDispatchContext::default(),
            );
            return match publication {
                Ok(pending) => FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                    conn,
                    command_id,
                    command_session_id,
                    PendingFetchCommandKind::ContinueResponse {
                        state: Box::new(
                            PendingContinueResponseState::BrowserOwnerNavigationDecision,
                        ),
                    },
                    PendingFetchCommandOperation::BrowserOwnerPausedNavigationDecision(pending),
                )),
                Err(failure) => {
                    let (error, pending) = failure.into_parts();
                    if let Some(pending) = pending {
                        match pending {
                            BrowserOwnerPausedNavigationSidecar::Response(transfer) => {
                                restore_pending_response_transfer_after_owner_publication_failure(
                                    conn,
                                    &page_owner,
                                    command_session_id,
                                    *transfer,
                                );
                            }
                            BrowserOwnerPausedNavigationSidecar::Request(_) => {
                                tracing::error!(
                                    "continueResponse publication returned a request-navigation sidecar"
                                );
                            }
                            BrowserOwnerPausedNavigationSidecar::Auth(_) => {
                                tracing::error!(
                                    "continueResponse publication returned an auth-navigation sidecar"
                                );
                            }
                        }
                    }
                    FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        format!("BrowserHostPausedNavigationDecisionAdmissionFailed: {error}"),
                    ))
                }
            };
        }
        if let Some(sender) =
            conn.background_navigation_completion_sender_for_session_owner(command_session_id)
        {
            match transfer.into_pending_streaming_document_response_navigation() {
                Ok(pending) => {
                    // Chromium ACKs Fetch.continueResponse when the response-stage
                    // pause is released; parser/DCL/body completion continues
                    // asynchronously. Keep streaming main-document bodies on the
                    // same background completion path as Page.navigate so the
                    // command result never waits for body EOF.
                    continue_streaming_document_response_in_background(
                        conn,
                        sender,
                        pending,
                        response_code,
                        transfer_response_headers,
                    );
                    return FetchCommandTaskStep::Complete(CommandOutputPlan::success());
                }
                Err(transfer) => {
                    return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                        conn,
                        command_id,
                        command_session_id,
                        PendingFetchCommandKind::ContinueResponse {
                            state: Box::new(PendingContinueResponseState::ResponseTransfer {
                                request_id,
                                transfer,
                                response_code,
                                response_headers: transfer_response_headers,
                            }),
                        },
                        PendingFetchCommandOperation::Ready,
                    ));
                }
            }
        }
        return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
            conn,
            command_id,
            command_session_id,
            PendingFetchCommandKind::ContinueResponse {
                state: Box::new(PendingContinueResponseState::ResponseTransfer {
                    request_id,
                    transfer: Box::new(transfer),
                    response_code,
                    response_headers: transfer_response_headers,
                }),
            },
            PendingFetchCommandOperation::Ready,
        ));
    }
    if let Some(pending) = take_pending_subresource_response_request_for_action_session(
        conn,
        command_session_id,
        action_session_id,
        &request_id,
    ) {
        if pending.response_body_taken_as_stream {
            conn.register_pending_subresource_fetch_response_request_for_session_owner(
                command_session_id,
                request_id,
                pending,
            );
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                CONTINUE_RESPONSE_AFTER_BODY_TAKEN_ERROR,
            ));
        }
        let has_remaining_response_stage_chain = pending
            .response_stage_pause_state()
            .is_some_and(|chain| !chain.remaining_sessions.is_empty());
        if has_remaining_response_stage_chain && command.auth_credentials.is_none() {
            if response_code.is_none()
                && response_headers.is_none()
                && command.response_phrase.is_none()
            {
                let Some(event) = super::subresource::next_chained_subresource_response_pause_event(
                    conn, pending,
                ) else {
                    return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        "RequestNotFound",
                    ));
                };
                let mut plan = CommandOutputPlan::default();
                plan.push_success();
                plan.push_background_event(event);
                return FetchCommandTaskStep::Complete(plan);
            }
            if let (Some(override_response_code), Some(response_headers)) =
                (response_code, response_headers.clone())
            {
                let mut pending = pending;
                pending.apply_response_head_override(override_response_code, response_headers);
                let Some(event) = super::subresource::next_chained_subresource_response_pause_event(
                    conn, pending,
                ) else {
                    return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        "RequestNotFound",
                    ));
                };
                let mut plan = CommandOutputPlan::default();
                plan.push_success();
                plan.push_background_event(event);
                return FetchCommandTaskStep::Complete(plan);
            }
            if response_code.is_some()
                || response_headers.is_some()
                || command.response_phrase.is_some()
            {
                conn.register_pending_subresource_fetch_response_request_for_session_owner(
                    command_session_id,
                    request_id,
                    pending,
                );
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32602,
                    CONTINUE_RESPONSE_PARTIAL_OVERRIDE_ERROR,
                ));
            }
        }
        let accumulated_response_head_override = pending.response_head_overridden
            && response_code.is_none()
            && response_headers.is_none()
            && command.auth_credentials.is_none();
        let continue_response_code = response_code
            .or_else(|| accumulated_response_head_override.then_some(pending.response_status));
        let continue_response_headers = if accumulated_response_head_override {
            Some(pending.response_headers.clone())
        } else {
            response_headers
        };
        let pending_internal_id = pending.internal_id;
        let page = match conn.loaded_page_mut_for_protocol_access(command_session_id) {
            Ok(page) => page,
            Err(message) if message == "NoDocumentLoaded" => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "NoDocumentLoaded",
                ));
            }
            Err(message) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(-32000, message));
            }
        };
        let pending_page = match page.start_continue_pending_subresource_response(
            pending_internal_id,
            continue_response_code,
            continue_response_headers,
        ) {
            Ok(pending_page) => pending_page,
            Err(error) => {
                return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    format!("subresource response continue failed: {error}"),
                ));
            }
        };
        return FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
            conn,
            command_id,
            command_session_id,
            PendingFetchCommandKind::ContinueResponse {
                state: Box::new(PendingContinueResponseState::SubresourceResponse {
                    pending: Box::new(pending),
                }),
            },
            PendingFetchCommandOperation::Page(pending_page),
        ));
    }
    if let Some(step) = super::auth::start_devtools_continue_with_auth_command_for_pending(
        conn,
        command_id,
        command_session_id,
        &continue_response_auth_command(command),
        false,
    ) {
        return step;
    }
    pending_request_action_output_plan(
        conn,
        command_session_id,
        &request_id,
        command.context.protocol == DevToolsProtocol::Cdp,
    )
    .map_or_else(FetchCommandTaskStep::Complete, |()| {
        FetchCommandTaskStep::Complete(CommandOutputPlan::success())
    })
}

fn continue_response_auth_command(
    command: &DevToolsContinueInterceptedResponseCommand,
) -> DevToolsContinueWithAuthCommand {
    let (action, username, password) = command.auth_credentials.as_ref().map_or(
        (DevToolsAuthChallengeAction::Default, None, None),
        |credentials| {
            (
                DevToolsAuthChallengeAction::ProvideCredentials,
                Some(credentials.username.clone()),
                Some(credentials.password.clone()),
            )
        },
    );
    DevToolsContinueWithAuthCommand {
        context: command.context.clone(),
        request_id: command.request_id.clone(),
        action,
        username,
        password,
    }
}

async fn continue_response_transfer_inline(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    out: &mut FetchCommandOutput,
    request_id: String,
    transfer: crate::conn::PausedDocumentTransfer,
    response_code: Option<u16>,
    response_headers: Vec<(String, String)>,
) {
    match transfer
        .continue_response_async(conn, response_code, response_headers)
        .await
    {
        Ok((document_navigation_token, navigation_state, navigation)) => {
            let navigation =
                network::materialize_navigation_load_result(conn, &navigation_state, navigation);
            emit_devtools_empty_success(out);
            complete_tokened_materialized_navigation_as_background_events_async(
                conn,
                out,
                document_navigation_token,
                navigation_state,
                navigation,
            )
            .await;
        }
        Err(transfer) => {
            conn.register_pending_fetch_response_transfer_for_session_owner(
                session_id, request_id, transfer,
            );
            out.push_error(-32000, "ResponseBodyStreamActive");
        }
    }
}

pub(super) async fn complete_continue_response_command_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    completed: CompletedFetchCommandOperation,
    state: PendingContinueResponseState,
    out: &mut FetchCommandOutput,
) {
    match state {
        PendingContinueResponseState::BrowserOwnerNavigationDecision => {
            let Some(completed) = completed.into_browser_owner_paused_navigation_decision() else {
                out.push_error(-32000, "Missing Browser Owner completion");
                return;
            };
            match completed {
                Ok(completed) => out.extend_browser_owner_projection_as_command_response(completed),
                Err(error) => out.push_error(-32000, error),
            }
        }
        PendingContinueResponseState::ResponseTransfer {
            request_id,
            transfer,
            response_code,
            response_headers,
        } => {
            continue_response_transfer_inline(
                conn,
                session_id,
                out,
                request_id,
                *transfer,
                response_code,
                response_headers,
            )
            .await;
        }
        PendingContinueResponseState::SubresourceResponse { pending } => {
            let pending = *pending;
            let completed = completed.into_page_completion();
            let Some(completed) = completed else {
                out.push_error(-32000, "Missing renderer completion");
                return;
            };
            let completion = match completed {
                Ok(completion) => completion,
                Err(error) => {
                    out.push_error(-32000, error);
                    return;
                }
            };
            match conn.loaded_page_mut_for_protocol_access(session_id) {
                Ok(mut page) => {
                    if let Err(error) =
                        page.finish_continue_pending_subresource_response(completion)
                    {
                        out.push_error(
                            -32000,
                            format!("subresource response continue failed: {error}"),
                        );
                        return;
                    }
                }
                Err(message) if message == "NoDocumentLoaded" => {
                    out.push_error(-32000, "NoDocumentLoaded");
                    return;
                }
                Err(message) => {
                    out.push_error(-32000, message);
                    return;
                }
            }
            emit_devtools_empty_success(out);
            let mut events = Vec::new();
            activity::flush_post_subresource_response_activity_background_events_async(
                conn,
                &mut events,
                session_id,
                &pending,
            )
            .await;
            out.extend_background_events(events);
        }
    }
}

#[cfg(test)]
mod protocol_neutral_tests {
    use crate::devtools_runtime::{AutomationEvent, DevToolsCommand, DevToolsProtocol};
    use moli_core::page::SubresourceResourceType;
    use serde_json::{Value, json};
    use url::Url;

    use crate::conn::{
        BrowserContext, CapturedBody, CdpConnection, Cmd, PendingSubresourceFetchOwnerKind,
        PendingSubresourceFetchRequest, PendingSubresourceFetchRequestStage,
        PendingSubresourceFetchRequestStageChain, PendingSubresourceFetchResponseRequest,
        PendingSubresourceFetchResponseStage, PendingSubresourceFetchResponseStageChain,
    };

    use super::{
        ContinueResponseParams, build_cdp_continue_intercepted_request_command,
        build_cdp_continue_intercepted_response_command,
        build_cdp_fail_intercepted_request_command, build_cdp_fulfill_intercepted_request_command,
        parsed_continue_request_url, start_devtools_fetch_command,
    };

    #[test]
    fn cdp_continue_request_builds_protocol_neutral_intercepted_request_command() {
        let conn = CdpConnection::new();
        let params = json!({
            "requestId": "interception-job-0",
            "url": "https://example.test/next",
            "method": "POST",
            "postData": "aGVsbG8=",
            "headers": [{"name": "x-test", "value": "1"}],
            "interceptResponse": true,
        });
        let cmd = Cmd::for_test(
            Some(20),
            "Fetch.continueRequest",
            &params,
            Some("SID-0"),
            r#"{"id":20,"method":"Fetch.continueRequest"}"#,
        );
        let params = cmd
            .get_params()
            .expect("continueRequest params should parse")
            .expect("continueRequest params should be present");

        let command = build_cdp_continue_intercepted_request_command(&conn, &cmd, params)
            .expect("continueRequest command should normalize");
        let parsed_url =
            parsed_continue_request_url(&command).expect("continueRequest URL should parse");

        assert_eq!(command.context.protocol, DevToolsProtocol::Cdp);
        assert_eq!(
            command.context.session_id.as_ref().map(|id| id.as_str()),
            Some("SID-0")
        );
        assert_eq!(command.context.target_id, None);
        assert_eq!(command.context.browser_context_id, None);
        assert_eq!(command.request_id.as_str(), "interception-job-0");
        assert_eq!(command.url.as_deref(), Some("https://example.test/next"));
        assert_eq!(command.method.as_deref(), Some("POST"));
        assert_eq!(command.post_data.as_deref(), Some("hello"));
        assert_eq!(
            command.headers,
            Some(vec![("x-test".to_owned(), "1".to_owned())])
        );
        assert!(command.intercept_response);
        assert_eq!(
            parsed_url.as_ref().map(|url| url.as_str()),
            Some("https://example.test/next")
        );
    }

    #[test]
    fn cdp_continue_response_builds_protocol_neutral_intercepted_response_command() {
        let conn = CdpConnection::new();
        let params = Value::Null;
        let cmd = Cmd::for_test(
            Some(21),
            "Fetch.continueResponse",
            &params,
            Some("SID-1"),
            r#"{"id":21,"method":"Fetch.continueResponse"}"#,
        );

        let command = build_cdp_continue_intercepted_response_command(
            &conn,
            &cmd,
            "interception-job-1",
            Some(204),
            Some(vec![("x-test".to_owned(), "1".to_owned())]),
            Some("No Content".to_owned()),
        );

        assert_eq!(command.context.protocol, DevToolsProtocol::Cdp);
        assert_eq!(
            command.context.session_id.as_ref().map(|id| id.as_str()),
            Some("SID-1")
        );
        assert_eq!(command.context.target_id, None);
        assert_eq!(command.context.browser_context_id, None);
        assert_eq!(command.request_id.as_str(), "interception-job-1");
        assert_eq!(command.response_code, Some(204));
        assert_eq!(
            command.response_headers,
            Some(vec![("x-test".to_owned(), "1".to_owned())])
        );
        assert_eq!(command.response_phrase.as_deref(), Some("No Content"));
    }

    #[test]
    fn devtools_fetch_entry_routes_continue_request_command() {
        let mut conn = CdpConnection::new();
        let params = json!({"requestId": "missing-request"});
        let cmd = Cmd::for_test(
            Some(24),
            "Fetch.continueRequest",
            &params,
            Some("SID-4"),
            r#"{"id":24,"method":"Fetch.continueRequest"}"#,
        );
        let params = cmd
            .get_params()
            .expect("continueRequest params should parse")
            .expect("continueRequest params should be present");
        let command = build_cdp_continue_intercepted_request_command(&conn, &cmd, params)
            .expect("continueRequest command should normalize");

        let step = start_devtools_fetch_command(
            &mut conn,
            cmd.id,
            cmd.session_id,
            DevToolsCommand::ContinueInterceptedRequest(command),
            false,
        );

        let super::super::FetchCommandTaskStep::Complete(plan) = step else {
            panic!("missing continueRequest should complete with a terminal protocol error");
        };
        let mut out = Vec::new();
        plan.emit_into(&mut out, cmd.id, cmd.session_id);
        assert_eq!(out[0]["id"], json!(24));
        assert!(out[0]["error"].is_object());
    }

    #[test]
    fn cdp_continue_request_repauses_next_matching_fetch_session() {
        let mut conn = CdpConnection::new();
        let mut browser_context = BrowserContext::new("BID-chain".to_owned());
        browser_context.set_active_target_id("TID-chain".to_owned());
        browser_context.attach_active_session("SID-primary".to_owned());
        assert!(
            browser_context.assign_auxiliary_session_to_target("TID-chain", "SID-aux".to_owned())
        );
        conn.insert_browser_context(browser_context);

        let page_owner = conn
            .target_page_residence_identity_for_session(Some("SID-primary"))
            .expect("test target should expose a Page residence identity");
        let pending = PendingSubresourceFetchRequest {
            residence: crate::conn::PendingSubresourceFetchResidence::InstalledPage(page_owner),
            owner_session_id: Some("SID-primary".to_owned()),
            action_session_id: Some("SID-primary".to_owned()),
            owner_kind: PendingSubresourceFetchOwnerKind::Fetch,
            internal_id: 77,
            network_request_id: "NETWORK-77".to_owned(),
            network_request_handle: None,
            frame_id: "FRAME-77".to_owned(),
            document_url: Url::parse("https://example.test/page").unwrap(),
            resource_type: SubresourceResourceType::Fetch,
            websocket_socket_id: None,
            request_stage_chain: Some(Box::new(PendingSubresourceFetchRequestStageChain {
                url: Url::parse("https://example.test/api").unwrap(),
                method: "GET".to_owned(),
                headers: vec![("x-old".to_owned(), "1".to_owned())],
                body: None,
                request_cookie_report: None,
                remaining_sessions: vec![PendingSubresourceFetchRequestStage {
                    session_id: Some("SID-aux".to_owned()),
                    owner_kind: PendingSubresourceFetchOwnerKind::Fetch,
                    request_id: "FETCH-aux".to_owned(),
                    blocked_intercepts: Vec::new(),
                }],
            })),
        };
        assert!(
            conn.register_pending_subresource_fetch_request_for_session_owner(
                Some("SID-primary"),
                "FETCH-primary".to_owned(),
                pending,
            )
        );

        let params = json!({
            "requestId": "FETCH-primary",
            "url": "https://example.test/rewritten",
            "method": "POST",
            "postData": "Ym9keQ==",
            "headers": [{"name": "x-new", "value": "2"}],
        });
        let cmd = Cmd::for_test(
            Some(31),
            "Fetch.continueRequest",
            &params,
            Some("SID-primary"),
            r#"{"id":31,"method":"Fetch.continueRequest"}"#,
        );
        let params = cmd
            .get_params()
            .expect("continueRequest params should parse")
            .expect("continueRequest params should be present");
        let command = build_cdp_continue_intercepted_request_command(&conn, &cmd, params)
            .expect("continueRequest command should normalize");

        let step = start_devtools_fetch_command(
            &mut conn,
            cmd.id,
            cmd.session_id,
            DevToolsCommand::ContinueInterceptedRequest(command),
            false,
        );

        let super::super::FetchCommandTaskStep::Complete(plan) = step else {
            panic!("chained continueRequest should complete without a page command");
        };
        let events = plan.into_background_events(cmd.id, cmd.session_id);
        assert_eq!(events.len(), 2);
        let (paused_message, paused_sidecar) = events[1].clone().into_parts();
        let Some(AutomationEvent::RequestPaused(paused_sidecar)) = paused_sidecar else {
            panic!("chained request pause should carry a typed automation sidecar");
        };
        assert_eq!(paused_sidecar.request_id.as_str(), "FETCH-aux");
        assert_eq!(
            paused_sidecar.blocked_intercepts,
            Vec::<crate::devtools_runtime::DevToolsNetworkInterceptId>::new()
        );
        assert_eq!(
            paused_sidecar.network_id.as_ref().map(|id| id.as_str()),
            Some("NETWORK-77")
        );
        let out = events
            .into_iter()
            .map(crate::conn::BackgroundProtocolEvent::into_protocol_message)
            .collect::<Vec<_>>();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["id"], json!(31));
        assert_eq!(out[0]["sessionId"], json!("SID-primary"));
        assert_eq!(out[1]["method"], json!("Fetch.requestPaused"));
        assert_eq!(paused_message, out[1]);
        assert_eq!(out[1]["sessionId"], json!("SID-aux"));
        assert_eq!(out[1]["params"]["requestId"], json!("FETCH-aux"));
        assert_eq!(out[1]["params"]["networkId"], json!("NETWORK-77"));
        assert_eq!(
            out[1]["params"]["request"]["url"],
            json!("https://example.test/rewritten")
        );
        assert_eq!(out[1]["params"]["request"]["method"], json!("POST"));
        assert_eq!(out[1]["params"]["request"]["postData"], json!("body"));
        assert_eq!(out[1]["params"]["request"]["headers"]["x-new"], json!("2"));

        assert!(
            conn.take_pending_subresource_fetch_request_for_session_owner(
                Some("SID-primary"),
                "FETCH-aux",
            )
            .is_none(),
            "old owner must not be able to consume the chained request"
        );
        let chained = conn
            .take_pending_subresource_fetch_request_for_session_owner(Some("SID-aux"), "FETCH-aux")
            .expect("next Fetch session should own the chained request");
        let (url, method, body, headers) =
            chained.accumulated_request_stage_continue_modifications();
        assert_eq!(
            url.as_ref().map(|url| url.as_str()),
            Some("https://example.test/rewritten")
        );
        assert_eq!(method.as_deref(), Some("POST"));
        assert_eq!(body, Some(Some("body".to_owned())));
        assert_eq!(headers, Some(vec![("x-new".to_owned(), "2".to_owned())]));
    }

    #[test]
    fn cdp_continue_response_repauses_next_matching_fetch_session() {
        let mut conn = CdpConnection::new();
        let mut browser_context = BrowserContext::new("BID-response-chain".to_owned());
        browser_context.set_active_target_id("TID-response-chain".to_owned());
        browser_context.attach_active_session("SID-primary".to_owned());
        assert!(
            browser_context
                .assign_auxiliary_session_to_target("TID-response-chain", "SID-aux".to_owned())
        );
        conn.insert_browser_context(browser_context);

        let page_owner = conn
            .target_page_residence_identity_for_session(Some("SID-primary"))
            .expect("test target should expose a Page residence identity");
        let pending = PendingSubresourceFetchResponseRequest {
            page_owner,
            owner_session_id: Some("SID-primary".to_owned()),
            action_session_id: Some("SID-primary".to_owned()),
            owner_kind: PendingSubresourceFetchOwnerKind::Fetch,
            internal_id: 79,
            network_request_id: "NETWORK-79".to_owned(),
            network_request_handle: None,
            frame_id: "FRAME-79".to_owned(),
            document_url: Url::parse("https://example.test/page").unwrap(),
            resource_type: SubresourceResourceType::Fetch,
            websocket_socket_id: None,
            url: Url::parse("https://example.test/api").unwrap(),
            method: "GET".to_owned(),
            request_headers: vec![("accept".to_owned(), "application/json".to_owned())],
            request_body: None,
            request_cookie_report: None,
            response_status: 200,
            response_headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            response_head_overridden: false,
            response_body_taken_as_stream: false,
            response_body: CapturedBody::from_bytes(br#"{"ok":true}"#.to_vec()),
            response_stage_chain: Some(Box::new(PendingSubresourceFetchResponseStageChain {
                remaining_sessions: vec![PendingSubresourceFetchResponseStage {
                    session_id: Some("SID-aux".to_owned()),
                    owner_kind: PendingSubresourceFetchOwnerKind::Fetch,
                    request_id: "FETCH-response-aux".to_owned(),
                    blocked_intercepts: Vec::new(),
                }],
            })),
        };
        assert!(
            conn.register_pending_subresource_fetch_response_request_for_session_owner(
                Some("SID-primary"),
                "FETCH-response-primary".to_owned(),
                pending,
            )
        );

        let params = json!({"requestId": "FETCH-response-primary"});
        let cmd = Cmd::for_test(
            Some(33),
            "Fetch.continueResponse",
            &params,
            Some("SID-primary"),
            r#"{"id":33,"method":"Fetch.continueResponse"}"#,
        );
        let params: ContinueResponseParams = cmd
            .get_params()
            .expect("continueResponse params should parse")
            .expect("continueResponse params should be present");
        let command = build_cdp_continue_intercepted_response_command(
            &conn,
            &cmd,
            params.request_id.as_ref(),
            None,
            None,
            None,
        );

        let step = start_devtools_fetch_command(
            &mut conn,
            cmd.id,
            cmd.session_id,
            DevToolsCommand::ContinueInterceptedResponse(command),
            false,
        );

        let super::super::FetchCommandTaskStep::Complete(plan) = step else {
            panic!("chained continueResponse should complete without a page command");
        };
        let events = plan.into_background_events(cmd.id, cmd.session_id);
        assert_eq!(events.len(), 2);
        let (paused_message, paused_sidecar) = events[1].clone().into_parts();
        let Some(AutomationEvent::RequestPaused(paused_sidecar)) = paused_sidecar else {
            panic!("chained response pause should carry a typed automation sidecar");
        };
        assert_eq!(paused_sidecar.request_id.as_str(), "FETCH-response-aux");
        assert_eq!(paused_sidecar.status, Some(200));
        assert_eq!(
            paused_sidecar.network_id.as_ref().map(|id| id.as_str()),
            Some("NETWORK-79")
        );
        let out = events
            .into_iter()
            .map(crate::conn::BackgroundProtocolEvent::into_protocol_message)
            .collect::<Vec<_>>();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["id"], json!(33));
        assert_eq!(out[0]["sessionId"], json!("SID-primary"));
        assert_eq!(out[1]["method"], json!("Fetch.requestPaused"));
        assert_eq!(paused_message, out[1]);
        assert_eq!(out[1]["sessionId"], json!("SID-aux"));
        assert_eq!(out[1]["params"]["requestId"], json!("FETCH-response-aux"));
        assert_eq!(out[1]["params"]["networkId"], json!("NETWORK-79"));
        assert_eq!(out[1]["params"]["responseStatusCode"], json!(200));
        assert_eq!(
            out[1]["params"]["responseHeaders"][0],
            json!({"name": "content-type", "value": "application/json"})
        );

        assert!(
            conn.take_pending_subresource_fetch_response_request_for_action_session_owner(
                Some("SID-primary"),
                Some("SID-primary"),
                "FETCH-response-aux",
            )
            .is_none(),
            "old owner action session must not be able to resolve the chained response"
        );
        let chained = conn
            .take_pending_subresource_fetch_response_request_for_action_session_owner(
                Some("SID-aux"),
                Some("SID-aux"),
                "FETCH-response-aux",
            )
            .expect("next Fetch session should own the chained response");
        assert_eq!(chained.owner_session_id.as_deref(), Some("SID-aux"));
        assert_eq!(chained.action_session_id.as_deref(), Some("SID-aux"));
        assert_eq!(chained.response_status, 200);
    }

    #[test]
    fn cdp_continue_request_repauses_network_or_bidi_stage_on_target_owner_route() {
        let mut conn = CdpConnection::new();
        let mut browser_context = BrowserContext::new("BID-chain-bidi".to_owned());
        browser_context.set_active_target_id("TID-chain-bidi".to_owned());
        browser_context.attach_active_session("SID-primary".to_owned());
        conn.insert_browser_context(browser_context);

        let page_owner = conn
            .target_page_residence_identity_for_session(Some("SID-primary"))
            .expect("test target should expose a Page residence identity");
        let pending = PendingSubresourceFetchRequest {
            residence: crate::conn::PendingSubresourceFetchResidence::InstalledPage(page_owner),
            owner_session_id: Some("SID-primary".to_owned()),
            action_session_id: Some("SID-primary".to_owned()),
            owner_kind: PendingSubresourceFetchOwnerKind::Fetch,
            internal_id: 88,
            network_request_id: "NETWORK-88".to_owned(),
            network_request_handle: None,
            frame_id: "FRAME-88".to_owned(),
            document_url: Url::parse("https://example.test/page").unwrap(),
            resource_type: SubresourceResourceType::Fetch,
            websocket_socket_id: None,
            request_stage_chain: Some(Box::new(PendingSubresourceFetchRequestStageChain {
                url: Url::parse("https://example.test/api").unwrap(),
                method: "GET".to_owned(),
                headers: Vec::new(),
                body: None,
                request_cookie_report: None,
                remaining_sessions: vec![PendingSubresourceFetchRequestStage {
                    session_id: Some("BIDI-SID".to_owned()),
                    owner_kind: PendingSubresourceFetchOwnerKind::NetworkOrBidi,
                    request_id: "FETCH-bidi".to_owned(),
                    blocked_intercepts: Vec::new(),
                }],
            })),
        };
        assert!(
            conn.register_pending_subresource_fetch_request_for_session_owner(
                Some("SID-primary"),
                "FETCH-primary".to_owned(),
                pending,
            )
        );

        let params = json!({
            "requestId": "FETCH-primary",
            "url": "https://example.test/rewritten",
        });
        let cmd = Cmd::for_test(
            Some(32),
            "Fetch.continueRequest",
            &params,
            Some("SID-primary"),
            r#"{"id":32,"method":"Fetch.continueRequest"}"#,
        );
        let params = cmd
            .get_params()
            .expect("continueRequest params should parse")
            .expect("continueRequest params should be present");
        let command = build_cdp_continue_intercepted_request_command(&conn, &cmd, params)
            .expect("continueRequest command should normalize");

        let step = start_devtools_fetch_command(
            &mut conn,
            cmd.id,
            cmd.session_id,
            DevToolsCommand::ContinueInterceptedRequest(command),
            false,
        );

        let super::super::FetchCommandTaskStep::Complete(plan) = step else {
            panic!("chained Network/BiDi continueRequest should complete without a page command");
        };
        let mut out = Vec::new();
        plan.emit_into(&mut out, cmd.id, cmd.session_id);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1]["method"], json!("Fetch.requestPaused"));
        assert_eq!(out[1]["sessionId"], json!("BIDI-SID"));
        assert_eq!(out[1]["params"]["requestId"], json!("FETCH-bidi"));
        assert_eq!(out[1]["params"]["networkId"], json!("NETWORK-88"));

        assert!(
            conn.take_pending_subresource_fetch_request_for_session_owner(
                Some("BIDI-SID"),
                "FETCH-bidi",
            )
            .is_none(),
            "BiDi-only session id has no CDP route and must not own the stored pending request"
        );
        let chained = conn
            .take_pending_subresource_fetch_request_for_action_session_owner(
                Some("SID-primary"),
                Some("BIDI-SID"),
                "FETCH-bidi",
            )
            .expect("target owner route should retain the chained Network/BiDi pending request");
        assert_eq!(
            chained.owner_session_id.as_deref(),
            Some("SID-primary"),
            "stored owner remains routable even though the protocol event targets BIDI-SID"
        );
        assert_eq!(
            chained.action_session_id.as_deref(),
            Some("BIDI-SID"),
            "BiDi event session is the session allowed to resolve the chained request"
        );
        assert_eq!(
            chained.owner_kind,
            PendingSubresourceFetchOwnerKind::NetworkOrBidi
        );
    }

    #[test]
    fn cdp_fail_request_builds_protocol_neutral_intercepted_request_command() {
        let conn = CdpConnection::new();
        let params = Value::Null;
        let cmd = Cmd::for_test(
            Some(22),
            "Fetch.failRequest",
            &params,
            Some("SID-2"),
            r#"{"id":22,"method":"Fetch.failRequest"}"#,
        );

        let command = build_cdp_fail_intercepted_request_command(
            &conn,
            &cmd,
            "interception-job-2".to_owned(),
            "net::ERR_BLOCKED_BY_CLIENT".to_owned(),
        );

        assert_eq!(command.context.protocol, DevToolsProtocol::Cdp);
        assert_eq!(
            command.context.session_id.as_ref().map(|id| id.as_str()),
            Some("SID-2")
        );
        assert_eq!(command.context.target_id, None);
        assert_eq!(command.context.browser_context_id, None);
        assert_eq!(command.request_id.as_str(), "interception-job-2");
        assert_eq!(command.error_text, "net::ERR_BLOCKED_BY_CLIENT");
    }

    #[test]
    fn cdp_fulfill_request_builds_protocol_neutral_intercepted_request_command() {
        let conn = CdpConnection::new();
        let params = Value::Null;
        let cmd = Cmd::for_test(
            Some(23),
            "Fetch.fulfillRequest",
            &params,
            Some("SID-3"),
            r#"{"id":23,"method":"Fetch.fulfillRequest"}"#,
        );

        let command = build_cdp_fulfill_intercepted_request_command(
            &conn,
            &cmd,
            "interception-job-3".to_owned(),
            204,
            vec![("x-test".to_owned(), "yes".to_owned())],
            Some(vec![1, 2, 3]),
            Some("No Content".to_owned()),
        );

        assert_eq!(command.context.protocol, DevToolsProtocol::Cdp);
        assert_eq!(
            command.context.session_id.as_ref().map(|id| id.as_str()),
            Some("SID-3")
        );
        assert_eq!(command.context.target_id, None);
        assert_eq!(command.context.browser_context_id, None);
        assert_eq!(command.request_id.as_str(), "interception-job-3");
        assert_eq!(command.response_code, 204);
        assert_eq!(
            command.response_headers,
            vec![("x-test".to_owned(), "yes".to_owned())]
        );
        assert_eq!(command.body, Some(vec![1, 2, 3]));
        assert_eq!(command.response_phrase.as_deref(), Some("No Content"));
    }
}
