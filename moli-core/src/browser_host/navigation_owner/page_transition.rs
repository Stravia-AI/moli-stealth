use crate::browser_host::{
    BrowserPageRuntimeAccess, BrowserPageRuntimeOwner, PageResidenceIdentity,
};
use crate::page::RendererPageLifetimeOwner;

use super::{
    BrowserDocumentNavigation, BrowserNavigationFailure, BrowserNavigationOwner,
    BrowserNavigationTraceEvent, BrowserPageOwnerKey, BrowserPageResidenceRegistryError,
    BrowserTargetRegistryError,
};

/// Browser-owned reason for changing one Target's Page residence outside a
/// successful loaded-Document replacement or Target termination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserPageResidenceTransitionKind {
    InitialDocumentMaterialization,
    FailedNavigationDiscard,
}

#[derive(Debug)]
enum BrowserPageResidenceTransitionRequest {
    InitialDocumentMaterialization,
    FailedNavigationDiscard {
        navigation: BrowserDocumentNavigation,
        failure: BrowserNavigationFailure,
    },
}

impl BrowserPageResidenceTransitionRequest {
    fn kind(&self) -> BrowserPageResidenceTransitionKind {
        match self {
            Self::InitialDocumentMaterialization => {
                BrowserPageResidenceTransitionKind::InitialDocumentMaterialization
            }
            Self::FailedNavigationDiscard { .. } => {
                BrowserPageResidenceTransitionKind::FailedNavigationDiscard
            }
        }
    }
}

/// Exact authorization for one Page residence transition.
///
/// Preparation freezes the Core-owned Target and Page generation without
/// mutating them. A physical Page projector may use `previous_page` only to
/// verify that it carries the same slot capability before Core commits.
#[derive(Debug)]
pub struct BrowserPageResidenceTransitionPermit {
    owner: BrowserPageOwnerKey,
    previous_page: PageResidenceIdentity,
    request: BrowserPageResidenceTransitionRequest,
}

impl BrowserPageResidenceTransitionPermit {
    pub fn owner(&self) -> &BrowserPageOwnerKey {
        &self.owner
    }

    pub fn previous_page(&self) -> &PageResidenceIdentity {
        &self.previous_page
    }

    pub fn kind(&self) -> BrowserPageResidenceTransitionKind {
        self.request.kind()
    }
}

/// Browser-owned result of committing a successor Page generation.
///
/// Protocol may synchronously project a physical `Page` payload or absence to
/// `current_page`, but cannot choose or advance that identity itself.
#[derive(Debug)]
pub struct BrowserPageResidenceTransition {
    owner: BrowserPageOwnerKey,
    previous_page: PageResidenceIdentity,
    current_page: PageResidenceIdentity,
    kind: BrowserPageResidenceTransitionKind,
    retired_renderer_page_owner: Option<RendererPageLifetimeOwner>,
    current_page_runtime: Option<BrowserPageRuntimeAccess>,
}

impl BrowserPageResidenceTransition {
    pub fn owner(&self) -> &BrowserPageOwnerKey {
        &self.owner
    }

    pub fn previous_page(&self) -> &PageResidenceIdentity {
        &self.previous_page
    }

    pub fn current_page(&self) -> &PageResidenceIdentity {
        &self.current_page
    }

    pub fn kind(&self) -> BrowserPageResidenceTransitionKind {
        self.kind
    }

    /// Takes the exact renderer Page lifetime retired by this transition.
    pub fn take_retired_renderer_page_owner(&mut self) -> Option<RendererPageLifetimeOwner> {
        self.retired_renderer_page_owner.take()
    }

    /// Non-owning access to the successor Page payload, when this transition
    /// materialized a concrete Page rather than Page absence.
    pub fn current_page_runtime(&self) -> Option<&BrowserPageRuntimeAccess> {
        self.current_page_runtime.as_ref()
    }
}

/// Exact reason why a prepared Page residence transition became stale.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserPageResidenceTransitionCommitError {
    Target(BrowserTargetRegistryError),
    PageResidence(BrowserPageResidenceRegistryError),
    InitialDocumentNoLongerMaterializable(BrowserPageOwnerKey),
    BrowserContextDisposing(BrowserPageOwnerKey),
    NavigationNoLongerPending {
        owner: BrowserPageOwnerKey,
        navigation: BrowserDocumentNavigation,
    },
    RendererPageOwnerMissing(BrowserPageOwnerKey),
    TransitionKindMismatch {
        expected: BrowserPageResidenceTransitionKind,
        actual: BrowserPageResidenceTransitionKind,
    },
}

