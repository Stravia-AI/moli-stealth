use moli_core::browser_host::{
    BrowserContextHandle, BrowserContextRegistrationMetadata, BrowserContextRegistryError,
    BrowserFact, BrowserPageOwnerKey,
};
use std::sync::Arc;

use super::{BrowserContextProjectionError, CdpConnection};
use crate::conn::BrowserContext;

fn connection_with_two_contexts() -> CdpConnection {
    let mut connection = CdpConnection::new();
    let mut first = connection.new_browser_context("context-a".to_owned());
    first.set_active_target_id("target-a");
    connection
        .try_insert_browser_context(first)
        .expect("first BrowserContext should register");
    let mut second = connection.new_browser_context("context-b".to_owned());
    second.set_active_target_id("target-b");
    connection
        .try_insert_browser_context(second)
        .expect("second BrowserContext should register");
    connection
}

fn connection_with_two_empty_contexts() -> CdpConnection {
    let mut connection = CdpConnection::new();
    connection
        .try_insert_browser_context(BrowserContext::new("context-a".to_owned()))
        .expect("first BrowserContext should register");
    connection
        .try_insert_browser_context(BrowserContext::new("context-b".to_owned()))
        .expect("second BrowserContext should register");
    connection
}

fn inject_physical_engine_owner_divergence(connection: &mut CdpConnection) -> BrowserPageOwnerKey {
    connection
        .browser_context
        .as_mut()
        .expect("selected physical BrowserContext")
        .set_active_target_id("physical-only-target");
    BrowserPageOwnerKey::new("context-a", "physical-only-target")
}

fn assert_original_physical_context_order(connection: &CdpConnection) {
    assert_eq!(
        connection
            .browser_context
            .as_ref()
            .map(|context| context.id.as_str()),
        Some("context-a")
    );
    assert_eq!(
        connection
            .inactive_browser_contexts
            .iter()
            .map(|context| context.id.as_str())
            .collect::<Vec<_>>(),
        vec!["context-b"]
    );
}

#[test]
fn default_target_creation_metadata_commits_with_browser_context_registration() {
    let mut connection = CdpConnection::new();
    let owner = BrowserPageOwnerKey::new(
        connection.default_browser_context_id(),
        connection.default_target_id(),
    );

    connection.install_default_browser_target();

    let initial_document = connection
        .target_initial_empty_document_for_owner(&owner)
        .expect("default Target registration must install initial Document metadata");
    assert_eq!(initial_document.initial_url(), "about:blank");
    assert!(initial_document.is_on_initial_empty_document());
    assert_eq!(connection.registered_browser_context_count(), 1);

    let creation = connection
        .browser_fact_snapshot_for_test()
        .into_iter()
        .find(|fact| {
            fact.target_id().as_str() == owner.target_id()
                && matches!(fact.fact(), BrowserFact::TargetCreated)
        })
        .expect("bootstrap Target creation should remain in the Browser journal");
    assert_eq!(
        creation.page_residence().browser_context_id(),
        owner.browser_context_id()
    );
    assert_eq!(
        connection.last_projected_browser_fact_sequence_for_test(),
        Some(creation.sequence().get()),
        "bootstrap projection must consume the occurrence before later discovery resnapshot"
    );
}

#[test]
fn browser_context_registration_moves_all_target_session_storage_into_core() {
    let mut connection = CdpConnection::new();
    let mut browser_context = connection.new_browser_context("context-storage".to_owned());
    browser_context.set_active_target_id("target-storage");
    browser_context.stage_background_target(
        "target-background",
        None,
        "about:blank#background".to_owned(),
        None,
        None,
    );
    let candidate_store = browser_context
        .target_session_storage_store("target-storage")
        .expect("candidate Target namespace");
    let background_candidate_store = browser_context
        .target_session_storage_store("target-background")
        .expect("candidate background Target namespace");

    connection
        .try_insert_browser_context(browser_context)
        .expect("BrowserContext should register its Target namespace");

    let browser_context = connection
        .browser_context_by_id("context-storage")
        .expect("physical BrowserContext projection");
    let projected_store = browser_context
        .target_session_storage_store("target-storage")
        .expect("projected Target namespace");
    let background_projected_store = browser_context
        .target_session_storage_store("target-background")
        .expect("projected background Target namespace");
    let authoritative = connection
        .browser_host_state
        .navigation_owner()
        .target_session_storage_access("context-storage", "target-storage")
        .expect("Core Target registry must own the namespace association");
    let background_authoritative = connection
        .browser_host_state
        .navigation_owner()
        .target_session_storage_access("context-storage", "target-background")
        .expect("Core Target registry must own the background namespace association");
    assert!(authoritative.is_live());
    assert_eq!(
        authoritative.target_handle(),
        browser_context
            .active_target_handle()
            .expect("active exact Target handle")
    );
    assert!(Arc::ptr_eq(authoritative.store(), &candidate_store));
    assert!(Arc::ptr_eq(&projected_store, authoritative.store()));
    assert!(background_authoritative.is_live());
    assert!(Arc::ptr_eq(
        background_authoritative.store(),
        &background_candidate_store
    ));
    assert!(Arc::ptr_eq(
        &background_projected_store,
        background_authoritative.store()
    ));
}

