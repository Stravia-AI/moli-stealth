use moli_core::browser_host::{BrowserFact, BrowserTargetCreationMetadata};
use std::sync::Arc;

use crate::conn::{
    BackgroundTarget, BrowserContext, BrowserTargetCreatedFactProjection, CdpConnection,
};

use super::TargetProjectionError;

fn connection_with_active_target() -> CdpConnection {
    let mut connection = CdpConnection::new();
    let mut browser_context = BrowserContext::new("context-1".to_owned());
    browser_context.set_active_target_id("target-a");
    connection.insert_browser_context(browser_context);
    connection
}

fn register_background_target(connection: &mut CdpConnection, target_id: &str) {
    let target = BackgroundTarget::with_url(
        target_id.to_owned(),
        None,
        format!("about:blank#{target_id}"),
    );
    connection
        .register_background_target_projection(
            "context-1",
            target_id,
            move |browser_context, target_handle, page_residence, session_storage_access| {
                let mut target = target;
                target.replace_target_handle(target_handle);
                target.replace_page_residence_handle(page_residence);
                target.bind_session_storage_access(session_storage_access);
                browser_context.background_targets.push(target);
            },
        )
        .expect("background Target projection should register");
}

fn register_background_target_and_take_fact(
    connection: &mut CdpConnection,
    target_id: &str,
) -> BrowserTargetCreatedFactProjection {
    let target = BackgroundTarget::with_url(
        target_id.to_owned(),
        None,
        format!("about:blank#{target_id}"),
    );
    connection
        .register_background_target_with_creation_metadata_projection(
            "context-1",
            target_id,
            BrowserTargetCreationMetadata::default(),
            move |browser_context, target_handle, page_residence, session_storage_access| {
                let mut target = target;
                target.replace_target_handle(target_handle);
                target.replace_page_residence_handle(page_residence);
                target.bind_session_storage_access(session_storage_access);
                browser_context.background_targets.push(target);
            },
        )
        .expect("background Target projection should register")
        .into_browser_fact()
        .expect("top-level Target registration should claim its exact Browser fact")
}

#[test]
fn projected_target_registration_claims_occurrence_without_discovery_subscription() {
    let mut connection = connection_with_active_target();
    assert!(!connection.has_any_target_discovery());

    let created = register_background_target_and_take_fact(&mut connection, "target-b");

    assert!(matches!(
        created.envelope().fact(),
        BrowserFact::TargetCreated
    ));
    assert_eq!(created.envelope().target_id().as_str(), "target-b");
    assert!(connection.target_created_fact_matches_current_target(&created));
    assert_eq!(
        connection.last_projected_browser_fact_sequence_for_test(),
        Some(created.envelope().sequence().get())
    );
}

#[test]
fn reused_public_target_id_cannot_project_predecessor_creation_fact() {
    let mut connection = connection_with_active_target();
    let predecessor = register_background_target_and_take_fact(&mut connection, "target-b");

    connection
        .rollback_staged_background_target_projection("context-1", "target-b")
        .expect("staged predecessor should retire");
    let successor = register_background_target_and_take_fact(&mut connection, "target-b");

    assert!(!connection.target_created_fact_matches_current_target(&predecessor));
    assert!(connection.target_created_fact_matches_current_target(&successor));
    assert!(
        !predecessor
            .envelope()
            .page_residence()
            .same_residence_instance(successor.envelope().page_residence())
    );
}

