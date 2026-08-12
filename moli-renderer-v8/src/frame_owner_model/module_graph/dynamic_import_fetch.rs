use crate::frame_owner_model::{
    FrameDocumentModuleFetchClientStart, FrameDocumentModuleFetchDisposition, FrameDocumentOwner,
    FrameDocumentTaskOwner, FrameRealmId,
};
use crate::module_runtime::{
    DynamicModuleInflightFetch, DynamicModuleJoinedFetch, DynamicModuleScheduledFetch,
    ModuleEntryId, ModuleGraphFetchedSource, ModuleGraphHandle, ModuleImportPhase, ModuleLoadError,
    ModuleMapKey, NativeDynamicImportSingleModuleClient, NativeDynamicModuleImportReady,
    NativeModuleGraphFetchRequest, NativeModuleGraphJob, PendingDynamicModuleImport,
};
use moli_module_script_tree as module_tree;
use url::Url;

use super::super::module_clients::FrameDocumentDynamicImportTerminalWork;
use super::FrameDocumentModuleTerminalQueueFollowup;

pub(crate) struct ChildDynamicModuleInflightFetch {
    pub(crate) inflight: DynamicModuleInflightFetch,
}

pub(crate) struct ChildDynamicModuleJoinedFetch {
    pub(crate) owner: FrameDocumentOwner,
    pub(crate) realm_id: FrameRealmId,
    pub(crate) joined: DynamicModuleJoinedFetch,
}

pub(crate) enum ChildDynamicModuleFetchAction {
    Schedule(ChildDynamicModuleFetchScheduleAction),
    RestoreOwnerTerminalWithoutNetwork {
        settle: ChildDynamicModuleOwnerFetchWithoutNetworkSettlementAction,
        restore: ChildDynamicModuleOwnerTerminalRestoreAction,
    },
}

pub(crate) struct ChildDynamicModuleFetchScheduleAction {
    load_id: u64,
    request: Box<NativeModuleGraphFetchRequest>,
}

pub(crate) struct FrameDocumentDynamicImportWaitingFetchScheduleAction {
    owner: FrameDocumentOwner,
    realm_id: FrameRealmId,
    fetch: ChildDynamicModuleFetchScheduleAction,
}

pub(crate) struct ChildDynamicModuleCompletedFetchRestoreAction {
    owner: FrameDocumentOwner,
    realm_id: FrameRealmId,
    inflight: Box<DynamicModuleInflightFetch>,
}

pub(crate) struct ChildDynamicModuleOwnerFetchCompletionSettlementAction {
    start: FrameDocumentModuleFetchClientStart,
    source: Box<Result<ModuleGraphFetchedSource, ModuleLoadError>>,
}

pub(crate) struct ChildDynamicModuleOwnerFetchWithoutNetworkSettlementAction {
    start: FrameDocumentModuleFetchClientStart,
}

pub(crate) struct ChildDynamicModuleOwnerTerminalRestoreAction {
    load_id: u64,
}