impl std::fmt::Display for BrowserPageResidenceTransitionCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Target(error) => error.fmt(formatter),
            Self::PageResidence(error) => error.fmt(formatter),
            Self::InitialDocumentNoLongerMaterializable(owner) => write!(
                formatter,
                "initial Document for Target {:?} in BrowserContext {:?} is no longer materializable",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::BrowserContextDisposing(owner) => write!(
                formatter,
                "BrowserContext {:?} no longer accepts a Page transition for Target {:?}",
                owner.browser_context_id(),
                owner.target_id()
            ),
            Self::NavigationNoLongerPending { owner, navigation } => write!(
                formatter,
                "navigation {:?} is no longer pending for Target {:?} in BrowserContext {:?}",
                navigation,
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::RendererPageOwnerMissing(owner) => write!(
                formatter,
                "initial Document for Target {:?} in BrowserContext {:?} has no renderer Page lifetime owner",
                owner.target_id(),
                owner.browser_context_id()
            ),
            Self::TransitionKindMismatch { expected, actual } => write!(
                formatter,
                "Page residence commit expected {expected:?}, but permit carries {actual:?}"
            ),
        }
    }
}

impl std::error::Error for BrowserPageResidenceTransitionCommitError {}

impl From<BrowserTargetRegistryError> for BrowserPageResidenceTransitionCommitError {
    fn from(error: BrowserTargetRegistryError) -> Self {
        Self::Target(error)
    }
}

impl From<BrowserPageResidenceRegistryError> for BrowserPageResidenceTransitionCommitError {
    fn from(error: BrowserPageResidenceRegistryError) -> Self {
        Self::PageResidence(error)
    }
}

impl BrowserNavigationOwner {
    fn prepare_page_residence_transition(
        &self,
        owner: &BrowserPageOwnerKey,
        request: BrowserPageResidenceTransitionRequest,
    ) -> Option<BrowserPageResidenceTransitionPermit> {
        if !self.browser_context_accepts_owner_work(owner.browser_context_id()) {
            return None;
        }
        self.targets.validate_target_owner(owner).ok()?;
        let previous_page = self
            .page_residences
            .prepare_replacement(&self.target_runtimes, owner)?;
        Some(BrowserPageResidenceTransitionPermit {
            owner: owner.clone(),
            previous_page,
            request,
        })
    }

