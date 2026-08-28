use std::collections::HashMap;

use super::graph::{TargetGraph, TopLevelTarget};
use super::host::{TargetHost, TargetHostParent};
use crate::devtools_runtime::DevToolsTargetKind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TargetHostDelta {
    Created { target_id: String },
    InfoChanged { target_id: String },
    Destroyed { target_id: String },
}

impl TargetHostDelta {
    pub(crate) fn created(target_id: impl Into<String>) -> Self {
        Self::Created {
            target_id: target_id.into(),
        }
    }

    pub(crate) fn info_changed(target_id: impl Into<String>) -> Self {
        Self::InfoChanged {
            target_id: target_id.into(),
        }
    }

    pub(crate) fn destroyed(target_id: impl Into<String>) -> Self {
        Self::Destroyed {
            target_id: target_id.into(),
        }
    }

    pub(crate) fn target_id(&self) -> &str {
        match self {
            Self::Created { target_id }
            | Self::InfoChanged { target_id }
            | Self::Destroyed { target_id } => target_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetClosurePlan {
    top_level_target: TopLevelTarget,
    deltas: Vec<TargetHostDelta>,
}

impl TargetClosurePlan {
    fn from_top_level_target(target: TopLevelTarget) -> Self {
        let deltas = vec![
            TargetHostDelta::destroyed(target.page_target_id()),
            TargetHostDelta::destroyed(target.tab_target_id()),
        ];
        Self {
            top_level_target: target,
            deltas,
        }
    }

    pub(crate) fn top_level_target(&self) -> &TopLevelTarget {
        &self.top_level_target
    }

    pub(crate) fn destroyed_target_ids(&self) -> impl Iterator<Item = &str> {
        self.deltas.iter().filter_map(|delta| match delta {
            TargetHostDelta::Destroyed { target_id } => Some(target_id.as_str()),
            TargetHostDelta::Created { .. } | TargetHostDelta::InfoChanged { .. } => None,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TargetRegistry {
    hosts: HashMap<String, TargetHost>,
    top_level: TargetGraph,
}

impl TargetRegistry {
    pub(crate) fn register_top_level_page(
        &mut self,
        page_target_id: String,
        tab_target_id: String,
    ) {
        self.remove_top_level_page_by_page_target_id(&page_target_id);
        self.remove_top_level_page_by_tab_target_id(&tab_target_id);
        self.hosts.insert(
            tab_target_id.clone(),
            TargetHost::tab(tab_target_id.clone()),
        );
        self.hosts.insert(
            page_target_id.clone(),
            TargetHost::page(page_target_id.clone(), tab_target_id.clone()),
        );
        self.top_level
            .register_top_level_page(page_target_id, tab_target_id);
    }

    pub(crate) fn register_worker(&mut self, target_id: String, kind: DevToolsTargetKind) {
        debug_assert!(matches!(
            kind,
            DevToolsTargetKind::Worker
                | DevToolsTargetKind::SharedWorker
                | DevToolsTargetKind::ServiceWorker
        ));
        self.hosts
            .insert(target_id.clone(), TargetHost::worker(target_id, kind));
    }

    pub(crate) fn remove_worker(&mut self, target_id: &str) -> Option<TargetHost> {
        let host = self.hosts.get(target_id)?;
        if !matches!(
            host.kind(),
            DevToolsTargetKind::Worker
                | DevToolsTargetKind::SharedWorker
                | DevToolsTargetKind::ServiceWorker
        ) {
            return None;
        }
        self.hosts.remove(target_id)
    }

    pub(crate) fn remove_top_level_page_by_page_target_id(
        &mut self,
        page_target_id: &str,
    ) -> Option<TargetClosurePlan> {
        let target = self
            .top_level
            .remove_top_level_page_by_page_target_id(page_target_id)?;
        self.hosts.remove(target.page_target_id());
        self.hosts.remove(target.tab_target_id());
        Some(TargetClosurePlan::from_top_level_target(target))
    }

    pub(crate) fn remove_top_level_page_by_tab_target_id(
        &mut self,
        tab_target_id: &str,
    ) -> Option<TargetClosurePlan> {
        let page_target_id = self
            .top_level
            .page_target_id_for_tab_target_id(tab_target_id)?
            .to_owned();
        self.remove_top_level_page_by_page_target_id(&page_target_id)
    }

    pub(crate) fn tab_target_id_for_page_target_id(&self, page_target_id: &str) -> Option<&str> {
        let host = self.hosts.get(page_target_id)?;
        debug_assert_eq!(host.id(), page_target_id);
        if host.kind() != crate::devtools_runtime::DevToolsTargetKind::Page || !host.is_live() {
            return None;
        }
        if !matches!(host.parent(), TargetHostParent::Tab { .. }) {
            return None;
        }
        self.top_level
            .tab_target_id_for_page_target_id(page_target_id)
    }

    pub(crate) fn page_target_id_for_tab_target_id(&self, tab_target_id: &str) -> Option<&str> {
        let host = self.hosts.get(tab_target_id)?;
        debug_assert_eq!(host.id(), tab_target_id);
        if host.kind() != crate::devtools_runtime::DevToolsTargetKind::Tab || !host.is_live() {
            return None;
        }
        self.top_level
            .page_target_id_for_tab_target_id(tab_target_id)
    }

    pub(crate) fn primary_session_id_for_tab_target_id(&self, tab_target_id: &str) -> Option<&str> {
        self.top_level
            .primary_session_id_for_tab_target_id(tab_target_id)
    }

    pub(crate) fn assign_session_to_tab_target(
        &mut self,
        tab_target_id: &str,
        session_id: String,
        auxiliary: bool,
    ) -> bool {
        if self
            .page_target_id_for_tab_target_id(tab_target_id)
            .is_none()
        {
            return false;
        }
        self.top_level
            .assign_session_to_tab_target(tab_target_id, session_id, auxiliary)
    }

    pub(crate) fn remove_tab_session(&mut self, session_id: &str) -> Option<String> {
        self.top_level.remove_tab_session(session_id)
    }

    pub(crate) fn tab_target_id_for_session_id(&self, session_id: &str) -> Option<&str> {
        self.top_level.tab_target_id_for_session_id(session_id)
    }

    pub(crate) fn top_level_target_for_page_target_id(
        &self,
        page_target_id: &str,
    ) -> Option<&TopLevelTarget> {
        self.top_level
            .top_level_target_for_page_target_id(page_target_id)
    }

    pub(crate) fn top_level_target_for_tab_target_id(
        &self,
        tab_target_id: &str,
    ) -> Option<&TopLevelTarget> {
        let page_target_id = self.page_target_id_for_tab_target_id(tab_target_id)?;
        self.top_level_target_for_page_target_id(page_target_id)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.top_level.len()
    }

    #[cfg(test)]
    pub(crate) fn host_count(&self) -> usize {
        self.hosts.len()
    }

    pub(crate) fn host(&self, target_id: &str) -> Option<&TargetHost> {
        self.hosts.get(target_id)
    }
}

#[cfg(test)]
mod tests {
    use super::TargetHostParent;
    use crate::devtools_runtime::DevToolsTargetKind;

    use super::TargetRegistry;

    #[test]
    fn target_registry_registers_page_tab_hosts() {
        let mut registry = TargetRegistry::default();
        registry.register_top_level_page("TID-page".to_owned(), "TAB-TID-page".to_owned());

        assert_eq!(registry.len(), 1);
        assert_eq!(
            registry.tab_target_id_for_page_target_id("TID-page"),
            Some("TAB-TID-page")
        );
        assert_eq!(
            registry.page_target_id_for_tab_target_id("TAB-TID-page"),
            Some("TID-page")
        );

        let page = registry.host("TID-page").expect("page host");
        assert_eq!(page.id(), "TID-page");
        assert_eq!(page.kind(), DevToolsTargetKind::Page);
        assert_eq!(
            page.parent(),
            &TargetHostParent::Tab {
                tab_target_id: "TAB-TID-page".to_owned()
            }
        );

        let tab = registry.host("TAB-TID-page").expect("tab host");
        assert_eq!(tab.id(), "TAB-TID-page");
        assert_eq!(tab.kind(), DevToolsTargetKind::Tab);
        assert_eq!(tab.parent(), &TargetHostParent::Browser);
    }

    #[test]
    fn target_registry_removes_page_and_tab_hosts_together() {
        let mut registry = TargetRegistry::default();
        registry.register_top_level_page("TID-page".to_owned(), "TAB-TID-page".to_owned());

        let removed = registry
            .remove_top_level_page_by_tab_target_id("TAB-TID-page")
            .expect("removed top-level target");
        assert_eq!(removed.top_level_target().page_target_id(), "TID-page");
        assert_eq!(removed.top_level_target().tab_target_id(), "TAB-TID-page");
        assert_eq!(
            removed.destroyed_target_ids().collect::<Vec<_>>(),
            vec!["TID-page", "TAB-TID-page"]
        );
        assert_eq!(registry.host("TID-page"), None);
        assert_eq!(registry.host("TAB-TID-page"), None);
        assert_eq!(registry.tab_target_id_for_page_target_id("TID-page"), None);
        assert_eq!(
            registry.page_target_id_for_tab_target_id("TAB-TID-page"),
            None
        );
    }

    #[test]
    fn target_registry_registers_and_removes_worker_host_without_top_level_graph() {
        let mut registry = TargetRegistry::default();
        registry.register_worker(
            "TID-shared-worker".to_owned(),
            DevToolsTargetKind::SharedWorker,
        );

        assert_eq!(registry.len(), 0);
        assert_eq!(registry.host_count(), 1);
        let host = registry.host("TID-shared-worker").expect("worker host");
        assert_eq!(host.id(), "TID-shared-worker");
        assert_eq!(host.kind(), DevToolsTargetKind::SharedWorker);
        assert_eq!(host.parent(), &TargetHostParent::Browser);

        let removed = registry
            .remove_worker("TID-shared-worker")
            .expect("removed worker host");
        assert_eq!(removed.id(), "TID-shared-worker");
        assert_eq!(registry.host("TID-shared-worker"), None);
        assert_eq!(registry.host_count(), 0);
    }
}
