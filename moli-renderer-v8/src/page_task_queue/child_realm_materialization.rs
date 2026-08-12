use crate::{
    document_runtime::DomHandle, frame_owner_model::FrameDocumentTaskOwner,
    runtime::PageOwnerTurnOutcome,
};

use super::RendererPageChildFrameTaskOwner;

/// Exact child Document that requested its first default-world realm turn.
///
/// The reserved realm id stays in the authoritative child-owner record:
/// materialization is the action that makes that realm executable. The child
/// Document owner is exact and must never be rebound to a replacement
/// Document using the same iframe handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RendererPageChildRealmMaterializationTarget {
    child_handle: DomHandle,
    document_owner: FrameDocumentTaskOwner,
}

impl RendererPageChildRealmMaterializationTarget {
    pub(crate) const fn new(
        child_handle: DomHandle,
        document_owner: FrameDocumentTaskOwner,
    ) -> Self {
        Self {
            child_handle,
            document_owner,
        }
    }

    pub(crate) const fn child_handle(self) -> DomHandle {
        self.child_handle
    }

    pub(crate) const fn document_owner(self) -> FrameDocumentTaskOwner {
        self.document_owner
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageChildRealmMaterializationTargetEffect {
    /// The exact realm was established without entering stored script code.
    MaterializedCurrentOwnerWithoutDocumentStartScript,
    /// The exact realm was established and at least one stored document-start
    /// script entered V8. Reactions remain pending for the outer task end.
    MaterializedCurrentOwnerAfterDocumentStartScript,
    /// The exact current request failed before a usable realm was established.
    FailedCurrentOwner,
    /// Reentrant work consumed the request while the same exact owner remained
    /// current. This is a consumed current task, not stale work.
    CurrentOwnerHadNoPendingRequest,
    /// The queued exact owner no longer names the current child Document.
    IgnoredStaleOwner {
        current_owner: Option<RendererPageChildFrameTaskOwner>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageChildRealmMaterializationTurnAction {
    pub(crate) owner: RendererPageChildFrameTaskOwner,
    pub(crate) target_effect: PageChildRealmMaterializationTargetEffect,
}

pub(crate) type PageChildRealmMaterializationTurnOutcome =
    PageOwnerTurnOutcome<PageChildRealmMaterializationTurnAction>;
