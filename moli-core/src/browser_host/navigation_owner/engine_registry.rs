use crate::{RendererOutputTransportSender, runtime::NavigationEngine};

use super::{
    BrowserNavigationOwner, BrowserPageOwnerKey, BrowserTargetRegistryError,
    BrowserTargetResidence, target_runtime_registry::BrowserTargetRuntimeRegistry,
};

/// Strong owner of one renderer/navigation runtime.
///
/// Runtime work stays behind semantic operations on `BrowserNavigationOwner`;
/// frontend adapters may transfer an engine into the registry, but cannot
/// borrow the selected engine back out.
pub(super) struct BrowserPageOwner {
    pub(super) engine: NavigationEngine,
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

/// Browser-owned coordinator for unbound and per-Target NavigationEngines.
///
/// Target-backed engines live in `BrowserTargetRuntimeRegistry`; selection is
/// derived from BrowserContext and Target topology. Only the startup/empty-
/// Context engine has no Target runtime record.
pub(super) struct BrowserTargetEngineRegistry {
    unbound: Option<BrowserPageOwner>,
    renderer_output_transport_sender: Option<RendererOutputTransportSender>,
}

impl BrowserTargetEngineRegistry {
    pub(super) fn new(engine: NavigationEngine) -> Self {
        Self {
            unbound: Some(BrowserPageOwner::new(engine)),
            renderer_output_transport_sender: None,
        }
    }

