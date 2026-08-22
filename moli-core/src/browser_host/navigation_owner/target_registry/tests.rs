use super::*;
use std::sync::Arc;

fn slot(handle: &BrowserTargetHandle) -> BrowserTargetSlotProjection {
    BrowserTargetSlotProjection::new(handle.clone(), BrowserPageResidenceHandle::default())
}

fn register_context(
    registry: &mut BrowserTargetRegistry,
    browser_context_id: &str,
    active: Option<&BrowserTargetHandle>,
    background: &[&BrowserTargetHandle],
) {
    let projection = BrowserTargetTopologyProjection::new(
        browser_context_id,
        active.map(slot),
        background.iter().map(|handle| slot(handle)),
    );
    let registration = registry
        .begin_context_registration(projection, HashMap::new())
        .expect("test BrowserContext Target topology should stage");
    registry.commit_context_registration(registration);
}

fn topology_ids(
    registry: &BrowserTargetRegistry,
    browser_context_id: &str,
) -> (Option<String>, Vec<String>) {
    let topology = registry
        .contexts
        .get(&BrowserContextId::new(browser_context_id))
        .expect("test BrowserContext Target topology");
    (
        topology.active.as_ref().map(|id| id.as_str().to_owned()),
        topology
            .background
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect(),
    )
}

#[test]
fn context_registration_publishes_handles_only_at_commit() {
    let mut registry = BrowserTargetRegistry::default();
    let active = BrowserTargetHandle::staged("target-a");
    let background = BrowserTargetHandle::staged("target-b");
    let projection =
        BrowserTargetTopologyProjection::new("context-1", Some(slot(&active)), [slot(&background)]);

    let registration = registry
        .begin_context_registration(projection, HashMap::new())
        .expect("exact staged topology should begin registration");

    assert!(active.is_staged());
    assert!(background.is_staged());
    assert!(!registry.handle_is_current(&active));
    assert!(!registry.handle_is_current(&background));

    registry.commit_context_registration(registration);

    assert!(registry.handle_is_current(&active));
    assert!(registry.handle_is_current(&background));
}

#[test]
fn target_session_storage_follows_exact_registration_and_public_id_reuse() {
    let mut registry = BrowserTargetRegistry::default();
    register_context(&mut registry, "context-1", None, &[]);
    let browser_context_id = BrowserContextId::new("context-1");
    let target_id = BrowserTargetId::new("target-a");
    let owner = BrowserPageOwnerKey::new("context-1", "target-a");
    let predecessor_store = crate::network::new_shared_web_storage_store();

    let registration = registry
        .begin_background_registration(&browser_context_id, &target_id, predecessor_store.clone())
        .expect("first Target namespace should stage atomically");
    let predecessor = registration.session_storage_access().clone();
    assert!(!predecessor.is_live());
    assert!(Arc::ptr_eq(predecessor.store(), &predecessor_store));
    registry.commit_target_registration(registration);

    assert!(predecessor.is_live());
    assert_eq!(
        registry.session_storage_access(&owner),
        Some(predecessor.clone())
    );

    let removal = registry
        .begin_target_removal(&owner)
        .expect("first Target should reserve exact removal");
    assert!(registry.session_storage_access(&owner).is_none());
    assert!(predecessor.is_live());
    registry.commit_target_removal(removal);
    assert!(!predecessor.is_live());
    assert!(predecessor.target_handle().is_retired());

    let successor_store = crate::network::new_shared_web_storage_store();
    let registration = registry
        .begin_background_registration(&browser_context_id, &target_id, successor_store.clone())
        .expect("the retired public id should accept a new exact Target instance");
    let successor = registration.session_storage_access().clone();
    registry.commit_target_registration(registration);

    assert!(successor.is_live());
    assert_ne!(successor.target_handle(), predecessor.target_handle());
    assert!(!Arc::ptr_eq(successor.store(), predecessor.store()));
    assert!(Arc::ptr_eq(successor.store(), &successor_store));
    assert_eq!(registry.session_storage_access(&owner), Some(successor));
}

#[test]
fn target_session_storage_registration_rollback_publishes_no_access() {
    let mut registry = BrowserTargetRegistry::default();
    register_context(&mut registry, "context-1", None, &[]);
    let owner = BrowserPageOwnerKey::new("context-1", "target-a");
    let store = crate::network::new_shared_web_storage_store();

    let registration = registry
        .begin_background_registration(
            &BrowserContextId::new("context-1"),
            &BrowserTargetId::new("target-a"),
            store.clone(),
        )
        .expect("Target namespace should stage");
    let staged = registration.session_storage_access().clone();
    assert!(registry.rollback_target_registration(registration));

    assert!(registry.session_storage_access(&owner).is_none());
    assert!(!staged.is_live());
    assert!(staged.target_handle().is_staged());
    assert!(Arc::ptr_eq(staged.store(), &store));
}

