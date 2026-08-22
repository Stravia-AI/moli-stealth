use super::*;

fn run_patchright_runtimeless_large_stack<F, Fut>(thread_name: &str, future_factory: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    let result = std::thread::Builder::new()
        .name(thread_name.to_owned())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("large-stack patchright runtimeless test runtime should build")
                .block_on(future_factory());
        })
        .expect("large-stack patchright runtimeless test thread should spawn")
        .join();

    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_switching_back_to_first_context_keeps_older_page_runtime_and_utility_world_alive_without_runtime_enable()
 {
    let mut ctx = TestContext::new();
    let first =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 23910, 23911, 23912)
            .await;

    ctx.process_async(json!({
        "id": 23913,
        "method": "Page.addScriptToEvaluateOnNewDocument",
        "sessionId": first.session_id,
        "params": {
            "source": "globalThis.__lm_reactivated_utility_preload = 'first-preload';",
            "worldName": "utility"
        }
    }))
    .await;
    let preload = take_response_by_id(&mut ctx, 23913);
    assert_eq!(preload["sessionId"], json!(first.session_id));
    assert!(preload["result"]["identifier"].is_string());

    ctx.process_async(json!({
        "id": 23914,
        "method": "Runtime.addBinding",
        "sessionId": first.session_id,
        "params": {
            "name": "reactivatedUtilityBinding",
            "executionContextName": "utility"
        }
    }))
    .await;
    let add_binding = take_response_by_id(&mut ctx, 23914);
    assert_eq!(add_binding["result"], json!({}));
    assert!(
        !ctx.sent.iter().any(|message| {
            message["method"] == json!("Runtime.executionContextCreated")
                || message["method"] == json!("Runtime.executionContextsCleared")
        }),
        "Patchright-style setup should stay off Runtime.enable surfaces: {:?}",
        ctx.sent
    );
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 23915,
        "method": "Page.navigate",
        "sessionId": first.session_id,
        "params": {
            "url": "data:text/html,<body><div id=page>first-page</div></body>"
        }
    }))
    .await;
    let first_navigation = take_response_by_id(&mut ctx, 23915);
    assert_eq!(first_navigation["sessionId"], json!(first.session_id));
    assert!(
        ctx.sent.iter().all(|message| {
            message.get("error").is_none()
                && message["method"] != json!("Runtime.executionContextCreated")
                && message["method"] != json!("Runtime.executionContextsCleared")
        }),
        "unexpected protocol/runtime event during first navigation: {:?}",
        ctx.sent
    );
    ctx.take_all();

    let second =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 23916, 23917, 23918)
            .await;

    ctx.process_async(json!({
        "id": 23919,
        "method": "Page.navigate",
        "sessionId": second.session_id,
        "params": {
            "url": "data:text/html,<body><div id=page>second-page</div></body>"
        }
    }))
    .await;
    let second_navigation = take_response_by_id(&mut ctx, 23919);
    assert_eq!(second_navigation["sessionId"], json!(second.session_id));
    assert!(
        ctx.sent.iter().all(|message| {
            message.get("error").is_none()
                && message["method"] != json!("Runtime.executionContextCreated")
                && message["method"] != json!("Runtime.executionContextsCleared")
        }),
        "unexpected protocol/runtime event during second navigation: {:?}",
        ctx.sent
    );
    ctx.take_all();

    ctx.process_async(json!({
        "id": 23920,
        "method": "Target.attachToTarget",
        "params": { "targetId": first.target_id, "flatten": true }
    }))
    .await;
    let reattached_session_id = take_response_by_id(&mut ctx, 23920)["result"]["sessionId"]
        .as_str()
        .expect("reattached first session id")
        .to_owned();
    assert_ne!(reattached_session_id, first.session_id);
    ctx.take_first_matching("first target reattached", |message| {
        message["method"] == json!("Target.attachedToTarget")
            && message["params"]["sessionId"] == json!(reattached_session_id)
    });
    assert_eq!(
        ctx.conn.browser_context.as_ref().map(|bc| bc.id.as_str()),
        Some(first.browser_context_id.as_str()),
        "reattaching the first target should switch the active browser context back"
    );
    ctx.process_async(json!({
        "id": 23921,
        "method": "Runtime.evaluate",
        "sessionId": first.session_id,
        "params": {
            "expression": "document.querySelector('#page').textContent"
        }
    }))
    .await;
    let first_main_world_eval = take_response_by_id(&mut ctx, 23921);
    assert_eq!(
        first_main_world_eval["result"]["result"]["value"],
        json!("first-page")
    );

    ctx.process_async(json!({
        "id": 23922,
        "method": "Page.createIsolatedWorld",
        "sessionId": first.session_id,
        "params": {
            "frameId": first.target_id,
            "worldName": "utility"
        }
    }))
    .await;
    let first_utility_context =
        take_response_by_id(&mut ctx, 23922)["result"]["executionContextId"]
            .as_i64()
            .expect("reactivated first utility context id");
    ctx.take_all();

    ctx.process_async(json!({
        "id": 23923,
        "method": "Runtime.evaluate",
        "sessionId": first.session_id,
        "params": {
            "contextId": first_utility_context,
            "expression": "reactivatedUtilityBinding('payload-reactivated-first'); JSON.stringify([typeof globalThis.reactivatedUtilityBinding, globalThis.__lm_reactivated_utility_preload])"
        }
    })).await;
    let first_utility_eval = take_response_by_id(&mut ctx, 23923);
    assert_eq!(
        first_utility_eval["result"]["result"]["value"],
        json!("[\"function\",\"first-preload\"]")
    );
    let reactivated_binding_called = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("reactivatedUtilityBinding")
        })
        .cloned()
        .expect("reactivated first utility world should emit bindingCalled");
    assert_eq!(
        reactivated_binding_called["params"]["executionContextId"],
        json!(first_utility_context)
    );
    assert_eq!(
        reactivated_binding_called["params"]["payload"],
        json!("payload-reactivated-first")
    );

    let first_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == first.browser_context_id)
        .expect("first browser context should still exist");
    assert_eq!(
        first_context.active_target_id(),
        Some(first.target_id.as_str())
    );
    assert_eq!(
        first_context.active_session_id(),
        Some(first.session_id.as_str())
    );
    assert_eq!(
        first_context
            .active_target
            .owner_state
            .document_start_scripts
            .len(),
        1
    );
    assert!(
        first_context
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
                binding.name == "reactivatedUtilityBinding"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "first browser context should retain its utility-world binding definition after being reactivated"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_reactivated_first_context_cleanup_updates_current_and_future_utility_worlds_without_runtime_enable()
 {
    let mut ctx = TestContext::new();
    let first =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 23924, 23925, 23926)
            .await;

    ctx.process_async(json!({
        "id": 23927,
        "method": "Runtime.addBinding",
        "sessionId": first.session_id,
        "params": {
            "name": "reactivationCleanupBinding",
            "executionContextName": "utility"
        }
    }))
    .await;
    let add_binding = take_response_by_id(&mut ctx, 23927);
    assert_eq!(add_binding["result"], json!({}));
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 23928,
        "method": "Page.navigate",
        "sessionId": first.session_id,
        "params": {
            "url": "data:text/html,<body><div id=page>cleanup-first</div></body>"
        }
    }))
    .await;
    let first_navigation = take_response_by_id(&mut ctx, 23928);
    assert_eq!(first_navigation["sessionId"], json!(first.session_id));
    ctx.take_all();

    ctx.process_async(json!({
        "id": 23929,
        "method": "Page.createIsolatedWorld",
        "sessionId": first.session_id,
        "params": {
            "frameId": first.target_id,
            "worldName": "utility"
        }
    }))
    .await;
    let first_utility_context =
        take_response_by_id(&mut ctx, 23929)["result"]["executionContextId"]
            .as_i64()
            .expect("first utility context id");
    ctx.take_all();

    ctx.process_async(json!({
        "id": 23930,
        "method": "Page.addScriptToEvaluateOnNewDocument",
        "sessionId": first.session_id,
        "params": {
            "source": "globalThis.__lm_reactivated_cleanup_marker = 'current-world-kept';",
            "worldName": "utility",
            "runImmediately": true
        }
    }))
    .await;
    let preload_identifier = take_response_by_id(&mut ctx, 23930)["result"]["identifier"]
        .as_str()
        .expect("preload identifier")
        .to_owned();

    ctx.process_async(json!({
            "id": 23931,
            "method": "Runtime.evaluate",
            "sessionId": first.session_id,
            "params": {
                "contextId": first_utility_context,
                "expression": "JSON.stringify([typeof globalThis.reactivationCleanupBinding, globalThis.__lm_reactivated_cleanup_marker])"
            }
        })).await;
    let seeded_current_world = take_response_by_id(&mut ctx, 23931);
    assert_eq!(
        seeded_current_world["result"]["result"]["value"],
        json!("[\"function\",\"current-world-kept\"]")
    );

    let second =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 23932, 23933, 23934)
            .await;
    ctx.process_async(json!({
        "id": 23935,
        "method": "Page.navigate",
        "sessionId": second.session_id,
        "params": {
            "url": "data:text/html,<body><div id=page>cleanup-second</div></body>"
        }
    }))
    .await;
    let second_navigation = take_response_by_id(&mut ctx, 23935);
    assert_eq!(second_navigation["sessionId"], json!(second.session_id));
    ctx.take_all();

    ctx.process_async(json!({
        "id": 23936,
        "method": "Target.attachToTarget",
        "params": { "targetId": first.target_id, "flatten": true }
    }))
    .await;
    let reattached_session_id = take_response_by_id(&mut ctx, 23936)["result"]["sessionId"]
        .as_str()
        .expect("reattached first session id")
        .to_owned();
    assert_ne!(reattached_session_id, first.session_id);
    ctx.take_first_matching("first target reattached", |message| {
        message["method"] == json!("Target.attachedToTarget")
            && message["params"]["sessionId"] == json!(reattached_session_id)
    });

    ctx.process_async(json!({
        "id": 23937,
        "method": "Runtime.removeBinding",
        "sessionId": first.session_id,
        "params": { "name": "reactivationCleanupBinding" }
    }))
    .await;
    let remove_binding = take_response_by_id(&mut ctx, 23937);
    assert_eq!(remove_binding["result"], json!({}));

    ctx.process_async(json!({
        "id": 23938,
        "method": "Page.removeScriptToEvaluateOnNewDocument",
        "sessionId": first.session_id,
        "params": { "identifier": preload_identifier }
    }))
    .await;
    let remove_preload = take_response_by_id(&mut ctx, 23938);
    assert_eq!(remove_preload["result"], json!({}));

    ctx.process_async(json!({
        "id": 23939,
        "method": "Runtime.evaluate",
        "sessionId": first.session_id,
        "params": {
            "contextId": first_utility_context,
            "expression": "JSON.stringify([typeof globalThis.reactivationCleanupBinding, globalThis.__lm_reactivated_cleanup_marker])"
        }
    })).await;
    let current_world_after_cleanup = take_response_by_id(&mut ctx, 23939);
    assert_eq!(
        current_world_after_cleanup["result"]["result"]["value"],
        json!("[\"undefined\",\"current-world-kept\"]")
    );
    assert!(
        ctx.sent.iter().all(|message| {
            message["method"] != json!("Runtime.bindingCalled")
                || message["params"]["name"] != json!("reactivationCleanupBinding")
        }),
        "removed binding should not fire after reactivated current-world cleanup"
    );
    ctx.take_all();

    ctx.process_async(json!({
        "id": 23940,
        "method": "Page.navigate",
        "sessionId": first.session_id,
        "params": {
            "url": "data:text/html,<body><div id=page>cleanup-reactivated</div></body>"
        }
    }))
    .await;
    let renavigation = take_response_by_id(&mut ctx, 23940);
    assert_eq!(renavigation["sessionId"], json!(first.session_id));
    ctx.take_all();

    ctx.process_async(json!({
        "id": 23941,
        "method": "Page.createIsolatedWorld",
        "sessionId": first.session_id,
        "params": {
            "frameId": first.target_id,
            "worldName": "utility"
        }
    }))
    .await;
    let second_utility_context =
        take_response_by_id(&mut ctx, 23941)["result"]["executionContextId"]
            .as_i64()
            .expect("reactivated utility context id");
    ctx.take_all();

    ctx.process_async(json!({
        "id": 23942,
        "method": "Runtime.evaluate",
        "sessionId": first.session_id,
        "params": {
            "contextId": second_utility_context,
            "expression": "JSON.stringify([typeof globalThis.reactivationCleanupBinding, globalThis.__lm_reactivated_cleanup_marker ?? null])"
        }
    })).await;
    let fresh_world_after_cleanup = take_response_by_id(&mut ctx, 23942);
    assert_eq!(
        fresh_world_after_cleanup["result"]["result"]["value"],
        json!("[\"undefined\",null]")
    );
    assert!(
        ctx.sent.iter().all(|message| {
            message["method"] != json!("Runtime.bindingCalled")
                || message["params"]["name"] != json!("reactivationCleanupBinding")
        }),
        "removed binding should stay absent in future utility worlds"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_utility_world_init_persists_across_detach_and_reattach_without_runtime_enable()
 {
    let mut ctx = TestContext::new();
    let attached =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2240, 2241, 2242).await;

    ctx.process_async(json!({
        "id": 2243,
        "method": "Page.addScriptToEvaluateOnNewDocument",
        "sessionId": attached.session_id,
        "params": {
            "source": "globalThis.__lm_detach_reattach_preload = 'ready';",
            "worldName": "utility"
        }
    }))
    .await;
    let preload = take_response_by_id(&mut ctx, 2243);
    assert_eq!(preload["sessionId"], json!(attached.session_id));
    assert!(preload["result"]["identifier"].is_string());

    ctx.process_async(json!({
        "id": 2244,
        "method": "Runtime.addBinding",
        "sessionId": attached.session_id,
        "params": {
            "name": "detachReattachBinding",
            "executionContextName": "utility"
        }
    }))
    .await;
    let add_binding = take_response_by_id(&mut ctx, 2244);
    assert_eq!(add_binding["result"], json!({}));
    assert!(
        !ctx.sent.iter().any(|message| {
            message["method"] == json!("Runtime.executionContextCreated")
                || message["method"] == json!("Runtime.executionContextsCleared")
        }),
        "Patchright-style setup should stay off Runtime.enable surfaces: {:?}",
        ctx.sent
    );
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 2245,
        "method": "Page.navigate",
        "sessionId": attached.session_id,
        "params": {
            "url": "data:text/html,<body><div id=page>reattach-me</div></body>"
        }
    }))
    .await;
    let navigation = take_response_by_id(&mut ctx, 2245);
    assert_eq!(navigation["sessionId"], json!(attached.session_id));
    assert!(
        ctx.sent.iter().all(|message| {
            message.get("error").is_none()
                && message["method"] != json!("Runtime.executionContextCreated")
                && message["method"] != json!("Runtime.executionContextsCleared")
        }),
        "unexpected protocol/runtime event during initial navigation: {:?}",
        ctx.sent
    );
    ctx.take_all();

    ctx.process_async(json!({
        "id": 2246,
        "method": "Page.createIsolatedWorld",
        "sessionId": attached.session_id,
        "params": {
            "frameId": attached.target_id,
            "worldName": "utility"
        }
    }))
    .await;
    let initial_utility_context =
        take_response_by_id(&mut ctx, 2246)["result"]["executionContextId"]
            .as_i64()
            .expect("initial utility context id");
    ctx.take_all();

    ctx.process_async(json!({
        "id": 2247,
        "method": "Runtime.evaluate",
        "sessionId": attached.session_id,
        "params": {
            "contextId": initial_utility_context,
            "expression": "detachReattachBinding('before-detach'); JSON.stringify([typeof globalThis.detachReattachBinding, globalThis.__lm_detach_reattach_preload])"
        }
    })).await;
    let before_detach_eval = take_response_by_id(&mut ctx, 2247);
    assert_eq!(
        before_detach_eval["result"]["result"]["value"],
        json!("[\"function\",\"ready\"]")
    );
    let before_detach_binding_called = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("detachReattachBinding")
        })
        .cloned()
        .expect("utility world should emit bindingCalled before detach");
    assert_eq!(
        before_detach_binding_called["params"]["payload"],
        json!("before-detach")
    );
    assert_eq!(
        before_detach_binding_called["params"]["executionContextId"],
        json!(initial_utility_context)
    );
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 2248,
        "method": "Target.detachFromTarget",
        "params": {
            "targetId": attached.target_id,
            "sessionId": attached.session_id
        }
    }))
    .await;
    ctx.expect_result(2248, json!({}), None);
    ctx.expect_event(
        "Target.detachedFromTarget",
        Some(&json!({
            "targetId": attached.target_id,
            "sessionId": attached.session_id,
        })),
    );
    assert_eq!(
        ctx.conn
            .browser_contexts()
            .find(|bc| bc.id == attached.browser_context_id)
            .and_then(|bc| bc.active_session_id()),
        None,
        "detach should clear the current target session id"
    );

    ctx.process_async(json!({
        "id": 2249,
        "method": "Target.attachToTarget",
        "params": {
            "targetId": attached.target_id,
            "flatten": true
        }
    }))
    .await;
    let reattached_session_id = take_response_by_id(&mut ctx, 2249)["result"]["sessionId"]
        .as_str()
        .expect("reattached session id")
        .to_owned();
    assert_ne!(
        reattached_session_id, attached.session_id,
        "reattach after detach should allocate a fresh target session"
    );
    ctx.expect_event(
        "Target.attachedToTarget",
        Some(&json!({
            "sessionId": reattached_session_id,
            "targetInfo": {
                "targetId": attached.target_id,
                "browserContextId": attached.browser_context_id,
            }
        })),
    );
    assert!(
        !ctx.sent.iter().any(|message| {
            message["method"] == json!("Runtime.executionContextCreated")
                || message["method"] == json!("Runtime.executionContextsCleared")
        }),
        "reattach without Runtime.enable should stay off Runtime surfaces: {:?}",
        ctx.sent
    );
    ctx.take_all();

    ctx.process_async(json!({
        "id": 2250,
        "method": "Page.createIsolatedWorld",
        "sessionId": reattached_session_id,
        "params": {
            "frameId": attached.target_id,
            "worldName": "utility"
        }
    }))
    .await;
    let reattached_utility_context =
        take_response_by_id(&mut ctx, 2250)["result"]["executionContextId"]
            .as_i64()
            .expect("reattached utility context id");
    ctx.take_all();

    ctx.process_async(json!({
        "id": 2251,
        "method": "Runtime.evaluate",
        "sessionId": reattached_session_id,
        "params": {
            "contextId": reattached_utility_context,
            "expression": "detachReattachBinding('after-reattach'); JSON.stringify([typeof globalThis.detachReattachBinding, globalThis.__lm_detach_reattach_preload, document.querySelector('#page').textContent])"
        }
    })).await;
    let after_reattach_eval = take_response_by_id(&mut ctx, 2251);
    assert_eq!(
        after_reattach_eval["result"]["result"]["value"],
        json!("[\"function\",\"ready\",\"reattach-me\"]")
    );
    let after_reattach_binding_called = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("detachReattachBinding")
        })
        .cloned()
        .expect("utility world should emit bindingCalled after reattach");
    assert_eq!(
        after_reattach_binding_called["params"]["payload"],
        json!("after-reattach")
    );
    assert_eq!(
        after_reattach_binding_called["params"]["executionContextId"],
        json!(reattached_utility_context)
    );

    let active = ctx
        .conn
        .browser_context
        .as_ref()
        .expect("active browser context");
    assert_eq!(active.id, attached.browser_context_id);
    assert_eq!(active.active_target_id(), Some(attached.target_id.as_str()));
    assert_eq!(
        active.active_session_id(),
        Some(reattached_session_id.as_str())
    );
    assert_eq!(
        active
            .active_target
            .owner_state
            .document_start_scripts
            .len(),
        1
    );
    assert!(
        active
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
                binding.name == "detachReattachBinding"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "utility binding definition should survive detach/reattach on the browser context"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_utility_world_init_persists_when_auto_attach_reattaches_existing_target_without_runtime_enable()
 {
    let mut ctx = TestContext::new();
    let attached =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2252, 2253, 2254).await;

    ctx.process_async(json!({
        "id": 2255,
        "method": "Page.addScriptToEvaluateOnNewDocument",
        "sessionId": attached.session_id,
        "params": {
            "source": "globalThis.__lm_auto_attach_rehydrate_preload = 'ready';",
            "worldName": "utility"
        }
    }))
    .await;
    let preload = take_response_by_id(&mut ctx, 2255);
    assert_eq!(preload["sessionId"], json!(attached.session_id));
    assert!(preload["result"]["identifier"].is_string());

    ctx.process_async(json!({
        "id": 2256,
        "method": "Runtime.addBinding",
        "sessionId": attached.session_id,
        "params": {
            "name": "autoAttachRehydrateBinding",
            "executionContextName": "utility"
        }
    }))
    .await;
    let add_binding = take_response_by_id(&mut ctx, 2256);
    assert_eq!(add_binding["result"], json!({}));
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 2257,
        "method": "Page.navigate",
        "sessionId": attached.session_id,
        "params": {
            "url": "data:text/html,<body><div id=page>auto-attach-target</div></body>"
        }
    }))
    .await;
    let navigation = take_response_by_id(&mut ctx, 2257);
    assert_eq!(navigation["sessionId"], json!(attached.session_id));
    ctx.take_all();

    ctx.process_async(json!({
        "id": 2258,
        "method": "Target.detachFromTarget",
        "params": {
            "targetId": attached.target_id,
            "sessionId": attached.session_id
        }
    }))
    .await;
    ctx.expect_result(2258, json!({}), None);
    ctx.expect_event(
        "Target.detachedFromTarget",
        Some(&json!({
            "targetId": attached.target_id,
            "sessionId": attached.session_id,
        })),
    );
    assert_eq!(
        ctx.conn
            .browser_contexts()
            .find(|bc| bc.id == attached.browser_context_id)
            .and_then(|bc| bc.active_session_id()),
        None,
        "detach should leave the target sessionless before auto-attach takes over"
    );

    ctx.process_async(json!({
        "id": 2259,
        "method": "Target.setAutoAttach",
        "params": {
            "autoAttach": true,
            "waitForDebuggerOnStart": false
        }
    }))
    .await;
    ctx.expect_result(2259, json!({}), None);
    let auto_attach_event = ctx
        .take_first_matching("reattached target attachedToTarget", |message| {
            message["method"] == json!("Target.attachedToTarget")
        });
    assert_eq!(
        auto_attach_event["params"]["targetInfo"]["targetId"],
        json!(attached.target_id)
    );
    assert_eq!(
        auto_attach_event["params"]["targetInfo"]["browserContextId"],
        json!(attached.browser_context_id)
    );
    let auto_attached_session_id = auto_attach_event["params"]["sessionId"]
        .as_str()
        .expect("auto-attached session id")
        .to_owned();
    assert_ne!(
        auto_attached_session_id, attached.session_id,
        "auto-attach should allocate a fresh session for an existing unattached target"
    );
    assert!(
        !ctx.sent.iter().any(|message| {
            message["method"] == json!("Runtime.executionContextCreated")
                || message["method"] == json!("Runtime.executionContextsCleared")
        }),
        "auto-attach rehydration should stay off Runtime.enable surfaces: {:?}",
        ctx.sent
    );
    ctx.take_all();

    ctx.process_async(json!({
        "id": 2260,
        "method": "Page.createIsolatedWorld",
        "sessionId": auto_attached_session_id,
        "params": {
            "frameId": attached.target_id,
            "worldName": "utility"
        }
    }))
    .await;
    let utility_context = take_response_by_id(&mut ctx, 2260)["result"]["executionContextId"]
        .as_i64()
        .expect("utility context after auto-attach");
    ctx.take_all();

    ctx.process_async(json!({
        "id": 2261,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "contextId": utility_context,
            "expression": "autoAttachRehydrateBinding('after-auto-attach'); JSON.stringify([typeof globalThis.autoAttachRehydrateBinding, globalThis.__lm_auto_attach_rehydrate_preload, document.querySelector('#page').textContent])"
        }
    })).await;
    let evaluation = take_response_by_id(&mut ctx, 2261);
    assert_eq!(
        evaluation["result"]["result"]["value"],
        json!("[\"function\",\"ready\",\"auto-attach-target\"]")
    );
    let binding_called = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("autoAttachRehydrateBinding")
        })
        .cloned()
        .expect("auto-attached utility world should emit bindingCalled");
    assert_eq!(
        binding_called["params"]["payload"],
        json!("after-auto-attach")
    );
    assert_eq!(
        binding_called["params"]["executionContextId"],
        json!(utility_context)
    );

    let active = ctx
        .conn
        .browser_context
        .as_ref()
        .expect("active browser context");
    assert_eq!(active.id, attached.browser_context_id);
    assert_eq!(active.active_target_id(), Some(attached.target_id.as_str()));
    assert_eq!(
        active.active_session_id(),
        Some(auto_attached_session_id.as_str())
    );
    assert_eq!(
        active
            .active_target
            .owner_state
            .document_start_scripts
            .len(),
        1
    );
    assert!(
        active
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
                binding.name == "autoAttachRehydrateBinding"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "utility binding definition should survive auto-attach rehydration on the browser context"
    );
    assert!(ctx.conn.auto_attach);
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_crpage_cleanup_state_persists_across_detach_and_reattach_with_mixed_binding_kinds_without_runtime_enable()
 {
    let mut ctx = TestContext::new();
    let attached =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 35150, 35151, 35152)
            .await;

    ctx.process_async(json!({
        "id": 35153,
        "method": "Page.navigate",
        "sessionId": attached.session_id,
        "params": {
            "url": "data:text/html,<body><div id='main-handle-b'>main-handle-b</div><div id='utility-handle-b'>utility-handle-b</div></body>"
        }
    })).await;
    let navigation = take_response_by_id(&mut ctx, 35153);
    assert_eq!(navigation["sessionId"], json!(attached.session_id));
    ctx.take_all();

    ctx.process_async(json!({
        "id": 35154,
        "method": "Page.createIsolatedWorld",
        "sessionId": attached.session_id,
        "params": {
            "frameId": attached.target_id,
            "worldName": "utility"
        }
    }))
    .await;
    let initial_utility_context =
        take_response_by_id(&mut ctx, 35154)["result"]["executionContextId"]
            .as_i64()
            .expect("initial utility context id");
    ctx.take_all();

    let custom_wrapper_a_source = patchright_page_binding_wrapper_source(
        "customBindingA",
        "__lm_custom_binding_a_deliver",
        None,
        false,
    );
    let custom_wrapper_b_source = patchright_page_binding_wrapper_source(
        "customBindingB",
        "__lm_custom_binding_b_deliver",
        None,
        false,
    );
    let custom_handle_wrapper_a_source = patchright_page_binding_wrapper_source(
        "customHandleBindingA",
        "__lm_custom_handle_binding_a_deliver",
        Some("__lm_custom_handle_binding_a_take"),
        true,
    );
    let custom_handle_wrapper_b_source = patchright_page_binding_wrapper_source(
        "customHandleBindingB",
        "__lm_custom_handle_binding_b_deliver",
        Some("__lm_custom_handle_binding_b_take"),
        true,
    );
    let retained_wrapper_source = patchright_page_binding_wrapper_source(
        "__pw_keptBinding",
        "__lm_pw_kept_binding_deliver",
        None,
        false,
    );
    let retained_handle_wrapper_source = patchright_page_binding_wrapper_source(
        "__pw_keptHandleBinding",
        "__lm_pw_kept_handle_binding_deliver",
        Some("__lm_pw_kept_handle_binding_take"),
        true,
    );

    for (id, binding_name, source) in [
        (
            35155_u64,
            "customBindingA",
            custom_wrapper_a_source.as_str(),
        ),
        (
            35159_u64,
            "customBindingB",
            custom_wrapper_b_source.as_str(),
        ),
        (
            35163_u64,
            "customHandleBindingA",
            custom_handle_wrapper_a_source.as_str(),
        ),
        (
            35167_u64,
            "customHandleBindingB",
            custom_handle_wrapper_b_source.as_str(),
        ),
        (
            35171_u64,
            "__pw_keptBinding",
            retained_wrapper_source.as_str(),
        ),
        (
            35175_u64,
            "__pw_keptHandleBinding",
            retained_handle_wrapper_source.as_str(),
        ),
    ] {
        install_patchright_crpage_binding_in_existing_worlds_async(
            &mut ctx,
            &attached.session_id,
            initial_utility_context,
            id,
            id + 1,
            id + 2,
            id + 3,
            binding_name,
            source,
        )
        .await;
    }

    for (id, binding_name) in [
        (35179_u64, "customBindingA"),
        (35180_u64, "customHandleBindingA"),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.removeBinding",
            "sessionId": attached.session_id,
            "params": { "name": binding_name }
        }))
        .await;
        assert_eq!(take_response_by_id(&mut ctx, id)["result"], json!({}));
    }

    for (id, context_id, expected_state) in [
        (
            35181_u64,
            None::<i64>,
            json!(
                "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
            ),
        ),
        (
            35182_u64,
            Some(initial_utility_context),
            json!(
                "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
            ),
        ),
    ] {
        let mut params = json!({
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
        });
        if let Some(context_id) = context_id {
            params["contextId"] = json!(context_id);
        }
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": attached.session_id,
            "params": params
        }))
        .await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    ctx.process_async(json!({
        "id": 35183,
        "method": "Target.detachFromTarget",
        "params": {
            "targetId": attached.target_id,
            "sessionId": attached.session_id
        }
    }))
    .await;
    ctx.expect_result(35183, json!({}), None);
    ctx.expect_event(
        "Target.detachedFromTarget",
        Some(&json!({
            "targetId": attached.target_id,
            "sessionId": attached.session_id,
        })),
    );

    ctx.process_async(json!({
        "id": 35184,
        "method": "Target.attachToTarget",
        "params": {
            "targetId": attached.target_id,
            "flatten": true
        }
    }))
    .await;
    let reattached_session_id = take_response_by_id(&mut ctx, 35184)["result"]["sessionId"]
        .as_str()
        .expect("reattached session id")
        .to_owned();
    assert_ne!(reattached_session_id, attached.session_id);
    ctx.expect_event(
        "Target.attachedToTarget",
        Some(&json!({
            "sessionId": reattached_session_id,
            "targetInfo": {
                "targetId": attached.target_id,
                "browserContextId": attached.browser_context_id,
            }
        })),
    );
    ctx.take_all();

    for (id, context_id, expected_state) in [
        (
            35185_u64,
            None::<i64>,
            json!(
                "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
            ),
        ),
        (
            35186_u64,
            Some(initial_utility_context),
            json!(
                "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
            ),
        ),
    ] {
        let mut params = json!({
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
        });
        if let Some(context_id) = context_id {
            params["contextId"] = json!(context_id);
        }
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": reattached_session_id,
            "params": params
        }))
        .await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    ctx.process_async(json!({
        "id": 35187,
        "method": "Runtime.evaluate",
        "sessionId": reattached_session_id,
        "params": {
            "expression": "globalThis.__lm_reattach_custom_b = customBindingB({ source: 'after-reattach-custom-b', nested: { count: 1, values: ['a', 2, true] } }); 'scheduled-custom-b'",
            "awaitPromise": true
        }
    })).await;
    let scheduled_custom = take_response_by_id(&mut ctx, 35187);
    assert!(
        scheduled_custom["result"]["result"]["value"]
            .as_str()
            .expect("scheduled custom value")
            .starts_with("scheduled-")
    );
    let custom_binding_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(reattached_session_id)
                && message["params"]["name"] == json!("customBindingB")
        })
        .cloned()
        .expect("custom binding should emit Runtime.bindingCalled after reattach");
    let custom_payload = custom_binding_called["params"]["payload"]
        .as_str()
        .expect("custom binding payload should be string");
    let custom_payload: serde_json::Value =
        serde_json::from_str(custom_payload).expect("custom binding payload should be valid json");
    assert_eq!(custom_payload["name"], json!("customBindingB"));
    assert_eq!(
        custom_payload["serializedArgs"],
        json!([{
            "source": "after-reattach-custom-b",
            "nested": { "count": 1, "values": ["a", 2, true] }
        }])
    );
    assert_eq!(custom_payload["seq"], json!(1));
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 35188,
        "method": "Runtime.evaluate",
        "sessionId": reattached_session_id,
        "params": {
            "expression": "globalThis.__lm_custom_binding_b_deliver({ name: 'customBindingB', seq: 1, result: 'after-reattach-custom-b-ok' }); 'delivered'",
            "awaitPromise": true
        }
    })).await;
    let delivered_custom = take_response_by_id(&mut ctx, 35188);
    assert_eq!(
        delivered_custom["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35189,
        "method": "Runtime.evaluate",
        "sessionId": reattached_session_id,
        "params": {
            "expression": "globalThis.__lm_reattach_custom_b",
            "awaitPromise": true
        }
    }))
    .await;
    let resolved_custom = take_response_by_id(&mut ctx, 35189);
    assert_eq!(
        resolved_custom["result"]["result"]["value"],
        json!("after-reattach-custom-b-ok")
    );

    ctx.process_async(json!({
        "id": 35190,
        "method": "Runtime.evaluate",
        "sessionId": reattached_session_id,
        "params": {
            "contextId": initial_utility_context,
            "expression": "globalThis.__lm_reattach_pw_handle = __pw_keptHandleBinding(document.getElementById('utility-handle-b')); 'scheduled-pw-handle'",
            "awaitPromise": true
        }
    })).await;
    let scheduled_handle = take_response_by_id(&mut ctx, 35190);
    assert!(
        scheduled_handle["result"]["result"]["value"]
            .as_str()
            .expect("scheduled handle value")
            .starts_with("scheduled-")
    );
    let handle_binding_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(reattached_session_id)
                && message["params"]["name"] == json!("__pw_keptHandleBinding")
                && message["params"]["executionContextId"] == json!(initial_utility_context)
        })
        .cloned()
        .expect("retained handle binding should emit Runtime.bindingCalled after reattach");
    let handle_payload = handle_binding_called["params"]["payload"]
        .as_str()
        .expect("handle binding payload should be string");
    let handle_payload: serde_json::Value =
        serde_json::from_str(handle_payload).expect("handle binding payload should be valid json");
    let handle_seq = handle_payload["seq"]
        .as_i64()
        .expect("handle payload seq should be integer");
    assert_eq!(handle_seq, 1);
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 35191,
        "method": "Runtime.evaluate",
        "sessionId": reattached_session_id,
        "params": {
            "contextId": initial_utility_context,
            "expression": format!(
                "(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {handle_seq} }})]); }})()"
            )
        }
    })).await;
    let taken_handle = take_response_by_id(&mut ctx, 35191);
    assert_eq!(
        taken_handle["result"]["result"]["value"],
        json!("[\"utility-handle-b\",\"utility-handle-b\",\"undefined\"]")
    );

    ctx.process_async(json!({
        "id": 35192,
        "method": "Runtime.evaluate",
        "sessionId": reattached_session_id,
        "params": {
            "contextId": initial_utility_context,
            "expression": format!(
                "globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {handle_seq}, result: 'after-reattach-pw-handle-ok' }}); 'delivered'"
            ),
            "awaitPromise": true
        }
    })).await;
    let delivered_handle = take_response_by_id(&mut ctx, 35192);
    assert_eq!(
        delivered_handle["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35193,
        "method": "Runtime.evaluate",
        "sessionId": reattached_session_id,
        "params": {
            "contextId": initial_utility_context,
            "expression": "globalThis.__lm_reattach_pw_handle",
            "awaitPromise": true
        }
    }))
    .await;
    let resolved_handle = take_response_by_id(&mut ctx, 35193);
    assert_eq!(
        resolved_handle["result"]["result"]["value"],
        json!("after-reattach-pw-handle-ok")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_crpage_cleanup_state_persists_when_auto_attach_reattaches_existing_target_with_mixed_binding_kinds_without_runtime_enable()
 {
    run_patchright_runtimeless_large_stack(
        "patchright-runtimeless-auto-reattach-existing-target",
        || async {
            let mut ctx = TestContext::new();
            let attached = create_attached_page_session_without_runtime_enable_async(
                &mut ctx, 35194, 35195, 35196,
            )
            .await;

            ctx.process_async(json!({
        "id": 35197,
        "method": "Page.navigate",
        "sessionId": attached.session_id,
        "params": {
            "url": "data:text/html,<body><div id='main-handle-b'>main-handle-b</div><div id='utility-handle-b'>utility-handle-b</div></body>"
        }
    })).await;
            let navigation = take_response_by_id(&mut ctx, 35197);
            assert_eq!(navigation["sessionId"], json!(attached.session_id));
            ctx.take_all();

            ctx.process_async(json!({
                "id": 35198,
                "method": "Page.createIsolatedWorld",
                "sessionId": attached.session_id,
                "params": {
                    "frameId": attached.target_id,
                    "worldName": "utility"
                }
            }))
            .await;
            let initial_utility_context =
                take_response_by_id(&mut ctx, 35198)["result"]["executionContextId"]
                    .as_i64()
                    .expect("initial utility context id");
            ctx.take_all();

            let custom_wrapper_a_source = patchright_page_binding_wrapper_source(
                "customBindingA",
                "__lm_custom_binding_a_deliver",
                None,
                false,
            );
            let custom_wrapper_b_source = patchright_page_binding_wrapper_source(
                "customBindingB",
                "__lm_custom_binding_b_deliver",
                None,
                false,
            );
            let custom_handle_wrapper_a_source = patchright_page_binding_wrapper_source(
                "customHandleBindingA",
                "__lm_custom_handle_binding_a_deliver",
                Some("__lm_custom_handle_binding_a_take"),
                true,
            );
            let custom_handle_wrapper_b_source = patchright_page_binding_wrapper_source(
                "customHandleBindingB",
                "__lm_custom_handle_binding_b_deliver",
                Some("__lm_custom_handle_binding_b_take"),
                true,
            );
            let retained_wrapper_source = patchright_page_binding_wrapper_source(
                "__pw_keptBinding",
                "__lm_pw_kept_binding_deliver",
                None,
                false,
            );
            let retained_handle_wrapper_source = patchright_page_binding_wrapper_source(
                "__pw_keptHandleBinding",
                "__lm_pw_kept_handle_binding_deliver",
                Some("__lm_pw_kept_handle_binding_take"),
                true,
            );

            for (id, binding_name, source) in [
                (
                    35199_u64,
                    "customBindingA",
                    custom_wrapper_a_source.as_str(),
                ),
                (
                    35203_u64,
                    "customBindingB",
                    custom_wrapper_b_source.as_str(),
                ),
                (
                    35207_u64,
                    "customHandleBindingA",
                    custom_handle_wrapper_a_source.as_str(),
                ),
                (
                    35211_u64,
                    "customHandleBindingB",
                    custom_handle_wrapper_b_source.as_str(),
                ),
                (
                    35215_u64,
                    "__pw_keptBinding",
                    retained_wrapper_source.as_str(),
                ),
                (
                    35219_u64,
                    "__pw_keptHandleBinding",
                    retained_handle_wrapper_source.as_str(),
                ),
            ] {
                install_patchright_crpage_binding_in_existing_worlds_async(
                    &mut ctx,
                    &attached.session_id,
                    initial_utility_context,
                    id,
                    id + 1,
                    id + 2,
                    id + 3,
                    binding_name,
                    source,
                )
                .await;
            }

            for (id, binding_name) in [
                (35223_u64, "customBindingA"),
                (35224_u64, "customHandleBindingA"),
            ] {
                ctx.process_async(json!({
                    "id": id,
                    "method": "Runtime.removeBinding",
                    "sessionId": attached.session_id,
                    "params": { "name": binding_name }
                }))
                .await;
                assert_eq!(take_response_by_id(&mut ctx, id)["result"], json!({}));
            }

            ctx.process_async(json!({
                "id": 35225,
                "method": "Target.detachFromTarget",
                "params": {
                    "targetId": attached.target_id,
                    "sessionId": attached.session_id
                }
            }))
            .await;
            ctx.expect_result(35225, json!({}), None);
            ctx.expect_event(
                "Target.detachedFromTarget",
                Some(&json!({
                    "targetId": attached.target_id,
                    "sessionId": attached.session_id,
                })),
            );
            assert_eq!(
                ctx.conn
                    .browser_contexts()
                    .find(|bc| bc.id == attached.browser_context_id)
                    .and_then(|bc| bc.active_session_id()),
                None
            );

            ctx.process_async(json!({
                "id": 35226,
                "method": "Target.setAutoAttach",
                "params": {
                    "autoAttach": true,
                    "waitForDebuggerOnStart": false
                }
            }))
            .await;
            ctx.expect_result(35226, json!({}), None);
            let auto_attach_event = ctx
                .take_first_matching("reattached target attachedToTarget", |message| {
                    message["method"] == json!("Target.attachedToTarget")
                });
            assert_eq!(
                auto_attach_event["params"]["targetInfo"]["targetId"],
                json!(attached.target_id)
            );
            let auto_attached_session_id = auto_attach_event["params"]["sessionId"]
                .as_str()
                .expect("auto-attached session id")
                .to_owned();
            assert_ne!(auto_attached_session_id, attached.session_id);
            ctx.take_all();

            ctx.process_async(json!({
        "id": 35227,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
        }
    })).await;
            let main_state = take_response_by_id(&mut ctx, 35227);
            assert_eq!(
                main_state["result"]["result"]["value"],
                json!(
                    "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
                )
            );

            ctx.process_async(json!({
                "id": 35228,
                "method": "Page.createIsolatedWorld",
                "sessionId": auto_attached_session_id,
                "params": {
                    "frameId": attached.target_id,
                    "worldName": "utility"
                }
            }))
            .await;
            let reattached_utility_context =
                take_response_by_id(&mut ctx, 35228)["result"]["executionContextId"]
                    .as_i64()
                    .expect("reattached utility context id");
            ctx.take_all();

            for (id, binding_name) in [
                (35229_u64, "customBindingB"),
                (35230_u64, "customHandleBindingB"),
                (35231_u64, "__pw_keptBinding"),
                (35232_u64, "__pw_keptHandleBinding"),
            ] {
                ctx.process_async(json!({
            "id": id,
            "method": "Runtime.addBinding",
            "sessionId": auto_attached_session_id,
            "params": { "name": binding_name, "executionContextId": reattached_utility_context }
        }))
        .await;
                assert_eq!(take_response_by_id(&mut ctx, id)["result"], json!({}));
            }

            for (id, source, expected_type) in [
                (35233_u64, custom_wrapper_a_source.as_str(), "undefined"),
                (35234_u64, custom_wrapper_b_source.as_str(), "function"),
                (
                    35235_u64,
                    custom_handle_wrapper_a_source.as_str(),
                    "undefined",
                ),
                (
                    35236_u64,
                    custom_handle_wrapper_b_source.as_str(),
                    "function",
                ),
                (35237_u64, retained_wrapper_source.as_str(), "function"),
                (
                    35238_u64,
                    retained_handle_wrapper_source.as_str(),
                    "function",
                ),
            ] {
                ctx.process_async(json!({
                    "id": id,
                    "method": "Runtime.evaluate",
                    "sessionId": auto_attached_session_id,
                    "params": {
                        "contextId": reattached_utility_context,
                        "expression": source,
                        "awaitPromise": true
                    }
                }))
                .await;
                let replayed = take_response_by_id(&mut ctx, id);
                assert_eq!(replayed["result"]["result"]["value"], json!(expected_type));
            }

            ctx.process_async(json!({
        "id": 35239,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "contextId": reattached_utility_context,
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
        }
    })).await;
            let utility_state = take_response_by_id(&mut ctx, 35239);
            assert_eq!(
                utility_state["result"]["result"]["value"],
                json!(
                    "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
                )
            );

            ctx.process_async(json!({
        "id": 35240,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "expression": "globalThis.__lm_auto_reattach_custom_b = customBindingB({ source: 'after-auto-attach-custom-b', nested: { count: 1, values: ['a', 2, true] } }); 'scheduled-custom-b'",
            "awaitPromise": true
        }
    })).await;
            let scheduled_custom = take_response_by_id(&mut ctx, 35240);
            assert!(
                scheduled_custom["result"]["result"]["value"]
                    .as_str()
                    .expect("scheduled custom value")
                    .starts_with("scheduled-")
            );
            let custom_binding_called = ctx
                .sent
                .iter()
                .rev()
                .find(|message| {
                    message["method"] == json!("Runtime.bindingCalled")
                        && message["sessionId"] == json!(auto_attached_session_id)
                        && message["params"]["name"] == json!("customBindingB")
                })
                .cloned()
                .expect("custom binding should emit Runtime.bindingCalled after auto-attach");
            let custom_payload = custom_binding_called["params"]["payload"]
                .as_str()
                .expect("custom binding payload should be string");
            let custom_payload: serde_json::Value = serde_json::from_str(custom_payload)
                .expect("custom binding payload should be valid json");
            assert_eq!(custom_payload["name"], json!("customBindingB"));
            assert_eq!(custom_payload["seq"], json!(1));
            ctx.sent.clear();

            ctx.process_async(json!({
        "id": 35241,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "expression": "globalThis.__lm_custom_binding_b_deliver({ name: 'customBindingB', seq: 1, result: 'after-auto-attach-custom-b-ok' }); 'delivered'",
            "awaitPromise": true
        }
    })).await;
            let delivered_custom = take_response_by_id(&mut ctx, 35241);
            assert_eq!(
                delivered_custom["result"]["result"]["value"],
                json!("delivered")
            );

            ctx.process_async(json!({
                "id": 35242,
                "method": "Runtime.evaluate",
                "sessionId": auto_attached_session_id,
                "params": {
                    "expression": "globalThis.__lm_auto_reattach_custom_b",
                    "awaitPromise": true
                }
            }))
            .await;
            let resolved_custom = take_response_by_id(&mut ctx, 35242);
            assert_eq!(
                resolved_custom["result"]["result"]["value"],
                json!("after-auto-attach-custom-b-ok")
            );

            ctx.process_async(json!({
        "id": 35243,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "contextId": reattached_utility_context,
            "expression": "globalThis.__lm_auto_reattach_pw_handle = __pw_keptHandleBinding(document.getElementById('utility-handle-b')); 'scheduled-pw-handle'",
            "awaitPromise": true
        }
    })).await;
            let scheduled_handle = take_response_by_id(&mut ctx, 35243);
            assert!(
                scheduled_handle["result"]["result"]["value"]
                    .as_str()
                    .expect("scheduled handle value")
                    .starts_with("scheduled-")
            );
            let handle_binding_called = ctx
                .sent
                .iter()
                .rev()
                .find(|message| {
                    message["method"] == json!("Runtime.bindingCalled")
                        && message["sessionId"] == json!(auto_attached_session_id)
                        && message["params"]["name"] == json!("__pw_keptHandleBinding")
                        && message["params"]["executionContextId"]
                            == json!(reattached_utility_context)
                })
                .cloned()
                .expect(
                    "retained handle binding should emit Runtime.bindingCalled after auto-attach",
                );
            let handle_payload = handle_binding_called["params"]["payload"]
                .as_str()
                .expect("handle binding payload should be string");
            let handle_payload: serde_json::Value = serde_json::from_str(handle_payload)
                .expect("handle binding payload should be valid json");
            let handle_seq = handle_payload["seq"]
                .as_i64()
                .expect("handle payload seq should be integer");
            assert_eq!(handle_seq, 1);
            ctx.sent.clear();

            ctx.process_async(json!({
        "id": 35244,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "contextId": reattached_utility_context,
            "expression": format!(
                "(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {handle_seq} }})]); }})()"
            )
        }
    })).await;
            let taken_handle = take_response_by_id(&mut ctx, 35244);
            assert_eq!(
                taken_handle["result"]["result"]["value"],
                json!("[\"utility-handle-b\",\"utility-handle-b\",\"undefined\"]")
            );

            ctx.process_async(json!({
        "id": 35245,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "contextId": reattached_utility_context,
            "expression": format!(
                "globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {handle_seq}, result: 'after-auto-attach-pw-handle-ok' }}); 'delivered'"
            ),
            "awaitPromise": true
        }
    })).await;
            let delivered_handle = take_response_by_id(&mut ctx, 35245);
            assert_eq!(
                delivered_handle["result"]["result"]["value"],
                json!("delivered")
            );

            ctx.process_async(json!({
                "id": 35246,
                "method": "Runtime.evaluate",
                "sessionId": auto_attached_session_id,
                "params": {
                    "contextId": reattached_utility_context,
                    "expression": "globalThis.__lm_auto_reattach_pw_handle",
                    "awaitPromise": true
                }
            }))
            .await;
            let resolved_handle = take_response_by_id(&mut ctx, 35246);
            assert_eq!(
                resolved_handle["result"]["result"]["value"],
                json!("after-auto-attach-pw-handle-ok")
            );

            ctx.process_async(json!({
        "id": 35247,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "expression": "globalThis.__lm_auto_reattach_custom_b_reject = customBindingB({ source: 'after-auto-attach-custom-b-reject', nested: { count: 2, values: ['b', 3, false] } }).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-custom-b-reject'",
            "awaitPromise": true
        }
    })).await;
            let scheduled_custom_reject = take_response_by_id(&mut ctx, 35247);
            assert!(
                scheduled_custom_reject["result"]["result"]["value"]
                    .as_str()
                    .expect("scheduled custom reject value")
                    .starts_with("scheduled-")
            );
            let custom_reject_binding_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(auto_attached_session_id)
                && message["params"]["name"] == json!("customBindingB")
        })
        .cloned()
        .expect("custom binding should emit Runtime.bindingCalled for rejection after auto-attach");
            let custom_reject_payload = custom_reject_binding_called["params"]["payload"]
                .as_str()
                .expect("custom reject binding payload should be string");
            let custom_reject_payload: serde_json::Value =
                serde_json::from_str(custom_reject_payload)
                    .expect("custom reject binding payload should be valid json");
            assert_eq!(custom_reject_payload["name"], json!("customBindingB"));
            assert_eq!(custom_reject_payload["seq"], json!(2));
            ctx.sent.clear();

            ctx.process_async(json!({
        "id": 35248,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "expression": "globalThis.__lm_custom_binding_b_deliver({ name: 'customBindingB', seq: 2, error: 'after-auto-attach-custom-b-error' }); 'delivered'",
            "awaitPromise": true
        }
    })).await;
            let delivered_custom_reject = take_response_by_id(&mut ctx, 35248);
            assert_eq!(
                delivered_custom_reject["result"]["result"]["value"],
                json!("delivered")
            );

            ctx.process_async(json!({
                "id": 35249,
                "method": "Runtime.evaluate",
                "sessionId": auto_attached_session_id,
                "params": {
                    "expression": "globalThis.__lm_auto_reattach_custom_b_reject",
                    "awaitPromise": true
                }
            }))
            .await;
            let rejected_custom = take_response_by_id(&mut ctx, 35249);
            assert_eq!(
                rejected_custom["result"]["result"]["value"],
                json!("rejected:after-auto-attach-custom-b-error")
            );

            ctx.process_async(json!({
        "id": 35250,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "contextId": reattached_utility_context,
            "expression": "globalThis.__lm_auto_reattach_pw_handle_reject = __pw_keptHandleBinding(document.getElementById('utility-handle-b')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-pw-handle-reject'",
            "awaitPromise": true
        }
    })).await;
            let scheduled_handle_reject = take_response_by_id(&mut ctx, 35250);
            assert!(
                scheduled_handle_reject["result"]["result"]["value"]
                    .as_str()
                    .expect("scheduled handle reject value")
                    .starts_with("scheduled-")
            );
            let handle_reject_binding_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(auto_attached_session_id)
                && message["params"]["name"] == json!("__pw_keptHandleBinding")
                && message["params"]["executionContextId"] == json!(reattached_utility_context)
        })
        .cloned()
        .expect("retained handle binding should emit Runtime.bindingCalled for rejection after auto-attach");
            let handle_reject_payload = handle_reject_binding_called["params"]["payload"]
                .as_str()
                .expect("handle reject binding payload should be string");
            let handle_reject_payload: serde_json::Value =
                serde_json::from_str(handle_reject_payload)
                    .expect("handle reject binding payload should be valid json");
            let handle_reject_seq = handle_reject_payload["seq"]
                .as_i64()
                .expect("handle reject payload seq should be integer");
            assert_eq!(handle_reject_seq, 2);
            ctx.sent.clear();

            ctx.process_async(json!({
        "id": 35251,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "contextId": reattached_utility_context,
            "expression": format!(
                "(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {handle_reject_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {handle_reject_seq} }})]); }})()"
            )
        }
    })).await;
            let taken_handle_reject = take_response_by_id(&mut ctx, 35251);
            assert_eq!(
                taken_handle_reject["result"]["result"]["value"],
                json!("[\"utility-handle-b\",\"utility-handle-b\",\"undefined\"]")
            );

            ctx.process_async(json!({
        "id": 35252,
        "method": "Runtime.evaluate",
        "sessionId": auto_attached_session_id,
        "params": {
            "contextId": reattached_utility_context,
            "expression": format!(
                "globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {handle_reject_seq}, error: 'after-auto-attach-pw-handle-error' }}); 'delivered'"
            ),
            "awaitPromise": true
        }
    })).await;
            let delivered_handle_reject = take_response_by_id(&mut ctx, 35252);
            assert_eq!(
                delivered_handle_reject["result"]["result"]["value"],
                json!("delivered")
            );

            ctx.process_async(json!({
                "id": 35253,
                "method": "Runtime.evaluate",
                "sessionId": auto_attached_session_id,
                "params": {
                    "contextId": reattached_utility_context,
                    "expression": "globalThis.__lm_auto_reattach_pw_handle_reject",
                    "awaitPromise": true
                }
            }))
            .await;
            let rejected_handle = take_response_by_id(&mut ctx, 35253);
            assert_eq!(
                rejected_handle["result"]["result"]["value"],
                json!("rejected:after-auto-attach-pw-handle-error")
            );
        },
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_crpage_cleanup_state_rehydrates_correctly_on_new_target_in_same_context_without_runtime_enable()
 {
    run_patchright_runtimeless_large_stack("patchright-runtimeless-rehydrate", || async {
        let mut ctx = TestContext::new();
        let first = create_attached_page_session_without_runtime_enable_async(
            &mut ctx, 35254, 35255, 35256,
        )
        .await;

        ctx.process_async(json!({
        "id": 35257,
        "method": "Page.navigate",
        "sessionId": first.session_id,
        "params": {
            "url": "data:text/html,<body><div id='main-handle-b'>main-handle-b</div><div id='utility-handle-b'>utility-handle-b</div></body>"
        }
    })).await;
        let first_navigation = take_response_by_id(&mut ctx, 35257);
        assert_eq!(first_navigation["sessionId"], json!(first.session_id));
        ctx.take_all();

        ctx.process_async(json!({
            "id": 35258,
            "method": "Page.createIsolatedWorld",
            "sessionId": first.session_id,
            "params": {
                "frameId": first.target_id,
                "worldName": "utility"
            }
        }))
        .await;
        let first_utility_context =
            take_response_by_id(&mut ctx, 35258)["result"]["executionContextId"]
                .as_i64()
                .expect("first utility context id");
        ctx.take_all();

        let custom_wrapper_a_source = patchright_page_binding_wrapper_source(
            "customBindingA",
            "__lm_custom_binding_a_deliver",
            None,
            false,
        );
        let custom_wrapper_b_source = patchright_page_binding_wrapper_source(
            "customBindingB",
            "__lm_custom_binding_b_deliver",
            None,
            false,
        );
        let custom_handle_wrapper_a_source = patchright_page_binding_wrapper_source(
            "customHandleBindingA",
            "__lm_custom_handle_binding_a_deliver",
            Some("__lm_custom_handle_binding_a_take"),
            true,
        );
        let custom_handle_wrapper_b_source = patchright_page_binding_wrapper_source(
            "customHandleBindingB",
            "__lm_custom_handle_binding_b_deliver",
            Some("__lm_custom_handle_binding_b_take"),
            true,
        );
        let retained_wrapper_source = patchright_page_binding_wrapper_source(
            "__pw_keptBinding",
            "__lm_pw_kept_binding_deliver",
            None,
            false,
        );
        let retained_handle_wrapper_source = patchright_page_binding_wrapper_source(
            "__pw_keptHandleBinding",
            "__lm_pw_kept_handle_binding_deliver",
            Some("__lm_pw_kept_handle_binding_take"),
            true,
        );

        for (id, binding_name, source) in [
            (
                35259_u64,
                "customBindingA",
                custom_wrapper_a_source.as_str(),
            ),
            (
                35263_u64,
                "customBindingB",
                custom_wrapper_b_source.as_str(),
            ),
            (
                35267_u64,
                "customHandleBindingA",
                custom_handle_wrapper_a_source.as_str(),
            ),
            (
                35271_u64,
                "customHandleBindingB",
                custom_handle_wrapper_b_source.as_str(),
            ),
            (
                35275_u64,
                "__pw_keptBinding",
                retained_wrapper_source.as_str(),
            ),
            (
                35279_u64,
                "__pw_keptHandleBinding",
                retained_handle_wrapper_source.as_str(),
            ),
        ] {
            install_patchright_crpage_binding_in_existing_worlds_async(
                &mut ctx,
                &first.session_id,
                first_utility_context,
                id,
                id + 1,
                id + 2,
                id + 3,
                binding_name,
                source,
            )
            .await;
        }

        for (id, source, world_name) in [
            (35283_u64, custom_wrapper_a_source.as_str(), None),
            (35284_u64, custom_wrapper_b_source.as_str(), None),
            (35285_u64, custom_handle_wrapper_a_source.as_str(), None),
            (35286_u64, custom_handle_wrapper_b_source.as_str(), None),
            (35287_u64, retained_wrapper_source.as_str(), None),
            (35288_u64, retained_handle_wrapper_source.as_str(), None),
            (35289_u64, custom_wrapper_a_source.as_str(), Some("utility")),
            (35290_u64, custom_wrapper_b_source.as_str(), Some("utility")),
            (
                35291_u64,
                custom_handle_wrapper_a_source.as_str(),
                Some("utility"),
            ),
            (
                35292_u64,
                custom_handle_wrapper_b_source.as_str(),
                Some("utility"),
            ),
            (35293_u64, retained_wrapper_source.as_str(), Some("utility")),
            (
                35294_u64,
                retained_handle_wrapper_source.as_str(),
                Some("utility"),
            ),
        ] {
            let mut params = json!({
                "source": source,
                "runImmediately": true
            });
            if let Some(world_name) = world_name {
                params["worldName"] = json!(world_name);
            }
            ctx.process_async(json!({
                "id": id,
                "method": "Page.addScriptToEvaluateOnNewDocument",
                "sessionId": first.session_id,
                "params": params
            }))
            .await;
            assert!(
                take_response_by_id(&mut ctx, id)["result"]["identifier"]
                    .as_str()
                    .is_some()
            );
        }

        for (id, binding_name) in [
            (35295_u64, "customBindingA"),
            (35296_u64, "customHandleBindingA"),
        ] {
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.removeBinding",
                "sessionId": first.session_id,
                "params": { "name": binding_name }
            }))
            .await;
            assert_eq!(take_response_by_id(&mut ctx, id)["result"], json!({}));
        }

        ctx.process_async(json!({
            "id": 35297,
            "method": "Target.closeTarget",
            "params": {
                "targetId": first.target_id
            }
        }))
        .await;
        ctx.expect_result(35297, json!({ "success": true }), None);
        ctx.take_all();

        let second = attach_page_session_without_runtime_enable_in_existing_context_async(
            &mut ctx,
            &first.browser_context_id,
            35298,
            35299,
        )
        .await;
        assert_ne!(second.target_id, first.target_id);

        ctx.process_async(json!({
        "id": 35300,
        "method": "Page.navigate",
        "sessionId": second.session_id,
        "params": {
            "url": "data:text/html,<body><div id='main-handle-b'>new-main-handle-b</div><div id='utility-handle-b'>new-utility-handle-b</div></body>"
        }
    })).await;
        let second_navigation = take_response_by_id(&mut ctx, 35300);
        assert_eq!(second_navigation["sessionId"], json!(second.session_id));
        ctx.take_all();

        for (id, source, expected_type) in [
            (35321_u64, custom_wrapper_a_source.as_str(), "undefined"),
            (35322_u64, custom_wrapper_b_source.as_str(), "function"),
            (
                35323_u64,
                custom_handle_wrapper_a_source.as_str(),
                "undefined",
            ),
            (
                35324_u64,
                custom_handle_wrapper_b_source.as_str(),
                "function",
            ),
            (35325_u64, retained_wrapper_source.as_str(), "function"),
            (
                35326_u64,
                retained_handle_wrapper_source.as_str(),
                "function",
            ),
        ] {
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": second.session_id,
                "params": {
                    "expression": source,
                    "awaitPromise": true
                }
            }))
            .await;
            let replayed = take_response_by_id(&mut ctx, id);
            assert_eq!(replayed["result"]["result"]["value"], json!(expected_type));
        }

        ctx.process_async(json!({
        "id": 35301,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
        }
    })).await;
        let second_main_state = take_response_by_id(&mut ctx, 35301);
        assert_eq!(
            second_main_state["result"]["result"]["value"],
            json!(
                "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
            )
        );

        ctx.process_async(json!({
            "id": 35302,
            "method": "Page.createIsolatedWorld",
            "sessionId": second.session_id,
            "params": {
                "frameId": second.target_id,
                "worldName": "utility"
            }
        }))
        .await;
        let second_utility_context =
            take_response_by_id(&mut ctx, 35302)["result"]["executionContextId"]
                .as_i64()
                .expect("second utility context id");
        ctx.take_all();

        for (id, binding_name) in [
            (35303_u64, "customBindingB"),
            (35304_u64, "customHandleBindingB"),
            (35305_u64, "__pw_keptBinding"),
            (35306_u64, "__pw_keptHandleBinding"),
        ] {
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.addBinding",
                "sessionId": second.session_id,
                "params": {
                    "name": binding_name,
                    "executionContextId": second_utility_context
                }
            }))
            .await;
            assert_eq!(take_response_by_id(&mut ctx, id)["result"], json!({}));
        }

        for (id, source, expected_type) in [
            (35307_u64, custom_wrapper_a_source.as_str(), "undefined"),
            (35308_u64, custom_wrapper_b_source.as_str(), "function"),
            (
                35309_u64,
                custom_handle_wrapper_a_source.as_str(),
                "undefined",
            ),
            (
                35310_u64,
                custom_handle_wrapper_b_source.as_str(),
                "function",
            ),
            (35311_u64, retained_wrapper_source.as_str(), "function"),
            (
                35312_u64,
                retained_handle_wrapper_source.as_str(),
                "function",
            ),
        ] {
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": second.session_id,
                "params": {
                    "contextId": second_utility_context,
                    "expression": source,
                    "awaitPromise": true
                }
            }))
            .await;
            let replayed = take_response_by_id(&mut ctx, id);
            assert_eq!(replayed["result"]["result"]["value"], json!(expected_type));
        }

        ctx.process_async(json!({
        "id": 35313,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": second_utility_context,
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
        }
    })).await;
        let second_utility_state = take_response_by_id(&mut ctx, 35313);
        assert_eq!(
            second_utility_state["result"]["result"]["value"],
            json!(
                "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
            )
        );

        ctx.process_async(json!({
        "id": 35314,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "expression": "globalThis.__lm_second_target_custom_b = customBindingB({ source: 'new-target-custom-b', nested: { count: 1, values: ['a', 2, true] } }); 'scheduled-custom-b'",
            "awaitPromise": true
        }
    })).await;
        let scheduled_custom = take_response_by_id(&mut ctx, 35314);
        assert!(
            scheduled_custom["result"]["result"]["value"]
                .as_str()
                .expect("scheduled custom value")
                .starts_with("scheduled-")
        );
        let custom_binding_called = ctx
            .sent
            .iter()
            .rev()
            .find(|message| {
                message["method"] == json!("Runtime.bindingCalled")
                    && message["sessionId"] == json!(second.session_id)
                    && message["params"]["name"] == json!("customBindingB")
            })
            .cloned()
            .expect("custom binding should emit Runtime.bindingCalled on the new target");
        let custom_payload = custom_binding_called["params"]["payload"]
            .as_str()
            .expect("custom binding payload should be string");
        let custom_payload: serde_json::Value = serde_json::from_str(custom_payload)
            .expect("custom binding payload should be valid json");
        assert_eq!(
            custom_payload["serializedArgs"],
            json!([{
                "source": "new-target-custom-b",
                "nested": { "count": 1, "values": ["a", 2, true] }
            }])
        );
        assert_eq!(custom_payload["seq"], json!(1));
        ctx.sent.clear();

        ctx.process_async(json!({
        "id": 35315,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "expression": "globalThis.__lm_custom_binding_b_deliver({ name: 'customBindingB', seq: 1, result: 'new-target-custom-b-ok' }); 'delivered'",
            "awaitPromise": true
        }
    })).await;
        let delivered_custom = take_response_by_id(&mut ctx, 35315);
        assert_eq!(
            delivered_custom["result"]["result"]["value"],
            json!("delivered")
        );

        ctx.process_async(json!({
            "id": 35316,
            "method": "Runtime.evaluate",
            "sessionId": second.session_id,
            "params": {
                "expression": "globalThis.__lm_second_target_custom_b",
                "awaitPromise": true
            }
        }))
        .await;
        let resolved_custom = take_response_by_id(&mut ctx, 35316);
        assert_eq!(
            resolved_custom["result"]["result"]["value"],
            json!("new-target-custom-b-ok")
        );

        ctx.process_async(json!({
        "id": 35327,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "expression": "globalThis.__lm_second_target_custom_b_reject = customBindingB({ source: 'new-target-custom-b-reject', nested: { count: 2, values: ['b', 3, false] } }).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-custom-b-reject'",
            "awaitPromise": true
        }
    })).await;
        let scheduled_custom_reject = take_response_by_id(&mut ctx, 35327);
        assert!(
            scheduled_custom_reject["result"]["result"]["value"]
                .as_str()
                .expect("scheduled custom reject value")
                .starts_with("scheduled-")
        );
        let custom_reject_binding_called = ctx
            .sent
            .iter()
            .rev()
            .find(|message| {
                message["method"] == json!("Runtime.bindingCalled")
                    && message["sessionId"] == json!(second.session_id)
                    && message["params"]["name"] == json!("customBindingB")
            })
            .cloned()
            .expect("custom binding should emit Runtime.bindingCalled rejection on the new target");
        let custom_reject_payload = custom_reject_binding_called["params"]["payload"]
            .as_str()
            .expect("custom reject payload should be string");
        let custom_reject_payload: serde_json::Value = serde_json::from_str(custom_reject_payload)
            .expect("custom reject payload should be valid json");
        assert_eq!(custom_reject_payload["seq"], json!(2));
        assert_eq!(
            custom_reject_payload["serializedArgs"],
            json!([{
                "source": "new-target-custom-b-reject",
                "nested": { "count": 2, "values": ["b", 3, false] }
            }])
        );
        ctx.sent.clear();

        ctx.process_async(json!({
        "id": 35328,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "expression": "globalThis.__lm_custom_binding_b_deliver({ name: 'customBindingB', seq: 2, error: 'new-target-custom-b-error' }); 'delivered'",
            "awaitPromise": true
        }
    })).await;
        let delivered_custom_reject = take_response_by_id(&mut ctx, 35328);
        assert_eq!(
            delivered_custom_reject["result"]["result"]["value"],
            json!("delivered")
        );

        ctx.process_async(json!({
            "id": 35329,
            "method": "Runtime.evaluate",
            "sessionId": second.session_id,
            "params": {
                "expression": "globalThis.__lm_second_target_custom_b_reject",
                "awaitPromise": true
            }
        }))
        .await;
        let rejected_custom = take_response_by_id(&mut ctx, 35329);
        assert_eq!(
            rejected_custom["result"]["result"]["value"],
            json!("rejected:new-target-custom-b-error")
        );

        ctx.process_async(json!({
        "id": 35317,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": second_utility_context,
            "expression": "globalThis.__lm_second_target_pw_handle = __pw_keptHandleBinding(document.getElementById('utility-handle-b')); 'scheduled-pw-handle'",
            "awaitPromise": true
        }
    })).await;
        let scheduled_handle = take_response_by_id(&mut ctx, 35317);
        assert!(
            scheduled_handle["result"]["result"]["value"]
                .as_str()
                .expect("scheduled handle value")
                .starts_with("scheduled-")
        );
        let handle_binding_called = ctx
            .sent
            .iter()
            .rev()
            .find(|message| {
                message["method"] == json!("Runtime.bindingCalled")
                    && message["sessionId"] == json!(second.session_id)
                    && message["params"]["name"] == json!("__pw_keptHandleBinding")
                    && message["params"]["executionContextId"] == json!(second_utility_context)
            })
            .cloned()
            .expect("retained handle binding should emit Runtime.bindingCalled on the new target");
        let handle_payload = handle_binding_called["params"]["payload"]
            .as_str()
            .expect("handle binding payload should be string");
        let handle_payload: serde_json::Value = serde_json::from_str(handle_payload)
            .expect("handle binding payload should be valid json");
        let handle_seq = handle_payload["seq"]
            .as_i64()
            .expect("handle payload seq should be integer");
        assert_eq!(handle_seq, 1);
        ctx.sent.clear();

        ctx.process_async(json!({
        "id": 35318,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": second_utility_context,
            "expression": format!(
                "(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {handle_seq} }})]); }})()"
            )
        }
    })).await;
        let taken_handle = take_response_by_id(&mut ctx, 35318);
        assert_eq!(
            taken_handle["result"]["result"]["value"],
            json!("[\"utility-handle-b\",\"new-utility-handle-b\",\"undefined\"]")
        );

        ctx.process_async(json!({
        "id": 35319,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": second_utility_context,
            "expression": format!(
                "globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {handle_seq}, result: 'new-target-pw-handle-ok' }}); 'delivered'"
            ),
            "awaitPromise": true
        }
    })).await;
        let delivered_handle = take_response_by_id(&mut ctx, 35319);
        assert_eq!(
            delivered_handle["result"]["result"]["value"],
            json!("delivered")
        );

        ctx.process_async(json!({
            "id": 35320,
            "method": "Runtime.evaluate",
            "sessionId": second.session_id,
            "params": {
                "contextId": second_utility_context,
                "expression": "globalThis.__lm_second_target_pw_handle",
                "awaitPromise": true
            }
        }))
        .await;
        let resolved_handle = take_response_by_id(&mut ctx, 35320);
        assert_eq!(
            resolved_handle["result"]["result"]["value"],
            json!("new-target-pw-handle-ok")
        );

        ctx.process_async(json!({
        "id": 35330,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": second_utility_context,
            "expression": "globalThis.__lm_second_target_pw_handle_reject = __pw_keptHandleBinding(document.getElementById('utility-handle-b')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-pw-handle-reject'",
            "awaitPromise": true
        }
    })).await;
        let scheduled_handle_reject = take_response_by_id(&mut ctx, 35330);
        assert!(
            scheduled_handle_reject["result"]["result"]["value"]
                .as_str()
                .expect("scheduled handle reject value")
                .starts_with("scheduled-")
        );
        let handle_reject_binding_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(second.session_id)
                && message["params"]["name"] == json!("__pw_keptHandleBinding")
                && message["params"]["executionContextId"] == json!(second_utility_context)
        })
        .cloned()
        .expect(
            "retained handle binding should emit rejection Runtime.bindingCalled on the new target",
        );
        let handle_reject_payload = handle_reject_binding_called["params"]["payload"]
            .as_str()
            .expect("handle reject payload should be string");
        let handle_reject_payload: serde_json::Value = serde_json::from_str(handle_reject_payload)
            .expect("handle reject payload should be valid json");
        let handle_reject_seq = handle_reject_payload["seq"]
            .as_i64()
            .expect("handle reject payload seq should be integer");
        assert_eq!(handle_reject_seq, 2);
        ctx.sent.clear();

        ctx.process_async(json!({
        "id": 35331,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": second_utility_context,
            "expression": format!(
                "(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {handle_reject_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {handle_reject_seq} }})]); }})()"
            )
        }
    })).await;
        let taken_handle_reject = take_response_by_id(&mut ctx, 35331);
        assert_eq!(
            taken_handle_reject["result"]["result"]["value"],
            json!("[\"utility-handle-b\",\"new-utility-handle-b\",\"undefined\"]")
        );

        ctx.process_async(json!({
        "id": 35332,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": second_utility_context,
            "expression": format!(
                "globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {handle_reject_seq}, error: 'new-target-pw-handle-error' }}); 'delivered'"
            ),
            "awaitPromise": true
        }
    })).await;
        let delivered_handle_reject = take_response_by_id(&mut ctx, 35332);
        assert_eq!(
            delivered_handle_reject["result"]["result"]["value"],
            json!("delivered")
        );

        ctx.process_async(json!({
            "id": 35333,
            "method": "Runtime.evaluate",
            "sessionId": second.session_id,
            "params": {
                "contextId": second_utility_context,
                "expression": "globalThis.__lm_second_target_pw_handle_reject",
                "awaitPromise": true
            }
        }))
        .await;
        let rejected_handle = take_response_by_id(&mut ctx, 35333);
        assert_eq!(
            rejected_handle["result"]["result"]["value"],
            json!("rejected:new-target-pw-handle-error")
        );

        ctx.process_async(json!({
        "id": 35334,
        "method": "Page.navigate",
        "sessionId": second.session_id,
        "params": {
            "url": "data:text/html,<body><div id='main-handle-b'>replay-main-handle-b</div><div id='utility-handle-b'>replay-utility-handle-b</div></body>"
        }
    })).await;
        let replay_navigation = take_response_by_id(&mut ctx, 35334);
        assert_eq!(replay_navigation["sessionId"], json!(second.session_id));
        ctx.take_all();

        for (id, source, expected_type) in [
            (35335_u64, custom_wrapper_a_source.as_str(), "undefined"),
            (35336_u64, custom_wrapper_b_source.as_str(), "function"),
            (
                35337_u64,
                custom_handle_wrapper_a_source.as_str(),
                "undefined",
            ),
            (
                35338_u64,
                custom_handle_wrapper_b_source.as_str(),
                "function",
            ),
            (35339_u64, retained_wrapper_source.as_str(), "function"),
            (
                35340_u64,
                retained_handle_wrapper_source.as_str(),
                "function",
            ),
        ] {
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": second.session_id,
                "params": {
                    "expression": source,
                    "awaitPromise": true
                }
            }))
            .await;
            let replayed = take_response_by_id(&mut ctx, id);
            assert_eq!(replayed["result"]["result"]["value"], json!(expected_type));
        }

        ctx.process_async(json!({
        "id": 35341,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
        }
    })).await;
        let replay_main_state = take_response_by_id(&mut ctx, 35341);
        assert_eq!(
            replay_main_state["result"]["result"]["value"],
            json!(
                "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
            )
        );

        ctx.process_async(json!({
            "id": 35342,
            "method": "Page.createIsolatedWorld",
            "sessionId": second.session_id,
            "params": {
                "frameId": second.target_id,
                "worldName": "utility"
            }
        }))
        .await;
        let replay_utility_context =
            take_response_by_id(&mut ctx, 35342)["result"]["executionContextId"]
                .as_i64()
                .expect("replay utility context id");
        ctx.take_all();

        for (id, binding_name) in [
            (35343_u64, "customBindingB"),
            (35344_u64, "customHandleBindingB"),
            (35345_u64, "__pw_keptBinding"),
            (35346_u64, "__pw_keptHandleBinding"),
        ] {
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.addBinding",
                "sessionId": second.session_id,
                "params": {
                    "name": binding_name,
                    "executionContextId": replay_utility_context
                }
            }))
            .await;
            assert_eq!(take_response_by_id(&mut ctx, id)["result"], json!({}));
        }

        for (id, source, expected_type) in [
            (35347_u64, custom_wrapper_a_source.as_str(), "undefined"),
            (35348_u64, custom_wrapper_b_source.as_str(), "function"),
            (
                35349_u64,
                custom_handle_wrapper_a_source.as_str(),
                "undefined",
            ),
            (
                35350_u64,
                custom_handle_wrapper_b_source.as_str(),
                "function",
            ),
            (35351_u64, retained_wrapper_source.as_str(), "function"),
            (
                35352_u64,
                retained_handle_wrapper_source.as_str(),
                "function",
            ),
        ] {
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": second.session_id,
                "params": {
                    "contextId": replay_utility_context,
                    "expression": source,
                    "awaitPromise": true
                }
            }))
            .await;
            let replayed = take_response_by_id(&mut ctx, id);
            assert_eq!(replayed["result"]["result"]["value"], json!(expected_type));
        }

        ctx.process_async(json!({
        "id": 35353,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": replay_utility_context,
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
        }
    })).await;
        let replay_utility_state = take_response_by_id(&mut ctx, 35353);
        assert_eq!(
            replay_utility_state["result"]["result"]["value"],
            json!(
                "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
            )
        );

        ctx.process_async(json!({
        "id": 35354,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "expression": "globalThis.__lm_replay_target_custom_b = customBindingB({ source: 'replay-target-custom-b', nested: { count: 3, values: ['c', 4, true] } }); 'scheduled-replay-custom-b'",
            "awaitPromise": true
        }
    })).await;
        let scheduled_replay_custom = take_response_by_id(&mut ctx, 35354);
        assert!(
            scheduled_replay_custom["result"]["result"]["value"]
                .as_str()
                .expect("scheduled replay custom value")
                .starts_with("scheduled-")
        );
        let replay_custom_binding_called = ctx
            .sent
            .iter()
            .rev()
            .find(|message| {
                message["method"] == json!("Runtime.bindingCalled")
                    && message["sessionId"] == json!(second.session_id)
                    && message["params"]["name"] == json!("customBindingB")
            })
            .cloned()
            .expect("replay custom binding should emit Runtime.bindingCalled");
        let replay_custom_payload = replay_custom_binding_called["params"]["payload"]
            .as_str()
            .expect("replay custom payload should be string");
        let replay_custom_payload: serde_json::Value = serde_json::from_str(replay_custom_payload)
            .expect("replay custom payload should be valid json");
        assert_eq!(replay_custom_payload["seq"], json!(1));
        ctx.sent.clear();

        ctx.process_async(json!({
        "id": 35355,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "expression": "globalThis.__lm_custom_binding_b_deliver({ name: 'customBindingB', seq: 1, result: 'replay-target-custom-b-ok' }); 'delivered'",
            "awaitPromise": true
        }
    })).await;
        let delivered_replay_custom = take_response_by_id(&mut ctx, 35355);
        assert_eq!(
            delivered_replay_custom["result"]["result"]["value"],
            json!("delivered")
        );

        ctx.process_async(json!({
            "id": 35356,
            "method": "Runtime.evaluate",
            "sessionId": second.session_id,
            "params": {
                "expression": "globalThis.__lm_replay_target_custom_b",
                "awaitPromise": true
            }
        }))
        .await;
        let resolved_replay_custom = take_response_by_id(&mut ctx, 35356);
        assert_eq!(
            resolved_replay_custom["result"]["result"]["value"],
            json!("replay-target-custom-b-ok")
        );

        ctx.process_async(json!({
        "id": 35357,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": replay_utility_context,
            "expression": "globalThis.__lm_replay_target_pw_handle_reject = __pw_keptHandleBinding(document.getElementById('utility-handle-b')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-replay-pw-handle-reject'",
            "awaitPromise": true
        }
    })).await;
        let scheduled_replay_handle_reject = take_response_by_id(&mut ctx, 35357);
        assert!(
            scheduled_replay_handle_reject["result"]["result"]["value"]
                .as_str()
                .expect("scheduled replay handle reject value")
                .starts_with("scheduled-")
        );
        let replay_handle_binding_called = ctx
            .sent
            .iter()
            .rev()
            .find(|message| {
                message["method"] == json!("Runtime.bindingCalled")
                    && message["sessionId"] == json!(second.session_id)
                    && message["params"]["name"] == json!("__pw_keptHandleBinding")
                    && message["params"]["executionContextId"] == json!(replay_utility_context)
            })
            .cloned()
            .expect("replay retained handle binding should emit Runtime.bindingCalled");
        let replay_handle_payload = replay_handle_binding_called["params"]["payload"]
            .as_str()
            .expect("replay handle payload should be string");
        let replay_handle_payload: serde_json::Value = serde_json::from_str(replay_handle_payload)
            .expect("replay handle payload should be valid json");
        let replay_handle_seq = replay_handle_payload["seq"]
            .as_i64()
            .expect("replay handle seq should be integer");
        assert_eq!(replay_handle_seq, 1);
        ctx.sent.clear();

        ctx.process_async(json!({
        "id": 35358,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": replay_utility_context,
            "expression": format!(
                "(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {replay_handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {replay_handle_seq} }})]); }})()"
            )
        }
    })).await;
        let taken_replay_handle = take_response_by_id(&mut ctx, 35358);
        assert_eq!(
            taken_replay_handle["result"]["result"]["value"],
            json!("[\"utility-handle-b\",\"replay-utility-handle-b\",\"undefined\"]")
        );

        ctx.process_async(json!({
        "id": 35359,
        "method": "Runtime.evaluate",
        "sessionId": second.session_id,
        "params": {
            "contextId": replay_utility_context,
            "expression": format!(
                "globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {replay_handle_seq}, error: 'replay-target-pw-handle-error' }}); 'delivered'"
            ),
            "awaitPromise": true
        }
    })).await;
        let delivered_replay_handle = take_response_by_id(&mut ctx, 35359);
        assert_eq!(
            delivered_replay_handle["result"]["result"]["value"],
            json!("delivered")
        );

        ctx.process_async(json!({
            "id": 35360,
            "method": "Runtime.evaluate",
            "sessionId": second.session_id,
            "params": {
                "contextId": replay_utility_context,
                "expression": "globalThis.__lm_replay_target_pw_handle_reject",
                "awaitPromise": true
            }
        }))
        .await;
        let rejected_replay_handle = take_response_by_id(&mut ctx, 35360);
        assert_eq!(
            rejected_replay_handle["result"]["result"]["value"],
            json!("rejected:replay-target-pw-handle-error")
        );

        ctx.process_async(json!({
            "id": 35361,
            "method": "Target.detachFromTarget",
            "params": {
                "targetId": second.target_id,
                "sessionId": second.session_id
            }
        }))
        .await;
        ctx.expect_result(35361, json!({}), None);
        ctx.expect_event(
            "Target.detachedFromTarget",
            Some(&json!({
                "targetId": second.target_id,
                "sessionId": second.session_id,
            })),
        );

        ctx.process_async(json!({
            "id": 35362,
            "method": "Target.attachToTarget",
            "params": {
                "targetId": second.target_id,
                "flatten": true
            }
        }))
        .await;
        let reattached_second_session_id =
            take_response_by_id(&mut ctx, 35362)["result"]["sessionId"]
                .as_str()
                .expect("reattached second target session id")
                .to_owned();
        assert_ne!(reattached_second_session_id, second.session_id);
        ctx.expect_event(
            "Target.attachedToTarget",
            Some(&json!({
                "sessionId": reattached_second_session_id,
                "targetInfo": {
                    "targetId": second.target_id,
                    "browserContextId": second.browser_context_id,
                }
            })),
        );
        ctx.take_all();

        for (id, context_id, expected_state) in [
            (
                35363_u64,
                None::<i64>,
                json!(
                    "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
                ),
            ),
            (
                35364_u64,
                Some(replay_utility_context),
                json!(
                    "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
                ),
            ),
        ] {
            let mut params = json!({
                "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
            });
            if let Some(context_id) = context_id {
                params["contextId"] = json!(context_id);
            }
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": reattached_second_session_id,
                "params": params
            }))
            .await;
            let state = take_response_by_id(&mut ctx, id);
            assert_eq!(state["result"]["result"]["value"], expected_state);
        }

        ctx.process_async(json!({
        "id": 35365,
        "method": "Runtime.evaluate",
        "sessionId": reattached_second_session_id,
        "params": {
            "expression": "globalThis.__lm_reattached_new_target_custom_b = customBindingB({ source: 'reattached-new-target-custom-b', nested: { count: 4, values: ['d', 5, true] } }); 'scheduled-custom-b'",
            "awaitPromise": true
        }
    })).await;
        let scheduled_reattached_custom = take_response_by_id(&mut ctx, 35365);
        assert!(
            scheduled_reattached_custom["result"]["result"]["value"]
                .as_str()
                .expect("scheduled reattached custom value")
                .starts_with("scheduled-")
        );
        let reattached_custom_binding_called = ctx
            .sent
            .iter()
            .rev()
            .find(|message| {
                message["method"] == json!("Runtime.bindingCalled")
                    && message["sessionId"] == json!(reattached_second_session_id)
                    && message["params"]["name"] == json!("customBindingB")
            })
            .cloned()
            .expect(
                "custom binding should emit Runtime.bindingCalled after reattaching the new target",
            );
        let reattached_custom_payload = reattached_custom_binding_called["params"]["payload"]
            .as_str()
            .expect("reattached custom payload should be string");
        let reattached_custom_payload: serde_json::Value =
            serde_json::from_str(reattached_custom_payload)
                .expect("reattached custom payload should be valid json");
        assert_eq!(reattached_custom_payload["seq"], json!(2));
        assert_eq!(
            reattached_custom_payload["serializedArgs"],
            json!([{
                "source": "reattached-new-target-custom-b",
                "nested": { "count": 4, "values": ["d", 5, true] }
            }])
        );
        ctx.sent.clear();

        ctx.process_async(json!({
        "id": 35366,
        "method": "Runtime.evaluate",
        "sessionId": reattached_second_session_id,
        "params": {
            "expression": "globalThis.__lm_custom_binding_b_deliver({ name: 'customBindingB', seq: 2, result: 'reattached-new-target-custom-b-ok' }); 'delivered'",
            "awaitPromise": true
        }
    })).await;
        let delivered_reattached_custom = take_response_by_id(&mut ctx, 35366);
        assert_eq!(
            delivered_reattached_custom["result"]["result"]["value"],
            json!("delivered")
        );

        ctx.process_async(json!({
            "id": 35367,
            "method": "Runtime.evaluate",
            "sessionId": reattached_second_session_id,
            "params": {
                "expression": "globalThis.__lm_reattached_new_target_custom_b",
                "awaitPromise": true
            }
        }))
        .await;
        let resolved_reattached_custom = take_response_by_id(&mut ctx, 35367);
        assert_eq!(
            resolved_reattached_custom["result"]["result"]["value"],
            json!("reattached-new-target-custom-b-ok")
        );

        ctx.process_async(json!({
        "id": 35368,
        "method": "Runtime.evaluate",
        "sessionId": reattached_second_session_id,
        "params": {
            "contextId": replay_utility_context,
            "expression": "globalThis.__lm_reattached_new_target_pw_handle_reject = __pw_keptHandleBinding(document.getElementById('utility-handle-b')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-pw-handle-reject'",
            "awaitPromise": true
        }
    })).await;
        let scheduled_reattached_handle_reject = take_response_by_id(&mut ctx, 35368);
        assert!(
            scheduled_reattached_handle_reject["result"]["result"]["value"]
                .as_str()
                .expect("scheduled reattached handle reject value")
                .starts_with("scheduled-")
        );
        let reattached_handle_binding_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(reattached_second_session_id)
                && message["params"]["name"] == json!("__pw_keptHandleBinding")
                && message["params"]["executionContextId"] == json!(replay_utility_context)
        })
        .cloned()
        .expect("retained handle binding should emit Runtime.bindingCalled after reattaching the new target");
        let reattached_handle_payload = reattached_handle_binding_called["params"]["payload"]
            .as_str()
            .expect("reattached handle payload should be string");
        let reattached_handle_payload: serde_json::Value =
            serde_json::from_str(reattached_handle_payload)
                .expect("reattached handle payload should be valid json");
        let reattached_handle_seq = reattached_handle_payload["seq"]
            .as_i64()
            .expect("reattached handle seq should be integer");
        assert_eq!(reattached_handle_seq, 2);
        ctx.sent.clear();

        ctx.process_async(json!({
        "id": 35369,
        "method": "Runtime.evaluate",
        "sessionId": reattached_second_session_id,
        "params": {
            "contextId": replay_utility_context,
            "expression": format!(
                "(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {reattached_handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {reattached_handle_seq} }})]); }})()"
            )
        }
    })).await;
        let taken_reattached_handle = take_response_by_id(&mut ctx, 35369);
        assert_eq!(
            taken_reattached_handle["result"]["result"]["value"],
            json!("[\"utility-handle-b\",\"replay-utility-handle-b\",\"undefined\"]")
        );

        ctx.process_async(json!({
        "id": 35370,
        "method": "Runtime.evaluate",
        "sessionId": reattached_second_session_id,
        "params": {
            "contextId": replay_utility_context,
            "expression": format!(
                "globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {reattached_handle_seq}, error: 'reattached-new-target-pw-handle-error' }}); 'delivered'"
            ),
            "awaitPromise": true
        }
    })).await;
        let delivered_reattached_handle = take_response_by_id(&mut ctx, 35370);
        assert_eq!(
            delivered_reattached_handle["result"]["result"]["value"],
            json!("delivered")
        );

        ctx.process_async(json!({
            "id": 35371,
            "method": "Runtime.evaluate",
            "sessionId": reattached_second_session_id,
            "params": {
                "contextId": replay_utility_context,
                "expression": "globalThis.__lm_reattached_new_target_pw_handle_reject",
                "awaitPromise": true
            }
        }))
        .await;
        let rejected_reattached_handle = take_response_by_id(&mut ctx, 35371);
        assert_eq!(
            rejected_reattached_handle["result"]["result"]["value"],
            json!("rejected:reattached-new-target-pw-handle-error")
        );

        ctx.process_async(json!({
            "id": 35372,
            "method": "Target.detachFromTarget",
            "params": {
                "targetId": second.target_id,
                "sessionId": reattached_second_session_id
            }
        }))
        .await;
        ctx.expect_result(35372, json!({}), None);
        ctx.expect_event(
            "Target.detachedFromTarget",
            Some(&json!({
                "targetId": second.target_id,
                "sessionId": reattached_second_session_id,
            })),
        );
        assert_eq!(
            ctx.conn
                .browser_contexts()
                .find(|bc| bc.id == second.browser_context_id)
                .and_then(|bc| bc.active_session_id()),
            None
        );

        ctx.process_async(json!({
            "id": 35373,
            "method": "Target.setAutoAttach",
            "params": {
                "autoAttach": true,
                "waitForDebuggerOnStart": false
            }
        }))
        .await;
        ctx.expect_result(35373, json!({}), None);
        let reauto_attach_event = ctx
            .take_first_matching("re-auto-attached target attachedToTarget", |message| {
                message["method"] == json!("Target.attachedToTarget")
            });
        assert_eq!(
            reauto_attach_event["params"]["targetInfo"]["targetId"],
            json!(second.target_id)
        );
        let reauto_attached_second_session_id = reauto_attach_event["params"]["sessionId"]
            .as_str()
            .expect("re-auto-attached second target session id")
            .to_owned();
        assert_ne!(
            reauto_attached_second_session_id,
            reattached_second_session_id
        );
        ctx.take_all();

        ctx.process_async(json!({
        "id": 35374,
        "method": "Runtime.evaluate",
        "sessionId": reauto_attached_second_session_id,
        "params": {
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
        }
    })).await;
        let reauto_main_state = take_response_by_id(&mut ctx, 35374);
        assert_eq!(
            reauto_main_state["result"]["result"]["value"],
            json!(
                "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
            )
        );

        ctx.process_async(json!({
            "id": 35375,
            "method": "Page.createIsolatedWorld",
            "sessionId": reauto_attached_second_session_id,
            "params": {
                "frameId": second.target_id,
                "worldName": "utility"
            }
        }))
        .await;
        let reauto_utility_context =
            take_response_by_id(&mut ctx, 35375)["result"]["executionContextId"]
                .as_i64()
                .expect("re-auto-attached utility context id");
        ctx.take_all();

        for (id, binding_name) in [
            (35376_u64, "customBindingB"),
            (35377_u64, "customHandleBindingB"),
            (35378_u64, "__pw_keptBinding"),
            (35379_u64, "__pw_keptHandleBinding"),
        ] {
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.addBinding",
                "sessionId": reauto_attached_second_session_id,
                "params": {
                    "name": binding_name,
                    "executionContextId": reauto_utility_context
                }
            }))
            .await;
            assert_eq!(take_response_by_id(&mut ctx, id)["result"], json!({}));
        }

        for (id, source, expected_type) in [
            (35380_u64, custom_wrapper_a_source.as_str(), "undefined"),
            (35381_u64, custom_wrapper_b_source.as_str(), "function"),
            (
                35382_u64,
                custom_handle_wrapper_a_source.as_str(),
                "undefined",
            ),
            (
                35383_u64,
                custom_handle_wrapper_b_source.as_str(),
                "function",
            ),
            (35384_u64, retained_wrapper_source.as_str(), "function"),
            (
                35385_u64,
                retained_handle_wrapper_source.as_str(),
                "function",
            ),
        ] {
            ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": reauto_attached_second_session_id,
                "params": {
                    "contextId": reauto_utility_context,
                    "expression": source,
                    "awaitPromise": true
                }
            }))
            .await;
            let replayed = take_response_by_id(&mut ctx, id);
            assert_eq!(replayed["result"]["result"]["value"], json!(expected_type));
        }

        ctx.process_async(json!({
        "id": 35386,
        "method": "Runtime.evaluate",
        "sessionId": reauto_attached_second_session_id,
        "params": {
            "contextId": reauto_utility_context,
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.customHandleBindingB, typeof globalThis.__pw_keptBinding, typeof globalThis.__pw_keptHandleBinding])"
        }
    })).await;
        let reauto_utility_state = take_response_by_id(&mut ctx, 35386);
        assert_eq!(
            reauto_utility_state["result"]["result"]["value"],
            json!(
                "[\"undefined\",\"function\",\"undefined\",\"function\",\"function\",\"function\"]"
            )
        );

        ctx.process_async(json!({
        "id": 35387,
        "method": "Runtime.evaluate",
        "sessionId": reauto_attached_second_session_id,
        "params": {
            "expression": "globalThis.__lm_reauto_new_target_custom_b = customBindingB({ source: 'reauto-new-target-custom-b', nested: { count: 5, values: ['e', 6, true] } }); 'scheduled-custom-b'",
            "awaitPromise": true
        }
    })).await;
        let scheduled_reauto_custom = take_response_by_id(&mut ctx, 35387);
        assert!(
            scheduled_reauto_custom["result"]["result"]["value"]
                .as_str()
                .expect("scheduled re-auto custom value")
                .starts_with("scheduled-")
        );
        let reauto_custom_binding_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(reauto_attached_second_session_id)
                && message["params"]["name"] == json!("customBindingB")
        })
        .cloned()
        .expect("custom binding should emit Runtime.bindingCalled after re-auto-attaching the new target");
        let reauto_custom_payload = reauto_custom_binding_called["params"]["payload"]
            .as_str()
            .expect("re-auto custom payload should be string");
        let reauto_custom_payload: serde_json::Value = serde_json::from_str(reauto_custom_payload)
            .expect("re-auto custom payload should be valid json");
        assert_eq!(reauto_custom_payload["seq"], json!(3));
        assert_eq!(
            reauto_custom_payload["serializedArgs"],
            json!([{
                "source": "reauto-new-target-custom-b",
                "nested": { "count": 5, "values": ["e", 6, true] }
            }])
        );
        ctx.sent.clear();

        ctx.process_async(json!({
        "id": 35388,
        "method": "Runtime.evaluate",
        "sessionId": reauto_attached_second_session_id,
        "params": {
            "expression": "globalThis.__lm_custom_binding_b_deliver({ name: 'customBindingB', seq: 3, result: 'reauto-new-target-custom-b-ok' }); 'delivered'",
            "awaitPromise": true
        }
    })).await;
        let delivered_reauto_custom = take_response_by_id(&mut ctx, 35388);
        assert_eq!(
            delivered_reauto_custom["result"]["result"]["value"],
            json!("delivered")
        );

        ctx.process_async(json!({
            "id": 35389,
            "method": "Runtime.evaluate",
            "sessionId": reauto_attached_second_session_id,
            "params": {
                "expression": "globalThis.__lm_reauto_new_target_custom_b",
                "awaitPromise": true
            }
        }))
        .await;
        let resolved_reauto_custom = take_response_by_id(&mut ctx, 35389);
        assert_eq!(
            resolved_reauto_custom["result"]["result"]["value"],
            json!("reauto-new-target-custom-b-ok")
        );

        ctx.process_async(json!({
        "id": 35390,
        "method": "Runtime.evaluate",
        "sessionId": reauto_attached_second_session_id,
        "params": {
            "contextId": reauto_utility_context,
            "expression": "globalThis.__lm_reauto_new_target_pw_handle_reject = __pw_keptHandleBinding(document.getElementById('utility-handle-b')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-pw-handle-reject'",
            "awaitPromise": true
        }
    })).await;
        let scheduled_reauto_handle_reject = take_response_by_id(&mut ctx, 35390);
        assert!(
            scheduled_reauto_handle_reject["result"]["result"]["value"]
                .as_str()
                .expect("scheduled re-auto handle reject value")
                .starts_with("scheduled-")
        );
        let reauto_handle_binding_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(reauto_attached_second_session_id)
                && message["params"]["name"] == json!("__pw_keptHandleBinding")
                && message["params"]["executionContextId"] == json!(reauto_utility_context)
        })
        .cloned()
        .expect("retained handle binding should emit Runtime.bindingCalled after re-auto-attaching the new target");
        let reauto_handle_payload = reauto_handle_binding_called["params"]["payload"]
            .as_str()
            .expect("re-auto handle payload should be string");
        let reauto_handle_payload: serde_json::Value = serde_json::from_str(reauto_handle_payload)
            .expect("re-auto handle payload should be valid json");
        let reauto_handle_seq = reauto_handle_payload["seq"]
            .as_i64()
            .expect("re-auto handle seq should be integer");
        assert_eq!(reauto_handle_seq, 1);
        ctx.sent.clear();

        ctx.process_async(json!({
        "id": 35391,
        "method": "Runtime.evaluate",
        "sessionId": reauto_attached_second_session_id,
        "params": {
            "contextId": reauto_utility_context,
            "expression": format!(
                "(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {reauto_handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {reauto_handle_seq} }})]); }})()"
            )
        }
    })).await;
        let taken_reauto_handle = take_response_by_id(&mut ctx, 35391);
        assert_eq!(
            taken_reauto_handle["result"]["result"]["value"],
            json!("[\"utility-handle-b\",\"replay-utility-handle-b\",\"undefined\"]")
        );

        ctx.process_async(json!({
        "id": 35392,
        "method": "Runtime.evaluate",
        "sessionId": reauto_attached_second_session_id,
        "params": {
            "contextId": reauto_utility_context,
            "expression": format!(
                "globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {reauto_handle_seq}, error: 'reauto-new-target-pw-handle-error' }}); 'delivered'"
            ),
            "awaitPromise": true
        }
    })).await;
        let delivered_reauto_handle = take_response_by_id(&mut ctx, 35392);
        assert_eq!(
            delivered_reauto_handle["result"]["result"]["value"],
            json!("delivered")
        );

        ctx.process_async(json!({
            "id": 35393,
            "method": "Runtime.evaluate",
            "sessionId": reauto_attached_second_session_id,
            "params": {
                "contextId": reauto_utility_context,
                "expression": "globalThis.__lm_reauto_new_target_pw_handle_reject",
                "awaitPromise": true
            }
        }))
        .await;
        let rejected_reauto_handle = take_response_by_id(&mut ctx, 35393);
        assert_eq!(
            rejected_reauto_handle["result"]["result"]["value"],
            json!("rejected:reauto-new-target-pw-handle-error")
        );
    });
}
