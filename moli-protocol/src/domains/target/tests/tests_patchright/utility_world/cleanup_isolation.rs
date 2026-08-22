use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_auto_attach_sweep_rehydrates_existing_targets_without_runtime_enable_and_keeps_context_isolation()
 {
    let mut ctx = TestContext::new();
    let first =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2262, 2263, 2264).await;
    let second =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2265, 2266, 2267).await;

    for (id, session_id, source, binding_name, label) in [
        (
            2268_u64,
            first.session_id.as_str(),
            "globalThis.__lm_auto_attach_sweep_marker = 'first';",
            "autoAttachSweepBindingFirst",
            "first-page",
        ),
        (
            2271_u64,
            second.session_id.as_str(),
            "globalThis.__lm_auto_attach_sweep_marker = 'second';",
            "autoAttachSweepBindingSecond",
            "second-page",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.addScriptToEvaluateOnNewDocument",
            "sessionId": session_id,
            "params": {
                "source": source,
                "worldName": "utility"
            }
        }))
        .await;
        let preload = take_response_by_id(&mut ctx, id);
        assert_eq!(preload["sessionId"], json!(session_id));
        assert!(preload["result"]["identifier"].is_string());

        ctx.process_async(json!({
            "id": id + 1,
            "method": "Runtime.addBinding",
            "sessionId": session_id,
            "params": {
                "name": binding_name,
                "executionContextName": "utility"
            }
        }))
        .await;
        let add_binding = take_response_by_id(&mut ctx, id + 1);
        assert_eq!(add_binding["result"], json!({}));

        ctx.process_async(json!({
            "id": id + 2,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": {
                "url": format!("data:text/html,<body><div id=page>{label}</div></body>")
            }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id + 2);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, target_id, session_id) in [
        (
            2274_u64,
            first.target_id.as_str(),
            first.session_id.as_str(),
        ),
        (
            2275_u64,
            second.target_id.as_str(),
            second.session_id.as_str(),
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

    assert_eq!(
        ctx.conn
            .browser_contexts()
            .filter(|bc| !bc.has_active_session())
            .count(),
        2,
        "both targets should be sessionless before auto-attach sweep"
    );

    ctx.process_async(json!({
        "id": 2276,
        "method": "Target.setAutoAttach",
        "params": {
            "autoAttach": true,
            "waitForDebuggerOnStart": false
        }
    }))
    .await;
    ctx.expect_result(2276, json!({}), None);
    let events = ctx.take_all();
    let attached_events = events
        .iter()
        .filter(|event| event["method"] == json!("Target.attachedToTarget"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        attached_events.len(),
        2,
        "auto-attach sweep should attach both targets"
    );
    let first_attached_event = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(first.target_id))
        .expect("first target auto-attach event");
    let second_attached_event = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(second.target_id))
        .expect("second target auto-attach event");
    let first_auto_session = first_attached_event["params"]["sessionId"]
        .as_str()
        .expect("first auto-attached session id")
        .to_owned();
    let second_auto_session = second_attached_event["params"]["sessionId"]
        .as_str()
        .expect("second auto-attached session id")
        .to_owned();
    assert_ne!(first_auto_session, first.session_id);
    assert_ne!(second_auto_session, second.session_id);
    assert_ne!(first_auto_session, second_auto_session);

    for (id, session_id, target_id, binding_name, expected_marker, expected_text) in [
        (
            2277_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            "autoAttachSweepBindingFirst",
            "first",
            "first-page",
        ),
        (
            2279_u64,
            second_auto_session.as_str(),
            second.target_id.as_str(),
            "autoAttachSweepBindingSecond",
            "second",
            "second-page",
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
        let utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context id after auto-attach sweep");
        ctx.take_all();

        ctx.process_async(json!({
                "id": id + 1,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context,
                    "expression": format!("{binding_name}('payload-{expected_marker}'); JSON.stringify([typeof globalThis.{binding_name}, globalThis.__lm_auto_attach_sweep_marker, document.querySelector('#page').textContent])")
                }
            })).await;
        let evaluation = take_response_by_id(&mut ctx, id + 1);
        assert_eq!(
            evaluation["result"]["result"]["value"],
            json!(format!(
                "[\"function\",\"{expected_marker}\",\"{expected_text}\"]"
            ))
        );
        let binding_called = ctx
            .sent
            .iter()
            .find(|message| {
                message["method"] == json!("Runtime.bindingCalled")
                    && message["params"]["name"] == json!(binding_name)
            })
            .cloned()
            .expect("auto-attached utility world should emit bindingCalled");
        assert_eq!(
            binding_called["params"]["payload"],
            json!(format!("payload-{expected_marker}"))
        );
        assert_eq!(
            binding_called["params"]["executionContextId"],
            json!(utility_context)
        );
        ctx.sent.clear();
    }

    let first_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == first.browser_context_id)
        .expect("first browser context should exist");
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
                binding.name == "autoAttachSweepBindingFirst"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "first browser context should retain its utility-world binding definition"
    );

    let second_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == second.browser_context_id)
        .expect("second browser context should exist");
    assert_eq!(
        second_context
            .active_target
            .owner_state
            .document_start_scripts
            .len(),
        1
    );
    assert!(
        second_context
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
                binding.name == "autoAttachSweepBindingSecond"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "second browser context should retain its utility-world binding definition"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_auto_attach_sweep_cleanup_stays_isolated_per_browser_context() {
    let mut ctx = TestContext::new();
    let first =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2281, 2282, 2283).await;
    let second =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2284, 2285, 2286).await;

    let mut first_preload_identifier = String::new();
    let mut second_preload_identifier = String::new();
    for (id, session_id, source, binding_name, label, preload_identifier) in [
        (
            2287_u64,
            first.session_id.as_str(),
            "globalThis.__lm_auto_attach_cleanup_marker = 'first';",
            "autoAttachCleanupBindingFirst",
            "first-page",
            &mut first_preload_identifier,
        ),
        (
            2290_u64,
            second.session_id.as_str(),
            "globalThis.__lm_auto_attach_cleanup_marker = 'second';",
            "autoAttachCleanupBindingSecond",
            "second-page",
            &mut second_preload_identifier,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.addScriptToEvaluateOnNewDocument",
            "sessionId": session_id,
            "params": {
                "source": source,
                "worldName": "utility"
            }
        }))
        .await;
        let preload = take_response_by_id(&mut ctx, id);
        assert_eq!(preload["sessionId"], json!(session_id));
        *preload_identifier = preload["result"]["identifier"]
            .as_str()
            .expect("preload identifier")
            .to_owned();

        ctx.process_async(json!({
            "id": id + 1,
            "method": "Runtime.addBinding",
            "sessionId": session_id,
            "params": {
                "name": binding_name,
                "executionContextName": "utility"
            }
        }))
        .await;
        let add_binding = take_response_by_id(&mut ctx, id + 1);
        assert_eq!(add_binding["result"], json!({}));

        ctx.process_async(json!({
            "id": id + 2,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": {
                "url": format!("data:text/html,<body><div id=page>{label}</div></body>")
            }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id + 2);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, target_id, session_id) in [
        (
            2293_u64,
            first.target_id.as_str(),
            first.session_id.as_str(),
        ),
        (
            2294_u64,
            second.target_id.as_str(),
            second.session_id.as_str(),
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
        "id": 2295,
        "method": "Target.setAutoAttach",
        "params": {
            "autoAttach": true,
            "waitForDebuggerOnStart": false
        }
    }))
    .await;
    ctx.expect_result(2295, json!({}), None);
    let events = ctx.take_all();
    let attached_events = events
        .iter()
        .filter(|event| event["method"] == json!("Target.attachedToTarget"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        attached_events.len(),
        2,
        "auto-attach sweep should attach both targets"
    );
    let first_auto_session = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(first.target_id))
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("first auto-attached session id")
        .to_owned();
    let second_auto_session = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(second.target_id))
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("second auto-attached session id")
        .to_owned();

    let mut first_utility_context = 0_i64;
    let mut second_utility_context = 0_i64;
    for (id, session_id, target_id, utility_context) in [
        (
            2296_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            &mut first_utility_context,
        ),
        (
            2297_u64,
            second_auto_session.as_str(),
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
        *utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context after auto-attach sweep");
        ctx.take_all();
    }

    ctx.process_async(json!({
        "id": 2298,
        "method": "Runtime.removeBinding",
        "sessionId": first_auto_session,
        "params": {
            "name": "autoAttachCleanupBindingFirst"
        }
    }))
    .await;
    let remove_binding = take_response_by_id(&mut ctx, 2298);
    assert_eq!(remove_binding["result"], json!({}));

    ctx.process_async(json!({
        "id": 2299,
        "method": "Page.removeScriptToEvaluateOnNewDocument",
        "sessionId": first_auto_session,
        "params": {
            "identifier": first_preload_identifier
        }
    }))
    .await;
    let remove_preload = take_response_by_id(&mut ctx, 2299);
    assert_eq!(remove_preload["result"], json!({}));

    for (id, session_id, utility_context, binding_name, expected_snapshot) in [
        (
            2300_u64,
            first_auto_session.as_str(),
            first_utility_context,
            "autoAttachCleanupBindingFirst",
            "[\"undefined\",\"first\",\"first-page\"]",
        ),
        (
            2301_u64,
            second_auto_session.as_str(),
            second_utility_context,
            "autoAttachCleanupBindingSecond",
            "[\"function\",\"second\",\"second-page\"]",
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context,
                    "expression": format!("JSON.stringify([typeof globalThis.{binding_name}, globalThis.__lm_auto_attach_cleanup_marker, document.querySelector('#page').textContent])")
                }
            })).await;
        let evaluation = take_response_by_id(&mut ctx, id);
        assert_eq!(
            evaluation["result"]["result"]["value"],
            json!(expected_snapshot)
        );
    }

    for (id, session_id, label) in [
        (2302_u64, first_auto_session.as_str(), "first-page-replay"),
        (2303_u64, second_auto_session.as_str(), "second-page-replay"),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": {
                "url": format!("data:text/html,<body><div id=page>{label}</div></body>")
            }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    let mut first_replay_utility_context = 0_i64;
    let mut second_replay_utility_context = 0_i64;
    for (id, session_id, target_id, utility_context) in [
        (
            2304_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            &mut first_replay_utility_context,
        ),
        (
            2305_u64,
            second_auto_session.as_str(),
            second.target_id.as_str(),
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
        *utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context after replay navigation");
        ctx.take_all();
    }

    for (id, session_id, utility_context, binding_name, expected_snapshot) in [
        (
            2306_u64,
            first_auto_session.as_str(),
            first_replay_utility_context,
            "autoAttachCleanupBindingFirst",
            "[\"undefined\",null,\"first-page-replay\"]",
        ),
        (
            2307_u64,
            second_auto_session.as_str(),
            second_replay_utility_context,
            "autoAttachCleanupBindingSecond",
            "[\"function\",\"second\",\"second-page-replay\"]",
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context,
                    "expression": format!("JSON.stringify([typeof globalThis.{binding_name}, globalThis.__lm_auto_attach_cleanup_marker, document.querySelector('#page').textContent])")
                }
            })).await;
        let evaluation = take_response_by_id(&mut ctx, id);
        assert_eq!(
            evaluation["result"]["result"]["value"],
            json!(expected_snapshot)
        );
    }

    let first_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == first.browser_context_id)
        .expect("first browser context should still exist");
    assert!(
        !first_context
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
                binding.name == "autoAttachCleanupBindingFirst"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "first browser context binding definition should be removed after cleanup"
    );
    assert!(
        !first_context
            .active_target
            .owner_state
            .document_start_scripts
            .iter()
            .any(|script| script.0 == first_preload_identifier),
        "first browser context preload definition should be removed after cleanup"
    );

    let second_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == second.browser_context_id)
        .expect("second browser context should still exist");
    assert!(
        second_context
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
                binding.name == "autoAttachCleanupBindingSecond"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "second browser context binding definition should remain intact"
    );
    assert!(
        second_context
            .active_target
            .owner_state
            .document_start_scripts
            .iter()
            .any(|script| script.0 == second_preload_identifier),
        "second browser context preload definition should remain intact"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_auto_attach_sweep_page_binding_cleanup_with_same_name_stays_isolated_per_browser_context()
 {
    super::super::patchright_8mb_stack(
        "patchright-utility-page-binding-cleanup-same-name",
        || async {
    let mut ctx = TestContext::new();
    let first =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2308, 2309, 2310).await;
    let second =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2311, 2312, 2313).await;

    for (id, session_id, label) in [
        (2314_u64, first.session_id.as_str(), "first-page"),
        (2316_u64, second.session_id.as_str(), "second-page"),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.addBinding",
            "sessionId": session_id,
            "params": {
                "name": "sharedPatchedBinding",
                "executionContextName": "utility"
            }
        }))
        .await;
        let add_binding = take_response_by_id(&mut ctx, id);
        assert_eq!(add_binding["result"], json!({}));

        ctx.process_async(json!({
            "id": id + 1,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": {
                "url": format!("data:text/html,<body><div id=page>{label}</div></body>")
            }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id + 1);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, target_id, session_id) in [
        (
            2318_u64,
            first.target_id.as_str(),
            first.session_id.as_str(),
        ),
        (
            2319_u64,
            second.target_id.as_str(),
            second.session_id.as_str(),
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
        "id": 2320,
        "method": "Target.setAutoAttach",
        "params": {
            "autoAttach": true,
            "waitForDebuggerOnStart": false
        }
    }))
    .await;
    ctx.expect_result(2320, json!({}), None);
    let events = ctx.take_all();
    let attached_events = events
        .iter()
        .filter(|event| event["method"] == json!("Target.attachedToTarget"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        attached_events.len(),
        2,
        "auto-attach sweep should attach both targets"
    );
    let first_auto_session = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(first.target_id))
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("first auto-attached session id")
        .to_owned();
    let second_auto_session = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(second.target_id))
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("second auto-attached session id")
        .to_owned();

    let mut first_utility_context = 0_i64;
    let mut second_utility_context = 0_i64;
    for (id, session_id, target_id, utility_context) in [
        (
            2321_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            &mut first_utility_context,
        ),
        (
            2322_u64,
            second_auto_session.as_str(),
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
        *utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context id after auto-attach");
        ctx.take_all();
    }

    let shared_wrapper_source = patchright_page_binding_wrapper_source(
        "sharedPatchedBinding",
        "__lm_deliverSharedPatchedBindingResult",
        None,
        false,
    );

    for (id, session_id, utility_context) in [
        (2323_u64, first_auto_session.as_str(), first_utility_context),
        (
            2324_u64,
            second_auto_session.as_str(),
            second_utility_context,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "contextId": utility_context,
                "expression": &shared_wrapper_source
            }
        }))
        .await;
        let install_wrapper = take_response_by_id(&mut ctx, id);
        assert_eq!(
            install_wrapper["result"]["result"]["value"],
            json!("function")
        );
    }

    for (id, session_id, utility_context, payload, expected_text) in [
        (
            2325_u64,
            first_auto_session.as_str(),
            first_utility_context,
            "payload-first",
            "first-page",
        ),
        (
            2328_u64,
            second_auto_session.as_str(),
            second_utility_context,
            "payload-second",
            "second-page",
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context,
                    "expression": format!("globalThis.__lm_bindingPromise = sharedPatchedBinding('{payload}'); JSON.stringify([typeof globalThis.sharedPatchedBinding, document.querySelector('#page').textContent])")
                }
            })).await;
        let scheduled = take_response_by_id(&mut ctx, id);
        assert_eq!(
            scheduled["result"]["result"]["value"],
            json!(format!("[\"function\",\"{expected_text}\"]"))
        );
        let binding_called = ctx
            .sent
            .iter()
            .find(|message| {
                message["method"] == json!("Runtime.bindingCalled")
                    && message["params"]["name"] == json!("sharedPatchedBinding")
                    && message["params"]["executionContextId"] == json!(utility_context)
            })
            .cloned()
            .expect("shared page binding wrapper should emit Runtime.bindingCalled");
        let binding_payload = binding_called["params"]["payload"]
            .as_str()
            .expect("binding payload should be a json string");
        let binding_payload: serde_json::Value =
            serde_json::from_str(binding_payload).expect("binding payload should be valid json");
        let seq = binding_payload["seq"]
            .as_i64()
            .expect("binding payload seq should be an integer");
        assert_eq!(binding_payload["name"], json!("sharedPatchedBinding"));
        assert_eq!(binding_payload["serializedArgs"], json!([payload]));
        ctx.sent.clear();

        ctx.process_async(json!({
                "id": id + 1,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context,
                    "expression": format!("globalThis.__lm_deliverSharedPatchedBindingResult({{ name: 'sharedPatchedBinding', seq: {seq}, result: 'ok-{payload}' }}); 'delivered'")
                }
            })).await;
        let delivered = take_response_by_id(&mut ctx, id + 1);
        assert_eq!(delivered["result"]["result"]["value"], json!("delivered"));

        ctx.process_async(json!({
            "id": id + 2,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "contextId": utility_context,
                "expression": "globalThis.__lm_bindingPromise",
                "awaitPromise": true
            }
        }))
        .await;
        let resolved = take_response_by_id(&mut ctx, id + 2);
        assert_eq!(
            resolved["result"]["result"]["value"],
            json!(format!("ok-{payload}"))
        );
    }

    ctx.process_async(json!({
        "id": 2331,
        "method": "Runtime.removeBinding",
        "sessionId": first_auto_session,
        "params": {
            "name": "sharedPatchedBinding"
        }
    }))
    .await;
    let remove_binding = take_response_by_id(&mut ctx, 2331);
    assert_eq!(remove_binding["result"], json!({}));

    ctx.process_async(json!({
        "id": 2332,
        "method": "Runtime.evaluate",
        "sessionId": first_auto_session,
        "params": {
            "contextId": first_utility_context,
            "expression": "typeof globalThis.sharedPatchedBinding"
        }
    }))
    .await;
    let first_removed = take_response_by_id(&mut ctx, 2332);
    assert_eq!(
        first_removed["result"]["result"]["value"],
        json!("undefined")
    );

    ctx.sent.clear();
    ctx.process_async(json!({
            "id": 2333,
            "method": "Runtime.evaluate",
            "sessionId": first_auto_session,
            "params": {
                "contextId": first_utility_context,
                "expression": "typeof globalThis.sharedPatchedBinding === 'function' ? sharedPatchedBinding('unexpected') : 'absent'"
            }
        })).await;
    let first_guarded = take_response_by_id(&mut ctx, 2333);
    assert_eq!(first_guarded["result"]["result"]["value"], json!("absent"));
    assert!(
        !ctx.sent.iter().any(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("sharedPatchedBinding")
        }),
        "removed shared page-binding wrapper should no longer emit bindingCalled in the cleaned-up context"
    );

    ctx.process_async(json!({
            "id": 2334,
            "method": "Runtime.evaluate",
            "sessionId": second_auto_session,
            "params": {
                "contextId": second_utility_context,
                "expression": "globalThis.__lm_bindingPromiseAfterCleanup = sharedPatchedBinding('payload-second-after-cleanup'); JSON.stringify([typeof globalThis.sharedPatchedBinding, document.querySelector('#page').textContent])"
            }
        })).await;
    let second_scheduled = take_response_by_id(&mut ctx, 2334);
    assert_eq!(
        second_scheduled["result"]["result"]["value"],
        json!("[\"function\",\"second-page\"]")
    );
    let second_binding_called = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("sharedPatchedBinding")
                && message["params"]["executionContextId"] == json!(second_utility_context)
        })
        .cloned()
        .expect("second context should still emit bindingCalled after first-context cleanup");
    let second_binding_payload = second_binding_called["params"]["payload"]
        .as_str()
        .expect("binding payload should be a json string");
    let second_binding_payload: serde_json::Value =
        serde_json::from_str(second_binding_payload).expect("binding payload should be valid json");
    let second_seq = second_binding_payload["seq"]
        .as_i64()
        .expect("binding payload seq should be an integer");
    assert_eq!(
        second_binding_payload["serializedArgs"],
        json!(["payload-second-after-cleanup"])
    );
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 2335,
            "method": "Runtime.evaluate",
            "sessionId": second_auto_session,
            "params": {
                "contextId": second_utility_context,
                "expression": format!("globalThis.__lm_deliverSharedPatchedBindingResult({{ name: 'sharedPatchedBinding', seq: {second_seq}, result: 'ok-second-after-cleanup' }}); 'delivered'")
            }
        })).await;
    let second_delivered = take_response_by_id(&mut ctx, 2335);
    assert_eq!(
        second_delivered["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 2336,
        "method": "Runtime.evaluate",
        "sessionId": second_auto_session,
        "params": {
            "contextId": second_utility_context,
            "expression": "globalThis.__lm_bindingPromiseAfterCleanup",
            "awaitPromise": true
        }
    }))
    .await;
    let second_resolved = take_response_by_id(&mut ctx, 2336);
    assert_eq!(
        second_resolved["result"]["result"]["value"],
        json!("ok-second-after-cleanup")
    );

    for (id, session_id, label) in [
        (2337_u64, first_auto_session.as_str(), "first-page-replay"),
        (2338_u64, second_auto_session.as_str(), "second-page-replay"),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": {
                "url": format!("data:text/html,<body><div id=page>{label}</div></body>")
            }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    let mut first_replay_utility_context = 0_i64;
    let mut second_replay_utility_context = 0_i64;
    for (id, session_id, target_id, utility_context) in [
        (
            2339_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            &mut first_replay_utility_context,
        ),
        (
            2340_u64,
            second_auto_session.as_str(),
            second.target_id.as_str(),
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
        *utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context after replay navigation");
        ctx.take_all();
    }

    for (id, session_id, utility_context, expected_type) in [
        (
            2341_u64,
            first_auto_session.as_str(),
            first_replay_utility_context,
            "undefined",
        ),
        (
            2342_u64,
            second_auto_session.as_str(),
            second_replay_utility_context,
            "function",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "contextId": utility_context,
                "expression": "typeof globalThis.sharedPatchedBinding"
            }
        }))
        .await;
        let replay_state = take_response_by_id(&mut ctx, id);
        assert_eq!(
            replay_state["result"]["result"]["value"],
            json!(expected_type)
        );
    }

    let first_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == first.browser_context_id)
        .expect("first browser context should still exist");
    assert!(
        !first_context
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
            binding.name == "sharedPatchedBinding"
                && binding.execution_context_name.as_deref() == Some("utility")
        }),
        "first browser context should no longer retain the shared binding definition"
    );

    let second_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == second.browser_context_id)
        .expect("second browser context should still exist");
    assert!(
        second_context
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
            binding.name == "sharedPatchedBinding"
                && binding.execution_context_name.as_deref() == Some("utility")
        }),
        "second browser context should retain the shared binding definition"
    );
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_auto_attach_sweep_page_binding_rejection_with_same_name_stays_isolated_per_browser_context()
 {
    super::super::patchright_8mb_stack(
        "patchright-utility-cleanup-rejecting-binding",
        || async {
    let mut ctx = TestContext::new();
    let first =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2411, 2412, 2413).await;
    let second =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2414, 2415, 2416).await;

    for (id, session_id, label) in [
        (2417_u64, first.session_id.as_str(), "first-page"),
        (2419_u64, second.session_id.as_str(), "second-page"),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.addBinding",
            "sessionId": session_id,
            "params": {
                "name": "sharedRejectingBinding",
                "executionContextName": "utility"
            }
        }))
        .await;
        let add_binding = take_response_by_id(&mut ctx, id);
        assert_eq!(add_binding["result"], json!({}));

        ctx.process_async(json!({
            "id": id + 1,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": {
                "url": format!("data:text/html,<body><div id=page>{label}</div></body>")
            }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id + 1);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, target_id, session_id) in [
        (
            2421_u64,
            first.target_id.as_str(),
            first.session_id.as_str(),
        ),
        (
            2422_u64,
            second.target_id.as_str(),
            second.session_id.as_str(),
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
        "id": 2423,
        "method": "Target.setAutoAttach",
        "params": {
            "autoAttach": true,
            "waitForDebuggerOnStart": false
        }
    }))
    .await;
    ctx.expect_result(2423, json!({}), None);
    let events = ctx.take_all();
    let attached_events = events
        .iter()
        .filter(|event| event["method"] == json!("Target.attachedToTarget"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        attached_events.len(),
        2,
        "auto-attach sweep should attach both targets"
    );
    let first_auto_session = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(first.target_id))
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("first auto-attached session id")
        .to_owned();
    let second_auto_session = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(second.target_id))
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("second auto-attached session id")
        .to_owned();

    let mut first_utility_context = 0_i64;
    let mut second_utility_context = 0_i64;
    for (id, session_id, target_id, utility_context) in [
        (
            2424_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            &mut first_utility_context,
        ),
        (
            2425_u64,
            second_auto_session.as_str(),
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
        *utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context id after auto-attach");
        ctx.take_all();
    }

    let shared_rejecting_wrapper_source = patchright_page_binding_wrapper_source(
        "sharedRejectingBinding",
        "__lm_deliverSharedRejectingBindingResult",
        None,
        false,
    );

    for (id, session_id, utility_context) in [
        (2426_u64, first_auto_session.as_str(), first_utility_context),
        (
            2427_u64,
            second_auto_session.as_str(),
            second_utility_context,
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "contextId": utility_context,
                "expression": &shared_rejecting_wrapper_source
            }
        }))
        .await;
        let install_wrapper = take_response_by_id(&mut ctx, id);
        assert_eq!(
            install_wrapper["result"]["result"]["value"],
            json!("function")
        );
    }

    ctx.process_async(json!({
        "id": 2428,
        "method": "Runtime.removeBinding",
        "sessionId": first_auto_session,
        "params": {
            "name": "sharedRejectingBinding"
        }
    }))
    .await;
    let remove_binding = take_response_by_id(&mut ctx, 2428);
    assert_eq!(remove_binding["result"], json!({}));

    ctx.process_async(json!({
        "id": 2429,
        "method": "Runtime.evaluate",
        "sessionId": first_auto_session,
        "params": {
            "contextId": first_utility_context,
            "expression": "typeof globalThis.sharedRejectingBinding"
        }
    }))
    .await;
    let first_removed = take_response_by_id(&mut ctx, 2429);
    assert_eq!(
        first_removed["result"]["result"]["value"],
        json!("undefined")
    );

    ctx.sent.clear();
    ctx.process_async(json!({
            "id": 2430,
            "method": "Runtime.evaluate",
            "sessionId": first_auto_session,
            "params": {
                "contextId": first_utility_context,
                "expression": "typeof globalThis.sharedRejectingBinding === 'function' ? sharedRejectingBinding('unexpected') : 'absent'"
            }
        })).await;
    let first_guarded = take_response_by_id(&mut ctx, 2430);
    assert_eq!(first_guarded["result"]["result"]["value"], json!("absent"));
    assert!(
        !ctx.sent.iter().any(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("sharedRejectingBinding")
        }),
        "removed shared rejecting page-binding wrapper should not emit bindingCalled in the cleaned-up context"
    );

    ctx.process_async(json!({
            "id": 2431,
            "method": "Runtime.evaluate",
            "sessionId": second_auto_session,
            "params": {
                "contextId": second_utility_context,
                "expression": "globalThis.__lm_bindingPromiseAfterCleanup = sharedRejectingBinding('payload-second-after-cleanup').then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled'"
            }
        })).await;
    let second_scheduled = take_response_by_id(&mut ctx, 2431);
    assert_eq!(
        second_scheduled["result"]["result"]["value"],
        json!("scheduled")
    );
    let second_binding_called = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("sharedRejectingBinding")
                && message["params"]["executionContextId"] == json!(second_utility_context)
        })
        .cloned()
        .expect("second context should still emit bindingCalled after first-context cleanup");
    let second_binding_payload = second_binding_called["params"]["payload"]
        .as_str()
        .expect("binding payload should be a json string");
    let second_binding_payload: serde_json::Value =
        serde_json::from_str(second_binding_payload).expect("binding payload should be valid json");
    let second_seq = second_binding_payload["seq"]
        .as_i64()
        .expect("binding payload seq should be an integer");
    assert_eq!(
        second_binding_payload["serializedArgs"],
        json!(["payload-second-after-cleanup"])
    );
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 2432,
            "method": "Runtime.evaluate",
            "sessionId": second_auto_session,
            "params": {
                "contextId": second_utility_context,
                "expression": format!("globalThis.__lm_deliverSharedRejectingBindingResult({{ name: 'sharedRejectingBinding', seq: {second_seq}, error: 'rejected-second-after-cleanup' }}); 'delivered'")
            }
        })).await;
    let second_delivered = take_response_by_id(&mut ctx, 2432);
    assert_eq!(
        second_delivered["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 2433,
        "method": "Runtime.evaluate",
        "sessionId": second_auto_session,
        "params": {
            "contextId": second_utility_context,
            "expression": "globalThis.__lm_bindingPromiseAfterCleanup",
            "awaitPromise": true
        }
    }))
    .await;
    let second_rejected = take_response_by_id(&mut ctx, 2433);
    assert_eq!(
        second_rejected["result"]["result"]["value"],
        json!("rejected:rejected-second-after-cleanup")
    );

    for (id, session_id, label) in [
        (2434_u64, first_auto_session.as_str(), "first-page-replay"),
        (2435_u64, second_auto_session.as_str(), "second-page-replay"),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Page.navigate",
            "sessionId": session_id,
            "params": {
                "url": format!("data:text/html,<body><div id=page>{label}</div></body>")
            }
        }))
        .await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    let mut first_replay_utility_context = 0_i64;
    let mut second_replay_utility_context = 0_i64;
    for (id, session_id, target_id, utility_context) in [
        (
            2436_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            &mut first_replay_utility_context,
        ),
        (
            2437_u64,
            second_auto_session.as_str(),
            second.target_id.as_str(),
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
        *utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context after replay navigation");
        ctx.take_all();
    }

    for (id, session_id, utility_context, expected_type) in [
        (
            2438_u64,
            first_auto_session.as_str(),
            first_replay_utility_context,
            "undefined",
        ),
        (
            2439_u64,
            second_auto_session.as_str(),
            second_replay_utility_context,
            "function",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "contextId": utility_context,
                "expression": "typeof globalThis.sharedRejectingBinding"
            }
        }))
        .await;
        let replay_state = take_response_by_id(&mut ctx, id);
        assert_eq!(
            replay_state["result"]["result"]["value"],
            json!(expected_type)
        );
    }

    let first_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == first.browser_context_id)
        .expect("first browser context should still exist");
    assert!(
        !first_context
            .devtools_session_state()

            .runtime_bindings
            .iter()
            .any(|binding| {
                binding.name == "sharedRejectingBinding"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "first browser context should no longer retain the shared rejecting binding definition"
    );

    let second_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == second.browser_context_id)
        .expect("second browser context should still exist");
    assert!(
        second_context
            .devtools_session_state()

            .runtime_bindings
            .iter()
            .any(|binding| {
                binding.name == "sharedRejectingBinding"
                    && binding.execution_context_name.as_deref() == Some("utility")
        }),
        "second browser context should retain the shared rejecting binding definition"
    );
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_auto_attach_sweep_handle_page_binding_cleanup_with_same_name_stays_isolated_per_browser_context()
 {
    super::super::patchright_8mb_stack(
        "patchright-utility-cleanup-same-name",
        || async {
    let mut ctx = TestContext::new();
    let first =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2343, 2344, 2345).await;
    let second =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2346, 2347, 2348).await;

    for (id, session_id, label, handle_id) in [
        (
            2349_u64,
            first.session_id.as_str(),
            "first-page",
            "first-handle",
        ),
        (
            2351_u64,
            second.session_id.as_str(),
            "second-page",
            "second-handle",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.addBinding",
            "sessionId": session_id,
            "params": {
                "name": "sharedHandleBinding",
                "executionContextName": "utility"
            }
        }))
        .await;
        let add_binding = take_response_by_id(&mut ctx, id);
        assert_eq!(add_binding["result"], json!({}));

        ctx.process_async(json!({
                "id": id + 1,
                "method": "Page.navigate",
                "sessionId": session_id,
                "params": {
                    "url": format!("data:text/html,<body><div id='page'>{label}</div><div id='{handle_id}'>node</div></body>")
                }
            })).await;
        let navigation = take_response_by_id(&mut ctx, id + 1);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, target_id, session_id) in [
        (
            2353_u64,
            first.target_id.as_str(),
            first.session_id.as_str(),
        ),
        (
            2354_u64,
            second.target_id.as_str(),
            second.session_id.as_str(),
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
        "id": 2355,
        "method": "Target.setAutoAttach",
        "params": {
            "autoAttach": true,
            "waitForDebuggerOnStart": false
        }
    }))
    .await;
    ctx.expect_result(2355, json!({}), None);
    let events = ctx.take_all();
    let attached_events = events
        .iter()
        .filter(|event| event["method"] == json!("Target.attachedToTarget"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        attached_events.len(),
        2,
        "auto-attach sweep should attach both targets"
    );
    let first_auto_session = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(first.target_id))
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("first auto-attached session id")
        .to_owned();
    let second_auto_session = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(second.target_id))
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("second auto-attached session id")
        .to_owned();

    let mut first_utility_context = 0_i64;
    let mut second_utility_context = 0_i64;
    for (id, session_id, target_id, utility_context) in [
        (
            2356_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            &mut first_utility_context,
        ),
        (
            2357_u64,
            second_auto_session.as_str(),
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
        *utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context id after auto-attach");
        ctx.take_all();
    }

    for (id, session_id, utility_context) in [
        (2358_u64, first_auto_session.as_str(), first_utility_context),
        (
            2359_u64,
            second_auto_session.as_str(),
            second_utility_context,
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context,
                    "expression": r#"
                        (() => {
                            function addHandleBinding(bindingName) {
                                const binding = globalThis[bindingName];
                                globalThis[bindingName] = (...args) => {
                                    const me = globalThis[bindingName];
                                    let callbacks = me.callbacks;
                                    if (!callbacks) {
                                        callbacks = new Map();
                                        me.callbacks = callbacks;
                                    }
                                    let handles = me.handles;
                                    if (!handles) {
                                        handles = new Map();
                                        me.handles = handles;
                                    }
                                    const seq = (me.lastSeq || 0) + 1;
                                    me.lastSeq = seq;
                                    handles.set(seq, args[0]);
                                    const promise = new Promise((resolve, reject) => callbacks.set(seq, { resolve, reject }));
                                    binding(JSON.stringify({ name: bindingName, seq }));
                                    return promise;
                                };
                            }
                            function takeBindingHandle(arg) {
                                const handles = globalThis[arg.name].handles;
                                const handle = handles.get(arg.seq);
                                handles.delete(arg.seq);
                                return handle;
                            }
                            function deliverBindingResult(arg) {
                                const callbacks = globalThis[arg.name].callbacks;
                                if ('error' in arg)
                                    callbacks.get(arg.seq).reject(arg.error);
                                else
                                    callbacks.get(arg.seq).resolve(arg.result);
                                callbacks.delete(arg.seq);
                            }
                            addHandleBinding('sharedHandleBinding');
                            globalThis.__lm_takeSharedHandleBindingHandle = takeBindingHandle;
                            globalThis.__lm_deliverSharedHandleBindingResult = deliverBindingResult;
                            return typeof globalThis.sharedHandleBinding;
                        })()
                    "#
                }
            })).await;
        let install_wrapper = take_response_by_id(&mut ctx, id);
        assert_eq!(
            install_wrapper["result"]["result"]["value"],
            json!("function")
        );
    }

    for (id, session_id, utility_context, handle_id, expected_tag) in [
        (
            2360_u64,
            first_auto_session.as_str(),
            first_utility_context,
            "first-handle",
            "FIRST-HANDLE",
        ),
        (
            2364_u64,
            second_auto_session.as_str(),
            second_utility_context,
            "second-handle",
            "SECOND-HANDLE",
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context,
                    "expression": format!("globalThis.__lm_handlePromise = sharedHandleBinding(document.getElementById('{handle_id}')); 'scheduled'")
                }
            })).await;
        let scheduled = take_response_by_id(&mut ctx, id);
        assert_eq!(scheduled["result"]["result"]["value"], json!("scheduled"));
        let binding_called = ctx
            .sent
            .iter()
            .find(|message| {
                message["method"] == json!("Runtime.bindingCalled")
                    && message["params"]["name"] == json!("sharedHandleBinding")
                    && message["params"]["executionContextId"] == json!(utility_context)
            })
            .cloned()
            .expect("shared handle binding wrapper should emit Runtime.bindingCalled");
        let binding_payload = binding_called["params"]["payload"]
            .as_str()
            .expect("binding payload should be a json string");
        let binding_payload: serde_json::Value =
            serde_json::from_str(binding_payload).expect("binding payload should be valid json");
        let seq = binding_payload["seq"]
            .as_i64()
            .expect("binding payload seq should be an integer");
        assert_eq!(binding_payload["name"], json!("sharedHandleBinding"));
        assert_eq!(
            binding_payload,
            json!({
                "name": "sharedHandleBinding",
                "seq": binding_payload["seq"],
            })
        );
        ctx.sent.clear();

        ctx.process_async(json!({
                "id": id + 1,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context,
                    "expression": format!("(() => {{ const handle = globalThis.__lm_takeSharedHandleBindingHandle({{ name: 'sharedHandleBinding', seq: {seq} }}); return JSON.stringify([handle.id, typeof globalThis.__lm_takeSharedHandleBindingHandle({{ name: 'sharedHandleBinding', seq: {seq} }})]); }})()")
                }
            })).await;
        let taken_handle = take_response_by_id(&mut ctx, id + 1);
        assert_eq!(
            taken_handle["result"]["result"]["value"],
            json!(format!("[\"{handle_id}\",\"undefined\"]"))
        );

        ctx.process_async(json!({
                "id": id + 2,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context,
                    "expression": format!("globalThis.__lm_deliverSharedHandleBindingResult({{ name: 'sharedHandleBinding', seq: {seq}, result: '{expected_tag}' }}); 'delivered'")
                }
            })).await;
        let delivered = take_response_by_id(&mut ctx, id + 2);
        assert_eq!(delivered["result"]["result"]["value"], json!("delivered"));

        ctx.process_async(json!({
            "id": id + 3,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "contextId": utility_context,
                "expression": "globalThis.__lm_handlePromise",
                "awaitPromise": true
            }
        }))
        .await;
        let resolved = take_response_by_id(&mut ctx, id + 3);
        assert_eq!(resolved["result"]["result"]["value"], json!(expected_tag));
    }

    ctx.process_async(json!({
        "id": 2368,
        "method": "Runtime.removeBinding",
        "sessionId": first_auto_session,
        "params": {
            "name": "sharedHandleBinding"
        }
    }))
    .await;
    let remove_binding = take_response_by_id(&mut ctx, 2368);
    assert_eq!(remove_binding["result"], json!({}));

    ctx.process_async(json!({
        "id": 2369,
        "method": "Runtime.evaluate",
        "sessionId": first_auto_session,
        "params": {
            "contextId": first_utility_context,
            "expression": "typeof globalThis.sharedHandleBinding"
        }
    }))
    .await;
    let first_removed = take_response_by_id(&mut ctx, 2369);
    assert_eq!(
        first_removed["result"]["result"]["value"],
        json!("undefined")
    );

    ctx.sent.clear();
    ctx.process_async(json!({
            "id": 2370,
            "method": "Runtime.evaluate",
            "sessionId": first_auto_session,
            "params": {
                "contextId": first_utility_context,
                "expression": "typeof globalThis.sharedHandleBinding === 'function' ? sharedHandleBinding(document.getElementById('first-handle')) : 'absent'"
            }
        })).await;
    let first_guarded = take_response_by_id(&mut ctx, 2370);
    assert_eq!(first_guarded["result"]["result"]["value"], json!("absent"));
    assert!(
        !ctx.sent.iter().any(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("sharedHandleBinding")
        }),
        "removed shared handle-binding wrapper should no longer emit bindingCalled in the cleaned-up context"
    );

    ctx.process_async(json!({
            "id": 2371,
            "method": "Runtime.evaluate",
            "sessionId": second_auto_session,
            "params": {
                "contextId": second_utility_context,
                "expression": "globalThis.__lm_handlePromiseAfterCleanup = sharedHandleBinding(document.getElementById('second-handle')); 'scheduled'"
            }
        })).await;
    let second_scheduled = take_response_by_id(&mut ctx, 2371);
    assert_eq!(
        second_scheduled["result"]["result"]["value"],
        json!("scheduled")
    );
    let second_binding_called = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("sharedHandleBinding")
                && message["params"]["executionContextId"] == json!(second_utility_context)
        })
        .cloned()
        .expect("second context should still emit bindingCalled after first-context cleanup");
    let second_binding_payload = second_binding_called["params"]["payload"]
        .as_str()
        .expect("binding payload should be a json string");
    let second_binding_payload: serde_json::Value =
        serde_json::from_str(second_binding_payload).expect("binding payload should be valid json");
    let second_seq = second_binding_payload["seq"]
        .as_i64()
        .expect("binding payload seq should be an integer");
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 2372,
            "method": "Runtime.evaluate",
            "sessionId": second_auto_session,
            "params": {
                "contextId": second_utility_context,
                "expression": format!("(() => {{ const handle = globalThis.__lm_takeSharedHandleBindingHandle({{ name: 'sharedHandleBinding', seq: {second_seq} }}); return JSON.stringify([handle.id, typeof globalThis.__lm_takeSharedHandleBindingHandle({{ name: 'sharedHandleBinding', seq: {second_seq} }})]); }})()")
            }
        })).await;
    let second_taken_handle = take_response_by_id(&mut ctx, 2372);
    assert_eq!(
        second_taken_handle["result"]["result"]["value"],
        json!("[\"second-handle\",\"undefined\"]")
    );

    ctx.process_async(json!({
            "id": 2373,
            "method": "Runtime.evaluate",
            "sessionId": second_auto_session,
            "params": {
                "contextId": second_utility_context,
                "expression": format!("globalThis.__lm_deliverSharedHandleBindingResult({{ name: 'sharedHandleBinding', seq: {second_seq}, result: 'SECOND-HANDLE-AFTER-CLEANUP' }}); 'delivered'")
            }
        })).await;
    let second_delivered = take_response_by_id(&mut ctx, 2373);
    assert_eq!(
        second_delivered["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 2374,
        "method": "Runtime.evaluate",
        "sessionId": second_auto_session,
        "params": {
            "contextId": second_utility_context,
            "expression": "globalThis.__lm_handlePromiseAfterCleanup",
            "awaitPromise": true
        }
    }))
    .await;
    let second_resolved = take_response_by_id(&mut ctx, 2374);
    assert_eq!(
        second_resolved["result"]["result"]["value"],
        json!("SECOND-HANDLE-AFTER-CLEANUP")
    );

    for (id, session_id, label, handle_id) in [
        (
            2375_u64,
            first_auto_session.as_str(),
            "first-page-replay",
            "first-replay-handle",
        ),
        (
            2376_u64,
            second_auto_session.as_str(),
            "second-page-replay",
            "second-replay-handle",
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Page.navigate",
                "sessionId": session_id,
                "params": {
                    "url": format!("data:text/html,<body><div id='page'>{label}</div><div id='{handle_id}'>node</div></body>")
                }
            })).await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    let mut first_replay_utility_context = 0_i64;
    let mut second_replay_utility_context = 0_i64;
    for (id, session_id, target_id, utility_context) in [
        (
            2377_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            &mut first_replay_utility_context,
        ),
        (
            2378_u64,
            second_auto_session.as_str(),
            second.target_id.as_str(),
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
        *utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context after replay navigation");
        ctx.take_all();
    }

    for (id, session_id, utility_context, expected_type) in [
        (
            2379_u64,
            first_auto_session.as_str(),
            first_replay_utility_context,
            "undefined",
        ),
        (
            2380_u64,
            second_auto_session.as_str(),
            second_replay_utility_context,
            "function",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "contextId": utility_context,
                "expression": "typeof globalThis.sharedHandleBinding"
            }
        }))
        .await;
        let replay_state = take_response_by_id(&mut ctx, id);
        assert_eq!(
            replay_state["result"]["result"]["value"],
            json!(expected_type)
        );
    }

    let first_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == first.browser_context_id)
        .expect("first browser context should still exist");
    assert!(
        !first_context
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
            binding.name == "sharedHandleBinding"
                && binding.execution_context_name.as_deref() == Some("utility")
        }),
        "first browser context should no longer retain the shared handle binding definition"
    );

    let second_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == second.browser_context_id)
        .expect("second browser context should still exist");
    assert!(
        second_context
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
            binding.name == "sharedHandleBinding"
                && binding.execution_context_name.as_deref() == Some("utility")
        }),
        "second browser context should retain the shared handle binding definition"
    );
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn patchright_over_cdp_auto_attach_sweep_handle_page_binding_rejection_with_same_name_stays_isolated_per_browser_context()
 {
    super::super::patchright_8mb_stack(
        "patchright-utility-handle-rejection-cleanup-same-name",
        run_patchright_over_cdp_auto_attach_sweep_handle_page_binding_rejection_with_same_name_stays_isolated_per_browser_context,
    )
    .await;
}

