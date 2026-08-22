use crate::{
    browser_host::{
        BrowserContextSelectionProjection, BrowserPageResidenceHandle,
        BrowserSelectedTargetEngineDisposition, BrowserTargetHandle, BrowserTargetResidence,
        BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
    },
    runtime::NavigationEngine,
};

use super::super::BrowserNavigationOwner;

fn empty_topology(context_id: &str) -> BrowserTargetTopologyProjection {
    BrowserTargetTopologyProjection::new(
        context_id,
        None,
        Vec::<BrowserTargetSlotProjection>::new(),
    )
}

fn selection(context_id: Option<&str>) -> BrowserContextSelectionProjection {
    BrowserContextSelectionProjection::new(
        context_id.map(str::to_owned),
        BrowserSelectedTargetEngineDisposition::Unbound,
    )
}

fn register_context(owner: &mut BrowserNavigationOwner, context_id: &str) {
    let selected = owner.selected_browser_context_id().map(str::to_owned);
    owner
        .register_browser_context(
            context_id,
            empty_topology(context_id),
            selection(selected.as_deref()),
            NavigationEngine::new,
        )
        .expect("test BrowserContext should register");
}

#[test]
fn snapshot_orders_core_context_and_target_topology() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    register_context(&mut owner, "context-selected");
    register_context(&mut owner, "context-inactive");

    let active = owner
        .register_active_target(
            "context-selected",
            "target-active",
            empty_topology("context-selected"),
            selection(Some("context-selected")),
            NavigationEngine::new,
        )
        .expect("active Target should register");
    owner
        .register_background_target(
            "context-selected",
            "target-background",
            BrowserTargetTopologyProjection::new(
                "context-selected",
                Some(BrowserTargetSlotProjection::new(
                    active.handle().clone(),
                    active.page_residence_handle().clone(),
                )),
                Vec::<BrowserTargetSlotProjection>::new(),
            ),
        )
        .expect("background Target should register");
    owner
        .register_background_target(
            "context-inactive",
            "target-inactive",
            empty_topology("context-inactive"),
        )
        .expect("inactive-context Target should register");

    let snapshot = owner
        .snapshot_top_level_targets()
        .expect("consistent owner state should snapshot");
    assert_eq!(snapshot.contexts().len(), 2);
    assert_eq!(
        snapshot.contexts()[0].browser_context_id(),
        "context-selected"
    );
    assert!(snapshot.contexts()[0].is_selected());
    assert_eq!(
        snapshot.contexts()[1].browser_context_id(),
        "context-inactive"
    );
    assert!(!snapshot.contexts()[1].is_selected());
    assert_eq!(snapshot.contexts()[0].targets().len(), 2);
    assert_eq!(
        snapshot.contexts()[0].targets()[0].target_id(),
        "target-active"
    );
    assert_eq!(
        snapshot.contexts()[0].targets()[0].residence(),
        BrowserTargetResidence::Active
    );
    assert_eq!(
        snapshot.contexts()[0].targets()[1].target_id(),
        "target-background"
    );
    assert_eq!(
        snapshot.contexts()[0].targets()[1].residence(),
        BrowserTargetResidence::Background
    );
    assert_eq!(
        snapshot.contexts()[1].targets()[0].target_id(),
        "target-inactive"
    );
}

#[test]
fn exact_snapshot_survives_document_generation_and_rejects_other_browser_instance() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    register_context(&mut owner, "context-1");
    let registration = owner
        .register_background_target("context-1", "target-1", empty_topology("context-1"))
        .expect("Target should register");
    let snapshot = owner
        .snapshot_top_level_targets()
        .expect("consistent owner state should snapshot");
    let context = snapshot.context("context-1").expect("snapshotted Context");
    let target = snapshot.target("target-1").expect("snapshotted Target");
    assert!(owner.browser_context_target_snapshot_is_current(context));
    assert!(owner.browser_target_state_snapshot_is_current(target));

    registration
        .page_residence_handle()
        .advance_generation_for_test_fixture();
    assert!(owner.browser_target_state_snapshot_is_current(target));

    let mut other = BrowserNavigationOwner::new(NavigationEngine::new());
    register_context(&mut other, "context-1");
    other
        .register_background_target("context-1", "target-1", empty_topology("context-1"))
        .expect("same public ids should register in another Browser instance");
    assert!(!other.browser_context_target_snapshot_is_current(context));
    assert!(!other.browser_target_state_snapshot_is_current(target));
}

#[test]
fn snapshot_rejects_replaced_target_and_page_slot() {
    let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let old_target = BrowserTargetHandle::staged("target-old");
    let old_page = BrowserPageResidenceHandle::default();
    owner
        .register_browser_context(
            "context-1",
            BrowserTargetTopologyProjection::new(
                "context-1",
                Some(BrowserTargetSlotProjection::new(
                    old_target.clone(),
                    old_page.clone(),
                )),
                Vec::<BrowserTargetSlotProjection>::new(),
            ),
            selection(None),
            NavigationEngine::new,
        )
        .expect("placeholder Target should register");
    let snapshot = owner
        .snapshot_top_level_targets()
        .expect("placeholder should snapshot");
    let stale = snapshot
        .target("target-old")
        .expect("placeholder snapshot")
        .clone();

    owner
        .replace_active_target(
            "context-1",
            "target-old",
            "target-new",
            BrowserTargetTopologyProjection::new(
                "context-1",
                Some(BrowserTargetSlotProjection::new(old_target, old_page)),
                Vec::<BrowserTargetSlotProjection>::new(),
            ),
            BrowserContextSelectionProjection::new(
                Some("context-1".to_owned()),
                BrowserSelectedTargetEngineDisposition::Discard(
                    crate::browser_host::BrowserPageOwnerKey::new("context-1", "target-old"),
                ),
            ),
            NavigationEngine::new,
        )
        .expect("exact placeholder should replace");

    assert!(!owner.browser_target_state_snapshot_is_current(&stale));
    assert!(
        owner.browser_target_state_snapshot_is_current(
            owner
                .snapshot_top_level_targets()
                .expect("successor should snapshot")
                .target("target-new")
                .expect("successor snapshot")
        )
    );
}

#[test]
fn empty_snapshot_keeps_exact_browser_provenance() {
    let owner = BrowserNavigationOwner::new(NavigationEngine::new());
    let snapshot = owner
        .snapshot_top_level_targets()
        .expect("empty Browser should snapshot");
    assert!(owner.browser_top_level_target_snapshot_is_from_current_browser(&snapshot));

    let other = BrowserNavigationOwner::new(NavigationEngine::new());
    assert!(!other.browser_top_level_target_snapshot_is_from_current_browser(&snapshot));
}
