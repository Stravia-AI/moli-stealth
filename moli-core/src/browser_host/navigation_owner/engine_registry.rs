use std::collections::HashMap;

use crate::{RendererOutputTransportSender, runtime::NavigationEngine};

use super::{
    BrowserNavigationOwner, BrowserPageOwnerKey, BrowserTargetRegistryError, BrowserTargetResidence,
};

/// Strong owner of one renderer/navigation runtime.
///
/// Runtime work stays behind semantic operations on `BrowserNavigationOwner`;
/// frontend adapters may transfer an engine into the registry, but cannot
/// borrow the selected engine back out.
struct BrowserPageOwner {
    engine: NavigationEngine,
}

impl BrowserPageOwner {
    fn new(engine: NavigationEngine) -> Self {
        Self { engine }
    }
}

/// What Browser Core must do with the engine selected before a handoff.
///
/// The owner identity is protocol-neutral. `Retain` is used only when the
/// physical Target still has a Page whose runtime must survive parking;
/// `Discard` permits same-context reuse because no Page is resident.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserSelectedTargetEngineDisposition {
    Unbound,
    Discard(BrowserPageOwnerKey),
    Retain(BrowserPageOwnerKey),
}

impl BrowserSelectedTargetEngineDisposition {
    pub(super) fn expected_owner(&self) -> Option<&BrowserPageOwnerKey> {
        match self {
            Self::Unbound => None,
            Self::Discard(owner) | Self::Retain(owner) => Some(owner),
        }
    }

    fn owner_to_retain(&self) -> Option<&BrowserPageOwnerKey> {
        match self {
            Self::Retain(owner) => Some(owner),
            Self::Unbound | Self::Discard(_) => None,
        }
    }
}

/// Exact same-BrowserContext Target engine selection request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTargetEngineHandoff {
    current: BrowserSelectedTargetEngineDisposition,
    next: BrowserPageOwnerKey,
}

impl BrowserTargetEngineHandoff {
    pub fn new(
        current: BrowserSelectedTargetEngineDisposition,
        next: BrowserPageOwnerKey,
    ) -> Result<Self, BrowserTargetEngineContextMismatch> {
        if let Some(current_owner) = current.expected_owner()
            && current_owner.browser_context_id() != next.browser_context_id()
        {
            return Err(BrowserTargetEngineContextMismatch {
                current: current_owner.clone(),
                next,
            });
        }
        Ok(Self { current, next })
    }
}

/// A same-context Target handoff attempted to cross BrowserContext identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTargetEngineContextMismatch {
    current: BrowserPageOwnerKey,
    next: BrowserPageOwnerKey,
}

impl BrowserTargetEngineContextMismatch {
    pub fn current(&self) -> &BrowserPageOwnerKey {
        &self.current
    }

    pub fn next(&self) -> &BrowserPageOwnerKey {
        &self.next
    }
}

impl std::fmt::Display for BrowserTargetEngineContextMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "same-context target engine handoff cannot move from BrowserContext {:?} to {:?}",
            self.current.browser_context_id(),
            self.next.browser_context_id()
        )
    }
}

impl std::error::Error for BrowserTargetEngineContextMismatch {}

/// Exact cross-BrowserContext engine selection request.
///
/// A context without an active Target selects an unbound engine. Unlike a
/// same-context Target handoff, a missing retained successor always creates a
/// new engine because renderer context runtime state cannot cross profiles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BrowserContextEngineHandoff {
    current: BrowserSelectedTargetEngineDisposition,
    next: Option<BrowserPageOwnerKey>,
}

impl BrowserContextEngineHandoff {
    pub(super) fn new(
        current: BrowserSelectedTargetEngineDisposition,
        next: Option<BrowserPageOwnerKey>,
    ) -> Self {
        Self { current, next }
    }
}

/// Where an exact loaded Target engine resides after physical Page commit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserTargetEngineResidence {
    Selected,
    Retained,
}

/// Observable outcome of one Browser-owned engine selection transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserTargetEngineHandoffOutcome {
    ReusedSelected,
    RestoredRetained,
    CreatedReplacement,
}

