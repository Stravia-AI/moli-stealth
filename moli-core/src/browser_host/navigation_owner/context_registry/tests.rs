use super::*;
use crate::browser_host::{
    BrowserContextHandle, BrowserContextRegistrationMetadata, BrowserFact,
    BrowserInitialEmptyDocumentSeed, BrowserPageResidenceHandle, BrowserTargetCreationMetadata,
    BrowserTargetEngineResidence, BrowserTargetHandle, BrowserTargetSlotProjection,
};

fn engine() -> NavigationEngine {
    NavigationEngine::new_with_page_vm_document_isolate_for_diagnostics()
}

fn projection(
    context_id: Option<&str>,
    disposition: BrowserSelectedTargetEngineDisposition,
) -> BrowserContextSelectionProjection {
    BrowserContextSelectionProjection::new(context_id.map(str::to_owned), disposition)
}

fn topology(context_id: &str, target_id: Option<&str>) -> BrowserTargetTopologyProjection {
    BrowserTargetTopologyProjection::new(
        context_id,
        target_id.map(|target_id| {
            BrowserTargetSlotProjection::new(
                BrowserTargetHandle::staged(target_id),
                BrowserPageResidenceHandle::default(),
            )
        }),
        Vec::<BrowserTargetSlotProjection>::new(),
    )
}

fn register_selected(
    owner: &mut BrowserNavigationOwner,
    context_id: &str,
    target_id: &str,
) -> BrowserPageOwnerKey {
    let key = BrowserPageOwnerKey::new(context_id, target_id);
    let registration = owner
        .register_browser_context(
            context_id,
            topology(context_id, Some(target_id)),
            projection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            engine,
        )
        .expect("first context should register");
    assert!(registration.is_selected());
    key
}

#[test]
fn registration_commits_active_target_creation_metadata_with_topology() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let target_owner = BrowserPageOwnerKey::new("context-a", "target-a");

    owner
        .register_browser_context_with_metadata(
            "context-a",
            BrowserContextRegistrationMetadata::with_active_target_creation(
                BrowserTargetCreationMetadata::with_initial_empty_document(
                    BrowserInitialEmptyDocumentSeed::new("about:blank#bootstrap"),
                ),
            ),
            topology("context-a", Some("target-a")),
            projection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            engine,
        )
        .expect("BrowserContext and active Target metadata should commit together");

    let initial_document = owner
        .target_initial_empty_document(&target_owner)
        .expect("committed Target must expose its creation metadata");
    assert_eq!(initial_document.initial_url(), "about:blank#bootstrap");
    assert!(
        owner
            .target_handle(target_owner.target_id())
            .expect("registered Target handle")
            .is_live()
    );
    let facts = owner.browser_fact_snapshot();
    assert_eq!(facts.len(), 1);
    assert!(matches!(facts[0].fact(), BrowserFact::TargetCreated));
    assert_eq!(facts[0].browser_context_id().as_str(), "context-a");
    assert_eq!(facts[0].target_id().as_str(), "target-a");
    assert_eq!(
        facts[0].page_residence(),
        &owner
            .capture_page_residence("context-a", "target-a")
            .expect("registered Target Page residence")
    );
}

#[test]
fn active_target_creation_metadata_without_active_target_is_rejected_before_mutation() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let mut replacement_created = false;
    let absent_target = BrowserPageOwnerKey::new("context-a", "target-a");

    let error = owner
        .register_browser_context_with_metadata(
            "context-a",
            BrowserContextRegistrationMetadata::with_active_target_creation(
                BrowserTargetCreationMetadata::with_initial_empty_document(
                    BrowserInitialEmptyDocumentSeed::new("about:blank"),
                ),
            ),
            topology("context-a", None),
            projection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            || {
                replacement_created = true;
                engine()
            },
        )
        .expect_err("metadata without an exact active Target must be rejected");

    assert_eq!(
        error,
        BrowserContextRegistryError::ActiveTargetCreationMetadataWithoutActiveTarget(
            BrowserContextId::new("context-a")
        )
    );
    assert!(!replacement_created);
    assert_eq!(owner.browser_context_count(), 0);
    assert_eq!(owner.selected_browser_context_id(), None);
    assert!(owner.target_handle(absent_target.target_id()).is_none());
    assert!(
        owner
            .target_initial_empty_document(&absent_target)
            .is_none()
    );
    assert!(owner.browser_fact_snapshot().is_empty());
}

