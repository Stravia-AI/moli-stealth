use moli_owner_queue::OwnerTaskReadySignal;

use super::{RendererOwnerWakeSender, RendererOwnerWakeSource};

/// Shared adapter from an owner-ready source edge to the Page owner wake lane.
///
/// Most Page task sources differ only in which typed admission hint they send.
/// Keeping that choice as data avoids defining one signal type and trait
/// implementation for every source while preserving the source-specific wake.
#[derive(Clone, Debug)]
pub(super) struct RendererPageTaskReadySignal {
    owner_wake: RendererOwnerWakeSender,
    source: RendererOwnerWakeSource,
}

impl RendererPageTaskReadySignal {
    pub(super) fn new(
        owner_wake: RendererOwnerWakeSender,
        source: RendererOwnerWakeSource,
    ) -> Self {
        Self { owner_wake, source }
    }
}

impl OwnerTaskReadySignal for RendererPageTaskReadySignal {
    fn signal_ready(&self) {
        self.owner_wake.signal_source(self.source);
    }
}
