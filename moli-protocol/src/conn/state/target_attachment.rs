use std::collections::HashMap;

use moli_core::browser_host::BrowserTargetHandle;

use super::DevToolsSessionState;

/// Frontend-owned CDP session state for one exact top-level Target.
///
/// The primary state also represents the root/no-`sessionId` route. Its
/// optional session id is only an attachment route: detaching or rebinding
/// that route must not move the state to the physical active/background
/// Target payload. Auxiliary states, by contrast, have an exact session
/// lifetime and disappear with their attachment route.
#[derive(Clone, Debug, Default, PartialEq)]
struct TopLevelTargetFrontendSessionProjection {
    primary_session_id: Option<String>,
    primary_state: DevToolsSessionState,
    auxiliary_session_states: HashMap<String, DevToolsSessionState>,
}

/// Exact-Target frontend session registry.
///
/// `unbound` is the root frontend candidate used while a BrowserContext has no
/// active Target. Binding a newly registered active Target adopts the current
/// candidate. Terminal retirement can reseed it with the explicitly retained
/// successor configuration, but it is never a residence for a registered
/// Target.
#[derive(Debug, Default)]
pub(crate) struct TopLevelTargetFrontendSessionRegistry {
    unbound: TopLevelTargetFrontendSessionProjection,
    by_target: HashMap<BrowserTargetHandle, TopLevelTargetFrontendSessionProjection>,
    target_by_session: HashMap<String, BrowserTargetHandle>,
}

impl TopLevelTargetFrontendSessionRegistry {
    fn clear_session_route(&mut self, session_id: &str) -> Option<BrowserTargetHandle> {
        let target = self.target_by_session.remove(session_id)?;
        if let Some(projection) = self.by_target.get_mut(&target) {
            if projection.primary_session_id.as_deref() == Some(session_id) {
                // The primary/root state belongs to the exact Target, not to
                // the optional explicit attachment route.
                projection.primary_session_id = None;
            } else {
                projection.auxiliary_session_states.remove(session_id);
            }
        }
        Some(target)
    }

    pub(crate) fn register_target(&mut self, target: BrowserTargetHandle) {
        self.by_target.entry(target).or_default();
    }

    pub(crate) fn register_new_active_target(&mut self, target: BrowserTargetHandle) {
        if self.by_target.contains_key(&target) {
            return;
        }
        let candidate = std::mem::take(&mut self.unbound);
        debug_assert!(
            candidate.primary_session_id.is_none() && candidate.auxiliary_session_states.is_empty(),
            "an unbound frontend candidate cannot own explicit Target routes"
        );
        self.by_target.insert(target, candidate);
    }

    #[cfg(test)]
    pub(crate) fn contains_target(&self, target: &BrowserTargetHandle) -> bool {
        self.by_target.contains_key(target)
    }

    pub(crate) fn auxiliary_session_count(&self) -> usize {
        self.by_target
            .values()
            .map(|projection| projection.auxiliary_session_states.len())
            .sum()
    }

    pub(crate) fn target_entries(
        &self,
    ) -> impl Iterator<
        Item = (
            &BrowserTargetHandle,
            &DevToolsSessionState,
            &HashMap<String, DevToolsSessionState>,
        ),
    > {
        self.by_target.iter().map(|(target, projection)| {
            (
                target,
                &projection.primary_state,
                &projection.auxiliary_session_states,
            )
        })
    }

    pub(crate) fn session_states(&self) -> impl Iterator<Item = &DevToolsSessionState> {
        self.by_target
            .values()
            .flat_map(|projection| {
                std::iter::once(&projection.primary_state)
                    .chain(projection.auxiliary_session_states.values())
            })
            .chain(std::iter::once(&self.unbound.primary_state))
            .chain(self.unbound.auxiliary_session_states.values())
    }

    pub(crate) fn primary_session_id(&self, target: &BrowserTargetHandle) -> Option<&str> {
        self.by_target
            .get(target)
            .and_then(|projection| projection.primary_session_id.as_deref())
    }

