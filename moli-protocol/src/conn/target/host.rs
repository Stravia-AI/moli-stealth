use crate::devtools_runtime::DevToolsTargetKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetHostParent {
    Browser,
    Tab { tab_target_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TargetHostLifecycle {
    Live,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetHost {
    id: String,
    kind: DevToolsTargetKind,
    parent: TargetHostParent,
    lifecycle: TargetHostLifecycle,
}

impl TargetHost {
    pub(crate) fn tab(id: String) -> Self {
        Self {
            id,
            kind: DevToolsTargetKind::Tab,
            parent: TargetHostParent::Browser,
            lifecycle: TargetHostLifecycle::Live,
        }
    }

    pub(crate) fn page(id: String, tab_target_id: String) -> Self {
        Self {
            id,
            kind: DevToolsTargetKind::Page,
            parent: TargetHostParent::Tab { tab_target_id },
            lifecycle: TargetHostLifecycle::Live,
        }
    }

    pub(crate) fn worker(id: String, kind: DevToolsTargetKind) -> Self {
        debug_assert!(matches!(
            kind,
            DevToolsTargetKind::Worker
                | DevToolsTargetKind::SharedWorker
                | DevToolsTargetKind::ServiceWorker
        ));
        Self {
            id,
            kind,
            parent: TargetHostParent::Browser,
            lifecycle: TargetHostLifecycle::Live,
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn kind(&self) -> DevToolsTargetKind {
        self.kind
    }

    pub(crate) fn parent(&self) -> &TargetHostParent {
        &self.parent
    }

    pub(crate) fn is_live(&self) -> bool {
        self.lifecycle == TargetHostLifecycle::Live
    }
}
