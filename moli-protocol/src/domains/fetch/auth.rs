use crate::conn::{
    BackgroundProtocolEvent, BrowserOwnerPausedNavigationSidecar, CdpConnection, Cmd,
    CommandDispatchContext, PendingFetchAuthNavigation, PendingFetchNavigation,
    PendingSubresourceFetchAuthRequest, PendingSubresourceFetchRequest,
    TargetPageResidenceIdentity,
};
use crate::devtools_runtime::{
    DevToolsAuthChallengeAction, DevToolsContinueWithAuthCommand, DevToolsProtocol,
    DevToolsRequestId,
};
use crate::domains::command_output::CommandOutputPlan;
use crate::domains::{activity, network};
use moli_core::browser_host::BrowserPausedNavigationDecision;
use moli_core::page::{CompletedPageCommand, SubresourceAuthCredentials};

use super::helpers::{
    pending_fetch_auth_navigation_required_event, pending_subresource_auth_required_event,
    request_auth_for_challenge,
};
use super::navigation::{
    complete_tokened_materialized_navigation_as_background_events_async,
    load_or_pause_navigation_for_auth_as_background_events_async,
};
use super::params::{AuthChallengeResponseResponse, ContinueWithAuthParams};
use super::state::{
    PreparedSubresourceCorrelation, action_session_id_for_devtools_context,
    pending_request_action_output_plan_with_id_validation,
    take_pending_auth_navigation_for_action_session,
    take_pending_subresource_auth_request_for_action_session,
};
use super::{CompletedFetchCommandOperation, PendingFetchCommandOperation};
use super::{
    FetchCommandOutput, FetchCommandTaskStep, PendingFetchCommandDispatch, PendingFetchCommandKind,
};

pub(super) enum PendingContinueWithAuthState {
    BrowserOwnerNavigationDecision,
    SubresourceAuthCancel {
        pending: Box<crate::conn::PendingSubresourceFetchAuthRequest>,
        correlation: Option<PreparedSubresourceCorrelation>,
    },
    SubresourceAuthFail {
        pending: Box<crate::conn::PendingSubresourceFetchAuthRequest>,
    },
    SubresourceAuthContinue {
        correlation: PreparedSubresourceCorrelation,
    },
    NavigationCancel {
        pending: Box<crate::conn::PendingFetchAuthNavigation>,
    },
    NavigationFail {
        pending: Box<crate::conn::PendingFetchAuthNavigation>,
    },
    NavigationContinue {
        pending: Box<crate::conn::PendingFetchAuthNavigation>,
        auth: SubresourceAuthCredentials,
    },
}

pub(super) fn start_continue_with_auth_command(
    conn: &mut CdpConnection,
    cmd: &Cmd<'_>,
) -> FetchCommandTaskStep {
    let params: ContinueWithAuthParams = match cmd.get_params() {
        Ok(Some(params)) => params,
        _ => {
            return FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32602,
                "InvalidParams",
            ));
        }
    };
    let command = build_cdp_continue_with_auth_command(conn, cmd, params);
    start_devtools_continue_with_auth_command(conn, cmd.id, cmd.session_id, &command, true)
}

fn build_cdp_continue_with_auth_command(
    conn: &CdpConnection,
    cmd: &Cmd<'_>,
    params: ContinueWithAuthParams,
) -> DevToolsContinueWithAuthCommand {
    let (browser_context_id, target_id) =
        super::commands::devtools_fetch_owner_identity_for_session(conn, cmd.session_id);
    DevToolsContinueWithAuthCommand {
        context: cmd.devtools_command_context(target_id.as_deref(), browser_context_id.as_deref()),
        request_id: DevToolsRequestId::from(params.request_id.as_ref().to_owned()),
        action: devtools_auth_action_from_cdp(params.auth_challenge_response.response),
        username: params.auth_challenge_response.username,
        password: params.auth_challenge_response.password,
    }
}