    pub(crate) fn primary_state(
        &self,
        target: &BrowserTargetHandle,
    ) -> Option<&DevToolsSessionState> {
        self.by_target
            .get(target)
            .map(|projection| &projection.primary_state)
    }

    #[cfg(test)]
    pub(crate) fn primary_state_mut(
        &mut self,
        target: &BrowserTargetHandle,
    ) -> Option<&mut DevToolsSessionState> {
        self.by_target
            .get_mut(target)
            .map(|projection| &mut projection.primary_state)
    }

    pub(crate) fn primary_state_or_unbound(
        &self,
        target: Option<&BrowserTargetHandle>,
    ) -> &DevToolsSessionState {
        target
            .and_then(|target| self.primary_state(target))
            .unwrap_or(&self.unbound.primary_state)
    }

    pub(crate) fn primary_state_or_unbound_mut(
        &mut self,
        target: Option<&BrowserTargetHandle>,
    ) -> &mut DevToolsSessionState {
        if let Some(projection) = target.and_then(|target| self.by_target.get_mut(target)) {
            return &mut projection.primary_state;
        }
        &mut self.unbound.primary_state
    }

    pub(crate) fn auxiliary_states(
        &self,
        target: &BrowserTargetHandle,
    ) -> Option<&HashMap<String, DevToolsSessionState>> {
        self.by_target
            .get(target)
            .map(|projection| &projection.auxiliary_session_states)
    }

    pub(crate) fn auxiliary_states_or_unbound(
        &self,
        target: Option<&BrowserTargetHandle>,
    ) -> &HashMap<String, DevToolsSessionState> {
        target
            .and_then(|target| self.auxiliary_states(target))
            .unwrap_or(&self.unbound.auxiliary_session_states)
    }

    pub(crate) fn states_or_unbound_mut(
        &mut self,
        target: Option<&BrowserTargetHandle>,
    ) -> (
        &mut DevToolsSessionState,
        &mut HashMap<String, DevToolsSessionState>,
    ) {
        let projection = match target.and_then(|target| self.by_target.get_mut(target)) {
            Some(projection) => projection,
            None => &mut self.unbound,
        };
        (
            &mut projection.primary_state,
            &mut projection.auxiliary_session_states,
        )
    }

    pub(crate) fn states_mut(
        &mut self,
        target: &BrowserTargetHandle,
    ) -> Option<(
        &mut DevToolsSessionState,
        &mut HashMap<String, DevToolsSessionState>,
    )> {
        let projection = self.by_target.get_mut(target)?;
        Some((
            &mut projection.primary_state,
            &mut projection.auxiliary_session_states,
        ))
    }

    pub(crate) fn replace_primary_session(
        &mut self,
        target: &BrowserTargetHandle,
        session_id: Option<String>,
    ) -> Option<String> {
        let previous = self
            .by_target
            .entry(target.clone())
            .or_default()
            .primary_session_id
            .take();
        if let Some(previous) = previous.as_ref()
            && self.target_by_session.get(previous) == Some(target)
        {
            self.target_by_session.remove(previous);
        }
        if let Some(session_id) = session_id {
            self.clear_session_route(&session_id);
            self.by_target
                .entry(target.clone())
                .or_default()
                .primary_session_id = Some(session_id.clone());
            self.target_by_session.insert(session_id, target.clone());
        }
        previous
    }

    pub(crate) fn attach_auxiliary_session(
        &mut self,
        target: &BrowserTargetHandle,
        session_id: String,
    ) -> bool {
        if self.is_auxiliary_session_for_target(target, &session_id) {
            return false;
        }
        self.clear_session_route(&session_id);
        let projection = self.by_target.entry(target.clone()).or_default();
        projection
            .auxiliary_session_states
            .insert(session_id.clone(), DevToolsSessionState::default());
        self.target_by_session.insert(session_id, target.clone());
        true
    }

    pub(crate) fn target_for_session(&self, session_id: &str) -> Option<&BrowserTargetHandle> {
        self.target_by_session.get(session_id)
    }