#[test]
fn registration_core_rejection_does_not_publish_candidate_projection() {
    let mut connection = connection_with_two_contexts();

    let error = connection
        .register_browser_context_projection_with_metadata(
            BrowserContext::new("context-a".to_owned()),
            BrowserContextRegistrationMetadata::default(),
        )
        .expect_err("duplicate Core BrowserContext must be a typed rejection");

    assert!(matches!(
        error,
        BrowserContextProjectionError::Core(BrowserContextRegistryError::DuplicateBrowserContext(
            _
        ))
    ));
    assert_original_physical_context_order(&connection);
    assert_eq!(connection.registered_browser_context_count(), 2);
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_browser_context_id(),
        Some("context-a")
    );
}

#[test]
fn physical_context_miss_rejects_activation_before_core_mutation() {
    let mut connection = connection_with_two_contexts();
    let missing = connection.inactive_browser_contexts.remove(0);
    let selected_engine_owner = connection
        .browser_host_state
        .navigation_owner()
        .selected_target_engine_owner()
        .cloned();

    let error = connection
        .activate_browser_context_projection_by_id("context-b")
        .expect_err("missing physical BrowserContext must be a typed rejection");

    assert_eq!(
        error,
        BrowserContextProjectionError::PhysicalContextCountMismatch {
            authoritative: 2,
            projected: 1,
        }
    );
    assert_eq!(missing.id, "context-b");
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_browser_context_id(),
        Some("context-a")
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_target_engine_owner(),
        selected_engine_owner.as_ref()
    );
    assert_eq!(
        connection
            .browser_context
            .as_ref()
            .map(|context| context.id.as_str()),
        Some("context-a")
    );
    assert!(connection.inactive_browser_contexts.is_empty());
}

#[test]
fn activation_core_rejection_restores_exact_physical_slots() {
    let mut connection = connection_with_two_empty_contexts();
    let divergent = inject_physical_engine_owner_divergence(&mut connection);
    let divergent_renderer_owner = connection
        .browser_host_state
        .navigation_owner()
        .active_renderer_owner_id_for_diagnostics();

    let error = connection
        .activate_browser_context_projection_by_id("context-b")
        .expect_err("engine owner divergence must reject BrowserContext activation");

    assert!(matches!(
        error,
        BrowserContextProjectionError::Core(BrowserContextRegistryError::EngineOwnerMismatch(_))
    ));
    assert_original_physical_context_order(&connection);
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_browser_context_id(),
        Some("context-a")
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_target_engine_owner(),
        None,
        "typed rejection must not bind the physical-only owner into Core"
    );
    assert_eq!(
        connection
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id),
        Some(divergent.target_id())
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .active_renderer_owner_id_for_diagnostics(),
        divergent_renderer_owner
    );
}

#[test]
fn selected_removal_core_rejection_restores_exact_physical_slots() {
    let mut connection = connection_with_two_empty_contexts();
    let divergent = inject_physical_engine_owner_divergence(&mut connection);
    let divergent_renderer_owner = connection
        .browser_host_state
        .navigation_owner()
        .active_renderer_owner_id_for_diagnostics();

    let error = match connection.remove_browser_context_projection_by_id("context-a") {
        Err(error) => error,
        Ok(_) => panic!("engine owner divergence must reject BrowserContext removal"),
    };

    assert!(matches!(
        error,
        BrowserContextProjectionError::Core(BrowserContextRegistryError::EngineOwnerMismatch(_))
    ));
    assert_original_physical_context_order(&connection);
    assert_eq!(connection.registered_browser_context_count(), 2);
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_browser_context_id(),
        Some("context-a")
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_target_engine_owner(),
        None,
        "typed rejection must not bind the physical-only owner into Core"
    );
    assert_eq!(
        connection
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id),
        Some(divergent.target_id())
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .active_renderer_owner_id_for_diagnostics(),
        divergent_renderer_owner
    );
}

