use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    sync::Arc,
};

use moli_core::{
    browser_host::{
        BrowserDocumentNavigation, BrowserFact, BrowserFactEnvelope, BrowserFactSequence,
        BrowserFactSubscriber, BrowserFactTryReceiveError, BrowserNavigationFailure,
        BrowserTargetMetadataTransition, BrowserTargetTermination, BrowserTargetTerminationKind,
        PageResidenceIdentity,
    },
    page::{
        RendererDocumentLifecycleEvent, RendererDocumentLifecycleEventKind,
        RendererDocumentLifecycleIdentity, RendererDocumentLifecycleMilestone,
        RendererLifecycleEventStamp,
    },
};

use super::{CdpConnection, CommittedRendererDocumentBinding, DocumentNavigationToken};

// Keep frontend-local pending projection bounded by the same order of
// magnitude as the Browser journal. This is not a second fact journal: it
// retains only facts that have not yet met their frontend projection binding.
// Payload ownership remains in the Browser journal's shared envelopes.
const MAX_PENDING_BROWSER_FACTS: usize = 1_024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserFactProjectionError {
    SubscriberLagged {
        skipped: u64,
    },
    SubscriberClosed,
    NonMonotonicSequence {
        previous: BrowserFactSequence,
        received: BrowserFactSequence,
    },
    WakeSequenceNotObserved {
        wake: BrowserFactSequence,
        observed: Option<BrowserFactSequence>,
    },
    PendingBrowserFactCapacityExceeded {
        capacity: usize,
    },
    MissingTargetCreationFact {
        browser_context_id: String,
        target_id: String,
        page: Box<PageResidenceIdentity>,
    },
    MissingNavigationTargetMetadataFact {
        navigation: BrowserDocumentNavigation,
        page: Box<PageResidenceIdentity>,
    },
    MissingNavigationAdmissionFact {
        navigation: BrowserDocumentNavigation,
    },
    MissingNavigationFailureFact {
        navigation: BrowserDocumentNavigation,
        failure: BrowserNavigationFailure,
    },
    MissingNavigationDownloadFact {
        navigation: BrowserDocumentNavigation,
    },
    MissingNavigationCommitFact {
        navigation: BrowserDocumentNavigation,
        page: PageResidenceIdentity,
    },
    MissingTargetTerminationFact {
        target_id: String,
        previous_page: Box<PageResidenceIdentity>,
        terminal_page: Box<PageResidenceIdentity>,
        kind: BrowserTargetTerminationKind,
    },
    NoCurrentPageForDocumentLifecycle {
        document: RendererDocumentLifecycleIdentity,
        milestone: RendererDocumentLifecycleMilestone,
        stamp: RendererLifecycleEventStamp,
    },
    MissingDocumentLifecycleFact {
        page: PageResidenceIdentity,
        document: RendererDocumentLifecycleIdentity,
        milestone: RendererDocumentLifecycleMilestone,
        stamp: RendererLifecycleEventStamp,
    },
    ConflictingDocumentLifecycleCausalLink {
        sequence: BrowserFactSequence,
    },
    MismatchedDocumentLifecycleCausalLink {
        sequence: BrowserFactSequence,
        page: PageResidenceIdentity,
        document: RendererDocumentLifecycleIdentity,
    },
}

impl fmt::Display for BrowserFactProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SubscriberLagged { skipped } => {
                write!(
                    formatter,
                    "Browser fact projector lagged by {skipped} facts"
                )
            }
            Self::SubscriberClosed => formatter.write_str("Browser fact projector closed"),
            Self::NonMonotonicSequence { previous, received } => write!(
                formatter,
                "Browser fact sequence moved from {} to non-monotonic {}",
                previous.get(),
                received.get()
            ),
            Self::WakeSequenceNotObserved { wake, observed } => write!(
                formatter,
                "Browser fact wake reached sequence {} but the frontend cursor reached only {}",
                wake.get(),
                observed.map_or(0, BrowserFactSequence::get)
            ),
            Self::PendingBrowserFactCapacityExceeded { capacity } => write!(
                formatter,
                "Browser fact projector exceeded its {capacity}-fact pending projection window"
            ),
            Self::MissingTargetCreationFact {
                browser_context_id,
                target_id,
                page,
            } => write!(
                formatter,
                "Target {target_id:?} in BrowserContext {browser_context_id:?} at Page {:?} has no exact Browser creation fact",
                page
            ),
            Self::MissingNavigationTargetMetadataFact { navigation, page } => write!(
                formatter,
                "committed navigation {navigation:?} for Page {page:?} has no exact Target metadata Browser fact"
            ),
            Self::MissingNavigationAdmissionFact { navigation } => write!(
                formatter,
                "accepted navigation {:?} has no exact Browser fact",
                navigation
            ),
            Self::MissingNavigationFailureFact {
                navigation,
                failure,
            } => write!(
                formatter,
                "failed navigation {:?} with terminal {:?} has no exact Browser fact",
                navigation, failure
            ),
            Self::MissingNavigationDownloadFact { navigation } => write!(
                formatter,
                "download navigation {:?} has no exact Browser fact",
                navigation
            ),
            Self::MissingNavigationCommitFact { navigation, page } => write!(
                formatter,
                "committed navigation {:?} for Page {:?} has no exact Browser fact",
                navigation, page
            ),
            Self::MissingTargetTerminationFact {
                target_id,
                previous_page,
                terminal_page,
                kind,
            } => write!(
                formatter,
                "Target {target_id:?} {kind:?} from Page {:?} to terminal Page {:?} has no exact Browser fact",
                previous_page, terminal_page
            ),
            Self::NoCurrentPageForDocumentLifecycle {
                document,
                milestone,
                stamp,
            } => write!(
                formatter,
                "protocol-visible renderer lifecycle {:?} at sequence {} for Document {:?} has no current Browser Page",
                milestone, stamp.sequence, document
            ),
            Self::MissingDocumentLifecycleFact {
                page,
                document,
                milestone,
                stamp,
            } => write!(
                formatter,
                "protocol-visible renderer lifecycle {:?} at sequence {} for Page {:?}, Document {:?} has no exact Browser fact",
                milestone, stamp.sequence, page, document
            ),
            Self::ConflictingDocumentLifecycleCausalLink { sequence } => write!(
                formatter,
                "Browser lifecycle fact {} was linked to conflicting frontend bindings",
                sequence.get()
            ),
            Self::MismatchedDocumentLifecycleCausalLink {
                sequence,
                page,
                document,
            } => write!(
                formatter,
                "Browser lifecycle fact {} for Page {:?}, Document {:?} does not match its frozen frontend binding",
                sequence.get(),
                page,
                document
            ),
        }
    }
}

impl std::error::Error for BrowserFactProjectionError {}

#[derive(Clone, Debug)]
pub(crate) struct BrowserDocumentLifecycleFactProjection {
    envelope: Arc<BrowserFactEnvelope>,
    binding: CommittedRendererDocumentBinding,
}

impl BrowserDocumentLifecycleFactProjection {
    pub(crate) fn envelope(&self) -> &BrowserFactEnvelope {
        self.envelope.as_ref()
    }

    pub(crate) fn binding(&self) -> &CommittedRendererDocumentBinding {
        &self.binding
    }

    pub(crate) fn reached(
        &self,
    ) -> Option<(
        RendererDocumentLifecycleIdentity,
        RendererDocumentLifecycleMilestone,
        RendererLifecycleEventStamp,
    )> {
        let BrowserFact::DocumentLifecycleReached {
            document,
            milestone,
            stamp,
        } = self.envelope.fact()
        else {
            return None;
        };
        Some((*document, *milestone, *stamp))
    }
}

