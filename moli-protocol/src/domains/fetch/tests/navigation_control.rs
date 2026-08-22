use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

use super::*;
use crate::devtools_runtime::{
    DevToolsCommand, DevToolsCommandContext, DevToolsCommandResult,
    DevToolsFulfillInterceptedRequestCommand, DevToolsProtocol, DevToolsRequestId,
    DevToolsSessionId, DevToolsTargetId,
};

async fn install_loaded_about_blank(ctx: &mut TestContext) {
    ctx.conn.insert_browser_context(attached_browser_context());
    ctx.install_navigation_fixture_for_session_owner("about:blank", Some("SID-1"))
        .await;
    ctx.sent.clear();
    ctx.conn
        .runtime_session_owner_slot_mut(Some("SID-1"))
        .expect("Fetch fixture target")
        .enable_primary_network_events();
}

#[test]
fn parse_binary_response_headers_decodes_nul_separated_header_block() {
    let headers =
        super::response_headers_from_params(None, Some("eC1iaW46IHllcwB4LXR3bzogMgA=".to_owned()))
            .expect("binary response headers");
    assert_eq!(
        headers,
        vec![
            ("x-bin".to_owned(), "yes".to_owned()),
            ("x-two".to_owned(), "2".to_owned())
        ]
    );
}

#[test]
fn parse_binary_response_headers_rejects_invalid_header_name() {
    let encoded = BASE64_STANDARD.encode(b"bad name: value");

    assert!(super::response_headers_from_params(None, Some(encoded)).is_err());
}

#[test]
fn parse_binary_response_headers_rejects_invalid_header_value() {
    let encoded = BASE64_STANDARD.encode(b"x-test: bad\x01value");

    assert!(super::response_headers_from_params(None, Some(encoded)).is_err());
}

