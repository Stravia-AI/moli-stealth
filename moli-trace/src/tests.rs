use std::time::Duration;

use super::*;

#[test]
fn browser_owner_machine_trace_schema_is_explicit_and_redacted() {
    let record = BrowserOwnerTraceRecord::new(
        "page_replacement_committed",
        "network",
        "commit-ready",
        "page-resident",
    )
    .with_browser_instance_id(Some(7))
    .with_browser_context_id(Some("context-1"))
    .with_target_id(Some("target-1"))
    .with_page_residence_generation(Some(3))
    .with_navigation_request_id(Some(11))
    .with_document_lifecycle_identity(Some(BrowserOwnerTraceDocument::new(5, 2, 1)))
    .with_browser_action_id(Some(13))
    .with_navigation_origin(Some("frontend-command"));

    let value = serde_json::to_value(record).expect("trace record should serialize");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["browser_instance_id"], 7);
    assert_eq!(value["document_lifecycle_identity"]["renderer_page_id"], 5);
    assert!(value.get("session_id").is_none());
    assert!(value.get("url").is_none());
    assert!(value.get("body").is_none());
    assert!(value.get("authorization").is_none());
}

#[test]
fn dom_binding_stats_are_empty_when_flag_disabled() {
    record_dom_binding_operation("appendChild", Duration::from_micros(7));
    assert!(take_dom_binding_operation_stats().is_empty());
}

#[test]
fn promise_hook_stats_are_empty_when_flag_disabled() {
    record_promise_hook_init();
    record_promise_hook_resolve();
    record_promise_reaction_before();
    record_promise_reaction_after();
    assert_eq!(take_promise_hook_stats(), PromiseHookStats::default());
}
