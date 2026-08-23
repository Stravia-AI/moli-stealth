use crate::browser_host::PageResidenceIdentity;
use crate::page::RendererPageLifetimeOwner;

use super::{
    BrowserDocumentNavigation, BrowserNavigationOwner, BrowserNavigationTraceEvent,
    BrowserPageOwnerKey, BrowserPageResidenceRegistryError, BrowserTargetRegistryError,
    BrowserTargetResidence, target_runtime_registry::BrowserTargetRuntimeRegistry,
};

/// Browser-level terminal transition for one exact Target residence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserTargetTerminationKind {
    Crash,
    Close,
}

/// Immutable request captured while an exact Target/Page slot is live.
///
/// The request contains no frontend session or command identity. Page
/// generation is part of the capability so a delayed action from a replaced
/// Page cannot terminate its successor.
#[derive(Debug)]
pub struct BrowserTargetTerminationRequest {
    owner: BrowserPageOwnerKey,
    page: PageResidenceIdentity,
    kind: BrowserTargetTerminationKind,
}

impl BrowserTargetTerminationRequest {
    pub fn owner(&self) -> &BrowserPageOwnerKey {
        &self.owner
    }

    pub fn page(&self) -> &PageResidenceIdentity {
        &self.page
    }

    pub fn kind(&self) -> BrowserTargetTerminationKind {
        self.kind
    }
}

/// Exact authorization to commit one Target terminal transition.
///
/// Preparing this permit is read-only. Browser Core revalidates it at commit
/// so participant/projection preparation cannot make stale work current.
#[derive(Debug)]
pub struct BrowserTargetTerminationPermit {
    request: BrowserTargetTerminationRequest,
}

impl BrowserTargetTerminationPermit {
    pub fn request(&self) -> &BrowserTargetTerminationRequest {
        &self.request
    }
}

/// Browser-owned result after the terminal transition commits.
#[derive(Debug)]
pub struct BrowserTargetTermination {
    owner: BrowserPageOwnerKey,
    previous_page: PageResidenceIdentity,
    terminal_page: PageResidenceIdentity,
    kind: BrowserTargetTerminationKind,
    closed_target_residence: Option<BrowserTargetResidence>,
    retired_renderer_page_owner: Option<RendererPageLifetimeOwner>,
}

impl BrowserTargetTermination {
    pub fn owner(&self) -> &BrowserPageOwnerKey {
        &self.owner
    }

    pub fn previous_page(&self) -> &PageResidenceIdentity {
        &self.previous_page
    }

    /// Exact successor generation representing the now-absent physical Page.
    /// Protocol migration storage must project this generation without
    /// advancing the shared handle a second time.
    pub fn terminal_page(&self) -> &PageResidenceIdentity {
        &self.terminal_page
    }

    pub fn kind(&self) -> BrowserTargetTerminationKind {
        self.kind
    }

    pub fn closed_target_residence(&self) -> Option<BrowserTargetResidence> {
        self.closed_target_residence
    }

    /// Takes the exact renderer Page lifetime retired by this terminal turn.
    pub fn take_retired_renderer_page_owner(&mut self) -> Option<RendererPageLifetimeOwner> {
        self.retired_renderer_page_owner.take()
    }
}

/// Exact reason why a prepared Target terminal transition no longer commits.
///
/// A permit may cross an actor turn. Target lifecycle, topology, and Page
/// generation are therefore ordinary commit guards rather than process-fatal
/// invariants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserTargetTerminationCommitError {
    Target(BrowserTargetRegistryError),
    PageResidence(BrowserPageResidenceRegistryError),
    TargetNoLongerAcceptsTermination {
        owner: BrowserPageOwnerKey,
        kind: BrowserTargetTerminationKind,
    },
}

impl std::fmt::Display for BrowserTargetTerminationCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Target(error) => error.fmt(formatter),
            Self::PageResidence(error) => error.fmt(formatter),
            Self::TargetNoLongerAcceptsTermination { owner, kind } => write!(
                formatter,
                "Target {:?} in BrowserContext {:?} no longer accepts {kind:?}",
                owner.target_id(),
                owner.browser_context_id()
            ),
        }
    }
}

