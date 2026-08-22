use moli_core::{
    browser_host::{
        BrowserPausedNavigationAuthDecision, BrowserPausedNavigationContinueDecision,
        BrowserPausedNavigationFulfillDecision, BrowserPausedNavigationResponseDecision,
    },
    page::{SubresourceAuthCredentials, SubresourceAuthScheme},
    runtime::NavigationEngine,
};
use moli_fetch::{
    NetworkFetchResult, NetworkObservationJournal, RawResponse, StreamingRawResponse,
};
use moli_web_mime::response_headers_indicate_attachment_download;

use crate::conn::{
    BackgroundBufferedResponseNavigationLoadJob, BackgroundCapturedResponseNavigationLoadJob,
    BackgroundInterceptedNavigationFetchJob, BackgroundInterceptedNavigationFetchMode,
    BackgroundInterceptedNavigationFetchResult, BackgroundNavigationBodyCompletionSink,
    BackgroundPausedStreamingResponseNavigationPreparationJob, BackgroundProtocolEvent,
    BackgroundStreamingResponseCollectionJob, BackgroundStreamingResponseNavigationLoadJob,
    CapturedBody, CdpConnection, DocumentBodySource, FetchAuthChallenge, NavigationLoadOutcome,
    PausedDocumentTransfer, PausedResponsePreparedDocument, PendingFetchNavigation,
    PendingStreamingDocumentResponseNavigation,
};
use crate::domains::{
    command_output::{CommandOutputBuffer, CommandOutputPlan},
    network, page,
};

use super::{
    helpers::{extract_auth_challenge, populate_auth_challenge_origin},
    navigation::{
        pause_buffered_raw_response_stage_navigation_into_buffer,
        pause_data_url_response_stage_navigation_into_buffer,
        pause_streaming_raw_response_stage_navigation_into_buffer,
        prepare_navigation_response_stage, register_navigation_auth_required_event,
        streaming_response_stage_extra_info,
    },
};

enum PendingPausedNavigationResumePhase {
    Fetch {
        pending: Box<PendingFetchNavigation>,
        should_handle_auth: bool,
        prior_network_observation_journal: Option<NetworkObservationJournal>,
        job: Box<BackgroundInterceptedNavigationFetchJob>,
    },
    CollectAuthChallenge {
        pending: Box<PendingFetchNavigation>,
        challenge: FetchAuthChallenge,
        request_cookie_report: Option<moli_cookie_jar::StoredCookieQueryReport>,
        job: Box<BackgroundStreamingResponseCollectionJob>,
    },
    BuildBuffered {
        prefix_events: Vec<BackgroundProtocolEvent>,
        token: crate::conn::DocumentNavigationToken,
        state: Box<crate::conn::NavigationDispatchState>,
        job: Box<BackgroundBufferedResponseNavigationLoadJob>,
    },
    BuildCaptured {
        prefix_events: Vec<BackgroundProtocolEvent>,
        token: crate::conn::DocumentNavigationToken,
        state: Box<crate::conn::NavigationDispatchState>,
        job: Box<BackgroundCapturedResponseNavigationLoadJob>,
    },
    BuildStreaming {
        prefix_events: Vec<BackgroundProtocolEvent>,
        token: crate::conn::DocumentNavigationToken,
        state: Box<crate::conn::NavigationDispatchState>,
        job: Box<BackgroundStreamingResponseNavigationLoadJob>,
    },
    PrepareResponseStage {
        prefix_events: Vec<BackgroundProtocolEvent>,
        pending: Box<PendingFetchNavigation>,
        response: Box<NetworkFetchResult<StreamingRawResponse>>,
        body_progress_source: network::MainDocumentBodyProgressSource,
        job: Box<BackgroundPausedStreamingResponseNavigationPreparationJob>,
    },
}

enum CompletedPausedNavigationResumePhase {
    Fetch {
        pending: Box<PendingFetchNavigation>,
        should_handle_auth: bool,
        prior_network_observation_journal: Option<NetworkObservationJournal>,
        result: Box<Result<BackgroundInterceptedNavigationFetchResult, String>>,
    },
    CollectAuthChallenge {
        pending: Box<PendingFetchNavigation>,
        challenge: FetchAuthChallenge,
        request_cookie_report: Option<moli_cookie_jar::StoredCookieQueryReport>,
        result: Box<Result<NetworkFetchResult<RawResponse>, String>>,
    },
    BuildBuffered {
        prefix_events: Vec<BackgroundProtocolEvent>,
        token: crate::conn::DocumentNavigationToken,
        state: Box<crate::conn::NavigationDispatchState>,
        engine: Box<NavigationEngine>,
        navigation: Box<Result<NavigationLoadOutcome, String>>,
    },
    BuildCaptured {
        prefix_events: Vec<BackgroundProtocolEvent>,
        token: crate::conn::DocumentNavigationToken,
        state: Box<crate::conn::NavigationDispatchState>,
        engine: Box<NavigationEngine>,
        navigation: Box<Result<NavigationLoadOutcome, String>>,
    },
    BuildStreaming {
        prefix_events: Vec<BackgroundProtocolEvent>,
        token: crate::conn::DocumentNavigationToken,
        state: Box<crate::conn::NavigationDispatchState>,
        engine: Box<NavigationEngine>,
        navigation: Box<Result<NavigationLoadOutcome, String>>,
    },
    PrepareResponseStage {
        prefix_events: Vec<BackgroundProtocolEvent>,
        pending: Box<PendingFetchNavigation>,
        response: Box<NetworkFetchResult<StreamingRawResponse>>,
        body_progress_source: network::MainDocumentBodyProgressSource,
        prepared_document: Box<Result<PausedResponsePreparedDocument, String>>,
    },
}

