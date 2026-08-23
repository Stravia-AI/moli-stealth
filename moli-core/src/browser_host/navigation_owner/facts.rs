use std::sync::Arc;

use crate::{
    browser_host::{
        BrowserDocumentLifecycleWaitOutcome, BrowserDocumentLifecycleWaitTicket, BrowserFact,
        BrowserFactEnvelope, BrowserFactPublishError, BrowserFactSubscriber,
        BrowserFactWakeSubscriber, PageResidenceIdentity,
    },
    page::{
        RendererDocumentLifecycleEvent, RendererDocumentLifecycleEventKind,
        RendererDocumentLifecycleIdentity, RendererDocumentLifecycleMilestone,
        RendererLifecycleEventStamp, RendererLifecycleTerminationStamp,
    },
};

use super::{
    BrowserDocumentNavigation, BrowserNavigationFailure, BrowserNavigationOwner,
    BrowserPageOwnerKey, BrowserTargetMetadataTransition, BrowserTargetTermination,
    BrowserTargetTerminationKind,
};

impl BrowserNavigationOwner {
    /// Creates a cursor over the retained window followed by future facts.
    pub fn subscribe_browser_facts(&self) -> BrowserFactSubscriber {
        self.browser_facts.subscribe()
    }

    /// Captures a move-only wait over the common Browser fact journal for one
    /// exact current Page and renderer Document.
    ///
    /// The caller may resolve protocol/session routing before this boundary,
    /// but only Browser Core validates the Page generation and chooses the
    /// journal cut. A stale physical binding therefore resolves as
    /// `Superseded` instead of registering a callback in the frontend Page
    /// projection.
    pub fn capture_document_lifecycle_wait(
        &self,
        expected_page: PageResidenceIdentity,
        expected_document: RendererDocumentLifecycleIdentity,
        milestone: RendererDocumentLifecycleMilestone,
    ) -> BrowserDocumentLifecycleWaitTicket {
        let Some(owner) = self.page_owner_key_if_current(&expected_page) else {
            return BrowserDocumentLifecycleWaitTicket::resolved(
                Some(expected_page),
                Some(expected_document),
                milestone,
                BrowserDocumentLifecycleWaitOutcome::Superseded,
            );
        };
        if self.has_pending_document_navigation(&owner) {
            return BrowserDocumentLifecycleWaitTicket::resolved(
                Some(expected_page),
                Some(expected_document),
                milestone,
                BrowserDocumentLifecycleWaitOutcome::Superseded,
            );
        }
        if let Some(outcome) =
            self.document_lifecycles
                .outcome(&owner, &expected_page, expected_document, milestone)
        {
            return BrowserDocumentLifecycleWaitTicket::resolved(
                Some(expected_page),
                Some(expected_document),
                milestone,
                outcome,
            );
        }
        let (wait_subscriber, readiness_subscriber) = self.browser_facts.subscribe_pair();
        BrowserDocumentLifecycleWaitTicket::new(
            wait_subscriber,
            readiness_subscriber,
            expected_page,
            expected_document,
            milestone,
        )
    }

    /// Creates a coalesced application-scheduler wake subscription.
    pub fn subscribe_browser_fact_wake(&self) -> BrowserFactWakeSubscriber {
        self.browser_facts.subscribe_wake()
    }

    /// Returns the currently retained bounded fact window.
    pub fn browser_fact_snapshot(&self) -> Vec<Arc<BrowserFactEnvelope>> {
        self.browser_facts.snapshot()
    }

    /// Publishes the unique occurrence of one committed top-level Target.
    ///
    /// The exact initial Page-slot identity distinguishes a later Target that
    /// reuses the same public id. DevTools discovery and attachment state are
    /// frontend concerns and therefore do not enter this fact.
    pub(super) fn record_target_created_fact(
        &mut self,
        owner: &BrowserPageOwnerKey,
        page: &PageResidenceIdentity,
    ) -> Result<Vec<Arc<BrowserFactEnvelope>>, BrowserFactPublishError> {
        debug_assert_eq!(page.browser_context_id(), owner.browser_context_id());
        debug_assert_eq!(page.target_id(), Some(owner.target_id()));
        debug_assert_eq!(self.page_owner_key_if_current(page).as_ref(), Some(owner));
        debug_assert!(self.has_target(owner.target_id()));
        self.browser_facts
            .publish_batch(page.clone(), vec![BrowserFact::TargetCreated])
    }