    pub(crate) fn primary_target_for_session(
        &self,
        session_id: &str,
    ) -> Option<&BrowserTargetHandle> {
        let target = self.target_by_session.get(session_id)?;
        (self.primary_session_id(target) == Some(session_id)).then_some(target)
    }

    pub(crate) fn is_auxiliary_session_for_target(
        &self,
        target: &BrowserTargetHandle,
        session_id: &str,
    ) -> bool {
        self.by_target
            .get(target)
            .is_some_and(|projection| projection.auxiliary_session_states.contains_key(session_id))
    }

    pub(crate) fn auxiliary_session_ids(&self, target: &BrowserTargetHandle) -> Vec<String> {
        let mut ids = self
            .by_target
            .get(target)
            .map(|projection| {
                projection
                    .auxiliary_session_states
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        ids.sort();
        ids
    }

    pub(crate) fn session_state(
        &self,
        target: &BrowserTargetHandle,
        is_auxiliary: bool,
        session_id: Option<&str>,
    ) -> Option<&DevToolsSessionState> {
        let projection = self.by_target.get(target)?;
        if is_auxiliary {
            return session_id
                .and_then(|session_id| projection.auxiliary_session_states.get(session_id));
        }
        Some(&projection.primary_state)
    }

    pub(crate) fn session_state_mut(
        &mut self,
        target: &BrowserTargetHandle,
        is_auxiliary: bool,
        session_id: Option<&str>,
    ) -> Option<&mut DevToolsSessionState> {
        let projection = self.by_target.get_mut(target)?;
        if is_auxiliary {
            return session_id
                .and_then(|session_id| projection.auxiliary_session_states.get_mut(session_id));
        }
        Some(&mut projection.primary_state)
    }

    pub(crate) fn reset_primary_state(&mut self, target: &BrowserTargetHandle) -> bool {
        let Some(projection) = self.by_target.get_mut(target) else {
            return false;
        };
        projection.primary_state = DevToolsSessionState::default();
        true
    }

    pub(crate) fn remove_auxiliary_session(
        &mut self,
        session_id: &str,
    ) -> Option<BrowserTargetHandle> {
        let target = self.target_by_session.remove(session_id)?;
        let projection = self.by_target.get_mut(&target)?;
        if projection
            .auxiliary_session_states
            .remove(session_id)
            .is_some()
        {
            Some(target)
        } else {
            // The reverse route may refer to a primary session. Restore it;
            // callers of this method only own auxiliary detach semantics.
            self.target_by_session.insert(session_id.to_owned(), target);
            None
        }
    }

    pub(crate) fn remove_target(
        &mut self,
        target: &BrowserTargetHandle,
    ) -> (Option<String>, Vec<String>) {
        let Some((_projection, primary_session_id, auxiliary_session_ids)) =
            self.take_target_projection_and_routes(target)
        else {
            return (None, Vec::new());
        };
        (primary_session_id, auxiliary_session_ids)
    }

    pub(crate) fn retire_target_to_unbound(
        &mut self,
        target: &BrowserTargetHandle,
    ) -> (Option<String>, Vec<String>) {
        let Some((projection, primary_session_id, auxiliary_session_ids)) =
            self.take_target_projection_and_routes(target)
        else {
            return (None, Vec::new());
        };
        let successor_primary_state = DevToolsSessionState {
            runtime_bindings: projection.primary_state.runtime_bindings,
            ..Default::default()
        };
        self.unbound = TopLevelTargetFrontendSessionProjection {
            primary_state: successor_primary_state,
            ..Default::default()
        };
        (primary_session_id, auxiliary_session_ids)
    }

    fn take_target_projection_and_routes(
        &mut self,
        target: &BrowserTargetHandle,
    ) -> Option<(
        TopLevelTargetFrontendSessionProjection,
        Option<String>,
        Vec<String>,
    )> {
        let projection = self.by_target.remove(target)?;
        let primary_session_id = projection.primary_session_id.clone();
        let mut auxiliary_session_ids = projection
            .auxiliary_session_states
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        auxiliary_session_ids.sort();
        for session_id in primary_session_id
            .iter()
            .chain(auxiliary_session_ids.iter())
        {
            if self.target_by_session.get(session_id) == Some(target) {
                self.target_by_session.remove(session_id);
            }
        }
        Some((projection, primary_session_id, auxiliary_session_ids))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn residence_changes_do_not_change_exact_frontend_session_state() {
        let target = BrowserTargetHandle::staged("TID-1");
        let mut registry = TopLevelTargetFrontendSessionRegistry::default();
        registry.register_target(target.clone());
        registry.replace_primary_session(&target, Some("SID-primary".to_owned()));
        registry.attach_auxiliary_session(&target, "SID-aux".to_owned());
        registry
            .primary_state_mut(&target)
            .unwrap()
            .runtime_session_state
            .runtime_frontend_enabled = true;
        registry
            .session_state_mut(&target, true, Some("SID-aux"))
            .unwrap()
            .page_session_state
            .page_lifecycle_events = true;

        assert_eq!(registry.primary_session_id(&target), Some("SID-primary"));
        assert_eq!(registry.target_for_session("SID-primary"), Some(&target));
        assert_eq!(registry.target_for_session("SID-aux"), Some(&target));
        assert!(
            registry
                .primary_state(&target)
                .unwrap()
                .runtime_session_state
                .runtime_frontend_enabled
        );
        assert!(
            registry
                .session_state(&target, true, Some("SID-aux"))
                .unwrap()
                .page_session_state
                .page_lifecycle_events
        );
    }

    #[test]
    fn same_public_id_cannot_inherit_predecessor_sessions_or_state() {
        let predecessor = BrowserTargetHandle::staged("TID-1");
        let successor = BrowserTargetHandle::staged("TID-1");
        let mut registry = TopLevelTargetFrontendSessionRegistry::default();
        registry.register_target(predecessor.clone());
        registry.replace_primary_session(&predecessor, Some("SID-old".to_owned()));
        registry.attach_auxiliary_session(&predecessor, "SID-old-aux".to_owned());
        registry
            .primary_state_mut(&predecessor)
            .unwrap()
            .runtime_session_state
            .runtime_frontend_enabled = true;
        registry.register_target(successor.clone());

        assert_eq!(registry.primary_session_id(&successor), None);
        assert!(
            !registry
                .primary_state(&successor)
                .unwrap()
                .runtime_session_state
                .runtime_frontend_enabled
        );
        assert_eq!(registry.target_for_session("SID-old"), Some(&predecessor));
        assert_eq!(
            registry.target_for_session("SID-old-aux"),
            Some(&predecessor)
        );
        let removed = registry.remove_target(&predecessor);
        assert_eq!(removed.0.as_deref(), Some("SID-old"));
        assert_eq!(removed.1, ["SID-old-aux"]);
        assert_eq!(registry.target_for_session("SID-old"), None);
        assert_eq!(registry.target_for_session("SID-old-aux"), None);
        assert!(registry.contains_target(&successor));
    }

    #[test]
    fn reusing_a_session_id_moves_one_route_and_drops_auxiliary_state() {
        let first = BrowserTargetHandle::staged("TID-1");
        let second = BrowserTargetHandle::staged("TID-2");
        let mut registry = TopLevelTargetFrontendSessionRegistry::default();
        registry.register_target(first.clone());
        registry.register_target(second.clone());
        registry.attach_auxiliary_session(&first, "SID-reused".to_owned());
        registry
            .session_state_mut(&first, true, Some("SID-reused"))
            .unwrap()
            .runtime_session_state
            .runtime_frontend_enabled = true;

        registry.replace_primary_session(&second, Some("SID-reused".to_owned()));

        assert!(!registry.is_auxiliary_session_for_target(&first, "SID-reused"));
        assert_eq!(registry.primary_session_id(&second), Some("SID-reused"));
        assert_eq!(registry.target_for_session("SID-reused"), Some(&second));
        assert!(registry.auxiliary_session_ids(&first).is_empty());
    }

    #[test]
    fn primary_state_survives_detach_and_rebind_on_the_same_exact_target() {
        let target = BrowserTargetHandle::staged("TID-1");
        let mut registry = TopLevelTargetFrontendSessionRegistry::default();
        registry.register_target(target.clone());
        registry.replace_primary_session(&target, Some("SID-old".to_owned()));
        registry
            .primary_state_mut(&target)
            .unwrap()
            .runtime_session_state
            .runtime_frontend_enabled = true;

        registry.replace_primary_session(&target, None);
        registry.replace_primary_session(&target, Some("SID-new".to_owned()));

        assert_eq!(registry.primary_session_id(&target), Some("SID-new"));
        assert!(
            registry
                .primary_state(&target)
                .unwrap()
                .runtime_session_state
                .runtime_frontend_enabled
        );
    }

    #[test]
    fn first_active_target_adopts_only_the_unbound_root_candidate() {
        let target = BrowserTargetHandle::staged("TID-1");
        let mut registry = TopLevelTargetFrontendSessionRegistry::default();
        registry
            .primary_state_or_unbound_mut(None)
            .runtime_session_state
            .runtime_frontend_enabled = true;

        registry.register_new_active_target(target.clone());

        assert!(
            registry
                .primary_state(&target)
                .unwrap()
                .runtime_session_state
                .runtime_frontend_enabled
        );
        assert!(
            !registry
                .primary_state_or_unbound(None)
                .runtime_session_state
                .runtime_frontend_enabled
        );
    }

    #[test]
    fn a_background_registration_does_not_hide_the_unbound_root_candidate() {
        let background = BrowserTargetHandle::staged("TID-background");
        let mut registry = TopLevelTargetFrontendSessionRegistry::default();
        registry
            .primary_state_or_unbound_mut(None)
            .runtime_session_state
            .runtime_frontend_enabled = true;
        registry.register_target(background);

        assert!(
            registry
                .session_states()
                .any(|state| state.runtime_session_state.runtime_frontend_enabled)
        );
    }

    #[test]
    fn retiring_an_active_target_reseeds_the_next_root_candidate_without_routes() {
        let first = BrowserTargetHandle::staged("TID-first");
        let second = BrowserTargetHandle::staged("TID-second");
        let mut registry = TopLevelTargetFrontendSessionRegistry::default();
        registry.register_new_active_target(first.clone());
        registry.replace_primary_session(&first, Some("SID-first".to_owned()));
        registry.attach_auxiliary_session(&first, "SID-aux".to_owned());
        registry
            .primary_state_mut(&first)
            .unwrap()
            .runtime_bindings
            .push(crate::conn::RuntimeBindingDefinition {
                name: "persisted".to_owned(),
                execution_context_name: None,
            });
        registry
            .primary_state_mut(&first)
            .unwrap()
            .runtime_session_state
            .runtime_frontend_enabled = true;
        registry
            .primary_state_mut(&first)
            .unwrap()
            .page_session_state
            .page_lifecycle_events = true;

        let removed = registry.retire_target_to_unbound(&first);
        registry.register_new_active_target(second.clone());

        assert_eq!(removed.0.as_deref(), Some("SID-first"));
        assert_eq!(removed.1, ["SID-aux"]);
        assert_eq!(registry.target_for_session("SID-first"), None);
        assert_eq!(registry.target_for_session("SID-aux"), None);
        assert_eq!(
            registry.primary_state(&second).unwrap().runtime_bindings[0].name,
            "persisted"
        );
        assert!(
            !registry
                .primary_state(&second)
                .unwrap()
                .runtime_session_state
                .runtime_frontend_enabled,
            "Target-local Runtime subscription must not enter the successor candidate"
        );
        assert!(
            !registry
                .primary_state(&second)
                .unwrap()
                .page_session_state
                .page_lifecycle_events,
            "Target-local Page subscription must not enter the successor candidate"
        );
    }
}