impl std::error::Error for BrowserTargetTerminationCommitError {}

impl From<BrowserTargetRegistryError> for BrowserTargetTerminationCommitError {
    fn from(error: BrowserTargetRegistryError) -> Self {
        Self::Target(error)
    }
}

impl From<BrowserPageResidenceRegistryError> for BrowserTargetTerminationCommitError {
    fn from(error: BrowserPageResidenceRegistryError) -> Self {
        Self::PageResidence(error)
    }
}

#[derive(Clone, Debug)]
pub(super) enum BrowserTargetTerminationState {
    Crashed,
    Recovering(BrowserDocumentNavigation),
    Closed,
}

#[derive(Debug)]
struct BrowserTargetTerminationCommitRollback {
    previous: Option<BrowserTargetTerminationState>,
}

/// Minimal Target lifecycle authority during the Phase-2 migration.
///
/// Closed targets disappear from the physical registry immediately after the
/// owner commit. Crashed targets remain addressable and may either close or
/// start one exact recovery navigation. Tracking that recovery request keeps a
/// failed/superseded recovery from accidentally authorizing an unrelated Page
/// replacement.
#[derive(Default)]
pub(super) struct BrowserTargetTerminationRegistry;

impl BrowserTargetTerminationRegistry {
    pub(super) fn accepts_termination(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
        kind: BrowserTargetTerminationKind,
    ) -> bool {
        match kind {
            BrowserTargetTerminationKind::Crash => self.state(runtimes, owner).is_none(),
            BrowserTargetTerminationKind::Close => !matches!(
                self.state(runtimes, owner),
                Some(BrowserTargetTerminationState::Closed)
            ),
        }
    }

    pub(super) fn accepts_page_replacement(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        match self.state(runtimes, owner) {
            None => true,
            Some(
                BrowserTargetTerminationState::Crashed | BrowserTargetTerminationState::Closed,
            ) => false,
            Some(BrowserTargetTerminationState::Recovering(expected)) => expected == navigation,
        }
    }

    pub(super) fn begin_navigation(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) {
        if matches!(
            self.state(runtimes, owner),
            Some(
                BrowserTargetTerminationState::Crashed
                    | BrowserTargetTerminationState::Recovering(_)
            )
        ) {
            runtimes
                .entries
                .entry(owner.clone())
                .or_default()
                .termination = Some(BrowserTargetTerminationState::Recovering(
                navigation.clone(),
            ));
        }
    }

    pub(super) fn cancel_navigation_if_matches(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) {
        if self.state(runtimes, owner).is_some_and(|state| {
            matches!(
                state,
                BrowserTargetTerminationState::Recovering(expected) if expected == navigation
            )
        }) {
            runtimes
                .entries
                .entry(owner.clone())
                .or_default()
                .termination = Some(BrowserTargetTerminationState::Crashed);
        }
    }

    pub(super) fn commit_navigation(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) {
        if self.state(runtimes, owner).is_some_and(|state| {
            matches!(
                state,
                BrowserTargetTerminationState::Recovering(expected) if expected == navigation
            )
        }) {
            if let Some(runtime) = runtimes.entries.get_mut(owner) {
                runtime.termination = None;
            }
            runtimes.prune_empty();
        }
    }

    fn commit_with_rollback_if_accepted(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
        kind: BrowserTargetTerminationKind,
    ) -> Option<BrowserTargetTerminationCommitRollback> {
        if !self.accepts_termination(runtimes, owner, kind) {
            return None;
        }
        let state = match kind {
            BrowserTargetTerminationKind::Crash => BrowserTargetTerminationState::Crashed,
            BrowserTargetTerminationKind::Close => BrowserTargetTerminationState::Closed,
        };
        let previous = runtimes
            .entries
            .entry(owner.clone())
            .or_default()
            .termination
            .replace(state);
        Some(BrowserTargetTerminationCommitRollback { previous })
    }

