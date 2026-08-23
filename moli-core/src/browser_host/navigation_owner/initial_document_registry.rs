use crate::browser_host::PageResidenceIdentity;

use super::{
    BrowserInitialEmptyDocumentSeed, BrowserInitialEmptyDocumentSnapshot,
    BrowserNavigationHistorySeed, BrowserNavigationOwner, BrowserPageOwnerKey,
    BrowserTargetRegistryError, initial_document::BrowserInitialEmptyDocumentRecord,
    target_runtime_registry::BrowserTargetRuntimeRegistry,
};

/// Authoritative initial-empty-Document lifecycle, keyed by browser Target.
///
/// Page construction remains a renderer/projection participant during the
/// migration, but creation metadata and lifecycle acceptance live here so a
/// frontend disconnect cannot erase or advance browser state.
#[derive(Default)]
pub(super) struct BrowserInitialEmptyDocumentRegistry;

impl BrowserInitialEmptyDocumentRegistry {
    pub(super) fn begin(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
        seed: BrowserInitialEmptyDocumentSeed,
    ) {
        if !seed.is_initial_empty_document() {
            if let Some(record) = runtimes.entries.get_mut(owner) {
                record.initial_empty_document = None;
            }
            runtimes.prune_empty();
            return;
        }
        runtimes
            .entries
            .entry(owner.clone())
            .or_default()
            .initial_empty_document = Some(BrowserInitialEmptyDocumentRecord::new(seed));
    }

    pub(super) fn snapshot(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
        has_pending_navigation: bool,
    ) -> Option<BrowserInitialEmptyDocumentSnapshot> {
        runtimes
            .entries
            .get(owner)
            .and_then(|record| record.initial_empty_document.as_ref())
            .map(|record| record.snapshot(owner.target_id(), has_pending_navigation))
    }

    pub(super) fn snapshots<'a>(
        &'a self,
        runtimes: &'a BrowserTargetRuntimeRegistry,
        has_pending_navigation: impl Fn(&BrowserPageOwnerKey) -> bool + 'a,
    ) -> impl Iterator<Item = BrowserInitialEmptyDocumentSnapshot> + 'a {
        runtimes.entries.iter().filter_map(move |(owner, runtime)| {
            runtime
                .initial_empty_document
                .as_ref()
                .map(|record| record.snapshot(owner.target_id(), has_pending_navigation(owner)))
        })
    }

    pub(super) fn history_seed(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
    ) -> Option<BrowserNavigationHistorySeed> {
        runtimes
            .entries
            .get(owner)
            .and_then(|record| record.initial_empty_document.as_ref())
            .map(BrowserInitialEmptyDocumentRecord::history_seed)
    }

    pub(super) fn accepts_materialization(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
    ) -> bool {
        runtimes
            .entries
            .get(owner)
            .and_then(|record| record.initial_empty_document.as_ref())
            .is_some_and(|state| state.is_on_initial_empty_document() && !state.materialized())
    }

    pub(super) fn mark_materialized(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
    ) -> bool {
        let Some(state) = runtimes
            .entries
            .get_mut(owner)
            .and_then(|record| record.initial_empty_document.as_mut())
        else {
            return false;
        };
        if !state.is_on_initial_empty_document() {
            return false;
        }
        state.mark_materialized();
        true
    }

    pub(super) fn rollback_materialized(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
    ) -> bool {
        runtimes
            .entries
            .get_mut(owner)
            .and_then(|record| record.initial_empty_document.as_mut())
            .is_some_and(BrowserInitialEmptyDocumentRecord::rollback_materialized)
    }

    pub(super) fn mark_exited(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
    ) {
        if let Some(state) = runtimes
            .entries
            .get_mut(owner)
            .and_then(|record| record.initial_empty_document.as_mut())
        {
            state.mark_exited();
        }
    }

    #[cfg(test)]
    pub(super) fn mark_exited_for_target(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        target_id: &str,
    ) {
        for (owner, runtime) in &mut runtimes.entries {
            if owner.target_id() == target_id
                && let Some(state) = runtime.initial_empty_document.as_mut()
            {
                state.mark_exited();
                break;
            }
        }
    }

    pub(super) fn can_install_current_page(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
    ) -> bool {
        self.accepts_materialization(runtimes, owner)
    }

    pub(super) fn accepts_initial_target_navigation(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
        replacement_url: &str,
    ) -> bool {
        runtimes
            .entries
            .get(owner)
            .and_then(|record| record.initial_empty_document.as_ref())
            .is_some_and(|state| {
                state.is_on_initial_empty_document() && state.initial_url() != replacement_url
            })
    }
}

