use crate::{
    browser_host::{
        BrowserContextSelectionProjection, BrowserFact, BrowserInitialEmptyDocumentCreator,
        BrowserInitialEmptyDocumentSeed, BrowserPageOwnerKey, BrowserPageResidenceHandle,
        BrowserSelectedTargetEngineDisposition, BrowserTargetCreationMetadata,
        BrowserTargetEngineHandoffOutcome, BrowserTargetHandle, BrowserTargetRegistration,
        BrowserTargetResidence, BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
    },
    runtime::NavigationEngine,
};

use super::super::BrowserNavigationOwner;

fn empty_context_projection(context_id: &str) -> BrowserTargetTopologyProjection {
    BrowserTargetTopologyProjection::new(
        context_id,
        None,
        Vec::<BrowserTargetSlotProjection>::new(),
    )
}

fn slot(
    target: BrowserTargetHandle,
    page_residence: BrowserPageResidenceHandle,
) -> BrowserTargetSlotProjection {
    BrowserTargetSlotProjection::new(target, page_residence)
}

fn registered_slot(registration: &BrowserTargetRegistration) -> BrowserTargetSlotProjection {
    slot(
        registration.handle().clone(),
        registration.page_residence_handle().clone(),
    )
}

fn selection(
    context_id: Option<&str>,
    disposition: BrowserSelectedTargetEngineDisposition,
) -> BrowserContextSelectionProjection {
    BrowserContextSelectionProjection::new(context_id.map(str::to_owned), disposition)
}

fn register_context(owner: &mut BrowserNavigationOwner, context_id: &str) {
    owner
        .register_browser_context(
            context_id,
            empty_context_projection(context_id),
            selection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            NavigationEngine::new,
        )
        .expect("test BrowserContext should register");
}

#[test]
fn disposing_context_rejects_new_target_registration_until_rollback() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    register_context(&mut owner, "context-disposing");
    let handle = owner
        .browser_context_handle("context-disposing")
        .expect("registered Context handle")
        .clone();
    let reservation = owner
        .begin_browser_context_disposal(&handle)
        .expect("Context should reserve disposal");

    let error = owner
        .register_background_target(
            "context-disposing",
            "target-too-late",
            empty_context_projection("context-disposing"),
        )
        .expect_err("a disposing Context must reject newly registered Targets");
    assert_eq!(
        error,
        crate::browser_host::BrowserTargetRegistryError::BrowserContext(
            crate::browser_host::BrowserContextRegistryError::BrowserContextDisposing(
                crate::browser_host::BrowserContextId::new("context-disposing")
            )
        )
    );
    assert_eq!(owner.browser_context_target_count("context-disposing"), 0);

    assert!(owner.rollback_browser_context_disposal(reservation));
    owner
        .register_background_target(
            "context-disposing",
            "target-after-rollback",
            empty_context_projection("context-disposing"),
        )
        .expect("rollback must restore Target registration authority");
}

#[test]
fn registration_and_activation_move_exact_target_topology_with_engine_owner() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    register_context(&mut owner, "context-1");

    let first = owner
        .register_active_target(
            "context-1",
            "target-a",
            empty_context_projection("context-1"),
            selection(
                Some("context-1"),
                BrowserSelectedTargetEngineDisposition::Unbound,
            ),
            NavigationEngine::new,
        )
        .expect("first active Target should register");
    assert_eq!(first.residence(), BrowserTargetResidence::Active);
    assert!(first.handle().is_live());
    assert_eq!(first.previous_active_target_id(), None);
    assert_eq!(
        first.engine_outcome(),
        Some(BrowserTargetEngineHandoffOutcome::ReusedSelected)
    );

    let second = owner
        .register_background_target(
            "context-1",
            "target-b",
            BrowserTargetTopologyProjection::new(
                "context-1",
                Some(registered_slot(&first)),
                Vec::<BrowserTargetSlotProjection>::new(),
            ),
        )
        .expect("background Target should register");
    let activated = owner
        .activate_target(
            "context-1",
            "target-b",
            BrowserTargetTopologyProjection::new(
                "context-1",
                Some(registered_slot(&first)),
                vec![registered_slot(&second)],
            ),
            selection(
                Some("context-1"),
                BrowserSelectedTargetEngineDisposition::Discard(
                    crate::browser_host::BrowserPageOwnerKey::new("context-1", "target-a"),
                ),
            ),
            NavigationEngine::new,
        )
        .expect("background Target should activate");

    assert!(activated.changed());
    assert_eq!(activated.previous_active_target_id(), Some("target-a"));
    assert_eq!(
        owner.active_target_id_for_browser_context("context-1"),
        Some("target-b")
    );
    assert_eq!(
        owner.target_browser_context_id("target-a"),
        Some("context-1")
    );
    assert_eq!(owner.target_count(), 2);
}