fn devtools_auth_action_from_cdp(
    action: AuthChallengeResponseResponse,
) -> DevToolsAuthChallengeAction {
    match action {
        AuthChallengeResponseResponse::Default => DevToolsAuthChallengeAction::Default,
        AuthChallengeResponseResponse::CancelAuth => DevToolsAuthChallengeAction::Cancel,
        AuthChallengeResponseResponse::ProvideCredentials => {
            DevToolsAuthChallengeAction::ProvideCredentials
        }
    }
}

pub(super) fn start_devtools_continue_with_auth_command(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    command: &DevToolsContinueWithAuthCommand,
    admit_to_browser_owner: bool,
) -> FetchCommandTaskStep {
    if let Some(step) = start_devtools_continue_with_auth_command_for_pending(
        conn,
        command_id,
        command_session_id,
        command,
        admit_to_browser_owner,
    ) {
        return step;
    }

    FetchCommandTaskStep::Complete(pending_request_action_output_plan_with_id_validation(
        conn,
        command_session_id,
        command.request_id.as_str(),
        command.context.protocol == DevToolsProtocol::Cdp,
    ))
}

pub(super) fn start_devtools_continue_with_auth_command_for_pending(
    conn: &mut CdpConnection,
    command_id: Option<u64>,
    command_session_id: Option<&str>,
    command: &DevToolsContinueWithAuthCommand,
    admit_to_browser_owner: bool,
) -> Option<FetchCommandTaskStep> {
    let request_id = command.request_id.as_str().to_owned();
    let action_session_id = action_session_id_for_devtools_context(
        command_session_id,
        command.context.protocol,
        command.context.session_id.as_ref(),
    );
    if let Some(pending) = take_pending_subresource_auth_request_for_action_session(
        conn,
        command_session_id,
        action_session_id,
        &request_id,
    ) {
        if matches!(command.action, DevToolsAuthChallengeAction::Default)
            && pending
                .auth_stage_pause_state()
                .is_some_and(|chain| !chain.remaining_sessions.is_empty())
        {
            return Some(FetchCommandTaskStep::Complete(
                chained_subresource_auth_required_output_plan(conn, command_session_id, pending)
                    .unwrap_or_else(|| CommandOutputPlan::error(-32000, "RequestNotFound")),
            ));
        }
        match command.action {
            DevToolsAuthChallengeAction::Default | DevToolsAuthChallengeAction::Cancel => {
                let cancel_correlation = if matches!(
                    command.action,
                    DevToolsAuthChallengeAction::Cancel
                ) && pending.intercept_response
                {
                    let continued = continued_subresource_request(&pending);
                    match PreparedSubresourceCorrelation::prepare(
                        conn,
                        command_session_id,
                        &request_id,
                        &continued,
                        true,
                    ) {
                        Some(correlation) => Some(correlation),
                        None => {
                            conn.register_pending_subresource_fetch_auth_request_for_session_owner(
                                command_session_id,
                                request_id,
                                pending,
                            );
                            return Some(FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                                -32000,
                                "RequestNotFound",
                            )));
                        }
                    }
                } else {
                    None
                };
                let pending_page =
                    match conn.loaded_page_mut_for_protocol_access(command_session_id) {
                        Ok(page) => (match command.action {
                            DevToolsAuthChallengeAction::Default => page
                                .start_fail_pending_subresource_auth(
                                    pending.internal_id,
                                    "Fetch auth challenge aborted".to_owned(),
                                ),
                            DevToolsAuthChallengeAction::Cancel => {
                                page.start_cancel_pending_subresource_auth(pending.internal_id)
                            }
                            DevToolsAuthChallengeAction::ProvideCredentials => unreachable!(),
                        })
                        .map_err(|error| error.to_string()),
                        Err(message) => Err(message.to_owned()),
                    };
                let pending_page = match pending_page {
                    Ok(pending_page) => pending_page,
                    Err(error) => {
                        if let Some(correlation) = cancel_correlation {
                            correlation.rollback(conn, command_session_id);
                        }
                        conn.register_pending_subresource_fetch_auth_request_for_session_owner(
                            command_session_id,
                            request_id,
                            pending,
                        );
                        return Some(FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                            -32000,
                            format!("subresource auth terminal action failed: {error}"),
                        )));
                    }
                };
                let state = match command.action {
                    DevToolsAuthChallengeAction::Default => {
                        PendingContinueWithAuthState::SubresourceAuthFail {
                            pending: Box::new(pending),
                        }
                    }
                    DevToolsAuthChallengeAction::Cancel => {
                        PendingContinueWithAuthState::SubresourceAuthCancel {
                            pending: Box::new(pending),
                            correlation: cancel_correlation,
                        }
                    }
                    DevToolsAuthChallengeAction::ProvideCredentials => unreachable!(),
                };
                return Some(FetchCommandTaskStep::Pending(
                    PendingFetchCommandDispatch::new(
                        conn,
                        command_id,
                        command_session_id,
                        PendingFetchCommandKind::ContinueWithAuth {
                            state: Box::new(state),
                        },
                        PendingFetchCommandOperation::Page(pending_page),
                    ),
                ));
            }
            DevToolsAuthChallengeAction::ProvideCredentials => {
                let Some(auth) = request_auth_for_challenge(
                    &pending.challenge,
                    command.username.as_deref().unwrap_or_default(),
                    command.password.as_deref().unwrap_or_default(),
                ) else {
                    conn.register_pending_subresource_fetch_auth_request_for_session_owner(
                        command_session_id,
                        request_id.clone(),
                        pending,
                    );
                    return Some(FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        "NotImplemented",
                    )));
                };
                let continued = continued_subresource_request(&pending);
                let correlation = match PreparedSubresourceCorrelation::prepare(
                    conn,
                    command_session_id,
                    &request_id,
                    &continued,
                    true,
                ) {
                    Some(correlation) => correlation,
                    None => {
                        conn.register_pending_subresource_fetch_auth_request_for_session_owner(
                            command_session_id,
                            request_id,
                            pending,
                        );
                        return Some(FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                            -32000,
                            "RequestNotFound",
                        )));
                    }
                };
                let pending_page =
                    match conn.loaded_page_mut_for_protocol_access(command_session_id) {
                        Ok(page) => page
                            .start_continue_pending_subresource_auth(pending.internal_id, auth)
                            .map_err(|error| format!("subresource auth continue failed: {error}")),
                        Err(message) => Err(message.to_owned()),
                    };
                let pending_page = match pending_page {
                    Ok(pending_page) => pending_page,
                    Err(message) => {
                        correlation.rollback(conn, command_session_id);
                        conn.register_pending_subresource_fetch_auth_request_for_session_owner(
                            command_session_id,
                            request_id,
                            pending,
                        );
                        return Some(FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                            -32000, message,
                        )));
                    }
                };
                return Some(FetchCommandTaskStep::Pending(
                    PendingFetchCommandDispatch::new(
                        conn,
                        command_id,
                        command_session_id,
                        PendingFetchCommandKind::ContinueWithAuth {
                            state: Box::new(
                                PendingContinueWithAuthState::SubresourceAuthContinue {
                                    correlation,
                                },
                            ),
                        },
                        PendingFetchCommandOperation::Page(pending_page),
                    ),
                ));
            }
        }
    }
    let pending = take_pending_auth_navigation_for_action_session(
        conn,
        command_session_id,
        action_session_id,
        &request_id,
    )?;

    if matches!(command.action, DevToolsAuthChallengeAction::Default)
        && pending
            .auth_stage_pause_state()
            .is_some_and(|chain| !chain.remaining_sessions.is_empty())
    {
        return Some(FetchCommandTaskStep::Complete(
            chained_navigation_auth_required_output_plan(conn, command_session_id, pending)
                .unwrap_or_else(|| CommandOutputPlan::error(-32000, "RequestNotFound")),
        ));
    }

    if !admit_to_browser_owner {
        return Some(match command.action {
            DevToolsAuthChallengeAction::Default => {
                FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                    conn,
                    command_id,
                    command_session_id,
                    PendingFetchCommandKind::ContinueWithAuth {
                        state: Box::new(PendingContinueWithAuthState::NavigationFail {
                            pending: Box::new(pending),
                        }),
                    },
                    PendingFetchCommandOperation::Ready,
                ))
            }
            DevToolsAuthChallengeAction::Cancel => {
                FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                    conn,
                    command_id,
                    command_session_id,
                    PendingFetchCommandKind::ContinueWithAuth {
                        state: Box::new(PendingContinueWithAuthState::NavigationCancel {
                            pending: Box::new(pending),
                        }),
                    },
                    PendingFetchCommandOperation::Ready,
                ))
            }
            DevToolsAuthChallengeAction::ProvideCredentials => {
                let Some(auth) = request_auth_for_challenge(
                    &pending.challenge,
                    command.username.as_deref().unwrap_or_default(),
                    command.password.as_deref().unwrap_or_default(),
                ) else {
                    conn.register_pending_fetch_auth_navigation_for_session_owner(
                        command_session_id,
                        request_id.clone(),
                        pending,
                    );
                    return Some(FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                        -32000,
                        "NotImplemented",
                    )));
                };
                FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
                    conn,
                    command_id,
                    command_session_id,
                    PendingFetchCommandKind::ContinueWithAuth {
                        state: Box::new(PendingContinueWithAuthState::NavigationContinue {
                            pending: Box::new(pending),
                            auth,
                        }),
                    },
                    PendingFetchCommandOperation::Ready,
                ))
            }
        });
    }

    let decision = match command.action {
        DevToolsAuthChallengeAction::Default => BrowserPausedNavigationDecision::fail_auth(),
        DevToolsAuthChallengeAction::Cancel => BrowserPausedNavigationDecision::cancel_auth(),
        DevToolsAuthChallengeAction::ProvideCredentials => {
            let Some(auth) = request_auth_for_challenge(
                &pending.challenge,
                command.username.as_deref().unwrap_or_default(),
                command.password.as_deref().unwrap_or_default(),
            ) else {
                conn.register_pending_fetch_auth_navigation_for_session_owner(
                    command_session_id,
                    request_id,
                    pending,
                );
                return Some(FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "NotImplemented",
                )));
            };
            BrowserPausedNavigationDecision::continue_auth(auth)
        }
    };
    let Some(page_owner) = page_owner_for_pending_auth_navigation(conn, &pending) else {
        let restored_request_id = pending.fetch_request_id.clone();
        if !conn.register_pending_fetch_auth_navigation_for_session_owner(
            command_session_id,
            restored_request_id,
            pending,
        ) {
            tracing::error!(
                request_id,
                "failed to restore auth-paused navigation after Page owner capture failure"
            );
        }
        return Some(FetchCommandTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "BrowserHostPausedNavigationDecisionAdmissionFailed: PageNotFound",
        )));
    };
    let publication = conn.publish_browser_owner_paused_navigation_auth_decision(
        page_owner.clone(),
        pending,
        decision,
        CommandDispatchContext::default(),
    );
    Some(match publication {
        Ok(pending) => FetchCommandTaskStep::Pending(PendingFetchCommandDispatch::new(
            conn,
            command_id,
            command_session_id,
            PendingFetchCommandKind::ContinueWithAuth {
                state: Box::new(PendingContinueWithAuthState::BrowserOwnerNavigationDecision),
            },
            PendingFetchCommandOperation::BrowserOwnerPausedNavigationDecision(pending),
        )),
        Err(failure) => {
            let (error, pending) = failure.into_parts();
            if let Some(pending) = pending {
                match pending {
                    BrowserOwnerPausedNavigationSidecar::Auth(pending) => {
                        restore_pending_auth_navigation_after_owner_publication_failure(
                            conn,
                            &page_owner,
                            command_session_id,
                            pending,
                        );
                    }
                    BrowserOwnerPausedNavigationSidecar::Request(_) => {
                        tracing::error!(
                            "continueWithAuth publication returned a request-navigation sidecar"
                        );
                    }
                    BrowserOwnerPausedNavigationSidecar::Response(_) => {
                        tracing::error!(
                            "continueWithAuth publication returned a response-navigation sidecar"
                        );
                    }
                }
            }
            FetchCommandTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                format!("BrowserHostPausedNavigationDecisionAdmissionFailed: {error}"),
            ))
        }
    })
}

