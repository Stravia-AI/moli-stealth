use std::collections::HashMap;

use crate::browser_host::PageResidenceIdentity;
use crate::page::SameDocumentHistoryUpdate;

use super::{
    BrowserHistoryTraversalDestination, BrowserHistoryTraversalResolution,
    BrowserHistoryTraversalResolutionError, BrowserNavigationHistory,
    BrowserNavigationHistoryEntry, BrowserNavigationHistoryPageSnapshot,
    BrowserNavigationHistorySeed, BrowserNavigationOwner, BrowserPageOwnerKey,
    BrowserSameDocumentHistoryUpdateError,
};

/// Exact reason an actor-selected Page could not resolve a history traversal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserExactHistoryTraversalResolutionError {
    PageResidenceNoLongerCurrent { expected: PageResidenceIdentity },
    History(BrowserHistoryTraversalResolutionError),
}

impl std::fmt::Display for BrowserExactHistoryTraversalResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PageResidenceNoLongerCurrent { expected } => write!(
                formatter,
                "history traversal Page is no longer current: browser_context={:?}, target={:?}, generation={}",
                expected.browser_context_id(),
                expected.target_id(),
                expected.loaded_page_generation()
            ),
            Self::History(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for BrowserExactHistoryTraversalResolutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PageResidenceNoLongerCurrent { .. } => None,
            Self::History(error) => Some(error),
        }
    }
}

impl From<BrowserHistoryTraversalResolutionError> for BrowserExactHistoryTraversalResolutionError {
    fn from(error: BrowserHistoryTraversalResolutionError) -> Self {
        Self::History(error)
    }
}

/// Exact reason a same-Document history fact could not commit in Browser Core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserSameDocumentNavigationCommitError {
    PageResidenceNoLongerCurrent { expected: PageResidenceIdentity },
    History(BrowserSameDocumentHistoryUpdateError),
}

impl std::fmt::Display for BrowserSameDocumentNavigationCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PageResidenceNoLongerCurrent { expected } => write!(
                formatter,
                "same-Document navigation Page is no longer current: browser_context={:?}, target={:?}, generation={}",
                expected.browser_context_id(),
                expected.target_id(),
                expected.loaded_page_generation()
            ),
            Self::History(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for BrowserSameDocumentNavigationCommitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PageResidenceNoLongerCurrent { .. } => None,
            Self::History(error) => Some(error),
        }
    }
}

impl From<BrowserSameDocumentHistoryUpdateError> for BrowserSameDocumentNavigationCommitError {
    fn from(error: BrowserSameDocumentHistoryUpdateError) -> Self {
        Self::History(error)
    }
}

/// Authoritative joint session-history registry, keyed by browser Target.
///
/// `history.rs` owns the history algorithm and value types. This module owns
/// target lookup and lifecycle only, so neither concern grows into the main
/// navigation-owner registry.
#[derive(Default)]
pub(super) struct BrowserNavigationHistoryRegistry {
    entries: HashMap<BrowserPageOwnerKey, BrowserTargetNavigationHistory>,
}

#[derive(Clone)]
struct BrowserTargetNavigationHistory {
    history: BrowserNavigationHistory,
    accepts_initial_seed: bool,
}

impl Default for BrowserTargetNavigationHistory {
    fn default() -> Self {
        Self {
            history: BrowserNavigationHistory::default(),
            accepts_initial_seed: true,
        }
    }
}

impl BrowserNavigationHistoryRegistry {
    fn history_mut(&mut self, key: &BrowserPageOwnerKey) -> &mut BrowserNavigationHistory {
        &mut self.entries.entry(key.clone()).or_default().history
    }

    fn ensure_seeded(
        target_history: &mut BrowserTargetNavigationHistory,
        seed: Option<BrowserNavigationHistorySeed>,
    ) {
        if !target_history.accepts_initial_seed || !target_history.history.is_empty() {
            return;
        }
        let Some(seed) = seed else {
            return;
        };
        let entry_id = target_history.history.allocate_entry_id();
        target_history.history.seed_entry(seed.into_entry(entry_id));
        target_history.accepts_initial_seed = false;
    }

    fn snapshot(
        &mut self,
        key: &BrowserPageOwnerKey,
        seed: Option<BrowserNavigationHistorySeed>,
    ) -> (usize, Vec<BrowserNavigationHistoryEntry>) {
        let target_history = self.entries.entry(key.clone()).or_default();
        Self::ensure_seeded(target_history, seed);
        target_history.history.snapshot()
    }