#[test]
fn target_registration_atomically_installs_initial_document_creation_metadata() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    register_context(&mut owner, "context-1");
    let key = BrowserPageOwnerKey::new("context-1", "target-a");
    let creator =
        BrowserInitialEmptyDocumentCreator::new("opener-a", "https://opener.example", "Secure");

    let registration = owner
        .register_background_target_with_creation_metadata(
            "context-1",
            "target-a",
            BrowserTargetCreationMetadata::with_initial_empty_document(
                BrowserInitialEmptyDocumentSeed::new("about:blank#created")
                    .with_creator(creator.clone()),
            ),
            empty_context_projection("context-1"),
        )
        .expect("Target and its creation metadata should commit together");

    assert_eq!(registration.owner(), &key);
    let initial = owner
        .target_initial_empty_document(&key)
        .expect("committed Target must expose its initial Document metadata");
    assert_eq!(initial.initial_url(), "about:blank#created");
    assert_eq!(initial.creator(), Some(&creator));
    assert_eq!(initial.loader_id(), "LID-INITIAL-target-a");

    let facts = owner.browser_fact_snapshot();
    assert_eq!(facts.len(), 1);
    assert!(matches!(facts[0].fact(), BrowserFact::TargetCreated));
    assert_eq!(facts[0].browser_context_id().as_str(), "context-1");
    assert_eq!(facts[0].target_id().as_str(), "target-a");
    assert_eq!(
        facts[0].page_residence(),
        registration.page_residence_identity()
    );
}

#[test]
fn bootstrap_active_target_replacement_retires_placeholder_without_demotion() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let placeholder = BrowserTargetHandle::staged("placeholder");
    let placeholder_page = BrowserPageResidenceHandle::default();
    owner
        .register_browser_context(
            "context-1",
            BrowserTargetTopologyProjection::new(
                "context-1",
                Some(slot(placeholder.clone(), placeholder_page.clone())),
                Vec::<BrowserTargetSlotProjection>::new(),
            ),
            selection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            NavigationEngine::new,
        )
        .expect("placeholder BrowserContext should register");

    let replacement = owner
        .replace_active_target(
            "context-1",
            "placeholder",
            "target-a",
            BrowserTargetTopologyProjection::new(
                "context-1",
                Some(slot(placeholder.clone(), placeholder_page)),
                Vec::<BrowserTargetSlotProjection>::new(),
            ),
            selection(
                Some("context-1"),
                BrowserSelectedTargetEngineDisposition::Discard(
                    crate::browser_host::BrowserPageOwnerKey::new("context-1", "placeholder"),
                ),
            ),
            NavigationEngine::new,
        )
        .expect("bootstrap placeholder should replace exactly");

    assert_eq!(replacement.residence(), BrowserTargetResidence::Active);
    assert!(placeholder.is_retired());
    assert!(replacement.handle().is_live());
    assert_eq!(replacement.previous_active_target_id(), Some("placeholder"));
    assert_eq!(
        owner.active_target_id_for_browser_context("context-1"),
        Some("target-a")
    );
    assert!(!owner.has_target("placeholder"));
    assert_eq!(owner.target_count(), 1);
}

