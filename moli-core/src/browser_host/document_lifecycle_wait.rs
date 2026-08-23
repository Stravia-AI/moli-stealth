use std::sync::Arc;

use parking_lot::Mutex;

use crate::page::{
    RendererDocumentLifecycleIdentity, RendererDocumentLifecycleMilestone,
    RendererLifecycleTerminationStamp,
};

use super::{
    BrowserFact, BrowserFactEnvelope, BrowserFactReceiveError, BrowserFactSubscriber,
    BrowserFactTryReceiveError, PageResidenceIdentity,
};

/// Why an exact Browser fact wait can no longer observe its Document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserDocumentLifecycleWaitUnavailableReason {
    NoCurrentPage,
    TargetCrashed,
    TargetClosed,
    SubscriberLagged { skipped: u64 },
    FactJournalClosed,
}

/// Protocol-neutral terminal for one exact Document milestone wait.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserDocumentLifecycleWaitOutcome {
    Reached,
    Interrupted {
        last_reached: Option<RendererDocumentLifecycleMilestone>,
        termination: RendererLifecycleTerminationStamp,
    },
    Superseded,
    Unavailable(BrowserDocumentLifecycleWaitUnavailableReason),
}

struct BrowserDocumentLifecycleWaitShared {
    expected_page: Option<PageResidenceIdentity>,
    expected_document: Option<RendererDocumentLifecycleIdentity>,
    milestone: RendererDocumentLifecycleMilestone,
    outcome: Mutex<Option<BrowserDocumentLifecycleWaitOutcome>>,
    readiness_subscriber: Mutex<Option<BrowserFactSubscriber>>,
}

impl std::fmt::Debug for BrowserDocumentLifecycleWaitShared {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserDocumentLifecycleWaitShared")
            .field("expected_page", &self.expected_page)
            .field("expected_document", &self.expected_document)
            .field("milestone", &self.milestone)
            .field("outcome", &self.current_outcome())
            .finish()
    }
}

impl BrowserDocumentLifecycleWaitShared {
    fn current_outcome(&self) -> Option<BrowserDocumentLifecycleWaitOutcome> {
        self.outcome.lock().clone()
    }

    fn poll_readiness(&self) -> Option<BrowserDocumentLifecycleWaitOutcome> {
        loop {
            if let Some(outcome) = self.current_outcome() {
                return Some(outcome);
            }
            let received = {
                let mut subscriber = self.readiness_subscriber.lock();
                let subscriber = subscriber.as_mut()?;
                subscriber.try_recv()
            };
            match received {
                Ok(fact) => {
                    if let Some(outcome) = self.outcome_for_fact(&fact) {
                        return Some(self.finish(outcome));
                    }
                }
                Err(BrowserFactTryReceiveError::Empty) => return None,
                Err(BrowserFactTryReceiveError::Lagged { skipped }) => {
                    return Some(
                        self.finish(BrowserDocumentLifecycleWaitOutcome::Unavailable(
                            BrowserDocumentLifecycleWaitUnavailableReason::SubscriberLagged {
                                skipped,
                            },
                        )),
                    );
                }
                Err(BrowserFactTryReceiveError::Closed) => {
                    return Some(
                        self.finish(BrowserDocumentLifecycleWaitOutcome::Unavailable(
                            BrowserDocumentLifecycleWaitUnavailableReason::FactJournalClosed,
                        )),
                    );
                }
            }
        }
    }

    fn outcome_for_fact(
        &self,
        envelope: &BrowserFactEnvelope,
    ) -> Option<BrowserDocumentLifecycleWaitOutcome> {
        let expected_page = self.expected_page.as_ref()?;
        let expected_document = self.expected_document?;
        match envelope.fact() {
            BrowserFact::NavigationAccepted { .. }
                if envelope.page_residence() == expected_page =>
            {
                Some(BrowserDocumentLifecycleWaitOutcome::Superseded)
            }
            BrowserFact::NavigationCommitted { previous_page, .. }
                if previous_page == expected_page =>
            {
                Some(BrowserDocumentLifecycleWaitOutcome::Superseded)
            }
            BrowserFact::TargetCrashed { previous_page, .. } if previous_page == expected_page => {
                Some(BrowserDocumentLifecycleWaitOutcome::Unavailable(
                    BrowserDocumentLifecycleWaitUnavailableReason::TargetCrashed,
                ))
            }
            BrowserFact::TargetClosed { previous_page, .. } if previous_page == expected_page => {
                Some(BrowserDocumentLifecycleWaitOutcome::Unavailable(
                    BrowserDocumentLifecycleWaitUnavailableReason::TargetClosed,
                ))
            }
            BrowserFact::DocumentLifecycleReached {
                document,
                milestone,
                ..
            } if envelope.page_residence() == expected_page
                && document == &expected_document
                && milestone_satisfies(*milestone, self.milestone) =>
            {
                Some(BrowserDocumentLifecycleWaitOutcome::Reached)
            }
            BrowserFact::DocumentLifecycleTerminated {
                document,
                last_reached,
                termination,
            } if envelope.page_residence() == expected_page && document == &expected_document => {
                if last_reached.is_some_and(|reached| milestone_satisfies(reached, self.milestone))
                {
                    Some(BrowserDocumentLifecycleWaitOutcome::Reached)
                } else {
                    Some(BrowserDocumentLifecycleWaitOutcome::Interrupted {
                        last_reached: *last_reached,
                        termination: *termination,
                    })
                }
            }
            _ => None,
        }
    }