    /// Publishes one accepted cross-Document request against the exact Page
    /// generation that it supersedes. If another request was already pending,
    /// its terminal fact is adjacent and ordered before the successor's
    /// acceptance in the same Browser Owner batch.
    pub(super) fn record_navigation_admission_facts(
        &mut self,
        owner: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
        superseded: Option<&BrowserDocumentNavigation>,
        page: &PageResidenceIdentity,
    ) -> Result<Vec<Arc<BrowserFactEnvelope>>, BrowserFactPublishError> {
        debug_assert_eq!(navigation.target_id(), owner.target_id());
        debug_assert_eq!(page.browser_context_id(), owner.browser_context_id());
        debug_assert_eq!(page.target_id(), Some(owner.target_id()));
        debug_assert_eq!(self.page_owner_key_if_current(page).as_ref(), Some(owner));
        let mut facts = Vec::with_capacity(1 + usize::from(superseded.is_some()));
        if let Some(superseded) = superseded {
            facts.push(BrowserFact::NavigationFailed {
                navigation: superseded.clone(),
                failure: BrowserNavigationFailure::Superseded {
                    replacement: navigation.clone(),
                },
                previous_page: None,
            });
        }
        facts.push(BrowserFact::NavigationAccepted {
            navigation: navigation.clone(),
        });
        self.browser_facts.publish_batch(page.clone(), facts)
    }

    /// Publishes the unique non-commit terminal fact for one exact request
    /// after its Browser-owned pending state has been retired.
    pub(super) fn record_navigation_failed_fact(
        &mut self,
        owner: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
        failure: BrowserNavigationFailure,
        previous_page: Option<PageResidenceIdentity>,
        current_page: &PageResidenceIdentity,
    ) -> Result<Vec<Arc<BrowserFactEnvelope>>, BrowserFactPublishError> {
        debug_assert_eq!(navigation.target_id(), owner.target_id());
        debug_assert_eq!(
            current_page.browser_context_id(),
            owner.browser_context_id()
        );
        debug_assert_eq!(current_page.target_id(), Some(owner.target_id()));
        if let Some(previous_page) = previous_page.as_ref() {
            self.document_lifecycles.retire_page(owner, previous_page);
        }
        self.browser_facts.publish_batch(
            current_page.clone(),
            vec![BrowserFact::NavigationFailed {
                navigation: navigation.clone(),
                failure,
                previous_page,
            }],
        )
    }

    /// Publishes the non-Document success terminal for one exact request after
    /// Core has retired its pending authority while retaining the current Page.
    pub(super) fn record_navigation_download_fact(
        &mut self,
        owner: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
        current_page: &PageResidenceIdentity,
    ) -> Result<Vec<Arc<BrowserFactEnvelope>>, BrowserFactPublishError> {
        debug_assert_eq!(navigation.target_id(), owner.target_id());
        debug_assert_eq!(
            current_page.browser_context_id(),
            owner.browser_context_id()
        );
        debug_assert_eq!(current_page.target_id(), Some(owner.target_id()));
        self.browser_facts.publish_batch(
            current_page.clone(),
            vec![BrowserFact::NavigationConvertedToDownload {
                navigation: navigation.clone(),
            }],
        )
    }

