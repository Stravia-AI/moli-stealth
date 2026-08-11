use super::super::*;
use super::support::CdpPageHarness;
use crate::testing::wait_until_messages;
use serde_json::json;

// P0 browser contract source:
// Chromium Runtime.consoleAPICalled event shape.
#[tokio::test(flavor = "multi_thread")]
async fn rust_cdp_p0_runtime_console_api_event_contract() {
    let mut ctx = TestContext::new_with_target_discovery(false);
    let page = CdpPageHarness::attach(&mut ctx, 134_000).await;
    ctx.sent.clear();

    let response = page
        .evaluate_value(
            &mut ctx,
            134_005,
            "console.log('p0-console', 42); 'console-ok'",
        )
        .await;
    assert_eq!(response["result"]["result"]["value"], json!("console-ok"));

    wait_until_messages(
        &mut ctx,
        Some(page.session_id.as_str()),
        "Runtime.consoleAPICalled event",
        |messages| {
            messages.iter().any(|message| {
                message["method"] == json!("Runtime.consoleAPICalled")
                    && message["params"]["type"] == json!("log")
                    && message["params"]["args"].as_array().is_some_and(|args| {
                        args.first().and_then(|arg| arg["value"].as_str()) == Some("p0-console")
                            && args.get(1).and_then(|arg| arg["value"].as_i64()) == Some(42)
                    })
            })
        },
    )
    .await;
}

// P0 browser contract source:
// Chromium Runtime.exceptionThrown event shape.
#[tokio::test(flavor = "multi_thread")]
async fn rust_cdp_p0_runtime_exception_thrown_event_contract() {
    let mut ctx = TestContext::new_with_target_discovery(false);
    let page = CdpPageHarness::attach(&mut ctx, 135_000).await;
    ctx.sent.clear();

    let response = page
        .evaluate_value(
            &mut ctx,
            135_005,
            "setTimeout(() => { throw new Error('p0 async boom'); }, 0); 'scheduled'",
        )
        .await;
    assert_eq!(response["result"]["result"]["value"], json!("scheduled"));

    wait_until_messages(
        &mut ctx,
        Some(page.session_id.as_str()),
        "Runtime.exceptionThrown event",
        |messages| {
            messages.iter().any(|message| {
                message["method"] == json!("Runtime.exceptionThrown")
                    && message["params"]["exceptionDetails"]["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("p0 async boom"))
            })
        },
    )
    .await;
}

// P0 browser contract source:
// Chromium Runtime execution context lifecycle across navigation.
#[tokio::test(flavor = "multi_thread")]
async fn rust_cdp_p0_runtime_navigation_recreates_default_context() {
    let mut ctx = TestContext::new_with_target_discovery(false);
    let page = CdpPageHarness::attach(&mut ctx, 141_000).await;

    page.navigate(
        &mut ctx,
        141_005,
        "data:text/html,<body>runtime-context-a</body>",
    )
    .await;
    ctx.sent.clear();

    page.navigate(
        &mut ctx,
        141_006,
        "data:text/html,<body>runtime-context-b</body>",
    )
    .await;
    wait_until_messages(
        &mut ctx,
        Some(page.session_id.as_str()),
        "Runtime context cleared and recreated after navigation",
        |messages| {
            messages
                .iter()
                .any(|message| message["method"] == json!("Runtime.executionContextsCleared"))
                && messages.iter().any(|message| {
                    message["method"] == json!("Runtime.executionContextCreated")
                        && message["params"]["context"]["auxData"]["isDefault"] == json!(true)
                        && message["params"]["context"]["auxData"]["frameId"]
                            == json!(page.target_id)
                })
        },
    )
    .await;
    let recreated = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.executionContextCreated")
                && message["params"]["context"]["auxData"]["isDefault"] == json!(true)
                && message["params"]["context"]["auxData"]["frameId"] == json!(page.target_id)
        })
        .unwrap_or_else(|| panic!("recreated default context: {:?}", ctx.sent));
    assert!(
        recreated["params"]["context"]["id"].as_i64().is_some(),
        "{recreated}"
    );
    assert_eq!(
        recreated["params"]["context"]["auxData"]["type"],
        json!("default")
    );
}

// P0 browser contract source:
// Chromium Runtime child-frame default execution context contract.
#[tokio::test(flavor = "multi_thread")]
async fn rust_cdp_p0_runtime_child_frame_default_context_created() {
    let fixture = super::super::tests_cdp_smoke_fixture::SmokeFixtureServer::start().await;
    let mut ctx = TestContext::new_with_target_discovery(false);
    let page = CdpPageHarness::attach(&mut ctx, 147_000).await;
    ctx.enable_page_events_for_test(Some(page.session_id.as_str()));

    page.navigate(&mut ctx, 147_005, fixture.url("/iframe"))
        .await;
    crate::testing::wait_until_message(
        &mut ctx,
        Some(page.session_id.as_str()),
        "child frame attachment after Page.navigate response",
        |message| {
            message["method"] == json!("Page.frameAttached")
                && message["params"]["parentFrameId"] == json!(page.target_id)
        },
    )
    .await;
    let child_frame_id = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Page.frameAttached")
                && message["params"]["parentFrameId"] == json!(page.target_id)
        })
        .and_then(|message| message["params"]["frameId"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("child frameAttached event: {:?}", ctx.sent));
    let child_attached_index = ctx
        .sent
        .iter()
        .position(|message| {
            message["method"] == json!("Page.frameAttached")
                && message["params"]["frameId"] == json!(child_frame_id)
        })
        .unwrap_or_else(|| panic!("child frameAttached index: {:?}", ctx.sent));
    crate::testing::wait_until_message(
        &mut ctx,
        Some(page.session_id.as_str()),
        "child default execution context after frame attachment",
        |message| {
            message["method"] == json!("Runtime.executionContextCreated")
                && message["params"]["context"]["auxData"]["isDefault"] == json!(true)
                && message["params"]["context"]["auxData"]["frameId"] == json!(child_frame_id)
        },
    )
    .await;
    let child_context = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.executionContextCreated")
                && message["params"]["context"]["auxData"]["isDefault"] == json!(true)
                && message["params"]["context"]["auxData"]["frameId"] == json!(child_frame_id)
        })
        .unwrap_or_else(|| panic!("child executionContextCreated: {:?}", ctx.sent));
    let child_context_index = ctx
        .sent
        .iter()
        .position(|message| std::ptr::eq(message, child_context))
        .unwrap_or_else(|| panic!("child executionContextCreated index: {:?}", ctx.sent));
    assert!(
        child_attached_index < child_context_index,
        "Page.frameAttached should precede child Runtime.executionContextCreated; sent={:?}",
        ctx.sent
    );
    assert_eq!(child_context["sessionId"], json!(page.session_id));
    assert!(
        child_context["params"]["context"]["id"].as_i64().is_some(),
        "{child_context}"
    );
    assert_eq!(
        child_context["params"]["context"]["auxData"]["type"],
        json!("default")
    );
}