/// A handoff named a different current Target than the registry owns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserTargetEngineOwnerMismatch {
    selected: Option<BrowserPageOwnerKey>,
    requested: Option<BrowserPageOwnerKey>,
}

impl BrowserTargetEngineOwnerMismatch {
    pub fn selected(&self) -> Option<&BrowserPageOwnerKey> {
        self.selected.as_ref()
    }

    pub fn requested(&self) -> Option<&BrowserPageOwnerKey> {
        self.requested.as_ref()
    }
}

impl std::fmt::Display for BrowserTargetEngineOwnerMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "selected target engine owner {:?} does not match requested current owner {:?}",
            self.selected, self.requested
        )
    }
}

impl std::error::Error for BrowserTargetEngineOwnerMismatch {}

/// A Target engine could not be adopted into the exact Browser-owned
/// topology and selected-engine state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserTargetEngineAdoptionError {
    Target(BrowserTargetRegistryError),
    EngineOwner(BrowserTargetEngineOwnerMismatch),
    SelectedTargetProjectionMismatch {
        authoritative: Option<BrowserPageOwnerKey>,
        projected: Option<BrowserPageOwnerKey>,
    },
}

impl std::fmt::Display for BrowserTargetEngineAdoptionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Target(error) => error.fmt(formatter),
            Self::EngineOwner(error) => error.fmt(formatter),
            Self::SelectedTargetProjectionMismatch {
                authoritative,
                projected,
            } => write!(
                formatter,
                "authoritative selected Target {:?} does not match projected current Target {:?}",
                authoritative, projected
            ),
        }
    }
}

impl std::error::Error for BrowserTargetEngineAdoptionError {}

impl From<BrowserTargetRegistryError> for BrowserTargetEngineAdoptionError {
    fn from(error: BrowserTargetRegistryError) -> Self {
        Self::Target(error)
    }
}

impl From<BrowserTargetEngineOwnerMismatch> for BrowserTargetEngineAdoptionError {
    fn from(error: BrowserTargetEngineOwnerMismatch) -> Self {
        Self::EngineOwner(error)
    }
}

/// Browser-owned active/parked NavigationEngine registry.
///
/// `selected_owner == None` is reserved for startup or an active
/// BrowserContext with no Target. Every selected Target engine is otherwise
/// keyed by `{browser_context_id, target_id}`; CDP session identity never
/// participates in lookup or handoff authorization.
pub(super) struct BrowserTargetEngineRegistry {
    selected: BrowserPageOwner,
    selected_owner: Option<BrowserPageOwnerKey>,
    retained: HashMap<BrowserPageOwnerKey, BrowserPageOwner>,
    renderer_output_transport_sender: Option<RendererOutputTransportSender>,
}

impl BrowserTargetEngineRegistry {
    pub(super) fn new(engine: NavigationEngine) -> Self {
        Self {
            selected: BrowserPageOwner::new(engine),
            selected_owner: None,
            retained: HashMap::new(),
            renderer_output_transport_sender: None,
        }
    }

    pub(super) fn selected_engine(&self) -> &NavigationEngine {
        &self.selected.engine
    }

    pub(super) fn selected_engine_mut(&mut self) -> &mut NavigationEngine {
        &mut self.selected.engine
    }