pub(crate) struct PendingPausedNavigationResumeOwnerTask {
    phase: PendingPausedNavigationResumePhase,
}

pub(crate) struct CompletedPausedNavigationResumeOwnerTask {
    phase: CompletedPausedNavigationResumePhase,
}

pub(crate) enum PausedNavigationResumeOwnerTaskStep {
    Pending(Box<PendingPausedNavigationResumeOwnerTask>),
    NavigatePending(Box<page::PendingNavigateCommand>),
    NavigateReady(Box<page::CompletedNavigateCommand>),
    CommandRejected(CommandOutputPlan),
    Complete(CommandOutputPlan),
}

pub(crate) enum PausedNavigationFulfillSource {
    Request(Box<PendingFetchNavigation>),
    Response(Box<PausedDocumentTransfer>),
}

impl PendingPausedNavigationResumeOwnerTask {
    pub(crate) async fn wait(self: Box<Self>) -> CompletedPausedNavigationResumeOwnerTask {
        let phase = match self.phase {
            PendingPausedNavigationResumePhase::Fetch {
                pending,
                should_handle_auth,
                prior_network_observation_journal,
                job,
            } => CompletedPausedNavigationResumePhase::Fetch {
                pending,
                should_handle_auth,
                prior_network_observation_journal,
                result: Box::new(job.run().await),
            },
            PendingPausedNavigationResumePhase::CollectAuthChallenge {
                pending,
                challenge,
                request_cookie_report,
                job,
            } => CompletedPausedNavigationResumePhase::CollectAuthChallenge {
                pending,
                challenge,
                request_cookie_report,
                result: Box::new(job.run().await),
            },
            PendingPausedNavigationResumePhase::BuildBuffered {
                prefix_events,
                token,
                state,
                job,
            } => {
                let (engine, navigation) = job.run().await;
                CompletedPausedNavigationResumePhase::BuildBuffered {
                    prefix_events,
                    token,
                    state,
                    engine: Box::new(engine),
                    navigation: Box::new(navigation),
                }
            }
            PendingPausedNavigationResumePhase::BuildCaptured {
                prefix_events,
                token,
                state,
                job,
            } => {
                let (engine, navigation) = job.run().await;
                CompletedPausedNavigationResumePhase::BuildCaptured {
                    prefix_events,
                    token,
                    state,
                    engine: Box::new(engine),
                    navigation: Box::new(navigation),
                }
            }
            PendingPausedNavigationResumePhase::BuildStreaming {
                prefix_events,
                token,
                state,
                job,
            } => {
                let (engine, navigation) = job.run(None).await;
                CompletedPausedNavigationResumePhase::BuildStreaming {
                    prefix_events,
                    token,
                    state,
                    engine: Box::new(engine),
                    navigation: Box::new(navigation),
                }
            }
            PendingPausedNavigationResumePhase::PrepareResponseStage {
                prefix_events,
                pending,
                response,
                body_progress_source,
                job,
            } => CompletedPausedNavigationResumePhase::PrepareResponseStage {
                prefix_events,
                pending,
                response,
                body_progress_source,
                prepared_document: Box::new(job.run().await),
            },
        };
        CompletedPausedNavigationResumeOwnerTask { phase }
    }
}