#[derive(Debug)]
struct PendingBrowserFact {
    envelope: Arc<BrowserFactEnvelope>,
    document_binding: Option<CommittedRendererDocumentBinding>,
}

/// Exact Browser occurrence authorizing one live top-level Target creation
/// projection. A single fact may fan out to CDP page/tab events and the typed
/// automation sidecar, but it can be claimed only once by this frontend.
#[derive(Clone, Debug)]
pub(crate) struct BrowserTargetCreatedFactProjection {
    envelope: Arc<BrowserFactEnvelope>,
}

impl BrowserTargetCreatedFactProjection {
    pub(crate) fn envelope(&self) -> &BrowserFactEnvelope {
        self.envelope.as_ref()
    }

    pub(crate) fn matches_residence_instance(&self, current: &PageResidenceIdentity) -> bool {
        self.envelope
            .page_residence()
            .same_residence_instance(current)
    }
}

/// Exact Browser occurrence authorizing one top-level Target metadata
/// projection. CDP attachment/discovery state is joined only after this fact
/// has been claimed by the frontend cursor.
#[derive(Clone, Debug)]
pub(crate) struct BrowserTargetMetadataFactProjection {
    envelope: Arc<BrowserFactEnvelope>,
}

impl BrowserTargetMetadataFactProjection {
    pub(crate) fn envelope(&self) -> &BrowserFactEnvelope {
        self.envelope.as_ref()
    }

    pub(crate) fn transition(&self) -> &BrowserTargetMetadataTransition {
        let BrowserFact::TargetMetadataChanged { transition } = self.envelope.fact() else {
            unreachable!("Target metadata projection must wrap its claimed fact")
        };
        transition
    }
}

/// Exact Browser fact authorizing one committed cross-Document projection.
///
/// The renderer commit supplies URL/security data later, but it cannot emit a
/// top-level commit unless this occurrence was observed from the Browser
/// journal.
#[derive(Clone, Debug)]
pub(crate) struct BrowserNavigationCommitFactProjection {
    committed: Arc<BrowserFactEnvelope>,
}

impl BrowserNavigationCommitFactProjection {
    pub(crate) fn sequence(&self) -> BrowserFactSequence {
        self.committed.sequence()
    }
}

/// Exact Browser fact authorizing one Target terminal projection.
#[derive(Clone, Debug)]
pub(crate) struct BrowserTargetTerminationFactProjection {
    terminal: Arc<BrowserFactEnvelope>,
}

impl BrowserTargetTerminationFactProjection {
    pub(crate) fn envelope(&self) -> &BrowserFactEnvelope {
        self.terminal.as_ref()
    }
}

/// One CDP/frontend cursor over the protocol-neutral Browser fact journal.
///
/// The cursor never publishes Browser state and never waits inside a Browser
/// Owner turn. Renderer lifecycle records still release the existing command
/// response visibility barrier, but they cannot authorize a DCL/load event
/// unless this projector observed the matching immutable Browser fact.
pub(crate) struct CdpBrowserFactProjector {
    subscriber: Option<BrowserFactSubscriber>,
    pending_facts: VecDeque<PendingBrowserFact>,
    lifecycle_causal_links: BTreeMap<BrowserFactSequence, CommittedRendererDocumentBinding>,
    last_observed_sequence: Option<BrowserFactSequence>,
    #[cfg(test)]
    last_projected_sequence: Option<BrowserFactSequence>,
    terminal_error: Option<BrowserFactProjectionError>,
}

impl CdpBrowserFactProjector {
    pub(crate) fn new(subscriber: BrowserFactSubscriber) -> Self {
        Self {
            subscriber: Some(subscriber),
            pending_facts: VecDeque::new(),
            lifecycle_causal_links: BTreeMap::new(),
            last_observed_sequence: None,
            #[cfg(test)]
            last_projected_sequence: None,
            terminal_error: None,
        }
    }

    /// Freezes the frontend attachment selected by the renderer ingress for
    /// each exact lifecycle fact without advancing this frontend's journal
    /// cursor.
    ///
    /// The Browser fact remains the occurrence authority. This causal link is
    /// only the protocol-neutral fact sequence to frontend projection binding
    /// join required later to preserve loader/session attribution. A separate
    /// application fact wake may therefore advance the cursor before the
    /// visible renderer record is projected.
    pub(crate) fn register_document_lifecycle_causal_links(
        &mut self,
        page: &PageResidenceIdentity,
        binding: &CommittedRendererDocumentBinding,
        published: &[Arc<BrowserFactEnvelope>],
    ) -> Result<(), BrowserFactProjectionError> {
        if let Some(error) = self.terminal_error.as_ref() {
            return Err(error.clone());
        }

        for envelope in published {
            let BrowserFact::DocumentLifecycleReached { document, .. } = envelope.fact() else {
                continue;
            };
            if !lifecycle_fact_matches_binding(envelope, Some(page), binding) {
                return Err(self.fail(
                    BrowserFactProjectionError::MismatchedDocumentLifecycleCausalLink {
                        sequence: envelope.sequence(),
                        page: page.clone(),
                        document: *document,
                    },
                ));
            }
            let sequence = envelope.sequence();
            if let Some(pending) = self
                .pending_facts
                .iter_mut()
                .find(|pending| pending.envelope.sequence() == sequence)
            {
                match pending.document_binding.as_ref() {
                    Some(previous) if previous != binding => {
                        return Err(self.fail(
                            BrowserFactProjectionError::ConflictingDocumentLifecycleCausalLink {
                                sequence,
                            },
                        ));
                    }
                    Some(_) => {}
                    None => pending.document_binding = Some(binding.clone()),
                }
                continue;
            }
            if let Some(previous) = self.lifecycle_causal_links.get(&sequence) {
                if previous != binding {
                    return Err(self.fail(
                        BrowserFactProjectionError::ConflictingDocumentLifecycleCausalLink {
                            sequence,
                        },
                    ));
                }
                continue;
            }
            self.lifecycle_causal_links
                .insert(sequence, binding.clone());
        }

        // The Browser journal retains at most this many facts. Causal links
        // older than its latest reachable window can no longer be delivered
        // without the subscriber first reporting typed lag, so retaining them
        // would only form a second unbounded queue.
        if let Some(latest) = published.last().map(|envelope| envelope.sequence()) {
            let retained_floor = latest
                .get()
                .saturating_sub((MAX_PENDING_BROWSER_FACTS as u64).saturating_sub(1));
            self.lifecycle_causal_links
                .retain(|sequence, _| sequence.get() >= retained_floor);
        }
        Ok(())
    }