fn page_owner_for_pending_auth_navigation(
    conn: &mut CdpConnection,
    pending: &PendingFetchAuthNavigation,
) -> Option<TargetPageResidenceIdentity> {
    let route = conn
        .target_session_route_for_target_id(&pending.navigation.frame_id)
        .or_else(|| conn.session_route(pending.navigation.navigate_session_id.as_deref()))?;
    let mut owner_scope = conn.scoped_none_session_owner_route_override(route);
    owner_scope
        .conn_mut()
        .target_page_residence_identity_for_session(None)
}

fn restore_pending_auth_navigation_after_owner_publication_failure(
    conn: &mut CdpConnection,
    page_owner: &TargetPageResidenceIdentity,
    fallback_session_id: Option<&str>,
    pending: PendingFetchAuthNavigation,
) {
    let request_id = pending.fetch_request_id.clone();
    let restored = if let Some(route) = conn.target_page_owner_route_if_current(page_owner) {
        let mut owner_scope = conn.scoped_none_session_owner_route_override(route);
        owner_scope
            .conn_mut()
            .register_pending_fetch_auth_navigation_for_session_owner(None, request_id, pending)
    } else {
        conn.register_pending_fetch_auth_navigation_for_session_owner(
            fallback_session_id,
            request_id,
            pending,
        )
    };
    if !restored {
        tracing::error!(
            target_id = page_owner.target_id(),
            "failed to restore auth-paused navigation after Browser Host publication failure"
        );
    }
}

