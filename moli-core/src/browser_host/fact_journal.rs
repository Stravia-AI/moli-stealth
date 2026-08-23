use std::{
    collections::VecDeque,
    fmt,
    num::{NonZeroU64, NonZeroUsize},
    sync::Arc,
};

use tokio::sync::{broadcast, watch};

use crate::page::{
    RendererDocumentLifecycleIdentity, RendererDocumentLifecycleMilestone,
    RendererLifecycleEventStamp, RendererLifecycleTerminationStamp,
};

use super::{
    BrowserContextId, BrowserDocumentNavigation, BrowserInstanceId, BrowserNavigationFailure,
    BrowserTargetId, BrowserTargetMetadataTransition, PageResidenceIdentity,
};

const DEFAULT_BROWSER_FACT_JOURNAL_CAPACITY: usize = 1_024;

/// Monotonic order allocated by one Browser Owner for protocol-neutral facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BrowserFactSequence(NonZeroU64);

impl BrowserFactSequence {
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Protocol-neutral browser state transition.
///
/// This intentionally starts narrow. Renderer-local inspector output and
/// large payloads do not belong in the Browser fact stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserFact {
    /// One top-level Target became live in Browser Core topology. The
    /// envelope carries its initial exact Page-slot residence. DevTools tab
    /// facades, attachment state and discovery subscription are frontend
    /// projections and are intentionally absent.
    TargetCreated,
    /// Browser-visible metadata changed for one exact top-level Target/Page
    /// occurrence. Attachment state and frontend discovery policy are not
    /// metadata transitions and never enter this fact.
    TargetMetadataChanged {
        transition: BrowserTargetMetadataTransition,
    },
    /// One exact cross-Document request became the Target's pending request.
    /// The envelope carries the Page generation that the request supersedes.
    NavigationAccepted {
        navigation: BrowserDocumentNavigation,
    },
    /// One exact cross-Document request atomically committed its successor
    /// Document and replaced the Target's previous Page generation. The
    /// envelope carries the committed successor Page while the payload retains
    /// the retired source Page.
    NavigationCommitted {
        navigation: BrowserDocumentNavigation,
        previous_page: PageResidenceIdentity,
    },
    /// One exact accepted request reached a non-commit terminal state. The
    /// envelope carries the Page generation after the terminal transaction.
    /// `previous_page` is present only when that same transaction retired the
    /// prior Page generation.
    NavigationFailed {
        navigation: BrowserDocumentNavigation,
        failure: BrowserNavigationFailure,
        previous_page: Option<PageResidenceIdentity>,
    },
    /// One exact accepted navigation became a download and therefore did not
    /// create a successor Document. The current Page remains resident.
    NavigationConvertedToDownload {
        navigation: BrowserDocumentNavigation,
    },
    /// One exact Target crash retired its current Page generation while
    /// preserving the Target for a possible recovery navigation. The
    /// envelope carries the terminal Page generation.
    TargetCrashed {
        previous_page: PageResidenceIdentity,
    },
    /// One exact Target close atomically retired both the Target and its
    /// current Page generation. The envelope carries the terminal Page
    /// identity even though that residence is no longer live in the registry.
    TargetClosed {
        previous_page: PageResidenceIdentity,
    },
    DocumentLifecycleReached {
        document: RendererDocumentLifecycleIdentity,
        milestone: RendererDocumentLifecycleMilestone,
        stamp: RendererLifecycleEventStamp,
    },
    /// One exact renderer Document terminated. A waiter can distinguish a
    /// milestone that had already been reached from an interrupted wait by
    /// inspecting `last_reached`.
    DocumentLifecycleTerminated {
        document: RendererDocumentLifecycleIdentity,
        last_reached: Option<RendererDocumentLifecycleMilestone>,
        termination: RendererLifecycleTerminationStamp,
    },
}

/// Immutable fact together with the exact Browser source that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserFactEnvelope {
    sequence: BrowserFactSequence,
    browser_instance_id: BrowserInstanceId,
    page_residence: PageResidenceIdentity,
    fact: BrowserFact,
}

impl BrowserFactEnvelope {
    pub fn sequence(&self) -> BrowserFactSequence {
        self.sequence
    }