pub(crate) struct FrameDocumentDynamicImportOwnerTerminalRestoreAction {
    owner: FrameDocumentOwner,
    realm_id: FrameRealmId,
    restore: ChildDynamicModuleOwnerTerminalRestoreAction,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameDocumentDynamicImportMissingJoinedTerminalFetch {
    owner: FrameDocumentOwner,
    realm_id: FrameRealmId,
    load_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameDocumentDynamicImportMissingJoinedTerminalClient {
    owner: FrameDocumentOwner,
    realm_id: FrameRealmId,
    tree_client: module_tree::SingleModuleClientToken,
}

pub(crate) enum FrameDocumentDynamicImportOwnerAction {
    TerminalClient(FrameDocumentDynamicImportTerminalClientAction),
    OwnerModuleFetchCompleted {
        load_id: u64,
        settle: ChildDynamicModuleOwnerFetchCompletionSettlementAction,
        restore: ChildDynamicModuleCompletedFetchRestoreAction,
    },
    Waiting(FrameDocumentDynamicImportWaitingAction),
    Ready(Box<FrameDocumentDynamicImportReadyAction>),
    Reject(Box<FrameDocumentDynamicImportRejectAction>),
}

pub(crate) enum FrameDocumentDynamicImportReadyAction {
    Source(FrameDocumentDynamicImportSourceReadyAction),
    Evaluation(FrameDocumentDynamicImportEvaluationReadyAction),
}

pub(crate) struct FrameDocumentDynamicImportTerminalClientAction {
    task_owner: FrameDocumentTaskOwner,
    realm_id: FrameRealmId,
    key: ModuleMapKey,
    client: NativeDynamicImportSingleModuleClient,
}

pub(crate) struct FrameDocumentDynamicImportSourceReadyAction {
    request: PendingDynamicModuleImport,
    root_entry: ModuleEntryId,
}

pub(crate) struct FrameDocumentDynamicImportEvaluationReadyAction {
    request: PendingDynamicModuleImport,
    graph: ModuleGraphHandle,
}

pub(crate) struct FrameDocumentDynamicImportRejectAction {
    reason: FrameDocumentDynamicImportRejectReason,
    request: PendingDynamicModuleImport,
    error: ModuleLoadError,
}

pub(crate) struct FrameDocumentDynamicImportWaitingAction {
    owner: FrameDocumentOwner,
    realm_id: FrameRealmId,
    fetch_action: ChildDynamicModuleFetchAction,
}

/// Non-empty fanout produced by one dynamic-import graph transition.
///
/// Fanout is expanded into separate prepared actions before it reaches a Page
/// task source. Keeping the non-empty invariant here prevents an executable
/// owner action from hiding a batch of fetch starts inside one scheduler turn.
pub(crate) struct FrameDocumentDynamicImportOwnerActions {
    actions: Vec<FrameDocumentDynamicImportOwnerAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentDynamicImportRejectReason {
    FetchFailure,
    GraphAdvanceFailure,
}

pub(crate) struct FrameDocumentDynamicImportTerminalPreparedAction {
    trace: FrameDocumentDynamicImportOwnerActionTrace,
    action: FrameDocumentDynamicImportOwnerAction,
}

pub(crate) enum FrameDocumentDynamicImportOwnerActionQueueRequest {
    Waiting {
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        fetch_actions: Vec<ChildDynamicModuleFetchAction>,
    },
    Continuation {
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        actions: FrameDocumentDynamicImportOwnerActions,
    },
    FetchCompletion {
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        load_id: u64,
        actions: FrameDocumentDynamicImportOwnerActions,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentDynamicImportQueueTaskOwnerResult {
    Current(FrameDocumentTaskOwner),
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentDynamicImportOwnerActionQueueTrace {
    Continuation {
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
    },
    FetchCompletion {
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        load_id: u64,
    },
    Waiting {
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        fetch_count: usize,
    },
}

pub(crate) trait FrameDocumentDynamicImportOwnerActionQueueHooks {
    fn current_dynamic_import_task_owner(
        &mut self,
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
    ) -> FrameDocumentDynamicImportQueueTaskOwnerResult;

    fn queue_dynamic_import_owner_actions(
        &mut self,
        actions: Vec<FrameDocumentDynamicImportTerminalPreparedAction>,
    ) -> FrameDocumentModuleTerminalQueueFollowup;

    fn record_stale_dynamic_import_owner_action(
        &mut self,
        trace: FrameDocumentDynamicImportOwnerActionQueueTrace,
    );
}

pub(crate) struct FrameDocumentDynamicImportOwnerActionQueueRunner<Hooks> {
    hooks: Hooks,
}

pub(crate) trait FrameDocumentDynamicImportOwnerActionHooks {
    fn finish_terminal_client(
        &mut self,
        action: FrameDocumentDynamicImportTerminalClientAction,
    ) -> Result<FrameDocumentDynamicImportTerminalClientFinishResult, String>;

    fn queue_owner_action_followups(
        &mut self,
        actions: Vec<FrameDocumentDynamicImportTerminalPreparedAction>,
    ) -> Result<FrameDocumentModuleTerminalQueueFollowup, String>;

    fn record_missing_joined_terminal_client(
        &mut self,
        missing: FrameDocumentDynamicImportMissingJoinedTerminalClient,
    ) -> Result<(), String>;

    fn settle_owner_module_fetch_completion(
        &mut self,
        action: ChildDynamicModuleOwnerFetchCompletionSettlementAction,
    ) -> Result<FrameDocumentDynamicImportOwnerFetchSettlementResult, String>;

    fn restore_completed_owner_module_fetch_as_joined_terminal_client(
        &mut self,
        restore: ChildDynamicModuleCompletedFetchRestoreAction,
    ) -> Result<FrameDocumentDynamicImportJoinedFetchRestoreResult, String>;

    fn finish_owner_module_fetch_without_network(
        &mut self,
        action: ChildDynamicModuleOwnerFetchWithoutNetworkSettlementAction,
    ) -> Result<FrameDocumentDynamicImportOwnerFetchSettlementResult, String>;

    fn restore_scheduled_fetch_as_joined_terminal_client(
        &mut self,
        action: FrameDocumentDynamicImportOwnerTerminalRestoreAction,
    ) -> Result<FrameDocumentDynamicImportJoinedFetchRestoreResult, String>;

    fn schedule_waiting_fetch(
        &mut self,
        action: FrameDocumentDynamicImportWaitingFetchScheduleAction,
    ) -> Result<FrameDocumentDynamicImportWaitingFetchScheduleResult, String>;

    fn record_missing_joined_terminal_fetch(
        &mut self,
        missing: FrameDocumentDynamicImportMissingJoinedTerminalFetch,
    ) -> Result<(), String>;

    fn resolve_ready_source_import(
        &mut self,
        action: FrameDocumentDynamicImportSourceReadyAction,
    ) -> Result<FrameDocumentDynamicImportSourceReadyResult, String>;

    fn continue_ready_evaluation_import(
        &mut self,
        action: FrameDocumentDynamicImportEvaluationReadyAction,
    ) -> Result<FrameDocumentDynamicImportEvaluationReadyResult, String>;

    fn record_restored_after_unexpected_complete(
        &mut self,
        diagnostic: FrameDocumentDynamicImportOwnerActionDiagnostic,
    ) -> Result<(), String>;

    fn reject_dynamic_import(
        &mut self,
        action: FrameDocumentDynamicImportRejectAction,
    ) -> Result<FrameDocumentDynamicImportRejectResult, String>;

    fn record_action_resumed(
        &mut self,
        diagnostic: FrameDocumentDynamicImportOwnerActionDiagnostic,
    );

    fn record_action_failed(
        &mut self,
        diagnostic: FrameDocumentDynamicImportOwnerActionDiagnostic,
        error: &str,
    );
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentDynamicImportJoinedFetchRestoreResult {
    Restored,
    Missing,
}

impl FrameDocumentDynamicImportJoinedFetchRestoreResult {
    pub(crate) fn from_restored(restored: bool) -> Self {
        if restored {
            Self::Restored
        } else {
            Self::Missing
        }
    }

    fn restored(self) -> bool {
        matches!(self, Self::Restored)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentDynamicImportOwnerFetchSettlementResult {
    Settled,
    Missing,
}

impl FrameDocumentDynamicImportOwnerFetchSettlementResult {
    pub(crate) fn from_settled(settled: bool) -> Self {
        if settled {
            Self::Settled
        } else {
            Self::Missing
        }
    }

    fn settled(self) -> bool {
        matches!(self, Self::Settled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentDynamicImportWaitingFetchScheduleResult {
    Scheduled,
    MissingLoader,
    StaleOwner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentDynamicImportSourceReadyResult {
    Resolved,
    Rejected,
    DroppedStaleOwner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentDynamicImportEvaluationReadyResult {
    Resolved,
    Pending,
    Rejected,
    DroppedStaleOwner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentDynamicImportRejectResult {
    Rejected,
    DroppedStaleOwner,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentDynamicImportOwnerActionQueueOutcome {
    stale_owner: bool,
    followup: FrameDocumentModuleTerminalQueueFollowup,
}

impl FrameDocumentDynamicImportOwnerActionQueueOutcome {
    pub(crate) fn stale_owner() -> Self {
        Self {
            stale_owner: true,
            ..Self::default()
        }
    }

    pub(crate) fn with_followup(followup: FrameDocumentModuleTerminalQueueFollowup) -> Self {
        Self {
            followup,
            ..Self::default()
        }
    }

    pub(crate) fn into_followup(self) -> FrameDocumentModuleTerminalQueueFollowup {
        self.followup
    }

    #[cfg(test)]
    pub(crate) fn stale_owner_was_recorded(self) -> bool {
        self.stale_owner
    }

    #[cfg(test)]
    pub(crate) fn dynamic_import_owner_action_was_queued(self) -> bool {
        self.followup.dynamic_import_owner_action_was_queued()
    }

    #[cfg(test)]
    pub(crate) fn dynamic_import_wait_was_retained(self) -> bool {
        self.followup.dynamic_import_wait_was_retained()
    }
}

pub(crate) struct FrameDocumentDynamicImportOwnerActionRunner<Hooks> {
    hooks: Hooks,
}

pub(crate) enum FrameDocumentDynamicImportTerminalClientFinishResult {
    MissingJoinedClient,
    FollowupActions(FrameDocumentDynamicImportOwnerActions),
    RestoredAfterUnexpectedComplete,
    WaitRetained,
}

#[derive(Clone, Debug)]
pub(crate) enum FrameDocumentDynamicImportOwnerActionTrace {
    Terminal(FrameDocumentDynamicImportTerminalTrace),
    FetchCompletion {
        task_owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
        load_id: u64,
    },
    Continuation {
        task_owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FrameDocumentDynamicImportOwnerActionDiagnostic {
    TerminalClient {
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        tree_client: module_tree::SingleModuleClientToken,
        import_phase: module_tree::ModuleImportPhase,
        url: Url,
    },
    FetchCompletion {
        task_owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
        load_id: u64,
    },
    Continuation {
        task_owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct FrameDocumentDynamicImportTerminalTrace {
    task_owner: FrameDocumentTaskOwner,
    realm_id: FrameRealmId,
    key: ModuleMapKey,
    tree_client: module_tree::SingleModuleClientToken,
    import_phase: module_tree::ModuleImportPhase,
}

impl FrameDocumentDynamicImportTerminalTrace {
    fn from_terminal_client_action(
        action: &FrameDocumentDynamicImportTerminalClientAction,
    ) -> Self {
        Self {
            task_owner: action.task_owner(),
            realm_id: action.realm_id(),
            key: action.key().clone(),
            tree_client: action.client_token(),
            import_phase: action.import_phase(),
        }
    }

    pub(crate) fn task_owner(&self) -> FrameDocumentTaskOwner {
        self.task_owner
    }

    pub(crate) fn owner(&self) -> FrameDocumentOwner {
        self.task_owner.document_owner()
    }

    pub(crate) fn realm_id(&self) -> FrameRealmId {
        self.realm_id
    }

    pub(crate) fn key(&self) -> &ModuleMapKey {
        &self.key
    }

    pub(crate) fn tree_client(&self) -> module_tree::SingleModuleClientToken {
        self.tree_client
    }

    pub(crate) fn import_phase(&self) -> module_tree::ModuleImportPhase {
        self.import_phase
    }

    fn diagnostic(&self) -> FrameDocumentDynamicImportOwnerActionDiagnostic {
        FrameDocumentDynamicImportOwnerActionDiagnostic::TerminalClient {
            owner: self.owner(),
            realm_id: self.realm_id(),
            tree_client: self.tree_client(),
            import_phase: self.import_phase(),
            url: self.key().url().clone(),
        }
    }

    fn missing_joined_terminal_client(
        &self,
    ) -> FrameDocumentDynamicImportMissingJoinedTerminalClient {
        FrameDocumentDynamicImportMissingJoinedTerminalClient {
            owner: self.owner(),
            realm_id: self.realm_id,
            tree_client: self.tree_client,
        }
    }
}

impl FrameDocumentDynamicImportOwnerActionTrace {
    pub(crate) fn task_owner(&self) -> FrameDocumentTaskOwner {
        match self {
            Self::Terminal(trace) => trace.task_owner(),
            Self::FetchCompletion { task_owner, .. } | Self::Continuation { task_owner, .. } => {
                *task_owner
            }
        }
    }

    pub(crate) fn realm_id(&self) -> FrameRealmId {
        match self {
            Self::Terminal(trace) => trace.realm_id(),
            Self::FetchCompletion { realm_id, .. } | Self::Continuation { realm_id, .. } => {
                *realm_id
            }
        }
    }

    fn diagnostic(&self) -> FrameDocumentDynamicImportOwnerActionDiagnostic {
        match self {
            Self::Terminal(trace) => trace.diagnostic(),
            Self::FetchCompletion {
                task_owner,
                realm_id,
                load_id,
            } => FrameDocumentDynamicImportOwnerActionDiagnostic::FetchCompletion {
                task_owner: *task_owner,
                realm_id: *realm_id,
                load_id: *load_id,
            },
            Self::Continuation {
                task_owner,
                realm_id,
            } => FrameDocumentDynamicImportOwnerActionDiagnostic::Continuation {
                task_owner: *task_owner,
                realm_id: *realm_id,
            },
        }
    }
}

impl FrameDocumentDynamicImportTerminalClientFinishResult {
    pub(crate) fn followup_action(action: FrameDocumentDynamicImportOwnerAction) -> Self {
        Self::FollowupActions(FrameDocumentDynamicImportOwnerActions::one(action))
    }

    pub(crate) fn followup_waiting_fetches(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        fetch_actions: Vec<ChildDynamicModuleFetchAction>,
    ) -> Self {
        FrameDocumentDynamicImportOwnerActions::waiting(owner, realm_id, fetch_actions)
            .map(Self::FollowupActions)
            .unwrap_or(Self::WaitRetained)
    }
}

impl FrameDocumentDynamicImportMissingJoinedTerminalClient {
    pub(crate) fn owner(&self) -> FrameDocumentOwner {
        self.owner
    }

    pub(crate) fn realm_id(&self) -> FrameRealmId {
        self.realm_id
    }

    pub(crate) fn tree_client(&self) -> module_tree::SingleModuleClientToken {
        self.tree_client
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentDynamicImportTerminalOutcome {
    terminal_work_consumed: bool,
    missing_joined_client: bool,
    missing_joined_terminal_fetch: bool,
    owner_action_queue_followup: FrameDocumentModuleTerminalQueueFollowup,
    owner_module_fetch_settled: bool,
    owner_module_fetch_restored: bool,
    waiting_fetch_scheduled: bool,
    waiting_fetch_missing_loader: bool,
    dynamic_import_wait_retained: bool,
    source_import_resolved: bool,
    source_import_rejected: bool,
    evaluation_import_resolved: bool,
    evaluation_import_pending: bool,
    evaluation_import_rejected: bool,
    stale_owner_dropped: bool,
    unexpected_complete_recorded: bool,
    dynamic_import_rejected: bool,
    resume_failed: bool,
}

impl FrameDocumentDynamicImportTerminalOutcome {
    pub(crate) fn terminal_work_consumed() -> Self {
        Self {
            terminal_work_consumed: true,
            ..Self::default()
        }
    }

    pub(crate) fn missing_joined_client() -> Self {
        Self {
            missing_joined_client: true,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn owner_action_queued() -> Self {
        Self::from_owner_action_queue_followup(
            FrameDocumentModuleTerminalQueueFollowup::dynamic_import_owner_action_queued(),
        )
    }

    pub(crate) fn from_owner_action_queue_followup(
        followup: FrameDocumentModuleTerminalQueueFollowup,
    ) -> Self {
        Self {
            owner_action_queue_followup: followup,
            ..Self::default()
        }
    }

    pub(crate) fn missing_joined_terminal_fetch() -> Self {
        Self {
            missing_joined_terminal_fetch: true,
            ..Self::default()
        }
    }

    pub(crate) fn owner_module_fetch_settled() -> Self {
        Self {
            owner_module_fetch_settled: true,
            ..Self::default()
        }
    }

    pub(crate) fn owner_module_fetch_restored() -> Self {
        Self {
            owner_module_fetch_restored: true,
            ..Self::default()
        }
    }

    pub(crate) fn waiting_fetch_scheduled() -> Self {
        Self {
            waiting_fetch_scheduled: true,
            ..Self::default()
        }
    }

    pub(crate) fn waiting_fetch_missing_loader() -> Self {
        Self {
            waiting_fetch_missing_loader: true,
            ..Self::default()
        }
    }

    pub(crate) fn dynamic_import_wait_retained() -> Self {
        Self {
            dynamic_import_wait_retained: true,
            ..Self::default()
        }
    }

    pub(crate) fn source_import_resolved() -> Self {
        Self {
            source_import_resolved: true,
            ..Self::default()
        }
    }

    pub(crate) fn source_import_rejected() -> Self {
        Self {
            source_import_rejected: true,
            dynamic_import_rejected: true,
            ..Self::default()
        }
    }

    pub(crate) fn evaluation_import_resolved() -> Self {
        Self {
            evaluation_import_resolved: true,
            ..Self::default()
        }
    }

    pub(crate) fn evaluation_import_pending() -> Self {
        Self {
            evaluation_import_pending: true,
            ..Self::default()
        }
    }

    pub(crate) fn evaluation_import_rejected() -> Self {
        Self {
            evaluation_import_rejected: true,
            dynamic_import_rejected: true,
            ..Self::default()
        }
    }

    pub(crate) fn stale_owner_dropped() -> Self {
        Self {
            stale_owner_dropped: true,
            ..Self::default()
        }
    }

    pub(crate) fn unexpected_complete_recorded() -> Self {
        Self {
            unexpected_complete_recorded: true,
            ..Self::default()
        }
    }

    pub(crate) fn dynamic_import_rejected() -> Self {
        Self {
            dynamic_import_rejected: true,
            ..Self::default()
        }
    }

    pub(crate) fn resume_failed() -> Self {
        Self {
            resume_failed: true,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn made_progress(self) -> bool {
        self.terminal_work_consumed
            || self.missing_joined_client
            || self.missing_joined_terminal_fetch
            || self.owner_action_queue_followup.made_progress()
            || self.owner_module_fetch_settled
            || self.owner_module_fetch_restored
            || self.waiting_fetch_scheduled
            || self.waiting_fetch_missing_loader
            || self.dynamic_import_wait_retained
            || self.source_import_resolved
            || self.source_import_rejected
            || self.evaluation_import_resolved
            || self.evaluation_import_pending
            || self.evaluation_import_rejected
            || self.stale_owner_dropped
            || self.unexpected_complete_recorded
            || self.dynamic_import_rejected
            || self.resume_failed
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.terminal_work_consumed |= other.terminal_work_consumed;
        self.missing_joined_client |= other.missing_joined_client;
        self.missing_joined_terminal_fetch |= other.missing_joined_terminal_fetch;
        self.owner_action_queue_followup
            .merge(other.owner_action_queue_followup);
        self.owner_module_fetch_settled |= other.owner_module_fetch_settled;
        self.owner_module_fetch_restored |= other.owner_module_fetch_restored;
        self.waiting_fetch_scheduled |= other.waiting_fetch_scheduled;
        self.waiting_fetch_missing_loader |= other.waiting_fetch_missing_loader;
        self.dynamic_import_wait_retained |= other.dynamic_import_wait_retained;
        self.source_import_resolved |= other.source_import_resolved;
        self.source_import_rejected |= other.source_import_rejected;
        self.evaluation_import_resolved |= other.evaluation_import_resolved;
        self.evaluation_import_pending |= other.evaluation_import_pending;
        self.evaluation_import_rejected |= other.evaluation_import_rejected;
        self.stale_owner_dropped |= other.stale_owner_dropped;
        self.unexpected_complete_recorded |= other.unexpected_complete_recorded;
        self.dynamic_import_rejected |= other.dynamic_import_rejected;
        self.resume_failed |= other.resume_failed;
    }

    pub(crate) fn owner_action_queue_followup(self) -> FrameDocumentModuleTerminalQueueFollowup {
        self.owner_action_queue_followup
    }

    #[cfg(test)]
    pub(crate) fn terminal_work_was_consumed(self) -> bool {
        self.terminal_work_consumed
    }

    #[cfg(test)]
    pub(crate) fn missing_joined_client_was_recorded(self) -> bool {
        self.missing_joined_client
    }

    #[cfg(test)]
    pub(crate) fn owner_action_was_queued(self) -> bool {
        self.owner_action_queue_followup
            .dynamic_import_owner_action_was_queued()
    }

    #[cfg(test)]
    pub(crate) fn missing_joined_terminal_fetch_was_recorded(self) -> bool {
        self.missing_joined_terminal_fetch
    }

    #[cfg(test)]
    pub(crate) fn owner_module_fetch_was_settled(self) -> bool {
        self.owner_module_fetch_settled
    }

    #[cfg(test)]
    pub(crate) fn owner_module_fetch_was_restored(self) -> bool {
        self.owner_module_fetch_restored
    }

    #[cfg(test)]
    pub(crate) fn waiting_fetch_was_scheduled(self) -> bool {
        self.waiting_fetch_scheduled
    }

    #[cfg(test)]
    pub(crate) fn waiting_fetch_missing_loader_was_recorded(self) -> bool {
        self.waiting_fetch_missing_loader
    }

    #[cfg(test)]
    pub(crate) fn dynamic_import_wait_was_retained(self) -> bool {
        self.dynamic_import_wait_retained
            || self
                .owner_action_queue_followup
                .dynamic_import_wait_was_retained()
    }

    #[cfg(test)]
    pub(crate) fn dynamic_import_job_was_resumed(self) -> bool {
        self.owner_action_queue_followup
            .dynamic_import_job_was_resumed()
    }

    pub(crate) fn source_import_was_resolved(self) -> bool {
        self.source_import_resolved
    }

    pub(crate) fn source_import_was_rejected(self) -> bool {
        self.source_import_rejected
    }

    #[cfg(test)]
    pub(crate) fn evaluation_import_was_continued(self) -> bool {
        self.evaluation_import_resolved || self.evaluation_import_pending
    }

    pub(crate) fn evaluation_import_was_resolved(self) -> bool {
        self.evaluation_import_resolved
    }

    pub(crate) fn evaluation_import_was_pending(self) -> bool {
        self.evaluation_import_pending
    }

    pub(crate) fn evaluation_import_was_rejected(self) -> bool {
        self.evaluation_import_rejected
    }

    #[cfg(test)]
    pub(crate) fn stale_owner_was_dropped(self) -> bool {
        self.stale_owner_dropped
    }

    #[cfg(test)]
    pub(crate) fn unexpected_complete_was_recorded(self) -> bool {
        self.unexpected_complete_recorded
    }

    pub(crate) fn dynamic_import_was_rejected(self) -> bool {
        self.dynamic_import_rejected
    }

    #[cfg(test)]
    pub(crate) fn resume_failure_was_recorded(self) -> bool {
        self.resume_failed
    }
}

impl FrameDocumentDynamicImportTerminalPreparedAction {
    fn new(
        trace: FrameDocumentDynamicImportTerminalTrace,
        action: FrameDocumentDynamicImportOwnerAction,
    ) -> Self {
        Self {
            trace: FrameDocumentDynamicImportOwnerActionTrace::Terminal(trace),
            action,
        }
    }

    pub(crate) fn from_fetch_completion(
        task_owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
        load_id: u64,
        action: FrameDocumentDynamicImportOwnerAction,
    ) -> Self {
        Self {
            trace: FrameDocumentDynamicImportOwnerActionTrace::FetchCompletion {
                task_owner,
                realm_id,
                load_id,
            },
            action,
        }
    }

    pub(crate) fn from_continuation(
        task_owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
        action: FrameDocumentDynamicImportOwnerAction,
    ) -> Self {
        Self {
            trace: FrameDocumentDynamicImportOwnerActionTrace::Continuation {
                task_owner,
                realm_id,
            },
            action,
        }
    }

    pub(crate) fn from_trace(
        trace: FrameDocumentDynamicImportOwnerActionTrace,
        action: FrameDocumentDynamicImportOwnerAction,
    ) -> Self {
        Self { trace, action }
    }

    pub(crate) fn from_terminal_work(work: FrameDocumentDynamicImportTerminalWork) -> Self {
        let (task_owner, realm_id, key, client) = work.into_terminal_parts();
        let action =
            FrameDocumentDynamicImportTerminalClientAction::new(task_owner, realm_id, key, client);
        Self::new(
            FrameDocumentDynamicImportTerminalTrace::from_terminal_client_action(&action),
            FrameDocumentDynamicImportOwnerAction::TerminalClient(action),
        )
    }

    #[cfg(test)]
    pub(crate) fn action(&self) -> &FrameDocumentDynamicImportOwnerAction {
        &self.action
    }

    pub(crate) fn task_owner(&self) -> FrameDocumentTaskOwner {
        self.trace.task_owner()
    }

    pub(crate) fn realm_id(&self) -> FrameRealmId {
        self.trace.realm_id()
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        FrameDocumentDynamicImportOwnerActionTrace,
        FrameDocumentDynamicImportOwnerAction,
    ) {
        (self.trace, self.action)
    }
}

impl FrameDocumentDynamicImportOwnerActionQueueRequest {
    pub(crate) fn waiting(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        fetch_actions: Vec<ChildDynamicModuleFetchAction>,
    ) -> Self {
        Self::Waiting {
            owner,
            realm_id,
            fetch_actions,
        }
    }

    pub(crate) fn continuation(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        action: FrameDocumentDynamicImportOwnerAction,
    ) -> Self {
        Self::Continuation {
            owner,
            realm_id,
            actions: FrameDocumentDynamicImportOwnerActions::one(action),
        }
    }

    pub(crate) fn fetch_completion(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        load_id: u64,
        action: FrameDocumentDynamicImportOwnerAction,
    ) -> Self {
        Self::fetch_completion_actions(
            owner,
            realm_id,
            load_id,
            FrameDocumentDynamicImportOwnerActions::one(action),
        )
    }

    pub(crate) fn fetch_completion_actions(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        load_id: u64,
        actions: FrameDocumentDynamicImportOwnerActions,
    ) -> Self {
        Self::FetchCompletion {
            owner,
            realm_id,
            load_id,
            actions,
        }
    }
}

impl<Hooks> FrameDocumentDynamicImportOwnerActionQueueRunner<Hooks>
where
    Hooks: FrameDocumentDynamicImportOwnerActionQueueHooks,
{
    pub(crate) fn new(hooks: Hooks) -> Self {
        Self { hooks }
    }

    #[cfg(test)]
    pub(crate) fn into_hooks(self) -> Hooks {
        self.hooks
    }

    pub(crate) fn run_queue_request(
        &mut self,
        request: FrameDocumentDynamicImportOwnerActionQueueRequest,
    ) -> FrameDocumentDynamicImportOwnerActionQueueOutcome {
        let queue_action = match self.queue_trace_and_actions(request) {
            FrameDocumentDynamicImportQueueAction::Prepared(trace, actions) => (trace, actions),
            FrameDocumentDynamicImportQueueAction::WaitRetained => {
                return FrameDocumentDynamicImportOwnerActionQueueOutcome::with_followup(
                    FrameDocumentModuleTerminalQueueFollowup::dynamic_import_wait_retained(),
                );
            }
        };
        let (trace, actions) = queue_action;
        let (owner, realm_id) = trace.owner_and_realm();
        let task_owner = match self
            .hooks
            .current_dynamic_import_task_owner(owner, realm_id)
        {
            FrameDocumentDynamicImportQueueTaskOwnerResult::Current(task_owner) => task_owner,
            FrameDocumentDynamicImportQueueTaskOwnerResult::Stale => {
                self.hooks.record_stale_dynamic_import_owner_action(trace);
                return FrameDocumentDynamicImportOwnerActionQueueOutcome::stale_owner();
            }
        };
        let prepared_actions = actions
            .into_actions()
            .into_iter()
            .map(|action| match trace {
                FrameDocumentDynamicImportOwnerActionQueueTrace::Continuation { .. }
                | FrameDocumentDynamicImportOwnerActionQueueTrace::Waiting { .. } => {
                    FrameDocumentDynamicImportTerminalPreparedAction::from_continuation(
                        task_owner, realm_id, action,
                    )
                }
                FrameDocumentDynamicImportOwnerActionQueueTrace::FetchCompletion {
                    load_id,
                    ..
                } => FrameDocumentDynamicImportTerminalPreparedAction::from_fetch_completion(
                    task_owner, realm_id, load_id, action,
                ),
            })
            .collect();
        let followup = self
            .hooks
            .queue_dynamic_import_owner_actions(prepared_actions);
        FrameDocumentDynamicImportOwnerActionQueueOutcome::with_followup(followup)
    }

    fn queue_trace_and_actions(
        &mut self,
        request: FrameDocumentDynamicImportOwnerActionQueueRequest,
    ) -> FrameDocumentDynamicImportQueueAction {
        match request {
            FrameDocumentDynamicImportOwnerActionQueueRequest::Waiting {
                owner,
                realm_id,
                fetch_actions,
            } => self.waiting_queue_trace_and_actions(owner, realm_id, fetch_actions),
            FrameDocumentDynamicImportOwnerActionQueueRequest::Continuation {
                owner,
                realm_id,
                actions,
            } => FrameDocumentDynamicImportQueueAction::Prepared(
                FrameDocumentDynamicImportOwnerActionQueueTrace::Continuation { owner, realm_id },
                actions,
            ),
            FrameDocumentDynamicImportOwnerActionQueueRequest::FetchCompletion {
                owner,
                realm_id,
                load_id,
                actions,
            } => FrameDocumentDynamicImportQueueAction::Prepared(
                FrameDocumentDynamicImportOwnerActionQueueTrace::FetchCompletion {
                    owner,
                    realm_id,
                    load_id,
                },
                actions,
            ),
        }
    }

    fn waiting_queue_trace_and_actions(
        &mut self,
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        fetch_actions: Vec<ChildDynamicModuleFetchAction>,
    ) -> FrameDocumentDynamicImportQueueAction {
        let fetch_count = fetch_actions.len();
        let Some(actions) =
            FrameDocumentDynamicImportOwnerActions::waiting(owner, realm_id, fetch_actions)
        else {
            return FrameDocumentDynamicImportQueueAction::WaitRetained;
        };
        FrameDocumentDynamicImportQueueAction::Prepared(
            FrameDocumentDynamicImportOwnerActionQueueTrace::Waiting {
                owner,
                realm_id,
                fetch_count,
            },
            actions,
        )
    }
}

enum FrameDocumentDynamicImportQueueAction {
    Prepared(
        FrameDocumentDynamicImportOwnerActionQueueTrace,
        FrameDocumentDynamicImportOwnerActions,
    ),
    WaitRetained,
}

impl FrameDocumentDynamicImportOwnerActionQueueTrace {
    fn owner_and_realm(self) -> (FrameDocumentOwner, FrameRealmId) {
        match self {
            Self::Continuation { owner, realm_id }
            | Self::FetchCompletion {
                owner, realm_id, ..
            }
            | Self::Waiting {
                owner, realm_id, ..
            } => (owner, realm_id),
        }
    }
}

impl std::fmt::Debug for FrameDocumentDynamicImportTerminalPreparedAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FrameDocumentDynamicImportTerminalPreparedAction")
            .field("trace", &self.trace)
            .field("action", &self.action.kind_label())
            .finish()
    }
}

impl FrameDocumentDynamicImportOwnerAction {
    pub(crate) fn waiting(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        fetch_action: ChildDynamicModuleFetchAction,
    ) -> Self {
        Self::Waiting(FrameDocumentDynamicImportWaitingAction::new(
            owner,
            realm_id,
            fetch_action,
        ))
    }

    pub(crate) fn ready(dynamic_import: NativeDynamicModuleImportReady) -> Self {
        Self::Ready(Box::new(
            FrameDocumentDynamicImportReadyAction::from_dynamic_import(dynamic_import),
        ))
    }

    pub(crate) fn graph_advance_failed(job: NativeModuleGraphJob, error: ModuleLoadError) -> Self {
        Self::Reject(Box::new(FrameDocumentDynamicImportRejectAction {
            reason: FrameDocumentDynamicImportRejectReason::GraphAdvanceFailure,
            request: job.into_dynamic_import_request(),
            error,
        }))
    }

    pub(crate) fn fetch_failed(
        request: PendingDynamicModuleImport,
        error: ModuleLoadError,
    ) -> Self {
        Self::Reject(Box::new(FrameDocumentDynamicImportRejectAction {
            reason: FrameDocumentDynamicImportRejectReason::FetchFailure,
            request,
            error,
        }))
    }

    fn kind_label(&self) -> &'static str {
        match self {
            Self::TerminalClient { .. } => "terminal-client",
            Self::OwnerModuleFetchCompleted { .. } => "owner-module-fetch-completed",
            Self::Waiting(_) => "waiting",
            Self::Ready(_) => "ready",
            Self::Reject(action) => action.kind_label(),
        }
    }
}

impl FrameDocumentDynamicImportOwnerActions {
    fn one(action: FrameDocumentDynamicImportOwnerAction) -> Self {
        Self {
            actions: vec![action],
        }
    }

    fn waiting(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        fetch_actions: Vec<ChildDynamicModuleFetchAction>,
    ) -> Option<Self> {
        if fetch_actions.is_empty() {
            return None;
        }
        Some(Self {
            actions: fetch_actions
                .into_iter()
                .map(|fetch_action| {
                    FrameDocumentDynamicImportOwnerAction::waiting(owner, realm_id, fetch_action)
                })
                .collect(),
        })
    }

    pub(crate) fn into_actions(self) -> Vec<FrameDocumentDynamicImportOwnerAction> {
        self.actions
    }

    #[cfg(test)]
    pub(crate) fn into_single_for_test(self) -> FrameDocumentDynamicImportOwnerAction {
        let mut actions = self.actions;
        assert_eq!(actions.len(), 1, "expected one dynamic-import owner action");
        actions
            .pop()
            .expect("single dynamic-import owner action must remain present")
    }
}

impl FrameDocumentDynamicImportReadyAction {
    fn from_dynamic_import(dynamic_import: NativeDynamicModuleImportReady) -> Self {
        let NativeDynamicModuleImportReady { job, graph } = dynamic_import;
        let request = job.into_dynamic_import_request();
        if request.phase() == ModuleImportPhase::Source {
            Self::Source(FrameDocumentDynamicImportSourceReadyAction {
                request,
                root_entry: graph.root_entry,
            })
        } else {
            Self::Evaluation(FrameDocumentDynamicImportEvaluationReadyAction { request, graph })
        }
    }

    #[cfg(test)]
    pub(crate) fn root_entry(&self) -> ModuleEntryId {
        match self {
            Self::Source(action) => action.root_entry(),
            Self::Evaluation(action) => action.root_entry(),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_source_phase(&self) -> bool {
        matches!(self, Self::Source(_))
    }

    #[cfg(test)]
    pub(crate) fn is_evaluation_phase(&self) -> bool {
        matches!(self, Self::Evaluation(_))
    }
}

impl FrameDocumentDynamicImportWaitingAction {
    fn new(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        fetch_action: ChildDynamicModuleFetchAction,
    ) -> Self {
        Self {
            owner,
            realm_id,
            fetch_action,
        }
    }

    fn into_parts(
        self,
    ) -> (
        FrameDocumentOwner,
        FrameRealmId,
        ChildDynamicModuleFetchAction,
    ) {
        (self.owner, self.realm_id, self.fetch_action)
    }
}

impl FrameDocumentDynamicImportTerminalClientAction {
    fn new(
        task_owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
        key: ModuleMapKey,
        client: NativeDynamicImportSingleModuleClient,
    ) -> Self {
        Self {
            task_owner,
            realm_id,
            key,
            client,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        FrameDocumentTaskOwner,
        FrameRealmId,
        ModuleMapKey,
        NativeDynamicImportSingleModuleClient,
    ) {
        (self.task_owner, self.realm_id, self.key, self.client)
    }

    pub(crate) fn task_owner(&self) -> FrameDocumentTaskOwner {
        self.task_owner
    }

    pub(crate) fn realm_id(&self) -> FrameRealmId {
        self.realm_id
    }

    pub(crate) fn key(&self) -> &ModuleMapKey {
        &self.key
    }

    pub(crate) fn client_token(&self) -> module_tree::SingleModuleClientToken {
        self.client.token()
    }

    pub(crate) fn import_phase(&self) -> module_tree::ModuleImportPhase {
        self.client.import_phase()
    }
}

impl FrameDocumentDynamicImportSourceReadyAction {
    pub(crate) fn into_parts(self) -> (PendingDynamicModuleImport, ModuleEntryId) {
        (self.request, self.root_entry)
    }

    #[cfg(test)]
    pub(crate) fn root_entry(&self) -> ModuleEntryId {
        self.root_entry
    }
}

impl FrameDocumentDynamicImportEvaluationReadyAction {
    pub(crate) fn into_parts(self) -> (PendingDynamicModuleImport, ModuleGraphHandle) {
        (self.request, self.graph)
    }

    #[cfg(test)]
    pub(crate) fn root_entry(&self) -> ModuleEntryId {
        self.graph.root_entry
    }
}

impl FrameDocumentDynamicImportRejectAction {
    fn kind_label(&self) -> &'static str {
        match self.reason {
            FrameDocumentDynamicImportRejectReason::FetchFailure => "fetch-failed",
            FrameDocumentDynamicImportRejectReason::GraphAdvanceFailure => "graph-advance-failed",
        }
    }

    pub(crate) fn into_parts(self) -> (PendingDynamicModuleImport, ModuleLoadError) {
        (self.request, self.error)
    }

    #[cfg(test)]
    pub(crate) fn reason(&self) -> FrameDocumentDynamicImportRejectReason {
        self.reason
    }

    #[cfg(test)]
    pub(crate) fn request(&self) -> &PendingDynamicModuleImport {
        &self.request
    }

    #[cfg(test)]
    pub(crate) fn error(&self) -> &ModuleLoadError {
        &self.error
    }
}

impl<Hooks> FrameDocumentDynamicImportOwnerActionRunner<Hooks>
where
    Hooks: FrameDocumentDynamicImportOwnerActionHooks,
{
    pub(crate) fn new(hooks: Hooks) -> Self {
        Self { hooks }
    }

    #[cfg(test)]
    pub(crate) fn into_hooks(self) -> Hooks {
        self.hooks
    }

    pub(crate) fn run_prepared_action(
        &mut self,
        prepared_action: FrameDocumentDynamicImportTerminalPreparedAction,
    ) -> FrameDocumentDynamicImportTerminalOutcome {
        let (trace, action) = prepared_action.into_parts();
        let diagnostic = trace.diagnostic();
        match self.run_owner_action(action) {
            Ok(outcome) => {
                self.hooks.record_action_resumed(diagnostic);
                outcome
            }
            Err(error) => {
                self.hooks.record_action_failed(diagnostic, &error);
                FrameDocumentDynamicImportTerminalOutcome::resume_failed()
            }
        }
    }

    pub(crate) fn run_owner_action(
        &mut self,
        action: FrameDocumentDynamicImportOwnerAction,
    ) -> Result<FrameDocumentDynamicImportTerminalOutcome, String> {
        match action {
            FrameDocumentDynamicImportOwnerAction::TerminalClient(action) => {
                self.run_terminal_client_action(action)
            }
            FrameDocumentDynamicImportOwnerAction::OwnerModuleFetchCompleted {
                load_id,
                settle,
                restore,
            } => self.run_owner_module_fetch_completion(load_id, settle, restore),
            FrameDocumentDynamicImportOwnerAction::Waiting(action) => {
                self.run_waiting_fetch(action)
            }
            FrameDocumentDynamicImportOwnerAction::Ready(ready_action) => {
                self.run_ready_import(*ready_action)
            }
            FrameDocumentDynamicImportOwnerAction::Reject(action) => self
                .hooks
                .reject_dynamic_import(*action)
                .map(|result| match result {
                    FrameDocumentDynamicImportRejectResult::Rejected => {
                        FrameDocumentDynamicImportTerminalOutcome::dynamic_import_rejected()
                    }
                    FrameDocumentDynamicImportRejectResult::DroppedStaleOwner => {
                        FrameDocumentDynamicImportTerminalOutcome::stale_owner_dropped()
                    }
                }),
        }
    }

    fn run_terminal_client_action(
        &mut self,
        action: FrameDocumentDynamicImportTerminalClientAction,
    ) -> Result<FrameDocumentDynamicImportTerminalOutcome, String> {
        let trace = FrameDocumentDynamicImportTerminalTrace::from_terminal_client_action(&action);
        let mut outcome = FrameDocumentDynamicImportTerminalOutcome::terminal_work_consumed();
        let finish = self.hooks.finish_terminal_client(action)?;
        let actions = match finish {
            FrameDocumentDynamicImportTerminalClientFinishResult::MissingJoinedClient => {
                self.hooks.record_missing_joined_terminal_client(
                    trace.missing_joined_terminal_client(),
                )?;
                outcome.merge(FrameDocumentDynamicImportTerminalOutcome::missing_joined_client());
                return Ok(outcome);
            }
            FrameDocumentDynamicImportTerminalClientFinishResult::WaitRetained => {
                outcome.merge(
                    FrameDocumentDynamicImportTerminalOutcome::dynamic_import_wait_retained(),
                );
                return Ok(outcome);
            }
            FrameDocumentDynamicImportTerminalClientFinishResult::RestoredAfterUnexpectedComplete => {
                self.hooks
                    .record_restored_after_unexpected_complete(trace.diagnostic())?;
                outcome.merge(
                    FrameDocumentDynamicImportTerminalOutcome::unexpected_complete_recorded(),
                );
                return Ok(outcome);
            }
            FrameDocumentDynamicImportTerminalClientFinishResult::FollowupActions(actions) => {
                actions
            }
        };
        let prepared_actions = actions
            .into_actions()
            .into_iter()
            .map(|action| {
                FrameDocumentDynamicImportTerminalPreparedAction::from_trace(
                    FrameDocumentDynamicImportOwnerActionTrace::Terminal(trace.clone()),
                    action,
                )
            })
            .collect();
        let followup = self.hooks.queue_owner_action_followups(prepared_actions)?;
        outcome.merge(
            FrameDocumentDynamicImportTerminalOutcome::from_owner_action_queue_followup(followup),
        );
        Ok(outcome)
    }

    fn run_ready_import(
        &mut self,
        ready_action: FrameDocumentDynamicImportReadyAction,
    ) -> Result<FrameDocumentDynamicImportTerminalOutcome, String> {
        match ready_action {
            FrameDocumentDynamicImportReadyAction::Source(action) => self
                .hooks
                .resolve_ready_source_import(action)
                .map(|result| match result {
                    FrameDocumentDynamicImportSourceReadyResult::Resolved => {
                        FrameDocumentDynamicImportTerminalOutcome::source_import_resolved()
                    }
                    FrameDocumentDynamicImportSourceReadyResult::Rejected => {
                        FrameDocumentDynamicImportTerminalOutcome::source_import_rejected()
                    }
                    FrameDocumentDynamicImportSourceReadyResult::DroppedStaleOwner => {
                        FrameDocumentDynamicImportTerminalOutcome::stale_owner_dropped()
                    }
                }),
            FrameDocumentDynamicImportReadyAction::Evaluation(action) => self
                .hooks
                .continue_ready_evaluation_import(action)
                .map(|result| match result {
                    FrameDocumentDynamicImportEvaluationReadyResult::Resolved => {
                        FrameDocumentDynamicImportTerminalOutcome::evaluation_import_resolved()
                    }
                    FrameDocumentDynamicImportEvaluationReadyResult::Pending => {
                        FrameDocumentDynamicImportTerminalOutcome::evaluation_import_pending()
                    }
                    FrameDocumentDynamicImportEvaluationReadyResult::Rejected => {
                        FrameDocumentDynamicImportTerminalOutcome::evaluation_import_rejected()
                    }
                    FrameDocumentDynamicImportEvaluationReadyResult::DroppedStaleOwner => {
                        FrameDocumentDynamicImportTerminalOutcome::stale_owner_dropped()
                    }
                }),
        }
    }

    fn run_owner_module_fetch_completion(
        &mut self,
        load_id: u64,
        settle: ChildDynamicModuleOwnerFetchCompletionSettlementAction,
        restore: ChildDynamicModuleCompletedFetchRestoreAction,
    ) -> Result<FrameDocumentDynamicImportTerminalOutcome, String> {
        let owner = restore.owner();
        let realm_id = restore.realm_id();
        let missing_fetch =
            FrameDocumentDynamicImportMissingJoinedTerminalFetch::new(owner, realm_id, load_id);
        let mut outcome = FrameDocumentDynamicImportTerminalOutcome::default();
        if self
            .hooks
            .settle_owner_module_fetch_completion(settle)?
            .settled()
        {
            outcome.merge(FrameDocumentDynamicImportTerminalOutcome::owner_module_fetch_settled());
        }
        let restore_result = self
            .hooks
            .restore_completed_owner_module_fetch_as_joined_terminal_client(restore)?;
        if restore_result.restored() {
            outcome.merge(FrameDocumentDynamicImportTerminalOutcome::owner_module_fetch_restored());
        } else {
            self.hooks
                .record_missing_joined_terminal_fetch(missing_fetch)?;
            outcome
                .merge(FrameDocumentDynamicImportTerminalOutcome::missing_joined_terminal_fetch());
        }
        Ok(outcome)
    }

    fn run_waiting_fetch(
        &mut self,
        action: FrameDocumentDynamicImportWaitingAction,
    ) -> Result<FrameDocumentDynamicImportTerminalOutcome, String> {
        let (owner, realm_id, fetch_action) = action.into_parts();
        let mut outcome = FrameDocumentDynamicImportTerminalOutcome::default();
        match fetch_action {
            ChildDynamicModuleFetchAction::Schedule(fetch) => {
                let action = FrameDocumentDynamicImportWaitingFetchScheduleAction::new(
                    owner, realm_id, fetch,
                );
                match self.hooks.schedule_waiting_fetch(action)? {
                    FrameDocumentDynamicImportWaitingFetchScheduleResult::Scheduled => {
                        outcome.merge(
                            FrameDocumentDynamicImportTerminalOutcome::waiting_fetch_scheduled(),
                        );
                    }
                    FrameDocumentDynamicImportWaitingFetchScheduleResult::MissingLoader => {
                        outcome.merge(
                            FrameDocumentDynamicImportTerminalOutcome::waiting_fetch_missing_loader(
                            ),
                        );
                    }
                    FrameDocumentDynamicImportWaitingFetchScheduleResult::StaleOwner => {
                        outcome.merge(
                            FrameDocumentDynamicImportTerminalOutcome::stale_owner_dropped(),
                        );
                    }
                }
            }
            ChildDynamicModuleFetchAction::RestoreOwnerTerminalWithoutNetwork {
                settle,
                restore,
            } => {
                let load_id = restore.load_id();
                let missing_fetch = FrameDocumentDynamicImportMissingJoinedTerminalFetch::new(
                    owner, realm_id, load_id,
                );
                if self
                    .hooks
                    .finish_owner_module_fetch_without_network(settle)?
                    .settled()
                {
                    outcome.merge(
                        FrameDocumentDynamicImportTerminalOutcome::owner_module_fetch_settled(),
                    );
                }
                let restore_action = FrameDocumentDynamicImportOwnerTerminalRestoreAction::new(
                    owner, realm_id, restore,
                );
                let restore_result = self
                    .hooks
                    .restore_scheduled_fetch_as_joined_terminal_client(restore_action)?;
                if restore_result.restored() {
                    outcome.merge(
                        FrameDocumentDynamicImportTerminalOutcome::owner_module_fetch_restored(),
                    );
                } else {
                    self.hooks
                        .record_missing_joined_terminal_fetch(missing_fetch)?;
                    outcome.merge(
                        FrameDocumentDynamicImportTerminalOutcome::missing_joined_terminal_fetch(),
                    );
                }
            }
        }
        Ok(outcome)
    }
}

impl ChildDynamicModuleFetchAction {
    fn from_scheduled_fetch(scheduled_fetch: DynamicModuleScheduledFetch) -> Self {
        if dynamic_module_scheduled_fetch_waits_for_owner_terminal_without_network(&scheduled_fetch)
        {
            let owner_start = scheduled_fetch
                .owner_module_fetch_start()
                .cloned()
                .expect("owner-terminal scheduled fetches have an owner module fetch start");
            let settle =
                ChildDynamicModuleOwnerFetchWithoutNetworkSettlementAction::new(owner_start);
            let restore =
                ChildDynamicModuleOwnerTerminalRestoreAction::from_scheduled_fetch(scheduled_fetch);
            Self::RestoreOwnerTerminalWithoutNetwork { settle, restore }
        } else {
            Self::Schedule(ChildDynamicModuleFetchScheduleAction::from_scheduled_fetch(
                scheduled_fetch,
            ))
        }
    }

    pub(crate) fn wrap_all(scheduled: Vec<DynamicModuleScheduledFetch>) -> Vec<Self> {
        scheduled
            .into_iter()
            .map(Self::from_scheduled_fetch)
            .collect()
    }
}

impl From<DynamicModuleScheduledFetch> for ChildDynamicModuleFetchAction {
    fn from(scheduled_fetch: DynamicModuleScheduledFetch) -> Self {
        Self::from_scheduled_fetch(scheduled_fetch)
    }
}

impl ChildDynamicModuleFetchScheduleAction {
    fn from_scheduled_fetch(scheduled_fetch: DynamicModuleScheduledFetch) -> Self {
        let (load_id, request, _) = scheduled_fetch.into_parts();
        Self {
            load_id,
            request: Box::new(request),
        }
    }

    pub(crate) fn into_parts(self) -> (u64, NativeModuleGraphFetchRequest) {
        (self.load_id, *self.request)
    }
}

impl FrameDocumentDynamicImportWaitingFetchScheduleAction {
    fn new(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        fetch: ChildDynamicModuleFetchScheduleAction,
    ) -> Self {
        Self {
            owner,
            realm_id,
            fetch,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        FrameDocumentOwner,
        FrameRealmId,
        ChildDynamicModuleFetchScheduleAction,
    ) {
        (self.owner, self.realm_id, self.fetch)
    }

    #[cfg(test)]
    pub(crate) fn owner(&self) -> FrameDocumentOwner {
        self.owner
    }

    #[cfg(test)]
    pub(crate) fn realm_id(&self) -> FrameRealmId {
        self.realm_id
    }
}

impl ChildDynamicModuleCompletedFetchRestoreAction {
    pub(crate) fn new(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        inflight: DynamicModuleInflightFetch,
    ) -> Self {
        Self {
            owner,
            realm_id,
            inflight: Box::new(inflight),
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (FrameDocumentOwner, FrameRealmId, DynamicModuleInflightFetch) {
        (self.owner, self.realm_id, *self.inflight)
    }

    pub(crate) fn owner(&self) -> FrameDocumentOwner {
        self.owner
    }

    pub(crate) fn realm_id(&self) -> FrameRealmId {
        self.realm_id
    }
}

impl ChildDynamicModuleOwnerFetchCompletionSettlementAction {
    pub(crate) fn new(
        start: FrameDocumentModuleFetchClientStart,
        source: Result<ModuleGraphFetchedSource, ModuleLoadError>,
    ) -> Self {
        Self {
            start,
            source: Box::new(source),
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        FrameDocumentModuleFetchClientStart,
        Result<ModuleGraphFetchedSource, ModuleLoadError>,
    ) {
        (self.start, *self.source)
    }

    #[cfg(test)]
    pub(crate) fn start(&self) -> &FrameDocumentModuleFetchClientStart {
        &self.start
    }

    #[cfg(test)]
    pub(crate) fn source(&self) -> &Result<ModuleGraphFetchedSource, ModuleLoadError> {
        &self.source
    }
}

impl ChildDynamicModuleOwnerFetchWithoutNetworkSettlementAction {
    fn new(start: FrameDocumentModuleFetchClientStart) -> Self {
        Self { start }
    }

    pub(crate) fn into_start(self) -> FrameDocumentModuleFetchClientStart {
        self.start
    }
}

impl ChildDynamicModuleOwnerTerminalRestoreAction {
    fn from_scheduled_fetch(scheduled_fetch: DynamicModuleScheduledFetch) -> Self {
        let load_id = scheduled_fetch.load_id();
        Self { load_id }
    }

    pub(crate) fn load_id(&self) -> u64 {
        self.load_id
    }
}

impl FrameDocumentDynamicImportOwnerTerminalRestoreAction {
    fn new(
        owner: FrameDocumentOwner,
        realm_id: FrameRealmId,
        restore: ChildDynamicModuleOwnerTerminalRestoreAction,
    ) -> Self {
        Self {
            owner,
            realm_id,
            restore,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        FrameDocumentOwner,
        FrameRealmId,
        ChildDynamicModuleOwnerTerminalRestoreAction,
    ) {
        (self.owner, self.realm_id, self.restore)
    }

    #[cfg(test)]
    pub(crate) fn owner(&self) -> FrameDocumentOwner {
        self.owner
    }

    #[cfg(test)]
    pub(crate) fn realm_id(&self) -> FrameRealmId {
        self.realm_id
    }
}

fn dynamic_module_scheduled_fetch_waits_for_owner_terminal_without_network(
    scheduled_fetch: &DynamicModuleScheduledFetch,
) -> bool {
    scheduled_fetch
        .owner_module_fetch_start()
        .is_some_and(|start| {
            !matches!(
                start.registration().fetch_disposition(),
                FrameDocumentModuleFetchDisposition::StartedFetch(_)
            )
        })
}

impl FrameDocumentDynamicImportMissingJoinedTerminalFetch {
    pub(crate) fn new(owner: FrameDocumentOwner, realm_id: FrameRealmId, load_id: u64) -> Self {
        Self {
            owner,
            realm_id,
            load_id,
        }
    }

    pub(crate) fn owner(self) -> FrameDocumentOwner {
        self.owner
    }

    pub(crate) fn realm_id(self) -> FrameRealmId {
        self.realm_id
    }

    pub(crate) fn load_id(self) -> u64 {
        self.load_id
    }
}