    fn rollback_commit(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
        kind: BrowserTargetTerminationKind,
        rollback: BrowserTargetTerminationCommitRollback,
    ) -> bool {
        let current_matches = matches!(
            (kind, self.state(runtimes, owner)),
            (
                BrowserTargetTerminationKind::Crash,
                Some(BrowserTargetTerminationState::Crashed)
            ) | (
                BrowserTargetTerminationKind::Close,
                Some(BrowserTargetTerminationState::Closed)
            )
        );
        if !current_matches {
            return false;
        }
        if let Some(runtime) = runtimes.entries.get_mut(owner) {
            runtime.termination = rollback.previous;
        }
        runtimes.prune_empty();
        true
    }

    fn state<'a>(
        &self,
        runtimes: &'a BrowserTargetRuntimeRegistry,
        owner: &BrowserPageOwnerKey,
    ) -> Option<&'a BrowserTargetTerminationState> {
        runtimes
            .entries
            .get(owner)
            .and_then(|runtime| runtime.termination.as_ref())
    }
}

impl BrowserNavigationOwner {
    /// Captures a protocol-neutral terminal request from the Target's exact
    /// physical Page slot. Capture does not change browser lifecycle state.
    pub fn capture_target_termination(
        &self,
        owner: &BrowserPageOwnerKey,
        kind: BrowserTargetTerminationKind,
    ) -> Option<BrowserTargetTerminationRequest> {
        if self.targets.validate_target_owner(owner).is_err()
            || !self
                .target_terminations
                .accepts_termination(&self.target_runtimes, owner, kind)
        {
            return None;
        }
        let page = self
            .page_residences
            .capture_termination(&self.target_runtimes, owner)?;
        Some(BrowserTargetTerminationRequest {
            owner: owner.clone(),
            page,
            kind,
        })
    }

    pub fn prepare_target_termination(
        &self,
        request: BrowserTargetTerminationRequest,
    ) -> Option<BrowserTargetTerminationPermit> {
        if !self.target_terminations.accepts_termination(
            &self.target_runtimes,
            request.owner(),
            request.kind(),
        ) || self.targets.validate_target_owner(request.owner()).is_err()
            || request.page().browser_context_id() != request.owner().browser_context_id()
            || request.page().target_id() != Some(request.owner().target_id())
            || !self.page_residences.accepts_transition(
                &self.target_runtimes,
                request.owner(),
                request.page(),
            )
        {
            return None;
        }
        Some(BrowserTargetTerminationPermit { request })
    }