pub(crate) fn start_paused_navigation_resume_owner_task(
    conn: &mut CdpConnection,
    mut pending: PendingFetchNavigation,
    decision: BrowserPausedNavigationContinueDecision,
) -> PausedNavigationResumeOwnerTaskStep {
    let (url, method, post_data, headers, intercept_response) = decision.into_parts();
    if intercept_response {
        pending.intercept_response = true;
        pending.response_stage_url_match_policy =
            crate::conn::ResponseStageUrlMatchPolicy::AlreadyMatched;
    }
    if let Some(url) = url {
        pending.navigation.requested_url = url;
    }
    if let Some(method) = method {
        pending.navigation.request_method = method;
    }
    if let Some(post_data) = post_data {
        pending.navigation.set_request_body_text(post_data);
    }
    if let Some(headers) = headers {
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

    start_pending_navigation_network_resume(conn, pending, None, None)
}

pub(crate) fn start_paused_navigation_auth_resume_owner_task(
    conn: &mut CdpConnection,
    pending: crate::conn::PendingFetchAuthNavigation,
    decision: BrowserPausedNavigationAuthDecision,
) -> PausedNavigationResumeOwnerTaskStep {
    match decision {
        BrowserPausedNavigationAuthDecision::Fail => {
            let pending = pending_auth_navigation_into_request(pending, false);
            navigation_error(
                conn,
                pending,
                "Fetch auth challenge aborted".to_owned(),
                Vec::new(),
            )
        }
        BrowserPausedNavigationAuthDecision::Cancel => cancel_paused_navigation_auth(conn, pending),
        BrowserPausedNavigationAuthDecision::Continue(auth) => {
            let prior_network_observation_journal =
                pending.auth_response.observation_journal().clone();
            let pending = pending_auth_navigation_into_request(pending, false);
            start_pending_navigation_network_resume(
                conn,
                pending,
                Some(auth),
                Some(prior_network_observation_journal),
            )
        }
    }
}

pub(crate) fn start_paused_navigation_response_resume_owner_task(
    conn: &mut CdpConnection,
    transfer: PausedDocumentTransfer,
    decision: BrowserPausedNavigationResponseDecision,
) -> PausedNavigationResumeOwnerTaskStep {
    let (response_code, response_headers) = decision.into_parts();
    if let Some(sender) = conn.background_navigation_completion_sender_for_session_owner(None) {
        match transfer.into_pending_streaming_document_response_navigation() {
            Ok(pending) => {
                continue_streaming_document_response_in_background(
                    conn,
                    sender,
                    pending,
                    response_code,
                    response_headers,
                );
                // The Fetch acknowledgement is projected by the outer owner
                // decision. The original navigation result remains on the
                // existing detached completion transport.
                return PausedNavigationResumeOwnerTaskStep::Complete(CommandOutputPlan::default());
            }
            Err(transfer) => {
                return start_non_streaming_response_resume(
                    conn,
                    *transfer,
                    response_code,
                    response_headers,
                );
            }
        }
    }
    start_non_streaming_response_resume(conn, transfer, response_code, response_headers)
}

pub(crate) fn start_paused_navigation_fulfill_owner_task(
    conn: &mut CdpConnection,
    source: PausedNavigationFulfillSource,
    decision: BrowserPausedNavigationFulfillDecision,
) -> PausedNavigationResumeOwnerTaskStep {
    let (response_code, response_headers, response_body) = decision.into_parts();
    let (token, state, final_url, request_cookie_report, body_progress_source) = match source {
        PausedNavigationFulfillSource::Request(pending) => {
            let pending = *pending;
            let final_url = pending.navigation.requested_url.clone();
            (
                pending.document_navigation_token,
                pending.navigation,
                final_url,
                pending.request_cookie_report,
                network::MainDocumentBodyProgressSource::default(),
            )
        }
        PausedNavigationFulfillSource::Response(transfer) => {
            (*transfer).into_synthetic_navigation_parts()
        }
    };
    let Some(token) = token else {
        return PausedNavigationResumeOwnerTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "Navigation aborted",
        ));
    };
    let job = conn.background_synthetic_response_navigation_load_job_for_navigation(
        &state,
        final_url,
        response_code,
        response_headers,
        CapturedBody::from_bytes(response_body.unwrap_or_default()),
        request_cookie_report,
        body_progress_source,
    );
    PausedNavigationResumeOwnerTaskStep::Pending(Box::new(PendingPausedNavigationResumeOwnerTask {
        phase: PendingPausedNavigationResumePhase::BuildCaptured {
            prefix_events: Vec::new(),
            token,
            state: Box::new(state),
            job: Box::new(job),
        },
    }))
}

