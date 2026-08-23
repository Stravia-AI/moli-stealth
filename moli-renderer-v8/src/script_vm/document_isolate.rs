use super::{
    inspector::{
        DocumentInspectorBinding, RendererInspectorIsolateBackend,
        RendererInspectorIsolateBackendHandle,
    },
    runtime_bindings::{
        PromiseRejectDispatchSlot, failed_access_check_callback, promise_reject_callback,
        promise_trace_hook,
    },
};
use crate::{
    browsing_context_model::{BrowsingContextGroupId, ScriptAgentId},
    context_bootstrap::{ContextBootstrapAssets, WINDOW_OPENER_SLOT},
    document_runtime::DocumentRuntime,
    exception_reporting::v8_message_listener,
    module_runtime::{
        dynamic_import_callback, dynamic_import_with_phase_callback,
        initialize_import_meta_object_callback,
    },
    native_bridge::bindings::NativeBridgeBindings,
    native_bridge::{
        JsContextHost, JsContextHostBridgeRef, RuntimeObservableContextToken,
        SharedPrebootstrappedChildDefaultContexts,
    },
    page_task_queue::{
        PageRuntimeTaskSource, PageRuntimeWakeSender, PageTaskSender,
        RendererPageV8ForegroundTaskSender,
    },
    resource_owner::ResourceOwnerId,
    runtime::{
        RendererAuxiliaryPageReservationAllocator, RendererPageContextCancelSender,
        RendererStagedAuxiliaryWindowProxy,
    },
    util::{get_private_value, set_private_value},
    v8_platform::{
        RendererScriptAgentPageMembership, RendererScriptAgentV8ForegroundTaskRouter,
        V8ForegroundTaskWake, V8PlatformIsolateRegistration,
    },
};
use anyhow::{Result, anyhow};
use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::HashMap,
    rc::{Rc, Weak},
    sync::atomic::{AtomicU64, Ordering},
};

static DOCUMENT_ISOLATE_CREATED_COUNT: AtomicU64 = AtomicU64::new(0);
static DOCUMENT_ISOLATE_DESTROYED_COUNT: AtomicU64 = AtomicU64::new(0);
static DOCUMENT_ISOLATE_LIVE_COUNT: AtomicU64 = AtomicU64::new(0);
static DOCUMENT_ISOLATE_RESERVED_COUNT: AtomicU64 = AtomicU64::new(0);
static NEXT_SCRIPT_AGENT_ID: AtomicU64 = AtomicU64::new(1);

fn allocate_script_agent_id() -> ScriptAgentId {
    let value = NEXT_SCRIPT_AGENT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .expect("script-agent id allocator overflow");
    ScriptAgentId::new(value)
}

pub(crate) fn renderer_document_isolate_accounting_diagnostics()
-> crate::runtime::RendererDocumentIsolateAccountingDiagnostics {
    crate::runtime::RendererDocumentIsolateAccountingDiagnostics {
        created: DOCUMENT_ISOLATE_CREATED_COUNT.load(Ordering::Relaxed),
        destroyed: DOCUMENT_ISOLATE_DESTROYED_COUNT.load(Ordering::Relaxed),
        live: DOCUMENT_ISOLATE_LIVE_COUNT.load(Ordering::Relaxed),
        reserved: DOCUMENT_ISOLATE_RESERVED_COUNT.load(Ordering::Relaxed),
    }
}

#[derive(Debug)]
pub(crate) struct RendererDocumentIsolateReservationAccounting;

