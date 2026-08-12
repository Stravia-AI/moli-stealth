//! Execution-produced result for one selected parser-owned module action.
//!
//! The queued main-Document runtime source carries only a stable continuation
//! ticket. After exact-owner authorization, the parser/module owner produces
//! the types in this module. Keeping them outside the generic source wiring
//! prevents the carrier from growing into a second module-completion owner.

use super::RendererPageMainDocumentRuntimeOwner;

/// Observable body activity from one already-selected parser-owned module
/// continuation.
///
/// A late TLA fulfillment can apply the exact task without entering page code
/// in that continuation. Graph evaluation, a script-element terminal, or a
/// Window error body can enter page code and therefore requires child-record
/// reconciliation after the task-end checkpoint. This fact is produced only
/// after execution and is never stored in the scheduler source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageParserOwnedModuleContinuationBodyActivity {
    NoPageCodeOrEventDispatch,
    PageCodeOrEventDispatch,
}

/// Exact-target result of one selected parser-owned module continuation.
///
/// `AppliedToSelectedOwner` retains task-end authority even when page code
/// replaces the Document during the body. `CurrentOwnerReservationSpent`
/// means the stable-source ticket no longer matched concrete ready work and
/// therefore must not manufacture a checkpoint. `DiscardedStaleOwner` rejects
/// the task before entering the replacement realm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageParserOwnedModuleContinuationTargetEffect {
    AppliedToSelectedOwner(PageParserOwnedModuleContinuationBodyActivity),
    CurrentOwnerReservationSpent,
    DiscardedStaleOwner,
}

/// Post-execution action reserved for `ContinueParserOwnedModule`.
///
/// Keeping this separate from the generic runtime action result prevents the
/// selected dispatcher from inferring checkpoint behavior from an action kind
/// joined with a broad `made_progress` boolean.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageParserOwnedModuleContinuationTurnAction {
    owner: RendererPageMainDocumentRuntimeOwner,
    target_effect: PageParserOwnedModuleContinuationTargetEffect,
}

impl PageParserOwnedModuleContinuationTurnAction {
    pub(super) const fn new(
        owner: RendererPageMainDocumentRuntimeOwner,
        target_effect: PageParserOwnedModuleContinuationTargetEffect,
    ) -> Self {
        Self {
            owner,
            target_effect,
        }
    }

    pub(crate) const fn owner(self) -> RendererPageMainDocumentRuntimeOwner {
        self.owner
    }

    pub(crate) const fn target_effect(self) -> PageParserOwnedModuleContinuationTargetEffect {
        self.target_effect
    }
}