#[tokio::test]
async fn continue_request_rejects_invalid_url_without_consuming_pending_navigation() {
    let mut ctx = TestContext::new();
    ctx.conn.insert_browser_context(attached_browser_context());

    ctx.process_async(json!({
        "id": 62,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(62, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 63,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/invalid-url" }
    }))
    .await;

    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("fetch request id")
        .to_owned();

    ctx.process_async(json!({
        "id": 64,
        "method": "Fetch.continueRequest",
        "sessionId": "SID-1",
        "params": {
            "requestId": request_id,
            "url": "::not-a-url::"
        }
    }))
    .await;
    ctx.expect_error(64, -32602, "InvalidParams");

    let bc = ctx.conn.browser_context.as_ref().expect("browser context");
    assert!(
        bc.active_target
            .fetch_owner
            .has_pending_fetch_request_id_for_test(&request_id)
    );
    assert!(
        bc.active_target
            .fetch_owner
            .has_pending_fetch_navigation_for_test(&request_id)
    );
}

#[tokio::test]
async fn continue_request_rejects_invalid_post_data_without_consuming_pending_navigation() {
    let mut ctx = TestContext::new();
    ctx.conn.insert_browser_context(attached_browser_context());

    ctx.process_async(json!({
        "id": 65,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(65, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 66,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/invalid-post-data" }
    }))
    .await;

    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("fetch request id")
        .to_owned();

    ctx.process_async(json!({
        "id": 67,
        "method": "Fetch.continueRequest",
        "sessionId": "SID-1",
        "params": {
            "requestId": request_id,
            "postData": "%%%not-base64%%%"
        }
    }))
    .await;
    ctx.expect_error(67, -32602, "InvalidParams");

    let bc = ctx.conn.browser_context.as_ref().expect("browser context");
    assert!(
        bc.active_target
            .fetch_owner
            .has_pending_fetch_request_id_for_test(&request_id)
    );
    assert!(
        bc.active_target
            .fetch_owner
            .has_pending_fetch_navigation_for_test(&request_id)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn request_paused_then_continue_request_resumes_main_document_navigation() {
    async fn handler() -> impl IntoResponse {
        (
            [(CONTENT_TYPE.as_str(), "text/html")],
            "<!doctype html><html><body><main>continued</main></body></html>",
        )
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/page", get(handler)))
            .await
            .unwrap();
    });

    let mut ctx = TestContext::new();
    ctx.conn.insert_browser_context(attached_browser_context());
    ctx.enable_page_events_for_test(Some("SID-1"));
    ctx.enable_dom_events_for_test(Some("SID-1"));
    let url = format!("http://{addr}/page");

    ctx.process_async(json!({
        "id": 30,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(30, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 31,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": url }
    }))
    .await;

    let paused = take_main_document_request_pause(&mut ctx).await;
    assert_eq!(paused["sessionId"], "SID-1");
    assert_eq!(paused["params"]["resourceType"], "Document");
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("fetch request id")
        .to_owned();

    ctx.process_async(json!({
        "id": 32,
        "method": "Fetch.continueRequest",
        "sessionId": "SID-1",
        "params": { "requestId": request_id }
    }))
    .await;

    ctx.expect_result(32, json!({}), Some("SID-1"));
    wait_until_frame_stopped_loading(&mut ctx, "TID-1").await;
    let messages = ctx.take_all();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["method"] == json!("Network.requestWillBeSent"))
            .count(),
        0
    );
    ctx.sent = messages;
    ctx.expect_result(
        31,
        json!({ "frameId": "TID-1", "loaderId": LOADER_ID }),
        Some("SID-1"),
    );
    assert_eq!(ctx.take_one()["method"], "Page.frameNavigated");
    assert_eq!(ctx.take_one()["method"], "DOM.documentUpdated");
    assert_eq!(ctx.take_one()["method"], "DOM.documentUpdated");
    assert_eq!(ctx.take_one()["method"], "Page.domContentEventFired");
    assert_eq!(ctx.take_one()["method"], "Page.loadEventFired");
    assert_eq!(ctx.take_one()["method"], "Page.frameStoppedLoading");
    assert!(ctx.sent.is_empty());

    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn continue_request_exact_owner_survives_frontend_wait_loss() {
    async fn handler() -> impl IntoResponse {
        (
            [(CONTENT_TYPE.as_str(), "text/html")],
            "<!doctype html><html><body><main>owner continue</main></body></html>",
        )
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/page", get(handler)))
            .await
            .unwrap();
    });
    let mut ctx = TestContext::new();
    install_loaded_about_blank(&mut ctx).await;
    ctx.process_async(json!({
        "id": 30_200,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(30_200, json!({}), Some("SID-1"));
    ctx.process_async(json!({
        "id": 30_201,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": format!("http://{addr}/page") }
    }))
    .await;
    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("Fetch request id")
        .to_owned();
    ctx.take_all();

    let expected_page = ctx
        .conn
        .target_page_residence_identity_for_session(Some("SID-1"))
        .expect("paused navigation Page owner");
    let raw = json!({
        "id": 30_202,
        "method": "Fetch.continueRequest",
        "sessionId": "SID-1",
        "params": { "requestId": request_id }
    })
    .to_string();
    let crate::conn::CdpCommandTaskStep::Pending(frontend_wait) =
        ctx.conn.start_command_dispatch(&raw)
    else {
        panic!("main-Document continueRequest must await Browser Host");
    };
    assert_eq!(ctx.browser_host_ready_len_for_test(), 1);
    let dispatch = ctx.start_one_ready_browser_host_turn_for_test();
    let (_, participant) = dispatch.into_parts();
    let participant = participant.expect("paused navigation Continue participant");
    assert_eq!(
        participant.paused_navigation_decision_page_owner_for_test(),
        Some(&expected_page)
    );
    drop(frontend_wait);

    let dispatch = ctx
        .conn
        .complete_browser_host_turn(participant.wait().await)
        .await;
    let host_outcome = ctx.finish_browser_host_turn_for_test(dispatch).await;
    let (messages, _) = ctx
        .route_completed_command_outcome_for_test(host_outcome)
        .await;
    assert!(messages.iter().all(|message| message["id"] != 30_202));
    assert!(messages.iter().any(|message| {
        message["id"] == 30_201
            && message["sessionId"] == "SID-1"
            && message["result"]["loaderId"] == LOADER_ID
    }));
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_continue_request_owner_participant_cannot_load_successor_page() {
    let mut ctx = TestContext::new();
    install_loaded_about_blank(&mut ctx).await;
    ctx.process_async(json!({
        "id": 30_210,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(30_210, json!({}), Some("SID-1"));
    ctx.process_async(json!({
        "id": 30_211,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/stale-owner-continue" }
    }))
    .await;
    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("Fetch request id")
        .to_owned();
    ctx.take_all();

    let raw = json!({
        "id": 30_212,
        "method": "Fetch.continueRequest",
        "sessionId": "SID-1",
        "params": { "requestId": request_id }
    })
    .to_string();
    let crate::conn::CdpCommandTaskStep::Pending(frontend_wait) =
        ctx.conn.start_command_dispatch(&raw)
    else {
        panic!("main-Document continueRequest must await Browser Host");
    };
    let dispatch = ctx.start_one_ready_browser_host_turn_for_test();
    let (_, participant) = dispatch.into_parts();
    let participant = participant.expect("paused navigation Continue participant");
    let captured_generation = participant
        .paused_navigation_decision_page_owner_for_test()
        .expect("participant Page identity")
        .loaded_page_generation();
    let residence = ctx
        .conn
        .target_page_residence_handle_for_session(Some("SID-1"))
        .expect("Page residence handle");
    assert_eq!(
        residence.advance_generation_for_test_fixture(),
        captured_generation + 1
    );

    let dispatch = ctx
        .conn
        .complete_browser_host_turn(participant.wait().await)
        .await;
    let host_outcome = ctx.finish_browser_host_turn_for_test(dispatch).await;
    let _ = ctx
        .route_completed_command_outcome_for_test(host_outcome)
        .await;
    let completed = frontend_wait.wait().await;
    let crate::conn::CdpCommandTaskStep::Complete(outcome) =
        ctx.conn.complete_pending_command_dispatch(completed).await
    else {
        panic!("stale continueRequest projection should be terminal");
    };
    let (messages, _) = ctx.route_completed_command_outcome_for_test(outcome).await;
    assert!(
        messages
            .iter()
            .any(|message| { message["id"] == 30_212 && message["result"] == json!({}) })
    );
    assert!(messages.iter().any(|message| {
        message["id"] == 30_211 && message["error"]["message"] == "Navigation aborted"
    }));
    assert!(messages.iter().all(|message| {
        message["method"] != "Network.responseReceived"
            && message["method"] != "Page.frameNavigated"
    }));
    assert_eq!(residence.generation(), captured_generation + 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn stopped_browser_host_restores_unmodified_continue_request_navigation() {
    let mut ctx = TestContext::new();
    install_loaded_about_blank(&mut ctx).await;
    ctx.process_async(json!({
        "id": 30_220,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(30_220, json!({}), Some("SID-1"));
    let original_url = "http://example.test/original-owner-continue";
    ctx.process_async(json!({
        "id": 30_221,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": original_url }
    }))
    .await;
    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("Fetch request id")
        .to_owned();
    ctx.take_all();
    ctx.stop_browser_host_for_test();

    let raw = json!({
        "id": 30_222,
        "method": "Fetch.continueRequest",
        "sessionId": "SID-1",
        "params": {
            "requestId": request_id,
            "url": "http://example.test/must-not-apply",
            "method": "POST",
            "postData": BASE64_STANDARD.encode("must-not-apply")
        }
    })
    .to_string();
    let crate::conn::CdpCommandTaskStep::Complete(outcome) = ctx.conn.start_command_dispatch(&raw)
    else {
        panic!("stopped Browser Host must reject without a fallback wait");
    };
    let (messages, _) = ctx.route_completed_command_outcome_for_test(outcome).await;
    assert!(messages.iter().any(|message| {
        message["id"] == 30_222
            && message["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("Browser Host stopped"))
    }));
    let pending = ctx
        .conn
        .browser_context
        .as_ref()
        .expect("browser context")
        .active_target
        .fetch_owner
        .pending_fetch_navigation_for_test(&request_id)
        .expect("restored pending navigation");
    assert_eq!(pending.navigation.requested_url.as_str(), original_url);
    assert_eq!(pending.navigation.request_method, "GET");
    assert!(pending.navigation.request_body.is_none());
}

#[tokio::test]
async fn main_document_request_uses_loader_id_as_observed_network_request_id() {
    let mut ctx = TestContext::new();
    ctx.conn.insert_browser_context(attached_browser_context());

    ctx.process_async(json!({
        "id": 301,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(301, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 302,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/document-request-id" }
    }))
    .await;

    consume_main_document_navigation_start(&mut ctx);
    let request = ctx.take_one();
    assert_eq!(request["method"], "Network.requestWillBeSent");
    assert_eq!(request["params"]["type"], "Document");
    assert_eq!(request["params"]["requestId"], LOADER_ID);
    assert_eq!(request["params"]["loaderId"], LOADER_ID);

    let paused = ctx.take_one();
    assert_eq!(paused["method"], "Fetch.requestPaused");
    assert_eq!(paused["params"]["requestId"], "INT-1");
    assert_eq!(paused["params"]["networkId"], LOADER_ID);
}

#[tokio::test]
async fn fail_request_blocked_by_client_maps_main_document_navigation_to_net_error_text() {
    let mut ctx = TestContext::new();
    let mut bc = attached_browser_context();
    bc.active_target
        .runtime_slot
        .enable_primary_network_events();
    ctx.conn.insert_browser_context(bc);

    ctx.process_async(json!({
        "id": 303,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(303, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 304,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/document-abort" }
    }))
    .await;

    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("fetch request id")
        .to_owned();

    ctx.process_async(json!({
        "id": 305,
        "method": "Fetch.failRequest",
        "sessionId": "SID-1",
        "params": { "requestId": request_id, "errorReason": "BlockedByClient" }
    }))
    .await;

    ctx.expect_result(305, json!({}), Some("SID-1"));
    let failed = ctx.take_one();
    assert_eq!(failed["method"], "Network.loadingFailed");
    assert_eq!(failed["sessionId"], "SID-1");
    assert_eq!(failed["params"]["requestId"], LOADER_ID);
    assert_eq!(failed["params"]["errorText"], "net::ERR_BLOCKED_BY_CLIENT");
    ctx.expect_error(304, -32000, "net::ERR_BLOCKED_BY_CLIENT");
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_request_main_document_is_applied_by_exact_browser_owner_participant() {
    let mut ctx = TestContext::new();
    install_loaded_about_blank(&mut ctx).await;

    ctx.process_async(json!({
        "id": 30_300,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(30_300, json!({}), Some("SID-1"));
    ctx.process_async(json!({
        "id": 30_301,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/owner-fail" }
    }))
    .await;
    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("Fetch request id")
        .to_owned();
    ctx.take_all();

    let expected_page = ctx
        .conn
        .target_page_residence_identity_for_session(Some("SID-1"))
        .expect("paused navigation Page owner");
    let raw = json!({
        "id": 30_302,
        "method": "Fetch.failRequest",
        "sessionId": "SID-1",
        "params": { "requestId": request_id, "errorReason": "BlockedByClient" }
    })
    .to_string();
    let crate::conn::CdpCommandTaskStep::Pending(frontend_wait) =
        ctx.conn.start_command_dispatch(&raw)
    else {
        panic!("main-Document failRequest must await Browser Host");
    };
    assert_eq!(ctx.browser_host_ready_len_for_test(), 1);

    let dispatch = ctx.start_one_ready_browser_host_turn_for_test();
    let (start_outcome, participant) = dispatch.into_parts();
    let (start_events, start_scheduler_events, start_predecessor) =
        start_outcome.into_protocol_event_parts();
    assert!(start_events.is_empty());
    assert!(start_scheduler_events.is_empty());
    assert!(start_predecessor.is_none());
    let participant = participant.expect("paused navigation decision participant");
    assert_eq!(
        participant.paused_navigation_decision_page_owner_for_test(),
        Some(&expected_page),
        "Browser Host must retain the exact Page captured at admission"
    );

    let dispatch = ctx
        .conn
        .complete_browser_host_turn(participant.wait().await)
        .await;
    let host_outcome = ctx.finish_browser_host_turn_for_test(dispatch).await;
    let (host_messages, _) = ctx
        .route_completed_command_outcome_for_test(host_outcome)
        .await;
    assert!(
        host_messages.is_empty(),
        "a live frontend receiver owns the final projection"
    );

    let completed = frontend_wait.wait().await;
    let crate::conn::CdpCommandTaskStep::Complete(outcome) =
        ctx.conn.complete_pending_command_dispatch(completed).await
    else {
        panic!("Browser Owner failRequest projection should be terminal");
    };
    let (messages, _) = ctx.route_completed_command_outcome_for_test(outcome).await;
    let fail_response = messages
        .iter()
        .position(|message| message["id"] == 30_302)
        .expect("Fetch.failRequest response");
    let loading_failed = messages
        .iter()
        .position(|message| message["method"] == "Network.loadingFailed")
        .expect("navigation loadingFailed event");
    let navigate_response = messages
        .iter()
        .position(|message| message["id"] == 30_301)
        .expect("original Page.navigate response");
    assert!(fail_response < loading_failed);
    assert!(loading_failed < navigate_response);
    assert_eq!(messages[fail_response]["result"], json!({}));
    assert_eq!(
        messages[loading_failed]["params"]["errorText"],
        "net::ERR_BLOCKED_BY_CLIENT"
    );
    assert_eq!(
        messages[navigate_response]["error"]["message"],
        "net::ERR_BLOCKED_BY_CLIENT"
    );
    let facts = ctx.conn.browser_fact_snapshot_for_test();
    let failed_navigation = facts
        .iter()
        .find(|fact| {
            matches!(
                fact.fact(),
                moli_core::browser_host::BrowserFact::NavigationFailed {
                    failure:
                        moli_core::browser_host::BrowserNavigationFailure::Network {
                            error_text,
                        },
                    ..
                } if error_text == "net::ERR_BLOCKED_BY_CLIENT"
            )
        })
        .expect("Fetch.failRequest should publish a failed navigation terminal");
    let moli_core::browser_host::BrowserFact::NavigationFailed { previous_page, .. } =
        failed_navigation.fact()
    else {
        unreachable!("the fact was selected as NavigationFailed")
    };
    assert_eq!(previous_page.as_ref(), Some(&expected_page));
    assert_eq!(
        failed_navigation.page_residence().loaded_page_generation(),
        expected_page.loaded_page_generation() + 1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_fail_request_owner_participant_cannot_invalidate_successor_page() {
    let mut ctx = TestContext::new();
    install_loaded_about_blank(&mut ctx).await;
    ctx.process_async(json!({
        "id": 30_310,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(30_310, json!({}), Some("SID-1"));
    ctx.process_async(json!({
        "id": 30_311,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/stale-owner-fail" }
    }))
    .await;
    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("Fetch request id")
        .to_owned();
    ctx.take_all();

    let raw = json!({
        "id": 30_312,
        "method": "Fetch.failRequest",
        "sessionId": "SID-1",
        "params": { "requestId": request_id, "errorReason": "Aborted" }
    })
    .to_string();
    let crate::conn::CdpCommandTaskStep::Pending(frontend_wait) =
        ctx.conn.start_command_dispatch(&raw)
    else {
        panic!("main-Document failRequest must await Browser Host");
    };
    let dispatch = ctx.start_one_ready_browser_host_turn_for_test();
    let (_, participant) = dispatch.into_parts();
    let participant = participant.expect("paused navigation decision participant");
    let captured_generation = participant
        .paused_navigation_decision_page_owner_for_test()
        .expect("participant Page identity")
        .loaded_page_generation();
    let residence = ctx
        .conn
        .target_page_residence_handle_for_session(Some("SID-1"))
        .expect("Page residence handle");
    assert_eq!(
        residence.advance_generation_for_test_fixture(),
        captured_generation + 1
    );

    let dispatch = ctx
        .conn
        .complete_browser_host_turn(participant.wait().await)
        .await;
    let host_outcome = ctx.finish_browser_host_turn_for_test(dispatch).await;
    let _ = ctx
        .route_completed_command_outcome_for_test(host_outcome)
        .await;
    let completed = frontend_wait.wait().await;
    let crate::conn::CdpCommandTaskStep::Complete(outcome) =
        ctx.conn.complete_pending_command_dispatch(completed).await
    else {
        panic!("stale failRequest projection should be terminal");
    };
    let (messages, _) = ctx.route_completed_command_outcome_for_test(outcome).await;
    assert!(
        messages
            .iter()
            .any(|message| { message["id"] == 30_312 && message["result"] == json!({}) })
    );
    assert!(messages.iter().any(|message| {
        message["id"] == 30_311 && message["error"]["message"] == "Navigation aborted"
    }));
    assert!(
        messages
            .iter()
            .all(|message| message["method"] != "Network.loadingFailed"),
        "stale work must not project a failure through the successor Page"
    );
    assert_eq!(residence.generation(), captured_generation + 1);
    assert!(
        ctx.conn
            .browser_context
            .as_ref()
            .expect("browser context")
            .has_loaded_page(),
        "stale failure must not discard the successor physical Page"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_fail_request_frontend_wait_does_not_cancel_browser_decision() {
    let mut ctx = TestContext::new();
    install_loaded_about_blank(&mut ctx).await;
    ctx.process_async(json!({
        "id": 30_320,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(30_320, json!({}), Some("SID-1"));
    ctx.process_async(json!({
        "id": 30_321,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/detached-owner-fail" }
    }))
    .await;
    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("Fetch request id")
        .to_owned();
    ctx.take_all();

    let raw = json!({
        "id": 30_322,
        "method": "Fetch.failRequest",
        "sessionId": "SID-1",
        "params": { "requestId": request_id, "errorReason": "Aborted" }
    })
    .to_string();
    let crate::conn::CdpCommandTaskStep::Pending(frontend_wait) =
        ctx.conn.start_command_dispatch(&raw)
    else {
        panic!("main-Document failRequest must await Browser Host");
    };
    drop(frontend_wait);

    let dispatch = ctx.start_one_ready_browser_host_turn_for_test();
    let host_outcome = ctx.finish_browser_host_turn_for_test(dispatch).await;
    let (messages, _) = ctx
        .route_completed_command_outcome_for_test(host_outcome)
        .await;
    assert!(messages.iter().all(|message| message["id"] != 30_322));
    assert!(
        messages
            .iter()
            .any(|message| { message["id"] == 30_321 && message["error"]["message"] == "Aborted" })
    );
    assert!(messages.iter().any(|message| {
        message["method"] == "Network.loadingFailed" && message["params"]["errorText"] == "Aborted"
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn stopped_browser_host_restores_fail_request_navigation_without_direct_fallback() {
    let mut ctx = TestContext::new();
    install_loaded_about_blank(&mut ctx).await;
    ctx.process_async(json!({
        "id": 30_330,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(30_330, json!({}), Some("SID-1"));
    ctx.process_async(json!({
        "id": 30_331,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/stopped-host-owner-fail" }
    }))
    .await;
    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("Fetch request id")
        .to_owned();
    ctx.take_all();
    ctx.stop_browser_host_for_test();

    let raw = json!({
        "id": 30_332,
        "method": "Fetch.failRequest",
        "sessionId": "SID-1",
        "params": { "requestId": request_id, "errorReason": "Aborted" }
    })
    .to_string();
    let crate::conn::CdpCommandTaskStep::Complete(outcome) = ctx.conn.start_command_dispatch(&raw)
    else {
        panic!("stopped Browser Host must reject without a fallback wait");
    };
    let (messages, _) = ctx.route_completed_command_outcome_for_test(outcome).await;
    assert!(messages.iter().any(|message| {
        message["id"] == 30_332
            && message["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("Browser Host stopped"))
    }));
    let fetch_owner = &ctx
        .conn
        .browser_context
        .as_ref()
        .expect("browser context")
        .active_target
        .fetch_owner;
    assert!(fetch_owner.has_pending_fetch_request_id_for_test(&request_id));
    assert!(fetch_owner.has_pending_fetch_navigation_for_test(&request_id));
}

#[tokio::test(flavor = "multi_thread")]
async fn fulfill_request_main_document_is_selected_by_exact_browser_owner_after_frontend_drop() {
    let mut ctx = TestContext::new();
    install_loaded_about_blank(&mut ctx).await;
    ctx.process_async(json!({
        "id": 30_340,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(30_340, json!({}), Some("SID-1"));
    ctx.process_async(json!({
        "id": 30_341,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/owner-fulfill" }
    }))
    .await;
    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("Fetch request id")
        .to_owned();
    ctx.take_all();

    let expected_page = ctx
        .conn
        .target_page_residence_identity_for_session(Some("SID-1"))
        .expect("paused navigation Page owner");
    let raw = json!({
        "id": 30_342,
        "method": "Fetch.fulfillRequest",
        "sessionId": "SID-1",
        "params": {
            "requestId": request_id,
            "responseCode": 201,
            "responseHeaders": [{ "name": "content-type", "value": "text/html" }],
            "body": "PCFkb2N0eXBlIGh0bWw+PG1haW4+b3duZXItZnVsZmlsbC1maW5pc2hlZDwvbWFpbj4="
        }
    })
    .to_string();
    let crate::conn::CdpCommandTaskStep::Pending(frontend_wait) =
        ctx.conn.start_command_dispatch(&raw)
    else {
        panic!("main-Document fulfillRequest must await Browser Host");
    };
    assert_eq!(ctx.browser_host_ready_len_for_test(), 1);
    let dispatch = ctx.start_one_ready_browser_host_turn_for_test();
    let (_, participant) = dispatch.into_parts();
    let participant = participant.expect("synthetic navigation participant");
    assert_eq!(
        participant.paused_navigation_decision_page_owner_for_test(),
        Some(&expected_page),
        "Browser Host must retain the exact Page captured at fulfill admission"
    );
    drop(frontend_wait);

    let dispatch = ctx
        .conn
        .complete_browser_host_turn(participant.wait().await)
        .await;
    let host_outcome = ctx.finish_browser_host_turn_for_test(dispatch).await;
    let (messages, _) = ctx
        .route_completed_command_outcome_for_test(host_outcome)
        .await;
    assert!(messages.iter().all(|message| message["id"] != 30_342));
    assert!(
        messages
            .iter()
            .any(|message| message["id"] == 30_341 && message["result"]["frameId"] == "TID-1"),
        "dropping the Fetch frontend must not cancel the accepted synthetic navigation"
    );
    assert!(
        loaded_page_html_for_test(&mut ctx)
            .await
            .contains("owner-fulfill-finished")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_fulfill_request_exposes_owner_wait_without_borrowing_connection() {
    let mut ctx = TestContext::new();
    install_loaded_about_blank(&mut ctx).await;
    ctx.process_async(json!({
        "id": 30_350,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(30_350, json!({}), Some("SID-1"));
    ctx.process_async(json!({
        "id": 30_351,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/typed-owner-fulfill" }
    }))
    .await;
    let paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("Fetch request id")
        .to_owned();
    ctx.take_all();

    let expected_page = ctx
        .conn
        .target_page_residence_identity_for_session(Some("SID-1"))
        .expect("paused navigation Page owner");
    let command =
        DevToolsCommand::FulfillInterceptedRequest(DevToolsFulfillInterceptedRequestCommand {
            context: DevToolsCommandContext {
                protocol: DevToolsProtocol::WebDriverBidi,
                session_id: Some(DevToolsSessionId::from("SID-1")),
                target_id: Some(DevToolsTargetId::from("TID-1")),
                browser_context_id: None,
            },
            request_id: DevToolsRequestId::from(request_id.as_str()),
            response_code: 202,
            response_headers: vec![("content-type".to_owned(), "text/html".to_owned())],
            body: Some(b"<!doctype html><main>typed-owner-finished</main>".to_vec()),
            response_phrase: None,
        });
    let step = ctx
        .conn
        .try_start_devtools_fetch_command_task(command)
        .await
        .expect("typed terminal Fetch command must use its scheduler-facing task");
    let crate::domains::fetch::DevToolsFetchCommandTaskStep::Pending(pending) = step else {
        panic!("typed main-Document fulfill must await Browser Host");
    };
    assert!(pending.holds_navigation_renderer_publication_gate());
    assert_eq!(ctx.browser_host_ready_len_for_test(), 1);

    let dispatch = ctx.start_one_ready_browser_host_turn_for_test();
    let (_, participant) = dispatch.into_parts();
    let participant = participant.expect("typed synthetic navigation participant");
    assert_eq!(
        participant.paused_navigation_decision_page_owner_for_test(),
        Some(&expected_page)
    );
    let dispatch = ctx
        .conn
        .complete_browser_host_turn(participant.wait().await)
        .await;
    let host_outcome = ctx.finish_browser_host_turn_for_test(dispatch).await;
    let (mut messages, _) = ctx
        .route_completed_command_outcome_for_test(host_outcome)
        .await;

    let completed = pending.wait().await;
    let outcome = ctx
        .conn
        .complete_devtools_fetch_command_task(completed)
        .await;
    let (
        result,
        scheduler_events,
        mut protocol_events,
        renderer_output_boundary,
        mut post_renderer_output_events,
        mut post_response_events,
        renderer_output_predecessor,
    ) = outcome.into_fenced_complete_parts();
    assert_eq!(
        result.expect("typed fulfill should project an empty result"),
        DevToolsCommandResult::Empty
    );
    assert!(scheduler_events.is_empty());
    protocol_events.append(&mut post_renderer_output_events);
    protocol_events.append(&mut post_response_events);
    messages.extend(
        protocol_events
            .into_iter()
            .map(crate::conn::BackgroundProtocolEvent::into_protocol_message),
    );
    assert!(
        messages
            .iter()
            .any(|message| message["id"] == 30_351 && message["result"]["frameId"] == "TID-1"),
        "typed owner completion must settle the original navigation: {messages:?}"
    );
    assert!(renderer_output_boundary.is_none());
    assert!(renderer_output_predecessor.is_none());
    assert!(
        loaded_page_html_for_test(&mut ctx)
            .await
            .contains("typed-owner-finished")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn request_paused_then_continue_request_fails_when_network_offline() {
    let mut ctx = TestContext::new();
    ctx.conn.insert_browser_context(attached_browser_context());

    ctx.process_async(json!({
        "id": 16630,
        "method": "Network.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(16630, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 16631,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(16631, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 16632,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/offline" }
    }))
    .await;

    let paused = take_main_document_request_pause(&mut ctx).await;
    assert_eq!(paused["method"], "Fetch.requestPaused");
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("fetch request id")
        .to_owned();

    ctx.process_async(json!({
        "id": 16633,
        "method": "Network.emulateNetworkConditions",
        "params": {
            "offline": true,
            "latency": 0,
            "downloadThroughput": -1,
            "uploadThroughput": -1
        }
    }))
    .await;
    ctx.expect_result(16633, json!({}), None);

    ctx.process_async(json!({
        "id": 16634,
        "method": "Fetch.continueRequest",
        "sessionId": "SID-1",
        "params": { "requestId": request_id }
    }))
    .await;
    ctx.expect_result(16634, json!({}), Some("SID-1"));
    let failed = ctx.take_one();
    assert_eq!(failed["method"], "Network.loadingFailed");
    assert_eq!(failed["sessionId"], "SID-1");
    assert_eq!(failed["params"]["errorText"], "Network emulation offline");
    ctx.expect_error(16632, -32000, "Network emulation offline");
}

#[tokio::test(flavor = "multi_thread")]
async fn response_stage_document_pattern_pauses_main_document_after_response() {
    async fn handler() -> impl IntoResponse {
        (
            [
                (CONTENT_TYPE.as_str(), "text/html"),
                ("x-stage", "response"),
            ],
            "<!doctype html><html><body><main>response-stage</main></body></html>",
        )
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/page", get(handler)))
            .await
            .unwrap();
    });

    let mut ctx = TestContext::new();
    let mut bc = attached_browser_context();
    bc.active_target
        .runtime_slot
        .enable_primary_network_events();
    let url = format!("http://{addr}/page");
    {
        let mut jar = bc.cookie_store_for_test().lock();
        jar.store_response_headers(
            &Url::parse(&url).unwrap(),
            &[("set-cookie".to_owned(), "sid=1; Path=/".to_owned())],
        );
    }
    ctx.conn.insert_browser_context(bc);

    ctx.process_async(json!({
            "id": 33,
            "method": "Fetch.enable",
            "sessionId": "SID-1",
            "params": {
                "patterns": [{ "urlPattern": "*", "requestStage": "Response", "resourceType": "Document" }]
            }
        })).await;
    ctx.expect_result(33, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 34,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": url }
    }))
    .await;

    consume_main_document_navigation_start(&mut ctx);
    let request = ctx.take_one();
    assert_eq!(request["method"], "Network.requestWillBeSent");
    let network_id = request["params"]["requestId"].as_str().unwrap().to_owned();
    let extra_info = ctx.take_one();
    assert_eq!(extra_info["method"], "Network.requestWillBeSentExtraInfo");
    assert_eq!(extra_info["params"]["requestId"], json!(network_id));
    let response_extra_info = ctx.take_one();
    assert_eq!(
        response_extra_info["method"],
        "Network.responseReceivedExtraInfo"
    );
    assert_eq!(
        response_extra_info["params"]["requestId"],
        json!(network_id)
    );
    assert_eq!(response_extra_info["params"]["statusCode"], 200);
    let paused = ctx.take_one();
    assert_eq!(paused["method"], "Fetch.requestPaused");
    assert_eq!(paused["params"]["resourceType"], "Document");
    assert_eq!(paused["params"]["networkId"], json!(network_id));
    assert_eq!(paused["params"]["responseStatusCode"], 200);
    assert_eq!(paused["params"]["responseHeaders"][1]["name"], "x-stage");
    assert!(ctx.sent.iter().all(|message| {
        message["method"] != json!("Fetch.requestPaused")
            || message["params"]["responseStatusCode"].is_number()
    }));
    let request_id = paused["params"]["requestId"].as_str().unwrap().to_owned();
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 35,
        "method": "Fetch.continueResponse",
        "sessionId": "SID-1",
        "params": { "requestId": request_id }
    }))
    .await;

    ctx.expect_result(35, json!({}), Some("SID-1"));
    let messages = ctx.take_all();
    let request_events = messages
        .iter()
        .filter(|message| message["method"] == json!("Network.requestWillBeSent"))
        .collect::<Vec<_>>();
    assert_eq!(request_events.len(), 0);
    let extra_info_events = messages
        .iter()
        .filter(|message| message["method"] == json!("Network.requestWillBeSentExtraInfo"))
        .collect::<Vec<_>>();
    assert_eq!(extra_info_events.len(), 0);
    let response_extra_info_events = messages
        .iter()
        .filter(|message| message["method"] == json!("Network.responseReceivedExtraInfo"))
        .collect::<Vec<_>>();
    assert_eq!(response_extra_info_events.len(), 0);
    let response_paused_events = messages
        .iter()
        .filter(|message| message["method"] == json!("Fetch.requestPaused"))
        .collect::<Vec<_>>();
    assert_eq!(response_paused_events.len(), 0);
    ctx.sent = messages;
    ctx.expect_result(
        34,
        json!({ "frameId": "TID-1", "loaderId": LOADER_ID }),
        Some("SID-1"),
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn document_url_pattern_only_pauses_matching_main_document() {
    async fn plain() -> impl IntoResponse {
        (
            [(CONTENT_TYPE.as_str(), "text/html")],
            "<!doctype html><html><body><main>plain</main></body></html>",
        )
    }

    async fn matched() -> impl IntoResponse {
        (
            [(CONTENT_TYPE.as_str(), "text/html")],
            "<!doctype html><html><body><main>matched</main></body></html>",
        )
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/plain", get(plain))
                .route("/match", get(matched)),
        )
        .await
        .unwrap();
    });

    let plain_url = format!("http://{addr}/plain");
    let match_url = format!("http://{addr}/match");
    let mut ctx = TestContext::new();
    let mut bc = BrowserContext::new("BID-1".into());
    bc.set_active_target_id("TID-1");
    bc.attach_active_session("SID-1");
    ctx.conn.insert_browser_context(bc);

    ctx.process_async(json!({
            "id": 394,
            "method": "Fetch.enable",
            "sessionId": "SID-1",
            "params": {
                "patterns": [{ "urlPattern": "*/match", "requestStage": "Request", "resourceType": "Document" }]
            }
        })).await;
    ctx.expect_result(394, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 395,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": plain_url }
    }))
    .await;
    let _ = take_response_by_id(&mut ctx, 395);
    assert!(
        !ctx.sent
            .iter()
            .any(|message| message["method"] == json!("Fetch.requestPaused")),
        "non-matching document url should not be paused"
    );
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 396,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": match_url }
    }))
    .await;
    let paused = take_main_document_request_pause(&mut ctx).await;
    assert_eq!(paused["method"], "Fetch.requestPaused");
    assert_eq!(paused["params"]["request"]["url"], json!(match_url));

    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn continue_request_with_post_data_marks_network_request_as_having_post_data() {
    async fn handler(body: String) -> impl IntoResponse {
        (
            [(CONTENT_TYPE.as_str(), "text/html")],
            format!("<!doctype html><html><body><main>{body}</main></body></html>"),
        )
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/page", axum::routing::post(handler)),
        )
        .await
        .unwrap();
    });

    let mut ctx = TestContext::new();
    let mut bc = attached_browser_context();
    bc.active_target
        .runtime_slot
        .enable_primary_network_events();
    ctx.conn.insert_browser_context(bc);
    let url = format!("http://{addr}/page");

    ctx.process_async(json!({
        "id": 340,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(340, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 341,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": url }
    }))
    .await;

    consume_main_document_navigation_start(&mut ctx);
    let initial_request = ctx.take_one();
    assert_eq!(initial_request["method"], "Network.requestWillBeSent");
    assert_eq!(initial_request["params"]["request"]["method"], "GET");
    assert_eq!(initial_request["params"]["request"]["hasPostData"], false);
    let paused = ctx.take_one();
    let request_id = paused["params"]["requestId"]
        .as_str()
        .expect("fetch request id")
        .to_owned();

    ctx.process_async(json!({
        "id": 342,
        "method": "Fetch.continueRequest",
        "sessionId": "SID-1",
        "params": {
            "requestId": request_id,
            "method": "POST",
            "postData": "cGF5bG9hZA=="
        }
    }))
    .await;
    ctx.expect_result(342, json!({}), Some("SID-1"));

    ctx.expect_result(
        341,
        json!({ "frameId": "TID-1", "loaderId": LOADER_ID }),
        Some("SID-1"),
    );
    let response = ctx
        .sent
        .iter()
        .find(|message| message["method"] == json!("Network.responseReceived"))
        .cloned()
        .expect("Network.responseReceived after continueRequest");
    assert_eq!(response["params"]["requestId"], LOADER_ID);
    let bc = ctx.conn.browser_context.as_ref().expect("browser context");
    assert_eq!(
        bc.captured_response_body(LOADER_ID).map(|body| body.body()),
        Some("<!doctype html><html><body><main>payload</main></body></html>".to_owned())
    );

    ctx.process_async(json!({
        "id": 343,
        "method": "Network.getRequestPostData",
        "sessionId": "SID-1",
        "params": { "requestId": LOADER_ID }
    }))
    .await;
    ctx.expect_result(
        343,
        json!({ "postData": "payload", "base64Encoded": false }),
        Some("SID-1"),
    );

    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn intercepted_form_post_navigation_body_is_available_by_network_request_id() {
    async fn page() -> impl IntoResponse {
        (
            [(CONTENT_TYPE.as_str(), "text/html")],
            r#"<!doctype html><form id="f" method="POST" action="/post"><input name="username" value="alice"><input name="pw" value="s3cret"></form>"#,
        )
    }

    async fn post_body(body: String) -> impl IntoResponse {
        (
            [(CONTENT_TYPE.as_str(), "text/html")],
            format!("<!doctype html><html><body><main>{body}</main></body></html>"),
        )
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/page", get(page))
                .route("/post", axum::routing::post(post_body)),
        )
        .await
        .unwrap();
    });

    let page_url = format!("http://{addr}/page");
    let post_url = format!("http://{addr}/post");
    let mut ctx = TestContext::new();
    ctx.conn.browser_context = Some(attached_browser_context());
    ctx.install_navigation_fixture_for_session_owner(&page_url, Some("SID-1"))
        .await;
    {
        let bc = ctx.conn.browser_context.as_mut().expect("browser context");
        bc.active_target
            .runtime_slot
            .enable_primary_network_events();
    }
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 420,
        "method": "Fetch.enable",
        "sessionId": "SID-1",
        "params": {
            "patterns": [{ "urlPattern": "*/post", "requestStage": "Request", "resourceType": "Document" }]
        }
    }))
    .await;
    ctx.expect_result(420, json!({}), Some("SID-1"));
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 421,
        "method": "Runtime.evaluate",
        "sessionId": "SID-1",
        "params": {
            "expression": "document.getElementById('f').submit(); 'scheduled'"
        }
    }))
    .await;
    let _ = take_response_by_id(&mut ctx, 421);

    wait_until_scheduler_message(
        &mut ctx,
        "intercepted form POST Fetch.requestPaused",
        |message| {
            message["method"] == json!("Fetch.requestPaused")
                && message["params"]["resourceType"] == json!("Document")
                && message["params"]["request"]["url"] == json!(post_url)
        },
    )
    .await;
    let paused = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Fetch.requestPaused")
                && message["params"]["resourceType"] == json!("Document")
                && message["params"]["request"]["url"] == json!(post_url)
        })
        .cloned()
        .expect("intercepted form POST pause");
    assert_eq!(paused["params"]["request"]["method"], json!("POST"));
    assert_eq!(
        paused["params"]["request"]["postData"],
        json!("username=alice&pw=s3cret"),
        "the paused form POST must carry its original body"
    );
    let fetch_request_id = paused["params"]["requestId"]
        .as_str()
        .expect("fetch request id")
        .to_owned();
    let network_request_id = paused["params"]["networkId"]
        .as_str()
        .expect("network request id")
        .to_owned();

    ctx.process_async(json!({
        "id": 422,
        "method": "Fetch.continueRequest",
        "sessionId": "SID-1",
        "params": { "requestId": fetch_request_id }
    }))
    .await;
    ctx.expect_result(422, json!({}), Some("SID-1"));

    wait_until_scheduler_message(
        &mut ctx,
        "intercepted form POST responseReceived",
        |message| {
            message["method"] == json!("Network.responseReceived")
                && message["params"]["requestId"] == json!(network_request_id)
        },
    )
    .await;

    ctx.process_async(json!({
        "id": 423,
        "method": "Network.getRequestPostData",
        "sessionId": "SID-1",
        "params": { "requestId": network_request_id }
    }))
    .await;
    ctx.expect_result(
        423,
        json!({ "postData": "username=alice&pw=s3cret", "base64Encoded": false }),
        Some("SID-1"),
    );

    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn continue_request_with_intercept_response_pauses_after_response_until_continue_response() {
    async fn handler() -> impl IntoResponse {
        (
            [(CONTENT_TYPE.as_str(), "text/html")],
            "<!doctype html><html><body><main>response-stage</main></body></html>",
        )
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/page", get(handler)))
            .await
            .unwrap();
    });

    let mut ctx = TestContext::new();
    let mut bc = attached_browser_context();
    bc.active_target
        .runtime_slot
        .enable_primary_network_events();
    ctx.conn.insert_browser_context(bc);
    ctx.enable_page_events_for_test(Some("SID-1"));
    ctx.enable_dom_events_for_test(Some("SID-1"));
    let url = format!("http://{addr}/page");

    ctx.process_async(json!({
        "id": 320,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(320, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 321,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": url }
    }))
    .await;

    let request_paused = take_main_document_request_pause(&mut ctx).await;
    let request_id = request_paused["params"]["requestId"]
        .as_str()
        .expect("fetch request id")
        .to_owned();
    assert_eq!(request_paused["params"]["networkId"], LOADER_ID);

    ctx.process_async(json!({
        "id": 322,
        "method": "Fetch.continueRequest",
        "sessionId": "SID-1",
        "params": { "requestId": request_id, "interceptResponse": true }
    }))
    .await;
    ctx.expect_result(322, json!({}), Some("SID-1"));

    assert!(
        !ctx.sent
            .iter()
            .any(|message| message["method"] == json!("Network.requestWillBeSent")),
        "continueRequest(interceptResponse=true) should not re-emit Network.requestWillBeSent"
    );
    wait_until_scheduler_message(
        &mut ctx,
        "response-stage Fetch.requestPaused after continueRequest",
        |message| {
            message["method"] == json!("Fetch.requestPaused")
                && message["params"]["responseStatusCode"].is_number()
        },
    )
    .await;
    let request_extra_info = ctx.take_one();
    assert_eq!(
        request_extra_info["method"],
        "Network.requestWillBeSentExtraInfo"
    );
    assert_eq!(request_extra_info["params"]["requestId"], LOADER_ID);
    let response_extra_info = ctx.take_one();
    assert_eq!(
        response_extra_info["method"],
        "Network.responseReceivedExtraInfo"
    );
    assert_eq!(response_extra_info["params"]["requestId"], LOADER_ID);
    assert_eq!(response_extra_info["params"]["statusCode"], 200);
    let response_paused = ctx.take_one();
    assert_eq!(response_paused["method"], "Fetch.requestPaused");
    assert_eq!(response_paused["params"]["requestId"], "INT-1");
    assert_eq!(response_paused["params"]["networkId"], LOADER_ID);
    assert_eq!(response_paused["params"]["responseStatusCode"], 200);
    assert_eq!(
        response_paused["params"]["responseHeaders"][0]["name"],
        "content-type"
    );

    ctx.process_async(json!({
        "id": 323,
        "method": "Fetch.getResponseBody",
        "sessionId": "SID-1",
        "params": { "requestId": "INT-1" }
    }))
    .await;
    ctx.expect_result(
        323,
        json!({
            "body": "<!doctype html><html><body><main>response-stage</main></body></html>",
            "base64Encoded": false
        }),
        Some("SID-1"),
    );

    assert!(ctx.sent.is_empty());

    ctx.process_async(json!({
        "id": 324,
        "method": "Fetch.continueResponse",
        "sessionId": "SID-1",
        "params": { "requestId": "INT-1" }
    }))
    .await;
    ctx.expect_result(324, json!({}), Some("SID-1"));
    ctx.expect_result(
        321,
        json!({ "frameId": "TID-1", "loaderId": LOADER_ID }),
        Some("SID-1"),
    );
    wait_until_frame_stopped_loading(&mut ctx, "TID-1").await;
    assert_eq!(ctx.take_one()["method"], "Network.responseReceived");
    assert_eq!(ctx.take_one()["method"], "Page.frameNavigated");
    assert_eq!(ctx.take_one()["method"], "DOM.documentUpdated");
    assert_eq!(ctx.take_one()["method"], "DOM.documentUpdated");
    assert_eq!(ctx.take_one()["method"], "Page.domContentEventFired");
    let data = ctx.take_one();
    assert_eq!(data["method"], "Network.dataReceived");
    assert_eq!(data["params"]["requestId"], LOADER_ID);
    assert_eq!(ctx.take_one()["method"], "Network.loadingFinished");
    assert_eq!(ctx.take_one()["method"], "Page.loadEventFired");
    assert_eq!(ctx.take_one()["method"], "Page.frameStoppedLoading");

    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn intercepted_navigation_start_events_stay_before_network_pause_with_background_sender() {
    let mut ctx = TestContext::new();
    let mut bc = attached_browser_context();
    bc.active_target
        .runtime_slot
        .enable_primary_network_events();
    ctx.conn.insert_browser_context(bc);
    let (sender, mut receiver) = crate::conn::browser_background_output_channel();
    ctx.conn.set_background_event_sender(sender);

    ctx.process_async(json!({
        "id": 330,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(330, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 331,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": "http://example.test/intercepted" }
    }))
    .await;

    assert_eq!(ctx.take_one()["method"], "Page.frameStartedNavigating");
    assert_eq!(ctx.take_one()["method"], "Page.frameStartedLoading");
    let request = ctx.take_one();
    assert_eq!(request["method"], "Network.requestWillBeSent");
    assert_eq!(request["params"]["requestId"], LOADER_ID);
    let paused = ctx.take_one();
    assert_eq!(paused["method"], "Fetch.requestPaused");
    assert_eq!(paused["params"]["networkId"], LOADER_ID);
    assert!(ctx.sent.is_empty());
    assert!(
        receiver.try_recv().is_err(),
        "Fetch-paused navigation start events should not race through background output"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn disable_aborts_paused_main_document_navigation() {
    async fn page() -> impl IntoResponse {
        (
            [(CONTENT_TYPE.as_str(), "text/html")],
            "<!doctype html><html><body>main doc</body></html>",
        )
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/page", get(page)))
            .await
            .unwrap();
    });

    let url = format!("http://{addr}/page");
    let mut ctx = TestContext::new();
    let mut bc = attached_browser_context();
    bc.active_target
        .runtime_slot
        .enable_primary_network_events();
    ctx.conn.insert_browser_context(bc);

    ctx.process_async(json!({
        "id": 78,
        "method": "Fetch.enable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(78, json!({}), Some("SID-1"));

    ctx.process_async(json!({
        "id": 79,
        "method": "Page.navigate",
        "sessionId": "SID-1",
        "params": { "url": url }
    }))
    .await;

    let paused = take_main_document_request_pause(&mut ctx).await;
    assert_eq!(paused["method"], "Fetch.requestPaused");
    assert_eq!(paused["params"]["resourceType"], "Document");
    let network_id = paused["params"]["networkId"].clone();

    ctx.process_async(json!({
        "id": 80,
        "method": "Fetch.disable",
        "sessionId": "SID-1"
    }))
    .await;
    ctx.expect_result(80, json!({}), Some("SID-1"));

    let failed = ctx.take_one();
    assert_eq!(failed["method"], "Network.loadingFailed");
    assert_eq!(failed["params"]["requestId"], network_id);
    assert_eq!(failed["params"]["errorText"], "Fetch interception disabled");

    let navigate_error = ctx.take_one();
    assert_eq!(navigate_error["id"], 79);
    assert_eq!(navigate_error["error"]["code"], -32000);
    assert_eq!(
        navigate_error["error"]["message"],
        "Fetch interception disabled"
    );

    server.abort();
}