    /// Publishes the immutable facts for one loaded-Document commit after
    /// request, Page generation, history and recovery state have committed
    /// together.
    ///
    /// This is intentionally owner-private: a physical Page projector cannot
    /// claim that either the navigation or replacement happened.
    /// `current_page` is carried by all three envelopes. The navigation
    /// outcome is published before the topology transition, followed by the
    /// frozen Target metadata, so consumers can depend on one stable
    /// request-outcome -> Page-replacement -> metadata order without
    /// inventing another commit boundary.
    pub(super) fn record_loaded_navigation_commit_facts(
        &mut self,
        owner: &BrowserPageOwnerKey,
        navigation: &BrowserDocumentNavigation,
        previous_page: &PageResidenceIdentity,
        current_page: &PageResidenceIdentity,
        metadata: BrowserTargetMetadataTransition,
    ) -> Result<Vec<Arc<BrowserFactEnvelope>>, BrowserFactPublishError> {
        debug_assert_eq!(navigation.target_id(), owner.target_id());
        debug_assert_eq!(
            current_page.browser_context_id(),
            owner.browser_context_id()
        );
        debug_assert_eq!(current_page.target_id(), Some(owner.target_id()));
        debug_assert_eq!(
            self.page_owner_key_if_current(current_page).as_ref(),
            Some(owner)
        );
        debug_assert_eq!(
            self.page_owner_key_for_same_slot(previous_page).as_ref(),
            Some(owner)
        );
        self.document_lifecycles.retire_page(owner, previous_page);
        self.browser_facts.publish_batch(
            current_page.clone(),
            vec![
                BrowserFact::NavigationCommitted {
                    navigation: navigation.clone(),
                },
                BrowserFact::PageReplaced {
                    previous_page: previous_page.clone(),
                    navigation: navigation.clone(),
                },
                BrowserFact::TargetMetadataChanged {
                    transition: metadata,
                },
            ],
        )
    }

    /// Publishes the immutable Target terminal fact after Target lifecycle,
    /// Page generation, request state, runtime ownership and history have
    /// committed together.
    ///
    /// A close has already removed the Target/Page registry entries, so this
    /// owner-private producer validates the immutable transaction result
    /// instead of asking the now-retired physical projection for authority.
    pub(super) fn record_target_termination_facts(
        &mut self,
        termination: &BrowserTargetTermination,
        pending_navigation: Option<&BrowserDocumentNavigation>,
    ) -> Result<Vec<Arc<BrowserFactEnvelope>>, BrowserFactPublishError> {
        let owner = termination.owner();
        let previous_page = termination.previous_page();
        let terminal_page = termination.terminal_page();
        debug_assert_eq!(
            previous_page.browser_context_id(),
            owner.browser_context_id()
        );
        debug_assert_eq!(previous_page.target_id(), Some(owner.target_id()));
        debug_assert_eq!(
            terminal_page.browser_context_id(),
            owner.browser_context_id()
        );
        debug_assert_eq!(terminal_page.target_id(), Some(owner.target_id()));
        debug_assert_eq!(
            Some(terminal_page.loaded_page_generation()),
            previous_page.loaded_page_generation().checked_add(1)
        );
        let target_fact = match termination.kind() {
            BrowserTargetTerminationKind::Crash => {
                debug_assert_eq!(
                    self.page_owner_key_if_current(terminal_page).as_ref(),
                    Some(owner)
                );
                debug_assert!(self.has_target(owner.target_id()));
                BrowserFact::TargetCrashed {
                    previous_page: previous_page.clone(),
                }
            }
            BrowserTargetTerminationKind::Close => {
                debug_assert!(
                    self.capture_page_residence(owner.browser_context_id(), owner.target_id())
                        .is_none()
                );
                debug_assert!(!self.has_target(owner.target_id()));
                BrowserFact::TargetClosed {
                    previous_page: previous_page.clone(),
                }
            }
        };
        let mut facts = Vec::with_capacity(1 + usize::from(pending_navigation.is_some()));
        if let Some(navigation) = pending_navigation {
            let failure = match termination.kind() {
                BrowserTargetTerminationKind::Crash => BrowserNavigationFailure::TargetCrashed,
                BrowserTargetTerminationKind::Close => BrowserNavigationFailure::TargetClosed,
            };
            facts.push(BrowserFact::NavigationFailed {
                navigation: navigation.clone(),
                failure,
                previous_page: Some(previous_page.clone()),
            });
        }
        facts.push(target_fact);
        self.document_lifecycles.retire_page(owner, previous_page);
        self.browser_facts
            .publish_batch(terminal_page.clone(), facts)
    }

