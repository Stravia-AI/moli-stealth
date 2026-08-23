use crate::browser_host::BrowserContextId;

use super::{
    BrowserDocumentNavigation, BrowserNavigationFailure, BrowserNavigationOwner,
    BrowserNavigationTraceContext, BrowserNavigationTraceEvent, BrowserPageOwnerKey,
    target_runtime_registry::BrowserTargetRuntimeRegistry,
};

#[derive(Clone)]
pub(super) struct BrowserDocumentNavigationRecord {
    navigation: BrowserDocumentNavigation,
    trace: Option<BrowserNavigationTraceContext>,
}

impl BrowserDocumentNavigationRecord {
    pub(super) fn navigation(&self) -> &BrowserDocumentNavigation {
        &self.navigation
    }

    pub(super) fn trace(&self) -> Option<&BrowserNavigationTraceContext> {
        self.trace.as_ref()
    }
}

#[derive(Default)]
pub(super) struct BrowserTargetDocumentNavigationState {
    pending: Option<BrowserDocumentNavigationRecord>,
    committed: Option<BrowserDocumentNavigationRecord>,
}

/// Rollback state for a same-turn request commit that precedes the shared
/// Page-generation compare/exchange.
pub(super) struct BrowserDocumentNavigationCommitRollback {
    previous_committed: Option<BrowserDocumentNavigationRecord>,
}

/// Authoritative cross-document request state keyed by browser Target.
///
/// Renderer attachment and lifecycle projections may retain the immutable
/// request token, but pending/committed acceptance is decided only here.
#[derive(Default)]
pub(super) struct BrowserDocumentNavigationRegistry;

impl BrowserDocumentNavigationRegistry {
    fn start(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
        loader_id: String,
        trace: Option<BrowserNavigationTraceContext>,
    ) -> (
        BrowserDocumentNavigation,
        Option<BrowserDocumentNavigationRecord>,
    ) {
        debug_assert!(
            trace
                .as_ref()
                .is_none_or(|trace| trace.addresses_owner(key)),
            "navigation trace context must address its exact Browser Page owner"
        );
        let navigation = BrowserDocumentNavigation::new(key.target_id(), loader_id);
        let record = BrowserDocumentNavigationRecord {
            navigation: navigation.clone(),
            trace: trace.filter(|trace| trace.addresses_owner(key)),
        };
        let state = self.state_mut_or_default(runtimes, key);
        let superseded = state.pending.replace(record);
        (navigation, superseded)
    }

    pub(super) fn accepts_pending(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        self.state(runtimes, key)
            .and_then(|state| state.pending.as_ref())
            .map(BrowserDocumentNavigationRecord::navigation)
            == Some(navigation)
    }

    fn accepts_body_completion(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        let Some(state) = self.state(runtimes, key) else {
            return false;
        };
        match state.pending.as_ref() {
            Some(pending) => pending.navigation() == navigation,
            None => state
                .committed
                .as_ref()
                .is_some_and(|committed| committed.navigation() == navigation),
        }
    }

    fn accepts_committed(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        self.state(runtimes, key)
            .and_then(|state| state.committed.as_ref())
            .map(BrowserDocumentNavigationRecord::navigation)
            == Some(navigation)
    }