impl RendererDocumentIsolateReservationAccounting {
    pub(crate) fn new() -> Self {
        DOCUMENT_ISOLATE_RESERVED_COUNT.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for RendererDocumentIsolateReservationAccounting {
    fn drop(&mut self) {
        let previous = DOCUMENT_ISOLATE_RESERVED_COUNT.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 0, "document isolate reservation count underflow");
    }
}

struct RendererDocumentIsolateAccountingGuard;

impl RendererDocumentIsolateAccountingGuard {
    fn new() -> Self {
        DOCUMENT_ISOLATE_CREATED_COUNT.fetch_add(1, Ordering::Relaxed);
        DOCUMENT_ISOLATE_LIVE_COUNT.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for RendererDocumentIsolateAccountingGuard {
    fn drop(&mut self) {
        DOCUMENT_ISOLATE_DESTROYED_COUNT.fetch_add(1, Ordering::Relaxed);
        let previous = DOCUMENT_ISOLATE_LIVE_COUNT.fetch_sub(1, Ordering::Relaxed);
        debug_assert!(previous > 0, "document isolate live count underflow");
    }
}

pub(super) struct ScriptVmPageRealmBootstrap {
    pub(super) resource_owner_id: ResourceOwnerId,
    pub(super) promise_reject_dispatch: PromiseRejectDispatchSlot,
    pub(super) page_inspector: DocumentInspectorBinding,
    pub(super) renderer_document_isolate: RendererDocumentIsolateHandle,
    pub(super) renderer_document_isolate_teardown: RendererDocumentIsolateTeardown,
    pub(super) document_runtime: Box<DocumentRuntime>,
    pub(super) root_frame_id: Option<String>,
    pub(super) context_host: Rc<RefCell<JsContextHost>>,
    pub(super) prebootstrapped_child_default_contexts: SharedPrebootstrappedChildDefaultContexts,
    pub(super) page_context_cancel_tx: RendererPageContextCancelSender,
    pub(super) post_domcontentloaded_page_task_tx: PageTaskSender,
    pub(super) page_runtime_wake_tx: PageRuntimeWakeSender,
    pub(super) storage_bucket_store: crate::context_bootstrap::SharedStorageBucketStore,
    pub(super) renderer_page_script_environment: Option<RendererPageScriptEnvironment>,
    pub(super) script_agent_page_membership: Option<RendererScriptAgentPageMembership>,
    pub(super) reuse_main_window_proxy: bool,
}

pub(super) struct ScriptVmContextBootstrap {
    pub(super) context: v8::Global<v8::Context>,
    pub(super) runtime_observable_context_token: RuntimeObservableContextToken,
    pub(super) bridge_ref: JsContextHostBridgeRef,
}

pub(crate) struct RendererDocumentIsolateBootstrap {
    pub(super) renderer_document_isolate: RendererDocumentIsolateHandle,
    pub(super) bridge_bindings: NativeBridgeBindings,
    pub(super) renderer_document_isolate_teardown: RendererDocumentIsolateTeardown,
    pub(super) inspector_isolate_backend: RendererInspectorIsolateBackendHandle,
    pub(super) page_inspector: DocumentInspectorBinding,
    pub(super) script_agent_page_membership: Option<RendererScriptAgentPageMembership>,
    pub(super) renderer_page_script_environment: Option<RendererPageScriptEnvironment>,
    pub(super) reuse_main_window_proxy: bool,
}

impl RendererDocumentIsolateBootstrap {
    pub(crate) fn renderer_devtools_agent_token(
        &self,
    ) -> crate::runtime::RendererDevToolsAgentToken {
        self.page_inspector.agent_token()
    }

    pub(crate) fn clone_renderer_document_isolate_handle_for_owner_retention(
        &self,
    ) -> RendererDocumentIsolateHandle {
        self.renderer_document_isolate.clone()
    }

    pub(crate) fn renderer_page_script_environment(&self) -> Option<RendererPageScriptEnvironment> {
        self.renderer_page_script_environment.clone()
    }

    pub(crate) fn script_agent_page_membership(&self) -> Option<RendererScriptAgentPageMembership> {
        self.script_agent_page_membership.clone()
    }

    pub(crate) fn inspector_isolate_backend_handle(&self) -> RendererInspectorIsolateBackendHandle {
        self.inspector_isolate_backend.clone()
    }

    pub(crate) fn with_renderer_page_script_environment(
        mut self,
        environment: RendererPageScriptEnvironment,
    ) -> Self {
        self.renderer_page_script_environment = Some(environment);
        self
    }

    pub(crate) fn with_page_inspector(mut self, page_inspector: DocumentInspectorBinding) -> Self {
        self.page_inspector = page_inspector;
        self
    }

    pub(crate) fn with_reused_main_window_proxy(mut self) -> Self {
        self.reuse_main_window_proxy = true;
        self
    }
}

#[derive(Clone)]
struct RendererRelatedPageGroup {
    id: BrowsingContextGroupId,
    named_targets: Rc<RefCell<HashMap<String, Vec<Weak<RendererRelatedPageTopLevelTargetState>>>>>,
    /// Related Page order is part of named-frame lookup. Chromium walks every
    /// live related Page's complete frame tree before consulting the next Page,
    /// so a name-indexed top-level map cannot represent this authority alone.
    top_level_targets: Rc<RefCell<Vec<Weak<RendererRelatedPageTopLevelTargetState>>>>,
}

impl Default for RendererRelatedPageGroup {
    fn default() -> Self {
        Self {
            id: BrowsingContextGroupId::allocate(),
            named_targets: Rc::new(RefCell::new(HashMap::new())),
            top_level_targets: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl RendererRelatedPageGroup {
    fn register_target(&self, target: &Rc<RendererRelatedPageTopLevelTargetState>) {
        self.top_level_targets
            .borrow_mut()
            .push(Rc::downgrade(target));
    }

    fn live_targets_in_page_order(&self) -> Vec<Rc<RendererRelatedPageTopLevelTargetState>> {
        let mut live = Vec::new();
        self.top_level_targets.borrow_mut().retain(|candidate| {
            let Some(candidate) = candidate.upgrade() else {
                return false;
            };
            if candidate.lifecycle.get() != RendererTopLevelBrowsingContextLifecycle::Active {
                return false;
            }
            if candidate.is_live() {
                live.push(candidate);
            }
            true
        });
        live
    }

    fn set_target_name(
        &self,
        target: &Rc<RendererRelatedPageTopLevelTargetState>,
        next_name: String,
    ) {
        let previous_name = target.name.replace(next_name.clone());
        if previous_name == next_name {
            return;
        }
        self.unregister_target_name(target, &previous_name);
        if reusable_top_level_browsing_context_name(&next_name)
            && target.lifecycle.get() == RendererTopLevelBrowsingContextLifecycle::Active
        {
            self.named_targets
                .borrow_mut()
                .entry(next_name)
                .or_default()
                .push(Rc::downgrade(target));
        }
    }

    fn unregister_target(&self, target: &Rc<RendererRelatedPageTopLevelTargetState>) {
        let name = target.name.borrow().clone();
        self.unregister_target_name(target, &name);
    }

    fn unregister_target_name(
        &self,
        target: &Rc<RendererRelatedPageTopLevelTargetState>,
        name: &str,
    ) {
        if !reusable_top_level_browsing_context_name(name) {
            return;
        }
        let mut named_targets = self.named_targets.borrow_mut();
        let remove_entry = named_targets.get_mut(name).is_some_and(|targets| {
            targets.retain(|candidate| {
                candidate
                    .upgrade()
                    .is_some_and(|candidate| !Rc::ptr_eq(&candidate, target))
            });
            targets.is_empty()
        });
        if remove_entry {
            named_targets.remove(name);
        }
    }

    fn find_named_target(
        &self,
        source: &Rc<RendererRelatedPageTopLevelTargetState>,
        name: &str,
    ) -> Option<Rc<RendererRelatedPageTopLevelTargetState>> {
        if !reusable_top_level_browsing_context_name(name) {
            return None;
        }
        if source.name.borrow().as_str() == name && source.is_live() {
            return Some(source.clone());
        }

        let mut named_targets = self.named_targets.borrow_mut();
        let mut found = None;
        let remove_entry = named_targets.get_mut(name).is_some_and(|targets| {
            targets.retain(|candidate| {
                let Some(candidate) = candidate.upgrade() else {
                    return false;
                };
                if !candidate.is_live() {
                    return false;
                }
                if found.is_none() {
                    found = Some(candidate);
                }
                true
            });
            targets.is_empty()
        });
        if remove_entry {
            named_targets.remove(name);
        }
        found
    }
}

struct RendererRelatedPageTopLevelTargetState {
    residence: crate::RendererResolvedPopupTarget,
    opened_by_dom: bool,
    global_proxy: OnceCell<v8::Global<v8::Object>>,
    current_default_context: RefCell<Option<v8::Global<v8::Context>>>,
    // Page-scoped opener edge. The value belongs to the stable top-level
    // browsing context rather than to one replaceable LocalWindow realm.
    opener_edge: RefCell<Option<v8::Global<v8::Value>>>,
    lifecycle: Cell<RendererTopLevelBrowsingContextLifecycle>,
    active: Cell<bool>,
    focused: Cell<bool>,
    name: RefCell<String>,
    current_cross_origin_opener_policy:
        RefCell<Option<crate::cross_origin_isolation::TopLevelDocumentCrossOriginOpenerPolicy>>,
}

impl RendererRelatedPageTopLevelTargetState {
    fn is_live(&self) -> bool {
        self.lifecycle.get() == RendererTopLevelBrowsingContextLifecycle::Active
            && self.global_proxy.get().is_some()
            && self.current_default_context.borrow().is_some()
    }
}

fn reusable_top_level_browsing_context_name(name: &str) -> bool {
    !name.is_empty()
        && !name.eq_ignore_ascii_case("_self")
        && !name.eq_ignore_ascii_case("_parent")
        && !name.eq_ignore_ascii_case("_top")
        && !name.eq_ignore_ascii_case("_blank")
}

#[derive(Clone)]
pub(crate) struct RendererPageScriptEnvironment {
    page_id: u64,
    auxiliary_page_reservation_allocator: RendererAuxiliaryPageReservationAllocator,
    renderer_document_isolate: RendererDocumentIsolateHandle,
    inspector_isolate_backend: RendererInspectorIsolateBackendHandle,
    script_agent_page_membership: RendererScriptAgentPageMembership,
    page_runtime_task_source: PageRuntimeTaskSource,
    output_journal: crate::runtime::RendererTurnOutputJournal,
    related_page_group: RendererRelatedPageGroup,
    top_level_target: Rc<RendererRelatedPageTopLevelTargetState>,
    initial_global_proxy_facade_context: Rc<RefCell<Option<v8::Global<v8::Context>>>>,
    initial_global_proxy_security_token: Rc<RefCell<Option<v8::Global<v8::Value>>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RendererTopLevelBrowsingContextLifecycle {
    Active,
    Closing,
    Closed,
    /// A COOP commit replaced this group-visible browsing context with a new
    /// group endpoint. Old-group WindowProxy references stay safely callable
    /// but expose closed/disconnected behavior.
    Disconnected,
}

impl std::fmt::Debug for RendererPageScriptEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RendererPageScriptEnvironment")
            .field("page_id", &self.page_id)
            .field(
                "isolate_identity_key",
                &self.renderer_document_isolate.identity_key(),
            )
            .field("script_agent_id", &self.script_agent_id())
            .field(
                "browsing_context_group_id",
                &self.browsing_context_group_id(),
            )
            .field(
                "runtime_task_source_identity_key",
                &self.page_runtime_task_source.identity_key(),
            )
            .field("output_stream", &self.output_journal.stream())
            .field(
                "has_global_proxy",
                &self.top_level_target.global_proxy.get().is_some(),
            )
            .field(
                "has_top_level_opener_edge",
                &self.top_level_target.opener_edge.borrow().is_some(),
            )
            .field(
                "top_level_browsing_context_lifecycle",
                &self.top_level_target.lifecycle.get(),
            )
            .field("top_level_page_active", &self.top_level_target.active.get())
            .field(
                "top_level_page_focused",
                &self.top_level_target.focused.get(),
            )
            .field(
                "top_level_browsing_context_name",
                &self.top_level_target.name,
            )
            .finish()
    }
}

impl RendererPageScriptEnvironment {
    pub(crate) fn new(
        page_id: u64,
        opened_by_dom: bool,
        initially_active: bool,
        initially_focused: bool,
        auxiliary_page_reservation_allocator: RendererAuxiliaryPageReservationAllocator,
        renderer_document_isolate: RendererDocumentIsolateHandle,
        inspector_isolate_backend: RendererInspectorIsolateBackendHandle,
        script_agent_page_membership: RendererScriptAgentPageMembership,
        page_runtime_task_source: PageRuntimeTaskSource,
        output_journal: crate::runtime::RendererTurnOutputJournal,
    ) -> Result<Self> {
        Self::new_in_related_page_group(
            page_id,
            opened_by_dom,
            initially_active,
            initially_focused,
            auxiliary_page_reservation_allocator,
            renderer_document_isolate,
            inspector_isolate_backend,
            script_agent_page_membership,
            page_runtime_task_source,
            output_journal,
            RendererRelatedPageGroup::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_related(
        page_id: u64,
        opened_by_dom: bool,
        initially_active: bool,
        initially_focused: bool,
        auxiliary_page_reservation_allocator: RendererAuxiliaryPageReservationAllocator,
        renderer_document_isolate: RendererDocumentIsolateHandle,
        inspector_isolate_backend: RendererInspectorIsolateBackendHandle,
        script_agent_page_membership: RendererScriptAgentPageMembership,
        page_runtime_task_source: PageRuntimeTaskSource,
        output_journal: crate::runtime::RendererTurnOutputJournal,
        source_environment: &Self,
    ) -> Result<Self> {
        Self::new_in_related_page_group(
            page_id,
            opened_by_dom,
            initially_active,
            initially_focused,
            auxiliary_page_reservation_allocator,
            renderer_document_isolate,
            inspector_isolate_backend,
            script_agent_page_membership,
            page_runtime_task_source,
            output_journal,
            source_environment.related_page_group.clone(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_in_related_page_group(
        page_id: u64,
        opened_by_dom: bool,
        initially_active: bool,
        initially_focused: bool,
        auxiliary_page_reservation_allocator: RendererAuxiliaryPageReservationAllocator,
        renderer_document_isolate: RendererDocumentIsolateHandle,
        inspector_isolate_backend: RendererInspectorIsolateBackendHandle,
        script_agent_page_membership: RendererScriptAgentPageMembership,
        page_runtime_task_source: PageRuntimeTaskSource,
        output_journal: crate::runtime::RendererTurnOutputJournal,
        related_page_group: RendererRelatedPageGroup,
    ) -> Result<Self> {
        anyhow::ensure!(
            script_agent_page_membership.page_id().as_u64() == page_id,
            "Page script environment membership belongs to a different Page"
        );
        let residence =
            crate::RendererResolvedPopupTarget::from_residence(output_journal.stream().residence())
                .ok_or_else(|| {
                    anyhow!("Page script environment has a non-Page output residence")
                })?;
        let top_level_target = Rc::new(RendererRelatedPageTopLevelTargetState {
            residence,
            opened_by_dom,
            global_proxy: OnceCell::new(),
            current_default_context: RefCell::new(None),
            opener_edge: RefCell::new(None),
            lifecycle: Cell::new(RendererTopLevelBrowsingContextLifecycle::Active),
            active: Cell::new(initially_active),
            focused: Cell::new(initially_focused),
            name: RefCell::new(String::new()),
            current_cross_origin_opener_policy: RefCell::new(None),
        });
        related_page_group.register_target(&top_level_target);
        Ok(Self {
            page_id,
            auxiliary_page_reservation_allocator,
            renderer_document_isolate,
            inspector_isolate_backend,
            script_agent_page_membership,
            page_runtime_task_source,
            output_journal,
            related_page_group,
            top_level_target,
            initial_global_proxy_facade_context: Rc::new(RefCell::new(None)),
            initial_global_proxy_security_token: Rc::new(RefCell::new(None)),
        })
    }

    pub(crate) fn page_id(&self) -> u64 {
        self.page_id
    }

    pub(crate) fn opened_by_dom(&self) -> bool {
        self.top_level_target.opened_by_dom
    }

    pub(crate) fn browsing_context_group_id(&self) -> BrowsingContextGroupId {
        self.related_page_group.id
    }

    pub(crate) fn current_top_level_cross_origin_opener_policy(
        &self,
    ) -> Option<crate::cross_origin_isolation::TopLevelDocumentCrossOriginOpenerPolicy> {
        self.top_level_target
            .current_cross_origin_opener_policy
            .borrow()
            .clone()
    }

    pub(crate) fn commit_top_level_cross_origin_opener_policy(
        &self,
        state: crate::cross_origin_isolation::TopLevelDocumentCrossOriginOpenerPolicy,
    ) {
        *self
            .top_level_target
            .current_cross_origin_opener_policy
            .borrow_mut() = Some(state);
    }

    pub(crate) fn top_level_page_is_focused(&self) -> bool {
        self.top_level_target.focused.get()
    }

    pub(crate) fn top_level_page_is_active(&self) -> bool {
        self.top_level_target.active.get()
    }

    pub(crate) fn top_level_page_residence(&self) -> crate::RendererResolvedPopupTarget {
        self.top_level_target.residence
    }

    pub(crate) fn set_top_level_page_activation(
        &self,
        active: bool,
        focused: bool,
    ) -> (bool, bool) {
        (
            self.top_level_target.active.replace(active) != active,
            self.top_level_target.focused.replace(focused) != focused,
        )
    }

    pub(crate) fn auxiliary_page_reservation_allocator(
        &self,
    ) -> RendererAuxiliaryPageReservationAllocator {
        self.auxiliary_page_reservation_allocator.clone()
    }

    pub(crate) fn page_runtime_task_source(&self) -> PageRuntimeTaskSource {
        self.page_runtime_task_source.clone()
    }

    pub(crate) fn output_journal(&self) -> crate::runtime::RendererTurnOutputJournal {
        self.output_journal.clone()
    }

    /// Begins the script-visible close transaction exactly once.
    ///
    /// Like Blink's `window_is_closing_`, `Closing` is observable immediately,
    /// before the browser owner has retired the target. The Page-owned output
    /// record produced by the caller is what later performs that retirement.
    pub(crate) fn begin_top_level_browsing_context_close(&self) -> bool {
        if self.top_level_target.lifecycle.get() != RendererTopLevelBrowsingContextLifecycle::Active
        {
            return false;
        }
        self.related_page_group
            .unregister_target(&self.top_level_target);
        self.top_level_target
            .lifecycle
            .set(RendererTopLevelBrowsingContextLifecycle::Closing);
        true
    }

    pub(crate) fn mark_top_level_browsing_context_closed(&self) {
        self.related_page_group
            .unregister_target(&self.top_level_target);
        self.top_level_target
            .lifecycle
            .set(RendererTopLevelBrowsingContextLifecycle::Closed);
    }

    pub(crate) fn disconnect_top_level_browsing_context_for_group_switch(
        &self,
        scope: &mut v8::PinScope<'_, '_>,
    ) -> bool {
        if self.top_level_target.lifecycle.get() != RendererTopLevelBrowsingContextLifecycle::Active
        {
            return false;
        }
        self.related_page_group
            .unregister_target(&self.top_level_target);
        self.top_level_target
            .lifecycle
            .set(RendererTopLevelBrowsingContextLifecycle::Disconnected);
        self.sever_top_level_opener_edge(scope);
        true
    }

    pub(crate) fn top_level_browsing_context_is_closed(&self) -> bool {
        self.top_level_target.lifecycle.get() != RendererTopLevelBrowsingContextLifecycle::Active
    }

    pub(crate) fn signal_top_level_close_output_handoff(&self) {
        self.page_runtime_task_source
            .signal_top_level_close_output_handoff();
    }

    pub(crate) fn stage_related_initial_empty_page_in_scope(
        &self,
        scope: &mut v8::PinScope<'_, '_>,
        source_bridge_bindings: &NativeBridgeBindings,
        pending: crate::runtime::RendererPendingAuxiliaryPage,
        init: crate::runtime::RendererRelatedInitialEmptyPageRealmInit,
    ) -> Result<()> {
        self.auxiliary_page_reservation_allocator
            .stage_related_initial_empty_page_in_scope(
                scope,
                pending,
                self,
                source_bridge_bindings,
                init,
            )
    }

    pub(crate) fn clear_page_runtime_tasks(&self) {
        self.page_runtime_task_source.clear();
    }

    pub(crate) fn retire_output_stream(&self) {
        self.output_journal
            .retire(crate::runtime::RendererOutputStreamCloseReason::ResidenceRetired);
    }

    pub(crate) fn retire_script_agent_page_membership(&self) {
        self.script_agent_page_membership.retire();
    }

    pub(crate) fn isolate_identity_key(&self) -> usize {
        self.renderer_document_isolate.identity_key()
    }

    pub(crate) fn script_agent_id(&self) -> ScriptAgentId {
        self.renderer_document_isolate.script_agent_id()
    }

    pub(crate) fn is_related_page_peer(&self, other: &Self) -> bool {
        // One RendererDocumentIsolate owns exactly one script agent. Access
        // checks run while that isolate is already mutably borrowed, so this
        // hot path must use the stable Rc identity instead of borrowing the
        // holder again to read its script-agent id.
        self.page_id != other.page_id
            && self.renderer_document_isolate.identity_key()
                == other.renderer_document_isolate.identity_key()
    }

    pub(crate) fn bootstrap_replacement_document_isolate(
        &self,
    ) -> Result<RendererDocumentIsolateBootstrap> {
        let bridge_bindings = self.renderer_document_isolate.build_bridge_bindings()?;
        Ok(RendererDocumentIsolateBootstrap {
            renderer_document_isolate: self.renderer_document_isolate.clone(),
            bridge_bindings,
            renderer_document_isolate_teardown:
                RendererDocumentIsolateTeardown::owner_reserved_page(),
            inspector_isolate_backend: self.inspector_isolate_backend.clone(),
            page_inspector: DocumentInspectorBinding::new(self.inspector_isolate_backend.clone())
                .with_output_journal(self.output_journal()),
            script_agent_page_membership: None,
            renderer_page_script_environment: Some(self.clone()),
            reuse_main_window_proxy: true,
        })
    }

    pub(crate) fn bootstrap_related_page_document_isolate(
        &self,
        v8_foreground_task_sender: RendererPageV8ForegroundTaskSender,
    ) -> Result<RendererDocumentIsolateBootstrap> {
        let script_agent_page_membership = self
            .script_agent_page_membership
            .admit_related_page(v8_foreground_task_sender)?;
        let bridge_bindings = match self.renderer_document_isolate.build_bridge_bindings() {
            Ok(bindings) => bindings,
            Err(error) => {
                script_agent_page_membership.retire();
                return Err(error);
            }
        };
        Ok(self
            .related_page_document_isolate_bootstrap(bridge_bindings, script_agent_page_membership))
    }

    /// Prepares an explicitly related Page isolate bootstrap without
    /// re-entering or re-borrowing the document-isolate holder.
    ///
    /// This is the admission half of synchronous auxiliary realm creation.
    /// The caller owns an already-entered opener scope, so the source Page's
    /// retained membership and bridge templates are the only authorities this
    /// operation may use.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn bootstrap_related_page_document_isolate_in_scope(
        &self,
        scope: &mut v8::PinScope<'_, '_>,
        source_bridge_bindings: &NativeBridgeBindings,
        v8_foreground_task_sender: RendererPageV8ForegroundTaskSender,
    ) -> Result<RendererDocumentIsolateBootstrap> {
        let script_agent_page_membership = self
            .script_agent_page_membership
            .admit_related_page(v8_foreground_task_sender)?;
        let bridge_bindings = source_bridge_bindings.build_peer_in_scope(scope);
        Ok(self
            .related_page_document_isolate_bootstrap(bridge_bindings, script_agent_page_membership))
    }

    fn related_page_document_isolate_bootstrap(
        &self,
        bridge_bindings: NativeBridgeBindings,
        script_agent_page_membership: RendererScriptAgentPageMembership,
    ) -> RendererDocumentIsolateBootstrap {
        RendererDocumentIsolateBootstrap {
            renderer_document_isolate: self.renderer_document_isolate.clone(),
            bridge_bindings,
            renderer_document_isolate_teardown:
                RendererDocumentIsolateTeardown::owner_reserved_page(),
            inspector_isolate_backend: self.inspector_isolate_backend.clone(),
            page_inspector: DocumentInspectorBinding::new(self.inspector_isolate_backend.clone()),
            script_agent_page_membership: Some(script_agent_page_membership),
            renderer_page_script_environment: None,
            reuse_main_window_proxy: false,
        }
    }

    pub(super) fn install_initial_main_window_proxy(
        &self,
        global_proxy: v8::Global<v8::Object>,
    ) -> Result<()> {
        self.top_level_target
            .global_proxy
            .set(global_proxy)
            .map_err(|_| anyhow!("page script environment already retains its main WindowProxy"))
    }

    pub(crate) fn install_staged_initial_main_window_proxy(
        &self,
        staged: RendererStagedAuxiliaryWindowProxy,
    ) -> Result<()> {
        anyhow::ensure!(
            self.initial_global_proxy_facade_context.borrow().is_none(),
            "page script environment already retains a WindowProxy facade context"
        );
        let (window_proxy, facade_context, security_token) = staged.into_parts();
        self.install_initial_main_window_proxy(window_proxy)?;
        *self.initial_global_proxy_facade_context.borrow_mut() = Some(facade_context);
        *self.initial_global_proxy_security_token.borrow_mut() = security_token;
        Ok(())
    }

    pub(super) fn take_main_window_proxy_for_context<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_, ()>,
    ) -> Result<v8::Local<'s, v8::Object>> {
        let window_proxy =
            self.with_main_window_proxy(|window_proxy| v8::Local::new(scope, window_proxy))?;
        if let Some(facade_context) = self.initial_global_proxy_facade_context.borrow_mut().take() {
            v8::Local::new(scope, &facade_context).detach_global();
        }
        Ok(window_proxy)
    }

    pub(super) fn take_initial_main_window_security_token<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_, ()>,
    ) -> Option<v8::Local<'s, v8::Value>> {
        self.initial_global_proxy_security_token
            .borrow_mut()
            .take()
            .map(|token| v8::Local::new(scope, &token))
    }

    pub(super) fn with_main_window_proxy<T>(
        &self,
        op: impl FnOnce(&v8::Global<v8::Object>) -> T,
    ) -> Result<T> {
        let global_proxy = self.top_level_target.global_proxy.get().ok_or_else(|| {
            anyhow!("replacement context is missing its page-owned main WindowProxy")
        })?;
        Ok(op(global_proxy))
    }

    pub(crate) fn set_top_level_browsing_context_name(&self, name: String) {
        self.related_page_group
            .set_target_name(&self.top_level_target, name);
    }

    pub(crate) fn related_page_named_target_for_navigation<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        name: &str,
        replacement_opener: Option<v8::Local<'s, v8::Object>>,
    ) -> Option<(
        v8::Local<'s, v8::Object>,
        v8::Local<'s, v8::Context>,
        crate::RendererResolvedPopupTarget,
    )> {
        let target = self
            .related_page_group
            .find_named_target(&self.top_level_target, name)?;
        if let Some(opener) = replacement_opener {
            let opener: v8::Local<'s, v8::Value> = opener.into();
            *target.opener_edge.borrow_mut() = Some(v8::Global::new(scope, opener));
        }
        let window_proxy = v8::Local::new(scope, target.global_proxy.get()?);
        let context = v8::Local::new(scope, target.current_default_context.borrow().as_ref()?);
        Some((window_proxy, context, target.residence))
    }

    pub(crate) fn related_page_top_level_targets_for_navigation<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
    ) -> Vec<(
        v8::Local<'s, v8::Object>,
        v8::Local<'s, v8::Context>,
        crate::RendererResolvedPopupTarget,
        String,
        bool,
    )> {
        self.related_page_group
            .live_targets_in_page_order()
            .into_iter()
            .filter_map(|target| {
                let window = v8::Local::new(scope, target.global_proxy.get()?);
                let context =
                    v8::Local::new(scope, target.current_default_context.borrow().as_ref()?);
                let name = target.name.borrow().clone();
                Some((
                    window,
                    context,
                    target.residence,
                    name,
                    Rc::ptr_eq(&target, &self.top_level_target),
                ))
            })
            .collect()
    }

    pub(crate) fn related_page_current_context_for_residence<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        residence: crate::RendererResolvedPopupTarget,
    ) -> Option<v8::Local<'s, v8::Context>> {
        let target = self
            .related_page_group
            .live_targets_in_page_order()
            .into_iter()
            .find(|target| target.residence == residence)?;
        Some(v8::Local::new(
            scope,
            target.current_default_context.borrow().as_ref()?,
        ))
    }