    /// Commits Target lifecycle, Page generation, request state, runtime
    /// ownership, and history in one Browser Owner mutation.
    pub fn commit_target_termination(
        &mut self,
        permit: BrowserTargetTerminationPermit,
    ) -> Result<BrowserTargetTermination, BrowserTargetTerminationCommitError> {
        let request = permit.request;
        let kind = request.kind();
        self.targets.validate_target_owner(request.owner())?;
        let selected_engine_owner = self.selected_target_engine_owner().cloned();
        let Some(termination_rollback) = self.target_terminations.commit_with_rollback_if_accepted(
            &mut self.target_runtimes,
            request.owner(),
            kind,
        ) else {
            return Err(
                BrowserTargetTerminationCommitError::TargetNoLongerAcceptsTermination {
                    owner: request.owner().clone(),
                    kind,
                },
            );
        };
        let mut target_removal = if kind == BrowserTargetTerminationKind::Close {
            match self.targets.begin_target_removal(request.owner()) {
                Ok(removal) => Some(removal),
                Err(error) => {
                    let rolled_back = self.target_terminations.rollback_commit(
                        &mut self.target_runtimes,
                        request.owner(),
                        kind,
                        termination_rollback,
                    );
                    debug_assert!(
                        rolled_back,
                        "same-turn Target removal rejection must restore termination state"
                    );
                    return Err(error.into());
                }
            }
        } else {
            None
        };
        let (terminal_page, retired_renderer_page_owner) =
            match self.page_residences.commit_termination(
                &mut self.target_runtimes,
                request.owner(),
                request.page(),
                kind == BrowserTargetTerminationKind::Close,
            ) {
                Ok(committed) => committed,
                Err(error) => {
                    if let Some(removal) = target_removal.take() {
                        let rolled_back = self.targets.rollback_target_removal(removal);
                        debug_assert!(
                            rolled_back,
                            "same-turn stale Page commit must restore Target topology"
                        );
                    }
                    let rolled_back = self.target_terminations.rollback_commit(
                        &mut self.target_runtimes,
                        request.owner(),
                        kind,
                        termination_rollback,
                    );
                    debug_assert!(
                        rolled_back,
                        "same-turn stale Page commit must restore termination state"
                    );
                    return Err(error.into());
                }
            };

        let closed_target_residence =
            target_removal.map(|removal| self.targets.commit_target_removal(removal));

        let target_id = request.owner().target_id();
        let pending_navigation = self
            .document_navigations
            .pending_record(&self.target_runtimes, request.owner())
            .cloned();
        self.target_engines.retire_target(
            &mut self.target_runtimes,
            selected_engine_owner.as_ref(),
            request.owner(),
            kind == BrowserTargetTerminationKind::Close,
        );
        match kind {
            BrowserTargetTerminationKind::Crash => {
                self.document_navigations
                    .forget_target(&mut self.target_runtimes, target_id);
                self.navigation_histories
                    .clear(&mut self.target_runtimes, request.owner());
                self.initial_empty_documents
                    .mark_exited(&mut self.target_runtimes, request.owner());
            }
            BrowserTargetTerminationKind::Close => {
                let removed = self.target_runtimes.remove(request.owner());
                debug_assert!(
                    removed.is_some(),
                    "closed Target must retire its exact aggregate runtime record"
                );
            }
        }
        let termination = BrowserTargetTermination {
            owner: request.owner,
            previous_page: request.page,
            terminal_page,
            kind,
            closed_target_residence,
            retired_renderer_page_owner,
        };
        if let Err(error) = self.record_target_termination_facts(
            &termination,
            pending_navigation
                .as_ref()
                .map(|record| record.navigation()),
        ) {
            tracing::error!(
                %error,
                browser_context_id = termination.owner().browser_context_id(),
                target_id = termination.owner().target_id(),
                previous_page_generation = termination.previous_page().loaded_page_generation(),
                terminal_page_generation = termination.terminal_page().loaded_page_generation(),
                kind = ?termination.kind(),
                "failed to publish committed Target termination Browser fact"
            );
        }
        if let Some(record) = pending_navigation
            && let Some(trace) = record.trace()
        {
            trace.emit(
                BrowserNavigationTraceEvent::new(
                    match kind {
                        BrowserTargetTerminationKind::Crash => {
                            "navigation_request_terminated_by_target_crash"
                        }
                        BrowserTargetTerminationKind::Close => {
                            "navigation_request_terminated_by_target_close"
                        }
                    },
                    trace.origin(),
                    "request-pending",
                    "target-terminal",
                )
                .with_navigation(record.navigation())
                .with_page(termination.terminal_page().clone()),
            );
        }
        Ok(termination)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        browser_host::{
            BrowserContextSelectionProjection, BrowserFact, BrowserInitialEmptyDocumentSeed,
            BrowserNavigationHistoryPageSnapshot, BrowserNavigationHistorySeed,
            BrowserPageResidenceHandle, BrowserSelectedTargetEngineDisposition,
            BrowserTargetEngineResidence, BrowserTargetHandle, BrowserTargetResidence,
            BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
        },
        runtime::NavigationEngine,
    };

    use super::*;