    /// Records lifecycle current state plus reached/terminated facts accepted
    /// for one exact current Page. Started selects the Browser-owned snapshot
    /// but remains outside the occurrence journal until a consumer needs that
    /// distinct fact.
    pub fn record_document_lifecycle_facts(
        &mut self,
        expected_page: &PageResidenceIdentity,
        events: &[RendererDocumentLifecycleEvent],
    ) -> Result<Vec<Arc<BrowserFactEnvelope>>, BrowserFactPublishError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let Some(target_id) = expected_page.target_id() else {
            return Err(BrowserFactPublishError::TargetlessPageResidence(
                expected_page.clone(),
            ));
        };
        let Some(owner) = self.page_owner_key_if_current(expected_page) else {
            return Err(BrowserFactPublishError::StalePageResidence(
                expected_page.clone(),
            ));
        };
        debug_assert_eq!(
            owner.browser_context_id(),
            expected_page.browser_context_id()
        );
        debug_assert_eq!(owner.target_id(), target_id);
        self.document_lifecycles
            .record(&owner, expected_page, events);
        let facts = events
            .iter()
            .filter_map(|event| {
                let document = RendererDocumentLifecycleIdentity {
                    frame: event.frame,
                    document: event.document,
                    epoch: event.epoch,
                };
                match event.kind {
                    RendererDocumentLifecycleEventKind::Started { .. } => None,
                    RendererDocumentLifecycleEventKind::Milestone(milestone) => {
                        Some(BrowserFact::DocumentLifecycleReached {
                            document,
                            milestone,
                            stamp: RendererLifecycleEventStamp {
                                sequence: event.sequence,
                                timestamp_micros: event.timestamp_micros,
                            },
                        })
                    }
                    RendererDocumentLifecycleEventKind::Terminated {
                        last_reached,
                        reason,
                    } => Some(BrowserFact::DocumentLifecycleTerminated {
                        document,
                        last_reached,
                        termination: RendererLifecycleTerminationStamp {
                            sequence: event.sequence,
                            timestamp_micros: event.timestamp_micros,
                            reason,
                        },
                    }),
                }
            })
            .collect::<Vec<_>>();
        if facts.is_empty() {
            return Ok(Vec::new());
        }
        self.browser_facts
            .publish_batch(expected_page.clone(), facts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PageId,
        browser_host::BrowserPageOwnerKey,
        page::{
            RendererDocumentLifecycleMilestone, RendererDocumentTerminationReason,
            RendererDocumentToken, RendererFrameToken, RendererLifecycleEpoch,
        },
        runtime::NavigationEngine,
    };

    fn dcl_event() -> RendererDocumentLifecycleEvent {
        let page_id = PageId::new_for_testing(11);
        RendererDocumentLifecycleEvent {
            frame: RendererFrameToken { page_id },
            document: RendererDocumentToken::new_for_testing(page_id, 1),
            epoch: RendererLifecycleEpoch(1),
            sequence: 2,
            timestamp_micros: 20,
            kind: RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        }
    }

    fn terminated_event() -> RendererDocumentLifecycleEvent {
        RendererDocumentLifecycleEvent {
            sequence: 3,
            timestamp_micros: 30,
            kind: RendererDocumentLifecycleEventKind::Terminated {
                last_reached: Some(RendererDocumentLifecycleMilestone::DomContentLoaded),
                reason: RendererDocumentTerminationReason::Stopped,
            },
            ..dcl_event()
        }
    }

    #[test]
    fn exact_terminated_record_publishes_after_reached_fact() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let page_owner = BrowserPageOwnerKey::new("context-1", "target-1");
        let registration = owner
            .page_residences
            .begin_target_registration(page_owner.clone())
            .expect("test Page should stage");
        owner
            .page_residences
            .commit_target_registration(registration);
        let page = owner
            .capture_page_residence(page_owner.browser_context_id(), page_owner.target_id())
            .expect("test Page should be live");