#[test]
fn session_storage_metadata_for_absent_target_is_rejected_before_mutation() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let mut replacement_created = false;

    let error = owner
        .register_browser_context_with_metadata(
            "context-a",
            BrowserContextRegistrationMetadata::default().with_target_session_storage_store(
                "target-absent",
                crate::network::new_shared_web_storage_store(),
            ),
            topology("context-a", Some("target-a")),
            projection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            || {
                replacement_created = true;
                engine()
            },
        )
        .expect_err("namespace metadata must name an exact projected Target");

    assert_eq!(
        error,
        BrowserContextRegistryError::TargetSessionStorageMetadataWithoutTarget {
            browser_context_id: BrowserContextId::new("context-a"),
            target_id: BrowserTargetId::new("target-absent"),
        }
    );
    assert!(!replacement_created);
    assert_eq!(owner.browser_context_count(), 0);
    assert_eq!(owner.target_count(), 0);
}

#[test]
fn context_registration_publishes_active_then_background_target_occurrences() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let active_page = BrowserPageResidenceHandle::default();
    let background_page = BrowserPageResidenceHandle::default();

    owner
        .register_browser_context(
            "context-a",
            BrowserTargetTopologyProjection::new(
                "context-a",
                Some(BrowserTargetSlotProjection::new(
                    BrowserTargetHandle::staged("target-active"),
                    active_page.clone(),
                )),
                vec![BrowserTargetSlotProjection::new(
                    BrowserTargetHandle::staged("target-background"),
                    background_page.clone(),
                )],
            ),
            projection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            engine,
        )
        .expect("BrowserContext topology should register atomically");

    let facts = owner.browser_fact_snapshot();
    assert_eq!(facts.len(), 2);
    assert!(
        facts
            .iter()
            .all(|fact| matches!(fact.fact(), BrowserFact::TargetCreated))
    );
    assert_eq!(
        facts
            .iter()
            .map(|fact| fact.target_id().as_str())
            .collect::<Vec<_>>(),
        vec!["target-active", "target-background"]
    );
    assert_eq!(
        facts[0].page_residence(),
        &active_page.identity("context-a".to_owned(), Some("target-active".to_owned()))
    );
    assert_eq!(
        facts[1].page_residence(),
        &background_page.identity("context-a".to_owned(), Some("target-background".to_owned()),)
    );
}

#[test]
fn first_registration_selects_and_later_registration_stays_inactive() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let first = register_selected(&mut owner, "context-a", "target-a");
    let mut replacement_created = false;

    let second = owner
        .register_browser_context(
            "context-b",
            topology("context-b", Some("target-b")),
            projection(
                Some("context-a"),
                BrowserSelectedTargetEngineDisposition::Discard(first.clone()),
            ),
            || {
                replacement_created = true;
                engine()
            },
        )
        .expect("second context should register inactive");

    assert!(!second.is_selected());
    assert!(!replacement_created);
    assert_eq!(owner.selected_browser_context_id(), Some("context-a"));
    assert_eq!(owner.selected_target_engine_owner(), Some(&first));
    assert_eq!(owner.browser_context_count(), 2);
}

#[test]
fn activation_atomically_swaps_context_and_exact_engine_owner() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let first = register_selected(&mut owner, "context-a", "target-a");
    owner
        .register_browser_context(
            "context-b",
            topology("context-b", Some("target-b")),
            projection(
                Some("context-a"),
                BrowserSelectedTargetEngineDisposition::Discard(first.clone()),
            ),
            engine,
        )
        .expect("second context should register");
    let second = BrowserPageOwnerKey::new("context-b", "target-b");

    let activation = owner
        .activate_browser_context(
            "context-b",
            projection(
                Some("context-a"),
                BrowserSelectedTargetEngineDisposition::Discard(first),
            ),
            engine,
        )
        .expect("second context should activate");

    assert!(activation.changed());
    assert_eq!(activation.previous_browser_context_id(), "context-a");
    assert_eq!(activation.browser_context_id(), "context-b");
    assert_eq!(owner.selected_browser_context_id(), Some("context-b"));
    assert_eq!(owner.selected_target_engine_owner(), Some(&second));
}