#[test]
fn context_removal_rolls_back_or_retires_target_session_storage_accesses_atomically() {
    let mut registry = BrowserTargetRegistry::default();
    let active = BrowserTargetHandle::staged("target-a");
    let background = BrowserTargetHandle::staged("target-b");
    register_context(&mut registry, "context-1", Some(&active), &[&background]);
    let active_owner = BrowserPageOwnerKey::new("context-1", "target-a");
    let background_owner = BrowserPageOwnerKey::new("context-1", "target-b");
    let active_access = registry
        .session_storage_access(&active_owner)
        .expect("active namespace access")
        .clone();
    let background_access = registry
        .session_storage_access(&background_owner)
        .expect("background namespace access")
        .clone();

    let removal = registry
        .begin_context_removal(&BrowserContextId::new("context-1"))
        .expect("Context removal should stage all Target namespaces");
    assert!(registry.session_storage_access(&active_owner).is_none());
    assert!(registry.session_storage_access(&background_owner).is_none());
    assert!(active_access.is_live());
    assert!(background_access.is_live());
    assert!(registry.rollback_context_removal(removal));
    assert_eq!(
        registry.session_storage_access(&active_owner),
        Some(active_access.clone())
    );
    assert_eq!(
        registry.session_storage_access(&background_owner),
        Some(background_access.clone())
    );

    let removal = registry
        .begin_context_removal(&BrowserContextId::new("context-1"))
        .expect("Context removal should restage after rollback");
    registry.commit_context_removal(removal);
    assert!(!active_access.is_live());
    assert!(!background_access.is_live());
    assert!(active_access.target_handle().is_retired());
    assert!(background_access.target_handle().is_retired());
}

#[test]
fn context_registration_rejects_nonstaged_handle_without_partial_topology() {
    let mut registry = BrowserTargetRegistry::default();
    let active = BrowserTargetHandle::staged("target-a");
    let invalid = BrowserTargetHandle::staged("target-b");
    assert!(invalid.reserve_activation());
    invalid.commit_activation_reservation();
    let projection =
        BrowserTargetTopologyProjection::new("context-1", Some(slot(&active)), [slot(&invalid)]);

    let error = registry
        .begin_context_registration(projection, HashMap::new())
        .expect_err("a live physical handle cannot be registered as staged");

    assert_eq!(
        error,
        BrowserContextRegistryError::TargetHandleNotStaged(BrowserTargetId::new("target-b"))
    );
    assert!(active.is_staged());
    assert!(
        !registry
            .contexts
            .contains_key(&BrowserContextId::new("context-1"))
    );
    assert!(registry.owners.is_empty());
}

#[test]
fn context_removal_missing_reverse_owner_is_typed_and_nonmutating() {
    let mut registry = BrowserTargetRegistry::default();
    let active = BrowserTargetHandle::staged("target-a");
    let background = BrowserTargetHandle::staged("target-b");
    register_context(&mut registry, "context-1", Some(&active), &[&background]);
    registry.owners.remove(&BrowserTargetId::new("target-b"));

    let error = registry
        .begin_context_removal(&BrowserContextId::new("context-1"))
        .expect_err("missing reverse owner must reject context removal");

    assert_eq!(
        error,
        BrowserContextRegistryError::TargetTopologyOwnerMissing {
            browser_context_id: BrowserContextId::new("context-1"),
            target_id: BrowserTargetId::new("target-b"),
        }
    );
    assert_eq!(
        topology_ids(&registry, "context-1"),
        (Some("target-a".to_owned()), vec!["target-b".to_owned()])
    );
    assert!(active.is_live());
}

#[test]
fn context_removal_rejects_retired_handle_without_removing_registry_entries() {
    let mut registry = BrowserTargetRegistry::default();
    let active = BrowserTargetHandle::staged("target-a");
    register_context(&mut registry, "context-1", Some(&active), &[]);
    assert!(active.reserve_retirement());
    active.commit_retirement_reservation();

    let error = registry
        .begin_context_removal(&BrowserContextId::new("context-1"))
        .expect_err("already-retired handle must reject context removal");

    assert_eq!(
        error,
        BrowserContextRegistryError::TargetHandleNotLive(BrowserTargetId::new("target-a"))
    );
    assert_eq!(
        topology_ids(&registry, "context-1"),
        (Some("target-a".to_owned()), Vec::new())
    );
    assert!(
        registry
            .owners
            .contains_key(&BrowserTargetId::new("target-a"))
    );
}