pub(crate) fn continue_streaming_document_response_in_background(
    conn: &mut CdpConnection,
    sender: tokio::sync::mpsc::UnboundedSender<page::BackgroundNavigationCompletion>,
    pending: PendingStreamingDocumentResponseNavigation,
    response_code: Option<u16>,
    response_headers: Vec<(String, String)>,
) {
    let PendingStreamingDocumentResponseNavigation {
        document_navigation_token,
        navigation,
        response,
        network_observation_journal,
        body_progress_source,
        prepared_document,
    } = pending;
    let session_id = navigation.navigate_session_id.clone();
    let cancellation = crate::conn::BackgroundNavigationCancellation::from_fetch_cancel_handle(
        response.cancellation_handle(),
    );
    let none_session_owner_route = session_id
        .is_none()
        .then(|| conn.none_session_owner_route_override())
        .flatten();
    if response_code.is_none()
        && response_headers.is_empty()
        && let Some(prepared_document) = prepared_document
    {
        conn.record_background_navigation_started_scheduler_event(
            &document_navigation_token,
            &navigation,
            cancellation.clone(),
        );
        tokio::task::spawn_local(async move {
            let body_completion_sink = BackgroundNavigationBodyCompletionSink::new(
                sender.clone(),
                document_navigation_token.clone(),
                navigation.clone(),
                none_session_owner_route.clone(),
            );
            let (engine, navigation_result) =
                prepared_document.resume_streaming(response, Some(body_completion_sink));
            let _ = sender.send(page::BackgroundNavigationCompletion::new(
                document_navigation_token,
                navigation,
                none_session_owner_route,
                engine,
                Ok(navigation_result),
            ));
        });
        return;
    }
    let job = conn.background_streaming_response_navigation_load_job_for_navigation(
        &navigation,
        response,
        network_observation_journal,
        response_code,
        response_headers,
        body_progress_source,
    );
    conn.record_background_navigation_started_scheduler_event(
        &document_navigation_token,
        &navigation,
        cancellation,
    );
    tokio::task::spawn_local(async move {
        let body_completion_sink = BackgroundNavigationBodyCompletionSink::new(
            sender.clone(),
            document_navigation_token.clone(),
            navigation.clone(),
            none_session_owner_route.clone(),
        );
        let (engine, navigation_result) = job.run(Some(body_completion_sink)).await;
        let _ = sender.send(page::BackgroundNavigationCompletion::new(
            document_navigation_token,
            navigation,
            none_session_owner_route,
            engine,
            navigation_result,
        ));
    });
}