fn continued_subresource_request(
    pending: &PendingSubresourceFetchAuthRequest,
) -> PendingSubresourceFetchRequest {
    PendingSubresourceFetchRequest {
        residence: crate::conn::PendingSubresourceFetchResidence::InstalledPage(
            pending.page_owner.clone(),
        ),
        owner_session_id: pending.owner_session_id.clone(),
        action_session_id: pending.action_session_id.clone(),
        owner_kind: pending.owner_kind,
        internal_id: pending.internal_id,
        network_request_id: pending.network_request_id.clone(),
        network_request_handle: pending.network_request_handle,
        frame_id: pending.frame_id.clone(),
        document_url: pending.document_url.clone(),
        resource_type: pending.resource_type,
        websocket_socket_id: pending.websocket_socket_id,
        request_stage_chain: None,
    }
}

fn chained_subresource_auth_required_output_plan(
    conn: &mut CdpConnection,
    command_session_id: Option<&str>,
    pending: PendingSubresourceFetchAuthRequest,
) -> Option<CommandOutputPlan> {
    let event = next_chained_subresource_auth_required_event(conn, command_session_id, pending)?;
    let mut plan = CommandOutputPlan::default();
    plan.push_success();
    plan.push_background_event(event);
    Some(plan)
}

fn chained_navigation_auth_required_output_plan(
    conn: &mut CdpConnection,
    command_session_id: Option<&str>,
    pending: PendingFetchAuthNavigation,
) -> Option<CommandOutputPlan> {
    let event = next_chained_navigation_auth_required_event(conn, command_session_id, pending)?;
    let mut plan = CommandOutputPlan::default();
    plan.push_success();
    plan.push_background_event(event);
    Some(plan)
}