    /// Prepares the only transition allowed to materialize a Target's initial
    /// empty Document. Duplicate or exited creation records are rejected by
    /// Core before the physical Page can be installed.
    pub fn prepare_initial_document_page_materialization(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<BrowserPageResidenceTransitionPermit> {
        if !self
            .initial_empty_documents
            .accepts_materialization(&self.target_runtimes, owner)
        {
            return None;
        }
        self.prepare_page_residence_transition(
            owner,
            BrowserPageResidenceTransitionRequest::InitialDocumentMaterialization,
        )
    }

    /// Prepares retirement of the current Page after a document navigation
    /// failure invalidates the committed Document but leaves the Target alive.
    pub fn prepare_failed_navigation_page_discard(
        &self,
        owner: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
        failure: BrowserNavigationFailure,
    ) -> Option<BrowserPageResidenceTransitionPermit> {
        if navigation.target_id() != owner.target_id()
            || !self
                .document_navigations
                .accepts_pending(&self.target_runtimes, owner, navigation)
        {
            return None;
        }
        self.prepare_page_residence_transition(
            owner,
            BrowserPageResidenceTransitionRequest::FailedNavigationDiscard {
                navigation: navigation.clone(),
                failure,
            },
        )
    }

    /// Commits the Page generation and all browser-owned state associated with
    /// the selected transition. No participant work may await between physical
    /// permit validation and the matching projection. A permit may safely
    /// cross an actor turn because every authoritative input is revalidated.
    fn commit_page_residence_transition_inner(
        &mut self,
        permit: BrowserPageResidenceTransitionPermit,
        successor_renderer_page_owner: &mut Option<RendererPageLifetimeOwner>,
        successor_page_runtime_owner: &mut Option<BrowserPageRuntimeOwner>,
    ) -> Result<BrowserPageResidenceTransition, BrowserPageResidenceTransitionCommitError> {
        let kind = permit.kind();
        if !self.browser_context_accepts_owner_work(permit.owner.browser_context_id()) {
            return Err(
                BrowserPageResidenceTransitionCommitError::BrowserContextDisposing(permit.owner),
            );
        }
        self.targets.validate_target_owner(&permit.owner)?;
        if kind == BrowserPageResidenceTransitionKind::InitialDocumentMaterialization
            && !self
                .initial_empty_documents
                .accepts_materialization(&self.target_runtimes, &permit.owner)
        {
            return Err(
                BrowserPageResidenceTransitionCommitError::InitialDocumentNoLongerMaterializable(
                    permit.owner,
                ),
            );
        }

        let failed_navigation_record = match &permit.request {
            BrowserPageResidenceTransitionRequest::FailedNavigationDiscard {
                navigation, ..
            } => {
                let Some(record) = self.document_navigations.take_pending_if_matches(
                    &mut self.target_runtimes,
                    &permit.owner,
                    navigation,
                ) else {
                    return Err(
                        BrowserPageResidenceTransitionCommitError::NavigationNoLongerPending {
                            owner: permit.owner,
                            navigation: navigation.clone(),
                        },
                    );
                };
                Some(record)
            }
            BrowserPageResidenceTransitionRequest::InitialDocumentMaterialization => None,
        };
        let materialization_precommitted =
            kind == BrowserPageResidenceTransitionKind::InitialDocumentMaterialization;
        if materialization_precommitted
            && !self
                .initial_empty_documents
                .mark_materialized(&mut self.target_runtimes, &permit.owner)
        {
            return Err(
                BrowserPageResidenceTransitionCommitError::InitialDocumentNoLongerMaterializable(
                    permit.owner,
                ),
            );
        }
        let (current_page, retired_renderer_page_owner, current_page_runtime) = match self
            .page_residences
            .commit_transition_with_page_owners(
                &mut self.target_runtimes,
                &permit.owner,
                &permit.previous_page,
                successor_renderer_page_owner,
                successor_page_runtime_owner,
            ) {
            Ok(committed) => committed,
            Err(error) => {
                if materialization_precommitted {
                    let rolled_back = self
                        .initial_empty_documents
                        .rollback_materialized(&mut self.target_runtimes, &permit.owner);
                    debug_assert!(
                        rolled_back,
                        "same-turn stale Page generation must restore initial Document state"
                    );
                }
                if let Some(record) = failed_navigation_record {
                    let rolled_back = self.document_navigations.restore_pending_if_vacant(
                        &mut self.target_runtimes,
                        &permit.owner,
                        record,
                    );
                    debug_assert!(
                        rolled_back,
                        "same-turn stale Page generation must restore failed navigation authority"
                    );
                }
                return Err(error.into());
            }
        };

        match &permit.request {
            BrowserPageResidenceTransitionRequest::InitialDocumentMaterialization => {}
            BrowserPageResidenceTransitionRequest::FailedNavigationDiscard {
                navigation, ..
            } => {
                let selected_engine_owner = self.selected_target_engine_owner().cloned();
                self.target_engines.discard_target_page_runtime(
                    &mut self.target_runtimes,
                    selected_engine_owner.as_ref(),
                    permit.owner.target_id(),
                );
                self.document_navigations
                    .forget_target(&mut self.target_runtimes, permit.owner.target_id());
                self.target_terminations.cancel_navigation_if_matches(
                    &mut self.target_runtimes,
                    &permit.owner,
                    navigation,
                );
                self.initial_empty_documents
                    .mark_exited(&mut self.target_runtimes, &permit.owner);
            }
        }

        if let (
            Some(record),
            BrowserPageResidenceTransitionRequest::FailedNavigationDiscard {
                navigation,
                failure,
            },
        ) = (failed_navigation_record, &permit.request)
        {
            if let Err(error) = self.record_navigation_failed_fact(
                &permit.owner,
                navigation,
                failure.clone(),
                Some(permit.previous_page.clone()),
                &current_page,
            ) {
                tracing::error!(
                    %error,
                    browser_context_id = permit.owner.browser_context_id(),
                    target_id = permit.owner.target_id(),
                    previous_page_generation = permit.previous_page.loaded_page_generation(),
                    current_page_generation = current_page.loaded_page_generation(),
                    navigation_request_id = navigation.request_id().get(),
                    loader_id = navigation.loader_id(),
                    ?failure,
                    "failed to publish invalidating navigation failure Browser fact"
                );
            }
            if let Some(trace) = record.trace() {
                trace.emit(
                    BrowserNavigationTraceEvent::new(
                        "navigation_request_failed_and_page_discarded",
                        trace.origin(),
                        "request-pending",
                        "page-absent",
                    )
                    .with_navigation(navigation)
                    .with_page(current_page.clone()),
                );
            }
        }

        Ok(BrowserPageResidenceTransition {
            owner: permit.owner,
            previous_page: permit.previous_page,
            current_page,
            kind,
            retired_renderer_page_owner,
            current_page_runtime,
        })
    }

    /// Commits initial Document materialization and transfers its unique
    /// renderer Page lifetime to Browser Host in the same owner mutation.
    pub fn commit_initial_document_page_materialization(
        &mut self,
        permit: BrowserPageResidenceTransitionPermit,
        renderer_page_owner: &mut Option<RendererPageLifetimeOwner>,
        page_runtime_owner: &mut Option<BrowserPageRuntimeOwner>,
    ) -> Result<BrowserPageResidenceTransition, BrowserPageResidenceTransitionCommitError> {
        if permit.kind() != BrowserPageResidenceTransitionKind::InitialDocumentMaterialization {
            return Err(
                BrowserPageResidenceTransitionCommitError::TransitionKindMismatch {
                    expected: BrowserPageResidenceTransitionKind::InitialDocumentMaterialization,
                    actual: permit.kind(),
                },
            );
        }
        if renderer_page_owner.is_none() || page_runtime_owner.is_none() {
            return Err(
                BrowserPageResidenceTransitionCommitError::RendererPageOwnerMissing(permit.owner),
            );
        }
        self.commit_page_residence_transition_inner(permit, renderer_page_owner, page_runtime_owner)
    }

    /// Commits Page absence after a failed navigation and returns the retired
    /// renderer Page lifetime through the transition result.
    pub fn commit_failed_navigation_page_discard(
        &mut self,
        permit: BrowserPageResidenceTransitionPermit,
    ) -> Result<BrowserPageResidenceTransition, BrowserPageResidenceTransitionCommitError> {
        if permit.kind() != BrowserPageResidenceTransitionKind::FailedNavigationDiscard {
            return Err(
                BrowserPageResidenceTransitionCommitError::TransitionKindMismatch {
                    expected: BrowserPageResidenceTransitionKind::FailedNavigationDiscard,
                    actual: permit.kind(),
                },
            );
        }
        let mut no_successor_renderer = None;
        let mut no_successor_runtime = None;
        self.commit_page_residence_transition_inner(
            permit,
            &mut no_successor_renderer,
            &mut no_successor_runtime,
        )
    }

    /// Authority-only helper for registry tests that intentionally have no
    /// physical renderer Page payload.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn commit_page_residence_transition_without_renderer_owner_for_testing(
        &mut self,
        permit: BrowserPageResidenceTransitionPermit,
    ) -> Result<BrowserPageResidenceTransition, BrowserPageResidenceTransitionCommitError> {
        let mut no_successor_renderer = None;
        let mut no_successor_runtime = None;
        self.commit_page_residence_transition_inner(
            permit,
            &mut no_successor_renderer,
            &mut no_successor_runtime,
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        browser_host::{
            BrowserContextSelectionProjection, BrowserFact, BrowserInitialEmptyDocumentSeed,
            BrowserNavigationFailure, BrowserPageResidenceHandle,
            BrowserSelectedTargetEngineDisposition, BrowserTargetHandle,
            BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
        },
        runtime::NavigationEngine,
    };

    use super::*;

    fn owner_with_target() -> (
        BrowserNavigationOwner,
        BrowserPageOwnerKey,
        BrowserPageResidenceHandle,
    ) {
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = BrowserPageResidenceHandle::default();
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
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
        (owner, key, page_residence)
    }

    #[test]
    fn initial_document_materialization_advances_page_and_lifecycle_together() {
        let (mut owner, key, page_residence) = owner_with_target();
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank#initial"),
            )
            .expect("live Target should accept initial metadata");

        let permit = owner
            .prepare_initial_document_page_materialization(&key)
            .expect("current initial Document should prepare");
        let previous = permit.previous_page().clone();
        let transition = owner
            .commit_page_residence_transition_without_renderer_owner_for_testing(permit)
            .expect("exact initial materialization should commit");

        assert_eq!(transition.previous_page(), &previous);
        assert_eq!(transition.current_page().loaded_page_generation(), 1);
        assert_eq!(page_residence.generation(), 1);
        assert!(
            owner
                .target_initial_empty_document(&key)
                .expect("initial Document record")
                .materialized()
        );
        assert!(
            owner
                .prepare_initial_document_page_materialization(&key)
                .is_none(),
            "the materialization authority must be one-shot"
        );
    }