    pub(super) fn retained_engine_mut(
        &mut self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<&mut NavigationEngine> {
        self.retained.get_mut(owner).map(|owner| &mut owner.engine)
    }

    pub(super) fn selected_owner(&self) -> Option<&BrowserPageOwnerKey> {
        self.selected_owner.as_ref()
    }

    pub(super) fn set_renderer_output_transport_sender(
        &mut self,
        sender: RendererOutputTransportSender,
    ) {
        self.renderer_output_transport_sender = Some(sender.clone());
        self.selected
            .engine
            .set_renderer_output_transport_sender(sender.clone());
        for owner in self.retained.values() {
            owner
                .engine
                .set_renderer_output_transport_sender(sender.clone());
        }
    }

    pub(super) fn configure_detached_engine(&self, engine: &NavigationEngine) {
        if let Some(sender) = self.renderer_output_transport_sender.as_ref() {
            engine.set_renderer_output_transport_sender(sender.clone());
        }
    }

    fn configure_and_wrap(&self, engine: NavigationEngine) -> BrowserPageOwner {
        self.configure_detached_engine(&engine);
        BrowserPageOwner::new(engine)
    }

    pub(super) fn validate_current(
        &self,
        requested: &BrowserSelectedTargetEngineDisposition,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        let requested_owner = requested.expected_owner();
        if self.selected_owner.as_ref() == requested_owner {
            return Ok(());
        }
        Err(BrowserTargetEngineOwnerMismatch {
            selected: self.selected_owner.clone(),
            requested: requested_owner.cloned(),
        })
    }

    fn retain_previous_if_requested(
        &mut self,
        disposition: &BrowserSelectedTargetEngineDisposition,
        previous: BrowserPageOwner,
    ) {
        if let Some(owner) = disposition.owner_to_retain() {
            self.retained.insert(owner.clone(), previous);
        }
    }

    pub(super) fn install_unbound_engine(
        &mut self,
        engine: NavigationEngine,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        if self.selected_owner.is_some() {
            return Err(BrowserTargetEngineOwnerMismatch {
                selected: self.selected_owner.clone(),
                requested: None,
            });
        }
        self.selected = self.configure_and_wrap(engine);
        Ok(())
    }

    pub(super) fn adopt_target_engine(
        &mut self,
        owner: BrowserPageOwnerKey,
        residence: BrowserTargetEngineResidence,
        engine: NavigationEngine,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        let engine = self.configure_and_wrap(engine);
        match residence {
            BrowserTargetEngineResidence::Selected => {
                if self
                    .selected_owner
                    .as_ref()
                    .is_some_and(|selected| selected != &owner)
                {
                    return Err(BrowserTargetEngineOwnerMismatch {
                        selected: self.selected_owner.clone(),
                        requested: Some(owner),
                    });
                }
                self.retained.remove(&owner);
                self.selected = engine;
                self.selected_owner = Some(owner);
            }
            BrowserTargetEngineResidence::Retained => {
                if self.selected_owner.as_ref() == Some(&owner) {
                    return Err(BrowserTargetEngineOwnerMismatch {
                        selected: self.selected_owner.clone(),
                        requested: None,
                    });
                }
                self.retained.insert(owner, engine);
            }
        }
        Ok(())
    }

    pub(super) fn handoff_target_engine<F>(
        &mut self,
        handoff: BrowserTargetEngineHandoff,
        create_replacement: F,
    ) -> Result<BrowserTargetEngineHandoffOutcome, BrowserTargetEngineOwnerMismatch>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.validate_current(&handoff.current)?;
        if handoff.current.expected_owner() == Some(&handoff.next) {
            self.selected_owner = Some(handoff.next);
            return Ok(BrowserTargetEngineHandoffOutcome::ReusedSelected);
        }

        if let Some(next) = self.retained.remove(&handoff.next) {
            let previous = std::mem::replace(&mut self.selected, next);
            self.retain_previous_if_requested(&handoff.current, previous);
            self.selected_owner = Some(handoff.next);
            return Ok(BrowserTargetEngineHandoffOutcome::RestoredRetained);
        }

        if handoff.current.owner_to_retain().is_some() {
            let replacement = self.configure_and_wrap(create_replacement());
            let previous = std::mem::replace(&mut self.selected, replacement);
            self.retain_previous_if_requested(&handoff.current, previous);
            self.selected_owner = Some(handoff.next);
            return Ok(BrowserTargetEngineHandoffOutcome::CreatedReplacement);
        }

        // No Page is resident in the current Target, so its selected engine
        // can become the next Target's engine without manufacturing a second
        // renderer owner.
        self.selected_owner = Some(handoff.next);
        Ok(BrowserTargetEngineHandoffOutcome::ReusedSelected)
    }