#[test]
fn retired_active_source_rejects_replacement_without_registering_successor() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let placeholder = BrowserTargetHandle::staged("placeholder");
    let placeholder_page = BrowserPageResidenceHandle::default();
    owner
        .register_browser_context(
            "context-1",
            BrowserTargetTopologyProjection::new(
                "context-1",
                Some(slot(placeholder.clone(), placeholder_page.clone())),
                Vec::<BrowserTargetSlotProjection>::new(),
            ),
            selection(None, BrowserSelectedTargetEngineDisposition::Unbound),
            NavigationEngine::new,
        )
        .expect("placeholder BrowserContext should register");
    let renderer_owner = owner.active_renderer_owner_id_for_diagnostics();
    assert!(placeholder.reserve_retirement());
    placeholder.commit_retirement_reservation();

    let error = owner
        .replace_active_target(
            "context-1",
            "placeholder",
            "target-a",
            BrowserTargetTopologyProjection::new(
                "context-1",
                Some(slot(placeholder.clone(), placeholder_page)),
                Vec::<BrowserTargetSlotProjection>::new(),
            ),
            selection(
                Some("context-1"),
                BrowserSelectedTargetEngineDisposition::Discard(BrowserPageOwnerKey::new(
                    "context-1",
                    "placeholder",
                )),
            ),
            NavigationEngine::new,
        )
        .expect_err("retired exact source must reject active replacement");

    assert!(matches!(
        error,
        crate::browser_host::BrowserTargetRegistryError::TargetHandleProjectionMismatch(_)
    ));
    assert_eq!(
        owner.active_target_id_for_browser_context("context-1"),
        Some("placeholder")
    );
    assert!(owner.has_target("placeholder"));
    assert!(!owner.has_target("target-a"));
    assert_eq!(owner.target_count(), 1);
    assert_eq!(
        owner.active_renderer_owner_id_for_diagnostics(),
        renderer_owner
    );
}

#[test]
fn duplicate_target_and_wrong_physical_projection_are_rejected_without_mutation() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    register_context(&mut owner, "context-1");
    let first = owner
        .register_background_target(
            "context-1",
            "target-a",
            empty_context_projection("context-1"),
        )
        .expect("background Target should register");

    assert!(
        owner
            .register_background_target(
                "context-1",
                "target-a",
                BrowserTargetTopologyProjection::new(
                    "context-1",
                    None,
                    vec![registered_slot(&first)],
                ),
            )
            .is_err()
    );
    assert!(
        owner
            .register_background_target(
                "context-1",
                "target-b",
                empty_context_projection("context-1"),
            )
            .is_err()
    );
    let same_id_wrong_instance = BrowserTargetHandle::staged("target-a");
    assert!(matches!(
        owner.register_background_target(
            "context-1",
            "target-b",
            BrowserTargetTopologyProjection::new(
                "context-1",
                None,
                vec![slot(
                    same_id_wrong_instance.clone(),
                    first.page_residence_handle().clone(),
                )],
            ),
        ),
        Err(crate::browser_host::BrowserTargetRegistryError::TargetHandleProjectionMismatch(_))
    ));
    assert!(!same_id_wrong_instance.is_live());
    assert!(first.handle().is_live());
    assert_eq!(owner.target_count(), 1);
    assert_eq!(
        owner.target_browser_context_id("target-a"),
        Some("context-1")
    );
    assert!(!owner.has_target("target-b"));
}

#[test]
fn wrong_page_residence_projection_is_rejected_without_mutation() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    register_context(&mut owner, "context-1");
    let first = owner
        .register_background_target(
            "context-1",
            "target-a",
            empty_context_projection("context-1"),
        )
        .expect("background Target should register");
    let wrong_page = BrowserPageResidenceHandle::default();

    assert!(matches!(
        owner.register_background_target(
            "context-1",
            "target-b",
            BrowserTargetTopologyProjection::new(
                "context-1",
                None,
                vec![slot(first.handle().clone(), wrong_page)],
            ),
        ),
        Err(
            crate::browser_host::BrowserTargetRegistryError::PageResidence(
                crate::browser_host::BrowserPageResidenceRegistryError::ProjectionMismatch(_)
            )
        )
    ));
    assert_eq!(owner.target_count(), 1);
    assert!(!owner.has_target("target-b"));
    assert!(owner.page_residence_handle_is_current(first.owner(), first.page_residence_handle()));
}

#[test]
fn target_ids_are_globally_unique_across_browser_contexts() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    register_context(&mut owner, "context-1");
    owner
        .register_browser_context(
            "context-2",
            empty_context_projection("context-2"),
            selection(
                Some("context-1"),
                BrowserSelectedTargetEngineDisposition::Unbound,
            ),
            NavigationEngine::new,
        )
        .expect("second BrowserContext should register");
    owner
        .register_background_target(
            "context-1",
            "target-a",
            empty_context_projection("context-1"),
        )
        .expect("first Target should register");

    assert!(
        owner
            .register_background_target(
                "context-2",
                "target-a",
                empty_context_projection("context-2"),
            )
            .is_err()
    );
    assert_eq!(
        owner.target_browser_context_id("target-a"),
        Some("context-1")
    );
}
