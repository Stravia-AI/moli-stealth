use crate::{
    browser_host::{
        BrowserContextSelectionProjection, BrowserSelectedTargetEngineDisposition,
        BrowserTargetHandle, BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
    },
    runtime::NavigationEngine,
};

use super::*;

fn slot(
    target_id: &str,
    page_residence: BrowserPageResidenceHandle,
) -> BrowserTargetSlotProjection {
    BrowserTargetSlotProjection::new(BrowserTargetHandle::staged(target_id), page_residence)
}

fn topology(
    context_id: &str,
    active: Option<BrowserTargetSlotProjection>,
) -> BrowserTargetTopologyProjection {
    BrowserTargetTopologyProjection::new(
        context_id,
        active,
        Vec::<BrowserTargetSlotProjection>::new(),
    )
}

fn register_target(
    owner: &mut BrowserNavigationOwner,
    context_id: &str,
    target_id: &str,
) -> BrowserPageResidenceHandle {
    let page_residence = BrowserPageResidenceHandle::default();
    owner
        .register_browser_context(
            context_id,
            topology(context_id, Some(slot(target_id, page_residence.clone()))),
            BrowserContextSelectionProjection::new(
                None,
                BrowserSelectedTargetEngineDisposition::Unbound,
            ),
            NavigationEngine::new,
        )
        .expect("test Target topology should register");
    page_residence
}

#[test]
fn staged_target_registration_is_hidden_until_exact_commit() {
    let mut registry = BrowserPageResidenceRegistry;
    let mut runtimes = BrowserTargetRuntimeRegistry::default();
    let owner = BrowserPageOwnerKey::new("context-1", "target-1");
    let registration = registry
        .begin_target_registration(&mut runtimes, owner.clone())
        .expect("fresh Page residence should stage");
    let handle = registration.handle().clone();

    assert!(registry.identity(&runtimes, &owner).is_none());
    assert!(registry.handle_for_target(&runtimes, &owner).is_none());
    assert_eq!(
        registry.live_handle(&runtimes, &owner),
        Err(BrowserPageResidenceRegistryError::TargetNotLive(
            owner.clone()
        ))
    );

    let committed = registry.commit_target_registration(&mut runtimes, registration);
    assert!(committed.same_instance(&handle));
    assert!(registry.handle_is_current(&runtimes, &owner, &handle));
}

#[test]
fn target_registration_rollback_removes_only_its_staged_residence() {
    let mut registry = BrowserPageResidenceRegistry;
    let mut runtimes = BrowserTargetRuntimeRegistry::default();
    let owner = BrowserPageOwnerKey::new("context-1", "target-1");
    let first = registry
        .begin_target_registration(&mut runtimes, owner.clone())
        .expect("fresh Page residence should stage");
    let first_handle = first.handle().clone();

    assert!(registry.rollback_target_registration(&mut runtimes, first));
    assert!(!runtimes.entries.contains_key(&owner));

    let second = registry
        .begin_target_registration(&mut runtimes, owner.clone())
        .expect("rolled-back owner should stage again");
    let second_handle = second.handle().clone();
    assert!(!first_handle.same_instance(&second_handle));
    registry.commit_target_registration(&mut runtimes, second);
    assert!(registry.handle_is_current(&runtimes, &owner, &second_handle));
}

#[test]
fn context_registration_stages_every_page_and_rolls_back_exactly() {
    let mut registry = BrowserPageResidenceRegistry;
    let mut runtimes = BrowserTargetRuntimeRegistry::default();
    let active_page = BrowserPageResidenceHandle::default();
    let background_page = BrowserPageResidenceHandle::default();
    let projection = BrowserTargetTopologyProjection::new(
        "context-1",
        Some(slot("target-a", active_page.clone())),
        vec![slot("target-b", background_page.clone())],
    );
    let active_owner = BrowserPageOwnerKey::new("context-1", "target-a");
    let background_owner = BrowserPageOwnerKey::new("context-1", "target-b");

    let registration = registry
        .begin_context_registration(
            &mut runtimes,
            &BrowserContextId::new("context-1"),
            &projection,
        )
        .expect("fresh context Page residences should stage");
    assert!(registry.identity(&runtimes, &active_owner).is_none());
    assert!(registry.identity(&runtimes, &background_owner).is_none());

    assert!(registry.rollback_context_registration(&mut runtimes, registration));
    assert!(!runtimes.entries.contains_key(&active_owner));
    assert!(!runtimes.entries.contains_key(&background_owner));
}