fn start_non_streaming_response_resume(
    conn: &mut CdpConnection,
    transfer: PausedDocumentTransfer,
    response_code: Option<u16>,
    response_headers: Vec<(String, String)>,
) -> PausedNavigationResumeOwnerTaskStep {
    let request_id = transfer.fetch_request_id().to_owned();
    let (token, state, body) = match transfer.into_pending_navigation_parts() {
        Ok(parts) => parts,
        Err(transfer) => {
            let restored = conn.register_pending_fetch_response_transfer_for_session_owner(
                None, request_id, *transfer,
            );
            if !restored {
                tracing::error!(
                    "failed to restore active response body stream after owner continue rejection"
                );
            }
            return PausedNavigationResumeOwnerTaskStep::CommandRejected(CommandOutputPlan::error(
                -32000,
                "ResponseBodyStreamActive",
            ));
        }
    };
    let Some(token) = token else {
        return PausedNavigationResumeOwnerTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "Navigation aborted",
        ));
    };
    let has_response_override = response_code.is_some() || !response_headers.is_empty();
    match body {
        DocumentBodySource::BufferedRaw {
            response,
            network_observation_journal,
            ..
        } if !has_response_override => {
            let job = conn.background_buffered_response_navigation_load_job_for_navigation(
                &state,
                NetworkFetchResult::with_observation_journal(response, network_observation_journal),
                network::MainDocumentBodyProgressSource::default(),
            );
            PausedNavigationResumeOwnerTaskStep::Pending(Box::new(
                PendingPausedNavigationResumeOwnerTask {
                    phase: PendingPausedNavigationResumePhase::BuildBuffered {
                        prefix_events: Vec::new(),
                        token,
                        state: Box::new(state),
                        job: Box::new(job),
                    },
                },
            ))
        }
        DocumentBodySource::BufferedRaw {
            response,
            network_observation_journal,
            ..
        } => {
            let (head, body) = response.into_body();
            let Ok(body) = body.try_into_materialized_bytes() else {
                return PausedNavigationResumeOwnerTaskStep::Complete(CommandOutputPlan::error(
                    -32000,
                    "BufferedResponseBodyNotMaterialized",
                ));
            };
            start_captured_response_build(
                conn,
                token,
                state,
                head,
                crate::conn::CapturedBody::from_bytes(body),
                network_observation_journal,
                network::MainDocumentBodyProgressSource::default(),
                response_code,
                response_headers,
            )
        }
        DocumentBodySource::StreamingRaw {
            response,
            network_observation_journal,
            body_progress_source,
            prepared_document,
            ..
        } => {
            if !has_response_override && let Some(prepared_document) = prepared_document {
                let (engine, navigation) = prepared_document.resume_streaming(response, None);
                return PausedNavigationResumeOwnerTaskStep::NavigateReady(Box::new(
                    page::CompletedNavigateCommand::loaded(
                        Vec::new(),
                        token,
                        state,
                        engine,
                        Ok(navigation),
                    ),
                ));
            }
            let job = conn.background_streaming_response_navigation_load_job_for_navigation(
                &state,
                response,
                network_observation_journal,
                response_code,
                response_headers,
                body_progress_source,
            );
            PausedNavigationResumeOwnerTaskStep::Pending(Box::new(
                PendingPausedNavigationResumeOwnerTask {
                    phase: PendingPausedNavigationResumePhase::BuildStreaming {
                        prefix_events: Vec::new(),
                        token,
                        state: Box::new(state),
                        job: Box::new(job),
                    },
                },
            ))
        }
        DocumentBodySource::CapturedRaw {
            head,
            body,
            network_observation_journal,
            body_progress_source,
            ..
        } => start_captured_response_build(
            conn,
            token,
            state,
            head,
            body,
            network_observation_journal,
            body_progress_source,
            response_code,
            response_headers,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn start_captured_response_build(
    conn: &mut CdpConnection,
    token: crate::conn::DocumentNavigationToken,
    state: crate::conn::NavigationDispatchState,
    head: moli_fetch::ResponseHead,
    body: crate::conn::CapturedBody,
    network_observation_journal: NetworkObservationJournal,
    body_progress_source: network::MainDocumentBodyProgressSource,
    response_code: Option<u16>,
    response_headers: Vec<(String, String)>,
) -> PausedNavigationResumeOwnerTaskStep {
    let job = conn.background_captured_response_navigation_load_job_for_navigation(
        &state,
        head,
        body,
        network_observation_journal,
        body_progress_source,
        response_code,
        response_headers,
    );
    PausedNavigationResumeOwnerTaskStep::Pending(Box::new(PendingPausedNavigationResumeOwnerTask {
        phase: PendingPausedNavigationResumePhase::BuildCaptured {
            prefix_events: Vec::new(),
            token,
            state: Box::new(state),
            job: Box::new(job),
        },
    }))
}

fn start_pending_navigation_network_resume(
    conn: &mut CdpConnection,
    pending: PendingFetchNavigation,
    auth: Option<SubresourceAuthCredentials>,
    prior_network_observation_journal: Option<NetworkObservationJournal>,
) -> PausedNavigationResumeOwnerTaskStep {
    let Some(token) = pending.document_navigation_token.clone() else {
        return PausedNavigationResumeOwnerTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "Navigation aborted",
        ));
    };
    if pending.intercept_response && pending.navigation.requested_url.scheme() == "data" {
        let mut output = CommandOutputBuffer::default();
        pause_data_url_response_stage_navigation_into_buffer(conn, &mut output, pending);
        return PausedNavigationResumeOwnerTaskStep::Complete(output.into_plan());
    }

    let should_handle_auth = conn.target_fetch_matches_auth_required_for_session_owner(
        pending.navigation.navigate_session_id.as_deref(),
        &pending.navigation.requested_url,
    ) && pending.navigation.requested_url.scheme() != "data";
    if !should_handle_auth && !pending.intercept_response && auth.is_none() {
        let state = pending.navigation;
        let job = conn.background_navigation_load_job_for_navigation(
            &state,
            network::MainDocumentBodyProgressSource::default(),
            None,
        );
        return PausedNavigationResumeOwnerTaskStep::NavigatePending(Box::new(
            page::PendingNavigateCommand::load(Vec::new(), token, state, job),
        ));
    }

    let mode = match auth.as_ref() {
        Some(auth) if pending.intercept_response && auth.scheme != SubresourceAuthScheme::Basic => {
            return navigation_error(
                conn,
                pending,
                format!(
                    "Fetch response-stage interception after {:?} authentication is not supported for navigation without buffering",
                    auth.scheme
                ),
                Vec::new(),
            );
        }
        Some(_) if pending.intercept_response => {
            BackgroundInterceptedNavigationFetchMode::Streaming
        }
        Some(_) => BackgroundInterceptedNavigationFetchMode::Buffered,
        None if should_handle_auth && !pending.intercept_response => {
            BackgroundInterceptedNavigationFetchMode::CollectStreaming
        }
        None => BackgroundInterceptedNavigationFetchMode::Streaming,
    };
    let job = match conn.background_intercepted_navigation_fetch_job_for_navigation(
        &pending.navigation,
        auth,
        mode,
    ) {
        Ok(job) => job,
        Err(error) => return navigation_error(conn, pending, error, Vec::new()),
    };
    PausedNavigationResumeOwnerTaskStep::Pending(Box::new(PendingPausedNavigationResumeOwnerTask {
        phase: PendingPausedNavigationResumePhase::Fetch {
            pending: Box::new(pending),
            should_handle_auth,
            prior_network_observation_journal,
            job: Box::new(job),
        },
    }))
}

