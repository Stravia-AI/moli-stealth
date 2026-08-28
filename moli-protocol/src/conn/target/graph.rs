use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TargetSessionSet {
    primary_session_id: Option<String>,
    auxiliary_session_ids: HashSet<String>,
}

impl TargetSessionSet {
    pub(crate) fn has_session(&self) -> bool {
        self.primary_session_id.is_some() || !self.auxiliary_session_ids.is_empty()
    }

    fn primary_session_id(&self) -> Option<&str> {
        self.primary_session_id.as_deref()
    }

    fn insert_session(&mut self, session_id: String, auxiliary: bool) {
        if !auxiliary && self.primary_session_id.is_none() {
            self.primary_session_id = Some(session_id);
        } else {
            self.auxiliary_session_ids.insert(session_id);
        }
    }

    fn remove_session(&mut self, session_id: &str) -> bool {
        if self.primary_session_id.as_deref() == Some(session_id) {
            self.primary_session_id = None;
            return true;
        }
        self.auxiliary_session_ids.remove(session_id)
    }

    fn session_ids(&self) -> Vec<String> {
        self.primary_session_id
            .iter()
            .cloned()
            .chain(self.auxiliary_session_ids.iter().cloned())
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TopLevelTarget {
    page_target_id: String,
    tab_target_id: String,
    tab_sessions: TargetSessionSet,
}

impl TopLevelTarget {
    fn new(page_target_id: String, tab_target_id: String) -> Self {
        Self {
            page_target_id,
            tab_target_id,
            tab_sessions: TargetSessionSet::default(),
        }
    }

    pub(crate) fn page_target_id(&self) -> &str {
        &self.page_target_id
    }

    pub(crate) fn tab_target_id(&self) -> &str {
        &self.tab_target_id
    }

    pub(crate) fn tab_session_ids(&self) -> Vec<String> {
        self.tab_sessions.session_ids()
    }

    pub(crate) fn tab_has_session(&self) -> bool {
        self.tab_sessions.has_session()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TargetGraph {
    top_level_targets: HashMap<String, TopLevelTarget>,
    page_to_tab: HashMap<String, String>,
    tab_to_page: HashMap<String, String>,
    tab_session_to_tab: HashMap<String, String>,
}

impl TargetGraph {
    pub(crate) fn register_top_level_page(
        &mut self,
        page_target_id: String,
        tab_target_id: String,
    ) {
        self.remove_top_level_page_by_page_target_id(&page_target_id);
        self.remove_top_level_page_by_tab_target_id(&tab_target_id);
        self.page_to_tab
            .insert(page_target_id.clone(), tab_target_id.clone());
        self.tab_to_page
            .insert(tab_target_id.clone(), page_target_id.clone());
        self.top_level_targets.insert(
            page_target_id.clone(),
            TopLevelTarget::new(page_target_id, tab_target_id),
        );
    }

    pub(crate) fn remove_top_level_page_by_page_target_id(
        &mut self,
        page_target_id: &str,
    ) -> Option<TopLevelTarget> {
        let target = self.top_level_targets.remove(page_target_id)?;
        if let Some(session_id) = target.tab_sessions.primary_session_id() {
            self.tab_session_to_tab.remove(session_id);
        }
        for session_id in &target.tab_sessions.auxiliary_session_ids {
            self.tab_session_to_tab.remove(session_id);
        }
        self.page_to_tab.remove(target.page_target_id());
        self.tab_to_page.remove(target.tab_target_id());
        Some(target)
    }

    pub(crate) fn remove_top_level_page_by_tab_target_id(
        &mut self,
        tab_target_id: &str,
    ) -> Option<TopLevelTarget> {
        let page_target_id = self.tab_to_page.get(tab_target_id)?.clone();
        self.remove_top_level_page_by_page_target_id(&page_target_id)
    }

    pub(crate) fn tab_target_id_for_page_target_id(&self, page_target_id: &str) -> Option<&str> {
        self.page_to_tab.get(page_target_id).map(String::as_str)
    }

    pub(crate) fn page_target_id_for_tab_target_id(&self, tab_target_id: &str) -> Option<&str> {
        self.tab_to_page.get(tab_target_id).map(String::as_str)
    }

    pub(crate) fn primary_session_id_for_tab_target_id(&self, tab_target_id: &str) -> Option<&str> {
        let page_target_id = self.tab_to_page.get(tab_target_id)?;
        self.top_level_targets
            .get(page_target_id)?
            .tab_sessions
            .primary_session_id()
    }

    pub(crate) fn assign_session_to_tab_target(
        &mut self,
        tab_target_id: &str,
        session_id: String,
        auxiliary: bool,
    ) -> bool {
        self.remove_tab_session(&session_id);
        let Some(page_target_id) = self.tab_to_page.get(tab_target_id).cloned() else {
            return false;
        };
        let Some(target) = self.top_level_targets.get_mut(&page_target_id) else {
            return false;
        };
        target
            .tab_sessions
            .insert_session(session_id.clone(), auxiliary);
        self.tab_session_to_tab
            .insert(session_id, tab_target_id.to_owned());
        true
    }

    pub(crate) fn remove_tab_session(&mut self, session_id: &str) -> Option<String> {
        let tab_target_id = self.tab_session_to_tab.remove(session_id)?;
        let page_target_id = self.tab_to_page.get(&tab_target_id)?;
        let target = self.top_level_targets.get_mut(page_target_id)?;
        target.tab_sessions.remove_session(session_id);
        Some(tab_target_id)
    }

    pub(crate) fn tab_target_id_for_session_id(&self, session_id: &str) -> Option<&str> {
        self.tab_session_to_tab.get(session_id).map(String::as_str)
    }

    pub(crate) fn top_level_target_for_page_target_id(
        &self,
        page_target_id: &str,
    ) -> Option<&TopLevelTarget> {
        self.top_level_targets.get(page_target_id)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.top_level_targets.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.top_level_targets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::TargetGraph;

    #[test]
    fn target_graph_registers_top_level_page_tab_pair() {
        let mut graph = TargetGraph::default();
        graph.register_top_level_page("TID-page".to_owned(), "TAB-TID-page".to_owned());

        assert_eq!(graph.len(), 1);
        assert_eq!(
            graph.tab_target_id_for_page_target_id("TID-page"),
            Some("TAB-TID-page")
        );
        assert_eq!(
            graph.page_target_id_for_tab_target_id("TAB-TID-page"),
            Some("TID-page")
        );
        let target = graph
            .top_level_target_for_page_target_id("TID-page")
            .expect("top level target");
        assert_eq!(target.page_target_id(), "TID-page");
        assert_eq!(target.tab_target_id(), "TAB-TID-page");
        assert!(!target.tab_has_session());
    }

    #[test]
    fn target_graph_rekey_removes_stale_reverse_entries() {
        let mut graph = TargetGraph::default();
        graph.register_top_level_page("TID-a".to_owned(), "TAB-a".to_owned());
        graph.register_top_level_page("TID-a".to_owned(), "TAB-b".to_owned());

        assert_eq!(graph.len(), 1);
        assert_eq!(
            graph.tab_target_id_for_page_target_id("TID-a"),
            Some("TAB-b")
        );
        assert_eq!(graph.page_target_id_for_tab_target_id("TAB-a"), None);
        assert_eq!(
            graph.page_target_id_for_tab_target_id("TAB-b"),
            Some("TID-a")
        );

        graph.register_top_level_page("TID-b".to_owned(), "TAB-b".to_owned());
        assert_eq!(graph.len(), 1);
        assert_eq!(graph.tab_target_id_for_page_target_id("TID-a"), None);
        assert_eq!(
            graph.tab_target_id_for_page_target_id("TID-b"),
            Some("TAB-b")
        );
        assert_eq!(
            graph.page_target_id_for_tab_target_id("TAB-b"),
            Some("TID-b")
        );
    }

    #[test]
    fn target_graph_removes_pair_from_either_side() {
        let mut graph = TargetGraph::default();
        graph.register_top_level_page("TID-page".to_owned(), "TAB-TID-page".to_owned());

        let removed = graph
            .remove_top_level_page_by_tab_target_id("TAB-TID-page")
            .expect("removed target");
        assert_eq!(removed.page_target_id(), "TID-page");
        assert!(graph.is_empty());
        assert_eq!(graph.tab_target_id_for_page_target_id("TID-page"), None);
        assert_eq!(graph.page_target_id_for_tab_target_id("TAB-TID-page"), None);
    }
}