impl BrowserNavigationOwner {
    /// Returns whether one exact Page may replace its initial empty Document
    /// with the Target creation URL now.
    ///
    /// Every trigger is advisory. Browser Host re-evaluates this predicate
    /// only after selecting the mailbox turn, so duplicate Page.enable,
    /// debugger-resume, or create-target inputs collapse against the same
    /// pending navigation state. A stale generation or recreated Target can
    /// never authorize the successor Page.
    pub fn accepts_initial_target_navigation(
        &self,
        expected: &PageResidenceIdentity,
        replacement_url: &str,
    ) -> bool {
        let Some(owner) = self.page_owner_key_if_current(expected) else {
            return false;
        };
        !self
            .document_navigations
            .has_pending(&self.target_runtimes, &owner)
            && self
                .initial_empty_documents
                .accepts_initial_target_navigation(&self.target_runtimes, &owner, replacement_url)
    }

    /// Transitional typed metadata install for an already registered Target.
    ///
    /// Production Target and BrowserContext creation carry
    /// `BrowserTargetCreationMetadata` in their registration transaction. This
    /// entry point remains for focused lifecycle fixtures and legacy callers;
    /// it validates exact live Target ownership and never assumes raw ids are
    /// sufficient authority.
    pub fn register_target_initial_empty_document(
        &mut self,
        owner: &BrowserPageOwnerKey,
        seed: BrowserInitialEmptyDocumentSeed,
    ) -> Result<(), BrowserTargetRegistryError> {
        self.targets.validate_target_owner(owner)?;
        self.initial_empty_documents
            .begin(&mut self.target_runtimes, owner, seed);
        Ok(())
    }

    pub fn target_initial_empty_document(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<BrowserInitialEmptyDocumentSnapshot> {
        self.initial_empty_documents.snapshot(
            &self.target_runtimes,
            owner,
            self.document_navigations
                .has_pending(&self.target_runtimes, owner),
        )
    }

    pub fn initial_empty_documents(
        &self,
    ) -> impl Iterator<Item = BrowserInitialEmptyDocumentSnapshot> + '_ {
        self.initial_empty_documents
            .snapshots(&self.target_runtimes, |owner| {
                self.document_navigations
                    .has_pending(&self.target_runtimes, owner)
            })
    }

    pub fn can_install_target_initial_empty_document_page(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> bool {
        self.targets.validate_target_owner(owner).is_ok()
            && self
                .initial_empty_documents
                .can_install_current_page(&self.target_runtimes, owner)
    }

    pub fn mark_target_initial_empty_document_exited(&mut self, owner: &BrowserPageOwnerKey) {
        self.initial_empty_documents
            .mark_exited(&mut self.target_runtimes, owner);
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        browser_host::{
            BrowserContextSelectionProjection, BrowserPageResidenceHandle,
            BrowserSelectedTargetEngineDisposition, BrowserTargetHandle,
            BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
        },
        runtime::NavigationEngine,
    };

    use super::*;

    fn owner_with_target() -> (BrowserNavigationOwner, BrowserPageOwnerKey) {
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        owner
            .register_browser_context(
                key.browser_context_id(),
                BrowserTargetTopologyProjection::new(
                    key.browser_context_id(),
                    Some(BrowserTargetSlotProjection::new(
                        BrowserTargetHandle::staged(key.target_id()),
                        BrowserPageResidenceHandle::default(),
                    )),
                    Vec::<BrowserTargetSlotProjection>::new(),
                ),
                BrowserContextSelectionProjection::new(
                    None,
                    BrowserSelectedTargetEngineDisposition::Unbound,
                ),
                NavigationEngine::new,
            )
            .expect("test Target should register");
        (owner, key)
    }

    #[test]
    fn lifecycle_record_survives_exit_and_rejects_late_materialization() {
        let (mut owner, key) = owner_with_target();
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank#initial"),
            )
            .expect("live Target should accept initial metadata");

        assert!(owner.can_install_target_initial_empty_document_page(&key));
        let permit = owner
            .prepare_initial_document_page_materialization(&key)
            .expect("current initial Document should prepare materialization");
        owner
            .commit_page_residence_transition_without_renderer_owner_for_testing(permit)
            .expect("exact initial materialization should commit");
        let first = owner
            .target_initial_empty_document(&key)
            .expect("initial document record");
        assert!(first.materialized());
        assert!(first.is_on_initial_empty_document());

        owner.mark_target_initial_empty_document_exited(&key);
        assert!(!owner.can_install_target_initial_empty_document_page(&key));
        assert!(
            owner
                .prepare_initial_document_page_materialization(&key)
                .is_none()
        );
        let exited = owner
            .target_initial_empty_document(&key)
            .expect("exited metadata remains diagnostic");
        assert!(exited.exited());
        assert!(exited.materialized());
    }