    pub fn browser_instance_id(&self) -> BrowserInstanceId {
        self.browser_instance_id
    }

    pub fn browser_context_id(&self) -> &BrowserContextId {
        self.page_residence.browser_context_identity()
    }

    pub fn target_id(&self) -> &BrowserTargetId {
        self.page_residence
            .target_identity()
            .expect("Browser fact envelopes are always target-backed")
    }

    pub fn page_residence(&self) -> &PageResidenceIdentity {
        &self.page_residence
    }

    pub fn fact(&self) -> &BrowserFact {
        &self.fact
    }
}

/// Rejection before a browser transition can enter the fact journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserFactPublishError {
    TargetlessPageResidence(PageResidenceIdentity),
    StalePageResidence(PageResidenceIdentity),
    SequenceExhausted,
}

impl fmt::Display for BrowserFactPublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetlessPageResidence(page) => write!(
                formatter,
                "Page residence in BrowserContext {:?} has no Target identity",
                page.browser_context_id()
            ),
            Self::StalePageResidence(page) => write!(
                formatter,
                "Page residence for Target {:?} in BrowserContext {:?} is stale",
                page.target_id(),
                page.browser_context_id()
            ),
            Self::SequenceExhausted => formatter.write_str("Browser fact sequence is exhausted"),
        }
    }
}

impl std::error::Error for BrowserFactPublishError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserFactReceiveError {
    Lagged { skipped: u64 },
    Closed,
}

impl fmt::Display for BrowserFactReceiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lagged { skipped } => {
                write!(
                    formatter,
                    "Browser fact subscriber lagged by {skipped} facts"
                )
            }
            Self::Closed => formatter.write_str("Browser fact journal is closed"),
        }
    }
}

impl std::error::Error for BrowserFactReceiveError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserFactTryReceiveError {
    Empty,
    Lagged { skipped: u64 },
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserFactWakeReceiveError {
    Closed,
}

impl fmt::Display for BrowserFactWakeReceiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("Browser fact wake subscription is closed"),
        }
    }
}

impl std::error::Error for BrowserFactWakeReceiveError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserFactWakeTryReceiveError {
    Empty,
    Closed,
}

/// Coalesced, payload-free wake subscription for an application scheduler.
///
/// The retained journal remains the source of truth. This subscription only
/// reports the newest sequence that may be consumed, so an idle frontend can
/// wake without allocating a second per-fact queue or backpressuring Browser
/// Owner publication.
pub struct BrowserFactWakeSubscriber {
    browser_instance_id: BrowserInstanceId,
    receiver: watch::Receiver<Option<BrowserFactSequence>>,
    last_delivered: Option<BrowserFactSequence>,
}

impl BrowserFactWakeSubscriber {
    pub fn browser_instance_id(&self) -> BrowserInstanceId {
        self.browser_instance_id
    }

    pub async fn recv(&mut self) -> Result<BrowserFactSequence, BrowserFactWakeReceiveError> {
        loop {
            let newest = *self.receiver.borrow_and_update();
            if newest != self.last_delivered
                && let Some(sequence) = newest
            {
                self.last_delivered = Some(sequence);
                return Ok(sequence);
            }
            self.receiver
                .changed()
                .await
                .map_err(|_| BrowserFactWakeReceiveError::Closed)?;
        }
    }

    pub fn try_recv(&mut self) -> Result<BrowserFactSequence, BrowserFactWakeTryReceiveError> {
        loop {
            let newest = *self.receiver.borrow_and_update();
            if newest != self.last_delivered
                && let Some(sequence) = newest
            {
                self.last_delivered = Some(sequence);
                return Ok(sequence);
            }
            match self.receiver.has_changed() {
                Ok(true) => continue,
                Ok(false) => return Err(BrowserFactWakeTryReceiveError::Empty),
                Err(_) => return Err(BrowserFactWakeTryReceiveError::Closed),
            }
        }
    }
}

/// Independent bounded cursor over retained bootstrap and future facts.
///
/// A slow consumer receives an explicit lag result. Publishing never awaits a
/// frontend and therefore cannot backpressure the Browser Owner.
pub struct BrowserFactSubscriber {
    browser_instance_id: BrowserInstanceId,
    replay: VecDeque<Arc<BrowserFactEnvelope>>,
    receiver: broadcast::Receiver<Arc<BrowserFactEnvelope>>,
}

