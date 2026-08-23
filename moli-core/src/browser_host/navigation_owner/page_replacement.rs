use crate::browser_host::{
    BrowserPageRuntimeAccess, BrowserPageRuntimeOwner, PageResidenceIdentity,
};
use crate::page::RendererPageLifetimeOwner;

use super::{
    BrowserDocumentNavigation, BrowserNavigationHistoryPageSnapshot, BrowserNavigationOwner,
    BrowserNavigationTraceEvent, BrowserPageOwnerKey, BrowserPageResidenceRegistryError,
    BrowserTargetMetadataTransition,
};

/// Exact Browser Core authorization for one loaded cross-document Page swap.
///
/// Preparing the permit does not mutate request, history, or Page generation,
/// so protocol-specific renderer attachment preparation may still fail safely.
/// The permit contains no frontend/session identity and can be committed only
/// by the same Browser Owner while its request and Page residence remain exact.
#[derive(Debug)]
pub struct BrowserPageReplacementPermit {
    owner: BrowserPageOwnerKey,
    navigation: BrowserDocumentNavigation,
    previous_page: PageResidenceIdentity,
}

impl BrowserPageReplacementPermit {
    pub fn owner(&self) -> &BrowserPageOwnerKey {
        &self.owner
    }

    pub fn navigation(&self) -> &BrowserDocumentNavigation {
        &self.navigation
    }

    pub fn previous_page(&self) -> &PageResidenceIdentity {
        &self.previous_page
    }
}

/// Browser-owned result of atomically committing a successor Page residence.
#[derive(Debug)]
pub struct BrowserPageReplacement {
    owner: BrowserPageOwnerKey,
    navigation: BrowserDocumentNavigation,
    previous_page: PageResidenceIdentity,
    current_page: PageResidenceIdentity,
    retired_renderer_page_owner: Option<RendererPageLifetimeOwner>,
    current_page_runtime: Option<BrowserPageRuntimeAccess>,
}

impl BrowserPageReplacement {
    pub fn owner(&self) -> &BrowserPageOwnerKey {
        &self.owner
    }

    pub fn navigation(&self) -> &BrowserDocumentNavigation {
        &self.navigation
    }

    pub fn previous_page(&self) -> &PageResidenceIdentity {
        &self.previous_page
    }

    pub fn current_page(&self) -> &PageResidenceIdentity {
        &self.current_page
    }

    /// Takes the exact renderer Page lifetime retired by this replacement.
    pub fn take_retired_renderer_page_owner(&mut self) -> Option<RendererPageLifetimeOwner> {
        self.retired_renderer_page_owner.take()
    }

    pub fn current_page_runtime(&self) -> Option<&BrowserPageRuntimeAccess> {
        self.current_page_runtime.as_ref()
    }
}

/// Exact reason why a previously prepared Page replacement no longer commits.
///
/// A permit is intentionally revalidated at the owner turn that consumes it.
/// Stale request, recovery, or Page generation state is an ordinary rejected
/// owner action rather than a process-fatal invariant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserPageReplacementCommitError {
    NavigationNoLongerPending {
        owner: BrowserPageOwnerKey,
        navigation: BrowserDocumentNavigation,
    },
    TargetNoLongerAcceptsReplacement {
        owner: BrowserPageOwnerKey,
        navigation: BrowserDocumentNavigation,
    },
    RendererPageOwnerMissing {
        owner: BrowserPageOwnerKey,
        navigation: BrowserDocumentNavigation,
    },
    PageResidence(BrowserPageResidenceRegistryError),
}

