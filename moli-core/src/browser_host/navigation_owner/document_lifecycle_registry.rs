use std::collections::HashMap;

use crate::{
    browser_host::{BrowserDocumentLifecycleWaitOutcome, PageResidenceIdentity},
    page::{
        RendererDocumentLifecycleEvent, RendererDocumentLifecycleEventKind,
        RendererDocumentLifecycleIdentity, RendererDocumentLifecycleMilestone,
        RendererLifecycleTerminationStamp,
    },
};

use super::BrowserPageOwnerKey;

/// Small Browser-owned current-state index used to bootstrap lifecycle fact
/// waits after the bounded journal has evicted an older milestone occurrence.
///
/// The journal remains the occurrence stream. This registry retains only one
/// exact Page/Document record per live top-level Target; it does not retain
/// protocol events, subscribers, physical Pages, or renderer callbacks.
#[derive(Default)]
pub(super) struct BrowserDocumentLifecycleRegistry {
    records: HashMap<BrowserPageOwnerKey, BrowserDocumentLifecycleRecord>,
}

struct BrowserDocumentLifecycleRecord {
    page: PageResidenceIdentity,
    document: RendererDocumentLifecycleIdentity,
    last_reached: Option<RendererDocumentLifecycleMilestone>,
    termination: Option<RendererLifecycleTerminationStamp>,
}

impl BrowserDocumentLifecycleRegistry {
    pub(super) fn record(
        &mut self,
        owner: &BrowserPageOwnerKey,
        page: &PageResidenceIdentity,
        events: &[RendererDocumentLifecycleEvent],
    ) {
        for event in events {
            let document = RendererDocumentLifecycleIdentity {
                frame: event.frame,
                document: event.document,
                epoch: event.epoch,
            };
            let record = self.records.entry(owner.clone()).or_insert_with(|| {
                BrowserDocumentLifecycleRecord {
                    page: page.clone(),
                    document,
                    last_reached: None,
                    termination: None,
                }
            });
            if record.page != *page || record.document != document {
                *record = BrowserDocumentLifecycleRecord {
                    page: page.clone(),
                    document,
                    last_reached: None,
                    termination: None,
                };
            }
            match event.kind {
                RendererDocumentLifecycleEventKind::Started { .. } => {}
                RendererDocumentLifecycleEventKind::Milestone(milestone) => {
                    record.last_reached = furthest_milestone(record.last_reached, Some(milestone));
                }
                RendererDocumentLifecycleEventKind::Terminated {
                    last_reached,
                    reason,
                } => {
                    record.last_reached = furthest_milestone(record.last_reached, last_reached);
                    record.termination = Some(RendererLifecycleTerminationStamp {
                        sequence: event.sequence,
                        timestamp_micros: event.timestamp_micros,
                        reason,
                    });
                }
            }
        }
    }

    pub(super) fn outcome(
        &self,
        owner: &BrowserPageOwnerKey,
        page: &PageResidenceIdentity,
        document: RendererDocumentLifecycleIdentity,
        milestone: RendererDocumentLifecycleMilestone,
    ) -> Option<BrowserDocumentLifecycleWaitOutcome> {
        let record = self.records.get(owner)?;
        if record.page != *page || record.document != document {
            return None;
        }
        if record
            .last_reached
            .is_some_and(|reached| milestone_satisfies(reached, milestone))
        {
            return Some(BrowserDocumentLifecycleWaitOutcome::Reached);
        }
        record.termination.map(
            |termination| BrowserDocumentLifecycleWaitOutcome::Interrupted {
                last_reached: record.last_reached,
                termination,
            },
        )
    }