    pub(super) fn trace_context<'a>(
        &self,
        runtimes: &'a BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> Option<&'a BrowserNavigationTraceContext> {
        let state = self.state(runtimes, key)?;
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.navigation() == navigation)
        {
            return state.pending.as_ref().and_then(|pending| pending.trace());
        }
        state
            .committed
            .as_ref()
            .filter(|committed| committed.navigation() == navigation)
            .and_then(|committed| committed.trace())
    }

    pub(super) fn has_pending(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
    ) -> bool {
        self.state(runtimes, key)
            .is_some_and(|state| state.pending.is_some())
    }

    fn current_loader_id(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
    ) -> Option<String> {
        let state = self.state(runtimes, key)?;
        state
            .pending
            .as_ref()
            .or(state.committed.as_ref())
            .map(|record| record.navigation().loader_id().to_owned())
    }

    fn committed_loader_id(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
    ) -> Option<String> {
        self.state(runtimes, key)?
            .committed
            .as_ref()
            .map(|record| record.navigation().loader_id().to_owned())
    }

    pub(super) fn commit_if_matches(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        let Some(state) = self.state_mut(runtimes, key) else {
            return false;
        };
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| pending.navigation() != navigation)
        {
            return false;
        }
        state.committed = state.pending.take();
        true
    }

    pub(super) fn commit_with_rollback_if_matches(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> Option<BrowserDocumentNavigationCommitRollback> {
        let state = self.state_mut(runtimes, key)?;
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| pending.navigation() != navigation)
        {
            return None;
        }
        let rollback = BrowserDocumentNavigationCommitRollback {
            previous_committed: state.committed.take(),
        };
        state.committed = state.pending.take();
        Some(rollback)
    }

    pub(super) fn rollback_commit(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
        rollback: BrowserDocumentNavigationCommitRollback,
    ) -> bool {
        let Some(state) = self.state_mut(runtimes, key) else {
            return false;
        };
        if state.pending.is_some()
            || state
                .committed
                .as_ref()
                .is_none_or(|committed| committed.navigation() != navigation)
        {
            return false;
        }
        state.pending = state.committed.take();
        state.committed = rollback.previous_committed;
        true
    }

    pub(super) fn take_pending_if_matches(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> Option<BrowserDocumentNavigationRecord> {
        let state = self.state_mut(runtimes, key)?;
        if state
            .pending
            .as_ref()
            .is_none_or(|pending| pending.navigation() != navigation)
        {
            return None;
        }
        state.pending.take()
    }

    pub(super) fn restore_pending_if_vacant(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
        record: BrowserDocumentNavigationRecord,
    ) -> bool {
        let state = self.state_mut_or_default(runtimes, key);
        if state.pending.is_some() {
            return false;
        }
        state.pending = Some(record);
        true
    }

    pub(super) fn pending_record<'a>(
        &self,
        runtimes: &'a BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
    ) -> Option<&'a BrowserDocumentNavigationRecord> {
        self.state(runtimes, key)?.pending.as_ref()
    }

    pub(super) fn forget_target(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        target_id: &str,
    ) {
        for (owner, runtime) in &mut runtimes.entries {
            if owner.target_id() == target_id {
                runtime.document_navigation = None;
            }
        }
        runtimes.prune_empty();
    }

    fn state<'a>(
        &self,
        runtimes: &'a BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
    ) -> Option<&'a BrowserTargetDocumentNavigationState> {
        runtimes
            .entries
            .get(key)
            .and_then(|runtime| runtime.document_navigation.as_ref())
    }

    fn state_mut<'a>(
        &mut self,
        runtimes: &'a mut BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
    ) -> Option<&'a mut BrowserTargetDocumentNavigationState> {
        runtimes
            .entries
            .get_mut(key)
            .and_then(|runtime| runtime.document_navigation.as_mut())
    }

    fn state_mut_or_default<'a>(
        &mut self,
        runtimes: &'a mut BrowserTargetRuntimeRegistry,
        key: &BrowserPageOwnerKey,
    ) -> &'a mut BrowserTargetDocumentNavigationState {
        runtimes
            .entries
            .entry(key.clone())
            .or_default()
            .document_navigation
            .get_or_insert_default()
    }
}

impl BrowserNavigationOwner {
    #[cfg(test)]
    pub fn start_document_navigation(
        &mut self,
        key: &BrowserPageOwnerKey,
        loader_id: String,
    ) -> BrowserDocumentNavigation {
        self.start_document_navigation_with_trace(key, loader_id, None)
    }

    #[cfg(test)]
    pub fn start_document_navigation_with_trace(
        &mut self,
        key: &BrowserPageOwnerKey,
        loader_id: String,
        trace: Option<BrowserNavigationTraceContext>,
    ) -> BrowserDocumentNavigation {
        self.try_start_document_navigation_with_trace(key, loader_id, trace)
            .expect("test navigation Context must not be reserved for disposal")
    }