impl BrowserFactSubscriber {
    pub fn browser_instance_id(&self) -> BrowserInstanceId {
        self.browser_instance_id
    }

    pub async fn recv(&mut self) -> Result<Arc<BrowserFactEnvelope>, BrowserFactReceiveError> {
        if let Some(fact) = self.replay.pop_front() {
            return Ok(fact);
        }
        self.receiver.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Lagged(skipped) => {
                BrowserFactReceiveError::Lagged { skipped }
            }
            broadcast::error::RecvError::Closed => BrowserFactReceiveError::Closed,
        })
    }

    pub fn try_recv(&mut self) -> Result<Arc<BrowserFactEnvelope>, BrowserFactTryReceiveError> {
        if let Some(fact) = self.replay.pop_front() {
            return Ok(fact);
        }
        self.receiver.try_recv().map_err(|error| match error {
            broadcast::error::TryRecvError::Empty => BrowserFactTryReceiveError::Empty,
            broadcast::error::TryRecvError::Lagged(skipped) => {
                BrowserFactTryReceiveError::Lagged { skipped }
            }
            broadcast::error::TryRecvError::Closed => BrowserFactTryReceiveError::Closed,
        })
    }
}

/// Bounded Browser-owned record plus non-blocking subscriber fanout.
pub(super) struct BrowserFactJournal {
    browser_instance_id: BrowserInstanceId,
    capacity: NonZeroUsize,
    next_sequence: Option<NonZeroU64>,
    retained: VecDeque<Arc<BrowserFactEnvelope>>,
    sender: broadcast::Sender<Arc<BrowserFactEnvelope>>,
    wake_sender: watch::Sender<Option<BrowserFactSequence>>,
}

impl BrowserFactJournal {
    pub(super) fn new(browser_instance_id: BrowserInstanceId) -> Self {
        let capacity =
            NonZeroUsize::new(DEFAULT_BROWSER_FACT_JOURNAL_CAPACITY).unwrap_or(NonZeroUsize::MIN);
        Self::with_capacity(browser_instance_id, capacity)
    }

    fn with_capacity(browser_instance_id: BrowserInstanceId, capacity: NonZeroUsize) -> Self {
        let (sender, _) = broadcast::channel(capacity.get());
        let (wake_sender, _) = watch::channel(None);
        Self {
            browser_instance_id,
            capacity,
            next_sequence: Some(NonZeroU64::MIN),
            retained: VecDeque::with_capacity(capacity.get()),
            sender,
            wake_sender,
        }
    }

    pub(super) fn subscribe(&self) -> BrowserFactSubscriber {
        // Register for future facts before copying the retained window. The
        // single Browser owner prevents concurrent mutation today, and this
        // order also preserves a gap-free cut if the journal later moves
        // behind an independently scheduled Host boundary.
        let receiver = self.sender.subscribe();
        BrowserFactSubscriber {
            browser_instance_id: self.browser_instance_id,
            replay: self.retained.iter().cloned().collect(),
            receiver,
        }
    }

    pub(super) fn subscribe_pair(&self) -> (BrowserFactSubscriber, BrowserFactSubscriber) {
        // Both receivers are registered before taking one retained snapshot,
        // so the pair represents one gap-free journal cut even after Browser
        // Host subscription becomes an independently scheduled operation.
        let first_receiver = self.sender.subscribe();
        let second_receiver = self.sender.subscribe();
        let replay = self.retained.iter().cloned().collect::<VecDeque<_>>();
        (
            BrowserFactSubscriber {
                browser_instance_id: self.browser_instance_id,
                replay: replay.clone(),
                receiver: first_receiver,
            },
            BrowserFactSubscriber {
                browser_instance_id: self.browser_instance_id,
                replay,
                receiver: second_receiver,
            },
        )
    }

    pub(super) fn subscribe_wake(&self) -> BrowserFactWakeSubscriber {
        BrowserFactWakeSubscriber {
            browser_instance_id: self.browser_instance_id,
            receiver: self.wake_sender.subscribe(),
            last_delivered: None,
        }
    }