#[test]
fn reused_public_target_id_cannot_rebind_predecessor_session_storage() {
    let mut connection = connection_with_active_target();
    register_background_target(&mut connection, "target-b");
    let predecessor = connection
        .browser_host_state
        .navigation_owner()
        .target_session_storage_access("context-1", "target-b")
        .expect("first Target namespace access");
    let predecessor_store = connection
        .browser_context_by_id("context-1")
        .and_then(|context| context.background_target("target-b"))
        .expect("first physical Target")
        .session_storage_store()
        .clone();
    assert!(Arc::ptr_eq(predecessor.store(), &predecessor_store));

    connection
        .rollback_staged_background_target_projection("context-1", "target-b")
        .expect("first Target should retire");
    register_background_target(&mut connection, "target-b");
    let successor = connection
        .browser_host_state
        .navigation_owner()
        .target_session_storage_access("context-1", "target-b")
        .expect("replacement Target namespace access");
    let successor_store = connection
        .browser_context_by_id("context-1")
        .and_then(|context| context.background_target("target-b"))
        .expect("replacement physical Target")
        .session_storage_store()
        .clone();

    assert!(!predecessor.is_live());
    assert!(successor.is_live());
    assert_ne!(predecessor.target_handle(), successor.target_handle());
    assert!(!Arc::ptr_eq(predecessor.store(), successor.store()));
    assert!(Arc::ptr_eq(successor.store(), &successor_store));
}

#[test]
fn registration_and_activation_keep_core_and_physical_topology_exact() {
    let mut connection = connection_with_active_target();
    register_background_target(&mut connection, "target-b");

    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .target_browser_context_id("target-b"),
        Some("context-1")
    );
    let activation = connection
        .activate_target_projection("target-b")
        .expect("registered background Target should activate");

    assert!(activation.synchronize_loaded_page());
    let browser_context = connection
        .browser_context
        .as_ref()
        .expect("selected BrowserContext");
    assert_eq!(browser_context.active_target_id(), Some("target-b"));
    assert_eq!(browser_context.background_targets.len(), 1);
    assert_eq!(
        browser_context.background_targets[0].target_id(),
        "target-a"
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .active_target_id_for_browser_context("context-1"),
        Some("target-b")
    );
    connection
        .validate_browser_target_topology_projection()
        .expect("Core and physical Target topology should remain exact");
}

#[test]
fn active_placeholder_replacement_keeps_one_core_and_physical_target() {
    let mut connection = connection_with_active_target();
    let placeholder = connection
        .browser_context
        .as_ref()
        .and_then(BrowserContext::active_target_handle)
        .expect("placeholder handle")
        .clone();

    connection
        .replace_active_target_projection(
            "target-a",
            "target-bootstrap",
            |browser_context, target_handle, page_residence, session_storage_access| {
                browser_context.bind_new_active_target_registration(
                    target_handle,
                    page_residence,
                    session_storage_access,
                );
            },
        )
        .expect("active placeholder projection should be replaced");

    let browser_context = connection
        .browser_context
        .as_ref()
        .expect("selected BrowserContext");
    assert_eq!(browser_context.active_target_id(), Some("target-bootstrap"));
    assert!(browser_context.background_targets.is_empty());
    assert!(placeholder.is_retired());
    assert!(
        browser_context
            .active_target_handle()
            .is_some_and(moli_core::browser_host::BrowserTargetHandle::is_live)
    );
    assert!(
        !connection
            .browser_host_state
            .navigation_owner()
            .has_target("target-a")
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .target_count(),
        1
    );
    connection
        .validate_browser_target_topology_projection()
        .expect("Core and physical Target topology should remain exact");
}

#[test]
fn staged_background_rollback_removes_core_and_physical_projection_together() {
    let mut connection = connection_with_active_target();
    register_background_target(&mut connection, "target-b");

    let (removed, retired_renderer_page_owner) = connection
        .rollback_staged_background_target_projection("context-1", "target-b")
        .expect("staged background Target should roll back");

    assert!(retired_renderer_page_owner.is_none());
    assert_eq!(removed.target_id(), "target-b");
    assert!(removed.target_handle().is_retired());
    assert!(
        !connection
            .browser_host_state
            .navigation_owner()
            .has_target("target-b")
    );
    assert!(
        connection
            .browser_context
            .as_ref()
            .is_some_and(|context| context.background_targets.is_empty())
    );
    connection
        .validate_browser_target_topology_projection()
        .expect("Core and physical Target topology should remain exact");
}