    fn resolve_traversal(
        &mut self,
        key: &BrowserPageOwnerKey,
        seed: Option<BrowserNavigationHistorySeed>,
        destination: BrowserHistoryTraversalDestination,
    ) -> Result<BrowserHistoryTraversalResolution, BrowserHistoryTraversalResolutionError> {
        let target_history = self.entries.entry(key.clone()).or_default();
        Self::ensure_seeded(target_history, seed);
        target_history.history.resolve_traversal(destination)
    }

    fn reset(
        &mut self,
        key: &BrowserPageOwnerKey,
        seed: Option<BrowserNavigationHistorySeed>,
    ) -> bool {
        let target_history = self.entries.entry(key.clone()).or_default();
        Self::ensure_seeded(target_history, seed);
        target_history.history.prune_all_but_current()
    }

    fn can_reset(
        &mut self,
        key: &BrowserPageOwnerKey,
        seed: Option<BrowserNavigationHistorySeed>,
    ) -> bool {
        let target_history = self.entries.entry(key.clone()).or_default();
        Self::ensure_seeded(target_history, seed);
        target_history.history.can_prune_all_but_current()
    }

    fn mark_replace_current(&mut self, key: &BrowserPageOwnerKey) {
        self.history_mut(key).mark_replace_current();
    }

    fn mark_replace_initial_empty_document(&mut self, key: &BrowserPageOwnerKey) {
        self.history_mut(key).mark_replace_initial_empty_document();
    }

    fn mark_traverse_to_entry(&mut self, key: &BrowserPageOwnerKey, entry_id: i32) {
        self.history_mut(key).mark_traverse_to_entry(entry_id);
    }

    fn clear_pending_update(&mut self, key: &BrowserPageOwnerKey) {
        self.history_mut(key).clear_pending_update();
    }

    pub(super) fn record_loaded_page(
        &mut self,
        key: &BrowserPageOwnerKey,
        seed: Option<BrowserNavigationHistorySeed>,
        page: BrowserNavigationHistoryPageSnapshot,
    ) {
        let target_history = self.entries.entry(key.clone()).or_default();
        Self::ensure_seeded(target_history, seed);
        let entry_id = target_history.history.allocate_entry_id();
        target_history
            .history
            .record_loaded_entry(page.into_typed_entry(entry_id));
        target_history.accepts_initial_seed = false;
    }

    fn update_current_title(&mut self, key: &BrowserPageOwnerKey, title: String) -> Option<bool> {
        self.entries
            .get_mut(key)?
            .history
            .update_current_entry_title(title)
    }

    fn record_same_document(
        &mut self,
        key: &BrowserPageOwnerKey,
        seed: Option<BrowserNavigationHistorySeed>,
        url: String,
        title: String,
        update: SameDocumentHistoryUpdate,
    ) -> Result<(), BrowserSameDocumentHistoryUpdateError> {
        let entry_already_existed = self.entries.contains_key(key);
        let recorded = {
            let target_history = self.entries.entry(key.clone()).or_default();
            let seeded_for_this_update = target_history.accepts_initial_seed
                && target_history.history.is_empty()
                && seed.is_some();
            let seed_rollback = seeded_for_this_update.then(|| target_history.clone());
            Self::ensure_seeded(target_history, seed);
            let recorded = target_history
                .history
                .record_same_document_update(url, title, update);
            if recorded.is_ok() {
                target_history.accepts_initial_seed = false;
            } else if let Some(seed_rollback) = seed_rollback {
                *target_history = seed_rollback;
            }
            recorded
        };
        if recorded.is_err() && !entry_already_existed {
            self.entries.remove(key);
        }
        recorded
    }

    pub(super) fn clear(&mut self, key: &BrowserPageOwnerKey) {
        let target_history = self.entries.entry(key.clone()).or_default();
        target_history.history.clear();
        target_history.accepts_initial_seed = false;
    }

    pub(super) fn forget_target(&mut self, target_id: &str) {
        self.entries.retain(|key, _| key.target_id() != target_id);
    }
}

impl BrowserNavigationOwner {
    pub fn navigation_history_snapshot(
        &mut self,
        key: &BrowserPageOwnerKey,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
    ) -> (usize, Vec<BrowserNavigationHistoryEntry>) {
        let seed = self
            .initial_empty_documents
            .history_seed(key)
            .or(fallback_page_seed);
        self.navigation_histories.snapshot(key, seed)
    }

