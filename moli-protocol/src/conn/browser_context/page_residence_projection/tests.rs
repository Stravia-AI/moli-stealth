use moli_core::browser_host::{
    BrowserInitialEmptyDocumentSeed, BrowserNavigationFailure, BrowserPageOwnerKey,
    BrowserPageResidenceHandle,
};
use url::Url;

use crate::conn::{BrowserContext, CdpConnection, TargetProjectionError};

use super::PageResidenceProjectionError;

#[tokio::test]
async fn physical_residence_divergence_rejects_failed_discard_before_core_commit() {
    let mut connection = CdpConnection::default();
    connection.install_default_browser_target();
    let owner = connection
        .target_page_owner_key_for_session(None)
        .expect("default Target owner");
    let authoritative = connection
        .browser_host_state
        .navigation_owner()
        .page_residence_handle(&owner)
        .expect("authoritative Page residence");
    let generation_before = authoritative.generation();
    let had_loaded_page = connection
        .browser_context
        .as_ref()
        .is_some_and(BrowserContext::has_loaded_page);
    let navigation = connection
        .start_document_navigation_for_session_owner(None, "LOADER-failed".to_owned())
        .expect("default Target should start failed navigation");
    let wrong_residence = BrowserPageResidenceHandle::default();
    connection
        .browser_context
        .as_mut()
        .expect("selected BrowserContext")
        .active_target
        .runtime_slot
        .page_slot_mut()
        .replace_page_residence_handle(wrong_residence.clone());

    let error = connection
        .discard_loaded_page_after_failed_navigation_for_session_owner_async(
            None,
            &navigation,
            BrowserNavigationFailure::Network {
                error_text: "net::ERR_FAILED".to_owned(),
            },
            &Url::parse("https://failed.example/").expect("failure URL"),
        )
        .await
        .expect_err("wrong physical residence must reject before Core commit");

    assert!(matches!(
        error,
        PageResidenceProjectionError::TargetTopology(
            TargetProjectionError::PhysicalPageResidenceMismatch {
                ref browser_context_id,
                ref target_id,
            }
        ) if browser_context_id == owner.browser_context_id() && target_id == owner.target_id()
    ));
    assert_eq!(
        authoritative.generation(),
        generation_before,
        "rejected physical projection must not advance Browser Core"
    );
    let physical = connection
        .browser_context
        .as_ref()
        .expect("physical BrowserContext must remain installed");
    assert_eq!(
        physical.has_loaded_page(),
        had_loaded_page,
        "rejected projection must preserve physical Page presence"
    );
    assert_eq!(
        physical.active_target.runtime_slot.page_residence_handle(),
        &wrong_residence,
        "typed rejection must preserve the exact divergent physical payload"
    );
}

#[tokio::test]
async fn occupied_initial_document_slot_is_rejected_before_materialization_commit() {
    let context_id = "CTX-occupied-initial";
    let target_id = "TID-occupied-initial";
    let mut context = BrowserContext::new(context_id.to_owned());
    context.set_active_target_id(target_id);
    context.mark_active_initial_document_page_build_pending();
    let mut connection = CdpConnection::default();
    connection.insert_browser_context(context);
    let owner = BrowserPageOwnerKey::new(context_id, target_id);
    connection
        .register_target_initial_empty_document_for_test(
            &owner,
            BrowserInitialEmptyDocumentSeed::new("about:blank"),
        )
        .expect("registered Target should accept initial Document metadata");
    let occupied_page = connection
        .load_page_via_runtime_async("data:text/html,<title>occupied</title>")
        .await
        .expect("occupied Page fixture should load");
    connection
        .browser_context
        .as_mut()
        .expect("selected BrowserContext")
        .active_target
        .runtime_slot
        .set_loaded_page_for_test(occupied_page);
    let residence = connection
        .browser_host_state
        .navigation_owner()
        .page_residence_handle(&owner)
        .expect("authoritative Page residence");
    let generation_before = residence.generation();
    let permit = connection
        .browser_host_state
        .navigation_owner()
        .prepare_initial_document_page_materialization(&owner)
        .expect("initial Document should remain materializable in Core");

    let error = connection
        .stage_physical_page_residence_projection(&permit, true)
        .err()
        .expect("occupied physical slot must reject staging");

    assert_eq!(
        error,
        PageResidenceProjectionError::InitialDocumentPageAlreadyPresent {
            browser_context_id: context_id.to_owned(),
            target_id: target_id.to_owned(),
        }
    );
    assert_eq!(residence.generation(), generation_before);
    assert!(
        !connection
            .browser_host_state
            .navigation_owner()
            .target_initial_empty_document(&owner)
            .expect("initial Document metadata must remain registered")
            .materialized(),
        "physical rejection must happen before Core marks the Document materialized"
    );
    assert!(
        connection
            .browser_context
            .as_ref()
            .is_some_and(BrowserContext::has_loaded_page),
        "rejected staging must restore the occupied physical BrowserContext"
    );
}

#[tokio::test]
async fn background_failed_discard_restores_exact_target_index_after_projection() {
    let mut context = BrowserContext::new("CTX-background-discard".to_owned());
    context.set_active_target_id("TID-active");
    context.stage_background_target(
        "TID-before".to_owned(),
        Some("SID-before".to_owned()),
        "about:blank#before".to_owned(),
        None,
        None,
    );
    context.stage_background_target(
        "TID-selected".to_owned(),
        Some("SID-selected".to_owned()),
        "about:blank#selected".to_owned(),
        None,
        None,
    );
    context.stage_background_target(
        "TID-after".to_owned(),
        Some("SID-after".to_owned()),
        "about:blank#after".to_owned(),
        None,
        None,
    );
    let mut connection = CdpConnection::default();
    connection.insert_browser_context(context);
    let residence = connection
        .target_page_residence_handle_for_session(Some("SID-selected"))
        .expect("background Page residence");
    let generation_before = residence.generation();
    let final_url = Url::parse("https://failed.example/background").expect("failure URL");
    let navigation = connection
        .start_document_navigation_for_session_owner(
            Some("SID-selected"),
            "LOADER-background-failed".to_owned(),
        )
        .expect("background Target should start failed navigation");
    let admission_sequence = connection
        .last_projected_browser_fact_sequence_for_test()
        .expect("navigation admission should claim its exact Browser fact");

    assert_eq!(
        connection
            .discard_loaded_page_after_failed_navigation_for_session_owner_async(
                Some("SID-selected"),
                &navigation,
                BrowserNavigationFailure::Network {
                    error_text: "net::ERR_FAILED".to_owned(),
                },
                &final_url,
            )
            .await,
        Ok(Some(()))
    );

    let context = connection
        .browser_context
        .as_ref()
        .expect("selected BrowserContext must be restored");
    assert_eq!(
        context
            .background_targets
            .iter()
            .map(|target| target.target_id())
            .collect::<Vec<_>>(),
        ["TID-before", "TID-selected", "TID-after"],
        "same-turn transaction must restore the exact background index"
    );
    assert_eq!(
        context
            .background_target("TID-selected")
            .expect("projected background Target")
            .target_url(),
        final_url.as_str()
    );
    assert_eq!(residence.generation(), generation_before + 1);
    assert_eq!(
        connection.last_projected_browser_fact_sequence_for_test(),
        Some(admission_sequence + 1),
        "failed Page discard must consume its exact NavigationFailed fact after admission"
    );
    connection
        .validate_browser_target_topology_projection()
        .expect("Core and physical topology must remain exact");
}