#[test]
fn same_public_id_wrong_target_handle_cannot_authorize_activation() {
    let mut connection = connection_with_active_target();
    register_background_target(&mut connection, "target-b");
    let authoritative = connection
        .browser_host_state
        .navigation_owner()
        .target_handle("target-b")
        .expect("registered Target handle");
    let wrong_instance = moli_core::browser_host::BrowserTargetHandle::staged("target-b");
    connection
        .browser_context
        .as_mut()
        .and_then(|context| context.background_target_mut("target-b"))
        .expect("physical background Target")
        .replace_target_handle(wrong_instance.clone());

    let error = connection
        .activate_target_projection("target-b")
        .expect_err("wrong Target handle must reject activation");
    assert!(matches!(
        error,
        TargetProjectionError::PhysicalTargetHandleMismatch {
            ref browser_context_id,
            ref target_id,
        } if browser_context_id == "context-1" && target_id == "target-b"
    ));
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .active_target_id_for_browser_context("context-1"),
        Some("target-a")
    );
    assert_eq!(
        connection
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id),
        Some("target-a")
    );
    assert!(authoritative.is_live());
    assert!(!wrong_instance.is_live());
}

#[test]
fn same_target_wrong_page_handle_cannot_authorize_activation() {
    let mut connection = connection_with_active_target();
    register_background_target(&mut connection, "target-b");
    let owner = moli_core::browser_host::BrowserPageOwnerKey::new("context-1", "target-b");
    let authoritative = connection
        .browser_host_state
        .navigation_owner()
        .page_residence_handle(&owner)
        .expect("registered Page residence");
    let wrong_instance = moli_core::browser_host::BrowserPageResidenceHandle::default();
    connection
        .browser_context
        .as_mut()
        .and_then(|context| context.background_target_mut("target-b"))
        .expect("physical background Target")
        .replace_page_residence_handle(wrong_instance.clone());

    let error = connection
        .activate_target_projection("target-b")
        .expect_err("wrong Page residence must reject activation");
    assert!(matches!(
        error,
        TargetProjectionError::PhysicalPageResidenceMismatch {
            ref browser_context_id,
            ref target_id,
        } if browser_context_id == "context-1" && target_id == "target-b"
    ));
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .active_target_id_for_browser_context("context-1"),
        Some("target-a")
    );
    assert_eq!(
        connection
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id),
        Some("target-a")
    );
    assert!(
        connection
            .browser_host_state
            .navigation_owner()
            .page_residence_handle_is_current(&owner, &authoritative)
    );
    assert!(
        !connection
            .browser_host_state
            .navigation_owner()
            .page_residence_handle_is_current(&owner, &wrong_instance)
    );
}

#[test]
fn idempotent_physical_target_id_write_preserves_core_handle() {
    let mut connection = connection_with_active_target();
    let before = connection
        .browser_context
        .as_ref()
        .and_then(BrowserContext::active_target_handle)
        .expect("active Target handle")
        .clone();

    connection
        .browser_context
        .as_mut()
        .expect("selected BrowserContext")
        .set_active_target_id("target-a");

    let after = connection
        .browser_context
        .as_ref()
        .and_then(BrowserContext::active_target_handle)
        .expect("active Target handle");
    assert_eq!(after, &before);
    assert!(
        connection
            .browser_host_state
            .navigation_owner()
            .target_handle_is_current(after)
    );
}

