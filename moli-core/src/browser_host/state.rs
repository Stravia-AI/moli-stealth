use std::{
    cell::{Ref, RefCell, RefMut},
    fmt,
    rc::Rc,
};

use crate::runtime::NavigationEngine;
use crate::runtime::{
    RendererBrowserContextRuntimeOwner, RendererBrowserContextRuntimeOwnerAccess,
};

use super::context_runtime_registry::BrowserContextRuntimeRegistry;
use super::identity_allocator::BrowserHostIdentityState;
use super::{
    BrowserCommandId, BrowserContextHandle, BrowserContextRegistration,
    BrowserContextRegistrationMetadata, BrowserContextRemoval, BrowserContextRemovalPermit,
    BrowserContextRuntimeRegistryError, BrowserContextSelectionProjection, BrowserDownloadPolicy,
    BrowserDownloadPolicyState, BrowserDownloadPolicyUpdate, BrowserDownloadRegistry,
    BrowserFactSubscriber, BrowserFactWakeSubscriber, BrowserHostNetworkPolicySnapshot,
    BrowserHostPolicyState, BrowserHostPolicyUpdate, BrowserNavigationOwner,
    BrowserNetworkArtifactStore, BrowserTargetIdAllocator, BrowserTargetTopologyProjection,
};

/// Application-owned residence for authoritative Browser Host state.
///
/// The renderer runtime is deliberately single-threaded, so this residence is
/// `Rc`/`RefCell` rather than a misleading cross-thread mutex. Protocol
/// adapters may clone this capability, but the authoritative registries,
/// navigation engines and fact journal live in one shared Host allocation and
/// therefore are not embedded in a frontend connection.
#[derive(Clone)]
pub struct BrowserHostState {
    inner: Rc<BrowserHostStateInner>,
}

struct BrowserHostStateInner {
    navigation_owner: RefCell<Option<BrowserNavigationOwner>>,
    policy: RefCell<BrowserHostPolicyState>,
    download_policy: RefCell<BrowserDownloadPolicyState>,
    download_registry: BrowserDownloadRegistry,
    network_artifacts: BrowserNetworkArtifactStore,
    identities: BrowserHostIdentityState,
    renderer_runtime_roots: RefCell<BrowserContextRuntimeRegistry>,
}

impl Drop for BrowserHostStateInner {
    fn drop(&mut self) {
        let roots = self.renderer_runtime_roots.get_mut();
        roots.terminate_renderer_producers();

        // Navigation engines own the last Page/renderer leases. Release them
        // before the BrowserContext roots close fetch admission and join their
        // semantic/curl owners.
        drop(self.navigation_owner.get_mut().take());

        roots.shutdown_network_and_join();
    }
}

impl fmt::Debug for BrowserHostState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserHostState")
            .finish_non_exhaustive()
    }
}

impl BrowserHostState {
    pub fn new(active_engine: NavigationEngine) -> Self {
        Self::new_with_target_id_allocator(active_engine, BrowserTargetIdAllocator::default())
    }

    pub fn new_with_target_id_allocator(
        active_engine: NavigationEngine,
        target_id_allocator: BrowserTargetIdAllocator,
    ) -> Self {
        let policy = BrowserHostPolicyState::from_fetch_config(active_engine.fetch_config());
        Self {
            inner: Rc::new(BrowserHostStateInner {
                navigation_owner: RefCell::new(Some(BrowserNavigationOwner::new(active_engine))),
                policy: RefCell::new(policy),
                download_policy: RefCell::new(BrowserDownloadPolicyState::default()),
                download_registry: BrowserDownloadRegistry::default(),
                network_artifacts: BrowserNetworkArtifactStore::default(),
                identities: BrowserHostIdentityState::new(target_id_allocator),
                renderer_runtime_roots: RefCell::new(BrowserContextRuntimeRegistry::default()),
            }),
        }
    }