    pub(super) fn handoff_browser_context_engine<F>(
        &mut self,
        handoff: BrowserContextEngineHandoff,
        create_replacement: F,
    ) -> Result<BrowserTargetEngineHandoffOutcome, BrowserTargetEngineOwnerMismatch>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.validate_current(&handoff.current)?;
        let (next, outcome) = match handoff
            .next
            .as_ref()
            .and_then(|owner| self.retained.remove(owner))
        {
            Some(next) => (next, BrowserTargetEngineHandoffOutcome::RestoredRetained),
            None => (
                self.configure_and_wrap(create_replacement()),
                BrowserTargetEngineHandoffOutcome::CreatedReplacement,
            ),
        };
        let previous = std::mem::replace(&mut self.selected, next);
        self.retain_previous_if_requested(&handoff.current, previous);
        self.selected_owner = handoff.next;
        Ok(outcome)
    }

    pub(super) fn discard_target_page_runtime(&mut self, target_id: &str) {
        self.retained
            .retain(|owner, _| owner.target_id() != target_id);
    }

    pub(super) fn forget_target(&mut self, target_id: &str) {
        self.discard_target_page_runtime(target_id);
        if self
            .selected_owner
            .as_ref()
            .is_some_and(|owner| owner.target_id() == target_id)
        {
            self.selected_owner = None;
        }
    }

    pub(super) fn forget_browser_context(&mut self, browser_context_id: &str) {
        self.retained
            .retain(|owner, _| owner.browser_context_id() != browser_context_id);
        if self
            .selected_owner
            .as_ref()
            .is_some_and(|owner| owner.browser_context_id() == browser_context_id)
        {
            self.selected_owner = None;
        }
    }

    pub(super) fn retire_target(&mut self, owner: &BrowserPageOwnerKey, unbind_selected: bool) {
        self.retained.remove(owner);
        if unbind_selected && self.selected_owner.as_ref() == Some(owner) {
            self.selected_owner = None;
        }
    }

    pub(super) fn retained_count(&self) -> usize {
        self.retained.len()
    }

    pub(super) fn retained_keys(&self) -> impl Iterator<Item = &BrowserPageOwnerKey> {
        self.retained.keys()
    }

    pub(super) fn clone_retained_engine(
        &self,
        owner: &BrowserPageOwnerKey,
    ) -> Option<NavigationEngine> {
        self.retained.get(owner).map(|owner| owner.engine.clone())
    }

    pub(super) fn retained_renderer_owner_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.retained
            .values()
            .map(|owner| owner.engine.renderer_owner_id_for_diagnostics())
    }
}

impl BrowserNavigationOwner {
    pub(super) fn active_engine(&self) -> &NavigationEngine {
        self.target_engines.selected_engine()
    }

    pub(super) fn active_engine_mut(&mut self) -> &mut NavigationEngine {
        self.target_engines.selected_engine_mut()
    }

    pub fn selected_target_engine_owner(&self) -> Option<&BrowserPageOwnerKey> {
        self.target_engines.selected_owner()
    }

    /// Transitional exact-engine access for activity-source routing. The
    /// caller has already resolved the physical active Target; new operations
    /// should be expressed as semantic Browser Owner methods instead.
    pub fn active_engine_for_activity_source_mut(&mut self) -> &mut NavigationEngine {
        self.target_engines.selected_engine_mut()
    }

    /// Transitional exact retained-engine access for activity-source routing.
    pub fn retained_background_engine_mut(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
    ) -> Option<&mut NavigationEngine> {
        self.target_engines
            .retained_engine_mut(&BrowserPageOwnerKey::new(browser_context_id, target_id))
    }

    pub fn set_renderer_output_transport_sender(&mut self, sender: RendererOutputTransportSender) {
        self.target_engines
            .set_renderer_output_transport_sender(sender);
    }

    pub fn configure_detached_engine(&self, engine: &NavigationEngine) {
        self.target_engines.configure_detached_engine(engine);
    }

