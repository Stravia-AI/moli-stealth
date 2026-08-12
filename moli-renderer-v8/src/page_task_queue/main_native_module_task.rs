//! Execution-produced results for main-Document native-module tasks.
//!
//! `ContinueDynamicModuleJob` and `ContinueNativeModuleOwnerEvent` share the
//! native module-map/dynamic-import implementation, but they remain distinct
//! scheduler actions.  The types in this module exist only after one of those
//! exact actions has been authorized and attempted; they are never queued and
//! cannot be used as scheduler policy.

use super::RendererPageMainDocumentRuntimeOwner;

/// Page-realm activity performed by one selected native-module body.
///
/// Module-map updates, fetch scheduling, and transfer into another typed
/// continuation are state-only.  Module evaluation, dynamic-import Promise
/// settlement, and a modulepreload link event enter the Page realm.  Both are
/// real current tasks and owe an ordinary checkpoint; keeping the distinction
/// makes the execution fact useful without granting either body generic
/// callback/runtime-drain authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageMainNativeModuleBodyActivity {
    StateTransitionOnly,
    PageRealmBodyAttempted,
}

/// Exact-target result of one selected main native-module task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageMainNativeModuleTargetEffect {
    /// A concrete action was consumed for the selected owner.  This remains
    /// applied even if Page code replaces the Document during the body.
    AppliedToSelectedOwner(PageMainNativeModuleBodyActivity),
    /// The stable ticket was consumed after another path had already removed
    /// its concrete owner work.  It must not manufacture a checkpoint.
    CurrentOwnerReservationSpent,
    /// The root Document/runtime generation was stale before body entry.
    DiscardedStaleOwner,
}

/// Body settlement retained until the selected dispatcher has submitted the
/// task-end checkpoint.
///
/// JavaScript side effects are not rolled back when a later native-module
/// operation reports an error.  Keeping the error beside the execution fact
/// prevents `?` from skipping the task-end boundary after Page realm entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PageMainNativeModuleSettlement {
    Completed,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PageMainNativeModuleTurnResult {
    owner: RendererPageMainDocumentRuntimeOwner,
    target_effect: PageMainNativeModuleTargetEffect,
    settlement: PageMainNativeModuleSettlement,
}

impl PageMainNativeModuleTurnResult {
    const fn new(
        owner: RendererPageMainDocumentRuntimeOwner,
        target_effect: PageMainNativeModuleTargetEffect,
        settlement: PageMainNativeModuleSettlement,
    ) -> Self {
        Self {
            owner,
            target_effect,
            settlement,
        }
    }

    #[cfg(test)]
    const fn owner(&self) -> RendererPageMainDocumentRuntimeOwner {
        self.owner
    }

    #[cfg(test)]
    const fn target_effect(&self) -> PageMainNativeModuleTargetEffect {
        self.target_effect
    }

    fn into_parts(
        self,
    ) -> (
        PageMainNativeModuleTargetEffect,
        PageMainNativeModuleSettlement,
    ) {
        (self.target_effect, self.settlement)
    }
}

/// Post-execution action reserved for `ContinueDynamicModuleJob`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PageDynamicModuleJobTurnAction(PageMainNativeModuleTurnResult);

impl PageDynamicModuleJobTurnAction {
    pub(super) const fn new(
        owner: RendererPageMainDocumentRuntimeOwner,
        target_effect: PageMainNativeModuleTargetEffect,
        settlement: PageMainNativeModuleSettlement,
    ) -> Self {
        Self(PageMainNativeModuleTurnResult::new(
            owner,
            target_effect,
            settlement,
        ))
    }

    #[cfg(test)]
    pub(crate) const fn owner(&self) -> RendererPageMainDocumentRuntimeOwner {
        self.0.owner()
    }

    #[cfg(test)]
    pub(crate) const fn target_effect(&self) -> PageMainNativeModuleTargetEffect {
        self.0.target_effect()
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        PageMainNativeModuleTargetEffect,
        PageMainNativeModuleSettlement,
    ) {
        self.0.into_parts()
    }
}

/// Post-execution action reserved for `ContinueNativeModuleOwnerEvent`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PageNativeModuleOwnerEventTurnAction(PageMainNativeModuleTurnResult);

impl PageNativeModuleOwnerEventTurnAction {
    pub(super) const fn new(
        owner: RendererPageMainDocumentRuntimeOwner,
        target_effect: PageMainNativeModuleTargetEffect,
        settlement: PageMainNativeModuleSettlement,
    ) -> Self {
        Self(PageMainNativeModuleTurnResult::new(
            owner,
            target_effect,
            settlement,
        ))
    }

    #[cfg(test)]
    pub(crate) const fn owner(&self) -> RendererPageMainDocumentRuntimeOwner {
        self.0.owner()
    }

    #[cfg(test)]
    pub(crate) const fn target_effect(&self) -> PageMainNativeModuleTargetEffect {
        self.0.target_effect()
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        PageMainNativeModuleTargetEffect,
        PageMainNativeModuleSettlement,
    ) {
        self.0.into_parts()
    }
}
