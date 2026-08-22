use crate::conn::{BackgroundTarget, BrowserContext, CdpConnection};
use moli_core::browser_host::BrowserPageResidenceHandle;

fn connection_with_active_and_background_targets() -> CdpConnection {
    let mut connection = CdpConnection::new();
    let mut browser_context = BrowserContext::new("context-1".to_owned());
    browser_context.set_active_target_id("target-active");
    connection.insert_browser_context(browser_context);
    let target = BackgroundTarget::with_url(
        "target-background".to_owned(),
        None,
        "about:blank#background".to_owned(),
    );
    connection
        .register_background_target_projection(
            "context-1",
            "target-background",
            move |browser_context, target_handle, page_residence, session_storage_access| {
                let mut target = target;
                target.replace_target_handle(target_handle);
                target.replace_page_residence_handle(page_residence);
                target.bind_session_storage_access(session_storage_access);
                browser_context.background_targets.push(target);
            },
        )
        .expect("background Target projection should register");
    connection
}

#[test]
fn frontend_projection_uses_browser_snapshot_order() {
    let connection = connection_with_active_and_background_targets();
    let snapshot = connection
        .capture_browser_top_level_target_snapshot()
        .expect("exact physical state should snapshot");
    let infos = connection
        .project_devtools_target_infos_from_browser_snapshot(&snapshot)
        .expect("exact snapshot should project");
    let target_ids = infos
        .iter()
        .filter_map(|info| info.target_id.as_ref().map(|id| id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(target_ids, ["target-active", "target-background"]);
}

#[test]
fn stale_target_snapshot_cannot_project_replacement_payload() {
    let mut connection = connection_with_active_and_background_targets();
    let snapshot = connection
        .capture_browser_top_level_target_snapshot()
        .expect("exact physical state should snapshot");
    let stale = snapshot
        .target("target-background")
        .expect("background snapshot")
        .clone();

    connection
        .rollback_staged_background_target_projection("context-1", "target-background")
        .expect("old Target should retire");
    let successor = BackgroundTarget::with_url(
        "target-background".to_owned(),
        None,
        "about:blank#successor".to_owned(),
    );
    connection
        .register_background_target_projection(
            "context-1",
            "target-background",
            move |browser_context, target_handle, page_residence, session_storage_access| {
                let mut target = successor;
                target.replace_target_handle(target_handle);
                target.replace_page_residence_handle(page_residence);
                target.bind_session_storage_access(session_storage_access);
                browser_context.background_targets.push(target);
            },
        )
        .expect("successor with reused public id should register");

    let error = connection
        .project_top_level_target_snapshot(&stale)
        .expect_err("predecessor snapshot must not project successor metadata");
    assert!(error.message.contains("current-state snapshot"));
}

#[test]
fn exact_snapshot_rejects_wrong_physical_page_slot() {
    let mut connection = connection_with_active_and_background_targets();
    let snapshot = connection
        .capture_browser_top_level_target_snapshot()
        .expect("exact physical state should snapshot");
    let target = snapshot
        .target("target-background")
        .expect("background snapshot")
        .clone();
    connection
        .browser_context
        .as_mut()
        .expect("browser context")
        .background_target_mut("target-background")
        .expect("physical background Target")
        .replace_page_residence_handle(BrowserPageResidenceHandle::default());

    let error = connection
        .project_top_level_target_snapshot(&target)
        .expect_err("wrong physical Page slot must not project");
    assert!(error.message.contains("Page residence"));
}

#[test]
fn empty_snapshot_from_another_browser_cannot_project() {
    let source = CdpConnection::new();
    let snapshot = source
        .capture_browser_top_level_target_snapshot()
        .expect("empty Browser should snapshot");
    let destination = CdpConnection::new();

    let error = destination
        .project_devtools_target_infos_from_browser_snapshot(&snapshot)
        .expect_err("foreign empty snapshot must retain Browser provenance");
    assert!(error.message.contains("another Browser instance"));
}