    fn register_test_target(
        owner: &mut BrowserNavigationOwner,
        key: &BrowserPageOwnerKey,
        residence: BrowserTargetResidence,
    ) -> BrowserPageResidenceHandle {
        let handle = BrowserTargetHandle::staged(key.target_id());
        let page_residence = BrowserPageResidenceHandle::default();
        let slot = BrowserTargetSlotProjection::new(handle, page_residence.clone());
        let (active, background) = match residence {
            BrowserTargetResidence::Active => (Some(slot), Vec::new()),
            BrowserTargetResidence::Background => (None, vec![slot]),
        };
        owner
            .register_browser_context(
                key.browser_context_id(),
                BrowserTargetTopologyProjection::new(key.browser_context_id(), active, background),
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
    fn close_commits_exactly_once_and_retires_all_target_runtime_state() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence =
            register_test_target(&mut owner, &key, BrowserTargetResidence::Background);
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank"),
            )
            .expect("live target should accept initial metadata");
        let target_handle = owner.target_handle("target-1").expect("live Target handle");
        owner
            .adopt_target_engine(
                key.clone(),
                BrowserTargetEngineResidence::Retained,
                NavigationEngine::new(),
            )
            .expect("test target should retain its engine");
        let navigation = owner.start_document_navigation(&key, "loader-1".to_owned());
        owner.record_loaded_page_navigation_history(
            &key,
            BrowserNavigationHistoryPageSnapshot::new("https://example.test/", "title"),
        );
        let request = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Close)
            .expect("live target should capture close");
        let previous = request.page().clone();
        let permit = owner
            .prepare_target_termination(request)
            .expect("exact close should prepare");
        let mut subscriber = owner.subscribe_browser_facts();

        let termination = owner
            .commit_target_termination(permit)
            .expect("exact close should commit");

