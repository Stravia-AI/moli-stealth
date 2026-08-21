use moli_module_script_tree as module_tree;
use std::sync::Arc;

use crate::dom::native::NativeNodeId;
use crate::frame_owner_model::{
    FrameDocumentModulepreloadLinkClient, MainDocumentModulepreloadEventOwner,
};

use super::ModuleMapKey;

/// Exact owner client for one connected `<link rel=modulepreload>` processing.
///
/// Keeping the captured key and processing identity together prevents a later
/// attribute mutation from accepting the terminal of an older link request.
#[derive(Debug)]
pub(crate) struct NativeModulepreloadLinkClient {
    owner: NativeNodeId,
    key: ModuleMapKey,
    main_document_event_owner: Option<MainDocumentModulepreloadEventOwner>,
    frame_document_client: Option<FrameDocumentModulepreloadLinkClient>,
}

impl NativeModulepreloadLinkClient {
    #[cfg(test)]
    pub(crate) fn new(owner: NativeNodeId, key: ModuleMapKey) -> Arc<Self> {
        Arc::new(Self {
            owner,
            key,
            main_document_event_owner: None,
            frame_document_client: None,
        })
    }

    pub(crate) fn new_with_main_document_event_owner(
        owner: NativeNodeId,
        key: ModuleMapKey,
        main_document_event_owner: MainDocumentModulepreloadEventOwner,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner,
            key,
            main_document_event_owner: Some(main_document_event_owner),
            frame_document_client: None,
        })
    }

    pub(crate) fn new_for_frame_document(
        owner: NativeNodeId,
        key: ModuleMapKey,
        frame_document_client: FrameDocumentModulepreloadLinkClient,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner,
            key,
            main_document_event_owner: None,
            frame_document_client: Some(frame_document_client),
        })
    }

    pub(crate) fn owner(&self) -> NativeNodeId {
        self.owner
    }

    pub(crate) fn key(&self) -> &ModuleMapKey {
        &self.key
    }

    pub(crate) fn main_document_event_owner(&self) -> Option<MainDocumentModulepreloadEventOwner> {
        self.main_document_event_owner
    }

    pub(crate) fn frame_document_client(&self) -> Option<FrameDocumentModulepreloadLinkClient> {
        self.frame_document_client
    }

    pub(crate) fn ptr_eq(left: &Arc<Self>, right: &Arc<Self>) -> bool {
        Arc::ptr_eq(left, right)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeModuleScriptSingleModuleClient {
    token: module_tree::SingleModuleClientToken,
    import_phase: module_tree::ModuleImportPhase,
}

impl NativeModuleScriptSingleModuleClient {
    pub(crate) fn new(
        token: module_tree::SingleModuleClientToken,
        import_phase: module_tree::ModuleImportPhase,
    ) -> Self {
        Self {
            token,
            import_phase,
        }
    }

    pub(crate) fn token(&self) -> module_tree::SingleModuleClientToken {
        self.token
    }

    pub(crate) fn import_phase(&self) -> module_tree::ModuleImportPhase {
        self.import_phase
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeDynamicImportSingleModuleClient {
    token: module_tree::SingleModuleClientToken,
    import_phase: module_tree::ModuleImportPhase,
}

impl NativeDynamicImportSingleModuleClient {
    pub(crate) fn new(
        token: module_tree::SingleModuleClientToken,
        import_phase: module_tree::ModuleImportPhase,
    ) -> Self {
        Self {
            token,
            import_phase,
        }
    }

    pub(crate) fn token(&self) -> module_tree::SingleModuleClientToken {
        self.token
    }

    pub(crate) fn import_phase(&self) -> module_tree::ModuleImportPhase {
        self.import_phase
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeModuleMapSingleModuleClient {
    ModuleScript(NativeModuleScriptSingleModuleClient),
    DynamicImport(NativeDynamicImportSingleModuleClient),
}

impl NativeModuleMapSingleModuleClient {
    pub(crate) fn module_script(
        token: module_tree::SingleModuleClientToken,
        import_phase: module_tree::ModuleImportPhase,
    ) -> Self {
        Self::ModuleScript(NativeModuleScriptSingleModuleClient::new(
            token,
            import_phase,
        ))
    }

    pub(crate) fn dynamic_import(
        token: module_tree::SingleModuleClientToken,
        import_phase: module_tree::ModuleImportPhase,
    ) -> Self {
        Self::DynamicImport(NativeDynamicImportSingleModuleClient::new(
            token,
            import_phase,
        ))
    }

    pub(crate) fn token(&self) -> module_tree::SingleModuleClientToken {
        match self {
            Self::ModuleScript(client) => client.token(),
            Self::DynamicImport(client) => client.token(),
        }
    }

    pub(crate) fn import_phase(&self) -> module_tree::ModuleImportPhase {
        match self {
            Self::ModuleScript(client) => client.import_phase(),
            Self::DynamicImport(client) => client.import_phase(),
        }
    }

    pub(crate) fn is_module_script_client(&self) -> bool {
        matches!(self, Self::ModuleScript(_))
    }

    pub(crate) fn is_dynamic_import_client(&self) -> bool {
        matches!(self, Self::DynamicImport(_))
    }

    pub(crate) fn client_name(&self) -> &'static str {
        match self {
            Self::ModuleScript(_) => "ModuleScript",
            Self::DynamicImport(_) => "DynamicImport",
        }
    }
}