    pub(crate) fn capture_available(&mut self) -> Result<(), BrowserFactProjectionError> {
        if let Some(error) = self.terminal_error.as_ref() {
            return Err(error.clone());
        }

        loop {
            let received = match self.subscriber.as_mut() {
                Some(subscriber) => subscriber.try_recv(),
                None => return Err(self.fail(BrowserFactProjectionError::SubscriberClosed)),
            };
            match received {
                Ok(envelope) => self.observe(envelope)?,
                Err(BrowserFactTryReceiveError::Empty) => break,
                Err(BrowserFactTryReceiveError::Lagged { skipped }) => {
                    return Err(self.fail(BrowserFactProjectionError::SubscriberLagged { skipped }));
                }
                Err(BrowserFactTryReceiveError::Closed) => {
                    return Err(self.fail(BrowserFactProjectionError::SubscriberClosed));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn capture_wake(
        &mut self,
        through: BrowserFactSequence,
    ) -> Result<(), BrowserFactProjectionError> {
        self.capture_available()?;
        if self
            .last_observed_sequence
            .is_none_or(|observed| observed < through)
        {
            return Err(
                self.fail(BrowserFactProjectionError::WakeSequenceNotObserved {
                    wake: through,
                    observed: self.last_observed_sequence,
                }),
            );
        }
        Ok(())
    }

    /// Claims the unique occurrence fact for one committed top-level Target.
    pub(crate) fn take_target_created_fact(
        &mut self,
        page: &PageResidenceIdentity,
    ) -> Result<BrowserTargetCreatedFactProjection, BrowserFactProjectionError> {
        self.capture_available()?;
        let position = self.pending_facts.iter().position(|pending| {
            pending.envelope.browser_context_id().as_str() == page.browser_context_id()
                && pending.envelope.target_id().as_str() == page.target_id().unwrap_or_default()
                && pending.envelope.page_residence() == page
                && matches!(pending.envelope.fact(), BrowserFact::TargetCreated)
        });
        let Some(position) = position else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingTargetCreationFact {
                    browser_context_id: page.browser_context_id().to_owned(),
                    target_id: page.target_id().unwrap_or_default().to_owned(),
                    page: Box::new(page.clone()),
                }),
            );
        };
        let Some(created) = self.pending_facts.remove(position) else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingTargetCreationFact {
                    browser_context_id: page.browser_context_id().to_owned(),
                    target_id: page.target_id().unwrap_or_default().to_owned(),
                    page: Box::new(page.clone()),
                }),
            );
        };
        self.record_projected(created.envelope.sequence());
        Ok(BrowserTargetCreatedFactProjection {
            envelope: created.envelope,
        })
    }

    pub(crate) fn take_navigation_target_metadata_changed_fact(
        &mut self,
        navigation: &BrowserDocumentNavigation,
        page: &PageResidenceIdentity,
    ) -> Result<BrowserTargetMetadataFactProjection, BrowserFactProjectionError> {
        self.capture_available()?;
        let position = self.pending_facts.iter().position(|pending| {
            pending.envelope.page_residence() == page
                && matches!(
                    pending.envelope.fact(),
                    BrowserFact::TargetMetadataChanged {
                        transition: observed,
                    } if observed.navigation() == navigation
                )
        });
        let Some(position) = position else {
            return Err(self.fail(
                BrowserFactProjectionError::MissingNavigationTargetMetadataFact {
                    navigation: navigation.clone(),
                    page: Box::new(page.clone()),
                },
            ));
        };
        let Some(changed) = self.pending_facts.remove(position) else {
            return Err(self.fail(
                BrowserFactProjectionError::MissingNavigationTargetMetadataFact {
                    navigation: navigation.clone(),
                    page: Box::new(page.clone()),
                },
            ));
        };
        self.record_projected(changed.envelope.sequence());
        Ok(BrowserTargetMetadataFactProjection {
            envelope: changed.envelope,
        })
    }

    /// Claims the self-contained occurrence produced by one accepted request.
    pub(crate) fn take_navigation_admission_fact(
        &mut self,
        navigation: &BrowserDocumentNavigation,
    ) -> Result<Arc<BrowserFactEnvelope>, BrowserFactProjectionError> {
        self.capture_available()?;
        let Some(position) = self.pending_facts.iter().position(|pending| {
            matches!(
                pending.envelope.fact(),
                BrowserFact::NavigationAccepted {
                    navigation: accepted,
                    ..
                } if accepted == navigation
            )
        }) else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingNavigationAdmissionFact {
                    navigation: navigation.clone(),
                }),
            );
        };
        let Some(accepted) = self.pending_facts.remove(position) else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingNavigationAdmissionFact {
                    navigation: navigation.clone(),
                }),
            );
        };
        self.record_projected(accepted.envelope.sequence());
        Ok(accepted.envelope)
    }

    pub(crate) fn take_navigation_failure_fact(
        &mut self,
        navigation: &BrowserDocumentNavigation,
        failure: &BrowserNavigationFailure,
    ) -> Result<Arc<BrowserFactEnvelope>, BrowserFactProjectionError> {
        self.capture_available()?;
        let position = self.pending_facts.iter().position(|pending| {
            matches!(
                pending.envelope.fact(),
                BrowserFact::NavigationFailed {
                    navigation: failed_navigation,
                    failure: observed_failure,
                    ..
                } if failed_navigation == navigation && observed_failure == failure
            )
        });
        let Some(position) = position else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingNavigationFailureFact {
                    navigation: navigation.clone(),
                    failure: failure.clone(),
                }),
            );
        };
        let Some(failed) = self.pending_facts.remove(position) else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingNavigationFailureFact {
                    navigation: navigation.clone(),
                    failure: failure.clone(),
                }),
            );
        };
        self.record_projected(failed.envelope.sequence());
        Ok(failed.envelope)
    }

    pub(crate) fn take_navigation_download_fact(
        &mut self,
        navigation: &BrowserDocumentNavigation,
    ) -> Result<Arc<BrowserFactEnvelope>, BrowserFactProjectionError> {
        self.capture_available()?;
        let position = self.pending_facts.iter().position(|pending| {
            matches!(
                pending.envelope.fact(),
                BrowserFact::NavigationConvertedToDownload {
                    navigation: downloaded,
                } if downloaded == navigation
            )
        });
        let Some(position) = position else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingNavigationDownloadFact {
                    navigation: navigation.clone(),
                }),
            );
        };
        let Some(downloaded) = self.pending_facts.remove(position) else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingNavigationDownloadFact {
                    navigation: navigation.clone(),
                }),
            );
        };
        self.record_projected(downloaded.envelope.sequence());
        Ok(downloaded.envelope)
    }

    /// Claims the atomic navigation-outcome/Page-topology fact before a
    /// renderer commit is allowed to release the existing CDP event shape.
    pub(crate) fn take_navigation_commit_fact(
        &mut self,
        navigation: &BrowserDocumentNavigation,
        page: &PageResidenceIdentity,
    ) -> Result<BrowserNavigationCommitFactProjection, BrowserFactProjectionError> {
        self.capture_available()?;
        let position = self.pending_facts.iter().position(|pending| {
            pending.envelope.page_residence() == page
                && matches!(
                    pending.envelope.fact(),
                    BrowserFact::NavigationCommitted {
                        navigation: committed,
                        ..
                    } if committed == navigation
                )
        });
        let Some(position) = position else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingNavigationCommitFact {
                    navigation: navigation.clone(),
                    page: page.clone(),
                }),
            );
        };
        let Some(committed) = self.pending_facts.remove(position) else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingNavigationCommitFact {
                    navigation: navigation.clone(),
                    page: page.clone(),
                }),
            );
        };
        self.record_projected(committed.envelope.sequence());
        Ok(BrowserNavigationCommitFactProjection {
            committed: committed.envelope,
        })
    }

    /// Claims one self-contained Target terminal occurrence.
    pub(crate) fn take_target_termination_fact(
        &mut self,
        termination: &BrowserTargetTermination,
    ) -> Result<BrowserTargetTerminationFactProjection, BrowserFactProjectionError> {
        self.capture_available()?;
        let terminal_position = self.pending_facts.iter().position(|pending| {
            pending.envelope.target_id().as_str() == termination.owner().target_id()
                && pending.envelope.page_residence() == termination.terminal_page()
                && match (termination.kind(), pending.envelope.fact()) {
                    (
                        BrowserTargetTerminationKind::Crash,
                        BrowserFact::TargetCrashed { previous_page, .. },
                    )
                    | (
                        BrowserTargetTerminationKind::Close,
                        BrowserFact::TargetClosed { previous_page, .. },
                    ) => previous_page == termination.previous_page(),
                    _ => false,
                }
        });
        let Some(terminal_position) = terminal_position else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingTargetTerminationFact {
                    target_id: termination.owner().target_id().to_owned(),
                    previous_page: Box::new(termination.previous_page().clone()),
                    terminal_page: Box::new(termination.terminal_page().clone()),
                    kind: termination.kind(),
                }),
            );
        };
        let Some(terminal) = self.pending_facts.remove(terminal_position) else {
            return Err(
                self.fail(BrowserFactProjectionError::MissingTargetTerminationFact {
                    target_id: termination.owner().target_id().to_owned(),
                    previous_page: Box::new(termination.previous_page().clone()),
                    terminal_page: Box::new(termination.terminal_page().clone()),
                    kind: termination.kind(),
                }),
            );
        };
        self.record_projected(terminal.envelope.sequence());
        Ok(BrowserTargetTerminationFactProjection {
            terminal: terminal.envelope,
        })
    }

    pub(crate) fn take_visible_document_lifecycle_facts(
        &mut self,
        page: Option<&PageResidenceIdentity>,
        binding: &CommittedRendererDocumentBinding,
        visible_events: &[RendererDocumentLifecycleEvent],
    ) -> Result<Vec<BrowserDocumentLifecycleFactProjection>, BrowserFactProjectionError> {
        let Some((milestone, stamp)) = visible_events.iter().find_map(|event| {
            if event.frame != binding.renderer_frame || event.document != binding.renderer_document
            {
                return None;
            }
            let RendererDocumentLifecycleEventKind::Milestone(milestone) = event.kind else {
                return None;
            };
            Some((
                milestone,
                RendererLifecycleEventStamp {
                    sequence: event.sequence,
                    timestamp_micros: event.timestamp_micros,
                },
            ))
        }) else {
            return Ok(Vec::new());
        };
        let Some(page) = page else {
            return Err(self.fail(
                BrowserFactProjectionError::NoCurrentPageForDocumentLifecycle {
                    document: binding.renderer_document_identity(),
                    milestone,
                    stamp,
                },
            ));
        };
        self.capture_available()?;

        let mut projected = Vec::new();
        for event in visible_events {
            if event.frame != binding.renderer_frame || event.document != binding.renderer_document
            {
                continue;
            }
            let RendererDocumentLifecycleEventKind::Milestone(milestone) = event.kind else {
                continue;
            };
            let document = RendererDocumentLifecycleIdentity {
                frame: event.frame,
                document: event.document,
                epoch: event.epoch,
            };
            let stamp = RendererLifecycleEventStamp {
                sequence: event.sequence,
                timestamp_micros: event.timestamp_micros,
            };
            let position = self.pending_facts.iter().position(|pending| {
                pending.envelope.page_residence() == page
                    && pending.document_binding.as_ref() == Some(binding)
                    && matches!(
                        pending.envelope.fact(),
                        BrowserFact::DocumentLifecycleReached {
                            document: fact_document,
                            milestone: fact_milestone,
                            stamp: fact_stamp,
                        } if *fact_document == document
                            && *fact_milestone == milestone
                            && *fact_stamp == stamp
                    )
            });
            let Some(position) = position else {
                return Err(
                    self.fail(BrowserFactProjectionError::MissingDocumentLifecycleFact {
                        page: page.clone(),
                        document,
                        milestone,
                        stamp,
                    }),
                );
            };
            let requested_sequence = self.pending_facts[position].envelope.sequence();
            let Some(pending) = self.pending_facts.remove(position) else {
                return Err(
                    self.fail(BrowserFactProjectionError::MissingDocumentLifecycleFact {
                        page: page.clone(),
                        document,
                        milestone,
                        stamp,
                    }),
                );
            };
            // A frontend may begin observing between DCL and load. Chromium's
            // Page.enable does not replay Page.domContentEventFired, so a
            // later exact load is allowed to cross an earlier lifecycle fact
            // that this frontend never selected. Retire those older facts now
            // so the bounded queue cannot leak them or project them backwards.
            self.pending_facts.retain(|candidate| {
                !matches!(
                    candidate.envelope.fact(),
                    BrowserFact::DocumentLifecycleReached { .. }
                ) || candidate.envelope.page_residence() != page
                    || candidate.document_binding.as_ref() != Some(binding)
                    || candidate.envelope.sequence() >= requested_sequence
            });
            let Some(frozen_binding) = pending.document_binding else {
                return Err(
                    self.fail(BrowserFactProjectionError::MissingDocumentLifecycleFact {
                        page: page.clone(),
                        document,
                        milestone,
                        stamp,
                    }),
                );
            };
            self.record_projected(pending.envelope.sequence());
            projected.push(BrowserDocumentLifecycleFactProjection {
                envelope: pending.envelope,
                binding: frozen_binding,
            });
        }
        Ok(projected)
    }

    fn observe(
        &mut self,
        envelope: Arc<BrowserFactEnvelope>,
    ) -> Result<(), BrowserFactProjectionError> {
        if let Some(previous) = self.last_observed_sequence
            && envelope.sequence() <= previous
        {
            return Err(self.fail(BrowserFactProjectionError::NonMonotonicSequence {
                previous,
                received: envelope.sequence(),
            }));
        }
        self.last_observed_sequence = Some(envelope.sequence());

        // Document termination is consumed by the independent high-level wait
        // subscriber and has no CDP event shape. Advancing the common cursor is
        // sufficient; retaining it would leak one pending frontend item per
        // replacement.
        if matches!(
            envelope.fact(),
            BrowserFact::DocumentLifecycleTerminated { .. }
        ) {
            return Ok(());
        }
        if let Some(retired_page) = retired_page_residence(envelope.fact()) {
            self.pending_facts
                .retain(|pending| match pending.envelope.fact() {
                    BrowserFact::TargetCreated => !pending
                        .envelope
                        .page_residence()
                        .same_residence_instance(retired_page),
                    BrowserFact::TargetMetadataChanged { .. }
                    | BrowserFact::DocumentLifecycleReached { .. } => {
                        pending.envelope.page_residence() != retired_page
                    }
                    _ => true,
                });
        }
        if self.pending_facts.len() == MAX_PENDING_BROWSER_FACTS {
            return Err(self.fail(
                BrowserFactProjectionError::PendingBrowserFactCapacityExceeded {
                    capacity: MAX_PENDING_BROWSER_FACTS,
                },
            ));
        }
        let document_binding = matches!(
            envelope.fact(),
            BrowserFact::DocumentLifecycleReached { .. }
        )
        .then(|| self.lifecycle_causal_links.remove(&envelope.sequence()))
        .flatten();
        self.pending_facts.push_back(PendingBrowserFact {
            envelope,
            document_binding,
        });
        Ok(())
    }

    fn record_projected(&mut self, sequence: BrowserFactSequence) {
        #[cfg(test)]
        {
            self.last_projected_sequence = Some(
                self.last_projected_sequence
                    .map_or(sequence, |previous| previous.max(sequence)),
            );
        }
        #[cfg(not(test))]
        let _ = sequence;
    }

    fn fail(&mut self, error: BrowserFactProjectionError) -> BrowserFactProjectionError {
        self.subscriber = None;
        self.pending_facts.clear();
        self.lifecycle_causal_links.clear();
        self.terminal_error = Some(error.clone());
        error
    }

    #[cfg(test)]
    pub(crate) fn terminal_error(&self) -> Option<&BrowserFactProjectionError> {
        self.terminal_error.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn pending_document_lifecycle_fact_count(&self) -> usize {
        self.pending_facts
            .iter()
            .filter(|pending| {
                matches!(
                    pending.envelope.fact(),
                    BrowserFact::DocumentLifecycleReached { .. }
                )
            })
            .count()
    }

    #[cfg(test)]
    pub(crate) fn pending_fact_count(&self) -> usize {
        self.pending_facts.len()
    }

    #[cfg(test)]
    pub(crate) fn last_projected_sequence(&self) -> Option<BrowserFactSequence> {
        self.last_projected_sequence
    }
}