    /// Borrows the authoritative Browser state for one short synchronous
    /// owner operation.
    pub fn navigation_owner(&self) -> Ref<'_, BrowserNavigationOwner> {
        Ref::map(self.inner.navigation_owner.borrow(), |owner| {
            owner
                .as_ref()
                .expect("Browser Host state was already taken for teardown")
        })
    }

    /// Mutably borrows the authoritative Browser state for one short
    /// synchronous owner operation.
    pub fn navigation_owner_mut(&self) -> RefMut<'_, BrowserNavigationOwner> {
        RefMut::map(self.inner.navigation_owner.borrow_mut(), |owner| {
            owner
                .as_mut()
                .expect("Browser Host state was already taken for teardown")
        })
    }

    /// Registers one exact physical BrowserContext runtime root together with
    /// its authoritative Core topology transaction.
    ///
    /// The root is move-owned before the Context handle becomes live. A Core
    /// rejection drops only this uncommitted candidate; a successful commit
    /// installs the root in Browser Host before returning to Protocol.
    pub fn register_browser_context_with_runtime<F>(
        &self,
        browser_context_id: String,
        browser_context_handle: BrowserContextHandle,
        registration_metadata: BrowserContextRegistrationMetadata,
        target_topology: BrowserTargetTopologyProjection,
        selection_projection: BrowserContextSelectionProjection,
        renderer_runtime_owner: RendererBrowserContextRuntimeOwner,
        create_replacement: F,
    ) -> Result<BrowserContextRegistration, BrowserContextRuntimeRegistryError>
    where
        F: FnOnce(RendererBrowserContextRuntimeOwnerAccess) -> NavigationEngine,
    {
        self.inner.renderer_runtime_roots.borrow_mut().register(
            browser_context_handle.clone(),
            renderer_runtime_owner,
            |renderer_runtime_access| {
                self.navigation_owner_mut()
                    .register_browser_context_with_handle_and_metadata(
                        browser_context_id,
                        browser_context_handle,
                        registration_metadata,
                        target_topology,
                        selection_projection,
                        || create_replacement(renderer_runtime_access),
                    )
            },
        )
    }

    /// Commits one exact Context removal and returns its unique renderer and
    /// network runtime root in the same synchronous Browser Host turn.
    pub fn commit_browser_context_removal_with_runtime<F>(
        &self,
        permit: BrowserContextRemovalPermit,
        projection: BrowserContextSelectionProjection,
        create_unbound_replacement: F,
    ) -> Result<
        (BrowserContextRemoval, RendererBrowserContextRuntimeOwner),
        BrowserContextRuntimeRegistryError,
    >
    where
        F: FnOnce() -> NavigationEngine,
    {
        let browser_context_handle = permit.browser_context_handle().clone();
        self.inner
            .renderer_runtime_roots
            .borrow_mut()
            .remove(browser_context_handle, || {
                self.navigation_owner_mut().commit_browser_context_removal(
                    permit,
                    projection,
                    create_unbound_replacement,
                )
            })
    }

    /// Commits selected Context removal plus Core-chosen successor activation
    /// and returns the predecessor's exact runtime root.
    pub fn commit_browser_context_removal_with_successor_runtime<F>(
        &self,
        permit: BrowserContextRemovalPermit,
        projection: BrowserContextSelectionProjection,
        create_replacement: F,
    ) -> Result<
        (BrowserContextRemoval, RendererBrowserContextRuntimeOwner),
        BrowserContextRuntimeRegistryError,
    >
    where
        F: FnOnce() -> NavigationEngine,
    {
        let browser_context_handle = permit.browser_context_handle().clone();
        self.inner
            .renderer_runtime_roots
            .borrow_mut()
            .remove(browser_context_handle, || {
                self.navigation_owner_mut()
                    .commit_browser_context_removal_with_successor(
                        permit,
                        projection,
                        create_replacement,
                    )
            })
    }

    /// Returns a cloneable, non-owning capability for an exact live Context.
    pub fn renderer_runtime_owner_access(
        &self,
        browser_context_handle: &BrowserContextHandle,
    ) -> Option<RendererBrowserContextRuntimeOwnerAccess> {
        self.inner
            .renderer_runtime_roots
            .borrow()
            .owner_access(browser_context_handle)
    }

    #[cfg(test)]
    fn renderer_runtime_root_count_for_test(&self) -> usize {
        self.inner.renderer_runtime_roots.borrow().len()
    }

    pub fn subscribe_browser_facts(&self) -> BrowserFactSubscriber {
        self.navigation_owner().subscribe_browser_facts()
    }

    pub fn subscribe_browser_fact_wake(&self) -> BrowserFactWakeSubscriber {
        self.navigation_owner().subscribe_browser_fact_wake()
    }

    /// Returns an owned snapshot so frontend application work never retains a
    /// mutable Browser Host borrow across renderer or network waits.
    pub fn policy_snapshot(&self) -> BrowserHostPolicyState {
        self.inner.policy.borrow().clone()
    }

    pub fn network_policy_snapshot(&self) -> BrowserHostNetworkPolicySnapshot {
        self.inner.policy.borrow().network_snapshot()
    }

    /// Applies one move-owned mutation in a short synchronous Browser Host
    /// step. No policy borrow or caller-provided callback escapes this method.
    pub fn apply_policy_update(&self, update: BrowserHostPolicyUpdate) {
        self.inner.policy.borrow_mut().apply_update(update);
    }

    pub fn download_policy_snapshot(&self) -> BrowserDownloadPolicyState {
        self.inner.download_policy.borrow().clone()
    }

    pub fn effective_download_policy(
        &self,
        browser_context_id: Option<&str>,
    ) -> BrowserDownloadPolicy {
        self.inner
            .download_policy
            .borrow()
            .effective_for_browser_context(browser_context_id)
    }

    pub fn apply_download_policy_update(&self, update: BrowserDownloadPolicyUpdate) {
        self.inner.download_policy.borrow_mut().apply(update);
    }

    pub fn download_registry(&self) -> BrowserDownloadRegistry {
        self.inner.download_registry.clone()
    }

    pub fn network_artifacts(&self) -> BrowserNetworkArtifactStore {
        self.inner.network_artifacts.clone()
    }

    pub fn allocate_browser_context_sequence(&self) -> u64 {
        self.inner.identities.allocate_browser_context_sequence()
    }

    pub fn allocate_target_sequence(&self) -> u64 {
        self.inner.identities.allocate_target_sequence()
    }

    pub fn allocate_browser_command_id(&self) -> BrowserCommandId {
        self.inner.identities.allocate_browser_command_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        OptionalResourceFetchMask,
        browser_host::{BrowserSelectedTargetEngineDisposition, BrowserTargetSlotProjection},
        runtime::RendererBrowserContextRuntime,
    };
    use moli_fetch::FetchConfig;

    fn context_projection(browser_context_id: Option<&str>) -> BrowserContextSelectionProjection {
        BrowserContextSelectionProjection::new(
            browser_context_id.map(str::to_owned),
            BrowserSelectedTargetEngineDisposition::Unbound,
        )
    }

    fn empty_topology(browser_context_id: &str) -> BrowserTargetTopologyProjection {
        BrowserTargetTopologyProjection::new(
            browser_context_id,
            None,
            Vec::<BrowserTargetSlotProjection>::new(),
        )
    }

    #[test]
    fn application_target_allocator_spans_browser_hosts() {
        let target_ids = BrowserTargetIdAllocator::default();
        let first = BrowserHostState::new_with_target_id_allocator(
            NavigationEngine::new(),
            target_ids.clone(),
        );
        let second =
            BrowserHostState::new_with_target_id_allocator(NavigationEngine::new(), target_ids);

        assert_eq!(first.allocate_target_sequence(), 1);
        assert_eq!(second.allocate_target_sequence(), 2);
    }

    fn engine_for_context(runtime: RendererBrowserContextRuntimeOwnerAccess) -> NavigationEngine {
        NavigationEngine::new_with_fetch_config_and_browser_context_access(
            FetchConfig::default(),
            runtime,
            OptionalResourceFetchMask::NONE,
            true,
        )
        .expect("live BrowserContext root should create a replacement engine")
    }

    #[test]
    fn cloned_residence_keeps_authoritative_state_alive() {
        let application_residence = BrowserHostState::new(NavigationEngine::new());
        let frontend_capability = application_residence.clone();
        let browser_instance_id = frontend_capability.navigation_owner().browser_instance_id();

        drop(frontend_capability);

        assert_eq!(
            application_residence
                .navigation_owner()
                .browser_instance_id(),
            browser_instance_id
        );
    }

    #[test]
    fn cloned_residence_observes_one_browser_policy_state() {
        let application_residence = BrowserHostState::new(NavigationEngine::new());
        let frontend_capability = application_residence.clone();

        frontend_capability
            .apply_policy_update(BrowserHostPolicyUpdate::SetGlobalCacheDisabled(true));
        let mut bounds = frontend_capability
            .policy_snapshot()
            .window_bounds()
            .clone();
        bounds.width = Some(1280);
        frontend_capability
            .apply_policy_update(BrowserHostPolicyUpdate::ReplaceWindowBounds(bounds));
        drop(frontend_capability);

        let policy = application_residence.policy_snapshot();
        assert!(policy.global_cache_disabled());
        assert_eq!(policy.window_bounds().width, Some(1280));
    }

    #[test]
    fn exact_runtime_root_rolls_back_with_context_rejection_and_survives_same_id_reuse() {
        let state = BrowserHostState::new(NavigationEngine::new());
        let first_handle = BrowserContextHandle::staged("context-reused");
        let first_root = RendererBrowserContextRuntime::new();
        let first_runtime_id = first_root.handle().id();

        state
            .register_browser_context_with_runtime(
                "context-reused".to_owned(),
                first_handle.clone(),
                BrowserContextRegistrationMetadata::default(),
                empty_topology("context-reused"),
                context_projection(None),
                first_root,
                engine_for_context,
            )
            .expect("first exact Context and runtime root should register");
        assert_eq!(state.renderer_runtime_root_count_for_test(), 1);
        assert_eq!(
            state
                .renderer_runtime_owner_access(&first_handle)
                .expect("registered exact root")
                .runtime()
                .id(),
            first_runtime_id
        );

        let rejected_permit = state
            .navigation_owner()
            .prepare_browser_context_removal_for_handle(&first_handle)
            .expect("first removal permit");
        let error = state
            .commit_browser_context_removal_with_runtime(
                rejected_permit,
                context_projection(None),
                NavigationEngine::new,
            )
            .expect_err("a stale physical selection must reject Context removal");
        assert!(matches!(
            error,
            BrowserContextRuntimeRegistryError::Context(_)
        ));
        assert_eq!(state.renderer_runtime_root_count_for_test(), 1);
        assert!(first_handle.is_live());
        assert_eq!(
            state
                .renderer_runtime_owner_access(&first_handle)
                .expect("typed rejection must restore the exact runtime root")
                .runtime()
                .id(),
            first_runtime_id
        );

        let permit = state
            .navigation_owner()
            .prepare_browser_context_removal_for_handle(&first_handle)
            .expect("second removal permit");
        let (_, mut retired_first_root) = state
            .commit_browser_context_removal_with_runtime(
                permit,
                context_projection(Some("context-reused")),
                NavigationEngine::new,
            )
            .expect("exact Context and root should retire together");
        assert_eq!(state.renderer_runtime_root_count_for_test(), 0);
        assert!(first_handle.is_retired());
        assert_eq!(retired_first_root.handle().id(), first_runtime_id);

        let replacement_handle = BrowserContextHandle::staged("context-reused");
        let replacement_root = RendererBrowserContextRuntime::new();
        let replacement_runtime_id = replacement_root.handle().id();
        state
            .register_browser_context_with_runtime(
                "context-reused".to_owned(),
                replacement_handle.clone(),
                BrowserContextRegistrationMetadata::default(),
                empty_topology("context-reused"),
                context_projection(None),
                replacement_root,
                engine_for_context,
            )
            .expect("same public id should register as a new exact Context");
        assert_eq!(state.renderer_runtime_root_count_for_test(), 1);
        assert_eq!(
            state
                .renderer_runtime_owner_access(&replacement_handle)
                .expect("replacement exact root")
                .runtime()
                .id(),
            replacement_runtime_id
        );
        assert_ne!(first_runtime_id, replacement_runtime_id);

        retired_first_root.shutdown_and_join();
    }
}