    pub(crate) fn bind_current_main_default_context(&self, context: v8::Global<v8::Context>) {
        *self.top_level_target.current_default_context.borrow_mut() = Some(context);
    }

    pub(crate) fn replace_related_page_top_level_opener<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        residence: crate::RendererResolvedPopupTarget,
        opener: v8::Local<'s, v8::Object>,
    ) -> bool {
        let Some(target) = self
            .related_page_group
            .live_targets_in_page_order()
            .into_iter()
            .find(|target| target.residence == residence)
        else {
            return false;
        };
        let opener: v8::Local<'s, v8::Value> = opener.into();
        *target.opener_edge.borrow_mut() = Some(v8::Global::new(scope, opener));
        true
    }

    pub(super) fn restore_main_window_name_after_navigation(
        &self,
        scope: &mut v8::PinScope<'_, '_>,
        window_proxy: v8::Local<'_, v8::Object>,
    ) {
        let name = self.top_level_target.name.borrow();
        let Some(name_value) = crate::util::v8_string(scope, name.as_str()) else {
            return;
        };
        let _ = window_proxy.define_own_property(
            scope,
            crate::util::v8_string(scope, crate::context_bootstrap::WINDOW_NAME_SLOT)
                .expect("static Window name slot should fit V8")
                .into(),
            name_value.into(),
            v8::PropertyAttribute::DONT_ENUM,
        );
    }