    #[test]
    fn stale_initial_document_permit_cannot_commit_after_another_transition() {
        let (mut owner, key, page_residence) = owner_with_target();
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank"),
            )
            .expect("live Target should accept initial metadata");
        let stale = owner
            .prepare_initial_document_page_materialization(&key)
            .expect("initial permit");
        page_residence.advance_generation_for_test_fixture();

        let error = owner
            .commit_page_residence_transition_without_renderer_owner_for_testing(stale)
            .expect_err("a successor generation must invalidate an old permit");
        assert_eq!(
            error,
            BrowserPageResidenceTransitionCommitError::PageResidence(
                BrowserPageResidenceRegistryError::StaleTransition {
                    owner: key.clone(),
                    expected_generation: 0,
                    current_generation: 1,
                }
            )
        );
        assert_eq!(page_residence.generation(), 1);
        assert!(
            !owner
                .target_initial_empty_document(&key)
                .expect("initial Document record")
                .materialized(),
            "a stale generation must roll back the precommitted lifecycle bit"
        );
    }

    #[test]
    fn failed_navigation_discard_advances_page_and_retires_runtime_state() {
        let (mut owner, key, page_residence) = owner_with_target();
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank"),
            )
            .expect("live Target should accept initial metadata");
        let materialize = owner
            .prepare_initial_document_page_materialization(&key)
            .expect("initial materialization");
        owner
            .commit_page_residence_transition_without_renderer_owner_for_testing(materialize)
            .expect("initial materialization should commit");
        let navigation = owner.start_document_navigation(&key, "loader-failed".to_owned());
        let failure = BrowserNavigationFailure::Network {
            error_text: "net::ERR_FAILED".to_owned(),
        };

        let permit = owner
            .prepare_failed_navigation_page_discard(&key, &navigation, failure.clone())
            .expect("live Target should prepare failed navigation discard");
        let previous = permit.previous_page().clone();
        let transition = owner
            .commit_page_residence_transition_without_renderer_owner_for_testing(permit)
            .expect("exact failed-navigation discard should commit");

        assert_eq!(
            transition.kind(),
            BrowserPageResidenceTransitionKind::FailedNavigationDiscard
        );
        assert_eq!(transition.current_page().loaded_page_generation(), 2);
        assert_eq!(page_residence.generation(), 2);
        assert!(!owner.has_pending_document_navigation(&key));
        assert!(!owner.accepts_pending_document_navigation(&key, &navigation));
        assert!(
            owner
                .target_initial_empty_document(&key)
                .expect("initial metadata remains diagnostic")
                .exited()
        );
        let facts = owner.browser_fact_snapshot();
        assert_eq!(facts.len(), 3);
        assert!(matches!(facts[0].fact(), BrowserFact::TargetCreated));
        assert_eq!(facts[2].page_residence(), transition.current_page());
        assert_eq!(
            facts[2].fact(),
            &BrowserFact::NavigationFailed {
                navigation,
                failure,
                previous_page: Some(previous),
            }
        );
    }

    #[test]
    fn stale_failed_navigation_discard_restores_exact_pending_authority() {
        let (mut owner, key, page_residence) = owner_with_target();
        let navigation = owner.start_document_navigation(&key, "loader-failed".to_owned());
        let permit = owner
            .prepare_failed_navigation_page_discard(
                &key,
                &navigation,
                BrowserNavigationFailure::Network {
                    error_text: "net::ERR_FAILED".to_owned(),
                },
            )
            .expect("exact failed navigation should prepare");
        page_residence.advance_generation_for_test_fixture();

        let error = owner
            .commit_page_residence_transition_without_renderer_owner_for_testing(permit)
            .expect_err("stale Page generation must reject failed discard");

        assert!(matches!(
            error,
            BrowserPageResidenceTransitionCommitError::PageResidence(
                BrowserPageResidenceRegistryError::StaleTransition { .. }
            )
        ));
        assert!(owner.accepts_pending_document_navigation(&key, &navigation));
        assert!(
            owner
                .browser_fact_snapshot()
                .iter()
                .all(|fact| !matches!(fact.fact(), BrowserFact::NavigationFailed { .. })),
            "a stale Page commit must not publish a terminal navigation fact"
        );
    }
}