    pub fn resolve_navigation_history_traversal(
        &mut self,
        key: &BrowserPageOwnerKey,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
        destination: BrowserHistoryTraversalDestination,
    ) -> Result<BrowserHistoryTraversalResolution, BrowserHistoryTraversalResolutionError> {
        let seed = self
            .initial_empty_documents
            .history_seed(key)
            .or(fallback_page_seed);
        self.navigation_histories
            .resolve_traversal(key, seed, destination)
    }

    /// Resolves one actor-selected history command against its exact Page.
    ///
    /// Exact residence is validated before lazy history seeding or destination
    /// classification, so a queued command cannot observe or mutate its
    /// replacement Page's history.
    pub fn resolve_exact_navigation_history_traversal(
        &mut self,
        expected_page: &PageResidenceIdentity,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
        destination: BrowserHistoryTraversalDestination,
    ) -> Result<BrowserHistoryTraversalResolution, BrowserExactHistoryTraversalResolutionError>
    {
        let Some(key) = self.page_owner_key_if_current(expected_page) else {
            return Err(
                BrowserExactHistoryTraversalResolutionError::PageResidenceNoLongerCurrent {
                    expected: expected_page.clone(),
                },
            );
        };
        self.resolve_navigation_history_traversal(&key, fallback_page_seed, destination)
            .map_err(Into::into)
    }

    pub fn reset_navigation_history(
        &mut self,
        key: &BrowserPageOwnerKey,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
    ) -> bool {
        let seed = self
            .initial_empty_documents
            .history_seed(key)
            .or(fallback_page_seed);
        self.navigation_histories.reset(key, seed)
    }

    pub fn can_reset_navigation_history(
        &mut self,
        key: &BrowserPageOwnerKey,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
    ) -> bool {
        let seed = self
            .initial_empty_documents
            .history_seed(key)
            .or(fallback_page_seed);
        self.navigation_histories.can_reset(key, seed)
    }

    pub fn mark_next_navigation_history_replace_current(&mut self, key: &BrowserPageOwnerKey) {
        self.navigation_histories.mark_replace_current(key);
    }

    pub fn mark_next_navigation_history_replace_initial_empty_document(
        &mut self,
        key: &BrowserPageOwnerKey,
    ) {
        self.navigation_histories
            .mark_replace_initial_empty_document(key);
    }

    pub fn mark_next_navigation_history_traverse_to_entry(
        &mut self,
        key: &BrowserPageOwnerKey,
        entry_id: i32,
    ) {
        self.navigation_histories
            .mark_traverse_to_entry(key, entry_id);
    }

    pub fn clear_pending_navigation_history_update(&mut self, key: &BrowserPageOwnerKey) {
        self.navigation_histories.clear_pending_update(key);
    }

    pub fn record_loaded_page_navigation_history(
        &mut self,
        key: &BrowserPageOwnerKey,
        page: BrowserNavigationHistoryPageSnapshot,
    ) {
        let seed = self.initial_empty_documents.history_seed(key);
        self.navigation_histories
            .record_loaded_page(key, seed, page);
    }

    /// Updates the current history entry from renderer title output for one
    /// exact, still-current Page residence.
    ///
    /// Returning `None` means either the Page was replaced or no committed
    /// history entry exists yet. In both cases no Browser-owned state changes.
    pub fn update_current_document_title(
        &mut self,
        expected_page: &PageResidenceIdentity,
        title: String,
    ) -> Option<bool> {
        let key = self.page_owner_key_if_current(expected_page)?;
        self.navigation_histories.update_current_title(&key, title)
    }

    /// Atomically commits a renderer same-Document history fact for one exact
    /// Page residence.
    ///
    /// A rejected traversal leaves both the history cursor and any lazy seed
    /// unchanged. The caller may therefore gate physical target projection and
    /// frontend facts directly on this typed result.
    pub fn commit_same_document_navigation_history(
        &mut self,
        expected_page: &PageResidenceIdentity,
        fallback_page_seed: Option<BrowserNavigationHistorySeed>,
        url: String,
        title: String,
        update: SameDocumentHistoryUpdate,
    ) -> Result<(), BrowserSameDocumentNavigationCommitError> {
        let Some(key) = self.page_owner_key_if_current(expected_page) else {
            return Err(
                BrowserSameDocumentNavigationCommitError::PageResidenceNoLongerCurrent {
                    expected: expected_page.clone(),
                },
            );
        };
        let seed = self
            .initial_empty_documents
            .history_seed(&key)
            .or(fallback_page_seed);
        self.navigation_histories
            .record_same_document(&key, seed, url, title, update)
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        browser_host::{
            BrowserContextSelectionProjection, BrowserInitialEmptyDocumentSeed,
            BrowserPageResidenceHandle, BrowserSelectedTargetEngineDisposition,
            BrowserTargetHandle, BrowserTargetSlotProjection, BrowserTargetTerminationKind,
            BrowserTargetTopologyProjection,
        },
        runtime::NavigationEngine,
    };