    pub(super) fn capture_main_window_opener_for_navigation<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        window_proxy: v8::Local<'s, v8::Object>,
    ) {
        // Once bound, the Page edge is authoritative. In particular, an
        // explicit `window.opener = null` must not be reconnected from a stale
        // realm-private slot during the next Document replacement.
        if self.top_level_target.opener_edge.borrow().is_some() {
            return;
        }
        *self.top_level_target.opener_edge.borrow_mut() =
            get_private_value(scope, window_proxy, WINDOW_OPENER_SLOT)
                .map(|opener| v8::Global::new(scope, opener));
    }

    pub(crate) fn set_top_level_opener_edge<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        opener: v8::Local<'s, v8::Value>,
    ) {
        *self.top_level_target.opener_edge.borrow_mut() = Some(v8::Global::new(scope, opener));
    }

    pub(crate) fn sever_top_level_opener_edge(&self, scope: &mut v8::PinScope<'_, '_>) {
        let opener: v8::Local<'_, v8::Value> = v8::null(scope).into();
        self.set_top_level_opener_edge(scope, opener);
    }

    pub(crate) fn top_level_opener_value<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
    ) -> Option<v8::Local<'s, v8::Value>> {
        let opener = {
            let edge = self.top_level_target.opener_edge.borrow();
            edge.as_ref().map(|opener| v8::Local::new(scope, opener))?
        };
        if let Ok(opener_window) = v8::Local::<v8::Object>::try_from(opener)
            && crate::native_bridge::top_level_window_proxy_is_finally_closed(scope, opener_window)
        {
            // Blink clears the opener edge when the opener browsing context is
            // discarded. Lazily collapsing the edge here also handles a Page
            // that outlives its opener without retaining the opener host.
            self.sever_top_level_opener_edge(scope);
            return Some(v8::null(scope).into());
        }
        Some(opener)
    }

    pub(super) fn restore_main_window_opener_after_navigation<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        window_proxy: v8::Local<'s, v8::Object>,
    ) {
        let Some(opener) = self.top_level_opener_value(scope) else {
            return;
        };
        set_private_value(scope, window_proxy, WINDOW_OPENER_SLOT, opener);
    }
}

