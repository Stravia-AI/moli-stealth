//! Host contract for deciding a pending Page's exact lifecycle target.
//!
//! This module contains only the data and one-shot callback crossing the
//! renderer boundary. Snapshot capture, callback isolation, scheduler gates,
//! and successor navigation stay in `owner::lifecycle_decision`.

use super::document_lifecycle::RendererDocumentLifecycleIdentity;
use crate::PageVmInitStage;
use url::Url;

/// Immutable facts for the exact Document that reached a requested
/// page-creation lifecycle target.
///
/// This snapshot is intentionally narrow. In particular, producing it does
/// not capture the whole Page VM state or advance lifecycle execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RendererLifecycleSnapshot {
    pub stage: PageVmInitStage,
    pub document: RendererDocumentLifecycleIdentity,
    pub requested_url: Url,
    pub final_url: Url,
    pub status: u16,
}

/// One-shot decision made synchronously when page creation reaches its exact
/// lifecycle target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RendererLifecycleDecision {
    /// Accept the Page at the lifecycle target and let page creation reply.
    Finish,
    /// Observe one cross-Document navigation within the supplied grace
    /// period, then follow its successor to the same lifecycle target before
    /// page creation replies.
    FollowNextDocument { navigation_grace_ms: u64 },
}

/// Synchronous, one-shot policy decider for a page-creation lifecycle target.
///
/// The renderer invokes this decider in the owner turn that observes the target,
/// after lifecycle output has been settled and before another Page turn can
/// run. The callback must only inspect the supplied snapshot and return
/// promptly. It must not block, await, or call back into this renderer owner;
/// doing so would stall or deadlock the owner lane.
pub struct RendererLifecycleDecider {
    callback: Box<
        dyn FnOnce(RendererLifecycleSnapshot) -> anyhow::Result<RendererLifecycleDecision>
            + Send
            + 'static,
    >,
}

impl RendererLifecycleDecider {
    pub fn new<F>(callback: F) -> Self
    where
        F: FnOnce(RendererLifecycleSnapshot) -> anyhow::Result<RendererLifecycleDecision>
            + Send
            + 'static,
    {
        Self {
            callback: Box::new(callback),
        }
    }

    pub(super) fn decide(
        self,
        snapshot: RendererLifecycleSnapshot,
    ) -> anyhow::Result<RendererLifecycleDecision> {
        (self.callback)(snapshot)
    }
}

impl std::fmt::Debug for RendererLifecycleDecider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RendererLifecycleDecider")
            .finish_non_exhaustive()
    }
}

pub(super) struct PendingLifecycleDecision {
    target_stage: PageVmInitStage,
    decider: RendererLifecycleDecider,
}

impl PendingLifecycleDecision {
    pub(super) fn new(target_stage: PageVmInitStage, decider: RendererLifecycleDecider) -> Self {
        Self {
            target_stage,
            decider,
        }
    }

    pub(super) fn into_parts(self) -> (PageVmInitStage, RendererLifecycleDecider) {
        (self.target_stage, self.decider)
    }
}