#[test]
fn duplicate_and_unknown_contexts_cannot_mutate_registry() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let first = register_selected(&mut owner, "context-a", "target-a");

    let duplicate = owner.register_browser_context(
        "context-a",
        topology("context-a", None),
        projection(
            Some("context-a"),
            BrowserSelectedTargetEngineDisposition::Discard(first.clone()),
        ),
        engine,
    );
    assert!(matches!(
        duplicate,
        Err(BrowserContextRegistryError::DuplicateBrowserContext(_))
    ));
    let unknown = owner.activate_browser_context(
        "missing",
        projection(
            Some("context-a"),
            BrowserSelectedTargetEngineDisposition::Discard(first.clone()),
        ),
        engine,
    );
    assert!(matches!(
        unknown,
        Err(BrowserContextRegistryError::UnknownBrowserContext(_))
    ));
    assert_eq!(owner.selected_browser_context_id(), Some("context-a"));
    assert_eq!(owner.selected_target_engine_owner(), Some(&first));
    assert_eq!(owner.browser_context_count(), 1);
}

#[test]
fn selected_removal_uses_core_successor_and_purges_removed_context_engines() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let first = register_selected(&mut owner, "context-a", "target-a");
    owner
        .register_browser_context(
            "context-b",
            topology("context-b", Some("target-b")),
            projection(
                Some("context-a"),
                BrowserSelectedTargetEngineDisposition::Retain(first.clone()),
            ),
            engine,
        )
        .expect("second context should register");
    let second = BrowserPageOwnerKey::new("context-b", "target-b");
    owner
        .adopt_target_engine(
            second.clone(),
            BrowserTargetEngineResidence::Retained,
            engine(),
        )
        .expect("successor engine should be retained");
    let successor_renderer_owner = owner
        .retained_renderer_owner_ids_for_diagnostics()
        .next()
        .expect("successor renderer owner");
    let removed_target_handle = owner
        .target_handle("target-a")
        .expect("selected context Target handle");
    let successor_target_handle = owner
        .target_handle("target-b")
        .expect("successor context Target handle");
    let permit = owner
        .prepare_browser_context_removal("context-a")
        .expect("selected removal should prepare");
    assert_eq!(permit.successor_browser_context_id(), Some("context-b"));

    let removal = owner
        .commit_browser_context_removal_with_successor(
            permit,
            projection(
                Some("context-a"),
                BrowserSelectedTargetEngineDisposition::Retain(first),
            ),
            engine,
        )
        .expect("selected removal should commit");

    assert_eq!(removal.selected_browser_context_id(), Some("context-b"));
    assert_eq!(owner.selected_target_engine_owner(), Some(&second));
    assert_eq!(
        owner.active_renderer_owner_id_for_diagnostics(),
        successor_renderer_owner
    );
    assert_eq!(owner.retained_background_engine_count(), 0);
    assert_eq!(owner.browser_context_count(), 1);
    assert!(removed_target_handle.is_retired());
    assert!(successor_target_handle.is_live());
}

#[test]
fn stale_removal_permit_cannot_remove_new_selection() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let first = register_selected(&mut owner, "context-a", "target-a");
    owner
        .register_browser_context(
            "context-b",
            topology("context-b", Some("target-b")),
            projection(
                Some("context-a"),
                BrowserSelectedTargetEngineDisposition::Discard(first.clone()),
            ),
            engine,
        )
        .expect("second context should register");
    let permit = owner
        .prepare_browser_context_removal("context-a")
        .expect("removal should prepare");
    owner
        .activate_browser_context(
            "context-b",
            projection(
                Some("context-a"),
                BrowserSelectedTargetEngineDisposition::Discard(first),
            ),
            engine,
        )
        .expect("second context should activate");
    let second = BrowserPageOwnerKey::new("context-b", "target-b");

    let error = owner
        .commit_browser_context_removal_with_successor(
            permit,
            projection(
                Some("context-b"),
                BrowserSelectedTargetEngineDisposition::Discard(second.clone()),
            ),
            engine,
        )
        .expect_err("stale removal must be rejected");

    assert!(matches!(
        error,
        BrowserContextRegistryError::StaleRemovalPermit { .. }
    ));
    assert_eq!(owner.selected_browser_context_id(), Some("context-b"));
    assert_eq!(owner.selected_target_engine_owner(), Some(&second));
    assert_eq!(owner.browser_context_count(), 2);
}