#[test]
fn physical_only_target_mutation_is_rejected_instead_of_becoming_authority() {
    let mut connection = connection_with_active_target();
    connection
        .browser_context
        .as_mut()
        .expect("selected BrowserContext")
        .background_targets
        .push(BackgroundTarget::with_url(
            "rogue-target".to_owned(),
            None,
            "about:blank#rogue".to_owned(),
        ));

    let target = BackgroundTarget::with_url(
        "target-b".to_owned(),
        None,
        "about:blank#target-b".to_owned(),
    );
    let error = connection
        .register_background_target_projection(
            "context-1",
            "target-b",
            move |browser_context, target_handle, page_residence, session_storage_access| {
                let mut target = target;
                target.replace_target_handle(target_handle);
                target.replace_page_residence_handle(page_residence);
                target.bind_session_storage_access(session_storage_access);
                browser_context.background_targets.push(target);
            },
        )
        .expect_err("physical-only Target mutation must reject registration");

    assert!(matches!(
        error,
        TargetProjectionError::PhysicalContextTargetCountMismatch {
            ref browser_context_id,
            authoritative: 1,
            projected: 2,
        } if browser_context_id == "context-1"
    ));
    assert!(
        !connection
            .browser_host_state
            .navigation_owner()
            .has_target("target-b")
    );
    assert!(
        !connection
            .browser_host_state
            .navigation_owner()
            .has_target("rogue-target")
    );
}

#[test]
fn core_registration_rejection_restores_the_exact_physical_context_slot() {
    let mut connection = connection_with_active_target();
    register_background_target(&mut connection, "target-b");
    let active_handle = connection
        .browser_context
        .as_ref()
        .and_then(BrowserContext::active_target_handle)
        .expect("active Target handle")
        .clone();
    let background_handle = connection
        .browser_context
        .as_ref()
        .and_then(|context| context.background_target("target-b"))
        .expect("background Target")
        .target_handle()
        .clone();

    let error = connection
        .register_background_target_projection(
            "context-1",
            "target-b",
            |_browser_context, _target_handle, _page_residence, _session_storage_access| {},
        )
        .expect_err("duplicate Target registration must be rejected by Core");

    assert!(matches!(
        error,
        TargetProjectionError::Core(
            moli_core::browser_host::BrowserTargetRegistryError::DuplicateTarget(ref id)
        ) if id.as_str() == "target-b"
    ));
    let browser_context = connection
        .browser_context
        .as_ref()
        .expect("selected BrowserContext must be restored");
    assert_eq!(browser_context.active_target_handle(), Some(&active_handle));
    assert_eq!(browser_context.background_targets.len(), 1);
    assert_eq!(
        browser_context.background_targets[0].target_handle(),
        &background_handle
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .target_count(),
        2
    );
    connection
        .validate_browser_target_topology_projection()
        .expect("rejected registration must preserve exact topology");
}

#[test]
fn core_registration_rejection_restores_the_exact_inactive_context_index() {
    let mut connection = connection_with_active_target();
    register_background_target(&mut connection, "target-b");
    for (browser_context_id, target_id) in [("context-2", "target-c"), ("context-3", "target-d")] {
        let mut browser_context = BrowserContext::new(browser_context_id.to_owned());
        browser_context.set_active_target_id(target_id);
        connection.insert_browser_context(browser_context);
    }
    assert_eq!(
        connection
            .inactive_browser_contexts
            .iter()
            .map(|context| context.id.as_str())
            .collect::<Vec<_>>(),
        vec!["context-2", "context-3"]
    );

    let error = connection
        .register_background_target_projection(
            "context-3",
            "target-b",
            |_browser_context, _target_handle, _page_residence, _session_storage_access| {},
        )
        .expect_err("duplicate Target registration must be rejected by Core");

    assert!(matches!(
        error,
        TargetProjectionError::Core(
            moli_core::browser_host::BrowserTargetRegistryError::DuplicateTarget(ref id)
        ) if id.as_str() == "target-b"
    ));
    assert_eq!(
        connection
            .inactive_browser_contexts
            .iter()
            .map(|context| context.id.as_str())
            .collect::<Vec<_>>(),
        vec!["context-2", "context-3"],
        "typed Core rejection must restore the removed inactive Context at its original index"
    );
    connection
        .validate_browser_target_topology_projection()
        .expect("rejected registration must preserve every Context Target topology");
}