fn next_chained_navigation_auth_required_event(
    conn: &mut CdpConnection,
    command_session_id: Option<&str>,
    mut pending: PendingFetchAuthNavigation,
) -> Option<BackgroundProtocolEvent> {
    let next_pause = pending.pop_next_auth_required_pause()?;
    let previous_owner_session_id = pending.owner_session_id.clone();
    let next_session_has_route = next_pause
        .session_id
        .as_deref()
        .is_some_and(|session_id| conn.session_route(Some(session_id)).is_some());
    pending.owner_session_id = if next_session_has_route {
        next_pause.session_id.clone()
    } else {
        previous_owner_session_id.clone()
    };
    pending.action_session_id = next_pause.session_id.clone();
    pending.owner_kind = next_pause.owner_kind;
    let owner_session_id = next_pause
        .session_id
        .as_deref()
        .filter(|_| next_session_has_route)
        .or(previous_owner_session_id.as_deref())
        .or(command_session_id);
    pending.fetch_request_id = next_pause.request_id.clone();
    if !conn.register_pending_fetch_auth_navigation_for_session_owner(
        owner_session_id,
        next_pause.request_id.clone(),
        pending.clone(),
    ) {
        return None;
    }
    Some(pending_fetch_auth_navigation_required_event(
        next_pause.session_id.as_deref(),
        &pending,
        &next_pause.blocked_intercepts,
    ))
}