        assert_eq!(termination.previous_page(), &previous);
        assert_eq!(
            termination.closed_target_residence(),
            Some(BrowserTargetResidence::Background)
        );
        assert_eq!(termination.terminal_page().loaded_page_generation(), 1);
        assert!(page_residence.is_current(termination.terminal_page()));
        assert!(owner.page_owner_key_if_current(&previous).is_none());
        assert!(
            owner
                .capture_target_termination(&key, BrowserTargetTerminationKind::Close)
                .is_none(),
            "closed target must not accept another terminal action"
        );
        assert!(!owner.accepts_pending_document_navigation(&key, &navigation));
        assert_eq!(owner.retained_background_engine_count(), 0);
        assert!(
            !owner.target_runtimes.entries.contains_key(&key),
            "Target close must remove its aggregate runtime record"
        );
        assert!(target_handle.is_retired());
        assert!(owner.target_initial_empty_document(&key).is_none());
        let created = subscriber
            .try_recv()
            .expect("Target registration should publish its creation occurrence");
        assert_eq!(created.sequence().get(), 1);
        assert_eq!(created.page_residence(), &previous);
        assert!(matches!(created.fact(), BrowserFact::TargetCreated));
        let accepted = subscriber
            .try_recv()
            .expect("navigation start should publish NavigationAccepted");
        assert_eq!(accepted.sequence().get(), 2);
        assert_eq!(accepted.page_residence(), &previous);
        assert_eq!(
            accepted.fact(),
            &BrowserFact::NavigationAccepted {
                navigation: navigation.clone(),
                superseded_navigation: None,
            }
        );
        let fact = subscriber
            .try_recv()
            .expect("close commit should publish its self-contained TargetClosed fact");
        assert_eq!(fact.sequence().get(), 3);
        assert_eq!(fact.browser_context_id().as_str(), "context-1");
        assert_eq!(fact.target_id().as_str(), "target-1");
        assert_eq!(fact.page_residence(), termination.terminal_page());
        assert_eq!(
            fact.fact(),
            &BrowserFact::TargetClosed {
                previous_page: previous.clone(),
                pending_navigation: Some(navigation),
            }
        );
        let (_, reseeded) = owner.navigation_history_snapshot(
            &key,
            Some(BrowserNavigationHistorySeed::initial_empty_document(
                "about:blank",
            )),
        );
        assert_eq!(reseeded.len(), 1, "closed target history must be forgotten");
    }

    #[test]
    fn crash_preserves_empty_history_tombstone_and_cannot_be_recaptured() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_test_target(&mut owner, &key, BrowserTargetResidence::Active);
        owner
            .register_target_initial_empty_document(
                &key,
                BrowserInitialEmptyDocumentSeed::new("about:blank"),
            )
            .expect("live target should accept initial metadata");
        let target_handle = owner.target_handle("target-1").expect("live Target handle");
        owner.record_loaded_page_navigation_history(
            &key,
            BrowserNavigationHistoryPageSnapshot::new("https://example.test/", "title"),
        );
        let navigation = owner.start_document_navigation(&key, "loader-pending".to_owned());
        let request = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Crash)
            .expect("live target should capture crash");
        let previous = request.page().clone();
        let permit = owner
            .prepare_target_termination(request)
            .expect("exact crash should prepare");
        let mut subscriber = owner.subscribe_browser_facts();
        let termination = owner
            .commit_target_termination(permit)
            .expect("exact crash should commit");
        assert_eq!(termination.closed_target_residence(), None);
        assert_eq!(page_residence.generation(), 1);
        assert!(
            target_handle.is_live(),
            "crash must preserve Target lifetime"
        );
        assert!(
            owner
                .target_initial_empty_document(&key)
                .is_some_and(|initial| initial.exited()),
            "crash must retire the destroyed initial Document without forgetting Target metadata"
        );

        assert!(
            owner
                .capture_target_termination(&key, BrowserTargetTerminationKind::Crash)
                .is_none(),
            "crashed target must not accept another terminal action"
        );
        let (_, history) = owner.navigation_history_snapshot(
            &key,
            Some(BrowserNavigationHistorySeed::initial_empty_document(
                "about:blank",
            )),
        );
        assert!(history.is_empty(), "crash must retain an empty tombstone");
        let created = subscriber
            .try_recv()
            .expect("Target registration should publish its creation occurrence");
        assert_eq!(created.sequence().get(), 1);
        assert_eq!(created.page_residence(), &previous);
        assert!(matches!(created.fact(), BrowserFact::TargetCreated));
        let accepted = subscriber
            .try_recv()
            .expect("pending navigation should publish NavigationAccepted");
        assert_eq!(accepted.sequence().get(), 2);
        assert_eq!(accepted.page_residence(), &previous);
        assert_eq!(
            accepted.fact(),
            &BrowserFact::NavigationAccepted {
                navigation: navigation.clone(),
                superseded_navigation: None,
            }
        );
        let fact = subscriber
            .try_recv()
            .expect("crash commit should publish its self-contained TargetCrashed fact");
        assert_eq!(fact.sequence().get(), 3);
        assert_eq!(fact.page_residence(), termination.terminal_page());
        assert_eq!(
            fact.fact(),
            &BrowserFact::TargetCrashed {
                previous_page: previous,
                pending_navigation: Some(navigation),
            }
        );
    }

    #[test]
    fn crashed_target_recovers_through_one_exact_navigation() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_test_target(&mut owner, &key, BrowserTargetResidence::Active);
        let request = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Crash)
            .expect("live target should capture crash");
        let permit = owner
            .prepare_target_termination(request)
            .expect("exact crash should prepare");
        owner
            .commit_target_termination(permit)
            .expect("exact crash should commit");

        let recovery = owner.start_document_navigation(&key, "loader-recovery".to_owned());
        let permit = owner
            .prepare_loaded_page_replacement(&key, &recovery)
            .expect("crashed target should authorize its exact recovery navigation");
        owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                permit,
                BrowserNavigationHistoryPageSnapshot::new(
                    "https://example.test/recovered",
                    "title",
                ),
            )
            .expect("exact recovery replacement should commit");
        assert_eq!(page_residence.generation(), 2);

        assert!(
            owner
                .capture_target_termination(&key, BrowserTargetTerminationKind::Crash)
                .is_some(),
            "successfully recovered target should become crashable again"
        );
    }

    #[test]
    fn failed_crash_recovery_does_not_authorize_an_unrelated_replacement() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let _page_residence =
            register_test_target(&mut owner, &key, BrowserTargetResidence::Active);
        let request = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Crash)
            .expect("live target should capture crash");
        let permit = owner
            .prepare_target_termination(request)
            .expect("exact crash should prepare");
        owner
            .commit_target_termination(permit)
            .expect("exact crash should commit");

        let failed = owner.start_document_navigation(&key, "loader-failed".to_owned());
        assert!(owner.fail_document_navigation_if_matches(
            &key,
            &failed,
            crate::browser_host::BrowserNavigationFailure::Network {
                error_text: "failed".to_owned(),
            },
        ));
        let unrelated = BrowserDocumentNavigation::new("target-1", "loader-unrelated");

        assert!(
            owner
                .prepare_loaded_page_replacement(&key, &failed)
                .is_none()
        );
        assert!(
            owner
                .prepare_loaded_page_replacement(&key, &unrelated)
                .is_none()
        );
    }

    #[test]
    fn crashed_target_can_still_be_closed() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let _page_residence =
            register_test_target(&mut owner, &key, BrowserTargetResidence::Active);
        owner
            .adopt_target_engine(
                key.clone(),
                BrowserTargetEngineResidence::Selected,
                NavigationEngine::new(),
            )
            .expect("live target should own the selected engine");
        let crash = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Crash)
            .expect("live target should capture crash");
        let crash = owner
            .prepare_target_termination(crash)
            .expect("exact crash should prepare");
        owner
            .commit_target_termination(crash)
            .expect("exact crash should commit");
        assert_eq!(
            owner.selected_target_engine_owner(),
            Some(&key),
            "crash must keep the exact Target engine selected for recovery"
        );

        let close = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Close)
            .expect("crashed target should remain closable");
        let close = owner
            .prepare_target_termination(close)
            .expect("exact crashed-target close should prepare");
        let closed = owner
            .commit_target_termination(close)
            .expect("exact crashed-target close should commit");

        assert_eq!(closed.kind(), BrowserTargetTerminationKind::Close);
        assert_eq!(
            closed.closed_target_residence(),
            Some(BrowserTargetResidence::Active)
        );
        assert_eq!(closed.terminal_page().loaded_page_generation(), 2);
        let facts = owner.browser_fact_snapshot();
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0].sequence().get(), 1);
        assert_eq!(facts[1].sequence().get(), 2);
        assert_eq!(facts[2].sequence().get(), 3);
        assert!(matches!(facts[0].fact(), BrowserFact::TargetCreated));
        assert!(matches!(
            facts[1].fact(),
            BrowserFact::TargetCrashed { previous_page, .. }
                if previous_page.loaded_page_generation() == 0
        ));
        assert!(matches!(
            facts[2].fact(),
            BrowserFact::TargetClosed { previous_page, .. }
                if previous_page == facts[1].page_residence()
        ));
        assert_eq!(facts[2].page_residence(), closed.terminal_page());
        assert_eq!(
            owner.selected_target_engine_owner(),
            None,
            "close must not leave the selected engine keyed to a dead Target"
        );
    }

    #[test]
    fn page_replacement_makes_delayed_termination_request_stale() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_test_target(&mut owner, &key, BrowserTargetResidence::Active);
        let request = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Close)
            .expect("live target should capture close");

        page_residence.advance_generation_for_test_fixture();

        assert!(owner.prepare_target_termination(request).is_none());
    }

    #[test]
    fn stale_close_commit_restores_target_and_preserves_runtime_state() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence =
            register_test_target(&mut owner, &key, BrowserTargetResidence::Background);
        let target_handle = owner.target_handle("target-1").expect("live Target handle");
        owner
            .adopt_target_engine(
                key.clone(),
                BrowserTargetEngineResidence::Retained,
                NavigationEngine::new(),
            )
            .expect("test target should retain its engine");
        let navigation = owner.start_document_navigation(&key, "loader-1".to_owned());
        owner.record_loaded_page_navigation_history(
            &key,
            BrowserNavigationHistoryPageSnapshot::new("https://example.test/", "title"),
        );
        let request = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Close)
            .expect("live target should capture close");
        let permit = owner
            .prepare_target_termination(request)
            .expect("exact close should prepare");

        page_residence.advance_generation_for_test_fixture();
        let error = owner
            .commit_target_termination(permit)
            .expect_err("stale Page generation must reject close");

        assert!(matches!(
            error,
            BrowserTargetTerminationCommitError::PageResidence(
                BrowserPageResidenceRegistryError::StaleTransition {
                    expected_generation: 0,
                    current_generation: 1,
                    ..
                }
            )
        ));
        assert!(target_handle.is_live());
        assert!(owner.has_target("target-1"));
        assert_eq!(owner.browser_context_target_count("context-1"), 1);
        assert_eq!(owner.retained_background_engine_count(), 1);
        assert!(owner.accepts_pending_document_navigation(&key, &navigation));
        let (_, history) = owner.navigation_history_snapshot(&key, None);
        assert_eq!(history.len(), 1, "stale close must not clear history");
        assert!(
            owner
                .capture_target_termination(&key, BrowserTargetTerminationKind::Close)
                .is_some(),
            "stale close must restore the previous termination state"
        );
        assert!(
            owner.browser_fact_snapshot().iter().all(|fact| !matches!(
                fact.fact(),
                BrowserFact::NavigationFailed { .. }
                    | BrowserFact::TargetCrashed { .. }
                    | BrowserFact::TargetClosed { .. }
            )),
            "a stale termination commit must not publish a Target terminal fact"
        );
    }

    #[test]
    fn delayed_crash_commit_is_rejected_while_target_is_recovering() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_test_target(&mut owner, &key, BrowserTargetResidence::Active);
        let first = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Crash)
            .expect("live target should capture first crash");
        let delayed = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Crash)
            .expect("live target should capture delayed crash");
        let first = owner
            .prepare_target_termination(first)
            .expect("first crash should prepare");
        let delayed = owner
            .prepare_target_termination(delayed)
            .expect("delayed crash should prepare while target is live");
        owner
            .commit_target_termination(first)
            .expect("first crash should commit");
        let recovery = owner.start_document_navigation(&key, "loader-recovery".to_owned());

        let error = owner
            .commit_target_termination(delayed)
            .expect_err("recovering target must reject delayed crash");

        assert_eq!(
            error,
            BrowserTargetTerminationCommitError::TargetNoLongerAcceptsTermination {
                owner: key.clone(),
                kind: BrowserTargetTerminationKind::Crash,
            }
        );
        assert_eq!(page_residence.generation(), 1);
        assert!(owner.accepts_pending_document_navigation(&key, &recovery));
        assert!(
            owner
                .prepare_loaded_page_replacement(&key, &recovery)
                .is_some(),
            "rejected delayed crash must preserve exact recovery authorization"
        );
    }

    #[test]
    fn stale_recovery_close_restores_recovering_state_and_target_topology() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let page_residence = register_test_target(&mut owner, &key, BrowserTargetResidence::Active);
        let target_handle = owner.target_handle("target-1").expect("live Target handle");
        let crash = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Crash)
            .expect("live target should capture crash");
        let crash = owner
            .prepare_target_termination(crash)
            .expect("exact crash should prepare");
        owner
            .commit_target_termination(crash)
            .expect("exact crash should commit");
        let recovery = owner.start_document_navigation(&key, "loader-recovery".to_owned());
        let close = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Close)
            .expect("recovering target should capture close");
        let close = owner
            .prepare_target_termination(close)
            .expect("exact recovering-target close should prepare");

        page_residence.advance_generation_for_test_fixture();
        let error = owner
            .commit_target_termination(close)
            .expect_err("stale recovering-target close must be rejected");

        assert!(matches!(
            error,
            BrowserTargetTerminationCommitError::PageResidence(
                BrowserPageResidenceRegistryError::StaleTransition { .. }
            )
        ));
        assert!(target_handle.is_live());
        assert!(owner.has_target("target-1"));
        assert_eq!(
            owner.active_target_id_for_browser_context("context-1"),
            Some("target-1")
        );
        assert!(owner.accepts_pending_document_navigation(&key, &recovery));
        assert!(
            owner
                .prepare_loaded_page_replacement(&key, &recovery)
                .is_some(),
            "stale close rollback must restore Recovering, not Crashed or Closed"
        );
    }

    #[test]
    fn termination_capture_uses_core_registered_capability() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let current = register_test_target(&mut owner, &key, BrowserTargetResidence::Active);
        let other = BrowserPageResidenceHandle::default();

        assert!(!owner.page_residence_handle_is_current(&key, &other));
        let request = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Close)
            .expect("Core must capture from its registered Page capability");
        let permit = owner
            .prepare_target_termination(request)
            .expect("exact Core capture should prepare");
        owner
            .commit_target_termination(permit)
            .expect("exact close should commit");
        assert_eq!(current.generation(), 1);
        assert_eq!(other.generation(), 0);
    }
}