pub(crate) fn complete_paused_navigation_resume_owner_task(
    conn: &mut CdpConnection,
    completed: CompletedPausedNavigationResumeOwnerTask,
) -> PausedNavigationResumeOwnerTaskStep {
    match completed.phase {
        CompletedPausedNavigationResumePhase::Fetch {
            pending,
            should_handle_auth,
            prior_network_observation_journal,
            result,
        } => match *result {
            Err(error) => navigation_error(conn, *pending, error, Vec::new()),
            Ok(BackgroundInterceptedNavigationFetchResult::Buffered(response)) => {
                complete_buffered_fetch(
                    conn,
                    *pending,
                    append_prior_network_observations(response, prior_network_observation_journal),
                    should_handle_auth,
                )
            }
            Ok(BackgroundInterceptedNavigationFetchResult::Streaming(response)) => {
                complete_streaming_fetch(
                    conn,
                    *pending,
                    append_prior_network_observations(response, prior_network_observation_journal),
                    should_handle_auth,
                )
            }
        },
        CompletedPausedNavigationResumePhase::CollectAuthChallenge {
            pending,
            challenge,
            request_cookie_report,
            result,
        } => match *result {
            Ok(response) => {
                let event = register_navigation_auth_required_event(
                    conn,
                    &pending,
                    challenge,
                    request_cookie_report,
                    response,
                );
                let mut output = CommandOutputBuffer::default();
                output.extend_background_events_after_messages([event]);
                PausedNavigationResumeOwnerTaskStep::Complete(output.into_plan())
            }
            Err(error) => navigation_error(conn, *pending, error, Vec::new()),
        },
        CompletedPausedNavigationResumePhase::BuildBuffered {
            prefix_events,
            token,
            state,
            engine,
            navigation,
        }
        | CompletedPausedNavigationResumePhase::BuildCaptured {
            prefix_events,
            token,
            state,
            engine,
            navigation,
        }
        | CompletedPausedNavigationResumePhase::BuildStreaming {
            prefix_events,
            token,
            state,
            engine,
            navigation,
        } => PausedNavigationResumeOwnerTaskStep::NavigateReady(Box::new(
            page::CompletedNavigateCommand::loaded(
                prefix_events,
                token,
                *state,
                *engine,
                *navigation,
            ),
        )),
        CompletedPausedNavigationResumePhase::PrepareResponseStage {
            prefix_events,
            pending,
            response,
            body_progress_source,
            prepared_document,
        } => match *prepared_document {
            Ok(prepared_document) => finish_streaming_response_stage_pause(
                conn,
                prefix_events,
                *pending,
                *response,
                body_progress_source,
                Some(prepared_document),
            ),
            Err(error) => navigation_error(conn, *pending, error, prefix_events),
        },
    }
}

fn split_pending_auth_navigation(
    pending: crate::conn::PendingFetchAuthNavigation,
    preserve_request_cookie_report: bool,
) -> (
    PendingFetchNavigation,
    std::sync::Arc<NetworkFetchResult<RawResponse>>,
) {
    let crate::conn::PendingFetchAuthNavigation {
        interception_session_id,
        response_stage_request_id,
        document_navigation_token,
        navigation,
        request_cookie_report,
        auth_response,
        intercept_response,
        response_stage_url_match_policy,
        ..
    } = pending;
    (
        PendingFetchNavigation {
            fetch_request_id: response_stage_request_id,
            interception_session_id,
            document_navigation_token,
            navigation,
            request_cookie_report: if preserve_request_cookie_report {
                request_cookie_report
            } else {
                None
            },
            intercept_response,
            response_stage_url_match_policy,
            auth_required_blocked_intercepts: Vec::new(),
        },
        auth_response,
    )
}

fn pending_auth_navigation_into_request(
    pending: crate::conn::PendingFetchAuthNavigation,
    preserve_request_cookie_report: bool,
) -> PendingFetchNavigation {
    split_pending_auth_navigation(pending, preserve_request_cookie_report).0
}