async fn run_patchright_over_cdp_auto_attach_sweep_handle_page_binding_rejection_with_same_name_stays_isolated_per_browser_context()
 {
    let mut ctx = TestContext::new();
    let first =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2381, 2382, 2383).await;
    let second =
        create_attached_page_session_without_runtime_enable_async(&mut ctx, 2384, 2385, 2386).await;

    for (id, session_id, label, handle_id) in [
        (
            2387_u64,
            first.session_id.as_str(),
            "first-page",
            "first-handle",
        ),
        (
            2389_u64,
            second.session_id.as_str(),
            "second-page",
            "second-handle",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.addBinding",
            "sessionId": session_id,
            "params": {
                "name": "sharedRejectingHandleBinding",
                "executionContextName": "utility"
            }
        }))
        .await;
        let add_binding = take_response_by_id(&mut ctx, id);
        assert_eq!(add_binding["result"], json!({}));

        ctx.process_async(json!({
                "id": id + 1,
                "method": "Page.navigate",
                "sessionId": session_id,
                "params": {
                    "url": format!("data:text/html,<body><div id='page'>{label}</div><div id='{handle_id}'>node</div></body>")
                }
            })).await;
        let navigation = take_response_by_id(&mut ctx, id + 1);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    for (id, target_id, session_id) in [
        (
            2391_u64,
            first.target_id.as_str(),
            first.session_id.as_str(),
        ),
        (
            2392_u64,
            second.target_id.as_str(),
            second.session_id.as_str(),
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
        "id": 2393,
        "method": "Target.setAutoAttach",
        "params": {
            "autoAttach": true,
            "waitForDebuggerOnStart": false
        }
    }))
    .await;
    ctx.expect_result(2393, json!({}), None);
    let events = ctx.take_all();
    let attached_events = events
        .iter()
        .filter(|event| event["method"] == json!("Target.attachedToTarget"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        attached_events.len(),
        2,
        "auto-attach sweep should attach both targets"
    );
    let first_auto_session = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(first.target_id))
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("first auto-attached session id")
        .to_owned();
    let second_auto_session = attached_events
        .iter()
        .find(|event| event["params"]["targetInfo"]["targetId"] == json!(second.target_id))
        .and_then(|event| event["params"]["sessionId"].as_str())
        .expect("second auto-attached session id")
        .to_owned();

    let mut first_utility_context = 0_i64;
    let mut second_utility_context = 0_i64;
    for (id, session_id, target_id, utility_context) in [
        (
            2394_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            &mut first_utility_context,
        ),
        (
            2395_u64,
            second_auto_session.as_str(),
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
        *utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context id after auto-attach");
        ctx.take_all();
    }

    for (id, session_id, utility_context) in [
        (2396_u64, first_auto_session.as_str(), first_utility_context),
        (
            2397_u64,
            second_auto_session.as_str(),
            second_utility_context,
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Runtime.evaluate",
                "sessionId": session_id,
                "params": {
                    "contextId": utility_context,
                    "expression": r#"
                        (() => {
                            function addHandleBinding(bindingName) {
                                const binding = globalThis[bindingName];
                                globalThis[bindingName] = (...args) => {
                                    const me = globalThis[bindingName];
                                    let callbacks = me.callbacks;
                                    if (!callbacks) {
                                        callbacks = new Map();
                                        me.callbacks = callbacks;
                                    }
                                    let handles = me.handles;
                                    if (!handles) {
                                        handles = new Map();
                                        me.handles = handles;
                                    }
                                    const seq = (me.lastSeq || 0) + 1;
                                    me.lastSeq = seq;
                                    handles.set(seq, args[0]);
                                    const promise = new Promise((resolve, reject) => callbacks.set(seq, { resolve, reject }));
                                    binding(JSON.stringify({ name: bindingName, seq }));
                                    return promise;
                                };
                            }
                            function takeBindingHandle(arg) {
                                const handles = globalThis[arg.name].handles;
                                const handle = handles.get(arg.seq);
                                handles.delete(arg.seq);
                                return handle;
                            }
                            function deliverBindingResult(arg) {
                                const callbacks = globalThis[arg.name].callbacks;
                                if ('error' in arg)
                                    callbacks.get(arg.seq).reject(arg.error);
                                else
                                    callbacks.get(arg.seq).resolve(arg.result);
                                callbacks.delete(arg.seq);
                            }
                            addHandleBinding('sharedRejectingHandleBinding');
                            globalThis.__lm_takeSharedRejectingHandleBindingHandle = takeBindingHandle;
                            globalThis.__lm_deliverSharedRejectingHandleBindingResult = deliverBindingResult;
                            return typeof globalThis.sharedRejectingHandleBinding;
                        })()
                    "#
                }
            })).await;
        let install_wrapper = take_response_by_id(&mut ctx, id);
        assert_eq!(
            install_wrapper["result"]["result"]["value"],
            json!("function")
        );
    }

    ctx.process_async(json!({
        "id": 2398,
        "method": "Runtime.removeBinding",
        "sessionId": first_auto_session,
        "params": {
            "name": "sharedRejectingHandleBinding"
        }
    }))
    .await;
    let remove_binding = take_response_by_id(&mut ctx, 2398);
    assert_eq!(remove_binding["result"], json!({}));

    ctx.process_async(json!({
        "id": 2399,
        "method": "Runtime.evaluate",
        "sessionId": first_auto_session,
        "params": {
            "contextId": first_utility_context,
            "expression": "typeof globalThis.sharedRejectingHandleBinding"
        }
    }))
    .await;
    let first_removed = take_response_by_id(&mut ctx, 2399);
    assert_eq!(
        first_removed["result"]["result"]["value"],
        json!("undefined")
    );

    ctx.sent.clear();
    ctx.process_async(json!({
            "id": 2400,
            "method": "Runtime.evaluate",
            "sessionId": first_auto_session,
            "params": {
                "contextId": first_utility_context,
                "expression": "typeof globalThis.sharedRejectingHandleBinding === 'function' ? sharedRejectingHandleBinding(document.getElementById('first-handle')) : 'absent'"
            }
        })).await;
    let first_guarded = take_response_by_id(&mut ctx, 2400);
    assert_eq!(first_guarded["result"]["result"]["value"], json!("absent"));
    assert!(
        !ctx.sent.iter().any(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("sharedRejectingHandleBinding")
        }),
        "removed shared rejecting handle-binding wrapper should no longer emit bindingCalled in the cleaned-up context"
    );

    ctx.process_async(json!({
            "id": 2401,
            "method": "Runtime.evaluate",
            "sessionId": second_auto_session,
            "params": {
                "contextId": second_utility_context,
                "expression": "globalThis.__lm_handlePromiseAfterCleanup = sharedRejectingHandleBinding(document.getElementById('second-handle')).then(() => 'unexpected-success', error => `rejected:${error}`); 'scheduled'"
            }
        })).await;
    let second_scheduled = take_response_by_id(&mut ctx, 2401);
    assert_eq!(
        second_scheduled["result"]["result"]["value"],
        json!("scheduled")
    );
    let second_binding_called = ctx
        .sent
        .iter()
        .find(|message| {
            message["method"] == json!("Runtime.bindingCalled")
                && message["params"]["name"] == json!("sharedRejectingHandleBinding")
                && message["params"]["executionContextId"] == json!(second_utility_context)
        })
        .cloned()
        .expect("second context should still emit bindingCalled after first-context cleanup");
    let second_binding_payload = second_binding_called["params"]["payload"]
        .as_str()
        .expect("binding payload should be a json string");
    let second_binding_payload: serde_json::Value =
        serde_json::from_str(second_binding_payload).expect("binding payload should be valid json");
    let second_seq = second_binding_payload["seq"]
        .as_i64()
        .expect("binding payload seq should be an integer");
    ctx.sent.clear();

    ctx.process_async(json!({
            "id": 2402,
            "method": "Runtime.evaluate",
            "sessionId": second_auto_session,
            "params": {
                "contextId": second_utility_context,
                "expression": format!("(() => {{ const handle = globalThis.__lm_takeSharedRejectingHandleBindingHandle({{ name: 'sharedRejectingHandleBinding', seq: {second_seq} }}); return JSON.stringify([handle.id, typeof globalThis.__lm_takeSharedRejectingHandleBindingHandle({{ name: 'sharedRejectingHandleBinding', seq: {second_seq} }})]); }})()")
            }
        })).await;
    let second_taken_handle = take_response_by_id(&mut ctx, 2402);
    assert_eq!(
        second_taken_handle["result"]["result"]["value"],
        json!("[\"second-handle\",\"undefined\"]")
    );

    ctx.process_async(json!({
            "id": 2403,
            "method": "Runtime.evaluate",
            "sessionId": second_auto_session,
            "params": {
                "contextId": second_utility_context,
                "expression": format!("globalThis.__lm_deliverSharedRejectingHandleBindingResult({{ name: 'sharedRejectingHandleBinding', seq: {second_seq}, error: 'reject-second-after-cleanup' }}); 'delivered'")
            }
        })).await;
    let second_delivered = take_response_by_id(&mut ctx, 2403);
    assert_eq!(
        second_delivered["result"]["result"]["value"],
        json!("delivered")
    );

    ctx.process_async(json!({
        "id": 2404,
        "method": "Runtime.evaluate",
        "sessionId": second_auto_session,
        "params": {
            "contextId": second_utility_context,
            "expression": "globalThis.__lm_handlePromiseAfterCleanup",
            "awaitPromise": true
        }
    }))
    .await;
    let second_rejected = take_response_by_id(&mut ctx, 2404);
    assert_eq!(
        second_rejected["result"]["result"]["value"],
        json!("rejected:reject-second-after-cleanup")
    );

    for (id, session_id, label, handle_id) in [
        (
            2405_u64,
            first_auto_session.as_str(),
            "first-page-replay",
            "first-replay-handle",
        ),
        (
            2406_u64,
            second_auto_session.as_str(),
            "second-page-replay",
            "second-replay-handle",
        ),
    ] {
        ctx.process_async(json!({
                "id": id,
                "method": "Page.navigate",
                "sessionId": session_id,
                "params": {
                    "url": format!("data:text/html,<body><div id='page'>{label}</div><div id='{handle_id}'>node</div></body>")
                }
            })).await;
        let navigation = take_response_by_id(&mut ctx, id);
        assert_eq!(navigation["sessionId"], json!(session_id));
        ctx.take_all();
    }

    let mut first_replay_utility_context = 0_i64;
    let mut second_replay_utility_context = 0_i64;
    for (id, session_id, target_id, utility_context) in [
        (
            2407_u64,
            first_auto_session.as_str(),
            first.target_id.as_str(),
            &mut first_replay_utility_context,
        ),
        (
            2408_u64,
            second_auto_session.as_str(),
            second.target_id.as_str(),
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
        *utility_context = take_response_by_id(&mut ctx, id)["result"]["executionContextId"]
            .as_i64()
            .expect("utility context after replay navigation");
        ctx.take_all();
    }

    for (id, session_id, utility_context, expected_type) in [
        (
            2409_u64,
            first_auto_session.as_str(),
            first_replay_utility_context,
            "undefined",
        ),
        (
            2410_u64,
            second_auto_session.as_str(),
            second_replay_utility_context,
            "function",
        ),
    ] {
        ctx.process_async(json!({
            "id": id,
            "method": "Runtime.evaluate",
            "sessionId": session_id,
            "params": {
                "contextId": utility_context,
                "expression": "typeof globalThis.sharedRejectingHandleBinding"
            }
        }))
        .await;
        let replay_state = take_response_by_id(&mut ctx, id);
        assert_eq!(
            replay_state["result"]["result"]["value"],
            json!(expected_type)
        );
    }

    let first_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == first.browser_context_id)
        .expect("first browser context should still exist");
    assert!(
        !first_context
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
                binding.name == "sharedRejectingHandleBinding"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "first browser context should no longer retain the shared rejecting handle binding definition"
    );

    let second_context = ctx
        .conn
        .browser_contexts()
        .find(|bc| bc.id == second.browser_context_id)
        .expect("second browser context should still exist");
    assert!(
        second_context
            .devtools_session_state()
            .runtime_bindings
            .iter()
            .any(|binding| {
                binding.name == "sharedRejectingHandleBinding"
                    && binding.execution_context_name.as_deref() == Some("utility")
            }),
        "second browser context should retain the shared rejecting handle binding definition"
    );
}