#[test]
fn context_removal_rollback_restores_exact_order_and_live_handles() {
    let mut registry = BrowserTargetRegistry::default();
    let active = BrowserTargetHandle::staged("target-a");
    let first = BrowserTargetHandle::staged("target-b");
    let second = BrowserTargetHandle::staged("target-c");
    register_context(
        &mut registry,
        "context-1",
        Some(&active),
        &[&first, &second],
    );

    let removal = registry
        .begin_context_removal(&BrowserContextId::new("context-1"))
        .expect("exact context removal should stage");
    assert!(
        !registry
            .contexts
            .contains_key(&BrowserContextId::new("context-1"))
    );
    assert!(active.is_live());
    assert!(first.is_live());
    assert!(second.is_live());

    assert!(registry.rollback_context_removal(removal));
    assert_eq!(
        topology_ids(&registry, "context-1"),
        (
            Some("target-a".to_owned()),
            vec!["target-b".to_owned(), "target-c".to_owned()]
        )
    );
    assert!(registry.handle_is_current(&active));
    assert!(registry.handle_is_current(&first));
    assert!(registry.handle_is_current(&second));
}

#[test]
fn active_registration_rollback_restores_previous_topology() {
    let mut registry = BrowserTargetRegistry::default();
    let active = BrowserTargetHandle::staged("target-a");
    let first = BrowserTargetHandle::staged("target-b");
    let second = BrowserTargetHandle::staged("target-c");
    register_context(
        &mut registry,
        "context-1",
        Some(&active),
        &[&first, &second],
    );

    let registration = registry
        .begin_active_registration(
            &BrowserContextId::new("context-1"),
            &BrowserTargetId::new("target-d"),
            crate::network::new_shared_web_storage_store(),
        )
        .expect("new active Target should stage");
    let staged = registration.handle().clone();
    assert_eq!(
        topology_ids(&registry, "context-1"),
        (
            Some("target-d".to_owned()),
            vec![
                "target-b".to_owned(),
                "target-c".to_owned(),
                "target-a".to_owned()
            ]
        )
    );

    assert!(registry.rollback_target_registration(registration));
    assert_eq!(
        topology_ids(&registry, "context-1"),
        (
            Some("target-a".to_owned()),
            vec!["target-b".to_owned(), "target-c".to_owned()]
        )
    );
    assert!(staged.is_staged());
    assert!(registry.handle_is_current(&active));
    assert!(
        !registry
            .owners
            .contains_key(&BrowserTargetId::new("target-d"))
    );
}

#[test]
fn active_replacement_rollback_restores_exact_source_capability() {
    let mut registry = BrowserTargetRegistry::default();
    let active = BrowserTargetHandle::staged("target-a");
    register_context(&mut registry, "context-1", Some(&active), &[]);

    let replacement = registry
        .begin_active_replacement(
            &BrowserContextId::new("context-1"),
            &BrowserTargetId::new("target-a"),
            &BrowserTargetId::new("target-b"),
            crate::network::new_shared_web_storage_store(),
        )
        .expect("exact active replacement should stage");
    let replacement_handle = replacement.replacement_handle().clone();
    assert!(active.is_live());
    assert!(replacement_handle.is_staged());
    assert_eq!(
        topology_ids(&registry, "context-1"),
        (Some("target-b".to_owned()), Vec::new())
    );

    assert!(registry.rollback_active_replacement(replacement));
    assert_eq!(
        topology_ids(&registry, "context-1"),
        (Some("target-a".to_owned()), Vec::new())
    );
    assert!(registry.handle_is_current(&active));
    assert!(replacement_handle.is_staged());
}

#[test]
fn activation_rollback_and_commit_preserve_background_vector_order() {
    let mut registry = BrowserTargetRegistry::default();
    let active = BrowserTargetHandle::staged("target-a");
    let first = BrowserTargetHandle::staged("target-b");
    let second = BrowserTargetHandle::staged("target-c");
    let third = BrowserTargetHandle::staged("target-d");
    register_context(
        &mut registry,
        "context-1",
        Some(&active),
        &[&first, &second, &third],
    );
    let owner = BrowserPageOwnerKey::new("context-1", "target-b");

    let activation = registry
        .begin_activation(&owner)
        .expect("background Target should stage for activation");
    assert!(registry.rollback_activation(activation));
    assert_eq!(
        topology_ids(&registry, "context-1"),
        (
            Some("target-a".to_owned()),
            vec![
                "target-b".to_owned(),
                "target-c".to_owned(),
                "target-d".to_owned()
            ]
        )
    );

    let activation = registry
        .begin_activation(&owner)
        .expect("background Target should stage again");
    assert_eq!(
        registry.commit_activation(activation),
        Some(BrowserTargetId::new("target-a"))
    );
    assert_eq!(
        topology_ids(&registry, "context-1"),
        (
            Some("target-b".to_owned()),
            vec![
                "target-c".to_owned(),
                "target-d".to_owned(),
                "target-a".to_owned()
            ]
        )
    );
}