pub(crate) struct ScriptVmDefaultWorldBootstrap {
    pub(super) resource_owner_id: ResourceOwnerId,
    pub(super) promise_reject_dispatch: PromiseRejectDispatchSlot,
    pub(super) page_inspector: DocumentInspectorBinding,
    pub(super) renderer_document_isolate: RendererDocumentIsolateHandle,
    pub(super) renderer_document_isolate_teardown: RendererDocumentIsolateTeardown,
    pub(super) renderer_page_script_environment: Option<RendererPageScriptEnvironment>,
    pub(super) script_agent_page_membership: Option<RendererScriptAgentPageMembership>,
    pub(super) page_default_context: v8::Global<v8::Context>,
    pub(super) bridge_ref: JsContextHostBridgeRef,
    pub(super) runtime_observable_context_token: RuntimeObservableContextToken,
    pub(super) baseline_globals: super::ScriptGlobalsBaseline,
    pub(super) root_frame_id: Option<String>,
    pub(super) prebootstrapped_child_default_contexts: SharedPrebootstrappedChildDefaultContexts,
    // `JsContextHost` stores a non-owning pointer into `document_runtime`.
    // Keep every realm/bridge owner before the host and the host before the
    // runtime so cancellation of a staged preinspector bootstrap is safe.
    pub(super) context_host: Rc<RefCell<JsContextHost>>,
    pub(super) document_runtime: Box<DocumentRuntime>,
    pub(super) page_context_cancel_tx: RendererPageContextCancelSender,
    pub(super) post_domcontentloaded_page_task_tx: PageTaskSender,
    pub(super) page_runtime_wake_tx: PageRuntimeWakeSender,
    pub(super) storage_bucket_store: crate::context_bootstrap::SharedStorageBucketStore,
}