    pub(super) fn retire_page(
        &mut self,
        owner: &BrowserPageOwnerKey,
        page: &PageResidenceIdentity,
    ) {
        if self
            .records
            .get(owner)
            .is_some_and(|record| record.page == *page)
        {
            self.records.remove(owner);
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

fn furthest_milestone(
    first: Option<RendererDocumentLifecycleMilestone>,
    second: Option<RendererDocumentLifecycleMilestone>,
) -> Option<RendererDocumentLifecycleMilestone> {
    match (first, second) {
        (Some(RendererDocumentLifecycleMilestone::Load), _)
        | (_, Some(RendererDocumentLifecycleMilestone::Load)) => {
            Some(RendererDocumentLifecycleMilestone::Load)
        }
        (Some(RendererDocumentLifecycleMilestone::DomContentLoaded), _)
        | (_, Some(RendererDocumentLifecycleMilestone::DomContentLoaded)) => {
            Some(RendererDocumentLifecycleMilestone::DomContentLoaded)
        }
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PageId,
        page::{
            RendererDocumentTerminationReason, RendererDocumentToken, RendererFrameToken,
            RendererLifecycleEpoch, RendererLifecycleStartReason,
        },
    };

    fn page(generation: u64) -> PageResidenceIdentity {
        PageResidenceIdentity::new(
            "context-1".to_owned(),
            Some("target-1".to_owned()),
            generation,
        )
    }

    fn event(
        generation: u64,
        sequence: u64,
        kind: RendererDocumentLifecycleEventKind,
    ) -> RendererDocumentLifecycleEvent {
        let page_id = PageId::new_for_testing(91);
        RendererDocumentLifecycleEvent {
            frame: RendererFrameToken { page_id },
            document: RendererDocumentToken::new_for_testing(page_id, generation),
            epoch: RendererLifecycleEpoch(generation),
            sequence,
            timestamp_micros: sequence * 10,
            kind,
        }
    }

    fn identity(event: RendererDocumentLifecycleEvent) -> RendererDocumentLifecycleIdentity {
        RendererDocumentLifecycleIdentity {
            frame: event.frame,
            document: event.document,
            epoch: event.epoch,
        }
    }

    #[test]
    fn current_snapshot_bootstraps_reached_and_interrupted_waits() {
        let owner = BrowserPageOwnerKey::new("context-1", "target-1");
        let page = page(2);
        let started = event(
            2,
            1,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::CrossDocumentCommit,
            },
        );
        let dcl = event(
            2,
            2,
            RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        );
        let terminated = event(
            2,
            3,
            RendererDocumentLifecycleEventKind::Terminated {
                last_reached: Some(RendererDocumentLifecycleMilestone::DomContentLoaded),
                reason: RendererDocumentTerminationReason::Stopped,
            },
        );
        let mut registry = BrowserDocumentLifecycleRegistry::default();
        registry.record(&owner, &page, &[started, dcl, terminated]);

        assert_eq!(
            registry.outcome(
                &owner,
                &page,
                identity(started),
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
            Some(BrowserDocumentLifecycleWaitOutcome::Reached)
        );
        assert!(matches!(
            registry.outcome(
                &owner,
                &page,
                identity(started),
                RendererDocumentLifecycleMilestone::Load,
            ),
            Some(BrowserDocumentLifecycleWaitOutcome::Interrupted {
                last_reached: Some(RendererDocumentLifecycleMilestone::DomContentLoaded),
                ..
            })
        ));
        registry.retire_page(&owner, &page);
        assert_eq!(
            registry.outcome(
                &owner,
                &page,
                identity(started),
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
            None,
            "a Page terminal must retire its current-state bootstrap"
        );
    }

    #[test]
    fn successor_document_replaces_only_the_same_targets_snapshot() {
        let owner = BrowserPageOwnerKey::new("context-1", "target-1");
        let page = page(2);
        let first = event(
            1,
            1,
            RendererDocumentLifecycleEventKind::Milestone(RendererDocumentLifecycleMilestone::Load),
        );
        let successor = event(
            2,
            2,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::ExplicitDocumentOpen,
            },
        );
        let mut registry = BrowserDocumentLifecycleRegistry::default();
        registry.record(&owner, &page, &[first, successor]);

        assert_eq!(
            registry.outcome(
                &owner,
                &page,
                identity(first),
                RendererDocumentLifecycleMilestone::Load,
            ),
            None
        );
        assert_eq!(
            registry.outcome(
                &owner,
                &page,
                identity(successor),
                RendererDocumentLifecycleMilestone::Load,
            ),
            None
        );
    }

    #[test]
    fn later_records_cannot_regress_a_reached_load_snapshot() {
        let owner = BrowserPageOwnerKey::new("context-1", "target-1");
        let page = page(2);
        let load = event(
            2,
            3,
            RendererDocumentLifecycleEventKind::Milestone(RendererDocumentLifecycleMilestone::Load),
        );
        let delayed_dcl = event(
            2,
            4,
            RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        );
        let mut registry = BrowserDocumentLifecycleRegistry::default();
        registry.record(&owner, &page, &[load, delayed_dcl]);

        assert_eq!(
            registry.outcome(
                &owner,
                &page,
                identity(load),
                RendererDocumentLifecycleMilestone::Load,
            ),
            Some(BrowserDocumentLifecycleWaitOutcome::Reached)
        );
    }
}