    pub fn install_unbound_engine(
        &mut self,
        engine: NavigationEngine,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        self.target_engines.install_unbound_engine(engine)
    }

    pub fn adopt_target_engine(
        &mut self,
        owner: BrowserPageOwnerKey,
        residence: BrowserTargetEngineResidence,
        engine: NavigationEngine,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        self.target_engines
            .adopt_target_engine(owner, residence, engine)
    }

    /// Adopts a loaded engine for one exact registered Target.
    ///
    /// Browser Core derives selected/retained residence from its own context
    /// and Target topology in the same owner call. Protocol cannot choose a
    /// residence from a projection and race a later engine-registry mutation.
    pub fn adopt_registered_target_engine(
        &mut self,
        owner: BrowserPageOwnerKey,
        engine: NavigationEngine,
    ) -> Result<BrowserTargetEngineResidence, BrowserTargetEngineAdoptionError> {
        let target_residence = self.targets.validate_target_owner(&owner)?;
        let selected = target_residence == BrowserTargetResidence::Active
            && self
                .browser_contexts
                .selected()
                .is_some_and(|browser_context_id| {
                    browser_context_id.as_str() == owner.browser_context_id()
                });
        let engine_residence = if selected {
            BrowserTargetEngineResidence::Selected
        } else {
            BrowserTargetEngineResidence::Retained
        };
        self.target_engines
            .adopt_target_engine(owner, engine_residence, engine)?;
        Ok(engine_residence)
    }

    /// Replaces the engine for the Browser-owned selected Target, or the
    /// unbound engine when the selected BrowserContext has no active Target.
    pub fn adopt_selected_target_engine_or_unbound(
        &mut self,
        projected_owner: Option<BrowserPageOwnerKey>,
        engine: NavigationEngine,
    ) -> Result<Option<BrowserTargetEngineResidence>, BrowserTargetEngineAdoptionError> {
        let authoritative_owner =
            if let Some(browser_context_id) = self.browser_contexts.selected().cloned() {
                self.targets
                    .active_target_for_registered_context(&browser_context_id)?
                    .map(|target_id| {
                        BrowserPageOwnerKey::new(browser_context_id.as_str(), target_id.as_str())
                    })
            } else {
                None
            };
        if authoritative_owner != projected_owner {
            return Err(
                BrowserTargetEngineAdoptionError::SelectedTargetProjectionMismatch {
                    authoritative: authoritative_owner,
                    projected: projected_owner,
                },
            );
        }
        let Some(owner) = authoritative_owner else {
            self.target_engines.install_unbound_engine(engine)?;
            return Ok(None);
        };
        self.adopt_registered_target_engine(owner, engine).map(Some)
    }

    pub fn handoff_target_engine<F>(
        &mut self,
        handoff: BrowserTargetEngineHandoff,
        create_replacement: F,
    ) -> Result<BrowserTargetEngineHandoffOutcome, BrowserTargetEngineOwnerMismatch>
    where
        F: FnOnce() -> NavigationEngine,
    {
        self.target_engines
            .handoff_target_engine(handoff, create_replacement)
    }

    pub fn retained_background_engine_count(&self) -> usize {
        self.target_engines.retained_count()
    }

    pub fn retained_background_engine_keys(&self) -> impl Iterator<Item = &BrowserPageOwnerKey> {
        self.target_engines.retained_keys()
    }

    pub fn clone_retained_background_engine(
        &self,
        browser_context_id: &str,
        target_id: &str,
    ) -> Option<NavigationEngine> {
        self.target_engines
            .clone_retained_engine(&BrowserPageOwnerKey::new(browser_context_id, target_id))
    }
}

#[cfg(test)]
mod tests {
    use crate::browser_host::{
        BrowserContextSelectionProjection, BrowserPageResidenceHandle, BrowserTargetHandle,
        BrowserTargetSlotProjection, BrowserTargetTopologyProjection,
    };

    use super::*;

    fn engine() -> NavigationEngine {
        NavigationEngine::new_with_page_vm_document_isolate_for_diagnostics()
    }