#[test]
fn mismatched_physical_projection_is_rejected_before_mutation() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let first = register_selected(&mut owner, "context-a", "target-a");
    let renderer_owner = owner.active_renderer_owner_id_for_diagnostics();

    let error = owner
        .register_browser_context(
            "context-b",
            topology("context-b", None),
            projection(
                Some("wrong-context"),
                BrowserSelectedTargetEngineDisposition::Discard(first.clone()),
            ),
            engine,
        )
        .expect_err("mismatched projection must be rejected");

    assert!(matches!(
        error,
        BrowserContextRegistryError::EngineProjectionContextMismatch { .. }
            | BrowserContextRegistryError::SelectionProjectionMismatch { .. }
    ));
    assert_eq!(owner.selected_browser_context_id(), Some("context-a"));
    assert_eq!(owner.selected_target_engine_owner(), Some(&first));
    assert_eq!(owner.browser_context_count(), 1);
    assert_eq!(
        owner.active_renderer_owner_id_for_diagnostics(),
        renderer_owner
    );
}

#[test]
fn retired_context_target_rejects_removal_before_context_or_engine_mutation() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let first = register_selected(&mut owner, "context-a", "target-a");
    let target_handle = owner
        .target_handle("target-a")
        .expect("registered Target handle");
    let renderer_owner = owner.active_renderer_owner_id_for_diagnostics();
    let permit = owner
        .prepare_browser_context_removal("context-a")
        .expect("single selected BrowserContext removal should prepare");
    assert!(target_handle.reserve_retirement());
    target_handle.commit_retirement_reservation();

    let error = owner
        .commit_browser_context_removal(
            permit,
            projection(
                Some("context-a"),
                BrowserSelectedTargetEngineDisposition::Discard(first.clone()),
            ),
            engine,
        )
        .expect_err("retired Target must reject BrowserContext removal");

    assert_eq!(
        error,
        BrowserContextRegistryError::TargetHandleNotLive(BrowserTargetId::new("target-a"))
    );
    assert_eq!(owner.selected_browser_context_id(), Some("context-a"));
    assert_eq!(owner.selected_target_engine_owner(), Some(&first));
    assert_eq!(
        owner.active_target_id_for_browser_context("context-a"),
        Some("target-a")
    );
    assert_eq!(owner.browser_context_count(), 1);
    assert_eq!(
        owner.active_renderer_owner_id_for_diagnostics(),
        renderer_owner
    );
    assert!(
        owner
            .browser_context_handle("context-a")
            .is_some_and(BrowserContextHandle::is_live),
        "failed removal must roll back the BrowserContext retirement reservation"
    );
}

#[test]
fn exact_context_handle_rejects_same_id_aba_after_removal_and_recreation() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let first_handle = BrowserContextHandle::staged("context-reused");
    let first_owner = BrowserPageOwnerKey::new("context-reused", "target-first");
    owner
        .register_browser_context_with_handle_and_metadata(
            "context-reused",
            first_handle.clone(),
            BrowserContextRegistrationMetadata::default(),
            topology("context-reused", Some("target-first")),
            projection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            engine,
        )
        .expect("first exact BrowserContext should register");
    assert!(first_handle.is_live());

    let permit = owner
        .prepare_browser_context_removal_for_handle(&first_handle)
        .expect("first exact BrowserContext should prepare removal");
    owner
        .commit_browser_context_removal(
            permit,
            projection(
                Some("context-reused"),
                BrowserSelectedTargetEngineDisposition::Discard(first_owner),
            ),
            engine,
        )
        .expect("first exact BrowserContext should be removed");
    assert!(first_handle.is_retired());

    let replacement_handle = BrowserContextHandle::staged("context-reused");
    owner
        .register_browser_context_with_handle_and_metadata(
            "context-reused",
            replacement_handle.clone(),
            BrowserContextRegistrationMetadata::default(),
            topology("context-reused", Some("target-replacement")),
            projection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            engine,
        )
        .expect("same public id should register as a new exact instance");

    let error = owner
        .prepare_browser_context_removal_for_handle(&first_handle)
        .expect_err("retired capability must not authorize the replacement context");
    assert_eq!(
        error,
        BrowserContextRegistryError::BrowserContextHandleProjectionMismatch(BrowserContextId::new(
            "context-reused"
        ))
    );
    assert!(replacement_handle.is_live());
    assert!(owner.browser_context_handle_is_current(&replacement_handle));
}

