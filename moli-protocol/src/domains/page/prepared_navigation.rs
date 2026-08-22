use moli_core::RendererRuntimeCommandCausalIdentity;
use moli_core::page::{
    RendererDocumentLifecycleIdentity, RendererDocumentSourcedSameDocumentNavigation,
    RendererDocumentSourcedTopLevelLocationNavigation, RendererPendingSameDocumentNavigation,
    RendererPendingTopLevelHistoryTraversal,
};

use crate::conn::TargetPageResidenceIdentity;

/// A same-Document protocol handoff bound to the exact target-local Page
/// residence from which it was captured.
///
/// `source_document` inside `navigation` is causal metadata. The Page
/// residence is the apply authority because `document.open()` replaces the
/// Document without undoing an already-applied history mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PagePreparedSameDocumentNavigation {
    owner: TargetPageResidenceIdentity,
    navigation: RendererDocumentSourcedSameDocumentNavigation,
}

impl PagePreparedSameDocumentNavigation {
    pub(super) fn new(
        owner: TargetPageResidenceIdentity,
        navigation: RendererDocumentSourcedSameDocumentNavigation,
    ) -> Self {
        Self { owner, navigation }
    }

    pub(super) fn owner(&self) -> &TargetPageResidenceIdentity {
        &self.owner
    }

    pub(super) fn source_document(&self) -> RendererDocumentLifecycleIdentity {
        self.navigation.source_document()
    }

    pub(super) fn into_navigation(self) -> RendererPendingSameDocumentNavigation {
        self.navigation.into_navigation()
    }
}

/// A renderer-requested top-level navigation bound to the exact target-local
/// Page residence that produced the prepared action.
///
/// Keeping the source Document and Page residence distinct is intentional: the
/// request survives `document.open()` in the same Page, but must not navigate a
/// target after that Page has been retired or replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PagePreparedTopLevelLocationNavigation {
    owner: TargetPageResidenceIdentity,
    navigation: RendererDocumentSourcedTopLevelLocationNavigation,
}

impl PagePreparedTopLevelLocationNavigation {
    pub(super) fn new(
        owner: TargetPageResidenceIdentity,
        navigation: RendererDocumentSourcedTopLevelLocationNavigation,
    ) -> Self {
        Self { owner, navigation }
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        TargetPageResidenceIdentity,
        RendererDocumentSourcedTopLevelLocationNavigation,
    ) {
        (self.owner, self.navigation)
    }

    pub(super) fn runtime_command_cause(&self) -> Option<&RendererRuntimeCommandCausalIdentity> {
        self.navigation.runtime_command_cause()
    }
}

/// A renderer-requested top-level history traversal bound to the exact Page
/// residence that published it.
///
/// The renderer payload intentionally contains only a relative delta. Browser
/// Owner resolves the history entry and URL after this Page wins its mailbox
/// turn, so delayed output cannot follow a replacement Page or mutable
/// frontend route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PagePreparedTopLevelHistoryTraversal {
    owner: TargetPageResidenceIdentity,
    traversal: RendererPendingTopLevelHistoryTraversal,
}

impl PagePreparedTopLevelHistoryTraversal {
    pub(super) fn new(
        owner: TargetPageResidenceIdentity,
        traversal: RendererPendingTopLevelHistoryTraversal,
    ) -> Self {
        Self { owner, traversal }
    }

    pub(super) fn into_parts(self) -> (TargetPageResidenceIdentity, i64) {
        (self.owner, self.traversal.delta)
    }
}