    fn owner_with_selected_target() -> (BrowserNavigationOwner, BrowserPageOwnerKey) {
        let key = BrowserPageOwnerKey::new("context-1", "target-a");
        let mut owner = BrowserNavigationOwner::new(engine());
        owner
            .register_browser_context(
                key.browser_context_id(),
                BrowserTargetTopologyProjection::new(
                    key.browser_context_id(),
                    Some(BrowserTargetSlotProjection::new(
                        BrowserTargetHandle::staged(key.target_id()),
                        BrowserPageResidenceHandle::default(),
                    )),
                    Vec::<BrowserTargetSlotProjection>::new(),
                ),
                BrowserContextSelectionProjection::new(
                    None,
                    BrowserSelectedTargetEngineDisposition::Unbound,
                ),
                engine,
            )
            .expect("test BrowserContext should register");
        (owner, key)
    }

    #[test]
    fn registered_engine_adoption_rejects_owner_divergence_without_mutation() {
        let (mut owner, target_a) = owner_with_selected_target();
        let divergent = BrowserPageOwnerKey::new("context-1", "target-b");
        owner.target_engines.selected_owner = Some(divergent.clone());
        let selected_renderer_owner = owner.active_renderer_owner_id_for_diagnostics();

        let error = owner
            .adopt_registered_target_engine(target_a.clone(), engine())
            .expect_err("divergent selected engine owner must reject adoption");

        assert_eq!(
            error,
            BrowserTargetEngineAdoptionError::EngineOwner(BrowserTargetEngineOwnerMismatch {
                selected: Some(divergent.clone()),
                requested: Some(target_a),
            })
        );
        assert_eq!(owner.selected_target_engine_owner(), Some(&divergent));
        assert_eq!(
            owner.active_renderer_owner_id_for_diagnostics(),
            selected_renderer_owner,
            "failed adoption must preserve the selected engine payload"
        );
        assert_eq!(owner.retained_background_engine_count(), 0);
    }

    #[test]
    fn target_handoff_parks_loaded_current_and_restores_exact_retained_engine() {
        let mut owner = BrowserNavigationOwner::new(engine());
        let target_a = BrowserPageOwnerKey::new("context-1", "target-a");
        let target_b = BrowserPageOwnerKey::new("context-1", "target-b");
        owner
            .adopt_target_engine(
                target_a.clone(),
                BrowserTargetEngineResidence::Selected,
                engine(),
            )
            .expect("target A should bind selected engine");
        let target_a_renderer_owner = owner.active_renderer_owner_id_for_diagnostics();
        owner
            .adopt_target_engine(
                target_b.clone(),
                BrowserTargetEngineResidence::Retained,
                engine(),
            )
            .expect("target B should retain its engine");
        let target_b_renderer_owner = owner
            .retained_renderer_owner_ids_for_diagnostics()
            .next()
            .expect("retained target B renderer owner");

        let outcome = owner
            .handoff_target_engine(
                BrowserTargetEngineHandoff::new(
                    BrowserSelectedTargetEngineDisposition::Retain(target_a.clone()),
                    target_b.clone(),
                )
                .expect("test targets share one BrowserContext"),
                engine,
            )
            .expect("exact target handoff should commit");

        assert_eq!(outcome, BrowserTargetEngineHandoffOutcome::RestoredRetained);
        assert_eq!(owner.selected_target_engine_owner(), Some(&target_b));
        assert_eq!(
            owner.active_renderer_owner_id_for_diagnostics(),
            target_b_renderer_owner
        );
        assert_eq!(owner.retained_background_engine_count(), 1);
        assert_eq!(
            owner.retained_renderer_owner_ids_for_diagnostics().next(),
            Some(target_a_renderer_owner)
        );
    }