fn cancel_paused_navigation_auth(
    conn: &mut CdpConnection,
    pending: crate::conn::PendingFetchAuthNavigation,
) -> PausedNavigationResumeOwnerTaskStep {
    let (mut pending, response) = split_pending_auth_navigation(pending, true);
    let response = match std::sync::Arc::try_unwrap(response) {
        Ok(response) => response,
        Err(response) => response.as_ref().clone(),
    };
    if response
        .observation_journal()
        .terminal_response_is_failed_proxy_connect()
    {
        let response_head = response.response();
        let progress = network::response_stage_main_document_navigation_network_progress(
            conn,
            &pending.navigation,
            pending.request_cookie_report.as_ref(),
        );
        let mut prefix_events = Vec::new();
        progress.emit_response_without_extra_info_into_background_events(
            &mut prefix_events,
            &response_head.final_url,
            response_head.status,
            &response_head.headers,
            false,
        );
        let Some(token) = pending.document_navigation_token else {
            return PausedNavigationResumeOwnerTaskStep::Complete(CommandOutputPlan::error(
                -32000,
                "Navigation aborted",
            ));
        };
        let state = pending.navigation;
        let navigation = network::materialize_navigation_load_result(
            conn,
            &state,
            Ok(NavigationLoadOutcome::network_failure(
                "net::ERR_HTTP_RESPONSE_CODE_FAILURE".to_owned(),
            )),
        );
        return PausedNavigationResumeOwnerTaskStep::NavigateReady(Box::new(
            page::CompletedNavigateCommand::materialized_with_prefix(
                prefix_events,
                page::MaterializedNavigationCompletion::new(token, state, navigation),
            ),
        ));
    }
    if prepare_navigation_response_stage(conn, &mut pending, &response.response().final_url) {
        let mut output = CommandOutputBuffer::default();
        pause_buffered_raw_response_stage_navigation_into_buffer(
            conn,
            &mut output,
            pending,
            response,
        );
        return PausedNavigationResumeOwnerTaskStep::Complete(output.into_plan());
    }
    pending.intercept_response = false;
    start_buffered_navigation_build(conn, pending, response, Vec::new())
}

fn append_prior_network_observations<R>(
    response: NetworkFetchResult<R>,
    prior: Option<NetworkObservationJournal>,
) -> NetworkFetchResult<R> {
    let Some(mut prior) = prior else {
        return response;
    };
    let (response, current) = response.into_parts_with_observation_journal();
    prior.append(current);
    NetworkFetchResult::with_observation_journal(response, prior)
}

fn complete_buffered_fetch(
    conn: &mut CdpConnection,
    mut pending: PendingFetchNavigation,
    response: NetworkFetchResult<RawResponse>,
    should_handle_auth: bool,
) -> PausedNavigationResumeOwnerTaskStep {
    let response_head = response.response();
    if let Err(error) = conn.ensure_navigation_response_status(
        pending.navigation.requested_url.as_str(),
        response_head.status,
        should_handle_auth,
    ) {
        return navigation_error(conn, pending, error, Vec::new());
    }
    if should_handle_auth
        && matches!(response_head.status, 401 | 407)
        && let Some(mut challenge) = extract_auth_challenge(&response_head.headers)
    {
        populate_auth_challenge_origin(
            conn,
            pending.navigation.navigate_session_id.as_deref(),
            &response_head.final_url,
            &mut challenge,
        );
        let event = register_navigation_auth_required_event(
            conn,
            &pending,
            challenge,
            response_head.request_cookie_report.clone(),
            response,
        );
        let mut output = CommandOutputBuffer::default();
        output.extend_background_events_after_messages([event]);
        return PausedNavigationResumeOwnerTaskStep::Complete(output.into_plan());
    }
    if prepare_navigation_response_stage(conn, &mut pending, &response_head.final_url) {
        let mut output = CommandOutputBuffer::default();
        pause_buffered_raw_response_stage_navigation_into_buffer(
            conn,
            &mut output,
            pending,
            response,
        );
        return PausedNavigationResumeOwnerTaskStep::Complete(output.into_plan());
    }
    start_buffered_navigation_build(conn, pending, response, Vec::new())
}

fn complete_streaming_fetch(
    conn: &mut CdpConnection,
    mut pending: PendingFetchNavigation,
    response: NetworkFetchResult<StreamingRawResponse>,
    should_handle_auth: bool,
) -> PausedNavigationResumeOwnerTaskStep {
    let response_head = response.response();
    if let Err(error) = conn.ensure_navigation_response_status(
        pending.navigation.requested_url.as_str(),
        response_head.status,
        should_handle_auth,
    ) {
        return navigation_error(conn, pending, error, Vec::new());
    }
    if should_handle_auth
        && matches!(response_head.status, 401 | 407)
        && let Some(mut challenge) = extract_auth_challenge(&response_head.headers)
    {
        populate_auth_challenge_origin(
            conn,
            pending.navigation.navigate_session_id.as_deref(),
            &response_head.final_url,
            &mut challenge,
        );
        let request_cookie_report = response_head.request_cookie_report.clone();
        let job = conn.background_streaming_response_collection_job(response);
        return PausedNavigationResumeOwnerTaskStep::Pending(Box::new(
            PendingPausedNavigationResumeOwnerTask {
                phase: PendingPausedNavigationResumePhase::CollectAuthChallenge {
                    pending: Box::new(pending),
                    challenge,
                    request_cookie_report,
                    job: Box::new(job),
                },
            },
        ));
    }
    if response_headers_indicate_attachment_download(&response_head.headers) {
        return start_streaming_navigation_build(conn, pending, response, Vec::new());
    }
    if !prepare_navigation_response_stage(conn, &mut pending, &response_head.final_url) {
        return start_streaming_navigation_build(conn, pending, response, Vec::new());
    }

    let (body_progress_source, prefix_events) = streaming_response_stage_extra_info(
        conn,
        &pending,
        response.response(),
        response.observation_journal(),
    );
    let job = match conn.background_paused_streaming_response_navigation_preparation_job(
        &pending.navigation,
        response.response(),
        response.observation_journal(),
        body_progress_source.clone(),
    ) {
        Ok(Some(job)) => job,
        Ok(None) => {
            return finish_streaming_response_stage_pause(
                conn,
                prefix_events,
                pending,
                response,
                body_progress_source,
                None,
            );
        }
        Err(error) => return navigation_error(conn, pending, error, prefix_events),
    };
    PausedNavigationResumeOwnerTaskStep::Pending(Box::new(PendingPausedNavigationResumeOwnerTask {
        phase: PendingPausedNavigationResumePhase::PrepareResponseStage {
            prefix_events,
            pending: Box::new(pending),
            response: Box::new(response),
            body_progress_source,
            job: Box::new(job),
        },
    }))
}