impl std::fmt::Display for BrowserPageReplacementCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NavigationNoLongerPending { owner, navigation } => write!(
                formatter,
                "navigation {:?} is no longer pending for Target {:?} in BrowserContext {:?}",
                navigation,
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::TargetNoLongerAcceptsReplacement { owner, navigation } => write!(
                formatter,
                "Target {:?} in BrowserContext {:?} no longer accepts replacement from navigation {:?}",
                owner.target_id(),
                owner.browser_context_id(),
                navigation
            ),
            Self::RendererPageOwnerMissing { owner, navigation } => write!(
                formatter,
                "replacement navigation {:?} for Target {:?} in BrowserContext {:?} has no renderer Page lifetime owner",
                navigation,
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::PageResidence(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for BrowserPageReplacementCommitError {}

impl From<BrowserPageResidenceRegistryError> for BrowserPageReplacementCommitError {
    fn from(error: BrowserPageResidenceRegistryError) -> Self {
        Self::PageResidence(error)
    }
}

impl BrowserNavigationOwner {
    pub fn prepare_loaded_page_replacement(
        &self,
        owner: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> Option<BrowserPageReplacementPermit> {
        if !self.browser_context_accepts_owner_work(owner.browser_context_id())
            || !self
                .target_terminations
                .accepts_page_replacement(owner, navigation)
            || owner.target_id() != navigation.target_id()
            || !self.document_navigations.accepts_pending(owner, navigation)
        {
            return None;
        }
        let previous_page = self.page_residences.prepare_replacement(owner)?;
        Some(BrowserPageReplacementPermit {
            owner: owner.clone(),
            navigation: navigation.clone(),
            previous_page,
        })
    }

    /// Commits request identity, Page generation, and joint history in one
    /// Browser Owner mutation. The exact request, recovery state, and Page
    /// generation are revalidated so a permit may safely cross an actor turn.
    fn commit_loaded_page_replacement_inner(
        &mut self,
        permit: BrowserPageReplacementPermit,
        history_page: BrowserNavigationHistoryPageSnapshot,
        successor_renderer_page_owner: &mut Option<RendererPageLifetimeOwner>,
        successor_page_runtime_owner: &mut Option<BrowserPageRuntimeOwner>,
    ) -> Result<BrowserPageReplacement, BrowserPageReplacementCommitError> {
        if !self
            .document_navigations
            .accepts_pending(&permit.owner, &permit.navigation)
        {
            return Err(
                BrowserPageReplacementCommitError::NavigationNoLongerPending {
                    owner: permit.owner,
                    navigation: permit.navigation,
                },
            );
        }
        if !self.browser_context_accepts_owner_work(permit.owner.browser_context_id())
            || !self
                .target_terminations
                .accepts_page_replacement(&permit.owner, &permit.navigation)
        {
            return Err(
                BrowserPageReplacementCommitError::TargetNoLongerAcceptsReplacement {
                    owner: permit.owner,
                    navigation: permit.navigation,
                },
            );
        }
        let Some(request_rollback) = self
            .document_navigations
            .commit_with_rollback_if_matches(&permit.owner, &permit.navigation)
        else {
            return Err(
                BrowserPageReplacementCommitError::NavigationNoLongerPending {
                    owner: permit.owner,
                    navigation: permit.navigation,
                },
            );
        };
        let (current_page, retired_renderer_page_owner, current_page_runtime) =
            match self.page_residences.commit_transition_with_page_owners(
                &permit.owner,
                &permit.previous_page,
                successor_renderer_page_owner,
                successor_page_runtime_owner,
            ) {
                Ok(committed) => committed,
                Err(error) => {
                    let rolled_back = self.document_navigations.rollback_commit(
                        &permit.owner,
                        &permit.navigation,
                        request_rollback,
                    );
                    debug_assert!(
                        rolled_back,
                        "same-turn stale Page generation must restore the pending request"
                    );
                    return Err(error.into());
                }
            };
        let metadata_transition = BrowserTargetMetadataTransition::navigation_committed(
            permit.navigation.clone(),
            history_page.url().to_owned(),
            history_page.title().to_owned(),
        );
        let history_seed = self.initial_empty_documents.history_seed(&permit.owner);
        self.navigation_histories
            .record_loaded_page(&permit.owner, history_seed, history_page);
        self.target_terminations
            .commit_navigation(&permit.owner, &permit.navigation);
        self.initial_empty_documents.mark_exited(&permit.owner);
        if let Some(trace) = self
            .document_navigations
            .trace_context(&permit.owner, &permit.navigation)
        {
            trace.emit(
                BrowserNavigationTraceEvent::new(
                    "page_replacement_committed",
                    trace.origin(),
                    "request-pending",
                    "page-resident",
                )
                .with_navigation(&permit.navigation)
                .with_page(current_page.clone()),
            );
        }
        let replacement = BrowserPageReplacement {
            owner: permit.owner,
            navigation: permit.navigation,
            previous_page: permit.previous_page,
            current_page,
            retired_renderer_page_owner,
            current_page_runtime,
        };
        if let Err(error) = self.record_loaded_navigation_commit_facts(
            replacement.owner(),
            replacement.navigation(),
            replacement.previous_page(),
            replacement.current_page(),
            metadata_transition,
        ) {
            tracing::error!(
                %error,
                browser_context_id = replacement.owner().browser_context_id(),
                target_id = replacement.owner().target_id(),
                navigation_request_id = replacement.navigation().request_id().get(),
                previous_page_generation = replacement.previous_page().loaded_page_generation(),
                current_page_generation = replacement.current_page().loaded_page_generation(),
                "failed to publish loaded navigation commit Browser facts"
            );
        }
        Ok(replacement)
    }

    /// Commits a loaded Document replacement and transfers the successor's
    /// unique renderer Page lifetime to Browser Host atomically with Page
    /// generation, request, history, and facts.
    pub fn commit_loaded_page_replacement(
        &mut self,
        permit: BrowserPageReplacementPermit,
        history_page: BrowserNavigationHistoryPageSnapshot,
        renderer_page_owner: &mut Option<RendererPageLifetimeOwner>,
        page_runtime_owner: &mut Option<BrowserPageRuntimeOwner>,
    ) -> Result<BrowserPageReplacement, BrowserPageReplacementCommitError> {
        if renderer_page_owner.is_none() || page_runtime_owner.is_none() {
            return Err(
                BrowserPageReplacementCommitError::RendererPageOwnerMissing {
                    owner: permit.owner,
                    navigation: permit.navigation,
                },
            );
        }
        self.commit_loaded_page_replacement_inner(
            permit,
            history_page,
            renderer_page_owner,
            page_runtime_owner,
        )
    }

    /// Authority-only helper for registry/fact tests that intentionally have
    /// no physical renderer Page payload.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn commit_loaded_page_replacement_without_renderer_owner_for_testing(
        &mut self,
        permit: BrowserPageReplacementPermit,
        history_page: BrowserNavigationHistoryPageSnapshot,
    ) -> Result<BrowserPageReplacement, BrowserPageReplacementCommitError> {
        let mut no_successor = None;
        let mut no_runtime_successor = None;
        self.commit_loaded_page_replacement_inner(
            permit,
            history_page,
            &mut no_successor,
            &mut no_runtime_successor,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        PageId,
        browser_host::{
            BrowserContextSelectionProjection, BrowserFact, BrowserInitialEmptyDocumentSeed,
            BrowserPageResidenceHandle, BrowserSelectedTargetEngineDisposition,
            BrowserTargetHandle, BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
        },
        page::{
            RendererDocumentLifecycleEvent, RendererDocumentLifecycleEventKind,
            RendererDocumentLifecycleMilestone, RendererDocumentToken, RendererFrameToken,
            RendererLifecycleEpoch,
        },
        runtime::NavigationEngine,
    };

    use super::*;

    fn page(url: &str) -> BrowserNavigationHistoryPageSnapshot {
        BrowserNavigationHistoryPageSnapshot::new(url, "title")
    }

    fn dcl_event() -> RendererDocumentLifecycleEvent {
        let page_id = PageId::new_for_testing(17);
        RendererDocumentLifecycleEvent {
            frame: RendererFrameToken { page_id },
            document: RendererDocumentToken::new_for_testing(page_id, 2),
            epoch: RendererLifecycleEpoch(1),
            sequence: 7,
            timestamp_micros: 70,
            kind: RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        }
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
    fn owner_commit_advances_page_and_commits_request_and_history_together() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_target(&mut owner, &key);
        let navigation = owner.start_document_navigation(&key, "loader-1".to_owned());
        let mut subscriber = owner.subscribe_browser_facts();
        let permit = owner
            .prepare_loaded_page_replacement(&key, &navigation)
            .expect("current request should prepare replacement");
        let previous = permit.previous_page().clone();

        let replacement = owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                permit,
                page("https://example.test/next"),
            )
            .expect("exact replacement should commit");

        assert_eq!(replacement.previous_page(), &previous);
        assert_eq!(replacement.current_page().loaded_page_generation(), 1);
        assert_eq!(page_residence.generation(), 1);
        assert!(owner.page_owner_key_if_current(&previous).is_none());
        assert_eq!(
            owner.page_owner_key_if_current(replacement.current_page()),
            Some(key.clone())
        );
        assert!(owner.accepts_committed_document_navigation(&key, &navigation));
        assert!(!owner.has_pending_document_navigation(&key));
        let (index, history) = owner.navigation_history_snapshot(&key, None);
        assert_eq!(index, 0);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].url, "https://example.test/next");

        let created_fact = subscriber
            .try_recv()
            .expect("Target registration should publish its creation occurrence");
        assert_eq!(created_fact.sequence().get(), 1);
        assert_eq!(created_fact.page_residence(), &previous);
        assert!(matches!(created_fact.fact(), BrowserFact::TargetCreated));
        let accepted_fact = subscriber
            .try_recv()
            .expect("live Browser subscriber should receive navigation acceptance");
        assert_eq!(accepted_fact.sequence().get(), 2);
        assert_eq!(accepted_fact.page_residence(), &previous);
        assert_eq!(
            accepted_fact.fact(),
            &BrowserFact::NavigationAccepted {
                navigation: navigation.clone(),
            }
        );
        let committed_fact = subscriber
            .try_recv()
            .expect("live Browser subscriber should receive navigation commit");
        assert_eq!(committed_fact.sequence().get(), 3);
        assert_eq!(committed_fact.browser_context_id().as_str(), "context-1");
        assert_eq!(committed_fact.target_id().as_str(), "target-1");
        assert_eq!(committed_fact.page_residence(), replacement.current_page());
        assert_eq!(
            committed_fact.fact(),
            &BrowserFact::NavigationCommitted {
                navigation: navigation.clone(),
                previous_page: previous.clone(),
            }
        );
        let metadata_fact = subscriber
            .try_recv()
            .expect("live Browser subscriber should receive Target metadata transition");
        assert_eq!(metadata_fact.sequence().get(), 4);
        assert_eq!(metadata_fact.page_residence(), replacement.current_page());
        assert_eq!(
            metadata_fact.fact(),
            &BrowserFact::TargetMetadataChanged {
                transition: BrowserTargetMetadataTransition::navigation_committed(
                    navigation.clone(),
                    "https://example.test/next".to_owned(),
                    "title".to_owned(),
                ),
            }
        );

        owner
            .record_document_lifecycle_facts(replacement.current_page(), &[dcl_event()])
            .expect("successor lifecycle fact should publish");
        let facts = owner.browser_fact_snapshot();
        assert_eq!(facts.len(), 5);
        assert_eq!(facts[0].as_ref(), created_fact.as_ref());
        assert_eq!(facts[1].as_ref(), accepted_fact.as_ref());
        assert_eq!(facts[2].as_ref(), committed_fact.as_ref());
        assert_eq!(facts[3].as_ref(), metadata_fact.as_ref());
        assert_eq!(facts[4].sequence().get(), 5);
        assert_eq!(facts[4].page_residence(), replacement.current_page());
        assert!(matches!(
            facts[4].fact(),
            BrowserFact::DocumentLifecycleReached {
                milestone: RendererDocumentLifecycleMilestone::DomContentLoaded,
                ..
            }
        ));
    }

    #[test]
    fn superseded_request_cannot_prepare_or_advance_page_replacement() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_target(&mut owner, &key);
        let stale = owner.start_document_navigation(&key, "loader-1".to_owned());
        let _current = owner.start_document_navigation(&key, "loader-2".to_owned());

        assert!(
            owner
                .prepare_loaded_page_replacement(&key, &stale)
                .is_none()
        );
        assert_eq!(page_residence.generation(), 0);
    }

    #[test]
    fn prepared_replacement_returns_typed_stale_after_request_is_superseded() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_target(&mut owner, &key);
        let stale = owner.start_document_navigation(&key, "loader-1".to_owned());
        let permit = owner
            .prepare_loaded_page_replacement(&key, &stale)
            .expect("current request should prepare replacement");
        let current = owner.start_document_navigation(&key, "loader-2".to_owned());

        let error = owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                permit,
                page("https://example.test/stale"),
            )
            .expect_err("a superseded permit must be rejected without panicking");

        assert_eq!(
            error,
            BrowserPageReplacementCommitError::NavigationNoLongerPending {
                owner: key.clone(),
                navigation: stale,
            }
        );
        assert_eq!(page_residence.generation(), 0);
        assert!(owner.accepts_pending_document_navigation(&key, &current));
        let (index, history) = owner.navigation_history_snapshot(&key, None);
        assert_eq!(index, 0);
        assert!(history.is_empty());
        assert!(
            owner.browser_fact_snapshot().iter().all(|fact| !matches!(
                fact.fact(),
                BrowserFact::NavigationCommitted { .. } | BrowserFact::TargetMetadataChanged { .. }
            )),
            "a stale replacement permit must not publish commit facts"
        );
    }

    #[test]
    fn stale_page_generation_rolls_back_precommitted_navigation_request() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_target(&mut owner, &key);
        let navigation = owner.start_document_navigation(&key, "loader-1".to_owned());
        let permit = owner
            .prepare_loaded_page_replacement(&key, &navigation)
            .expect("current request should prepare replacement");
        page_residence.advance_generation_for_test_fixture();

        let error = owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                permit,
                page("https://example.test/stale"),
            )
            .expect_err("a stale Page generation must reject owner commit");

        assert_eq!(
            error,
            BrowserPageReplacementCommitError::PageResidence(
                BrowserPageResidenceRegistryError::StaleTransition {
                    owner: key.clone(),
                    expected_generation: 0,
                    current_generation: 1,
                }
            )
        );
        assert!(owner.accepts_pending_document_navigation(&key, &navigation));
        assert!(!owner.accepts_committed_document_navigation(&key, &navigation));
        let (_, history) = owner.navigation_history_snapshot(&key, None);
        assert!(history.is_empty());
    }

    #[test]
    fn replacement_uses_core_registered_capability_without_frontend_handle() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let current_slot = register_target(&mut owner, &key);
        let other_slot = BrowserPageResidenceHandle::default();
        let navigation = owner.start_document_navigation(&key, "loader-1".to_owned());

        assert!(!owner.page_residence_handle_is_current(&key, &other_slot));
        let permit = owner
            .prepare_loaded_page_replacement(&key, &navigation)
            .expect("Core must resolve its registered Page capability");
        owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                permit,
                page("https://example.test/next"),
            )
            .expect("exact replacement should commit");
        assert_eq!(current_slot.generation(), 1);
        assert_eq!(other_slot.generation(), 0);
    }

    #[test]
    fn replacement_derives_initial_history_seed_and_exit_inside_owner_commit() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        register_target(&mut owner, &key);
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank#created"),
            )
            .expect("live Target should accept initial metadata");
        let navigation = owner.start_document_navigation(&key, "loader-1".to_owned());
        let permit = owner
            .prepare_loaded_page_replacement(&key, &navigation)
            .expect("exact navigation should prepare replacement");

        owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                permit,
                page("https://example.test/next"),
            )
            .expect("exact replacement should commit");

        let initial = owner
            .target_initial_empty_document(&key)
            .expect("creation metadata remains after replacement");
        assert!(initial.exited());
        let (_, history) = owner.navigation_history_snapshot(&key, None);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].url, "about:blank#created");
        assert_eq!(history[1].url, "https://example.test/next");
    }
}