fn next_chained_subresource_auth_required_event(
    conn: &mut CdpConnection,
    command_session_id: Option<&str>,
    mut pending: PendingSubresourceFetchAuthRequest,
) -> Option<BackgroundProtocolEvent> {
    let next_pause = pending.pop_next_auth_required_pause()?;
    let previous_owner_session_id = pending.owner_session_id.clone();
    let next_session_has_route = next_pause
        .session_id
        .as_deref()
        .is_some_and(|session_id| conn.session_route(Some(session_id)).is_some());
    pending.owner_session_id = if next_session_has_route {
        next_pause.session_id.clone()
    } else {
        previous_owner_session_id.clone()
    };
    pending.action_session_id = next_pause.session_id.clone();
    pending.owner_kind = next_pause.owner_kind;
    let owner_session_id = next_pause
        .session_id
        .as_deref()
        .filter(|_| next_session_has_route)
        .or(previous_owner_session_id.as_deref())
        .or(command_session_id);
    if !conn.register_pending_subresource_fetch_auth_request_for_session_owner(
        owner_session_id,
        next_pause.request_id.clone(),
        pending.clone(),
    ) {
        return None;
    }
    Some(pending_subresource_auth_required_event(
        next_pause.session_id.as_deref(),
        &next_pause.request_id,
        &pending,
        &next_pause.blocked_intercepts,
    ))
}