    /// Starts a cross-Document request unless the exact Context is reserved
    /// for whole-Context disposal.
    ///
    /// During migration an unregistered test/projection owner retains the
    /// legacy admission shape. A known disposing Context is the only new
    /// rejection; callers must not collapse that state with an unknown
    /// Context lookup.
    pub fn try_start_document_navigation_with_trace(
        &mut self,
        key: &BrowserPageOwnerKey,
        loader_id: String,
        trace: Option<BrowserNavigationTraceContext>,
    ) -> Option<BrowserDocumentNavigation> {
        if self
            .browser_contexts
            .is_disposing(&BrowserContextId::new(key.browser_context_id()))
        {
            return None;
        }
        let fact_page = self.capture_page_residence(key.browser_context_id(), key.target_id());
        let (navigation, superseded) =
            self.document_navigations
                .start(&mut self.target_runtimes, key, loader_id, trace);
        self.target_terminations
            .begin_navigation(&mut self.target_runtimes, key, &navigation);
        if let Some(page) = fact_page.as_ref() {
            if let Err(error) = self.record_navigation_admission_facts(
                key,
                &navigation,
                superseded
                    .as_ref()
                    .map(BrowserDocumentNavigationRecord::navigation),
                page,
            ) {
                tracing::error!(
                    %error,
                    browser_context_id = key.browser_context_id(),
                    target_id = key.target_id(),
                    page_residence_generation = page.loaded_page_generation(),
                    navigation_request_id = navigation.request_id().get(),
                    loader_id = navigation.loader_id(),
                    "failed to publish accepted navigation Browser fact"
                );
            }
        } else if self.has_target(key.target_id()) {
            tracing::error!(
                browser_context_id = key.browser_context_id(),
                target_id = key.target_id(),
                navigation_request_id = navigation.request_id().get(),
                loader_id = navigation.loader_id(),
                "accepted navigation for a registered Target without a Browser Page residence"
            );
        }
        if let Some(superseded) = superseded
            && let Some(trace) = superseded.trace()
        {
            trace.emit(
                BrowserNavigationTraceEvent::new(
                    "navigation_request_superseded",
                    trace.origin(),
                    "request-pending",
                    "request-terminal",
                )
                .with_navigation(superseded.navigation()),
            );
        }
        if let Some(trace) =
            self.document_navigations
                .trace_context(&self.target_runtimes, key, &navigation)
        {
            trace.emit(
                BrowserNavigationTraceEvent::new(
                    "navigation_request_started",
                    trace.origin(),
                    "action-accepted",
                    "request-pending",
                )
                .with_navigation(&navigation),
            );
        }
        Some(navigation)
    }