#[test]
fn last_selected_removal_core_rejection_restores_physical_selection() {
    let mut connection = CdpConnection::new();
    connection
        .try_insert_browser_context(BrowserContext::new("context-a".to_owned()))
        .expect("BrowserContext should register");
    let divergent = inject_physical_engine_owner_divergence(&mut connection);
    let selected_renderer_owner = connection
        .browser_host_state
        .navigation_owner()
        .active_renderer_owner_id_for_diagnostics();

    let error = match connection.remove_browser_context_projection_by_id("context-a") {
        Err(error) => error,
        Ok(_) => panic!("engine owner divergence must reject last-context removal"),
    };

    assert!(matches!(
        error,
        BrowserContextProjectionError::Core(BrowserContextRegistryError::EngineOwnerMismatch(_))
    ));
    assert_eq!(
        connection
            .browser_context
            .as_ref()
            .map(|context| context.id.as_str()),
        Some("context-a")
    );
    assert_eq!(
        connection
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id),
        Some(divergent.target_id())
    );
    assert!(connection.inactive_browser_contexts.is_empty());
    assert_eq!(connection.registered_browser_context_count(), 1);
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_browser_context_id(),
        Some("context-a")
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_target_engine_owner(),
        None
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .active_renderer_owner_id_for_diagnostics(),
        selected_renderer_owner
    );
}

#[test]
fn inactive_removal_core_rejection_restores_exact_vector_index() {
    let mut connection = connection_with_two_empty_contexts();
    connection
        .try_insert_browser_context(BrowserContext::new("context-c".to_owned()))
        .expect("third BrowserContext should register");
    let divergent = inject_physical_engine_owner_divergence(&mut connection);
    let selected_renderer_owner = connection
        .browser_host_state
        .navigation_owner()
        .active_renderer_owner_id_for_diagnostics();

    let error = match connection.remove_browser_context_projection_by_id("context-b") {
        Err(error) => error,
        Ok(_) => panic!("engine owner divergence must reject inactive-context removal"),
    };

    assert!(matches!(
        error,
        BrowserContextProjectionError::Core(BrowserContextRegistryError::EngineOwnerMismatch(_))
    ));
    assert_eq!(
        connection
            .browser_context
            .as_ref()
            .map(|context| context.id.as_str()),
        Some("context-a")
    );
    assert_eq!(
        connection
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id),
        Some(divergent.target_id())
    );
    assert_eq!(
        connection
            .inactive_browser_contexts
            .iter()
            .map(|context| context.id.as_str())
            .collect::<Vec<_>>(),
        vec!["context-b", "context-c"],
        "typed rejection must restore the removed inactive payload at its exact index"
    );
    assert_eq!(connection.registered_browser_context_count(), 3);
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_browser_context_id(),
        Some("context-a")
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_target_engine_owner(),
        None
    );
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .active_renderer_owner_id_for_diagnostics(),
        selected_renderer_owner
    );
}

#[test]
fn same_public_id_wrong_context_handle_rejects_projection_before_core_mutation() {
    let mut connection = connection_with_two_empty_contexts();
    let wrong = BrowserContextHandle::staged("context-a");
    let authoritative = connection
        .browser_context
        .as_mut()
        .expect("selected physical BrowserContext")
        .replace_browser_context_handle(wrong.clone());

    let error = connection
        .activate_browser_context_projection_by_id("context-b")
        .expect_err("same public id must not make the wrong Context instance authoritative");

    assert_eq!(
        error,
        BrowserContextProjectionError::Core(
            BrowserContextRegistryError::BrowserContextHandleProjectionMismatch(
                moli_core::browser_host::BrowserContextId::new("context-a")
            )
        )
    );
    assert!(authoritative.is_live());
    assert!(!wrong.is_live());
    assert_eq!(
        connection
            .browser_host_state
            .navigation_owner()
            .selected_browser_context_id(),
        Some("context-a")
    );
    assert_original_physical_context_order(&connection);
}