#[test]
fn context_registration_rejects_a_page_capability_live_in_another_context() {
    let mut registry = BrowserPageResidenceRegistry;
    let mut runtimes = BrowserTargetRuntimeRegistry::default();
    let shared = BrowserPageResidenceHandle::default();
    let first_owner = BrowserPageOwnerKey::new("context-a", "target-a");
    let first_projection = topology("context-a", Some(slot("target-a", shared.clone())));
    let first = registry
        .begin_context_registration(
            &mut runtimes,
            &BrowserContextId::new("context-a"),
            &first_projection,
        )
        .expect("first context should stage");
    registry.commit_context_registration(&mut runtimes, first);

    let second_owner = BrowserPageOwnerKey::new("context-b", "target-b");
    let second_projection = topology("context-b", Some(slot("target-b", shared.clone())));
    let error = registry
        .begin_context_registration(
            &mut runtimes,
            &BrowserContextId::new("context-b"),
            &second_projection,
        )
        .expect_err("one physical Page capability cannot authorize two Targets");

    assert_eq!(
        error,
        BrowserPageResidenceRegistryError::DuplicateProjectedHandle {
            first: first_owner.clone(),
            duplicate: second_owner.clone(),
        }
    );
    assert!(registry.handle_is_current(&runtimes, &first_owner, &shared));
    assert!(!runtimes.entries.contains_key(&second_owner));
}

#[test]
fn outstanding_page_stage_rejects_new_target_without_leaking_target_authority() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    owner
        .register_browser_context(
            "context-1",
            topology("context-1", None),
            BrowserContextSelectionProjection::new(
                None,
                BrowserSelectedTargetEngineDisposition::Unbound,
            ),
            NavigationEngine::new,
        )
        .expect("empty BrowserContext should register");
    let page_owner = BrowserPageOwnerKey::new("context-1", "target-a");
    let outstanding = owner
        .page_residences
        .begin_target_registration(&mut owner.target_runtimes, page_owner.clone())
        .expect("fixture should hold a staged Page residence");

    let error = owner
        .register_background_target("context-1", "target-a", topology("context-1", None))
        .expect_err("outstanding Page stage must reject duplicate registration");

    assert_eq!(
        error,
        super::super::BrowserTargetRegistryError::PageResidence(
            BrowserPageResidenceRegistryError::DuplicateTarget(page_owner.clone())
        )
    );
    assert!(!owner.has_target("target-a"));
    assert_eq!(owner.target_count(), 0);
    assert!(
        owner
            .capture_page_residence("context-1", "target-a")
            .is_none()
    );
    assert!(
        owner
            .page_residences
            .rollback_target_registration(&mut owner.target_runtimes, outstanding)
    );
}

#[test]
fn outstanding_page_stage_rejects_replacement_without_retiring_source_target() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let source_page = register_target(&mut owner, "context-1", "target-a");
    let source_target = owner
        .target_handle("target-a")
        .expect("source Target handle");
    let source_owner = BrowserPageOwnerKey::new("context-1", "target-a");
    let replacement_owner = BrowserPageOwnerKey::new("context-1", "target-b");
    let renderer_owner = owner.active_renderer_owner_id_for_diagnostics();
    let outstanding = owner
        .page_residences
        .begin_target_registration(&mut owner.target_runtimes, replacement_owner.clone())
        .expect("fixture should hold a staged replacement Page residence");

    let error = owner
        .replace_active_target(
            "context-1",
            "target-a",
            "target-b",
            topology(
                "context-1",
                Some(BrowserTargetSlotProjection::new(
                    source_target.clone(),
                    source_page.clone(),
                )),
            ),
            BrowserContextSelectionProjection::new(
                Some("context-1".to_owned()),
                BrowserSelectedTargetEngineDisposition::Discard(source_owner.clone()),
            ),
            NavigationEngine::new,
        )
        .expect_err("outstanding Page stage must reject active replacement");

    assert_eq!(
        error,
        super::super::BrowserTargetRegistryError::PageResidence(
            BrowserPageResidenceRegistryError::DuplicateTarget(replacement_owner.clone())
        )
    );
    assert!(source_target.is_live());
    assert!(!source_target.is_retired());
    assert_eq!(
        owner.active_target_id_for_browser_context("context-1"),
        Some("target-a")
    );
    assert!(owner.has_target("target-a"));
    assert!(!owner.has_target("target-b"));
    assert_eq!(owner.target_count(), 1);
    assert!(owner.page_residence_handle_is_current(&source_owner, &source_page));
    assert_eq!(
        owner.active_renderer_owner_id_for_diagnostics(),
        renderer_owner
    );
    assert!(
        owner
            .page_residences
            .rollback_target_registration(&mut owner.target_runtimes, outstanding)
    );
}