    /// Returns the bounded diagnostics sidecar only when `navigation` is the
    /// exact pending or committed request for `key`.
    pub fn document_navigation_trace_context(
        &self,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> Option<BrowserNavigationTraceContext> {
        self.document_navigations
            .trace_context(&self.target_runtimes, key, navigation)
            .cloned()
    }

    pub fn accepts_pending_document_navigation(
        &self,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        self.document_navigations
            .accepts_pending(&self.target_runtimes, key, navigation)
    }

    pub fn accepts_document_body_completion(
        &self,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        self.document_navigations
            .accepts_body_completion(&self.target_runtimes, key, navigation)
    }

    /// Returns whether `navigation` is the exact request that committed the
    /// current renderer Document for this browser Target.
    pub fn accepts_committed_document_navigation(
        &self,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        self.document_navigations
            .accepts_committed(&self.target_runtimes, key, navigation)
    }

    pub fn has_pending_document_navigation(&self, key: &BrowserPageOwnerKey) -> bool {
        self.document_navigations
            .has_pending(&self.target_runtimes, key)
    }

    pub fn current_document_loader_id(&self, key: &BrowserPageOwnerKey) -> Option<String> {
        self.document_navigations
            .current_loader_id(&self.target_runtimes, key)
    }

    pub fn committed_document_loader_id(&self, key: &BrowserPageOwnerKey) -> Option<String> {
        self.document_navigations
            .committed_loader_id(&self.target_runtimes, key)
    }

    pub fn commit_document_navigation_if_matches(
        &mut self,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        let committed =
            self.document_navigations
                .commit_if_matches(&mut self.target_runtimes, key, navigation);
        if committed {
            self.target_terminations
                .commit_navigation(&mut self.target_runtimes, key, navigation);
            self.initial_empty_documents
                .mark_exited(&mut self.target_runtimes, key);
        }
        committed
    }

    /// Retires one exact pending request as a non-commit terminal while the
    /// current Page remains resident. A stale completion cannot retire its
    /// successor or publish a duplicate fact.
    pub fn fail_document_navigation_if_matches(
        &mut self,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
        failure: BrowserNavigationFailure,
    ) -> bool {
        let fact_page = self.capture_page_residence(key.browser_context_id(), key.target_id());
        let Some(record) = self.document_navigations.take_pending_if_matches(
            &mut self.target_runtimes,
            key,
            navigation,
        ) else {
            return false;
        };
        self.target_terminations.cancel_navigation_if_matches(
            &mut self.target_runtimes,
            key,
            navigation,
        );
        if let Some(page) = fact_page.as_ref() {
            if let Err(error) =
                self.record_navigation_failed_fact(key, navigation, failure.clone(), None, page)
            {
                tracing::error!(
                    %error,
                    browser_context_id = key.browser_context_id(),
                    target_id = key.target_id(),
                    navigation_request_id = navigation.request_id().get(),
                    loader_id = navigation.loader_id(),
                    ?failure,
                    "failed to publish terminal navigation failure Browser fact"
                );
            }
        } else if self.has_target(key.target_id()) {
            tracing::error!(
                browser_context_id = key.browser_context_id(),
                target_id = key.target_id(),
                navigation_request_id = navigation.request_id().get(),
                loader_id = navigation.loader_id(),
                ?failure,
                "failed navigation for a registered Target without a Browser Page residence"
            );
        }
        if let Some(trace) = record.trace() {
            let mut event = BrowserNavigationTraceEvent::new(
                "navigation_request_failed",
                trace.origin(),
                "request-pending",
                "request-terminal",
            )
            .with_navigation(navigation);
            if let Some(page) = fact_page {
                event = event.with_page(page);
            }
            trace.emit(event);
        }
        true
    }

    /// Retires one exact pending request because its response became a
    /// download. This is a successful request terminal, but not a Document
    /// commit and therefore must not advance Page generation.
    pub fn convert_document_navigation_to_download_if_matches(
        &mut self,
        key: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
    ) -> bool {
        let fact_page = self.capture_page_residence(key.browser_context_id(), key.target_id());
        let Some(record) = self.document_navigations.take_pending_if_matches(
            &mut self.target_runtimes,
            key,
            navigation,
        ) else {
            return false;
        };
        self.target_terminations.cancel_navigation_if_matches(
            &mut self.target_runtimes,
            key,
            navigation,
        );
        if let Some(page) = fact_page.as_ref() {
            if let Err(error) = self.record_navigation_download_fact(key, navigation, page) {
                tracing::error!(
                    %error,
                    browser_context_id = key.browser_context_id(),
                    target_id = key.target_id(),
                    navigation_request_id = navigation.request_id().get(),
                    loader_id = navigation.loader_id(),
                    "failed to publish navigation download Browser fact"
                );
            }
        } else if self.has_target(key.target_id()) {
            tracing::error!(
                browser_context_id = key.browser_context_id(),
                target_id = key.target_id(),
                navigation_request_id = navigation.request_id().get(),
                loader_id = navigation.loader_id(),
                "download navigation for a registered Target without a Browser Page residence"
            );
        }
        if let Some(trace) = record.trace() {
            let mut event = BrowserNavigationTraceEvent::new(
                "navigation_request_converted_to_download",
                trace.origin(),
                "request-pending",
                "request-terminal",
            )
            .with_navigation(navigation);
            if let Some(page) = fact_page {
                event = event.with_page(page);
            }
            trace.emit(event);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        browser_host::{
            BrowserContextSelectionProjection, BrowserFact, BrowserNavigationTraceSource,
            BrowserPageResidenceHandle, BrowserSelectedTargetEngineDisposition,
            BrowserTargetHandle, BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
            PageResidenceIdentity,
        },
        runtime::NavigationEngine,
    };
    use moli_page_types::BrowserActionId;

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
            .expect("test Target topology should register");
        (owner, key)
    }

    #[test]
    fn newer_request_makes_old_pending_and_body_completion_stale() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let first = owner.start_document_navigation(&key, "loader-1".to_owned());
        assert!(owner.accepts_pending_document_navigation(&key, &first));
        assert!(owner.accepts_document_body_completion(&key, &first));

        let second = owner.start_document_navigation(&key, "loader-2".to_owned());
        assert!(!owner.accepts_pending_document_navigation(&key, &first));
        assert!(!owner.accepts_document_body_completion(&key, &first));
        assert!(owner.accepts_pending_document_navigation(&key, &second));
        assert_ne!(first.request_id(), second.request_id());
    }