    fn finish(
        &self,
        outcome: BrowserDocumentLifecycleWaitOutcome,
    ) -> BrowserDocumentLifecycleWaitOutcome {
        let mut current = self.outcome.lock();
        if let Some(current) = current.as_ref() {
            return current.clone();
        }
        *current = Some(outcome.clone());
        outcome
    }
}

/// Cloneable readiness over an independent cursor at the same journal cut.
///
/// It can consume facts only to decide whether the exact wait is terminal. It
/// cannot publish facts, mutate Browser state, or consume the ticket's async
/// cursor. This makes terminal-before-next-owner ordering independent of when
/// the adapter's wait task happens to be scheduled.
#[derive(Clone, Debug)]
pub struct BrowserDocumentLifecycleWaitReadiness {
    shared: Arc<BrowserDocumentLifecycleWaitShared>,
}

impl BrowserDocumentLifecycleWaitReadiness {
    pub fn is_terminal(&self) -> bool {
        self.shared.poll_readiness().is_some()
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn terminal_for_test_support() -> Self {
        Self {
            shared: Arc::new(BrowserDocumentLifecycleWaitShared {
                expected_page: None,
                expected_document: None,
                milestone: RendererDocumentLifecycleMilestone::Load,
                outcome: Mutex::new(Some(BrowserDocumentLifecycleWaitOutcome::Reached)),
                readiness_subscriber: Mutex::new(None),
            }),
        }
    }
}

/// Move-only subscriber ticket for one exact Page/Document milestone.
///
/// The ticket consumes the common Browser fact journal. It does not register
/// a Page-slot callback and cannot influence Browser Owner progress. Its two
/// cursors are created at one owner cut: one drives the async wait, while the
/// other lets the scheduler inspect readiness without stealing that wait.
pub struct BrowserDocumentLifecycleWaitTicket {
    wait_subscriber: Option<BrowserFactSubscriber>,
    shared: Arc<BrowserDocumentLifecycleWaitShared>,
}

impl std::fmt::Debug for BrowserDocumentLifecycleWaitTicket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserDocumentLifecycleWaitTicket")
            .field("shared", &self.shared)
            .finish()
    }
}

impl BrowserDocumentLifecycleWaitTicket {
    pub(crate) fn new(
        wait_subscriber: BrowserFactSubscriber,
        readiness_subscriber: BrowserFactSubscriber,
        expected_page: PageResidenceIdentity,
        expected_document: RendererDocumentLifecycleIdentity,
        milestone: RendererDocumentLifecycleMilestone,
    ) -> Self {
        let ticket = Self {
            wait_subscriber: Some(wait_subscriber),
            shared: Arc::new(BrowserDocumentLifecycleWaitShared {
                expected_page: Some(expected_page),
                expected_document: Some(expected_document),
                milestone,
                outcome: Mutex::new(None),
                readiness_subscriber: Mutex::new(Some(readiness_subscriber)),
            }),
        };
        let _ = ticket.shared.poll_readiness();
        ticket
    }

    pub fn resolved(
        expected_page: Option<PageResidenceIdentity>,
        expected_document: Option<RendererDocumentLifecycleIdentity>,
        milestone: RendererDocumentLifecycleMilestone,
        outcome: BrowserDocumentLifecycleWaitOutcome,
    ) -> Self {
        Self {
            wait_subscriber: None,
            shared: Arc::new(BrowserDocumentLifecycleWaitShared {
                expected_page,
                expected_document,
                milestone,
                outcome: Mutex::new(Some(outcome)),
                readiness_subscriber: Mutex::new(None),
            }),
        }
    }

    pub fn expected_page(&self) -> Option<&PageResidenceIdentity> {
        self.shared.expected_page.as_ref()
    }