#[test]
fn duplicate_page_capability_rolls_back_joint_context_target_registration() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let first_page = register_target(&mut owner, "context-a", "target-a");
    let first_target = owner
        .target_handle("target-a")
        .expect("first context Target handle");
    let first_owner = BrowserPageOwnerKey::new("context-a", "target-a");
    let candidate_target = BrowserTargetHandle::staged("target-b");
    let candidate_owner = BrowserPageOwnerKey::new("context-b", "target-b");

    let error = owner
        .register_browser_context(
            "context-b",
            BrowserTargetTopologyProjection::new(
                "context-b",
                Some(BrowserTargetSlotProjection::new(
                    candidate_target.clone(),
                    first_page.clone(),
                )),
                Vec::<BrowserTargetSlotProjection>::new(),
            ),
            BrowserContextSelectionProjection::new(
                Some("context-a".to_owned()),
                BrowserSelectedTargetEngineDisposition::Discard(first_owner.clone()),
            ),
            NavigationEngine::new,
        )
        .expect_err("one Page capability cannot be registered into a second context");

    assert_eq!(
        error,
        super::super::BrowserContextRegistryError::PageResidence(
            BrowserPageResidenceRegistryError::DuplicateProjectedHandle {
                first: first_owner.clone(),
                duplicate: candidate_owner,
            }
        )
    );
    assert!(candidate_target.is_staged());
    assert!(!candidate_target.is_live());
    assert!(first_target.is_live());
    assert_eq!(owner.browser_context_count(), 1);
    assert_eq!(owner.target_count(), 1);
    assert!(!owner.has_browser_context("context-b"));
    assert!(!owner.has_target("target-b"));
    assert!(owner.page_residence_handle_is_current(&first_owner, &first_page));
}

#[test]
fn registered_page_capability_resolves_only_its_current_generation() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let page_residence = register_target(&mut owner, "context-1", "target-1");
    let original = owner
        .capture_page_residence("context-1", "target-1")
        .expect("registered Target should expose its Page residence");

    let key = owner
        .page_owner_key_if_current(&original)
        .expect("captured residence should resolve");
    assert_eq!(key.browser_context_id(), "context-1");
    assert_eq!(key.target_id(), "target-1");

    page_residence.advance_generation_for_test_fixture();
    assert!(owner.page_owner_key_if_current(&original).is_none());
    let successor = owner
        .capture_page_residence("context-1", "target-1")
        .expect("the Target keeps the same Page-slot capability");
    assert_eq!(successor.loaded_page_generation(), 1);
    assert_eq!(owner.page_owner_key_if_current(&successor), Some(key));
}

#[test]
fn stable_slot_resolution_crosses_generation_but_rejects_same_id_recreation() {
    let mut registry = BrowserPageResidenceRegistry;
    let mut runtimes = BrowserTargetRuntimeRegistry::default();
    let key = BrowserPageOwnerKey::new("context-1", "target-1");
    let registration = registry
        .begin_target_registration(&mut runtimes, key.clone())
        .expect("fresh Page slot should stage");
    let first_handle = registration.handle().clone();
    registry.commit_target_registration(&mut runtimes, registration);
    let original = registry
        .identity(&runtimes, &key)
        .expect("live Page slot identity");

    first_handle.advance_generation_for_test_fixture();
    assert!(registry.resolve(&runtimes, &original).is_none());
    assert_eq!(
        registry.resolve_slot(&runtimes, &original),
        Some(key.clone())
    );

    registry.forget_target(&mut runtimes, &key);
    let replacement = registry
        .begin_target_registration(&mut runtimes, key.clone())
        .expect("same public Target id should obtain a new Page slot");
    registry.commit_target_registration(&mut runtimes, replacement);
    assert!(registry.resolve_slot(&runtimes, &original).is_none());
}

#[test]
fn dropping_protocol_clone_does_not_drop_core_page_authority() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let identity = {
        let _protocol_clone = register_target(&mut owner, "context-1", "target-1");
        owner
            .capture_page_residence("context-1", "target-1")
            .expect("registered Target should expose its Page residence")
    };

    assert!(owner.page_owner_key_if_current(&identity).is_some());
}

#[test]
fn arbitrary_same_target_page_handle_is_not_authoritative() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let registered = register_target(&mut owner, "context-1", "target-1");
    let key = BrowserPageOwnerKey::new("context-1", "target-1");
    let other = BrowserPageResidenceHandle::default();

    assert!(owner.page_residence_handle_is_current(&key, &registered));
    assert!(!owner.page_residence_handle_is_current(&key, &other));
}

#[test]
fn page_runtime_discard_retains_registered_slot_capability() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let page_residence = register_target(&mut owner, "context-1", "target-1");
    page_residence.advance_generation_for_test_fixture();

    owner.discard_target_page_runtime("target-1");

    let identity = owner
        .capture_page_residence("context-1", "target-1")
        .expect("failed navigation must not unregister the live Target Page slot");
    assert_eq!(identity.loaded_page_generation(), 1);
}

#[test]
fn target_termination_invalidates_registration_before_slot_drop() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let handle = register_target(&mut owner, "context-1", "target-1");
    let identity = owner
        .capture_page_residence("context-1", "target-1")
        .expect("registered Target should expose its Page residence");

    owner.forget_target("target-1");

    assert!(owner.page_owner_key_if_current(&identity).is_none());
    assert_eq!(handle.generation(), identity.loaded_page_generation());
}