    pub(super) fn selected_engine<'a>(
        &'a self,
        runtimes: &'a BrowserTargetRuntimeRegistry,
        selected_owner: Option<&BrowserPageOwnerKey>,
    ) -> &'a NavigationEngine {
        if let Some(owner) = selected_owner {
            return &runtimes
                .entries
                .get(owner)
                .and_then(|runtime| runtime.engine.as_ref())
                .expect("selected Target topology must own an engine")
                .engine;
        }
        &self
            .unbound
            .as_ref()
            .expect("unbound selection must own an engine")
            .engine
    }

    pub(super) fn selected_engine_mut<'a>(
        &'a mut self,
        runtimes: &'a mut BrowserTargetRuntimeRegistry,
        selected_owner: Option<&BrowserPageOwnerKey>,
    ) -> &'a mut NavigationEngine {
        if let Some(owner) = selected_owner {
            return &mut runtimes
                .entries
                .get_mut(owner)
                .and_then(|runtime| runtime.engine.as_mut())
                .expect("selected Target topology must own an engine")
                .engine;
        }
        &mut self
            .unbound
            .as_mut()
            .expect("unbound selection must own an engine")
            .engine
    }

    pub(super) fn retained_engine_mut<'a>(
        &mut self,
        runtimes: &'a mut BrowserTargetRuntimeRegistry,
        selected_owner: Option<&BrowserPageOwnerKey>,
        owner: &BrowserPageOwnerKey,
    ) -> Option<&'a mut NavigationEngine> {
        if selected_owner == Some(owner) {
            return None;
        }
        runtimes
            .entries
            .get_mut(owner)
            .and_then(|runtime| runtime.engine.as_mut())
            .map(|owner| &mut owner.engine)
    }

    pub(super) fn set_renderer_output_transport_sender(
        &mut self,
        runtimes: &BrowserTargetRuntimeRegistry,
        sender: RendererOutputTransportSender,
    ) {
        self.renderer_output_transport_sender = Some(sender.clone());
        if let Some(unbound) = self.unbound.as_ref() {
            unbound
                .engine
                .set_renderer_output_transport_sender(sender.clone());
        }
        for owner in runtimes
            .entries
            .values()
            .filter_map(|runtime| runtime.engine.as_ref())
        {
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
        selected_owner: Option<&BrowserPageOwnerKey>,
        requested: &BrowserSelectedTargetEngineDisposition,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        let requested_owner = requested.expected_owner();
        if selected_owner == requested_owner {
            return Ok(());
        }
        Err(BrowserTargetEngineOwnerMismatch {
            selected: selected_owner.cloned(),
            requested: requested_owner.cloned(),
        })
    }

    fn clear_discarded_current(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        disposition: &BrowserSelectedTargetEngineDisposition,
    ) {
        match disposition {
            BrowserSelectedTargetEngineDisposition::Unbound => {
                self.unbound = None;
            }
            BrowserSelectedTargetEngineDisposition::Discard(owner) => {
                if let Some(runtime) = runtimes.entries.get_mut(owner) {
                    runtime.engine = None;
                }
            }
            BrowserSelectedTargetEngineDisposition::Retain(_) => {}
        }
    }

    fn take_reusable_current(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        disposition: &BrowserSelectedTargetEngineDisposition,
    ) -> BrowserPageOwner {
        match disposition {
            BrowserSelectedTargetEngineDisposition::Unbound => self
                .unbound
                .take()
                .expect("unbound engine handoff must own its runtime"),
            BrowserSelectedTargetEngineDisposition::Discard(owner) => runtimes
                .entries
                .get_mut(owner)
                .and_then(|runtime| runtime.engine.take())
                .expect("discardable selected Target must own its runtime"),
            BrowserSelectedTargetEngineDisposition::Retain(_) => {
                unreachable!("retained engine cannot be moved into its successor")
            }
        }
    }

    pub(super) fn install_unbound_engine(
        &mut self,
        selected_owner: Option<&BrowserPageOwnerKey>,
        engine: NavigationEngine,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        if selected_owner.is_some() {
            return Err(BrowserTargetEngineOwnerMismatch {
                selected: selected_owner.cloned(),
                requested: None,
            });
        }
        self.unbound = Some(self.configure_and_wrap(engine));
        Ok(())
    }

    pub(super) fn adopt_target_engine(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        selected_owner: Option<&BrowserPageOwnerKey>,
        owner: BrowserPageOwnerKey,
        residence: BrowserTargetEngineResidence,
        engine: NavigationEngine,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        let engine = self.configure_and_wrap(engine);
        match residence {
            BrowserTargetEngineResidence::Selected => {
                if selected_owner != Some(&owner) {
                    return Err(BrowserTargetEngineOwnerMismatch {
                        selected: selected_owner.cloned(),
                        requested: Some(owner),
                    });
                }
                runtimes.entries.entry(owner).or_default().engine = Some(engine);
                self.unbound = None;
            }
            BrowserTargetEngineResidence::Retained => {
                if selected_owner == Some(&owner) {
                    return Err(BrowserTargetEngineOwnerMismatch {
                        selected: selected_owner.cloned(),
                        requested: None,
                    });
                }
                runtimes.entries.entry(owner).or_default().engine = Some(engine);
            }
        }
        Ok(())
    }

    pub(super) fn handoff_target_engine<F>(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        selected_owner: Option<&BrowserPageOwnerKey>,
        handoff: BrowserTargetEngineHandoff,
        create_replacement: F,
    ) -> Result<BrowserTargetEngineHandoffOutcome, BrowserTargetEngineOwnerMismatch>
    where
        F: FnOnce() -> NavigationEngine,
    {
        Self::validate_current(selected_owner, &handoff.current)?;
        if handoff.current.expected_owner() == Some(&handoff.next) {
            return Ok(BrowserTargetEngineHandoffOutcome::ReusedSelected);
        }

        if runtimes
            .entries
            .get(&handoff.next)
            .is_some_and(|runtime| runtime.engine.is_some())
        {
            self.clear_discarded_current(runtimes, &handoff.current);
            self.unbound = None;
            runtimes.prune_empty();
            return Ok(BrowserTargetEngineHandoffOutcome::RestoredRetained);
        }

        if handoff.current.owner_to_retain().is_some() {
            let replacement = self.configure_and_wrap(create_replacement());
            runtimes.entries.entry(handoff.next).or_default().engine = Some(replacement);
            self.unbound = None;
            return Ok(BrowserTargetEngineHandoffOutcome::CreatedReplacement);
        }

        // No Page is resident in the current Target, so its selected engine
        // can become the next Target's engine without manufacturing a second
        // renderer owner.
        let previous = self.take_reusable_current(runtimes, &handoff.current);
        runtimes.entries.entry(handoff.next).or_default().engine = Some(previous);
        runtimes.prune_empty();
        Ok(BrowserTargetEngineHandoffOutcome::ReusedSelected)
    }

    pub(super) fn handoff_browser_context_engine<F>(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        selected_owner: Option<&BrowserPageOwnerKey>,
        handoff: BrowserContextEngineHandoff,
        create_replacement: F,
    ) -> Result<BrowserTargetEngineHandoffOutcome, BrowserTargetEngineOwnerMismatch>
    where
        F: FnOnce() -> NavigationEngine,
    {
        Self::validate_current(selected_owner, &handoff.current)?;
        let next_is_retained = handoff
            .next
            .as_ref()
            .and_then(|owner| runtimes.entries.get(owner))
            .is_some_and(|runtime| runtime.engine.is_some());
        let replacement =
            (!next_is_retained).then(|| self.configure_and_wrap(create_replacement()));
        self.clear_discarded_current(runtimes, &handoff.current);
        match handoff.next {
            Some(next) => {
                if let Some(replacement) = replacement {
                    runtimes.entries.entry(next).or_default().engine = Some(replacement);
                }
                self.unbound = None;
            }
            None => {
                self.unbound = replacement;
            }
        }
        runtimes.prune_empty();
        let outcome = if next_is_retained {
            BrowserTargetEngineHandoffOutcome::RestoredRetained
        } else {
            BrowserTargetEngineHandoffOutcome::CreatedReplacement
        };
        Ok(outcome)
    }

    pub(super) fn discard_target_page_runtime(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        selected_owner: Option<&BrowserPageOwnerKey>,
        target_id: &str,
    ) {
        for (owner, runtime) in &mut runtimes.entries {
            if owner.target_id() == target_id && selected_owner != Some(owner) {
                runtime.engine = None;
            }
        }
        runtimes.prune_empty();
    }

    pub(super) fn retire_target(
        &mut self,
        runtimes: &mut BrowserTargetRuntimeRegistry,
        selected_owner: Option<&BrowserPageOwnerKey>,
        owner: &BrowserPageOwnerKey,
        unbind_selected: bool,
    ) {
        if let Some(runtime) = runtimes.entries.get_mut(owner) {
            if unbind_selected && selected_owner == Some(owner) {
                if let Some(engine) = runtime.engine.take() {
                    self.unbound = Some(engine);
                }
            } else if selected_owner != Some(owner) {
                runtime.engine = None;
            }
        }
        runtimes.prune_empty();
    }

    pub(super) fn retained_count(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        selected_owner: Option<&BrowserPageOwnerKey>,
    ) -> usize {
        self.retained_keys(runtimes, selected_owner).count()
    }

    pub(super) fn retained_keys<'a>(
        &self,
        runtimes: &'a BrowserTargetRuntimeRegistry,
        selected_owner: Option<&'a BrowserPageOwnerKey>,
    ) -> impl Iterator<Item = &'a BrowserPageOwnerKey> {
        runtimes.entries.iter().filter_map(move |(owner, runtime)| {
            (runtime.engine.is_some() && selected_owner != Some(owner)).then_some(owner)
        })
    }

    pub(super) fn clone_retained_engine(
        &self,
        runtimes: &BrowserTargetRuntimeRegistry,
        selected_owner: Option<&BrowserPageOwnerKey>,
        owner: &BrowserPageOwnerKey,
    ) -> Option<NavigationEngine> {
        if selected_owner == Some(owner) {
            return None;
        }
        runtimes
            .entries
            .get(owner)
            .and_then(|runtime| runtime.engine.as_ref())
            .map(|owner| owner.engine.clone())
    }

    pub(super) fn retained_renderer_owner_ids<'a>(
        &'a self,
        runtimes: &'a BrowserTargetRuntimeRegistry,
        selected_owner: Option<&'a BrowserPageOwnerKey>,
    ) -> impl Iterator<Item = u64> + 'a {
        self.retained_keys(runtimes, selected_owner)
            .filter_map(|owner| runtimes.entries.get(owner))
            .filter_map(|runtime| runtime.engine.as_ref())
            .map(|owner| owner.engine.renderer_owner_id_for_diagnostics())
    }
}