/// A fully bootstrapped main Page realm whose Inspector default-context
/// registration has deliberately not happened yet.
///
/// The V8 Context, stable WindowProxy, native bridge, and Document host are
/// already live at this boundary. Keeping Inspector attachment as a distinct
/// materialization step mirrors child-frame prebootstrap and is what makes it
/// possible to create an auxiliary realm synchronously from an opener callback
/// without re-entering the shared document isolate.
pub(crate) struct ScriptVmPreinspectorDefaultWorldBootstrap {
    pub(super) inner: ScriptVmDefaultWorldBootstrap,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct RendererDocumentIsolateTeardown {
    unregister_platform_on_context_teardown: bool,
    #[cfg(test)]
    requires_deferred_lifo_drop: bool,
}

impl RendererDocumentIsolateTeardown {
    fn owner_reserved_page() -> Self {
        #[cfg(test)]
        {
            Self {
                unregister_platform_on_context_teardown: false,
                requires_deferred_lifo_drop: false,
            }
        }
        #[cfg(not(test))]
        {
            Self {
                unregister_platform_on_context_teardown: false,
            }
        }
    }

    #[cfg(test)]
    fn standalone_test() -> Self {
        Self {
            unregister_platform_on_context_teardown: true,
            requires_deferred_lifo_drop: true,
        }
    }

    pub(super) fn unregister_platform_on_context_teardown(
        self,
        renderer_document_isolate: &RendererDocumentIsolateHandle,
    ) {
        if self.unregister_platform_on_context_teardown {
            renderer_document_isolate.unregister_renderer_document_isolate_platform();
        }
    }

    pub(super) fn requires_deferred_lifo_script_vm_drop(self) -> bool {
        #[cfg(test)]
        {
            self.requires_deferred_lifo_drop
        }
        #[cfg(not(test))]
        {
            false
        }
    }
}

#[derive(Clone)]
pub(crate) struct RendererDocumentIsolateHandle {
    inner: Rc<RefCell<RendererDocumentIsolateHolder>>,
}

impl std::fmt::Debug for RendererDocumentIsolateHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RendererDocumentIsolateHandle")
            .finish_non_exhaustive()
    }
}

impl RendererDocumentIsolateHandle {
    #[cfg(test)]
    pub(crate) fn new_standalone_without_owner_reservation_for_test(
        v8_foreground_task_sender: RendererPageV8ForegroundTaskSender,
    ) -> Result<RendererDocumentIsolateBootstrap> {
        Self::new_with_page_route(
            v8_foreground_task_sender,
            RendererDocumentIsolateTeardown::standalone_test(),
        )
    }

    pub(crate) fn new_owner_reserved_page(
        v8_foreground_task_sender: RendererPageV8ForegroundTaskSender,
    ) -> Result<RendererDocumentIsolateBootstrap> {
        Self::new_with_page_route(
            v8_foreground_task_sender,
            RendererDocumentIsolateTeardown::owner_reserved_page(),
        )
    }

    fn new_with_page_route(
        v8_foreground_task_sender: RendererPageV8ForegroundTaskSender,
        renderer_document_isolate_teardown: RendererDocumentIsolateTeardown,
    ) -> Result<RendererDocumentIsolateBootstrap> {
        let (renderer_document_isolate, bridge_bindings, script_agent_page_membership) =
            RendererDocumentIsolateHolder::new_holder(v8_foreground_task_sender)?;
        let renderer_document_isolate = Self {
            inner: Rc::new(RefCell::new(renderer_document_isolate)),
        };
        let isolate_backend = renderer_document_isolate.inspector_isolate_backend_handle();
        Ok(RendererDocumentIsolateBootstrap {
            renderer_document_isolate,
            bridge_bindings,
            renderer_document_isolate_teardown,
            inspector_isolate_backend: isolate_backend.clone(),
            page_inspector: DocumentInspectorBinding::new(isolate_backend),
            script_agent_page_membership: Some(script_agent_page_membership),
            renderer_page_script_environment: None,
            reuse_main_window_proxy: false,
        })
    }

    pub(crate) fn identity_key(&self) -> usize {
        Rc::as_ptr(&self.inner) as usize
    }

    pub(crate) fn script_agent_id(&self) -> ScriptAgentId {
        self.inner.borrow().script_agent_id
    }

    pub(crate) fn script_agent_scope(&self) -> crate::browsing_context_model::ScriptAgentScope {
        self.inner.borrow().script_agent_foreground_router.scope()
    }

    pub(crate) fn script_agent_page_count(&self) -> usize {
        self.inner
            .borrow()
            .script_agent_foreground_router
            .page_count()
    }

    pub(crate) fn inspector_isolate_backend_handle(&self) -> RendererInspectorIsolateBackendHandle {
        self.inner
            .borrow()
            .inspector_backend
            .as_ref()
            .expect("document isolate Inspector backend missing before ScriptVm drop")
            .handle()
    }

    fn build_bridge_bindings(&self) -> Result<NativeBridgeBindings> {
        let mut holder = self.inner.borrow_mut();
        let RendererDocumentIsolateHolder {
            isolate, bootstrap, ..
        } = &mut *holder;
        let isolate_ptr = unsafe { isolate.as_raw_isolate_ptr() };
        with_entered_owned_isolate(isolate, |isolate| {
            let scope = std::pin::pin!(v8::HandleScope::new(isolate));
            let scope = &mut scope.init();
            let global_template = bootstrap.global_template(scope);
            let cross_origin_window_global_template =
                bootstrap.cross_origin_window_global_template(scope);
            Ok(NativeBridgeBindings::build(
                scope,
                isolate_ptr,
                global_template,
                cross_origin_window_global_template,
            ))
        })
    }

    pub(super) fn with_renderer_document_isolate_and_inspector_mut<T>(
        &self,
        op: impl FnOnce(&mut v8::OwnedIsolate, &mut RendererInspectorIsolateBackend) -> T,
    ) -> T {
        let mut holder = self.inner.borrow_mut();
        let RendererDocumentIsolateHolder {
            isolate,
            inspector_backend,
            ..
        } = &mut *holder;
        let inspector_backend = inspector_backend
            .as_mut()
            .expect("document isolate Inspector backend missing before ScriptVm drop");
        with_entered_owned_isolate_value(isolate, |isolate| op(isolate, inspector_backend))
    }