pub(super) async fn complete_continue_with_auth_command_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    completed: CompletedFetchCommandOperation,
    state: PendingContinueWithAuthState,
    out: &mut FetchCommandOutput,
) {
    match state {
        PendingContinueWithAuthState::BrowserOwnerNavigationDecision => {
            let Some(completed) = completed.into_browser_owner_paused_navigation_decision() else {
                out.push_error(-32000, "Missing Browser Owner completion");
                return;
            };
            match completed {
                Ok(completed) => out.extend_browser_owner_projection_as_command_response(completed),
                Err(error) => out.push_error(-32000, error),
            }
        }
        PendingContinueWithAuthState::SubresourceAuthCancel {
            pending,
            correlation,
        } => {
            complete_subresource_auth_terminal_async(
                conn,
                session_id,
                completed.into_page_completion(),
                *pending,
                true,
                correlation,
                out,
            )
            .await;
        }
        PendingContinueWithAuthState::SubresourceAuthFail { pending } => {
            complete_subresource_auth_terminal_async(
                conn,
                session_id,
                completed.into_page_completion(),
                *pending,
                false,
                None,
                out,
            )
            .await;
        }
        PendingContinueWithAuthState::SubresourceAuthContinue { correlation } => {
            if let Err(error) =
                finish_continue_subresource_auth(conn, session_id, completed.into_page_completion())
            {
                correlation.rollback(conn, session_id);
                out.push_error(-32000, error);
                return;
            }
            out.push_success();
        }
        PendingContinueWithAuthState::NavigationCancel { pending } => {
            out.push_success();
            super::navigation::cancel_navigation_auth_as_background_events_async(
                conn, out, *pending,
            )
            .await;
        }
        PendingContinueWithAuthState::NavigationFail { pending } => {
            out.push_success();
            let token = pending.document_navigation_token;
            let navigation_state = pending.navigation;
            let navigation = network::materialize_navigation_load_result(
                conn,
                &navigation_state,
                Err("Fetch auth challenge aborted".to_owned()),
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
        PendingContinueWithAuthState::NavigationContinue { pending, auth } => {
            out.push_success();
            let prior_network_observation_journal =
                pending.auth_response.observation_journal().clone();
            load_or_pause_navigation_for_auth_as_background_events_async(
                conn,
                out,
                PendingFetchNavigation {
                    fetch_request_id: pending.response_stage_request_id,
                    interception_session_id: pending.interception_session_id.clone(),
                    document_navigation_token: pending.document_navigation_token,
                    navigation: pending.navigation,
                    request_cookie_report: None,
                    intercept_response: pending.intercept_response,
                    response_stage_url_match_policy: pending.response_stage_url_match_policy,
                    auth_required_blocked_intercepts: Vec::new(),
                },
                Some(auth),
                Some(prior_network_observation_journal),
            )
            .await;
        }
    }
}

async fn complete_subresource_auth_terminal_async(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    completed: Option<Result<CompletedPageCommand, String>>,
    pending: crate::conn::PendingSubresourceFetchAuthRequest,
    expose_challenged_response: bool,
    correlation: Option<PreparedSubresourceCorrelation>,
    out: &mut FetchCommandOutput,
) {
    let activity_session_id = pending.owner_session_id.as_deref().or(session_id);
    let Some(completed) = completed else {
        if let Some(correlation) = correlation {
            correlation.rollback(conn, activity_session_id);
        }
        out.push_error(-32000, "Missing renderer completion");
        return;
    };
    let completion = match completed {
        Ok(completion) => completion,
        Err(error) => {
            if let Some(correlation) = correlation {
                correlation.rollback(conn, activity_session_id);
            }
            out.push_error(-32000, error);
            return;
        }
    };
    let result = match conn.loaded_page_mut_for_protocol_access(activity_session_id) {
        Ok(mut page) if expose_challenged_response => page
            .finish_cancel_pending_subresource_auth(completion)
            .map(|_| ()),
        Ok(mut page) => page
            .finish_fail_pending_subresource_auth(completion)
            .map(|_| ()),
        Err(message) => {
            if let Some(correlation) = correlation {
                correlation.rollback(conn, activity_session_id);
            }
            out.push_error(-32000, message);
            return;
        }
    };
    if let Err(error) = result {
        if let Some(correlation) = correlation {
            correlation.rollback(conn, activity_session_id);
        }
        out.push_error(
            -32000,
            format!("subresource auth terminal action failed: {error}"),
        );
        return;
    }
    out.push_success();
    let mut events = Vec::new();
    activity::flush_post_subresource_auth_activity_background_events_async(
        conn,
        &mut events,
        activity_session_id,
        &pending,
    )
    .await;
    out.extend_background_events(events);
}

fn finish_continue_subresource_auth(
    conn: &mut CdpConnection,
    session_id: Option<&str>,
    completed: Option<Result<CompletedPageCommand, String>>,
) -> Result<(), String> {
    let completion = completed.ok_or_else(|| "Missing renderer completion".to_owned())??;
    let mut page = conn.loaded_page_mut_for_protocol_access(session_id)?;
    page.finish_continue_pending_subresource_auth(completion)
        .map(|_| ())
        .map_err(|error| format!("subresource auth continue failed: {error}"))
}