impl BrowserNavigationOwner {
    pub(super) fn active_engine(&self) -> &NavigationEngine {
        self.target_engines
            .selected_engine(&self.target_runtimes, self.selected_target_engine_owner())
    }

    pub(super) fn active_engine_mut(&mut self) -> &mut NavigationEngine {
        let selected_owner = self.selected_target_engine_owner().cloned();
        self.target_engines
            .selected_engine_mut(&mut self.target_runtimes, selected_owner.as_ref())
    }

    pub fn selected_target_engine_owner(&self) -> Option<&BrowserPageOwnerKey> {
        let browser_context_id = self.browser_contexts.selected()?;
        let target_id = self.targets.active_target(browser_context_id)?;
        self.target_runtimes.entries.keys().find(|owner| {
            owner.browser_context_id() == browser_context_id.as_str()
                && owner.target_id() == target_id.as_str()
        })
    }

    /// Transitional exact-engine access for activity-source routing. The
    /// caller has already resolved the physical active Target; new operations
    /// should be expressed as semantic Browser Owner methods instead.
    pub fn active_engine_for_activity_source_mut(&mut self) -> &mut NavigationEngine {
        self.active_engine_mut()
    }

    /// Transitional exact retained-engine access for activity-source routing.
    pub fn retained_background_engine_mut(
        &mut self,
        browser_context_id: &str,
        target_id: &str,
    ) -> Option<&mut NavigationEngine> {
        let selected_owner = self.selected_target_engine_owner().cloned();
        self.target_engines.retained_engine_mut(
            &mut self.target_runtimes,
            selected_owner.as_ref(),
            &BrowserPageOwnerKey::new(browser_context_id, target_id),
        )
    }