    pub fn expected_document(&self) -> Option<RendererDocumentLifecycleIdentity> {
        self.shared.expected_document
    }

    pub fn milestone(&self) -> RendererDocumentLifecycleMilestone {
        self.shared.milestone
    }

    pub fn readiness(&self) -> BrowserDocumentLifecycleWaitReadiness {
        BrowserDocumentLifecycleWaitReadiness {
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.outcome().is_some()
    }

    pub fn outcome(&self) -> Option<BrowserDocumentLifecycleWaitOutcome> {
        self.shared.poll_readiness()
    }

    pub async fn wait(mut self) -> BrowserDocumentLifecycleWaitOutcome {
        loop {
            if let Some(outcome) = self.shared.poll_readiness() {
                return outcome;
            }
            let Some(subscriber) = self.wait_subscriber.as_mut() else {
                return self
                    .shared
                    .finish(BrowserDocumentLifecycleWaitOutcome::Unavailable(
                        BrowserDocumentLifecycleWaitUnavailableReason::FactJournalClosed,
                    ));
            };
            match subscriber.recv().await {
                Ok(fact) => {
                    if let Some(outcome) = self.shared.outcome_for_fact(&fact) {
                        return self.shared.finish(outcome);
                    }
                }
                Err(BrowserFactReceiveError::Lagged { skipped }) => {
                    return self
                        .shared
                        .finish(BrowserDocumentLifecycleWaitOutcome::Unavailable(
                            BrowserDocumentLifecycleWaitUnavailableReason::SubscriberLagged {
                                skipped,
                            },
                        ));
                }
                Err(BrowserFactReceiveError::Closed) => {
                    return self
                        .shared
                        .finish(BrowserDocumentLifecycleWaitOutcome::Unavailable(
                            BrowserDocumentLifecycleWaitUnavailableReason::FactJournalClosed,
                        ));
                }
            }
        }
    }
}

fn milestone_satisfies(
    reached: RendererDocumentLifecycleMilestone,
    expected: RendererDocumentLifecycleMilestone,
) -> bool {
    matches!(
        (reached, expected),
        (
            RendererDocumentLifecycleMilestone::DomContentLoaded,
            RendererDocumentLifecycleMilestone::DomContentLoaded
        ) | (
            RendererDocumentLifecycleMilestone::Load,
            RendererDocumentLifecycleMilestone::DomContentLoaded
                | RendererDocumentLifecycleMilestone::Load
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_host::fact_journal::BrowserFactJournal;
    use crate::{
        PageId,
        browser_host::{BrowserDocumentNavigation, BrowserInstanceId},
        page::{
            RendererDocumentTerminationReason, RendererDocumentToken, RendererFrameToken,
            RendererLifecycleEpoch,
        },
    };

    fn page(generation: u64) -> PageResidenceIdentity {
        PageResidenceIdentity::new(
            "context-1".to_owned(),
            Some("target-1".to_owned()),
            generation,
        )
    }

    fn document(generation: u64) -> RendererDocumentLifecycleIdentity {
        let page_id = PageId::new_for_testing(41);
        RendererDocumentLifecycleIdentity {
            frame: RendererFrameToken { page_id },
            document: RendererDocumentToken::new_for_testing(page_id, generation),
            epoch: RendererLifecycleEpoch(generation),
        }
    }

    fn ticket(
        journal: &BrowserFactJournal,
        expected_page: PageResidenceIdentity,
        expected_document: RendererDocumentLifecycleIdentity,
    ) -> BrowserDocumentLifecycleWaitTicket {
        let (wait_subscriber, readiness_subscriber) = journal.subscribe_pair();
        BrowserDocumentLifecycleWaitTicket::new(
            wait_subscriber,
            readiness_subscriber,
            expected_page,
            expected_document,
            RendererDocumentLifecycleMilestone::Load,
        )
    }

    fn publish(journal: &mut BrowserFactJournal, page: PageResidenceIdentity, fact: BrowserFact) {
        journal
            .publish_batch(page, vec![fact])
            .expect("test fact should publish");
    }

    #[tokio::test]
    async fn retained_load_fact_resolves_exact_ticket_at_creation() {
        let mut journal = BrowserFactJournal::new(BrowserInstanceId::allocate());
        let expected_page = page(2);
        let expected_document = document(2);
        publish(
            &mut journal,
            expected_page.clone(),
            BrowserFact::DocumentLifecycleReached {
                document: expected_document,
                milestone: RendererDocumentLifecycleMilestone::Load,
                stamp: crate::page::RendererLifecycleEventStamp {
                    sequence: 7,
                    timestamp_micros: 70,
                },
            },
        );

        let ticket = ticket(&journal, expected_page, expected_document);
        assert!(ticket.is_terminal());
        assert_eq!(
            ticket.wait().await,
            BrowserDocumentLifecycleWaitOutcome::Reached
        );
    }

    #[tokio::test]
    async fn future_exact_termination_interrupts_load_ticket_and_readiness() {
        let mut journal = BrowserFactJournal::new(BrowserInstanceId::allocate());
        let expected_page = page(2);
        let expected_document = document(2);
        let ticket = ticket(&journal, expected_page.clone(), expected_document);
        let readiness = ticket.readiness();
        assert!(!readiness.is_terminal());
        let termination = RendererLifecycleTerminationStamp {
            sequence: 8,
            timestamp_micros: 80,
            reason: RendererDocumentTerminationReason::Stopped,
        };
        publish(
            &mut journal,
            expected_page,
            BrowserFact::DocumentLifecycleTerminated {
                document: expected_document,
                last_reached: Some(RendererDocumentLifecycleMilestone::DomContentLoaded),
                termination,
            },
        );

        assert!(
            readiness.is_terminal(),
            "readiness must consume the fact without waiting for the async ticket task"
        );
        assert_eq!(
            ticket.wait().await,
            BrowserDocumentLifecycleWaitOutcome::Interrupted {
                last_reached: Some(RendererDocumentLifecycleMilestone::DomContentLoaded),
                termination,
            }
        );
    }

    #[tokio::test]
    async fn newer_navigation_acceptance_supersedes_exact_page_ticket() {
        let mut journal = BrowserFactJournal::new(BrowserInstanceId::allocate());
        let expected_page = page(2);
        let expected_document = document(2);
        let ticket = ticket(&journal, expected_page.clone(), expected_document);
        publish(
            &mut journal,
            expected_page,
            BrowserFact::NavigationAccepted {
                navigation: BrowserDocumentNavigation::new("target-1", "loader-next"),
                superseded_navigation: None,
            },
        );

        assert_eq!(
            ticket.wait().await,
            BrowserDocumentLifecycleWaitOutcome::Superseded
        );
    }

    #[tokio::test]
    async fn retained_page_replacement_supersedes_ticket_when_acceptance_is_outside_cut() {
        let mut journal = BrowserFactJournal::new(BrowserInstanceId::allocate());
        let expected_page = page(2);
        let expected_document = document(2);
        let navigation = BrowserDocumentNavigation::new("target-1", "loader-next");
        publish(
            &mut journal,
            page(3),
            BrowserFact::NavigationCommitted {
                previous_page: expected_page.clone(),
                navigation,
            },
        );

        let ticket = ticket(&journal, expected_page, expected_document);
        assert_eq!(
            ticket.wait().await,
            BrowserDocumentLifecycleWaitOutcome::Superseded
        );
    }

    #[tokio::test]
    async fn unrelated_facts_are_ignored_before_exact_target_close() {
        let mut journal = BrowserFactJournal::new(BrowserInstanceId::allocate());
        let expected_page = page(2);
        let expected_document = document(2);
        let ticket = ticket(&journal, expected_page.clone(), expected_document);
        publish(
            &mut journal,
            page(7),
            BrowserFact::TargetClosed {
                previous_page: page(6),
                pending_navigation: None,
            },
        );
        assert!(!ticket.is_terminal());
        publish(
            &mut journal,
            page(3),
            BrowserFact::TargetClosed {
                previous_page: expected_page,
                pending_navigation: None,
            },
        );

        assert_eq!(
            ticket.wait().await,
            BrowserDocumentLifecycleWaitOutcome::Unavailable(
                BrowserDocumentLifecycleWaitUnavailableReason::TargetClosed
            )
        );
    }

    #[tokio::test]
    async fn readiness_reports_explicit_lag_instead_of_guessing_terminal_state() {
        let mut journal = BrowserFactJournal::new(BrowserInstanceId::allocate());
        let expected_page = page(2);
        let expected_document = document(2);
        let ticket = ticket(&journal, expected_page, expected_document);
        let readiness = ticket.readiness();
        for generation in 0..=1_024 {
            publish(
                &mut journal,
                page(7),
                BrowserFact::NavigationAccepted {
                    navigation: BrowserDocumentNavigation::new(
                        "target-1",
                        format!("unrelated-{generation}"),
                    ),
                    superseded_navigation: None,
                },
            );
        }

        assert!(readiness.is_terminal());
        assert_eq!(
            ticket.wait().await,
            BrowserDocumentLifecycleWaitOutcome::Unavailable(
                BrowserDocumentLifecycleWaitUnavailableReason::SubscriberLagged { skipped: 1 }
            )
        );
    }
}
