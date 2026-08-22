use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_utility_world_init_persists_across_targets_without_runtime_enable() {
    let mut ctx = TestContext::new();

    ctx.process_async(json!({
        "id": 211,
        "method": "Target.createBrowserContext"
    }))
    .await;
    let browser_context_id = ctx
        .conn
        .browser_context
        .as_ref()
        .expect("browser context should exist")
        .id
        .clone();
    ctx.expect_result(211, json!({ "browserContextId": browser_context_id }), None);

    ctx.process_async(json!({
        "id": 212,
        "method": "Target.createTarget",
        "params": {
            "browserContextId": browser_context_id,
            "url": "about:blank"
        }
    }))
    .await;
    let first_target = take_response_by_id(&mut ctx, 212)["result"]["targetId"]
        .as_str()
        .expect("first target id")
        .to_owned();
    ctx.expect_event(
        "Target.targetCreated",
        Some(&json!({
            "targetInfo": {
                "targetId": first_target,
                "browserContextId": browser_context_id,
            }
        })),
    );

    ctx.process_async(json!({
        "id": 213,
        "method": "Target.attachToTarget",
        "params": {
            "targetId": first_target,
            "flatten": true
        }
    }))
    .await;
    let first_session = ctx
        .conn
        .browser_context
        .as_ref()
        .and_then(|bc| bc.active_session_id_owned())
        .expect("first session id should exist");
    ctx.expect_result(213, json!({ "sessionId": first_session }), None);
    ctx.expect_event(
        "Target.attachedToTarget",
        Some(&json!({
            "sessionId": first_session,
            "targetInfo": {
                "targetId": first_target,
                "browserContextId": browser_context_id,
            }
        })),
    );

    ctx.process_async(json!({
        "id": 214,
        "method": "Page.addScriptToEvaluateOnNewDocument",
        "sessionId": first_session,
        "params": {
            "source": "globalThis.__lm_patchright_target_preload = 'ready';",
            "worldName": "utility"
        }
    }))
    .await;
    let preload = take_response_by_id(&mut ctx, 214);
    assert_eq!(preload["sessionId"], json!(first_session));
    assert!(preload["result"]["identifier"].is_string());

    ctx.process_async(json!({
        "id": 215,
        "method": "Runtime.addBinding",
        "sessionId": first_session,
        "params": {
            "name": "utilityBinding",
            "executionContextName": "utility"
        }
    }))
    .await;
    let add_binding = take_response_by_id(&mut ctx, 215);
    assert!(add_binding.get("error").is_none());
    assert!(
        !ctx.sent.iter().any(|message| {
            message["method"] == json!("Runtime.executionContextCreated")
                || message["method"] == json!("Runtime.executionContextsCleared")
        }),
        "Patchright-style setup should stay off Runtime.enable surfaces"
    );
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 216,
        "method": "Page.navigate",
        "sessionId": first_session,
        "params": {
            "url": "data:text/html,<body>first</body>"
        }
    }))
    .await;
    let first_navigation = take_response_by_id(&mut ctx, 216);
    assert_eq!(first_navigation["sessionId"], json!(first_session));
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

    ctx.process_async(json!({
        "id": 217,
        "method": "Page.createIsolatedWorld",
        "sessionId": first_session,
        "params": {
            "frameId": first_target,
            "worldName": "utility"
        }
    }))
    .await;
    let first_utility_context = take_response_by_id(&mut ctx, 217)["result"]["executionContextId"]
        .as_i64()
        .expect("first utility context id");

    ctx.process_async(json!({
            "id": 218,
            "method": "Runtime.evaluate",
            "sessionId": first_session,
            "params": {
                "contextId": first_utility_context,
                "expression": "utilityBinding('payload-first'); JSON.stringify([typeof globalThis.utilityBinding, globalThis.__lm_patchright_target_preload])"
            }
        })).await;
    let first_eval = take_response_by_id(&mut ctx, 218);
    assert_eq!(
        first_eval["result"]["result"]["value"],
        json!("[\"function\",\"ready\"]")
    );
    let first_binding_called = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("utilityBinding")
        })
        .cloned()
        .expect("first utility world binding call");
    assert_eq!(
        first_binding_called["params"]["payload"],
        json!("payload-first")
    );
    assert_eq!(
        first_binding_called["params"]["executionContextId"],
        json!(first_utility_context)
    );
    ctx.sent.clear();

    ctx.process_async(json!({
        "id": 219,
        "method": "Target.closeTarget",
        "params": { "targetId": first_target }
    }))
    .await;
    ctx.expect_result(219, json!({ "success": true }), None);
    ctx.take_all();

    ctx.process_async(json!({
        "id": 220,
        "method": "Target.createTarget",
        "params": {
            "browserContextId": browser_context_id,
            "url": "about:blank"
        }
    }))
    .await;
    let second_target = take_response_by_id(&mut ctx, 220)["result"]["targetId"]
        .as_str()
        .expect("second target id")
        .to_owned();
    ctx.expect_event(
        "Target.targetCreated",
        Some(&json!({
            "targetInfo": {
                "targetId": second_target,
                "browserContextId": browser_context_id,
            }
        })),
    );

    ctx.process_async(json!({
        "id": 221,
        "method": "Target.attachToTarget",
        "params": {
            "targetId": second_target,
            "flatten": true
        }
    }))
    .await;
    let second_session = ctx
        .conn
        .browser_context
        .as_ref()
        .and_then(|bc| bc.active_session_id_owned())
        .expect("second session id should exist");
    ctx.expect_result(221, json!({ "sessionId": second_session }), None);
    ctx.expect_event(
        "Target.attachedToTarget",
        Some(&json!({
            "sessionId": second_session,
            "targetInfo": {
                "targetId": second_target,
                "browserContextId": browser_context_id,
            }
        })),
    );

    ctx.process_async(json!({
        "id": 222,
        "method": "Page.navigate",
        "sessionId": second_session,
        "params": {
            "url": "data:text/html,<body>second</body>"
        }
    }))
    .await;
    let second_navigation = take_response_by_id(&mut ctx, 222);
    assert_eq!(second_navigation["sessionId"], json!(second_session));
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
        "id": 223,
        "method": "Page.createIsolatedWorld",
        "sessionId": second_session,
        "params": {
            "frameId": second_target,
            "worldName": "utility"
        }
    }))
    .await;
    let second_utility_context = take_response_by_id(&mut ctx, 223)["result"]["executionContextId"]
        .as_i64()
        .expect("second utility context id");

    ctx.process_async(json!({
            "id": 224,
            "method": "Runtime.evaluate",
            "sessionId": second_session,
            "params": {
                "contextId": second_utility_context,
                "expression": "utilityBinding('payload-second'); JSON.stringify([typeof globalThis.utilityBinding, globalThis.__lm_patchright_target_preload])"
            }
        })).await;
    let second_eval = take_response_by_id(&mut ctx, 224);
    assert_eq!(
        second_eval["result"]["result"]["value"],
        json!("[\"function\",\"ready\"]")
    );
    let second_binding_called = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("utilityBinding")
        })
        .cloned()
        .expect("second utility world binding call");
    assert_eq!(
        second_binding_called["params"]["payload"],
        json!("payload-second")
    );
    assert_eq!(
        second_binding_called["params"]["executionContextId"],
        json!(second_utility_context)
    );

    let active = ctx
        .conn
        .browser_context
        .as_ref()
        .expect("active browser context");
    assert_eq!(active.id, browser_context_id);
    assert_eq!(active.active_target_id(), Some(second_target.as_str()));
    assert_eq!(active.active_session_id(), Some(second_session.as_str()));
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
                binding.name == "utilityBinding"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "utility binding should persist on the browser context"
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_auto_attach_sweep_crpage_replacement_targets_keep_cleanup_isolated_per_browser_context_without_runtime_enable()
 {
    patchright_replacement_targets_large_stack(|| async {
    let mut ctx = TestContext::new();
    let first =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 35400, 35401, 35402)
            .await;
    let second =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 35403, 35404, 35405)
            .await;

    for (id, session_id, html) in [
        (
            35406_u64,
            first.session_id.as_str(),
            "<body><div id='utility-handle-a'>first-a</div><div id='utility-handle-b'>first-b</div></body>",
        ),
        (
            35407_u64,
            second.session_id.as_str(),
            "<body><div id='utility-handle-a'>second-a</div><div id='utility-handle-b'>second-b</div></body>",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": { "url": format!("data:text/html,{html}") }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    let mut first_utility_context = 0_i64;
    let mut second_utility_context = 0_i64;
    for (id, session_id, target_id, out_context) in [
        (
            35408_u64,
            first.session_id.as_str(),
            first.target_id.as_str(),
            &mut first_utility_context,
        ),
        (
            35409_u64,
            second.session_id.as_str(),
            second.target_id.as_str(),
            &mut second_utility_context,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.createIsolatedWorld",
            "sessionId": session_id,
            "params": {
                "frameId": target_id,
                "worldName": "utility"
            }
        }))
        .await;
        *out_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("initial utility context id");
        ctx.take_all();
    }

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
    let retained_handle_wrapper_source = patchright_page_binding_wrapper_source(
        "__pw_keptHandleBinding",
        "__lm_pw_kept_handle_binding_deliver",
        Some("__lm_pw_kept_handle_binding_take"),
        true,
    );

    for (session_id, utility_context_id, id_base) in [
        (first.session_id.as_str(), first_utility_context, 35410_u64),
        (
            second.session_id.as_str(),
            second_utility_context,
            35430_u64,
        ),
    ] {
        for (binding_name, source, offset) in [
            ("customBindingA", custom_wrapper_a_source.as_str(), 0_u64),
            ("customBindingB", custom_wrapper_b_source.as_str(), 4_u64),
            (
                "customHandleBindingA",
                custom_handle_wrapper_a_source.as_str(),
                8_u64,
            ),
            (
                "__pw_keptHandleBinding",
                retained_handle_wrapper_source.as_str(),
                12_u64,
            ),
        ] {
            install_patchright_crpage_binding_in_existing_worlds_async(
                &mut ctx,
                session_id,
                utility_context_id,
                id_base + offset,
                id_base + offset + 1,
                id_base + offset + 2,
                id_base + offset + 3,
                binding_name,
                source,
            )
            .await;
        }
    }

    for (session_id, id_base) in [
        (first.session_id.as_str(), 35450_u64),
        (second.session_id.as_str(), 35460_u64),
    ] {
        for (source, world_name, offset) in [
            (custom_wrapper_a_source.as_str(), None, 0_u64),
            (custom_wrapper_b_source.as_str(), None, 1_u64),
            (custom_handle_wrapper_a_source.as_str(), None, 2_u64),
            (retained_handle_wrapper_source.as_str(), None, 3_u64),
            (custom_wrapper_a_source.as_str(), Some("utility"), 4_u64),
            (custom_wrapper_b_source.as_str(), Some("utility"), 5_u64),
            (
                custom_handle_wrapper_a_source.as_str(),
                Some("utility"),
                6_u64,
            ),
            (
                retained_handle_wrapper_source.as_str(),
                Some("utility"),
                7_u64,
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
                "id": id_base + offset,
                "method": "Page.addScriptToEvaluateOnNewDocument",
                "sessionId": session_id,
                "params": params
            }))
            .await;
            assert!(
                take_response_by_id(&mut ctx, id_base + offset)["result"]["identifier"]
                    .as_str()
                    .is_some()
            );
        }
    }

    for (id, binding_name) in [
        (35470_u64, "customBindingA"),
        (35471_u64, "customHandleBindingA"),
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

    for (id, target_id) in [
        (35472_u64, first.target_id.as_str()),
        (35473_u64, second.target_id.as_str()),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Target.closeTarget",
            "params": { "targetId": target_id }
        }))
        .await;
        ctx.expect_result(id, json!({ "success": true }), None);
        ctx.take_all();
    }

    let first_replacement = attach_page_session_without_runtime_enable_in_existing_context_async(
        &mut ctx,
        &first.browser_context_id,
        35474,
        35475,
    )
    .await;
    let second_replacement = attach_page_session_without_runtime_enable_in_existing_context_async(
        &mut ctx,
        &second.browser_context_id,
        35476,
        35477,
    )
    .await;

    for (id, session_id, html) in [
        (
            35478_u64,
            first_replacement.session_id.as_str(),
            "<body><div id='utility-handle-a'>first-replacement-a</div><div id='utility-handle-b'>first-replacement-b</div></body>",
        ),
        (
            35479_u64,
            second_replacement.session_id.as_str(),
            "<body><div id='utility-handle-a'>second-replacement-a</div><div id='utility-handle-b'>second-replacement-b</div></body>",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": { "url": format!("data:text/html,{html}") }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, session_id, expected_state) in [
        (
            35480_u64,
            first_replacement.session_id.as_str(),
            json!("[\"undefined\",\"function\",\"undefined\",\"function\"]"),
        ),
        (
            35481_u64,
            second_replacement.session_id.as_str(),
            json!("[\"function\",\"function\",\"function\",\"function\"]"),
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "expression": format!(
                        "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.__pw_keptHandleBinding])"
                    )
                }
            })).await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    for (id, target_id, session_id) in [
        (
            35482_u64,
            first_replacement.target_id.as_str(),
            first_replacement.session_id.as_str(),
        ),
        (
            35483_u64,
            second_replacement.target_id.as_str(),
            second_replacement.session_id.as_str(),
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Target.detachFromTarget",
            "params": {
                "targetId": target_id,
                "sessionId": session_id
            }
        }))
        .await;
        ctx.expect_result(id, json!({}), None);
        ctx.expect_event(
            "Target.detachedFromTarget",
            Some(&json!({
                "targetId": target_id,
                "sessionId": session_id,
            })),
        );
    }

    ctx.process_async(json!({
        "id": 35484,
        "method": "Target.setAutoAttach",
        "params": {
            "autoAttach": true,
            "waitForDebuggerOnStart": false
        }
    }))
    .await;
    ctx.expect_result(35484, json!({}), None);
    let events = ctx.take_all();
    let attached_events = events
        .iter()
        .filter(|event| event["method"] == json!("Target.attachedToTarget"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(attached_events.len(), 2);
    let first_reauto_session = attached_events
        .iter()
        .find(|event| {
            event["params"]["targetInfo"]["targetId"] == json!(first_replacement.target_id)
        })
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("first replacement re-auto-attached session")
        .to_owned();
    let second_reauto_session = attached_events
        .iter()
        .find(|event| {
            event["params"]["targetInfo"]["targetId"] == json!(second_replacement.target_id)
        })
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("second replacement re-auto-attached session")
        .to_owned();

    for (id, session_id, expected_state) in [
        (
            35485_u64,
            first_reauto_session.as_str(),
            json!("[\"undefined\",\"function\",\"undefined\",\"function\"]"),
        ),
        (
            35486_u64,
            second_reauto_session.as_str(),
            json!("[\"function\",\"function\",\"function\",\"function\"]"),
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.__pw_keptHandleBinding])"
                }
            })).await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    let mut first_reauto_utility_context = 0_i64;
    let mut second_reauto_utility_context = 0_i64;
    for (id, session_id, target_id, out_context) in [
        (
            35487_u64,
            first_reauto_session.as_str(),
            first_replacement.target_id.as_str(),
            &mut first_reauto_utility_context,
        ),
        (
            35488_u64,
            second_reauto_session.as_str(),
            second_replacement.target_id.as_str(),
            &mut second_reauto_utility_context,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.createIsolatedWorld",
            "sessionId": session_id,
            "params": {
                "frameId": target_id,
                "worldName": "utility"
            }
        }))
        .await;
        *out_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("re-auto-attached utility context id");
        ctx.take_all();
    }

    for (session_id, utility_context_id, add_base, eval_base, cleaned) in [
        (
            first_reauto_session.as_str(),
            first_reauto_utility_context,
            35489_u64,
            35499_u64,
            true,
        ),
        (
            second_reauto_session.as_str(),
            second_reauto_utility_context,
            35509_u64,
            35519_u64,
            false,
        ),
    ] {
        let bindings = if cleaned {
            vec![
                ("customBindingB", custom_wrapper_b_source.as_str()),
                (
                    "__pw_keptHandleBinding",
                    retained_handle_wrapper_source.as_str(),
                ),
            ]
        } else {
            vec![
                ("customBindingA", custom_wrapper_a_source.as_str()),
                ("customBindingB", custom_wrapper_b_source.as_str()),
                (
                    "customHandleBindingA",
                    custom_handle_wrapper_a_source.as_str(),
                ),
                (
                    "__pw_keptHandleBinding",
                    retained_handle_wrapper_source.as_str(),
                ),
            ]
        };

        for (offset, (binding_name, source)) in bindings.iter().enumerate() {
            let add_id = add_base + offset as u64;
            let eval_id = eval_base + offset as u64;
            ctx.process_async(json!({
                "id": add_id,
                "method": "Runtime.addBinding",
                "sessionId": session_id,
                "params": {
                    "name": binding_name,
                    "executionContextId": utility_context_id
                }
            }))
            .await;
            assert_eq!(take_response_by_id(&mut ctx, add_id)["result"], json!({}));

            ctx.process_async(json!({
                "id": eval_id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context_id,
                    "expression": source,
                    "awaitPromise": true
                }
            }))
            .await;
            let installed = take_response_by_id(&mut ctx, eval_id);
            assert_eq!(installed["result"]["result"]["value"], json!("function"));
        }
    }

    for (id, session_id, context_id, expected_state) in [
        (
            35529_u64,
            first_reauto_session.as_str(),
            first_reauto_utility_context,
            json!("[\"undefined\",\"function\",\"undefined\",\"function\"]"),
        ),
        (
            35530_u64,
            second_reauto_session.as_str(),
            second_reauto_utility_context,
            json!("[\"function\",\"function\",\"function\",\"function\"]"),
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": context_id,
                    "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.__pw_keptHandleBinding])"
                }
            })).await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    for (id, session_id, source, expected_type) in [
        (
            35545_u64,
            first_reauto_session.as_str(),
            custom_wrapper_a_source.as_str(),
            "undefined",
        ),
        (
            35546_u64,
            first_reauto_session.as_str(),
            custom_wrapper_b_source.as_str(),
            "function",
        ),
        (
            35547_u64,
            first_reauto_session.as_str(),
            custom_handle_wrapper_a_source.as_str(),
            "undefined",
        ),
        (
            35548_u64,
            first_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
        (
            35549_u64,
            second_reauto_session.as_str(),
            custom_wrapper_a_source.as_str(),
            "function",
        ),
        (
            35550_u64,
            second_reauto_session.as_str(),
            custom_wrapper_b_source.as_str(),
            "function",
        ),
        (
            35551_u64,
            second_reauto_session.as_str(),
            custom_handle_wrapper_a_source.as_str(),
            "function",
        ),
        (
            35552_u64,
            second_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "expression": source,
                "awaitPromise": true
            }
        }))
        .await;
        let installed = take_response_by_id(&mut ctx, id);
        assert_eq!(installed["result"]["result"]["value"], json!(expected_type));
    }

    ctx.process_async(json!({
            "id": 35531,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "expression": "globalThis.__lm_first_reauto_custom_b = customBindingB({ source: 'first-reauto-custom-b', nested: { count: 1, values: ['x', 2, true] } }); 'scheduled-first'"
            }
        })).await;
    let scheduled_first_custom = take_response_by_id(&mut ctx, 35531);
    assert!(
        scheduled_first_custom["result"]["result"]["value"]
            .as_str()
            .expect("scheduled first custom")
            .starts_with("scheduled-")
    );
    let first_custom_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(first_reauto_session)
                && message["params"]["name"] == json!("customBindingB")
        })
        .cloned()
        .expect("first cleaned replacement should emit customBindingB");
    let first_custom_payload: serde_json::Value = serde_json::from_str(
        first_custom_called["params"]["payload"]
            .as_str()
            .expect("first custom payload string"),
    )
    .expect("first custom payload json");
    let first_custom_seq = first_custom_payload["seq"]
        .as_i64()
        .expect("first custom seq");
    assert_eq!(
        first_custom_payload["serializedArgs"],
        json!([{ "source": "first-reauto-custom-b", "nested": { "count": 1, "values": ["x", 2, true] } }])
    );
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35532,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "expression": format!("globalThis.__lm_custom_binding_b_deliver({{ name: 'customBindingB', seq: {first_custom_seq}, result: 'first-reauto-custom-b-ok' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35532)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35533,
        "method": "Runtime.evaluate",
        "sessionId": first_reauto_session,
        "params": {
            "expression": "globalThis.__lm_first_reauto_custom_b",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35533)["result"]["result"]["value"],
        json!("first-reauto-custom-b-ok")
    );

    ctx.process_async(json!({
            "id": 35534,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "expression": "globalThis.__lm_second_reauto_custom_a = customBindingA({ source: 'second-reauto-custom-a', nested: { count: 2, values: ['y', 3, false] } }); 'scheduled-second'"
            }
        })).await;
    let scheduled_second_custom = take_response_by_id(&mut ctx, 35534);
    assert!(
        scheduled_second_custom["result"]["result"]["value"]
            .as_str()
            .expect("scheduled second custom")
            .starts_with("scheduled-")
    );
    let second_custom_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(second_reauto_session)
                && message["params"]["name"] == json!("customBindingA")
        })
        .cloned()
        .expect("second untouched replacement should emit customBindingA");
    let second_custom_payload: serde_json::Value = serde_json::from_str(
        second_custom_called["params"]["payload"]
            .as_str()
            .expect("second custom payload string"),
    )
    .expect("second custom payload json");
    let second_custom_seq = second_custom_payload["seq"]
        .as_i64()
        .expect("second custom seq");
    assert_eq!(
        second_custom_payload["serializedArgs"],
        json!([{ "source": "second-reauto-custom-a", "nested": { "count": 2, "values": ["y", 3, false] } }])
    );
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35535,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "expression": format!("globalThis.__lm_custom_binding_a_deliver({{ name: 'customBindingA', seq: {second_custom_seq}, result: 'second-reauto-custom-a-ok' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35535)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35536,
        "method": "Runtime.evaluate",
        "sessionId": second_reauto_session,
        "params": {
            "expression": "globalThis.__lm_second_reauto_custom_a",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35536)["result"]["result"]["value"],
        json!("second-reauto-custom-a-ok")
    );

    ctx.process_async(json!({
            "id": 35537,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_reauto_utility_context,
                "expression": "globalThis.__lm_first_reauto_pw_handle_reject = __pw_keptHandleBinding(document.getElementById('utility-handle-b')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-first-handle'",
                "awaitPromise": true
            }
        })).await;
    let scheduled_first_handle = take_response_by_id(&mut ctx, 35537);
    assert!(
        scheduled_first_handle["result"]["result"]["value"]
            .as_str()
            .expect("scheduled first handle")
            .starts_with("scheduled-")
    );
    let first_handle_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(first_reauto_session)
                && message["params"]["name"] == json!("__pw_keptHandleBinding")
                && message["params"]["executionContextId"] == json!(first_reauto_utility_context)
        })
        .cloned()
        .expect("first cleaned replacement should emit retained handle binding");
    let first_handle_payload: serde_json::Value = serde_json::from_str(
        first_handle_called["params"]["payload"]
            .as_str()
            .expect("first handle payload string"),
    )
    .expect("first handle payload json");
    let first_handle_seq = first_handle_payload["seq"]
        .as_i64()
        .expect("first handle seq");
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35538,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_reauto_utility_context,
                "expression": format!("(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {first_handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {first_handle_seq} }})]); }})()")
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35538)["result"]["result"]["value"],
        json!("[\"utility-handle-b\",\"first-replacement-b\",\"undefined\"]")
    );

    ctx.process_async(json!({
            "id": 35539,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_reauto_utility_context,
                "expression": format!("globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {first_handle_seq}, error: 'first-reauto-pw-handle-error' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35539)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35540,
        "method": "Runtime.evaluate",
        "sessionId": first_reauto_session,
        "params": {
            "contextId": first_reauto_utility_context,
            "expression": "globalThis.__lm_first_reauto_pw_handle_reject",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35540)["result"]["result"]["value"],
        json!("rejected:first-reauto-pw-handle-error")
    );

    ctx.process_async(json!({
            "id": 35541,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "contextId": second_reauto_utility_context,
                "expression": "globalThis.__lm_second_reauto_custom_handle_a = customHandleBindingA(document.getElementById('utility-handle-a')); 'scheduled-second-handle'",
                "awaitPromise": true
            }
        })).await;
    let scheduled_second_handle = take_response_by_id(&mut ctx, 35541);
    assert!(
        scheduled_second_handle["result"]["result"]["value"]
            .as_str()
            .expect("scheduled second handle")
            .starts_with("scheduled-")
    );
    let second_handle_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(second_reauto_session)
                && message["params"]["name"] == json!("customHandleBindingA")
                && message["params"]["executionContextId"] == json!(second_reauto_utility_context)
        })
        .cloned()
        .expect("second untouched replacement should emit custom handle binding");
    let second_handle_payload: serde_json::Value = serde_json::from_str(
        second_handle_called["params"]["payload"]
            .as_str()
            .expect("second handle payload string"),
    )
    .expect("second handle payload json");
    let second_handle_seq = second_handle_payload["seq"]
        .as_i64()
        .expect("second handle seq");
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35542,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "contextId": second_reauto_utility_context,
                "expression": format!("(() => {{ const handle = globalThis.__lm_custom_handle_binding_a_take({{ name: 'customHandleBindingA', seq: {second_handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_custom_handle_binding_a_take({{ name: 'customHandleBindingA', seq: {second_handle_seq} }})]); }})()")
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35542)["result"]["result"]["value"],
        json!("[\"utility-handle-a\",\"second-replacement-a\",\"undefined\"]")
    );

    ctx.process_async(json!({
            "id": 35543,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "contextId": second_reauto_utility_context,
                "expression": format!("globalThis.__lm_custom_handle_binding_a_deliver({{ name: 'customHandleBindingA', seq: {second_handle_seq}, result: 'second-reauto-custom-handle-a-ok' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35543)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35544,
        "method": "Runtime.evaluate",
        "sessionId": second_reauto_session,
        "params": {
            "contextId": second_reauto_utility_context,
            "expression": "globalThis.__lm_second_reauto_custom_handle_a",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35544)["result"]["result"]["value"],
        json!("second-reauto-custom-handle-a-ok")
    );

    for (id, session_id, html) in [
        (
            35553_u64,
            first_reauto_session.as_str(),
            "<body><div id='utility-handle-a'>first-reauto-replay-a</div><div id='utility-handle-b'>first-reauto-replay-b</div></body>",
        ),
        (
            35554_u64,
            second_reauto_session.as_str(),
            "<body><div id='utility-handle-a'>second-reauto-replay-a</div><div id='utility-handle-b'>second-reauto-replay-b</div></body>",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": { "url": format!("data:text/html,{html}") }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, session_id, source, expected_type) in [
        (
            35555_u64,
            first_reauto_session.as_str(),
            custom_wrapper_a_source.as_str(),
            "undefined",
        ),
        (
            35556_u64,
            first_reauto_session.as_str(),
            custom_wrapper_b_source.as_str(),
            "function",
        ),
        (
            35557_u64,
            first_reauto_session.as_str(),
            custom_handle_wrapper_a_source.as_str(),
            "undefined",
        ),
        (
            35558_u64,
            first_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
        (
            35559_u64,
            second_reauto_session.as_str(),
            custom_wrapper_a_source.as_str(),
            "function",
        ),
        (
            35560_u64,
            second_reauto_session.as_str(),
            custom_wrapper_b_source.as_str(),
            "function",
        ),
        (
            35561_u64,
            second_reauto_session.as_str(),
            custom_handle_wrapper_a_source.as_str(),
            "function",
        ),
        (
            35562_u64,
            second_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "expression": source,
                "awaitPromise": true
            }
        }))
        .await;
        let replayed = take_response_by_id(&mut ctx, id);
        assert_eq!(replayed["result"]["result"]["value"], json!(expected_type));
    }

    let mut first_reauto_replay_utility_context = 0_i64;
    let mut second_reauto_replay_utility_context = 0_i64;
    for (id, session_id, target_id, out_context) in [
        (
            35563_u64,
            first_reauto_session.as_str(),
            first_replacement.target_id.as_str(),
            &mut first_reauto_replay_utility_context,
        ),
        (
            35564_u64,
            second_reauto_session.as_str(),
            second_replacement.target_id.as_str(),
            &mut second_reauto_replay_utility_context,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.createIsolatedWorld",
            "sessionId": session_id,
            "params": {
                "frameId": target_id,
                "worldName": "utility"
            }
        }))
        .await;
        *out_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("re-auto replay utility context id");
        ctx.take_all();
    }

    for (session_id, utility_context_id, add_base, eval_base, cleaned) in [
        (
            first_reauto_session.as_str(),
            first_reauto_replay_utility_context,
            35565_u64,
            35575_u64,
            true,
        ),
        (
            second_reauto_session.as_str(),
            second_reauto_replay_utility_context,
            35585_u64,
            35595_u64,
            false,
        ),
    ] {
        let bindings = if cleaned {
            vec![
                ("customBindingB", custom_wrapper_b_source.as_str()),
                (
                    "__pw_keptHandleBinding",
                    retained_handle_wrapper_source.as_str(),
                ),
            ]
        } else {
            vec![
                ("customBindingA", custom_wrapper_a_source.as_str()),
                ("customBindingB", custom_wrapper_b_source.as_str()),
                (
                    "customHandleBindingA",
                    custom_handle_wrapper_a_source.as_str(),
                ),
                (
                    "__pw_keptHandleBinding",
                    retained_handle_wrapper_source.as_str(),
                ),
            ]
        };

        for (offset, (binding_name, source)) in bindings.iter().enumerate() {
            let add_id = add_base + offset as u64;
            let eval_id = eval_base + offset as u64;
            ctx.process_async(json!({
                "id": add_id,
                "method": "Runtime.addBinding",
                "sessionId": session_id,
                "params": {
                    "name": binding_name,
                    "executionContextId": utility_context_id
                }
            }))
            .await;
            assert_eq!(take_response_by_id(&mut ctx, add_id)["result"], json!({}));

            ctx.process_async(json!({
                "id": eval_id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context_id,
                    "expression": source,
                    "awaitPromise": true
                }
            }))
            .await;
            let installed = take_response_by_id(&mut ctx, eval_id);
            assert_eq!(installed["result"]["result"]["value"], json!("function"));
        }
    }

    for (id, session_id, context_id, expected_state) in [
        (
            35605_u64,
            first_reauto_session.as_str(),
            first_reauto_replay_utility_context,
            json!("[\"undefined\",\"function\",\"undefined\",\"function\"]"),
        ),
        (
            35606_u64,
            second_reauto_session.as_str(),
            second_reauto_replay_utility_context,
            json!("[\"function\",\"function\",\"function\",\"function\"]"),
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": context_id,
                    "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.__pw_keptHandleBinding])"
                }
            })).await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    ctx.process_async(json!({
            "id": 35607,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "expression": "globalThis.__lm_first_reauto_replay_custom_b = customBindingB({ source: 'first-reauto-replay-custom-b', nested: { count: 6, values: ['m', 7, true] } }); 'scheduled-first-replay'"
            }
        })).await;
    let scheduled_first_replay_custom = take_response_by_id(&mut ctx, 35607);
    assert!(
        scheduled_first_replay_custom["result"]["result"]["value"]
            .as_str()
            .expect("scheduled first replay custom")
            .starts_with("scheduled-")
    );
    let first_replay_custom_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(first_reauto_session)
                && message["params"]["name"] == json!("customBindingB")
        })
        .cloned()
        .expect("first replay custom bindingCalled");
    let first_replay_custom_payload: serde_json::Value = serde_json::from_str(
        first_replay_custom_called["params"]["payload"]
            .as_str()
            .expect("first replay custom payload string"),
    )
    .expect("first replay custom payload json");
    let first_replay_custom_seq = first_replay_custom_payload["seq"]
        .as_i64()
        .expect("first replay custom seq");
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35608,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "expression": format!("globalThis.__lm_custom_binding_b_deliver({{ name: 'customBindingB', seq: {first_replay_custom_seq}, result: 'first-reauto-replay-custom-b-ok' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35608)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35609,
        "method": "Runtime.evaluate",
        "sessionId": first_reauto_session,
        "params": {
            "expression": "globalThis.__lm_first_reauto_replay_custom_b",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35609)["result"]["result"]["value"],
        json!("first-reauto-replay-custom-b-ok")
    );

    ctx.process_async(json!({
            "id": 35610,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "contextId": second_reauto_replay_utility_context,
                "expression": "globalThis.__lm_second_reauto_replay_custom_handle_a_reject = customHandleBindingA(document.getElementById('utility-handle-a')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-second-replay-handle'",
                "awaitPromise": true
            }
        })).await;
    let scheduled_second_replay_handle = take_response_by_id(&mut ctx, 35610);
    assert!(
        scheduled_second_replay_handle["result"]["result"]["value"]
            .as_str()
            .expect("scheduled second replay handle")
            .starts_with("scheduled-")
    );
    let second_replay_handle_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(second_reauto_session)
                && message["params"]["name"] == json!("customHandleBindingA")
                && message["params"]["executionContextId"]
                    == json!(second_reauto_replay_utility_context)
        })
        .cloned()
        .expect("second replay custom handle bindingCalled");
    let second_replay_handle_payload: serde_json::Value = serde_json::from_str(
        second_replay_handle_called["params"]["payload"]
            .as_str()
            .expect("second replay handle payload string"),
    )
    .expect("second replay handle payload json");
    let second_replay_handle_seq = second_replay_handle_payload["seq"]
        .as_i64()
        .expect("second replay handle seq");
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35611,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "contextId": second_reauto_replay_utility_context,
                "expression": format!("(() => {{ const handle = globalThis.__lm_custom_handle_binding_a_take({{ name: 'customHandleBindingA', seq: {second_replay_handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_custom_handle_binding_a_take({{ name: 'customHandleBindingA', seq: {second_replay_handle_seq} }})]); }})()")
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35611)["result"]["result"]["value"],
        json!("[\"utility-handle-a\",\"second-reauto-replay-a\",\"undefined\"]")
    );

    ctx.process_async(json!({
            "id": 35612,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "contextId": second_reauto_replay_utility_context,
                "expression": format!("globalThis.__lm_custom_handle_binding_a_deliver({{ name: 'customHandleBindingA', seq: {second_replay_handle_seq}, error: 'second-reauto-replay-custom-handle-a-error' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35612)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35613,
        "method": "Runtime.evaluate",
        "sessionId": second_reauto_session,
        "params": {
            "contextId": second_reauto_replay_utility_context,
            "expression": "globalThis.__lm_second_reauto_replay_custom_handle_a_reject",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35613)["result"]["result"]["value"],
        json!("rejected:second-reauto-replay-custom-handle-a-error")
    );

    ctx.process_async(json!({
        "id": 35614,
        "method": "Runtime.removeBinding",
        "sessionId": first_reauto_session,
        "params": { "name": "customBindingB" }
    }))
    .await;
    assert_eq!(take_response_by_id(&mut ctx, 35614)["result"], json!({}));

    for (id, session_id, expected_state) in [
        (
            35615_u64,
            first_reauto_session.as_str(),
            json!("[\"undefined\",\"undefined\",\"undefined\",\"function\"]"),
        ),
        (
            35616_u64,
            first_reauto_session.as_str(),
            json!("[\"undefined\",\"undefined\",\"undefined\",\"function\"]"),
        ),
        (
            35617_u64,
            second_reauto_session.as_str(),
            json!("[\"function\",\"function\",\"function\",\"function\"]"),
        ),
        (
            35618_u64,
            second_reauto_session.as_str(),
            json!("[\"function\",\"function\",\"function\",\"function\"]"),
        ),
    ] {
        let params = if id == 35615 || id == 35617 {
            json!({
                "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.__pw_keptHandleBinding])"
            })
        } else {
            json!({
                "contextId": if id == 35616 {
                    first_reauto_replay_utility_context
                } else {
                    second_reauto_replay_utility_context
                },
                "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.__pw_keptHandleBinding])"
            })
        };
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": params
        }))
        .await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    for (id, session_id, html) in [
        (
            35619_u64,
            first_reauto_session.as_str(),
            "<body><div id='utility-handle-a'>first-reauto-post-cleanup-a</div><div id='utility-handle-b'>first-reauto-post-cleanup-b</div></body>",
        ),
        (
            35620_u64,
            second_reauto_session.as_str(),
            "<body><div id='utility-handle-a'>second-reauto-post-cleanup-a</div><div id='utility-handle-b'>second-reauto-post-cleanup-b</div></body>",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": { "url": format!("data:text/html,{html}") }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, session_id, source, expected_type) in [
        (
            35621_u64,
            first_reauto_session.as_str(),
            custom_wrapper_a_source.as_str(),
            "undefined",
        ),
        (
            35622_u64,
            first_reauto_session.as_str(),
            custom_wrapper_b_source.as_str(),
            "undefined",
        ),
        (
            35623_u64,
            first_reauto_session.as_str(),
            custom_handle_wrapper_a_source.as_str(),
            "undefined",
        ),
        (
            35624_u64,
            first_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
        (
            35625_u64,
            second_reauto_session.as_str(),
            custom_wrapper_a_source.as_str(),
            "function",
        ),
        (
            35626_u64,
            second_reauto_session.as_str(),
            custom_wrapper_b_source.as_str(),
            "function",
        ),
        (
            35627_u64,
            second_reauto_session.as_str(),
            custom_handle_wrapper_a_source.as_str(),
            "function",
        ),
        (
            35628_u64,
            second_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "expression": source,
                "awaitPromise": true
            }
        }))
        .await;
        let replayed = take_response_by_id(&mut ctx, id);
        assert_eq!(replayed["result"]["result"]["value"], json!(expected_type));
    }

    let mut first_post_cleanup_utility_context = 0_i64;
    let mut second_post_cleanup_utility_context = 0_i64;
    for (id, session_id, target_id, out_context) in [
        (
            35629_u64,
            first_reauto_session.as_str(),
            first_replacement.target_id.as_str(),
            &mut first_post_cleanup_utility_context,
        ),
        (
            35630_u64,
            second_reauto_session.as_str(),
            second_replacement.target_id.as_str(),
            &mut second_post_cleanup_utility_context,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.createIsolatedWorld",
            "sessionId": session_id,
            "params": {
                "frameId": target_id,
                "worldName": "utility"
            }
        }))
        .await;
        *out_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("post-cleanup replay utility context id");
        ctx.take_all();
    }

    for (session_id, utility_context_id, add_base, eval_base, cleaned) in [
        (
            first_reauto_session.as_str(),
            first_post_cleanup_utility_context,
            35631_u64,
            35641_u64,
            true,
        ),
        (
            second_reauto_session.as_str(),
            second_post_cleanup_utility_context,
            35651_u64,
            35661_u64,
            false,
        ),
    ] {
        let bindings = if cleaned {
            vec![(
                "__pw_keptHandleBinding",
                retained_handle_wrapper_source.as_str(),
            )]
        } else {
            vec![
                ("customBindingA", custom_wrapper_a_source.as_str()),
                ("customBindingB", custom_wrapper_b_source.as_str()),
                (
                    "customHandleBindingA",
                    custom_handle_wrapper_a_source.as_str(),
                ),
                (
                    "__pw_keptHandleBinding",
                    retained_handle_wrapper_source.as_str(),
                ),
            ]
        };

        for (offset, (binding_name, source)) in bindings.iter().enumerate() {
            let add_id = add_base + offset as u64;
            let eval_id = eval_base + offset as u64;
            ctx.process_async(json!({
                "id": add_id,
                "method": "Runtime.addBinding",
                "sessionId": session_id,
                "params": {
                    "name": binding_name,
                    "executionContextId": utility_context_id
                }
            }))
            .await;
            assert_eq!(take_response_by_id(&mut ctx, add_id)["result"], json!({}));

            ctx.process_async(json!({
                "id": eval_id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context_id,
                    "expression": source,
                    "awaitPromise": true
                }
            }))
            .await;
            let installed = take_response_by_id(&mut ctx, eval_id);
            assert_eq!(installed["result"]["result"]["value"], json!("function"));
        }
    }

    for (id, session_id, context_id, expected_state) in [
        (
            35671_u64,
            first_reauto_session.as_str(),
            first_post_cleanup_utility_context,
            json!("[\"undefined\",\"undefined\",\"undefined\",\"function\"]"),
        ),
        (
            35672_u64,
            second_reauto_session.as_str(),
            second_post_cleanup_utility_context,
            json!("[\"function\",\"function\",\"function\",\"function\"]"),
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": context_id,
                    "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.__pw_keptHandleBinding])"
                }
            })).await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    ctx.process_async(json!({
            "id": 35673,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_post_cleanup_utility_context,
                "expression": "globalThis.__lm_first_post_cleanup_pw_handle = __pw_keptHandleBinding(document.getElementById('utility-handle-b')).then(value => `resolved:${value}`, error => `rejected:${error}`); 'scheduled-first-post-cleanup-handle'",
                "awaitPromise": true
            }
        })).await;
    let scheduled_first_post_cleanup_handle = take_response_by_id(&mut ctx, 35673);
    assert!(
        scheduled_first_post_cleanup_handle["result"]["result"]["value"]
            .as_str()
            .expect("scheduled first post-cleanup handle")
            .starts_with("scheduled-")
    );
    let first_post_cleanup_handle_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(first_reauto_session)
                && message["params"]["name"] == json!("__pw_keptHandleBinding")
                && message["params"]["executionContextId"]
                    == json!(first_post_cleanup_utility_context)
        })
        .cloned()
        .expect("first post-cleanup retained handle bindingCalled");
    let first_post_cleanup_handle_payload: serde_json::Value = serde_json::from_str(
        first_post_cleanup_handle_called["params"]["payload"]
            .as_str()
            .expect("first post-cleanup handle payload string"),
    )
    .expect("first post-cleanup handle payload json");
    let first_post_cleanup_handle_seq = first_post_cleanup_handle_payload["seq"]
        .as_i64()
        .expect("first post-cleanup handle seq");
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35674,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_post_cleanup_utility_context,
                "expression": format!("(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {first_post_cleanup_handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {first_post_cleanup_handle_seq} }})]); }})()")
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35674)["result"]["result"]["value"],
        json!("[\"utility-handle-b\",\"first-reauto-post-cleanup-b\",\"undefined\"]")
    );

    ctx.process_async(json!({
            "id": 35675,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_post_cleanup_utility_context,
                "expression": format!("globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {first_post_cleanup_handle_seq}, result: 'first-post-cleanup-pw-handle-ok' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35675)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35676,
        "method": "Runtime.evaluate",
        "sessionId": first_reauto_session,
        "params": {
            "contextId": first_post_cleanup_utility_context,
            "expression": "globalThis.__lm_first_post_cleanup_pw_handle",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35676)["result"]["result"]["value"],
        json!("resolved:first-post-cleanup-pw-handle-ok")
    );

    ctx.process_async(json!({
            "id": 35677,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "expression": "globalThis.__lm_second_post_cleanup_custom_a = customBindingA({ source: 'second-post-cleanup-custom-a', nested: { count: 8, values: ['q', 9, false] } }); 'scheduled-second-post-cleanup-custom-a'"
            }
        })).await;
    let scheduled_second_post_cleanup_custom = take_response_by_id(&mut ctx, 35677);
    assert!(
        scheduled_second_post_cleanup_custom["result"]["result"]["value"]
            .as_str()
            .expect("scheduled second post-cleanup custom")
            .starts_with("scheduled-")
    );
    let second_post_cleanup_custom_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(second_reauto_session)
                && message["params"]["name"] == json!("customBindingA")
        })
        .cloned()
        .expect("second post-cleanup custom bindingCalled");
    let second_post_cleanup_custom_payload: serde_json::Value = serde_json::from_str(
        second_post_cleanup_custom_called["params"]["payload"]
            .as_str()
            .expect("second post-cleanup custom payload string"),
    )
    .expect("second post-cleanup custom payload json");
    let second_post_cleanup_custom_seq = second_post_cleanup_custom_payload["seq"]
        .as_i64()
        .expect("second post-cleanup custom seq");
    assert_eq!(
        second_post_cleanup_custom_payload["serializedArgs"],
        json!([{ "source": "second-post-cleanup-custom-a", "nested": { "count": 8, "values": ["q", 9, false] } }])
    );
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35678,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "expression": format!("globalThis.__lm_custom_binding_a_deliver({{ name: 'customBindingA', seq: {second_post_cleanup_custom_seq}, result: 'second-post-cleanup-custom-a-ok' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35678)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35679,
        "method": "Runtime.evaluate",
        "sessionId": second_reauto_session,
        "params": {
            "expression": "globalThis.__lm_second_post_cleanup_custom_a",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35679)["result"]["result"]["value"],
        json!("second-post-cleanup-custom-a-ok")
    );

    ctx.process_async(json!({
        "id": 35680,
        "method": "Runtime.removeBinding",
        "sessionId": second_reauto_session,
        "params": { "name": "customHandleBindingA" }
    }))
    .await;
    assert_eq!(take_response_by_id(&mut ctx, 35680)["result"], json!({}));

    for (id, session_id, expected_state) in [
        (
            35681_u64,
            first_reauto_session.as_str(),
            json!("[\"undefined\",\"undefined\",\"undefined\",\"function\"]"),
        ),
        (
            35682_u64,
            second_reauto_session.as_str(),
            json!("[\"function\",\"function\",\"undefined\",\"function\"]"),
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.__pw_keptHandleBinding])"
                }
            })).await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    for (id, session_id, context_id, expected_state) in [
        (
            35683_u64,
            first_reauto_session.as_str(),
            first_post_cleanup_utility_context,
            json!("[\"undefined\",\"undefined\",\"undefined\",\"function\"]"),
        ),
        (
            35684_u64,
            second_reauto_session.as_str(),
            second_post_cleanup_utility_context,
            json!("[\"function\",\"function\",\"undefined\",\"function\"]"),
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": context_id,
                    "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.__pw_keptHandleBinding])"
                }
            })).await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    for (id, session_id, html) in [
        (
            35685_u64,
            first_reauto_session.as_str(),
            "<body><div id='utility-handle-a'>first-reauto-final-a</div><div id='utility-handle-b'>first-reauto-final-b</div></body>",
        ),
        (
            35686_u64,
            second_reauto_session.as_str(),
            "<body><div id='utility-handle-a'>second-reauto-final-a</div><div id='utility-handle-b'>second-reauto-final-b</div></body>",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": { "url": format!("data:text/html,{html}") }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, session_id, source, expected_type) in [
        (
            35687_u64,
            first_reauto_session.as_str(),
            custom_wrapper_a_source.as_str(),
            "undefined",
        ),
        (
            35688_u64,
            first_reauto_session.as_str(),
            custom_wrapper_b_source.as_str(),
            "undefined",
        ),
        (
            35689_u64,
            first_reauto_session.as_str(),
            custom_handle_wrapper_a_source.as_str(),
            "undefined",
        ),
        (
            35690_u64,
            first_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
        (
            35691_u64,
            second_reauto_session.as_str(),
            custom_wrapper_a_source.as_str(),
            "function",
        ),
        (
            35692_u64,
            second_reauto_session.as_str(),
            custom_wrapper_b_source.as_str(),
            "function",
        ),
        (
            35693_u64,
            second_reauto_session.as_str(),
            custom_handle_wrapper_a_source.as_str(),
            "undefined",
        ),
        (
            35694_u64,
            second_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "expression": source,
                "awaitPromise": true
            }
        }))
        .await;
        let replayed = take_response_by_id(&mut ctx, id);
        assert_eq!(replayed["result"]["result"]["value"], json!(expected_type));
    }

    let mut first_final_utility_context = 0_i64;
    let mut second_final_utility_context = 0_i64;
    for (id, session_id, target_id, out_context) in [
        (
            35695_u64,
            first_reauto_session.as_str(),
            first_replacement.target_id.as_str(),
            &mut first_final_utility_context,
        ),
        (
            35696_u64,
            second_reauto_session.as_str(),
            second_replacement.target_id.as_str(),
            &mut second_final_utility_context,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.createIsolatedWorld",
            "sessionId": session_id,
            "params": {
                "frameId": target_id,
                "worldName": "utility"
            }
        }))
        .await;
        *out_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("final replay utility context id");
        ctx.take_all();
    }

    for (session_id, utility_context_id, add_base, eval_base, variant) in [
        (
            first_reauto_session.as_str(),
            first_final_utility_context,
            35697_u64,
            35707_u64,
            0_u8,
        ),
        (
            second_reauto_session.as_str(),
            second_final_utility_context,
            35717_u64,
            35727_u64,
            1_u8,
        ),
    ] {
        let bindings = match variant {
            0 => vec![(
                "__pw_keptHandleBinding",
                retained_handle_wrapper_source.as_str(),
            )],
            1 => vec![
                ("customBindingA", custom_wrapper_a_source.as_str()),
                ("customBindingB", custom_wrapper_b_source.as_str()),
                (
                    "__pw_keptHandleBinding",
                    retained_handle_wrapper_source.as_str(),
                ),
            ],
            _ => unreachable!("variant"),
        };

        for (offset, (binding_name, source)) in bindings.iter().enumerate() {
            let add_id = add_base + offset as u64;
            let eval_id = eval_base + offset as u64;
            ctx.process_async(json!({
                "id": add_id,
                "method": "Runtime.addBinding",
                "sessionId": session_id,
                "params": {
                    "name": binding_name,
                    "executionContextId": utility_context_id
                }
            }))
            .await;
            assert_eq!(take_response_by_id(&mut ctx, add_id)["result"], json!({}));

            ctx.process_async(json!({
                "id": eval_id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context_id,
                    "expression": source,
                    "awaitPromise": true
                }
            }))
            .await;
            let installed = take_response_by_id(&mut ctx, eval_id);
            assert_eq!(installed["result"]["result"]["value"], json!("function"));
        }
    }

    for (id, session_id, context_id, expected_state) in [
        (
            35737_u64,
            first_reauto_session.as_str(),
            first_final_utility_context,
            json!("[\"undefined\",\"undefined\",\"undefined\",\"function\"]"),
        ),
        (
            35738_u64,
            second_reauto_session.as_str(),
            second_final_utility_context,
            json!("[\"function\",\"function\",\"undefined\",\"function\"]"),
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": context_id,
                    "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.customBindingB, typeof globalThis.customHandleBindingA, typeof globalThis.__pw_keptHandleBinding])"
                }
            })).await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    ctx.process_async(json!({
            "id": 35739,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "expression": "globalThis.__lm_second_final_custom_b = customBindingB({ source: 'second-final-custom-b', nested: { count: 10, values: ['z', 11, true] } }); 'scheduled-second-final-custom-b'"
            }
        })).await;
    let scheduled_second_final_custom = take_response_by_id(&mut ctx, 35739);
    assert!(
        scheduled_second_final_custom["result"]["result"]["value"]
            .as_str()
            .expect("scheduled second final custom")
            .starts_with("scheduled-")
    );
    let second_final_custom_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(second_reauto_session)
                && message["params"]["name"] == json!("customBindingB")
        })
        .cloned()
        .expect("second final custom bindingCalled");
    let second_final_custom_payload: serde_json::Value = serde_json::from_str(
        second_final_custom_called["params"]["payload"]
            .as_str()
            .expect("second final custom payload string"),
    )
    .expect("second final custom payload json");
    let second_final_custom_seq = second_final_custom_payload["seq"]
        .as_i64()
        .expect("second final custom seq");
    assert_eq!(
        second_final_custom_payload["serializedArgs"],
        json!([{ "source": "second-final-custom-b", "nested": { "count": 10, "values": ["z", 11, true] } }])
    );
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35740,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "expression": format!("globalThis.__lm_custom_binding_b_deliver({{ name: 'customBindingB', seq: {second_final_custom_seq}, result: 'second-final-custom-b-ok' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35740)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35741,
        "method": "Runtime.evaluate",
        "sessionId": second_reauto_session,
        "params": {
            "expression": "globalThis.__lm_second_final_custom_b",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35741)["result"]["result"]["value"],
        json!("second-final-custom-b-ok")
    );

    ctx.process_async(json!({
            "id": 35742,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_final_utility_context,
                "expression": "globalThis.__lm_first_final_pw_handle_reject = __pw_keptHandleBinding(document.getElementById('utility-handle-a')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-first-final-pw-handle'",
                "awaitPromise": true
            }
        })).await;
    let scheduled_first_final_handle = take_response_by_id(&mut ctx, 35742);
    assert!(
        scheduled_first_final_handle["result"]["result"]["value"]
            .as_str()
            .expect("scheduled first final handle")
            .starts_with("scheduled-")
    );
    let first_final_handle_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(first_reauto_session)
                && message["params"]["name"] == json!("__pw_keptHandleBinding")
                && message["params"]["executionContextId"] == json!(first_final_utility_context)
        })
        .cloned()
        .expect("first final retained handle bindingCalled");
    let first_final_handle_payload: serde_json::Value = serde_json::from_str(
        first_final_handle_called["params"]["payload"]
            .as_str()
            .expect("first final handle payload string"),
    )
    .expect("first final handle payload json");
    let first_final_handle_seq = first_final_handle_payload["seq"]
        .as_i64()
        .expect("first final handle seq");
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35743,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_final_utility_context,
                "expression": format!("(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {first_final_handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {first_final_handle_seq} }})]); }})()")
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35743)["result"]["result"]["value"],
        json!("[\"utility-handle-a\",\"first-reauto-final-a\",\"undefined\"]")
    );

    ctx.process_async(json!({
            "id": 35744,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_final_utility_context,
                "expression": format!("globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {first_final_handle_seq}, error: 'first-final-pw-handle-error' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35744)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35745,
        "method": "Runtime.evaluate",
        "sessionId": first_reauto_session,
        "params": {
            "contextId": first_final_utility_context,
            "expression": "globalThis.__lm_first_final_pw_handle_reject",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35745)["result"]["result"]["value"],
        json!("rejected:first-final-pw-handle-error")
    );
    })
    .await;
}
#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_auto_attach_sweep_replacement_targets_keep_thin_crpage_cleanup_isolated_per_browser_context_without_runtime_enable()
 {
    patchright_replacement_targets_large_stack(|| async {
    let mut ctx = TestContext::new();
    let first =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 35750, 35751, 35752)
            .await;
    let second =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 35753, 35754, 35755)
            .await;

    for (id, session_id, html) in [
        (
            35756_u64,
            first.session_id.as_str(),
            "<body><div id='utility-handle-a'>thin-first-a</div></body>",
        ),
        (
            35757_u64,
            second.session_id.as_str(),
            "<body><div id='utility-handle-a'>thin-second-a</div></body>",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": { "url": format!("data:text/html,{html}") }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    let mut first_utility_context = 0_i64;
    let mut second_utility_context = 0_i64;
    for (id, session_id, target_id, out_context) in [
        (
            35758_u64,
            first.session_id.as_str(),
            first.target_id.as_str(),
            &mut first_utility_context,
        ),
        (
            35759_u64,
            second.session_id.as_str(),
            second.target_id.as_str(),
            &mut second_utility_context,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.createIsolatedWorld",
            "sessionId": session_id,
            "params": {
                "frameId": target_id,
                "worldName": "utility"
            }
        }))
        .await;
        *out_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("thin initial utility context id");
        ctx.take_all();
    }

    let custom_wrapper_source = patchright_page_binding_wrapper_source(
        "customBindingA",
        "__lm_custom_binding_a_deliver",
        None,
        false,
    );
    let retained_handle_wrapper_source = patchright_page_binding_wrapper_source(
        "__pw_keptHandleBinding",
        "__lm_pw_kept_handle_binding_deliver",
        Some("__lm_pw_kept_handle_binding_take"),
        true,
    );

    for (session_id, utility_context_id, id_base) in [
        (first.session_id.as_str(), first_utility_context, 35760_u64),
        (
            second.session_id.as_str(),
            second_utility_context,
            35770_u64,
        ),
    ] {
        install_patchright_crpage_binding_in_existing_worlds_async(
            &mut ctx,
            session_id,
            utility_context_id,
            id_base,
            id_base + 1,
            id_base + 2,
            id_base + 3,
            "customBindingA",
            &custom_wrapper_source,
        )
        .await;
        install_patchright_crpage_binding_in_existing_worlds_async(
            &mut ctx,
            session_id,
            utility_context_id,
            id_base + 4,
            id_base + 5,
            id_base + 6,
            id_base + 7,
            "__pw_keptHandleBinding",
            &retained_handle_wrapper_source,
        )
        .await;
    }

    for (session_id, id_base) in [
        (first.session_id.as_str(), 35780_u64),
        (second.session_id.as_str(), 35790_u64),
    ] {
        for (source, world_name, offset) in [
            (custom_wrapper_source.as_str(), None, 0_u64),
            (retained_handle_wrapper_source.as_str(), None, 1_u64),
            (custom_wrapper_source.as_str(), Some("utility"), 2_u64),
            (
                retained_handle_wrapper_source.as_str(),
                Some("utility"),
                3_u64,
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
                "id": id_base + offset,
                "method": "Page.addScriptToEvaluateOnNewDocument",
                "sessionId": session_id,
                "params": params
            }))
            .await;
            assert!(
                take_response_by_id(&mut ctx, id_base + offset)["result"]["identifier"]
                    .as_str()
                    .is_some()
            );
        }
    }

    ctx.process_async(json!({
        "id": 35800,
        "method": "Runtime.removeBinding",
        "sessionId": first.session_id,
        "params": { "name": "customBindingA" }
    }))
    .await;
    assert_eq!(take_response_by_id(&mut ctx, 35800)["result"], json!({}));

    for (id, target_id) in [
        (35801_u64, first.target_id.as_str()),
        (35802_u64, second.target_id.as_str()),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Target.closeTarget",
            "params": { "targetId": target_id }
        }))
        .await;
        ctx.expect_result(id, json!({ "success": true }), None);
        ctx.take_all();
    }

    let first_replacement = attach_page_session_without_runtime_enable_in_existing_context_async(
        &mut ctx,
        &first.browser_context_id,
        35803,
        35804,
    )
    .await;
    let second_replacement = attach_page_session_without_runtime_enable_in_existing_context_async(
        &mut ctx,
        &second.browser_context_id,
        35805,
        35806,
    )
    .await;

    for (id, session_id, html) in [
        (
            35807_u64,
            first_replacement.session_id.as_str(),
            "<body><div id='utility-handle-a'>thin-first-replacement-a</div></body>",
        ),
        (
            35808_u64,
            second_replacement.session_id.as_str(),
            "<body><div id='utility-handle-a'>thin-second-replacement-a</div></body>",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": { "url": format!("data:text/html,{html}") }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, target_id, session_id) in [
        (
            35809_u64,
            first_replacement.target_id.as_str(),
            first_replacement.session_id.as_str(),
        ),
        (
            35810_u64,
            second_replacement.target_id.as_str(),
            second_replacement.session_id.as_str(),
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Target.detachFromTarget",
            "params": {
                "targetId": target_id,
                "sessionId": session_id
            }
        }))
        .await;
        ctx.expect_result(id, json!({}), None);
        ctx.expect_event(
            "Target.detachedFromTarget",
            Some(&json!({
                "targetId": target_id,
                "sessionId": session_id,
            })),
        );
    }

    ctx.process_async(json!({
        "id": 35811,
        "method": "Target.setAutoAttach",
        "params": {
            "autoAttach": true,
            "waitForDebuggerOnStart": false
        }
    }))
    .await;
    ctx.expect_result(35811, json!({}), None);
    let events = ctx.take_all();
    let attached_events = events
        .iter()
        .filter(|event| event["method"] == json!("Target.attachedToTarget"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(attached_events.len(), 2);
    let first_reauto_session = attached_events
        .iter()
        .find(|event| {
            event["params"]["targetInfo"]["targetId"] == json!(first_replacement.target_id)
        })
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("thin first replacement re-auto-attached session")
        .to_owned();
    let second_reauto_session = attached_events
        .iter()
        .find(|event| {
            event["params"]["targetInfo"]["targetId"] == json!(second_replacement.target_id)
        })
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("thin second replacement re-auto-attached session")
        .to_owned();

    for (id, session_id, source, expected_type) in [
        (
            35812_u64,
            first_reauto_session.as_str(),
            custom_wrapper_source.as_str(),
            "undefined",
        ),
        (
            35813_u64,
            first_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
        (
            35814_u64,
            second_reauto_session.as_str(),
            custom_wrapper_source.as_str(),
            "function",
        ),
        (
            35815_u64,
            second_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "expression": source,
                "awaitPromise": true
            }
        }))
        .await;
        let installed = take_response_by_id(&mut ctx, id);
        assert_eq!(installed["result"]["result"]["value"], json!(expected_type));
    }

    let mut first_reauto_utility_context = 0_i64;
    let mut second_reauto_utility_context = 0_i64;
    for (id, session_id, target_id, out_context) in [
        (
            35816_u64,
            first_reauto_session.as_str(),
            first_replacement.target_id.as_str(),
            &mut first_reauto_utility_context,
        ),
        (
            35817_u64,
            second_reauto_session.as_str(),
            second_replacement.target_id.as_str(),
            &mut second_reauto_utility_context,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.createIsolatedWorld",
            "sessionId": session_id,
            "params": {
                "frameId": target_id,
                "worldName": "utility"
            }
        }))
        .await;
        *out_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("thin re-auto utility context id");
        ctx.take_all();
    }

    install_patchright_crpage_binding_in_existing_worlds_async(
        &mut ctx,
        first_reauto_session.as_str(),
        first_reauto_utility_context,
        35818,
        35819,
        35820,
        35821,
        "__pw_keptHandleBinding",
        &retained_handle_wrapper_source,
    )
    .await;
    install_patchright_crpage_binding_in_existing_worlds_async(
        &mut ctx,
        second_reauto_session.as_str(),
        second_reauto_utility_context,
        35822,
        35823,
        35824,
        35825,
        "customBindingA",
        &custom_wrapper_source,
    )
    .await;
    install_patchright_crpage_binding_in_existing_worlds_async(
        &mut ctx,
        second_reauto_session.as_str(),
        second_reauto_utility_context,
        35826,
        35827,
        35828,
        35829,
        "__pw_keptHandleBinding",
        &retained_handle_wrapper_source,
    )
    .await;

    for (id, session_id, context_id, expected_state) in [
        (
            35830_u64,
            first_reauto_session.as_str(),
            first_reauto_utility_context,
            json!("[\"undefined\",\"function\"]"),
        ),
        (
            35831_u64,
            second_reauto_session.as_str(),
            second_reauto_utility_context,
            json!("[\"function\",\"function\"]"),
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": context_id,
                    "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.__pw_keptHandleBinding])"
                }
            })).await;
        let state = take_response_by_id(&mut ctx, id);
        assert_eq!(state["result"]["result"]["value"], expected_state);
    }

    for (id, session_id, html) in [
        (
            35832_u64,
            first_reauto_session.as_str(),
            "<body><div id='utility-handle-a'>thin-first-replay-a</div></body>",
        ),
        (
            35833_u64,
            second_reauto_session.as_str(),
            "<body><div id='utility-handle-a'>thin-second-replay-a</div></body>",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": { "url": format!("data:text/html,{html}") }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, session_id, source, expected_type) in [
        (
            35834_u64,
            first_reauto_session.as_str(),
            custom_wrapper_source.as_str(),
            "undefined",
        ),
        (
            35835_u64,
            first_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
        (
            35836_u64,
            second_reauto_session.as_str(),
            custom_wrapper_source.as_str(),
            "function",
        ),
        (
            35837_u64,
            second_reauto_session.as_str(),
            retained_handle_wrapper_source.as_str(),
            "function",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "expression": source,
                "awaitPromise": true
            }
        }))
        .await;
        let replayed = take_response_by_id(&mut ctx, id);
        assert_eq!(replayed["result"]["result"]["value"], json!(expected_type));
    }

    let mut first_replay_utility_context = 0_i64;
    let mut second_replay_utility_context = 0_i64;
    for (id, session_id, target_id, out_context) in [
        (
            35838_u64,
            first_reauto_session.as_str(),
            first_replacement.target_id.as_str(),
            &mut first_replay_utility_context,
        ),
        (
            35839_u64,
            second_reauto_session.as_str(),
            second_replacement.target_id.as_str(),
            &mut second_replay_utility_context,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.createIsolatedWorld",
            "sessionId": session_id,
            "params": {
                "frameId": target_id,
                "worldName": "utility"
            }
        }))
        .await;
        *out_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("thin replay utility context id");
        ctx.take_all();
    }

    install_patchright_crpage_binding_in_existing_worlds_async(
        &mut ctx,
        first_reauto_session.as_str(),
        first_replay_utility_context,
        35840,
        35841,
        35842,
        35843,
        "__pw_keptHandleBinding",
        &retained_handle_wrapper_source,
    )
    .await;
    install_patchright_crpage_binding_in_existing_worlds_async(
        &mut ctx,
        second_reauto_session.as_str(),
        second_replay_utility_context,
        35844,
        35845,
        35846,
        35847,
        "customBindingA",
        &custom_wrapper_source,
    )
    .await;
    install_patchright_crpage_binding_in_existing_worlds_async(
        &mut ctx,
        second_reauto_session.as_str(),
        second_replay_utility_context,
        35848,
        35849,
        35850,
        35851,
        "__pw_keptHandleBinding",
        &retained_handle_wrapper_source,
    )
    .await;

    ctx.process_async(json!({
            "id": 35852,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "expression": "globalThis.__lm_thin_second_custom_a = customBindingA({ source: 'thin-second-custom-a', nested: { count: 12, values: ['thin', 13, true] } }); 'scheduled-thin-second-custom-a'"
            }
        })).await;
    let scheduled_second_custom = take_response_by_id(&mut ctx, 35852);
    assert!(
        scheduled_second_custom["result"]["result"]["value"]
            .as_str()
            .expect("scheduled thin second custom")
            .starts_with("scheduled-")
    );
    let second_custom_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(second_reauto_session)
                && message["params"]["name"] == json!("customBindingA")
        })
        .cloned()
        .expect("thin second custom bindingCalled");
    let second_custom_payload: serde_json::Value = serde_json::from_str(
        second_custom_called["params"]["payload"]
            .as_str()
            .expect("thin second custom payload string"),
    )
    .expect("thin second custom payload json");
    let second_custom_seq = second_custom_payload["seq"]
        .as_i64()
        .expect("thin second custom seq");
    assert_eq!(
        second_custom_payload["serializedArgs"],
        json!([{ "source": "thin-second-custom-a", "nested": { "count": 12, "values": ["thin", 13, true] } }])
    );
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35853,
            "method": "Runtime.evaluate",
            "sessionId": second_reauto_session,
            "params": {
                "expression": format!("globalThis.__lm_custom_binding_a_deliver({{ name: 'customBindingA', seq: {second_custom_seq}, result: 'thin-second-custom-a-ok' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35853)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35854,
        "method": "Runtime.evaluate",
        "sessionId": second_reauto_session,
        "params": {
            "expression": "globalThis.__lm_thin_second_custom_a",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35854)["result"]["result"]["value"],
        json!("thin-second-custom-a-ok")
    );

    ctx.process_async(json!({
            "id": 35855,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_replay_utility_context,
                "expression": "globalThis.__lm_thin_first_pw_handle = __pw_keptHandleBinding(document.getElementById('utility-handle-a')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-thin-first-pw-handle'",
                "awaitPromise": true
            }
        })).await;
    let scheduled_first_handle = take_response_by_id(&mut ctx, 35855);
    assert!(
        scheduled_first_handle["result"]["result"]["value"]
            .as_str()
            .expect("scheduled thin first handle")
            .starts_with("scheduled-")
    );
    let first_handle_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(first_reauto_session)
                && message["params"]["name"] == json!("__pw_keptHandleBinding")
                && message["params"]["executionContextId"] == json!(first_replay_utility_context)
        })
        .cloned()
        .expect("thin first retained handle bindingCalled");
    let first_handle_payload: serde_json::Value = serde_json::from_str(
        first_handle_called["params"]["payload"]
            .as_str()
            .expect("thin first handle payload string"),
    )
    .expect("thin first handle payload json");
    let first_handle_seq = first_handle_payload["seq"]
        .as_i64()
        .expect("thin first handle seq");
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35856,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_replay_utility_context,
                "expression": format!("(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {first_handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {first_handle_seq} }})]); }})()")
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35856)["result"]["result"]["value"],
        json!("[\"utility-handle-a\",\"thin-first-replay-a\",\"undefined\"]")
    );

    ctx.process_async(json!({
            "id": 35857,
            "method": "Runtime.evaluate",
            "sessionId": first_reauto_session,
            "params": {
                "contextId": first_replay_utility_context,
                "expression": format!("globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {first_handle_seq}, error: 'thin-first-pw-handle-error' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35857)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35858,
        "method": "Runtime.evaluate",
        "sessionId": first_reauto_session,
        "params": {
            "contextId": first_replay_utility_context,
            "expression": "globalThis.__lm_thin_first_pw_handle",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35858)["result"]["result"]["value"],
        json!("rejected:thin-first-pw-handle-error")
    );

    for (id, session_id, target_id) in [
        (
            35859_u64,
            first_reauto_session.as_str(),
            first_replacement.target_id.as_str(),
        ),
        (
            35860_u64,
            second_reauto_session.as_str(),
            second_replacement.target_id.as_str(),
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Target.detachFromTarget",
            "params": {
                "targetId": target_id,
                "sessionId": session_id,
            }
        }))
        .await;
        ctx.expect_result(id, json!({}), None);
        ctx.expect_event(
            "Target.detachedFromTarget",
            Some(&json!({
                "targetId": target_id,
                "sessionId": session_id,
            })),
        );
    }

    ctx.process_async(json!({
        "id": 35861,
        "method": "Target.attachToTarget",
        "params": {
            "targetId": first_replacement.target_id,
            "flatten": true
        }
    }))
    .await;
    let first_reattached_session = take_response_by_id(&mut ctx, 35861)["result"]["sessionId"]
        .as_str()
        .expect("thin first reattached session id")
        .to_owned();
    assert_ne!(first_reattached_session, first_reauto_session);
    ctx.expect_event(
        "Target.attachedToTarget",
        Some(&json!({
            "sessionId": first_reattached_session,
            "targetInfo": {
                "targetId": first_replacement.target_id,
                "browserContextId": first.browser_context_id,
            }
        })),
    );
    ctx.take_all();

    ctx.process_async(json!({
        "id": 35862,
        "method": "Target.attachToTarget",
        "params": {
            "targetId": second_replacement.target_id,
            "flatten": true
        }
    }))
    .await;
    let second_reattached_session = take_response_by_id(&mut ctx, 35862)["result"]["sessionId"]
        .as_str()
        .expect("thin second reattached session id")
        .to_owned();
    assert_ne!(second_reattached_session, second_reauto_session);
    ctx.expect_event(
        "Target.attachedToTarget",
        Some(&json!({
            "sessionId": second_reattached_session,
            "targetInfo": {
                "targetId": second_replacement.target_id,
                "browserContextId": second.browser_context_id,
            }
        })),
    );
    ctx.take_all();

    for (id, session_id, context_id, expected_state) in [
        (
            35863_u64,
            first_reattached_session.as_str(),
            None::<i64>,
            json!("[\"undefined\",\"function\"]"),
        ),
        (
            35864_u64,
            first_reattached_session.as_str(),
            Some(first_replay_utility_context),
            json!("[\"undefined\",\"function\"]"),
        ),
        (
            35865_u64,
            second_reattached_session.as_str(),
            None::<i64>,
            json!("[\"function\",\"function\"]"),
        ),
        (
            35866_u64,
            second_reattached_session.as_str(),
            Some(second_replay_utility_context),
            json!("[\"function\",\"function\"]"),
        ),
    ] {
        let mut params = json!({
            "expression": "JSON.stringify([typeof globalThis.customBindingA, typeof globalThis.__pw_keptHandleBinding])"
        });
        if let Some(context_id) = context_id {
            params["contextId"] = json!(context_id);
        }
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": params
        }))
        .await;
        assert_eq!(
            take_response_by_id(&mut ctx, id)["result"]["result"]["value"],
            expected_state
        );
    }

    ctx.process_async(json!({
            "id": 35867,
            "method": "Runtime.evaluate",
            "sessionId": second_reattached_session,
            "params": {
                "expression": "globalThis.__lm_thin_reattached_second_custom_a = customBindingA({ source: 'thin-reattached-second-custom-a', nested: { count: 14, values: ['reattach', 15, true] } }); 'scheduled-thin-reattached-second-custom-a'",
                "awaitPromise": true
            }
        })).await;
    let scheduled_reattached_custom = take_response_by_id(&mut ctx, 35867);
    assert!(
        scheduled_reattached_custom["result"]["result"]["value"]
            .as_str()
            .expect("scheduled thin reattached second custom")
            .starts_with("scheduled-")
    );
    let reattached_custom_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(second_reattached_session)
                && message["params"]["name"] == json!("customBindingA")
        })
        .cloned()
        .expect("thin reattached second custom bindingCalled");
    let reattached_custom_payload: serde_json::Value = serde_json::from_str(
        reattached_custom_called["params"]["payload"]
            .as_str()
            .expect("thin reattached second custom payload string"),
    )
    .expect("thin reattached second custom payload json");
    let reattached_custom_seq = reattached_custom_payload["seq"]
        .as_i64()
        .expect("thin reattached second custom seq");
    assert_eq!(reattached_custom_seq, 2);
    assert_eq!(
        reattached_custom_payload["serializedArgs"],
        json!([{ "source": "thin-reattached-second-custom-a", "nested": { "count": 14, "values": ["reattach", 15, true] } }])
    );
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35868,
            "method": "Runtime.evaluate",
            "sessionId": second_reattached_session,
            "params": {
                "expression": "globalThis.__lm_custom_binding_a_deliver({ name: 'customBindingA', seq: 2, result: 'thin-reattached-second-custom-a-ok' }); 'delivered'",
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35868)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35869,
        "method": "Runtime.evaluate",
        "sessionId": second_reattached_session,
        "params": {
            "expression": "globalThis.__lm_thin_reattached_second_custom_a",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35869)["result"]["result"]["value"],
        json!("thin-reattached-second-custom-a-ok")
    );

    ctx.process_async(json!({
            "id": 35870,
            "method": "Runtime.evaluate",
            "sessionId": first_reattached_session,
            "params": {
                "contextId": first_replay_utility_context,
                "expression": "globalThis.__lm_thin_reattached_first_pw_handle = __pw_keptHandleBinding(document.getElementById('utility-handle-a')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled-thin-reattached-first-pw-handle'",
                "awaitPromise": true
            }
        })).await;
    let scheduled_reattached_handle = take_response_by_id(&mut ctx, 35870);
    assert!(
        scheduled_reattached_handle["result"]["result"]["value"]
            .as_str()
            .expect("scheduled thin reattached first handle")
            .starts_with("scheduled-")
    );
    let reattached_handle_called = ctx
        .sent
        .iter()
        .rev()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["sessionId"] == json!(first_reattached_session)
                && message["params"]["name"] == json!("__pw_keptHandleBinding")
                && message["params"]["executionContextId"] == json!(first_replay_utility_context)
        })
        .cloned()
        .expect("thin reattached first retained handle bindingCalled");
    let reattached_handle_payload: serde_json::Value = serde_json::from_str(
        reattached_handle_called["params"]["payload"]
            .as_str()
            .expect("thin reattached first handle payload string"),
    )
    .expect("thin reattached first handle payload json");
    let reattached_handle_seq = reattached_handle_payload["seq"]
        .as_i64()
        .expect("thin reattached first handle seq");
    assert_eq!(reattached_handle_seq, 2);
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 35871,
            "method": "Runtime.evaluate",
            "sessionId": first_reattached_session,
            "params": {
                "contextId": first_replay_utility_context,
                "expression": format!("(() => {{ const handle = globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {reattached_handle_seq} }}); return JSON.stringify([handle.id, handle.textContent, typeof globalThis.__lm_pw_kept_handle_binding_take({{ name: '__pw_keptHandleBinding', seq: {reattached_handle_seq} }})]); }})()")
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35871)["result"]["result"]["value"],
        json!("[\"utility-handle-a\",\"thin-first-replay-a\",\"undefined\"]")
    );

    ctx.process_async(json!({
            "id": 35872,
            "method": "Runtime.evaluate",
            "sessionId": first_reattached_session,
            "params": {
                "contextId": first_replay_utility_context,
                "expression": format!("globalThis.__lm_pw_kept_handle_binding_deliver({{ name: '__pw_keptHandleBinding', seq: {reattached_handle_seq}, error: 'thin-reattached-first-pw-handle-error' }}); 'delivered'"),
                "awaitPromise": true
            }
        })).await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35872)["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 35873,
        "method": "Runtime.evaluate",
        "sessionId": first_reattached_session,
        "params": {
            "contextId": first_replay_utility_context,
            "expression": "globalThis.__lm_thin_reattached_first_pw_handle",
            "awaitPromise": true
        }
    }))
    .await;
    assert_eq!(
        take_response_by_id(&mut ctx, 35873)["result"]["result"]["value"],
        json!("rejected:thin-reattached-first-pw-handle-error")
    );
    })
    .await;
}