    pub(super) fn with_entered_renderer_document_isolate_and_inspector_mut<T>(
        &self,
        op: impl FnOnce(&mut v8::OwnedIsolate, &mut RendererInspectorIsolateBackend) -> Result<T>,
    ) -> Result<T> {
        let mut holder = self.inner.borrow_mut();
        let RendererDocumentIsolateHolder {
            isolate,
            inspector_backend,
            ..
        } = &mut *holder;
        let inspector_backend = inspector_backend
            .as_mut()
            .ok_or_else(|| anyhow!("document isolate Inspector backend unavailable"))?;
        with_entered_owned_isolate(isolate, |isolate| op(isolate, inspector_backend))
    }

    pub(super) fn with_renderer_document_isolate_mut<T>(
        &self,
        op: impl FnOnce(&mut v8::OwnedIsolate) -> T,
    ) -> T {
        let mut holder = self.inner.borrow_mut();
        with_entered_owned_isolate_value(&mut holder.isolate, op)
    }

    pub(super) fn with_entered_renderer_document_isolate<T>(
        &self,
        op: impl FnOnce(&mut v8::OwnedIsolate) -> Result<T>,
    ) -> Result<T> {
        let mut holder = self.inner.borrow_mut();
        with_entered_owned_isolate(&mut holder.isolate, op)
    }

    pub(super) fn with_entered_renderer_document_isolate_and_bootstrap<T>(
        &self,
        op: impl FnOnce(&mut v8::OwnedIsolate, &IsolateBootstrapCache) -> Result<T>,
    ) -> Result<T> {
        let mut holder = self.inner.borrow_mut();
        let RendererDocumentIsolateHolder {
            isolate, bootstrap, ..
        } = &mut *holder;
        with_entered_owned_isolate(isolate, |isolate| op(isolate, &*bootstrap))
    }

    pub(super) fn with_renderer_document_isolate_and_bootstrap_mut<T>(
        &self,
        op: impl FnOnce(&mut v8::OwnedIsolate, &IsolateBootstrapCache) -> T,
    ) -> T {
        let mut holder = self.inner.borrow_mut();
        let RendererDocumentIsolateHolder {
            isolate, bootstrap, ..
        } = &mut *holder;
        with_entered_owned_isolate_value(isolate, |isolate| op(isolate, &*bootstrap))
    }

    pub(super) fn unregister_renderer_document_isolate_platform(&self) {
        self.inner.borrow_mut()._platform_registration.unregister();
    }

    pub(super) fn renderer_document_isolate_inspector_default_context_registry_count(
        &self,
    ) -> usize {
        self.inner.borrow().inspector_backend.as_ref().map_or(
            0,
            RendererInspectorIsolateBackend::default_context_registry_count,
        )
    }
}

pub(super) struct RendererDocumentIsolateHolder {
    // Inspector backend/session teardown touches V8 objects, so it must drop before the
    // isolate. `ScriptVm::drop` normally performs explicit context destruction;
    // this field order is the final safety net for partial construction paths.
    inspector_backend: Option<RendererInspectorIsolateBackend>,
    script_agent_id: ScriptAgentId,
    script_agent_foreground_router: RendererScriptAgentV8ForegroundTaskRouter,
    bootstrap: IsolateBootstrapCache,
    _platform_registration: V8PlatformIsolateRegistration,
    isolate: v8::OwnedIsolate,
    // Declared after the isolate so destroyed/live accounting changes only
    // after `OwnedIsolate::drop` has completed disposal.
    _accounting: RendererDocumentIsolateAccountingGuard,
}

impl RendererDocumentIsolateHolder {
    fn new_holder(
        v8_foreground_task_sender: RendererPageV8ForegroundTaskSender,
    ) -> Result<(
        Self,
        NativeBridgeBindings,
        RendererScriptAgentPageMembership,
    )> {
        let timing_enabled = moli_trace::cdp_nav_timing_enabled();
        let total_start = timing_enabled.then(std::time::Instant::now);
        let script_agent_id = allocate_script_agent_id();
        let (script_agent_foreground_router, script_agent_page_membership) =
            RendererScriptAgentV8ForegroundTaskRouter::new(
                script_agent_id,
                v8_foreground_task_sender,
            );
        let foreground_wake =
            V8ForegroundTaskWake::script_agent(script_agent_foreground_router.clone());

        let isolate_new_start = timing_enabled.then(std::time::Instant::now);
        // Window agents must not block their event loop with Atomics.wait().
        // Blink configures its main-thread isolates the same way; dedicated
        // workers keep V8's default and may use the blocking operation.
        let mut isolate = v8::Isolate::new(v8::CreateParams::default().allow_atomics_wait(false));
        if timing_enabled {
            tracing::info!(
                target: "moli_cdp_nav_timing",
                stage = "v8_isolate_new",
                elapsed_ms = isolate_new_start.unwrap().elapsed().as_secs_f64() * 1000.0,
                "v8::Isolate::new (cold, no snapshot)"
            );
        }

        // kExplicit: the owner loop manually checkpoints microtasks at
        // observable page/command boundaries.
        crate::context_bootstrap::install_agent_microtask_checkpoint_tasks(&mut isolate);
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        isolate.set_capture_stack_trace_for_uncaught_exceptions(true, 32);
        // V8 publishes ERROR messages for access-check exceptions before JavaScript gets a
        // chance to catch them. The script, callback, and promise owners already report values
        // that remain uncaught, so treating every ERROR-level listener message as uncaught
        // produces false process diagnostics for ordinary caught Web API exceptions.
        let non_exception_message_levels = v8::MessageErrorLevel::LOG
            | v8::MessageErrorLevel::DEBUG
            | v8::MessageErrorLevel::INFO
            | v8::MessageErrorLevel::WARNING;
        isolate.add_message_listener_with_error_level(
            v8_message_listener,
            non_exception_message_levels,
        );
        isolate.set_host_initialize_import_meta_object_callback(
            initialize_import_meta_object_callback,
        );
        isolate.set_host_import_module_dynamically_callback(dynamic_import_callback);
        isolate.set_host_import_module_with_phase_dynamically_callback(
            dynamic_import_with_phase_callback,
        );
        isolate.set_allow_wasm_code_generation_callback(
            super::security_policy::wasm_code_generation_check_callback,
        );
        isolate.set_modify_code_generation_from_strings_callback(
            super::security_policy::string_code_generation_check_callback,
        );
        if moli_trace::dom_binding_timing_enabled() {
            isolate.set_promise_hook(promise_trace_hook);
        }
        isolate.set_promise_reject_callback(promise_reject_callback);
        isolate.set_failed_access_check_callback_function(failed_access_check_callback);

        let platform_registration = V8PlatformIsolateRegistration::register(
            &mut isolate,
            foreground_wake.into_platform_wake(),
        );
        let isolate_ptr = unsafe { isolate.as_raw_isolate_ptr() };
        let isolate_bootstrap;
        let bridge_bindings;
        {
            let scope = std::pin::pin!(v8::HandleScope::new(&mut isolate));
            let scope = &mut scope.init();

            let bootstrap_start = timing_enabled.then(std::time::Instant::now);
            isolate_bootstrap = IsolateBootstrapCache::build(scope)?;
            if timing_enabled {
                tracing::info!(
                    target: "moli_cdp_nav_timing",
                    stage = "isolate_bootstrap_cache_build",
                    elapsed_ms = bootstrap_start.unwrap().elapsed().as_secs_f64() * 1000.0,
                    "IsolateBootstrapCache::build (246 constructor specs + global template)"
                );
            }

            let bridge_start = timing_enabled.then(std::time::Instant::now);
            let global_template = isolate_bootstrap.global_template(scope);
            let cross_origin_window_global_template =
                isolate_bootstrap.cross_origin_window_global_template(scope);
            bridge_bindings = NativeBridgeBindings::build(
                scope,
                isolate_ptr,
                global_template,
                cross_origin_window_global_template,
            );
            if timing_enabled {
                tracing::info!(
                    target: "moli_cdp_nav_timing",
                    stage = "native_bridge_bindings_build",
                    elapsed_ms = bridge_start.unwrap().elapsed().as_secs_f64() * 1000.0,
                    "NativeBridgeBindings::build"
                );
            }
        }

        let inspector_start = timing_enabled.then(std::time::Instant::now);
        let inspector_backend = RendererInspectorIsolateBackend::new(&mut isolate);
        if timing_enabled {
            tracing::info!(
                target: "moli_cdp_nav_timing",
                stage = "inspector_backend_new",
                elapsed_ms = inspector_start.unwrap().elapsed().as_secs_f64() * 1000.0,
                "RendererInspectorIsolateBackend::new"
            );
            tracing::info!(
                target: "moli_cdp_nav_timing",
                stage = "v8_isolate_init_total",
                elapsed_ms = total_start.unwrap().elapsed().as_secs_f64() * 1000.0,
                "V8 isolate initialization total (cold, no snapshot)"
            );
        }

        // `v8::Isolate::new` enters the isolate. Document isolates are owned
        // independently by PageVms and may be destroyed in any page order, so
        // no isolate may remain on V8's thread-local enter stack between
        // operations.
        unsafe {
            isolate.exit();
        }

        Ok((
            Self::new(
                script_agent_id,
                script_agent_foreground_router,
                inspector_backend,
                isolate_bootstrap,
                platform_registration,
                isolate,
            ),
            bridge_bindings,
            script_agent_page_membership,
        ))
    }