    pub(super) fn snapshot(&self) -> Vec<Arc<BrowserFactEnvelope>> {
        self.retained.iter().cloned().collect()
    }

    pub(super) fn publish_batch(
        &mut self,
        page_residence: PageResidenceIdentity,
        facts: Vec<BrowserFact>,
    ) -> Result<Vec<Arc<BrowserFactEnvelope>>, BrowserFactPublishError> {
        if page_residence.target_identity().is_none() {
            return Err(BrowserFactPublishError::TargetlessPageResidence(
                page_residence,
            ));
        }
        if facts.is_empty() {
            return Ok(Vec::new());
        }
        let first_sequence = self
            .next_sequence
            .ok_or(BrowserFactPublishError::SequenceExhausted)?;
        let sequence_span = u64::try_from(facts.len() - 1)
            .map_err(|_| BrowserFactPublishError::SequenceExhausted)?;
        let last_sequence = first_sequence
            .get()
            .checked_add(sequence_span)
            .ok_or(BrowserFactPublishError::SequenceExhausted)?;
        self.next_sequence = last_sequence.checked_add(1).and_then(NonZeroU64::new);

        let mut published = Vec::with_capacity(facts.len());
        for (raw_sequence, fact) in (first_sequence.get()..=last_sequence).zip(facts) {
            let sequence =
                BrowserFactSequence(NonZeroU64::new(raw_sequence).unwrap_or(NonZeroU64::MIN));
            let envelope = Arc::new(BrowserFactEnvelope {
                sequence,
                browser_instance_id: self.browser_instance_id,
                page_residence: page_residence.clone(),
                fact,
            });
            if self.retained.len() == self.capacity.get() {
                self.retained.pop_front();
            }
            self.retained.push_back(Arc::clone(&envelope));
            // No active subscriber is a valid state. The retained ring still
            // records the transition, while future facts keep their sequence.
            let _ = self.sender.send(Arc::clone(&envelope));
            published.push(envelope);
        }
        // One payload-free notification closes the whole committed batch.
        // `send_replace` is non-blocking and coalesces any slow application
        // scheduler to the latest sequence; the bounded journal cursor still
        // detects whether the subscriber can consume the intervening facts.
        self.wake_sender.send_replace(Some(BrowserFactSequence(
            NonZeroU64::new(last_sequence).unwrap_or(NonZeroU64::MIN),
        )));
        Ok(published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PageId,
        page::{
            RendererDocumentLifecycleIdentity, RendererDocumentLifecycleMilestone,
            RendererDocumentToken, RendererFrameToken, RendererLifecycleEpoch,
            RendererLifecycleEventStamp,
        },
    };

    fn lifecycle_fact(sequence: u64) -> BrowserFact {
        let page_id = PageId::new_for_testing(7);
        BrowserFact::DocumentLifecycleReached {
            document: RendererDocumentLifecycleIdentity {
                frame: RendererFrameToken { page_id },
                document: RendererDocumentToken::new_for_testing(page_id, 3),
                epoch: RendererLifecycleEpoch(2),
            },
            milestone: RendererDocumentLifecycleMilestone::Load,
            stamp: RendererLifecycleEventStamp {
                sequence,
                timestamp_micros: sequence * 10,
            },
        }
    }

    fn publish(journal: &mut BrowserFactJournal, source_sequence: u64) {
        journal
            .publish_batch(
                PageResidenceIdentity::new("context-1".to_owned(), Some("target-1".to_owned()), 0),
                vec![lifecycle_fact(source_sequence)],
            )
            .expect("test fact sequence should remain available");
    }

    #[test]
    fn journal_retains_unsubscribed_facts_with_monotonic_browser_sequence() {
        let instance = BrowserInstanceId::allocate();
        let mut journal = BrowserFactJournal::with_capacity(
            instance,
            NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
        );

        publish(&mut journal, 10);
        publish(&mut journal, 20);
        publish(&mut journal, 30);

        let retained = journal.snapshot();
        assert_eq!(retained.len(), 2);
        assert_eq!(retained[0].sequence().get(), 2);
        assert_eq!(retained[1].sequence().get(), 3);
        assert!(
            retained
                .iter()
                .all(|fact| fact.browser_instance_id() == instance)
        );
        let mut subscriber = journal.subscribe();
        assert_eq!(
            subscriber
                .try_recv()
                .expect("bounded bootstrap should replay first retained fact")
                .sequence()
                .get(),
            2
        );
        assert_eq!(
            subscriber
                .try_recv()
                .expect("bounded bootstrap should replay second retained fact")
                .sequence()
                .get(),
            3
        );
    }

    #[test]
    fn envelope_identity_is_derived_from_its_target_backed_page_source() {
        let mut journal = BrowserFactJournal::new(BrowserInstanceId::allocate());
        publish(&mut journal, 10);

        let envelope = journal
            .snapshot()
            .pop()
            .expect("published fact should be retained");
        assert_eq!(envelope.browser_context_id().as_str(), "context-1");
        assert_eq!(envelope.target_id().as_str(), "target-1");
        assert_eq!(envelope.page_residence().browser_context_id(), "context-1");
        assert_eq!(envelope.page_residence().target_id(), Some("target-1"));
    }

    #[test]
    fn targetless_page_source_is_rejected_before_allocating_a_fact_sequence() {
        let mut journal = BrowserFactJournal::new(BrowserInstanceId::allocate());
        let targetless = PageResidenceIdentity::new("context-1".to_owned(), None, 0);

        assert_eq!(
            journal.publish_batch(targetless.clone(), vec![lifecycle_fact(10)]),
            Err(BrowserFactPublishError::TargetlessPageResidence(targetless))
        );
        publish(&mut journal, 20);
        assert_eq!(
            journal
                .snapshot()
                .pop()
                .expect("valid fact should publish after rejection")
                .sequence()
                .get(),
            1
        );
    }

    #[test]
    fn slow_subscriber_reports_bounded_lag_without_blocking_publication() {
        let instance = BrowserInstanceId::allocate();
        let mut journal = BrowserFactJournal::with_capacity(
            instance,
            NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
        );
        let mut subscriber = journal.subscribe();

        publish(&mut journal, 10);
        publish(&mut journal, 20);
        publish(&mut journal, 30);

        assert_eq!(
            subscriber.try_recv(),
            Err(BrowserFactTryReceiveError::Lagged { skipped: 1 })
        );
        assert_eq!(
            subscriber
                .try_recv()
                .expect("first retained subscriber fact")
                .sequence()
                .get(),
            2
        );
        assert_eq!(
            subscriber
                .try_recv()
                .expect("second retained subscriber fact")
                .sequence()
                .get(),
            3
        );
        assert_eq!(
            subscriber.try_recv(),
            Err(BrowserFactTryReceiveError::Empty)
        );
    }

    #[test]
    fn application_wake_coalesces_to_latest_sequence_and_bootstraps_late_subscriber() {
        let instance = BrowserInstanceId::allocate();
        let mut journal = BrowserFactJournal::with_capacity(
            instance,
            NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
        );
        let mut wake = journal.subscribe_wake();

        assert_eq!(wake.try_recv(), Err(BrowserFactWakeTryReceiveError::Empty));
        publish(&mut journal, 10);
        publish(&mut journal, 20);
        publish(&mut journal, 30);

        assert_eq!(
            wake.try_recv()
                .expect("one coalesced wake should expose the latest sequence")
                .get(),
            3
        );
        assert_eq!(wake.try_recv(), Err(BrowserFactWakeTryReceiveError::Empty));

        let mut late_wake = journal.subscribe_wake();
        assert_eq!(late_wake.browser_instance_id(), instance);
        assert_eq!(
            late_wake
                .try_recv()
                .expect("a late application subscriber should bootstrap the retained tail")
                .get(),
            3
        );
    }

    #[tokio::test]
    async fn application_wake_delivers_committed_tail_before_closed_terminal() {
        let instance = BrowserInstanceId::allocate();
        let mut journal = BrowserFactJournal::with_capacity(
            instance,
            NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN),
        );
        let mut wake = journal.subscribe_wake();
        publish(&mut journal, 10);
        drop(journal);

        assert_eq!(
            wake.recv()
                .await
                .expect("the committed tail must survive sender shutdown")
                .get(),
            1
        );
        assert_eq!(wake.recv().await, Err(BrowserFactWakeReceiveError::Closed));
    }
}
