use super::RendererPageMainDocumentRuntimeOwner;

/// Exact-owner result of installing one parser async module into the shared
/// main-Document `PendingScript` store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageParserAsyncModuleAdmissionTargetEffect {
    AdmittedToCurrentOwner,
    RejectedByCurrentOwner,
    DiscardedStaleOwner,
}

/// Execution-produced result reserved for parser async-module admission.
///
/// Keeping this separate from runtime-created script admission prevents the
/// two lifetime models from being flattened into a `kind + bool` completion
/// policy: parser modules enter `PendingScript`, while runtime-created scripts
/// enter `DynamicScriptOwner`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageParserAsyncModuleAdmissionTurnAction {
    owner: RendererPageMainDocumentRuntimeOwner,
    target_effect: PageParserAsyncModuleAdmissionTargetEffect,
}

impl PageParserAsyncModuleAdmissionTurnAction {
    pub(super) const fn new(
        owner: RendererPageMainDocumentRuntimeOwner,
        target_effect: PageParserAsyncModuleAdmissionTargetEffect,
    ) -> Self {
        Self {
            owner,
            target_effect,
        }
    }

    #[cfg(test)]
    pub(crate) const fn owner(self) -> RendererPageMainDocumentRuntimeOwner {
        self.owner
    }

    pub(crate) const fn target_effect(self) -> PageParserAsyncModuleAdmissionTargetEffect {
        self.target_effect
    }
}