    use super::*;

    fn page(url: &str) -> BrowserNavigationHistoryPageSnapshot {
        BrowserNavigationHistoryPageSnapshot::new(url, String::new())
    }

    fn register_target(
        owner: &mut BrowserNavigationOwner,
        key: &BrowserPageOwnerKey,
    ) -> BrowserPageResidenceHandle {
        let page_residence = BrowserPageResidenceHandle::default();
        owner
            .register_browser_context(
                key.browser_context_id(),
                BrowserTargetTopologyProjection::new(
                    key.browser_context_id(),
                    Some(BrowserTargetSlotProjection::new(
                        BrowserTargetHandle::staged(key.target_id()),
                        page_residence.clone(),
                    )),
                    Vec::<BrowserTargetSlotProjection>::new(),
                ),
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
    fn target_histories_are_isolated_and_allocate_inside_core() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let first = BrowserPageOwnerKey::new("context-1", "target-1");
        let second = BrowserPageOwnerKey::new("context-1", "target-2");

        owner.record_loaded_page_navigation_history(&first, page("https://one.test/"));
        owner.record_loaded_page_navigation_history(&second, page("https://two.test/"));
        owner.mark_next_navigation_history_replace_current(&first);
        owner.record_loaded_page_navigation_history(&first, page("https://one.test/reload"));

        let (_, first_entries) = owner.navigation_history_snapshot(&first, None);
        let (_, second_entries) = owner.navigation_history_snapshot(&second, None);
        assert_eq!(first_entries.len(), 1);
        assert_eq!(first_entries[0].id, 1);
        assert_eq!(first_entries[0].transition_type, "reload");
        assert_eq!(second_entries.len(), 1);
        assert_eq!(second_entries[0].id, 1);
        assert_eq!(second_entries[0].url, "https://two.test/");
    }

    #[test]
    fn initial_document_seed_precedes_pending_loaded_page_update() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let _page_residence = register_target(&mut owner, &key);
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank"),
            )
            .expect("live Target should accept initial metadata");
        owner.mark_next_navigation_history_replace_current(&key);
        owner.record_loaded_page_navigation_history(&key, page("https://example.test/"));