    pub(super) fn new(
        script_agent_id: ScriptAgentId,
        script_agent_foreground_router: RendererScriptAgentV8ForegroundTaskRouter,
        inspector_backend: RendererInspectorIsolateBackend,
        bootstrap: IsolateBootstrapCache,
        platform_registration: V8PlatformIsolateRegistration,
        isolate: v8::OwnedIsolate,
    ) -> Self {
        Self {
            inspector_backend: Some(inspector_backend),
            script_agent_id,
            script_agent_foreground_router,
            bootstrap,
            _platform_registration: platform_registration,
            isolate,
            _accounting: RendererDocumentIsolateAccountingGuard::new(),
        }
    }
}

impl Drop for RendererDocumentIsolateHolder {
    fn drop(&mut self) {
        // Fields drop in declaration order after this method. Enter now so the
        // inspector and bootstrap globals are released in their owning
        // isolate, then the platform registration is canceled, and finally
        // `OwnedIsolate::drop` observes itself as current and disposes it.
        unsafe {
            self.isolate.enter();
        }
    }
}

struct EnteredIsolateGuard(*mut v8::OwnedIsolate);

impl Drop for EnteredIsolateGuard {
    fn drop(&mut self) {
        unsafe {
            (*self.0).exit();
        }
    }
}

fn with_entered_owned_isolate<T>(
    isolate: &mut v8::OwnedIsolate,
    op: impl FnOnce(&mut v8::OwnedIsolate) -> Result<T>,
) -> Result<T> {
    unsafe {
        isolate.enter();
    }
    let _guard = EnteredIsolateGuard(isolate);
    op(isolate)
}

fn with_entered_owned_isolate_value<T>(
    isolate: &mut v8::OwnedIsolate,
    op: impl FnOnce(&mut v8::OwnedIsolate) -> T,
) -> T {
    unsafe {
        isolate.enter();
    }
    let _guard = EnteredIsolateGuard(isolate);
    op(isolate)
}

pub(super) struct IsolateBootstrapCache {
    pub(super) context_assets: ContextBootstrapAssets,
}

impl IsolateBootstrapCache {
    pub(super) fn build(scope: &mut v8::PinScope<'_, '_, ()>) -> Result<Self> {
        Ok(Self {
            context_assets: ContextBootstrapAssets::build(scope)?,
        })
    }

    pub(super) fn global_template<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_, ()>,
    ) -> v8::Local<'s, v8::ObjectTemplate> {
        self.context_assets.global_template(scope)
    }

    pub(super) fn cross_origin_window_global_template<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_, ()>,
    ) -> v8::Local<'s, v8::ObjectTemplate> {
        self.context_assets
            .cross_origin_window_global_template(scope)
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    struct ContextSlotDropCounter(Rc<Cell<usize>>);

    impl Drop for ContextSlotDropCounter {
        fn drop(&mut self) {
            self.0.set(self.0.get().saturating_add(1));
        }
    }

    #[test]
    fn context_annex_weak_handles_are_safe_during_isolate_teardown() {
        crate::ensure_v8_for_test();

        const ISOLATE_COUNT: usize = 4;
        const CONTEXTS_PER_ISOLATE: usize = 32;
        let dropped_slots = Rc::new(Cell::new(0));

        for _ in 0..ISOLATE_COUNT {
            let mut isolate = v8::Isolate::new(Default::default());
            let mut contexts = Vec::with_capacity(CONTEXTS_PER_ISOLATE);
            {
                let scope = std::pin::pin!(v8::HandleScope::new(&mut isolate));
                let scope = &mut scope.init();
                for _ in 0..CONTEXTS_PER_ISOLATE {
                    let context = v8::Context::new(scope, Default::default());
                    let replaced = context
                        .set_slot(Rc::new(ContextSlotDropCounter(Rc::clone(&dropped_slots))));
                    assert!(replaced.is_none());
                    contexts.push(v8::Global::new(scope, context));
                }
            }

            // Leave ContextAnnex finalizers pending until OwnedIsolate teardown.
            drop(contexts);
            drop(isolate);
        }

        assert_eq!(dropped_slots.get(), ISOLATE_COUNT * CONTEXTS_PER_ISOLATE);
    }

    #[test]
    fn snapshot_creator_cleans_up_context_annex_before_creating_blob() {
        crate::ensure_v8_for_test();

        let dropped_slots = Rc::new(Cell::new(0));
        let mut snapshot_creator = v8::Isolate::snapshot_creator(None, None);
        {
            let scope = std::pin::pin!(v8::HandleScope::new(&mut snapshot_creator));
            let scope = &mut scope.init();
            let context = v8::Context::new(scope, Default::default());
            let replaced =
                context.set_slot(Rc::new(ContextSlotDropCounter(Rc::clone(&dropped_slots))));
            assert!(replaced.is_none());
            scope.set_default_context(context);
        }

        let startup_data = snapshot_creator
            .create_blob(v8::FunctionCodeHandling::Clear)
            .expect("snapshot creator should produce a blob");
        assert!(!startup_data.is_empty());
        assert_eq!(dropped_slots.get(), 1);
    }
}