    #[test]
    fn non_initial_url_does_not_create_initial_document_authority() {
        let (mut owner, key) = owner_with_target();
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("https://example.test/"),
            )
            .expect("live Target should accept metadata input");
        assert!(owner.target_initial_empty_document(&key).is_none());
    }

    #[test]
    fn snapshot_derives_pending_state_from_document_navigation_registry() {
        let (mut owner, key) = owner_with_target();
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank"),
            )
            .expect("live Target should accept initial metadata");

        let failed = owner.start_document_navigation(&key, "loader-failed".to_owned());
        assert!(
            owner
                .target_initial_empty_document(&key)
                .expect("initial state")
                .pending_cross_document_navigation()
        );
        assert!(owner.fail_document_navigation_if_matches(
            &key,
            &failed,
            crate::browser_host::BrowserNavigationFailure::Network {
                error_text: "failed".to_owned(),
            },
        ));
        assert!(
            !owner
                .target_initial_empty_document(&key)
                .expect("initial state")
                .pending_cross_document_navigation()
        );

        let committed = owner.start_document_navigation(&key, "loader-committed".to_owned());
        assert!(owner.commit_document_navigation_if_matches(&key, &committed));
        let exited = owner
            .target_initial_empty_document(&key)
            .expect("creation metadata remains after exit");
        assert!(exited.exited());
        assert!(!exited.pending_cross_document_navigation());
    }

    #[test]
    fn initial_target_navigation_acceptance_is_exact_and_coalesces_pending_triggers() {
        let (mut owner, key) = owner_with_target();
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank"),
            )
            .expect("live Target should accept initial metadata");
        let initial_page = owner
            .capture_page_residence(key.browser_context_id(), key.target_id())
            .expect("initial Page identity");

        assert!(!owner.accepts_initial_target_navigation(&initial_page, "about:blank"));
        assert!(
            owner.accepts_initial_target_navigation(&initial_page, "https://example.test/created")
        );

        let pending = owner.start_document_navigation(&key, "loader-pending".to_owned());
        assert!(
            !owner.accepts_initial_target_navigation(&initial_page, "https://example.test/created"),
            "a second trigger must collapse behind the accepted navigation"
        );
        assert!(owner.fail_document_navigation_if_matches(
            &key,
            &pending,
            crate::browser_host::BrowserNavigationFailure::Canceled {
                error_text: "canceled".to_owned(),
            },
        ));

        let materialization = owner
            .prepare_initial_document_page_materialization(&key)
            .expect("initial Document materialization should be current");
        owner
            .commit_page_residence_transition_without_renderer_owner_for_testing(materialization)
            .expect("materialization should advance the Page generation");
        assert!(
            !owner.accepts_initial_target_navigation(&initial_page, "https://example.test/created"),
            "a trigger captured before Page generation advance must be stale"
        );
        let materialized_page = owner
            .capture_page_residence(key.browser_context_id(), key.target_id())
            .expect("materialized Page identity");
        assert!(
            owner.accepts_initial_target_navigation(
                &materialized_page,
                "https://example.test/created"
            )
        );

        owner.mark_target_initial_empty_document_exited(&key);
        assert!(
            !owner.accepts_initial_target_navigation(
                &materialized_page,
                "https://example.test/created"
            )
        );
    }
}