    pub fn set_renderer_output_transport_sender(&mut self, sender: RendererOutputTransportSender) {
        self.target_engines
            .set_renderer_output_transport_sender(&self.target_runtimes, sender);
    }

    pub fn configure_detached_engine(&self, engine: &NavigationEngine) {
        self.target_engines.configure_detached_engine(engine);
    }

    pub fn install_unbound_engine(
        &mut self,
        engine: NavigationEngine,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        let selected_owner = self.selected_target_engine_owner().cloned();
        self.target_engines
            .install_unbound_engine(selected_owner.as_ref(), engine)
    }

    pub fn adopt_target_engine(
        &mut self,
        owner: BrowserPageOwnerKey,
        residence: BrowserTargetEngineResidence,
        engine: NavigationEngine,
    ) -> Result<(), BrowserTargetEngineOwnerMismatch> {
        let selected_owner = self.selected_target_engine_owner().cloned();
        self.target_engines.adopt_target_engine(
            &mut self.target_runtimes,
            selected_owner.as_ref(),
            owner,
            residence,
            engine,
        )
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
        self.adopt_target_engine(owner, engine_residence, engine)?;
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
            self.install_unbound_engine(engine)?;
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
        let selected_owner = self.selected_target_engine_owner().cloned();
        self.target_engines.handoff_target_engine(
            &mut self.target_runtimes,
            selected_owner.as_ref(),
            handoff,
            create_replacement,
        )
    }

    pub fn retained_background_engine_count(&self) -> usize {
        self.target_engines
            .retained_count(&self.target_runtimes, self.selected_target_engine_owner())
    }

    pub fn retained_background_engine_keys(&self) -> impl Iterator<Item = &BrowserPageOwnerKey> {
        self.target_engines
            .retained_keys(&self.target_runtimes, self.selected_target_engine_owner())
    }