        let (_, entries) = owner.navigation_history_snapshot(&key, None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 1);
        assert_eq!(entries[0].url, "https://example.test/");
        assert_eq!(entries[0].user_typed_url, "about:blank");
        assert_eq!(entries[0].transition_type, "reload");
    }

    #[test]
    fn direct_target_initial_url_replaces_initial_empty_document_entry() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let _page_residence = register_target(&mut owner, &key);
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank"),
            )
            .expect("live Target should accept initial metadata");
        owner.mark_next_navigation_history_replace_initial_empty_document(&key);
        owner.record_loaded_page_navigation_history(
            &key,
            BrowserNavigationHistoryPageSnapshot::new("https://example.test/direct", "direct"),
        );

        let (current_index, entries) = owner.navigation_history_snapshot(&key, None);
        assert_eq!(current_index, 0);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "https://example.test/direct");
        assert_eq!(entries[0].user_typed_url, "https://example.test/direct");
        assert_eq!(entries[0].title, "direct");
        assert_eq!(entries[0].transition_type, "auto_toplevel");
    }

    #[test]
    fn page_runtime_discard_preserves_history_but_target_termination_forgets_it() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let _page_residence = register_target(&mut owner, &key);
        owner.record_loaded_page_navigation_history(&key, page("https://example.test/"));

        owner.discard_target_page_runtime("target-1");
        assert!(owner.navigation_histories.entries.contains_key(&key));

        owner.forget_target("target-1");
        assert!(!owner.navigation_histories.entries.contains_key(&key));
    }

    #[test]
    fn crashed_target_history_stays_empty_instead_of_reseeding_initial_document() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let _page_residence = register_target(&mut owner, &key);
        owner.record_loaded_page_navigation_history(&key, page("https://example.test/"));

        let request = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Crash)
            .expect("live target should capture crash");
        let permit = owner
            .prepare_target_termination(request)
            .expect("exact crash should prepare");
        owner
            .commit_target_termination(permit)
            .expect("exact crash should commit");
        let (_, entries) = owner.navigation_history_snapshot(
            &key,
            Some(BrowserNavigationHistorySeed::initial_empty_document(
                "about:blank",
            )),
        );

        assert!(entries.is_empty());
    }

    #[test]
    fn rejected_same_document_traversal_leaves_core_history_unchanged() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let _page_residence = register_target(&mut owner, &key);
        owner.record_loaded_page_navigation_history(&key, page("https://example.test/current"));
        let expected_page = owner
            .capture_page_residence("context-1", "target-1")
            .expect("registered Target should expose its exact Page");
        let before = owner.navigation_history_snapshot(&key, None);

        let error = owner
            .commit_same_document_navigation_history(
                &expected_page,
                None,
                "https://example.test/missing".to_owned(),
                String::new(),
                SameDocumentHistoryUpdate::Traverse { delta: -1 },
            )
            .expect_err("out-of-range traversal must be rejected");

        assert!(matches!(
            error,
            BrowserSameDocumentNavigationCommitError::History(
                BrowserSameDocumentHistoryUpdateError::TraversalTargetOutsideHistory {
                    current_index: 0,
                    delta: -1,
                    entry_count: 1,
                }
            )
        ));
        assert_eq!(owner.navigation_history_snapshot(&key, None), before);
    }

    #[test]
    fn stale_page_cannot_commit_same_document_history() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_target(&mut owner, &key);
        owner.record_loaded_page_navigation_history(&key, page("https://example.test/current"));
        let stale_page = owner
            .capture_page_residence("context-1", "target-1")
            .expect("registered Target should expose its exact Page");
        page_residence.advance_generation_for_test_fixture();
        let before = owner.navigation_history_snapshot(&key, None);

        let error = owner
            .commit_same_document_navigation_history(
                &stale_page,
                None,
                "https://example.test/current#stale".to_owned(),
                String::new(),
                SameDocumentHistoryUpdate::Push,
            )
            .expect_err("retired Page must not mutate Target history");

        assert_eq!(
            error,
            BrowserSameDocumentNavigationCommitError::PageResidenceNoLongerCurrent {
                expected: stale_page,
            }
        );
        assert_eq!(owner.navigation_history_snapshot(&key, None), before);
    }

    #[test]
    fn stale_page_cannot_resolve_history_destination() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_target(&mut owner, &key);
        owner.record_loaded_page_navigation_history(&key, page("https://example.test/first"));
        owner.record_loaded_page_navigation_history(&key, page("https://example.test/second"));
        let stale_page = owner
            .capture_page_residence("context-1", "target-1")
            .expect("registered Target should expose its exact Page");
        let before = owner.navigation_history_snapshot(&key, None);
        let first_entry_id = before.1[0].id;
        page_residence.advance_generation_for_test_fixture();

        let error = owner
            .resolve_exact_navigation_history_traversal(
                &stale_page,
                None,
                BrowserHistoryTraversalDestination::Entry(first_entry_id),
            )
            .expect_err("retired Page must not resolve successor history");

        assert_eq!(
            error,
            BrowserExactHistoryTraversalResolutionError::PageResidenceNoLongerCurrent {
                expected: stale_page,
            }
        );
        assert_eq!(owner.navigation_history_snapshot(&key, None), before);
    }

    #[test]
    fn rejected_lazy_seed_restores_pending_history_update() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let _page_residence = register_target(&mut owner, &key);
        let expected_page = owner
            .capture_page_residence("context-1", "target-1")
            .expect("registered Target should expose its exact Page");
        owner.mark_next_navigation_history_traverse_to_entry(&key, 77);
        let fallback_seed =
            BrowserNavigationHistorySeed::page_snapshot(page("https://example.test/current"));

        assert!(matches!(
            owner.commit_same_document_navigation_history(
                &expected_page,
                Some(fallback_seed.clone()),
                "https://example.test/missing".to_owned(),
                String::new(),
                SameDocumentHistoryUpdate::Traverse { delta: -1 },
            ),
            Err(BrowserSameDocumentNavigationCommitError::History(
                BrowserSameDocumentHistoryUpdateError::TraversalTargetOutsideHistory { .. }
            ))
        ));
        assert!(
            !owner.can_reset_navigation_history(&key, Some(fallback_seed)),
            "rejected lazy seeding must restore the pending traversal exactly"
        );
    }
}