/// Frontend-facing Browser fact boundary for one connection.
///
/// `CdpConnection` delegates here instead of exposing its journal cursor to
/// Page, Target, BiDi, or scheduler domains. Core publication and frontend
/// projection remain distinct even while both residences are fields of the
/// migration-period connection object.
impl CdpConnection {
    pub(crate) fn record_authoritative_renderer_document_lifecycle_facts(
        &mut self,
        page: Option<&PageResidenceIdentity>,
        binding: Option<&CommittedRendererDocumentBinding>,
        events: &[RendererDocumentLifecycleEvent],
    ) {
        let lifecycle_fact_count = events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    RendererDocumentLifecycleEventKind::Milestone(_)
                        | RendererDocumentLifecycleEventKind::Terminated { .. }
                )
            })
            .count();
        if events.is_empty() {
            return;
        }
        let Some(page) = page else {
            tracing::warn!(
                lifecycle_fact_count,
                "accepted renderer lifecycle facts have no exact Browser Page residence"
            );
            return;
        };
        let published = match self
            .browser_host_state
            .record_document_lifecycle_facts(page, events)
        {
            Ok(published) => {
                tracing::trace!(
                    browser_context_id = page.browser_context_id(),
                    target_id = ?page.target_id(),
                    page_residence_generation = page.loaded_page_generation(),
                    fact_count = published.len(),
                    "published exact renderer lifecycle Browser facts"
                );
                published
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    browser_context_id = page.browser_context_id(),
                    target_id = ?page.target_id(),
                    page_residence_generation = page.loaded_page_generation(),
                    lifecycle_fact_count,
                    "rejected renderer lifecycle Browser facts"
                );
                return;
            }
        };
        let Some(binding) = binding else {
            return;
        };
        if let Err(error) = self
            .browser_fact_projector
            .register_document_lifecycle_causal_links(page, binding, &published)
        {
            tracing::error!(
                %error,
                browser_context_id = page.browser_context_id(),
                target_id = ?page.target_id(),
                page_residence_generation = page.loaded_page_generation(),
                "frontend Browser fact projector rejected a lifecycle causal link"
            );
        }
    }

    /// Creates a payload-free wake subscription for the application owner
    /// loop. The connection-local projector retains the independent bounded
    /// fact cursor; this wake can be dropped or coalesced without changing
    /// Browser Owner progress.
    pub fn subscribe_browser_fact_wake(
        &self,
    ) -> moli_core::browser_host::BrowserFactWakeSubscriber {
        self.browser_host_state
            .navigation_owner()
            .subscribe_browser_fact_wake()
    }

    /// Advances the one frontend fact cursor through a coalesced application
    /// wake. Fact-family projection claims below consume that same cursor's
    /// immutable pending window; domains never subscribe to the journal.
    pub fn capture_browser_fact_wake(
        &mut self,
        through: BrowserFactSequence,
    ) -> Result<(), BrowserFactProjectionError> {
        self.browser_fact_projector.capture_wake(through)
    }

    pub(crate) fn take_target_created_fact(
        &mut self,
        page: &PageResidenceIdentity,
    ) -> Result<BrowserTargetCreatedFactProjection, BrowserFactProjectionError> {
        self.browser_fact_projector.take_target_created_fact(page)
    }

    pub(crate) fn take_navigation_target_metadata_changed_fact(
        &mut self,
        navigation: &DocumentNavigationToken,
        page: &PageResidenceIdentity,
    ) -> Result<BrowserTargetMetadataFactProjection, BrowserFactProjectionError> {
        self.browser_fact_projector
            .take_navigation_target_metadata_changed_fact(navigation, page)
    }

    /// Revalidates that a delayed frontend creation projection still names
    /// the same Target Page-slot instance. Initial Document generation changes
    /// are allowed; close/reuse of the public Target id is not.
    pub(crate) fn target_created_fact_matches_current_target(
        &self,
        projection: &BrowserTargetCreatedFactProjection,
    ) -> bool {
        let envelope = projection.envelope();
        self.browser_host_state
            .navigation_owner()
            .capture_page_residence(
                envelope.browser_context_id().as_str(),
                envelope.target_id().as_str(),
            )
            .is_some_and(|current| projection.matches_residence_instance(&current))
    }

    pub(crate) fn take_navigation_admission_fact(
        &mut self,
        navigation: &DocumentNavigationToken,
    ) -> Result<Arc<BrowserFactEnvelope>, BrowserFactProjectionError> {
        self.browser_fact_projector
            .take_navigation_admission_fact(navigation)
    }

    pub(crate) fn take_navigation_failure_fact(
        &mut self,
        navigation: &DocumentNavigationToken,
        failure: &BrowserNavigationFailure,
    ) -> Result<Arc<BrowserFactEnvelope>, BrowserFactProjectionError> {
        self.browser_fact_projector
            .take_navigation_failure_fact(navigation, failure)
    }

    pub(crate) fn take_navigation_download_fact(
        &mut self,
        navigation: &DocumentNavigationToken,
    ) -> Result<Arc<BrowserFactEnvelope>, BrowserFactProjectionError> {
        self.browser_fact_projector
            .take_navigation_download_fact(navigation)
    }

    pub(crate) fn take_navigation_commit_fact(
        &mut self,
        navigation: &DocumentNavigationToken,
        page: &PageResidenceIdentity,
    ) -> Result<BrowserNavigationCommitFactProjection, BrowserFactProjectionError> {
        self.browser_fact_projector
            .take_navigation_commit_fact(navigation, page)
    }

    pub(crate) fn take_target_termination_fact(
        &mut self,
        termination: &BrowserTargetTermination,
    ) -> Result<BrowserTargetTerminationFactProjection, BrowserFactProjectionError> {
        self.browser_fact_projector
            .take_target_termination_fact(termination)
    }

    pub(crate) fn take_visible_renderer_document_lifecycle_facts(
        &mut self,
        session_id: Option<&str>,
        binding: &CommittedRendererDocumentBinding,
        events: &[RendererDocumentLifecycleEvent],
    ) -> Result<Vec<BrowserDocumentLifecycleFactProjection>, BrowserFactProjectionError> {
        let page = self.target_page_residence_identity_for_session(session_id);
        self.browser_fact_projector
            .take_visible_document_lifecycle_facts(page.as_ref(), binding, events)
    }

    #[cfg(test)]
    pub(crate) fn browser_fact_snapshot_for_test(&self) -> Vec<Arc<BrowserFactEnvelope>> {
        self.browser_host_state
            .navigation_owner()
            .browser_fact_snapshot()
    }

    #[cfg(test)]
    pub(crate) fn last_projected_browser_fact_sequence_for_test(&self) -> Option<u64> {
        self.browser_fact_projector
            .last_projected_sequence()
            .map(BrowserFactSequence::get)
    }
}