    pub fn clone_retained_background_engine(
        &self,
        browser_context_id: &str,
        target_id: &str,
    ) -> Option<NavigationEngine> {
        self.target_engines.clone_retained_engine(
            &self.target_runtimes,
            self.selected_target_engine_owner(),
            &BrowserPageOwnerKey::new(browser_context_id, target_id),
        )
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

    fn topology_for(
        owner: &BrowserNavigationOwner,
        active: &BrowserPageOwnerKey,
        background: &[&BrowserPageOwnerKey],
    ) -> BrowserTargetTopologyProjection {
        let slot = |key: &BrowserPageOwnerKey| {
            BrowserTargetSlotProjection::new(
                owner
                    .target_handle(key.target_id())
                    .expect("test Target handle"),
                owner
                    .page_residence_handle(key)
                    .expect("test Page residence handle"),
            )
        };
        BrowserTargetTopologyProjection::new(
            active.browser_context_id(),
            Some(slot(active)),
            background.iter().map(|key| slot(key)).collect::<Vec<_>>(),
        )
    }

    fn register_background_target(
        owner: &mut BrowserNavigationOwner,
        active: &BrowserPageOwnerKey,
        background: &BrowserPageOwnerKey,
    ) {
        let projection = topology_for(owner, active, &[]);
        owner
            .register_background_target(
                background.browser_context_id(),
                background.target_id(),
                projection,
            )
            .expect("test background Target should register");
    }

    #[test]
    fn registered_engine_adoption_rejects_non_selected_owner_without_mutation() {
        let (mut owner, target_a) = owner_with_selected_target();
        let divergent = BrowserPageOwnerKey::new("context-1", "target-b");
        let selected_renderer_owner = owner.active_renderer_owner_id_for_diagnostics();

        let error = owner
            .adopt_target_engine(
                divergent.clone(),
                BrowserTargetEngineResidence::Selected,
                engine(),
            )
            .expect_err("topology must reject a different selected engine owner");

        assert_eq!(
            error,
            BrowserTargetEngineOwnerMismatch {
                selected: Some(target_a.clone()),
                requested: Some(divergent),
            }
        );
        assert_eq!(owner.selected_target_engine_owner(), Some(&target_a));
        assert_eq!(
            owner.active_renderer_owner_id_for_diagnostics(),
            selected_renderer_owner,
            "failed adoption must preserve the selected engine payload"
        );
        assert_eq!(owner.retained_background_engine_count(), 0);
    }

    #[test]
    fn target_handoff_parks_loaded_current_and_restores_exact_retained_engine() {
        let (mut owner, target_a) = owner_with_selected_target();
        let target_b = BrowserPageOwnerKey::new("context-1", "target-b");
        owner
            .adopt_target_engine(
                target_a.clone(),
                BrowserTargetEngineResidence::Selected,
                engine(),
            )
            .expect("target A should bind selected engine");
        let target_a_renderer_owner = owner.active_renderer_owner_id_for_diagnostics();
        register_background_target(&mut owner, &target_a, &target_b);
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

        let projection = topology_for(&owner, &target_a, &[&target_b]);
        let activation = owner
            .activate_target(
                target_b.browser_context_id(),
                target_b.target_id(),
                projection,
                BrowserContextSelectionProjection::new(
                    Some(target_a.browser_context_id().to_owned()),
                    BrowserSelectedTargetEngineDisposition::Retain(target_a.clone()),
                ),
                engine,
            )
            .expect("exact target handoff should commit");

        assert_eq!(
            activation.engine_outcome(),
            Some(BrowserTargetEngineHandoffOutcome::RestoredRetained)
        );
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
        let (mut owner, target_a) = owner_with_selected_target();
        let target_b = BrowserPageOwnerKey::new("context-1", "target-b");
        owner
            .adopt_target_engine(
                target_a.clone(),
                BrowserTargetEngineResidence::Selected,
                engine(),
            )
            .expect("target A should bind selected engine");
        let renderer_owner = owner.active_renderer_owner_id_for_diagnostics();
        register_background_target(&mut owner, &target_a, &target_b);
        let mut replacement_created = false;

        let projection = topology_for(&owner, &target_a, &[&target_b]);
        let activation = owner
            .activate_target(
                target_b.browser_context_id(),
                target_b.target_id(),
                projection,
                BrowserContextSelectionProjection::new(
                    Some(target_a.browser_context_id().to_owned()),
                    BrowserSelectedTargetEngineDisposition::Discard(target_a),
                ),
                || {
                    replacement_created = true;
                    engine()
                },
            )
            .expect("unloaded target handoff should commit");

        assert_eq!(
            activation.engine_outcome(),
            Some(BrowserTargetEngineHandoffOutcome::ReusedSelected)
        );
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
        let (mut owner, selected) = owner_with_selected_target();
        let target_a = BrowserPageOwnerKey::new("context-1", "target-stale");
        let target_c = BrowserPageOwnerKey::new("context-1", "target-c");
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

        assert_eq!(error.selected(), Some(&selected));
        assert_eq!(error.requested(), Some(&target_a));
        assert_eq!(owner.selected_target_engine_owner(), Some(&selected));
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