        let published = owner
            .record_document_lifecycle_facts(&page, &[dcl_event(), terminated_event()])
            .expect("exact lifecycle facts should publish");

        assert_eq!(published.len(), 2);
        assert_eq!(published[0].sequence().get(), 1);
        assert_eq!(published[1].sequence().get(), 2);
        assert!(matches!(
            published[0].fact(),
            BrowserFact::DocumentLifecycleReached {
                milestone: RendererDocumentLifecycleMilestone::DomContentLoaded,
                ..
            }
        ));
        assert!(matches!(
            published[1].fact(),
            BrowserFact::DocumentLifecycleTerminated {
                last_reached: Some(RendererDocumentLifecycleMilestone::DomContentLoaded),
                termination: RendererLifecycleTerminationStamp {
                    reason: RendererDocumentTerminationReason::Stopped,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn lifecycle_fact_requires_the_exact_current_page_generation() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let page_owner = BrowserPageOwnerKey::new("context-1", "target-1");
        let registration = owner
            .page_residences
            .begin_target_registration(page_owner.clone())
            .expect("test Page should stage");
        let handle = owner
            .page_residences
            .commit_target_registration(registration);
        let original = owner
            .capture_page_residence(page_owner.browser_context_id(), page_owner.target_id())
            .expect("test Page should be live");
        handle.advance_generation_for_test_fixture();

        assert_eq!(
            owner.record_document_lifecycle_facts(&original, &[dcl_event()]),
            Err(BrowserFactPublishError::StalePageResidence(original))
        );
        assert!(owner.browser_fact_snapshot().is_empty());
    }

    #[test]
    fn lifecycle_wait_capture_uses_owner_snapshot_after_journal_eviction() {
        let mut owner = BrowserNavigationOwner::new(NavigationEngine::new());
        let page_owner = BrowserPageOwnerKey::new("context-1", "target-1");
        let registration = owner
            .page_residences
            .begin_target_registration(page_owner.clone())
            .expect("test Page should stage");
        let handle = owner
            .page_residences
            .commit_target_registration(registration);
        let page = owner
            .capture_page_residence(page_owner.browser_context_id(), page_owner.target_id())
            .expect("test Page should be live");
        let event = dcl_event();
        owner
            .record_document_lifecycle_facts(&page, &[event])
            .expect("exact lifecycle fact should publish");
        for sequence in 0..=1_024 {
            owner
                .browser_facts
                .publish_batch(
                    PageResidenceIdentity::new(
                        "context-noise".to_owned(),
                        Some("target-noise".to_owned()),
                        sequence,
                    ),
                    vec![BrowserFact::NavigationAccepted {
                        navigation: BrowserDocumentNavigation::new(
                            "target-noise",
                            format!("loader-noise-{sequence}"),
                        ),
                    }],
                )
                .expect("noise fact should publish");
        }
        assert!(
            owner
                .browser_fact_snapshot()
                .iter()
                .all(|fact| !matches!(fact.fact(), BrowserFact::DocumentLifecycleReached { .. })),
            "the reached occurrence must be outside the bounded journal window"
        );
        let document = RendererDocumentLifecycleIdentity {
            frame: event.frame,
            document: event.document,
            epoch: event.epoch,
        };

        let reached = owner.capture_document_lifecycle_wait(
            page.clone(),
            document,
            RendererDocumentLifecycleMilestone::DomContentLoaded,
        );
        assert_eq!(
            reached.outcome(),
            Some(BrowserDocumentLifecycleWaitOutcome::Reached),
            "the Browser-owned current snapshot must bootstrap beyond journal retention"
        );

        handle.advance_generation_for_test_fixture();
        let stale = owner.capture_document_lifecycle_wait(
            page,
            document,
            RendererDocumentLifecycleMilestone::Load,
        );
        assert_eq!(
            stale.outcome(),
            Some(BrowserDocumentLifecycleWaitOutcome::Superseded),
            "a stale frontend Page binding must not subscribe as the current Document"
        );
    }
}