fn lifecycle_fact_matches_binding(
    envelope: &BrowserFactEnvelope,
    page: Option<&PageResidenceIdentity>,
    binding: &CommittedRendererDocumentBinding,
) -> bool {
    page == Some(envelope.page_residence())
        && matches!(
            envelope.fact(),
            BrowserFact::DocumentLifecycleReached { document, .. }
                if document.frame == binding.renderer_frame
                    && document.document == binding.renderer_document
        )
}

fn retired_page_residence(fact: &BrowserFact) -> Option<&PageResidenceIdentity> {
    match fact {
        BrowserFact::NavigationFailed {
            previous_page: Some(previous_page),
            ..
        }
        | BrowserFact::NavigationCommitted { previous_page, .. }
        | BrowserFact::TargetCrashed { previous_page, .. }
        | BrowserFact::TargetClosed { previous_page, .. } => Some(previous_page),
        BrowserFact::TargetCreated
        | BrowserFact::TargetMetadataChanged { .. }
        | BrowserFact::NavigationAccepted { .. }
        | BrowserFact::NavigationFailed {
            previous_page: None,
            ..
        }
        | BrowserFact::NavigationConvertedToDownload { .. }
        | BrowserFact::DocumentLifecycleReached { .. }
        | BrowserFact::DocumentLifecycleTerminated { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use moli_core::{
        PageId,
        browser_host::{
            BrowserContextSelectionProjection, BrowserNavigationHistoryPageSnapshot,
            BrowserNavigationOwner, BrowserPageResidenceHandle,
            BrowserSelectedTargetEngineDisposition, BrowserTargetHandle,
            BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
        },
        page::{
            RendererDocumentLifecycleEvent, RendererDocumentLifecycleEventKind,
            RendererDocumentLifecycleMilestone, RendererDocumentToken, RendererFrameToken,
            RendererLifecycleEpoch,
        },
        runtime::NavigationEngine,
    };

    use super::*;
    use crate::conn::{DocumentNavigationToken, TargetPageAttachmentId};

    fn owner_with_target() -> (
        BrowserNavigationOwner,
        moli_core::browser_host::BrowserPageOwnerKey,
        PageResidenceIdentity,
    ) {
        let key = moli_core::browser_host::BrowserPageOwnerKey::new("context-1", "target-1");
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
        let page = owner
            .capture_page_residence(key.browser_context_id(), key.target_id())
            .expect("test Page residence");
        (owner, key, page)
    }

    fn lifecycle_event(sequence: u64) -> RendererDocumentLifecycleEvent {
        let page_id = PageId::new_for_testing(71);
        RendererDocumentLifecycleEvent {
            frame: RendererFrameToken { page_id },
            document: RendererDocumentToken::new_for_testing(page_id, 3),
            epoch: RendererLifecycleEpoch(2),
            sequence,
            timestamp_micros: sequence * 10,
            kind: RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::Load,
            ),
        }
    }

    fn binding(event: &RendererDocumentLifecycleEvent) -> CommittedRendererDocumentBinding {
        CommittedRendererDocumentBinding {
            renderer_frame: event.frame,
            renderer_document: event.document,
            renderer_epoch: event.epoch,
            navigation: Some(DocumentNavigationToken::new("target-1", "loader-1")),
            frame_id: "target-1".to_owned(),
            loader_id: "loader-1".to_owned(),
            page_attachment_id: TargetPageAttachmentId::from_raw_for_test(1),
            document_open_replacement_epoch: None,
        }
    }

    fn projector_after_target_creation(
        owner: &BrowserNavigationOwner,
        page: &PageResidenceIdentity,
    ) -> (CdpBrowserFactProjector, BrowserFactSequence) {
        let subscriber = owner.subscribe_browser_facts();
        let mut projector = CdpBrowserFactProjector::new(subscriber);
        let created = projector
            .take_target_created_fact(page)
            .expect("test frontend should claim the exact Target occurrence");
        assert!(matches!(
            created.envelope().fact(),
            BrowserFact::TargetCreated
        ));
        assert_eq!(created.envelope().page_residence(), page);
        (projector, created.envelope().sequence())
    }

    #[test]
    fn target_creation_claim_survives_document_generation_but_rejects_id_reuse() {
        let (owner, key, page) = owner_with_target();
        let subscriber = owner.subscribe_browser_facts();
        let mut projector = CdpBrowserFactProjector::new(subscriber);
        let created = projector
            .take_target_created_fact(&page)
            .expect("exact Target creation should be projectable once");

        let handle = owner
            .page_residence_handle(&key)
            .expect("registered Target Page handle");
        handle.advance_generation_for_test_fixture();
        let current = owner
            .capture_page_residence(key.browser_context_id(), key.target_id())
            .expect("successor Document should remain in the same Page slot");
        assert!(created.matches_residence_instance(&current));

        let reused_public_id = BrowserPageResidenceHandle::default().identity(
            key.browser_context_id().to_owned(),
            Some(key.target_id().to_owned()),
        );
        assert!(!created.matches_residence_instance(&reused_public_id));
        assert_eq!(projector.pending_fact_count(), 0);
        assert!(matches!(
            projector.take_target_created_fact(&page),
            Err(BrowserFactProjectionError::MissingTargetCreationFact { .. })
        ));
    }

    #[test]
    fn exact_fact_waits_for_visibility_and_cannot_project_twice() {
        let (mut owner, _, page) = owner_with_target();
        let (mut projector, created_sequence) = projector_after_target_creation(&owner, &page);
        let event = lifecycle_event(1);
        let binding = binding(&event);

        let published = owner
            .record_document_lifecycle_facts(&page, &[event])
            .expect("exact lifecycle fact should publish");
        projector
            .register_document_lifecycle_causal_links(&page, &binding, &published)
            .expect("renderer ingress should freeze the projection binding");
        projector
            .capture_available()
            .expect("frontend cursor should capture the fact independently");
        assert_eq!(projector.pending_document_lifecycle_fact_count(), 1);
        assert!(
            projector
                .take_visible_document_lifecycle_facts(Some(&page), &binding, &[])
                .expect("an unreleased fact is not an error")
                .is_empty()
        );

        let projected = projector
            .take_visible_document_lifecycle_facts(Some(&page), &binding, &[event])
            .expect("the matching visibility record should release the fact");
        assert_eq!(projected.len(), 1);
        assert_eq!(
            projected[0].envelope().sequence().get(),
            created_sequence.get() + 1
        );
        assert_eq!(
            projector.last_projected_sequence().map(|value| value.get()),
            Some(created_sequence.get() + 1)
        );
        assert_eq!(projector.pending_document_lifecycle_fact_count(), 0);

        assert!(matches!(
            projector.take_visible_document_lifecycle_facts(Some(&page), &binding, &[event]),
            Err(BrowserFactProjectionError::MissingDocumentLifecycleFact { .. })
        ));
    }

    #[test]
    fn navigation_fact_families_share_one_exact_frontend_cursor() {
        let (mut owner, key, page) = owner_with_target();
        let (mut projector, _) = projector_after_target_creation(&owner, &page);

        let first = owner
            .try_start_document_navigation_with_trace(&key, "loader-first".to_owned(), None)
            .expect("test Context accepts navigation");
        let first_admission = projector
            .take_navigation_admission_fact(&first)
            .expect("accepted request should project from its Browser fact");
        assert!(matches!(
            first_admission.fact(),
            BrowserFact::NavigationAccepted {
                navigation,
                superseded_navigation: None,
            } if navigation == &first
        ));

        let replacement = owner
            .try_start_document_navigation_with_trace(&key, "loader-replacement".to_owned(), None)
            .expect("replacement navigation");
        let replacement_admission = projector
            .take_navigation_admission_fact(&replacement)
            .expect("successor admission should carry its superseded request");
        assert!(matches!(
            replacement_admission.fact(),
            BrowserFact::NavigationAccepted {
                navigation,
                superseded_navigation: Some(superseded),
            } if navigation == &replacement && superseded == &first
        ));
        assert_eq!(projector.pending_fact_count(), 0);

        let failure = BrowserNavigationFailure::Canceled {
            error_text: "canceled by test".to_owned(),
        };
        assert!(owner.fail_document_navigation_if_matches(&key, &replacement, failure.clone(),));
        projector
            .take_navigation_failure_fact(&replacement, &failure)
            .expect("terminal request should project from its exact failure fact");

        let download = owner
            .try_start_document_navigation_with_trace(&key, "loader-download".to_owned(), None)
            .expect("download navigation");
        projector
            .take_navigation_admission_fact(&download)
            .expect("download request admission");
        assert!(owner.convert_document_navigation_to_download_if_matches(&key, &download));
        projector
            .take_navigation_download_fact(&download)
            .expect("download terminal should project from its exact fact");

        let committed = owner
            .try_start_document_navigation_with_trace(&key, "loader-committed".to_owned(), None)
            .expect("committed navigation");
        projector
            .take_navigation_admission_fact(&committed)
            .expect("committed request admission");
        let permit = owner
            .prepare_loaded_page_replacement(&key, &committed)
            .expect("current request prepares replacement");
        let replacement = owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                permit,
                BrowserNavigationHistoryPageSnapshot::new(
                    "https://example.test/committed".to_owned(),
                    "committed".to_owned(),
                ),
            )
            .expect("exact Page replacement commits");
        let commit_projection = projector
            .take_navigation_commit_fact(&committed, replacement.current_page())
            .expect("renderer commit should require the atomic commit fact");
        let metadata_projection = projector
            .take_navigation_target_metadata_changed_fact(&committed, replacement.current_page())
            .expect("Target metadata should require the same committed navigation fact batch");
        assert_eq!(
            metadata_projection.envelope().sequence().get(),
            commit_projection.sequence().get() + 1
        );
        assert_eq!(metadata_projection.transition().navigation(), &committed);
        assert_eq!(
            metadata_projection.transition().url(),
            "https://example.test/committed"
        );
        assert_eq!(metadata_projection.transition().title(), "committed");
        assert_eq!(projector.pending_fact_count(), 0);
    }

    #[test]
    fn page_replacement_retires_unprojected_predecessor_lifecycle_facts() {
        let (mut owner, key, previous_page) = owner_with_target();
        let (mut projector, _) = projector_after_target_creation(&owner, &previous_page);
        let lifecycle = lifecycle_event(1);
        let lifecycle_binding = binding(&lifecycle);

        let published = owner
            .record_document_lifecycle_facts(&previous_page, &[lifecycle])
            .expect("predecessor lifecycle should publish");
        projector
            .register_document_lifecycle_causal_links(
                &previous_page,
                &lifecycle_binding,
                &published,
            )
            .expect("predecessor projection binding should freeze");
        projector
            .capture_available()
            .expect("frontend cursor should retain the unreleased lifecycle fact");
        assert_eq!(projector.pending_document_lifecycle_fact_count(), 1);

        let navigation = owner
            .try_start_document_navigation_with_trace(&key, "loader-successor".to_owned(), None)
            .expect("successor navigation should start");
        projector
            .take_navigation_admission_fact(&navigation)
            .expect("successor admission should project");
        let permit = owner
            .prepare_loaded_page_replacement(&key, &navigation)
            .expect("current navigation should prepare replacement");
        let replacement = owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                permit,
                BrowserNavigationHistoryPageSnapshot::new(
                    "https://example.test/successor".to_owned(),
                    "successor".to_owned(),
                ),
            )
            .expect("successor Page should commit");
        projector
            .take_navigation_commit_fact(&navigation, replacement.current_page())
            .expect("successor commit should project from its exact fact");
        projector
            .take_navigation_target_metadata_changed_fact(&navigation, replacement.current_page())
            .expect("successor metadata should project from the same commit batch");

        assert_eq!(projector.pending_document_lifecycle_fact_count(), 0);
        assert_eq!(projector.pending_fact_count(), 0);
    }

    #[test]
    fn page_replacement_retires_unprojected_predecessor_metadata_fact() {
        let (mut owner, key, previous_page) = owner_with_target();
        let (mut projector, _) = projector_after_target_creation(&owner, &previous_page);

        let first = owner
            .try_start_document_navigation_with_trace(&key, "loader-first".to_owned(), None)
            .expect("first navigation should start");
        projector
            .take_navigation_admission_fact(&first)
            .expect("first navigation admission should project");
        let first_permit = owner
            .prepare_loaded_page_replacement(&key, &first)
            .expect("first navigation should prepare replacement");
        let first_replacement = owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                first_permit,
                BrowserNavigationHistoryPageSnapshot::new("https://example.test/first", "first"),
            )
            .expect("first Page should commit");
        projector
            .take_navigation_commit_fact(&first, first_replacement.current_page())
            .expect("first commit fact should project");
        assert_eq!(projector.pending_fact_count(), 1);

        let second = owner
            .try_start_document_navigation_with_trace(&key, "loader-second".to_owned(), None)
            .expect("second navigation should start");
        projector
            .take_navigation_admission_fact(&second)
            .expect("second navigation admission should project");
        let second_permit = owner
            .prepare_loaded_page_replacement(&key, &second)
            .expect("second navigation should prepare replacement");
        let second_replacement = owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                second_permit,
                BrowserNavigationHistoryPageSnapshot::new("https://example.test/second", "second"),
            )
            .expect("second Page should commit");
        projector
            .take_navigation_commit_fact(&second, second_replacement.current_page())
            .expect("second commit fact should project");
        let second_metadata = projector
            .take_navigation_target_metadata_changed_fact(
                &second,
                second_replacement.current_page(),
            )
            .expect("second metadata should remain claimable");

        assert_eq!(second_metadata.transition().navigation(), &second);
        assert_eq!(projector.pending_fact_count(), 0);
    }

    #[test]
    fn page_replacement_retires_unclaimed_creation_across_document_generation() {
        let (mut owner, key, _) = owner_with_target();
        let subscriber = owner.subscribe_browser_facts();
        let mut projector = CdpBrowserFactProjector::new(subscriber);
        let handle = owner
            .page_residence_handle(&key)
            .expect("registered Target Page handle");
        handle.advance_generation_for_test_fixture();

        let navigation = owner
            .try_start_document_navigation_with_trace(&key, "loader-successor".to_owned(), None)
            .expect("successor navigation should start");
        projector
            .take_navigation_admission_fact(&navigation)
            .expect("navigation admission should share the creation cursor");
        assert_eq!(
            projector.pending_fact_count(),
            1,
            "the unclaimed generation-zero Target occurrence should remain pending"
        );

        let permit = owner
            .prepare_loaded_page_replacement(&key, &navigation)
            .expect("current navigation should prepare replacement");
        assert_eq!(permit.previous_page().loaded_page_generation(), 1);
        let replacement = owner
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                permit,
                BrowserNavigationHistoryPageSnapshot::new(
                    "https://example.test/successor".to_owned(),
                    "successor".to_owned(),
                ),
            )
            .expect("successor Page should commit");
        projector
            .take_navigation_commit_fact(&navigation, replacement.current_page())
            .expect("successor commit should project its exact fact");
        projector
            .take_navigation_target_metadata_changed_fact(&navigation, replacement.current_page())
            .expect("successor metadata should project from the same commit batch");

        assert_eq!(projector.pending_fact_count(), 0);
    }

    #[test]
    fn target_terminal_carries_its_pending_navigation_on_the_common_cursor() {
        let (mut owner, key, page) = owner_with_target();
        let (mut projector, _) = projector_after_target_creation(&owner, &page);
        let navigation = owner
            .try_start_document_navigation_with_trace(&key, "loader-closing".to_owned(), None)
            .expect("closing Target should accept a pending navigation");
        projector
            .take_navigation_admission_fact(&navigation)
            .expect("pending navigation admission should project");
        let request = owner
            .capture_target_termination(&key, BrowserTargetTerminationKind::Close)
            .expect("live Target should capture close");
        let permit = owner
            .prepare_target_termination(request)
            .expect("exact Target close should prepare");
        let termination = owner
            .commit_target_termination(permit)
            .expect("exact Target close should commit");

        let projected = projector
            .take_target_termination_fact(&termination)
            .expect("Target close should consume its exact terminal fact batch");

        assert!(matches!(
            projected.envelope().fact(),
            BrowserFact::TargetClosed {
                pending_navigation: Some(pending),
                ..
            } if pending == &navigation
        ));
        assert_eq!(projector.pending_fact_count(), 0);
    }

    #[tokio::test]
    async fn application_wake_advances_the_same_frontend_cursor_through_its_committed_tail() {
        let (mut owner, _, page) = owner_with_target();
        let mut wake = owner.subscribe_browser_fact_wake();
        let (mut projector, created_sequence) = projector_after_target_creation(&owner, &page);
        let event = lifecycle_event(1);
        let binding = binding(&event);

        let published = owner
            .record_document_lifecycle_facts(&page, &[event])
            .expect("exact lifecycle fact should publish");
        projector
            .register_document_lifecycle_causal_links(&page, &binding, &published)
            .expect("renderer ingress should freeze the projection binding");
        assert_eq!(
            projector.pending_document_lifecycle_fact_count(),
            0,
            "registering the causal link must not consume the frontend cursor"
        );
        assert_eq!(projector.last_observed_sequence, Some(created_sequence));
        let through = wake
            .recv()
            .await
            .expect("fact publication should wake the application scheduler");
        projector
            .capture_wake(through)
            .expect("the frontend cursor should reach the coalesced wake sequence");

        assert_eq!(through.get(), created_sequence.get() + 1);
        assert_eq!(projector.pending_document_lifecycle_fact_count(), 1);
        let projected = projector
            .take_visible_document_lifecycle_facts(Some(&page), &binding, &[event])
            .expect("a wake-captured fact should retain its causal projection binding");
        assert_eq!(projected.len(), 1);
    }

    #[test]
    fn attachment_replacement_cannot_claim_an_old_lifecycle_fact() {
        let (mut owner, _, page) = owner_with_target();
        let (mut projector, _) = projector_after_target_creation(&owner, &page);
        let event = lifecycle_event(1);
        let old_binding = binding(&event);

        let published = owner
            .record_document_lifecycle_facts(&page, &[event])
            .expect("exact lifecycle fact should publish");
        projector
            .register_document_lifecycle_causal_links(&page, &old_binding, &published)
            .expect("old attachment should freeze the fact mapping");
        projector
            .capture_available()
            .expect("frontend cursor should advance independently of the visible record");
        let mut replacement_binding = old_binding.clone();
        replacement_binding.page_attachment_id = TargetPageAttachmentId::from_raw_for_test(2);

        assert!(matches!(
            projector.take_visible_document_lifecycle_facts(
                Some(&page),
                &replacement_binding,
                &[event],
            ),
            Err(BrowserFactProjectionError::MissingDocumentLifecycleFact { .. })
        ));
    }

    #[test]
    fn completed_epoch_uses_the_same_frozen_document_binding() {
        let (mut owner, _, page) = owner_with_target();
        let (mut projector, _) = projector_after_target_creation(&owner, &page);
        let event = lifecycle_event(1);
        let mut active_epoch_binding = binding(&event);
        active_epoch_binding.renderer_epoch = RendererLifecycleEpoch(event.epoch.0 + 1);

        let published = owner
            .record_document_lifecycle_facts(&page, &[event])
            .expect("completed lifecycle epoch should publish");
        projector
            .register_document_lifecycle_causal_links(&page, &active_epoch_binding, &published)
            .expect("the root Document binding should retain its completed epoch fact");
        projector
            .capture_available()
            .expect("frontend cursor should capture the completed epoch fact");
        let projected = projector
            .take_visible_document_lifecycle_facts(Some(&page), &active_epoch_binding, &[event])
            .expect("the completed epoch should remain projectable");

        assert_eq!(projected.len(), 1);
        assert_eq!(
            projected[0]
                .reached()
                .map(|(document, _, _)| document.epoch),
            Some(event.epoch)
        );
        assert_eq!(projected[0].binding(), &active_epoch_binding);
    }

    #[test]
    fn later_visible_load_retires_an_unselected_earlier_dcl_fact() {
        let (mut owner, _, page) = owner_with_target();
        let (mut projector, created_sequence) = projector_after_target_creation(&owner, &page);
        let load = lifecycle_event(2);
        let dcl = RendererDocumentLifecycleEvent {
            sequence: 1,
            timestamp_micros: 10,
            kind: RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
            ..load
        };
        let binding = binding(&load);

        let published = owner
            .record_document_lifecycle_facts(&page, &[dcl, load])
            .expect("ordered lifecycle facts should publish");
        projector
            .register_document_lifecycle_causal_links(&page, &binding, &published)
            .expect("renderer ingress should freeze both lifecycle links");
        projector
            .capture_available()
            .expect("frontend cursor should capture both facts independently");

        let projected = projector
            .take_visible_document_lifecycle_facts(Some(&page), &binding, &[load])
            .expect("a frontend may first observe the exact load fact");
        assert_eq!(projected.len(), 1);
        assert_eq!(
            projected[0].envelope().sequence().get(),
            created_sequence.get() + 2
        );
        assert_eq!(projector.pending_document_lifecycle_fact_count(), 0);
        assert!(matches!(
            projector.take_visible_document_lifecycle_facts(Some(&page), &binding, &[dcl]),
            Err(BrowserFactProjectionError::MissingDocumentLifecycleFact { .. })
        ));
    }

    #[test]
    fn slow_frontend_reports_journal_lag_instead_of_guessing_events() {
        let (mut owner, _, page) = owner_with_target();
        let (mut projector, _) = projector_after_target_creation(&owner, &page);
        let binding = binding(&lifecycle_event(1));
        for sequence in 1..=1_025 {
            let published = owner
                .record_document_lifecycle_facts(&page, &[lifecycle_event(sequence)])
                .expect("test fact sequence should remain available");
            projector
                .register_document_lifecycle_causal_links(&page, &binding, &published)
                .expect("causal-link registration must not backpressure Browser publication");
        }
        assert_eq!(
            projector.lifecycle_causal_links.len(),
            MAX_PENDING_BROWSER_FACTS,
            "the causal join must stay bounded with the Browser retention window"
        );

        assert_eq!(
            projector.capture_available(),
            Err(BrowserFactProjectionError::SubscriberLagged { skipped: 1 })
        );
        assert_eq!(
            projector.terminal_error(),
            Some(&BrowserFactProjectionError::SubscriberLagged { skipped: 1 })
        );
        assert_eq!(projector.pending_document_lifecycle_fact_count(), 0);
        assert!(projector.lifecycle_causal_links.is_empty());
    }
}