    #[test]
    fn committed_request_accepts_late_body_until_successor_starts() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let first = owner.start_document_navigation(&key, "loader-1".to_owned());
        assert!(owner.commit_document_navigation_if_matches(&key, &first));
        assert!(owner.accepts_committed_document_navigation(&key, &first));
        assert!(owner.accepts_document_body_completion(&key, &first));
        assert_eq!(
            owner.committed_document_loader_id(&key).as_deref(),
            Some("loader-1")
        );

        let second = owner.start_document_navigation(&key, "loader-2".to_owned());
        assert!(!owner.accepts_document_body_completion(&key, &first));
        assert!(owner.accepts_document_body_completion(&key, &second));
    }

    #[test]
    fn failing_pending_request_restores_committed_body_authority() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let committed = owner.start_document_navigation(&key, "loader-1".to_owned());
        assert!(owner.commit_document_navigation_if_matches(&key, &committed));
        let failed = owner.start_document_navigation(&key, "loader-2".to_owned());

        assert!(owner.fail_document_navigation_if_matches(
            &key,
            &failed,
            BrowserNavigationFailure::Network {
                error_text: "net::ERR_FAILED".to_owned(),
            },
        ));
        assert!(!owner.accepts_pending_document_navigation(&key, &failed));
        assert!(owner.accepts_document_body_completion(&key, &committed));
        assert_eq!(
            owner.current_document_loader_id(&key).as_deref(),
            Some("loader-1")
        );
    }

    #[test]
    fn page_runtime_discard_rejects_every_late_request_completion() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let navigation = owner.start_document_navigation(&key, "loader-1".to_owned());

        owner.discard_target_page_runtime("target-1");

        assert!(!owner.accepts_pending_document_navigation(&key, &navigation));
        assert!(!owner.accepts_document_body_completion(&key, &navigation));
        assert!(!owner.has_pending_document_navigation(&key));
    }

    #[test]
    fn trace_sidecar_follows_exact_request_commit_and_cleanup() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let key = BrowserPageOwnerKey::new("context-1", "target-1");
        let first_trace = BrowserNavigationTraceContext::new(
            owner.browser_instance_id,
            BrowserActionId::allocate(),
            BrowserNavigationTraceSource::FrontendCommand,
            PageResidenceIdentity::new("context-1".to_owned(), Some("target-1".to_owned()), 3),
            None,
        );
        let first_action = first_trace.browser_action_id();
        let first = owner.start_document_navigation_with_trace(
            &key,
            "loader-1".to_owned(),
            Some(first_trace),
        );

        assert_eq!(
            owner
                .document_navigation_trace_context(&key, &first)
                .map(|trace| trace.browser_action_id()),
            Some(first_action)
        );
        assert!(owner.commit_document_navigation_if_matches(&key, &first));
        assert_eq!(
            owner
                .document_navigation_trace_context(&key, &first)
                .map(|trace| trace.browser_action_id()),
            Some(first_action),
            "commit must move the sidecar with the exact request"
        );

        let second_trace = BrowserNavigationTraceContext::new(
            owner.browser_instance_id,
            BrowserActionId::allocate(),
            BrowserNavigationTraceSource::RendererIntent,
            PageResidenceIdentity::new("context-1".to_owned(), Some("target-1".to_owned()), 3),
            None,
        );
        let second = owner.start_document_navigation_with_trace(
            &key,
            "loader-2".to_owned(),
            Some(second_trace),
        );
        assert!(owner.fail_document_navigation_if_matches(
            &key,
            &second,
            BrowserNavigationFailure::Canceled {
                error_text: "canceled".to_owned(),
            },
        ));
        assert!(
            owner
                .document_navigation_trace_context(&key, &second)
                .is_none(),
            "failed pending cleanup must retire its trace sidecar"
        );
        assert_eq!(
            owner
                .document_navigation_trace_context(&key, &first)
                .map(|trace| trace.browser_action_id()),
            Some(first_action),
            "clearing a failed successor must retain committed request diagnostics"
        );
    }

    #[test]
    fn successor_acceptance_carries_superseded_request_in_one_page_generation() {
        let (mut owner, key) = owner_with_target();
        let mut subscriber = owner.subscribe_browser_facts();
        let first = owner.start_document_navigation(&key, "loader-1".to_owned());
        let second = owner.start_document_navigation(&key, "loader-2".to_owned());

        let created = subscriber.try_recv().expect("Target creation occurrence");
        let accepted_first = subscriber.try_recv().expect("first acceptance fact");
        let accepted_second = subscriber.try_recv().expect("successor acceptance fact");
        assert_eq!(created.sequence().get(), 1);
        assert!(matches!(created.fact(), BrowserFact::TargetCreated));
        assert_eq!(accepted_first.sequence().get(), 2);
        assert_eq!(accepted_second.sequence().get(), 3);
        assert_eq!(
            accepted_first.page_residence(),
            accepted_second.page_residence()
        );
        assert_eq!(
            accepted_second.fact(),
            &BrowserFact::NavigationAccepted {
                navigation: second,
                superseded_navigation: Some(first),
            }
        );
    }

    #[test]
    fn exact_failure_is_terminal_once_and_a_stale_completion_cannot_duplicate_it() {
        let (mut owner, key) = owner_with_target();
        let navigation = owner.start_document_navigation(&key, "loader-failed".to_owned());
        let failure = BrowserNavigationFailure::Network {
            error_text: "net::ERR_NAME_NOT_RESOLVED".to_owned(),
        };

        assert!(owner.fail_document_navigation_if_matches(&key, &navigation, failure.clone()));
        assert!(!owner.fail_document_navigation_if_matches(&key, &navigation, failure.clone()));
        assert!(!owner.has_pending_document_navigation(&key));
        let facts = owner.browser_fact_snapshot();
        assert_eq!(facts.len(), 3);
        assert!(matches!(facts[0].fact(), BrowserFact::TargetCreated));
        assert_eq!(
            facts[2].fact(),
            &BrowserFact::NavigationFailed {
                navigation,
                failure,
                previous_page: None,
            }
        );
    }

    #[test]
    fn download_is_a_distinct_terminal_without_page_replacement() {
        let (mut owner, key) = owner_with_target();
        let page = owner
            .capture_page_residence(key.browser_context_id(), key.target_id())
            .expect("current Page");
        let navigation = owner.start_document_navigation(&key, "loader-download".to_owned());

        assert!(owner.convert_document_navigation_to_download_if_matches(&key, &navigation));
        assert!(!owner.convert_document_navigation_to_download_if_matches(&key, &navigation));
        assert_eq!(
            owner
                .capture_page_residence(key.browser_context_id(), key.target_id())
                .as_ref(),
            Some(&page)
        );
        let facts = owner.browser_fact_snapshot();
        assert_eq!(facts.len(), 3);
        assert!(matches!(facts[0].fact(), BrowserFact::TargetCreated));
        assert_eq!(
            facts[2].fact(),
            &BrowserFact::NavigationConvertedToDownload { navigation }
        );
    }
}