#[test]
fn registration_rejects_handle_for_another_public_context_before_activation() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let wrong_handle = BrowserContextHandle::staged("context-other");

    let error = owner
        .register_browser_context_with_handle_and_metadata(
            "context-wanted",
            wrong_handle.clone(),
            BrowserContextRegistrationMetadata::default(),
            topology("context-wanted", Some("target-wanted")),
            projection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            engine,
        )
        .expect_err("a handle for another public id must be rejected");

    assert_eq!(
        error,
        BrowserContextRegistryError::BrowserContextHandleIdMismatch {
            expected: BrowserContextId::new("context-wanted"),
            projected: BrowserContextId::new("context-other"),
        }
    );
    assert!(wrong_handle.reserve_activation());
    wrong_handle.rollback_activation_reservation();
    assert_eq!(owner.browser_context_count(), 0);
}

#[test]
fn disposal_reservation_blocks_new_owner_work_and_rolls_back_exactly() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let page_owner = register_selected(&mut owner, "context-dispose", "target-live");
    let handle = owner
        .browser_context_handle("context-dispose")
        .expect("registered Context handle")
        .clone();
    let navigation =
        owner.start_document_navigation(&page_owner, "loader-before-dispose".to_owned());

    let reservation = owner
        .begin_browser_context_disposal(&handle)
        .expect("exact live Context should reserve disposal");
    assert!(!owner.browser_context_accepts_owner_work("context-dispose"));
    assert!(
        owner
            .try_start_document_navigation_with_trace(
                &page_owner,
                "loader-after-dispose".to_owned(),
                None,
            )
            .is_none(),
        "a reserved Context must reject a later Document navigation at Core admission"
    );
    assert!(
        owner
            .prepare_loaded_page_replacement(&page_owner, &navigation)
            .is_none(),
        "a navigation admitted before disposal must not replace the reserved Context's Page"
    );
    assert_eq!(
        owner
            .begin_browser_context_disposal(&handle)
            .expect_err("the exact Context cannot own two disposal reservations"),
        BrowserContextRegistryError::BrowserContextDisposing(BrowserContextId::new(
            "context-dispose"
        ))
    );
    assert_eq!(
        owner
            .activate_browser_context(
                "context-dispose",
                projection(
                    Some("context-dispose"),
                    BrowserSelectedTargetEngineDisposition::Retain(page_owner.clone()),
                ),
                engine,
            )
            .expect_err("ordinary activation must reject a disposing Context"),
        BrowserContextRegistryError::BrowserContextDisposing(BrowserContextId::new(
            "context-dispose"
        ))
    );

    assert!(owner.rollback_browser_context_disposal(reservation));
    assert!(owner.browser_context_accepts_owner_work("context-dispose"));
    assert!(
        owner
            .prepare_loaded_page_replacement(&page_owner, &navigation)
            .is_some(),
        "rollback must restore the exact Context's Page authority"
    );
}

#[test]
fn final_disposal_removal_retires_reservation_and_same_id_does_not_alias() {
    let mut owner = BrowserNavigationOwner::new(engine());
    let page_owner = register_selected(&mut owner, "context-reused", "target-old");
    let old_handle = owner
        .browser_context_handle("context-reused")
        .expect("old Context handle")
        .clone();
    let reservation = owner
        .begin_browser_context_disposal(&old_handle)
        .expect("old Context should reserve disposal");
    let permit = owner
        .prepare_browser_context_removal_for_disposal(&reservation)
        .expect("the exact reservation should authorize terminal removal");
    owner
        .commit_browser_context_removal(
            permit,
            projection(
                Some("context-reused"),
                BrowserSelectedTargetEngineDisposition::Discard(page_owner),
            ),
            engine,
        )
        .expect("terminal disposal removal should commit");
    assert!(old_handle.is_retired());

    let replacement_handle = BrowserContextHandle::staged("context-reused");
    owner
        .register_browser_context_with_handle_and_metadata(
            "context-reused",
            replacement_handle.clone(),
            BrowserContextRegistrationMetadata::default(),
            topology("context-reused", Some("target-new")),
            projection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            engine,
        )
        .expect("same public id should be reusable after exact disposal");
    assert_eq!(
        owner
            .begin_browser_context_disposal(&old_handle)
            .expect_err("the retired capability cannot reserve its replacement"),
        BrowserContextRegistryError::BrowserContextHandleProjectionMismatch(BrowserContextId::new(
            "context-reused"
        ))
    );
    assert!(owner.browser_context_handle_is_current(&replacement_handle));
    assert!(owner.browser_context_accepts_owner_work("context-reused"));
}