    #[test]
    fn target_handoff_reuses_unloaded_current_without_creating_replacement() {
        let mut owner = BrowserNavigationOwner::new(engine());
        let target_a = BrowserPageOwnerKey::new("context-1", "target-a");
        let target_b = BrowserPageOwnerKey::new("context-1", "target-b");
        owner
            .adopt_target_engine(
                target_a.clone(),
                BrowserTargetEngineResidence::Selected,
                engine(),
            )
            .expect("target A should bind selected engine");
        let renderer_owner = owner.active_renderer_owner_id_for_diagnostics();
        let mut replacement_created = false;

        let outcome = owner
            .handoff_target_engine(
                BrowserTargetEngineHandoff::new(
                    BrowserSelectedTargetEngineDisposition::Discard(target_a),
                    target_b.clone(),
                )
                .expect("test targets share one BrowserContext"),
                || {
                    replacement_created = true;
                    engine()
                },
            )
            .expect("unloaded target handoff should commit");

        assert_eq!(outcome, BrowserTargetEngineHandoffOutcome::ReusedSelected);
        assert!(!replacement_created);
        assert_eq!(owner.selected_target_engine_owner(), Some(&target_b));
        assert_eq!(
            owner.active_renderer_owner_id_for_diagnostics(),
            renderer_owner
        );
        assert_eq!(owner.retained_background_engine_count(), 0);
    }

    #[test]
    fn target_handoff_type_rejects_cross_context_engine_reuse() {
        let current = BrowserPageOwnerKey::new("context-a", "target-a");
        let next = BrowserPageOwnerKey::new("context-b", "target-b");

        let error = BrowserTargetEngineHandoff::new(
            BrowserSelectedTargetEngineDisposition::Discard(current.clone()),
            next.clone(),
        )
        .expect_err("same-context handoff must not authorize cross-context engine reuse");

        assert_eq!(error.current(), &current);
        assert_eq!(error.next(), &next);
    }

    #[test]
    fn stale_handoff_cannot_move_another_targets_engine() {
        let mut owner = BrowserNavigationOwner::new(engine());
        let target_a = BrowserPageOwnerKey::new("context-1", "target-a");
        let target_b = BrowserPageOwnerKey::new("context-1", "target-b");
        let target_c = BrowserPageOwnerKey::new("context-1", "target-c");
        owner
            .adopt_target_engine(
                target_b.clone(),
                BrowserTargetEngineResidence::Selected,
                engine(),
            )
            .expect("target B should bind selected engine");
        let renderer_owner = owner.active_renderer_owner_id_for_diagnostics();

        let error = owner
            .handoff_target_engine(
                BrowserTargetEngineHandoff::new(
                    BrowserSelectedTargetEngineDisposition::Retain(target_a.clone()),
                    target_c,
                )
                .expect("test targets share one BrowserContext"),
                engine,
            )
            .expect_err("stale target A handoff must be rejected");

        assert_eq!(error.selected(), Some(&target_b));
        assert_eq!(error.requested(), Some(&target_a));
        assert_eq!(owner.selected_target_engine_owner(), Some(&target_b));
        assert_eq!(
            owner.active_renderer_owner_id_for_diagnostics(),
            renderer_owner
        );
        assert_eq!(owner.retained_background_engine_count(), 0);
    }

    #[test]
    fn unbound_engine_cannot_authorize_a_handoff_that_claims_a_current_target() {
        let mut owner = BrowserNavigationOwner::new(engine());
        let target_a = BrowserPageOwnerKey::new("context-1", "target-a");
        let target_b = BrowserPageOwnerKey::new("context-1", "target-b");
        let renderer_owner = owner.active_renderer_owner_id_for_diagnostics();

        let error = owner
            .handoff_target_engine(
                BrowserTargetEngineHandoff::new(
                    BrowserSelectedTargetEngineDisposition::Discard(target_a.clone()),
                    target_b,
                )
                .expect("test targets share one BrowserContext"),
                engine,
            )
            .expect_err("unbound must not act as a wildcard current owner");

        assert_eq!(error.selected(), None);
        assert_eq!(error.requested(), Some(&target_a));
        assert_eq!(owner.selected_target_engine_owner(), None);
        assert_eq!(
            owner.active_renderer_owner_id_for_diagnostics(),
            renderer_owner
        );
    }
}