fn start_buffered_navigation_build(
    conn: &mut CdpConnection,
    pending: PendingFetchNavigation,
    response: NetworkFetchResult<RawResponse>,
    prefix_events: Vec<BackgroundProtocolEvent>,
) -> PausedNavigationResumeOwnerTaskStep {
    let Some(token) = pending.document_navigation_token else {
        return PausedNavigationResumeOwnerTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "Navigation aborted",
        ));
    };
    let state = pending.navigation;
    let job = conn.background_buffered_response_navigation_load_job_for_navigation(
        &state,
        response,
        network::MainDocumentBodyProgressSource::default(),
    );
    PausedNavigationResumeOwnerTaskStep::Pending(Box::new(PendingPausedNavigationResumeOwnerTask {
        phase: PendingPausedNavigationResumePhase::BuildBuffered {
            prefix_events,
            token,
            state: Box::new(state),
            job: Box::new(job),
        },
    }))
}

fn start_streaming_navigation_build(
    conn: &mut CdpConnection,
    pending: PendingFetchNavigation,
    response: NetworkFetchResult<StreamingRawResponse>,
    prefix_events: Vec<BackgroundProtocolEvent>,
) -> PausedNavigationResumeOwnerTaskStep {
    let Some(token) = pending.document_navigation_token else {
        return PausedNavigationResumeOwnerTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "Navigation aborted",
        ));
    };
    let state = pending.navigation;
    let (response, network_observation_journal) = response.into_parts_with_observation_journal();
    let job = conn.background_streaming_response_navigation_load_job_for_navigation(
        &state,
        response,
        network_observation_journal,
        None,
        Vec::new(),
        network::MainDocumentBodyProgressSource::default(),
    );
    PausedNavigationResumeOwnerTaskStep::Pending(Box::new(PendingPausedNavigationResumeOwnerTask {
        phase: PendingPausedNavigationResumePhase::BuildStreaming {
            prefix_events,
            token,
            state: Box::new(state),
            job: Box::new(job),
        },
    }))
}

fn finish_streaming_response_stage_pause(
    conn: &mut CdpConnection,
    prefix_events: Vec<BackgroundProtocolEvent>,
    pending: PendingFetchNavigation,
    response: NetworkFetchResult<StreamingRawResponse>,
    body_progress_source: network::MainDocumentBodyProgressSource,
    prepared_document: Option<PausedResponsePreparedDocument>,
) -> PausedNavigationResumeOwnerTaskStep {
    let mut output = CommandOutputBuffer::default();
    output.extend_background_events_after_messages(prefix_events);
    pause_streaming_raw_response_stage_navigation_into_buffer(
        conn,
        &mut output,
        pending,
        response,
        body_progress_source,
        prepared_document,
    );
    PausedNavigationResumeOwnerTaskStep::Complete(output.into_plan())
}

fn navigation_error(
    conn: &mut CdpConnection,
    pending: PendingFetchNavigation,
    error: String,
    prefix_events: Vec<BackgroundProtocolEvent>,
) -> PausedNavigationResumeOwnerTaskStep {
    let Some(token) = pending.document_navigation_token else {
        return PausedNavigationResumeOwnerTaskStep::Complete(CommandOutputPlan::error(
            -32000,
            "Navigation aborted",
        ));
    };
    let state = pending.navigation;
    let navigation = network::materialize_navigation_load_result(conn, &state, Err(error));
    PausedNavigationResumeOwnerTaskStep::NavigateReady(Box::new(
        page::CompletedNavigateCommand::materialized_with_prefix(
            prefix_events,
            page::MaterializedNavigationCompletion::new(token, state, navigation),
        ),
    ))
}
