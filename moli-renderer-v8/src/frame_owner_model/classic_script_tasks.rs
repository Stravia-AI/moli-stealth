use super::{
    records::{
        DocumentId, DocumentLoadDelayTokenId, FrameDocumentOwner, FrameDocumentTaskOwner,
        FrameRealmId, FrameRequestId, FrameScriptJob, FrameScriptJobKind,
    },
    script_events::FrameDocumentScriptElementEvent,
};
use crate::document_runtime::DomHandle;
use crate::document_script_scheduler::{
    FrameDocumentClassicScriptSchedulerWork, ParserPendingScriptKey,
};
use crate::document_task_lane::DocumentRealmTask;
use crate::parser_script::action::{
    ParserClassicScriptBeginExecutionAction, ParserClassicScriptCompletionAction,
    ParserClassicScriptExecutionStart, ParserClassicScriptScheduling,
    ParserClassicScriptSourceFailureAction, ParserClassicScriptSourceLoadClientAction,
    ParserClassicScriptSourceLoadCompletionAction, ParserClassicScriptSourceLoadRequestAction,
    ParserPendingClassicScriptBeginExecutionAction, ParserPendingClassicScriptReadyKind,
    ParserPendingClassicScriptSourceLoadAction, ParserPendingClassicScriptSourceLoadClientAction,
    ParserPendingClassicScriptSourceLoadCompletionAction,
};

pub(crate) type FrameDocumentClassicScriptSourceLoadTask = DocumentRealmTask<
    FrameDocumentTaskOwner,
    FrameRealmId,
    FrameDocumentClassicScriptSourceLoadTaskPayload,
>;

/// Execution-produced result of consuming one exact classic source-start
/// reservation. This is not a fetch terminal: the successful request and any
/// rejected parser successor remain work for later typed sources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FrameDocumentClassicScriptSourceLoadStartOutcome {
    NetworkRequestStarted,
    RejectedBeforeNetworkStart,
}

pub(crate) type FrameDocumentClassicScriptBeginExecutionAction =
    ParserClassicScriptBeginExecutionAction<FrameDocumentClassicScriptTarget>;

pub(crate) type FrameDocumentClassicScriptCompletionAction = ParserClassicScriptCompletionAction<
    FrameDocumentClassicScriptCompletionTarget,
    FrameDocumentScriptElementEvent,
>;

pub(crate) type FrameDocumentClassicScriptSourceFailureAction =
    ParserClassicScriptSourceFailureAction<
        FrameDocumentClassicScriptSourceFailureTarget,
        FrameDocumentScriptElementEvent,
    >;

pub(crate) type FrameDocumentClassicScriptSourceLoadClient =
    ParserClassicScriptSourceLoadClientAction<FrameDocumentClassicScriptSourceLoadClientTarget>;

pub(crate) type FrameDocumentClassicScriptSourceLoadRequest =
    ParserClassicScriptSourceLoadRequestAction<FrameDocumentClassicScriptSourceLoadRequestTarget>;

pub(crate) type FrameDocumentClassicScriptSourceLoadCompletionAction =
    ParserClassicScriptSourceLoadCompletionAction<
        FrameDocumentClassicScriptSourceLoadCompletionTarget,
    >;

pub(crate) type FrameClassicDocumentScriptExecutionStart = ParserClassicScriptExecutionStart<
    FrameClassicDocumentScriptExecutionAction,
    FrameDocumentClassicScriptCompletionAction,
>;

pub(crate) type FrameDocumentClassicScriptScheduling = ParserClassicScriptScheduling;

#[derive(Debug, Clone)]
pub(crate) struct FrameDocumentClassicScriptExecutionFinish {
    pub(crate) child_handle: DomHandle,
    pub(crate) owner: FrameDocumentOwner,
    pub(crate) task_owner: FrameDocumentTaskOwner,
    pub(crate) realm_id: FrameRealmId,
    pub(crate) script_handle: DomHandle,
    pub(crate) script_url: url::Url,
    pub(crate) script_base_url: url::Url,
    pub(crate) scheduling: FrameDocumentClassicScriptScheduling,
    pub(crate) pending_script_key: Option<ParserPendingScriptKey>,
    pub(crate) load_delay_token: Option<DocumentLoadDelayTokenId>,
}

impl FrameDocumentClassicScriptExecutionFinish {
    #[cfg(test)]
    pub(crate) fn target(&self) -> FrameDocumentClassicScriptCompletionTarget {
        FrameDocumentClassicScriptCompletionTarget::new(
            self.child_handle,
            self.task_owner,
            self.realm_id,
        )
        .with_scheduling(self.scheduling)
        .with_pending_script_key(self.pending_script_key)
        .with_load_delay_token(self.load_delay_token)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FrameClassicDocumentScriptExecutionAction {
    job: FrameScriptJob,
    finish: FrameDocumentClassicScriptExecutionFinish,
}

impl FrameClassicDocumentScriptExecutionAction {
    pub(crate) fn new(
        job: FrameScriptJob,
        finish: FrameDocumentClassicScriptExecutionFinish,
    ) -> Self {
        Self { job, finish }
    }

    pub(crate) fn into_parts(self) -> (FrameScriptJob, FrameDocumentClassicScriptExecutionFinish) {
        (self.job, self.finish)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentClassicPrepareDropReason {
    RealmMaterializationFailed,
    MissingCurrentRealm,
    StaleRealm,
    StaleRunnerOwner,
    MovedFromOriginalDocumentWithoutCompletion,
    StaleDocumentOwner,
    BeginExecutionUnavailable,
    ExecutionActionUnavailable,
    StaleParserSuspension,
}

#[derive(Debug, Clone)]
pub(crate) struct FrameDocumentClassicPrepareApplication {
    start: FrameClassicDocumentScriptExecutionStart,
    drop_reason: Option<FrameDocumentClassicPrepareDropReason>,
}

impl FrameDocumentClassicPrepareApplication {
    pub(crate) fn started(start: FrameClassicDocumentScriptExecutionStart) -> Self {
        Self {
            start,
            drop_reason: None,
        }
    }

    pub(crate) fn dropped(reason: FrameDocumentClassicPrepareDropReason) -> Self {
        Self {
            start: FrameClassicDocumentScriptExecutionStart::Dropped,
            drop_reason: Some(reason),
        }
    }

    pub(crate) fn drop_reason(&self) -> Option<FrameDocumentClassicPrepareDropReason> {
        self.drop_reason
    }

    pub(crate) fn into_start(self) -> FrameClassicDocumentScriptExecutionStart {
        self.start
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicPrepareFollowup {
    realm_materialization_attempted: bool,
    realm_materialized: bool,
    execution_prepared: bool,
    completion_produced: bool,
    drop_reason: Option<FrameDocumentClassicPrepareDropReason>,
}

impl FrameDocumentClassicPrepareFollowup {
    pub(crate) fn note_realm_materialization_attempted(&mut self) {
        self.realm_materialization_attempted = true;
    }

    pub(crate) fn note_realm_materialized(&mut self) {
        self.realm_materialized = true;
    }

    pub(crate) fn note_execution_prepared(&mut self) {
        self.execution_prepared = true;
    }

    pub(crate) fn note_completion_produced(&mut self) {
        self.completion_produced = true;
    }

    pub(crate) fn note_dropped(&mut self, reason: FrameDocumentClassicPrepareDropReason) {
        self.drop_reason = Some(reason);
    }

    pub(crate) fn made_progress(self) -> bool {
        self.realm_materialization_attempted
            || self.realm_materialized
            || self.execution_prepared
            || self.completion_produced
            || self.drop_reason.is_some()
    }

    #[cfg(test)]
    pub(crate) fn drop_reason(self) -> Option<FrameDocumentClassicPrepareDropReason> {
        self.drop_reason
    }

    #[cfg(test)]
    pub(crate) fn execution_was_prepared(self) -> bool {
        self.execution_prepared
    }

    #[cfg(test)]
    pub(crate) fn completion_was_produced(self) -> bool {
        self.completion_produced
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicExecutionFollowup {
    script_job_attempted: bool,
    script_job_failed: bool,
    completion_produced: bool,
}

impl FrameDocumentClassicExecutionFollowup {
    pub(crate) fn note_script_job_attempted(&mut self) {
        self.script_job_attempted = true;
    }

    pub(crate) fn note_script_job_failed(&mut self) {
        self.script_job_failed = true;
    }

    pub(crate) fn note_completion_produced(&mut self) {
        self.completion_produced = true;
    }

    pub(crate) fn made_progress(self) -> bool {
        self.script_job_attempted || self.script_job_failed || self.completion_produced
    }

    pub(crate) fn script_job_was_attempted(self) -> bool {
        self.script_job_attempted
    }

    #[cfg(test)]
    pub(crate) fn script_job_failed(self) -> bool {
        self.script_job_failed
    }

    #[cfg(test)]
    pub(crate) fn completion_was_produced(self) -> bool {
        self.completion_produced
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FrameDocumentClassicSourceFailureReportApplication {
    completion: Option<FrameDocumentClassicScriptCompletionAction>,
    skip_reason: Option<FrameDocumentClassicSourceFailureReportSkipReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentClassicSourceFailureReportSkipReason {
    RealmMaterializationFailed,
    MissingCurrentRealm,
    StaleRealm,
    StaleRunnerOwner,
}

impl FrameDocumentClassicSourceFailureReportApplication {
    pub(crate) fn completed(completion: FrameDocumentClassicScriptCompletionAction) -> Self {
        Self {
            completion: Some(completion),
            skip_reason: None,
        }
    }

    pub(crate) fn skipped(reason: FrameDocumentClassicSourceFailureReportSkipReason) -> Self {
        Self {
            completion: None,
            skip_reason: Some(reason),
        }
    }

    pub(crate) fn skip_reason(&self) -> Option<FrameDocumentClassicSourceFailureReportSkipReason> {
        self.skip_reason
    }

    pub(crate) fn into_completion(self) -> Option<FrameDocumentClassicScriptCompletionAction> {
        self.completion
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicSourceFailureReportFollowup {
    failure_logged: bool,
    completion_produced: bool,
    skip_reason: Option<FrameDocumentClassicSourceFailureReportSkipReason>,
}

impl FrameDocumentClassicSourceFailureReportFollowup {
    pub(crate) fn note_failure_logged(&mut self) {
        self.failure_logged = true;
    }

    pub(crate) fn note_completion_produced(&mut self) {
        self.completion_produced = true;
    }

    pub(crate) fn note_skipped(
        &mut self,
        reason: FrameDocumentClassicSourceFailureReportSkipReason,
    ) {
        self.skip_reason = Some(reason);
    }

    pub(crate) fn made_progress(self) -> bool {
        self.failure_logged || self.completion_produced || self.skip_reason.is_some()
    }

    #[cfg(test)]
    pub(crate) fn failure_was_logged(self) -> bool {
        self.failure_logged
    }

    #[cfg(test)]
    pub(crate) fn completion_was_produced(self) -> bool {
        self.completion_produced
    }

    #[cfg(test)]
    pub(crate) fn skip_reason(self) -> Option<FrameDocumentClassicSourceFailureReportSkipReason> {
        self.skip_reason
    }
}

pub(crate) struct FrameDocumentClassicCompletionFinishAction {
    target: FrameDocumentClassicScriptCompletionTarget,
    script_element_event: Option<FrameDocumentScriptElementEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicCompletionScriptEventAction {
    target: FrameDocumentClassicScriptCompletionTarget,
    event: FrameDocumentScriptElementEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicParserResumeCompletionAction {
    target: FrameDocumentClassicScriptCompletionTarget,
}

impl FrameDocumentClassicCompletionScriptEventAction {
    pub(crate) fn new(
        target: FrameDocumentClassicScriptCompletionTarget,
        event: FrameDocumentScriptElementEvent,
    ) -> Self {
        Self { target, event }
    }

    pub(crate) fn target(&self) -> FrameDocumentClassicScriptCompletionTarget {
        self.target
    }

    pub(crate) fn event(&self) -> FrameDocumentScriptElementEvent {
        self.event
    }
}

impl FrameDocumentClassicParserResumeCompletionAction {
    pub(crate) fn new(target: FrameDocumentClassicScriptCompletionTarget) -> Self {
        Self { target }
    }

    pub(crate) fn into_target(self) -> FrameDocumentClassicScriptCompletionTarget {
        self.target
    }

    pub(crate) fn target(&self) -> FrameDocumentClassicScriptCompletionTarget {
        self.target
    }
}

impl FrameDocumentClassicCompletionFinishAction {
    pub(crate) fn from_completion(completion: FrameDocumentClassicScriptCompletionAction) -> Self {
        let (target, script_element_event) = completion.into_parts();
        Self {
            target,
            script_element_event,
        }
    }

    pub(crate) fn script_element_event_action(
        &self,
    ) -> Option<FrameDocumentClassicCompletionScriptEventAction> {
        self.script_element_event
            .map(|event| FrameDocumentClassicCompletionScriptEventAction::new(self.target, event))
    }

    pub(crate) fn target(&self) -> FrameDocumentClassicScriptCompletionTarget {
        self.target
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FrameDocumentClassicDeferredCompletionApplication {
    scheduler_work: Option<FrameDocumentClassicScriptSchedulerWork>,
    order_slot_released: bool,
    queued_document_script_ready: bool,
    domcontentloaded_queued: bool,
}

impl FrameDocumentClassicDeferredCompletionApplication {
    pub(crate) fn new(
        scheduler_work: Option<FrameDocumentClassicScriptSchedulerWork>,
        queued_document_script_ready: bool,
        domcontentloaded_queued: bool,
    ) -> Self {
        Self {
            scheduler_work,
            order_slot_released: false,
            queued_document_script_ready,
            domcontentloaded_queued,
        }
    }

    pub(crate) fn into_scheduler_work(self) -> Option<FrameDocumentClassicScriptSchedulerWork> {
        self.scheduler_work
    }

    pub(crate) fn with_order_slot_released(mut self) -> Self {
        self.order_slot_released = true;
        self
    }

    pub(crate) fn order_slot_was_released(&self) -> bool {
        self.order_slot_released
    }

    pub(crate) fn domcontentloaded_was_queued(&self) -> bool {
        self.domcontentloaded_queued
    }

    pub(crate) fn document_script_ready_was_queued(&self) -> bool {
        self.queued_document_script_ready
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FrameDocumentClassicParserResumeApplication {
    scheduler_work: Option<FrameDocumentClassicScriptSchedulerWork>,
    resumed_parser: bool,
    skip_reason: Option<FrameDocumentClassicParserResumeSkipReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameDocumentClassicParserResumeSkipReason {
    StaleDocumentOwner,
    StaleRealm,
    StaleParserSuspension,
    MissingCurrentChildSnapshot,
    MissingLiveParser,
}

impl FrameDocumentClassicParserResumeApplication {
    pub(crate) fn skipped(reason: FrameDocumentClassicParserResumeSkipReason) -> Self {
        Self {
            scheduler_work: None,
            resumed_parser: false,
            skip_reason: Some(reason),
        }
    }

    pub(crate) fn resumed(scheduler_work: Option<FrameDocumentClassicScriptSchedulerWork>) -> Self {
        Self {
            scheduler_work,
            resumed_parser: true,
            skip_reason: None,
        }
    }

    pub(crate) fn parser_was_resumed(&self) -> bool {
        self.resumed_parser
    }

    pub(crate) fn skip_reason(&self) -> Option<FrameDocumentClassicParserResumeSkipReason> {
        self.skip_reason
    }

    pub(crate) fn into_scheduler_work(self) -> Option<FrameDocumentClassicScriptSchedulerWork> {
        self.scheduler_work
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicCompletionScriptEventFollowup {
    script_event_dispatched: bool,
    script_event_dispatch_failed: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicCompletionLifecycleFollowup {
    parser_resume_attempted: bool,
    parser_resumed: bool,
    parser_resume_skip_reason: Option<FrameDocumentClassicParserResumeSkipReason>,
    parser_deferred_order_released: bool,
    document_script_ready_queued: bool,
    domcontentloaded_queued: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicCompletionFollowup {
    script_event: FrameDocumentClassicCompletionScriptEventFollowup,
    lifecycle: FrameDocumentClassicCompletionLifecycleFollowup,
}

impl FrameDocumentClassicCompletionScriptEventFollowup {
    pub(crate) fn note_script_event_dispatched(&mut self) {
        self.script_event_dispatched = true;
    }

    pub(crate) fn note_script_event_dispatch_failed(&mut self) {
        self.script_event_dispatch_failed = true;
    }

    pub(crate) fn made_progress(self) -> bool {
        self.script_event_dispatched || self.script_event_dispatch_failed
    }

    pub(crate) fn script_event_was_dispatched(self) -> bool {
        self.script_event_dispatched
    }
}

impl FrameDocumentClassicCompletionLifecycleFollowup {
    pub(crate) fn note_parser_resume_attempted(&mut self) {
        self.parser_resume_attempted = true;
    }

    pub(crate) fn note_parser_resumed(&mut self) {
        self.parser_resumed = true;
    }

    pub(crate) fn note_parser_resume_skipped(
        &mut self,
        reason: FrameDocumentClassicParserResumeSkipReason,
    ) {
        self.parser_resume_skip_reason = Some(reason);
    }

    pub(crate) fn note_document_script_ready_queued(&mut self) {
        self.document_script_ready_queued = true;
    }

    pub(crate) fn note_parser_deferred_order_released(&mut self) {
        self.parser_deferred_order_released = true;
    }

    pub(crate) fn note_domcontentloaded_queued(&mut self) {
        self.domcontentloaded_queued = true;
    }

    pub(crate) fn made_progress(self) -> bool {
        self.parser_resume_attempted
            || self.parser_resumed
            || self.parser_resume_skip_reason.is_some()
            || self.parser_deferred_order_released
            || self.document_script_ready_queued
            || self.domcontentloaded_queued
    }

    #[cfg(test)]
    pub(crate) fn parser_resume_was_attempted(self) -> bool {
        self.parser_resume_attempted
    }

    #[cfg(test)]
    pub(crate) fn parser_was_resumed(self) -> bool {
        self.parser_resumed
    }

    #[cfg(test)]
    pub(crate) fn parser_resume_skip_reason(
        self,
    ) -> Option<FrameDocumentClassicParserResumeSkipReason> {
        self.parser_resume_skip_reason
    }

    #[cfg(test)]
    pub(crate) fn document_script_ready_was_queued(self) -> bool {
        self.document_script_ready_queued
    }

    #[cfg(test)]
    pub(crate) fn domcontentloaded_was_queued(self) -> bool {
        self.domcontentloaded_queued
    }
}

impl FrameDocumentClassicCompletionFollowup {
    pub(crate) fn from_parts(
        script_event: FrameDocumentClassicCompletionScriptEventFollowup,
        lifecycle: FrameDocumentClassicCompletionLifecycleFollowup,
    ) -> Self {
        Self {
            script_event,
            lifecycle,
        }
    }

    pub(crate) fn made_progress(self) -> bool {
        self.script_event.made_progress() || self.lifecycle.made_progress()
    }

    #[cfg(test)]
    pub(crate) fn script_event_followup(self) -> FrameDocumentClassicCompletionScriptEventFollowup {
        self.script_event
    }

    #[cfg(test)]
    pub(crate) fn lifecycle_followup(self) -> FrameDocumentClassicCompletionLifecycleFollowup {
        self.lifecycle
    }

    pub(crate) fn script_event_was_dispatched(self) -> bool {
        self.script_event.script_event_was_dispatched()
    }

    #[cfg(test)]
    pub(crate) fn parser_resume_was_attempted(self) -> bool {
        self.lifecycle.parser_resume_was_attempted()
    }

    #[cfg(test)]
    pub(crate) fn parser_was_resumed(self) -> bool {
        self.lifecycle.parser_was_resumed()
    }

    #[cfg(test)]
    pub(crate) fn parser_resume_skip_reason(
        self,
    ) -> Option<FrameDocumentClassicParserResumeSkipReason> {
        self.lifecycle.parser_resume_skip_reason()
    }

    #[cfg(test)]
    pub(crate) fn document_script_ready_was_queued(self) -> bool {
        self.lifecycle.document_script_ready_was_queued()
    }

    #[cfg(test)]
    pub(crate) fn domcontentloaded_was_queued(self) -> bool {
        self.lifecycle.domcontentloaded_was_queued()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicScriptReadyTarget {
    child_handle: DomHandle,
    task_owner: FrameDocumentTaskOwner,
    realm_id: Option<FrameRealmId>,
    original_owner_document_handle: DomHandle,
    scheduling: FrameDocumentClassicScriptScheduling,
    pending_script_key: Option<ParserPendingScriptKey>,
    load_delay_token: Option<DocumentLoadDelayTokenId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicScriptTarget {
    child_handle: DomHandle,
    task_owner: FrameDocumentTaskOwner,
    realm_id: Option<FrameRealmId>,
    scheduling: FrameDocumentClassicScriptScheduling,
    pending_script_key: Option<ParserPendingScriptKey>,
    load_delay_token: Option<DocumentLoadDelayTokenId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicScriptCompletionTarget {
    child_handle: DomHandle,
    task_owner: FrameDocumentTaskOwner,
    realm_id: FrameRealmId,
    scheduling: FrameDocumentClassicScriptScheduling,
    pending_script_key: Option<ParserPendingScriptKey>,
    load_delay_token: Option<DocumentLoadDelayTokenId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicScriptSourceFailureTarget {
    child_handle: DomHandle,
    task_owner: FrameDocumentTaskOwner,
    realm_id: Option<FrameRealmId>,
    scheduling: FrameDocumentClassicScriptScheduling,
    pending_script_key: Option<ParserPendingScriptKey>,
    load_delay_token: Option<DocumentLoadDelayTokenId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicScriptSourceLoadTaskPayload {
    client: FrameDocumentClassicScriptSourceLoadClient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicScriptSourceLoadClientTarget {
    child_handle: DomHandle,
    owner: FrameDocumentOwner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicScriptSourceLoadRequestTarget {
    child_handle: DomHandle,
    task_owner: FrameDocumentTaskOwner,
    owner_request_id: FrameRequestId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameDocumentClassicScriptSourceLoadCompletionTarget {
    task_owner: FrameDocumentTaskOwner,
    owner_request_id: FrameRequestId,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameDocumentClassicScriptSourceLoadOwner {
    pub(crate) task_owner: FrameDocumentTaskOwner,
    pub(crate) request_id: FrameRequestId,
}

impl FrameDocumentClassicScriptSourceLoadTaskPayload {
    pub(crate) fn new(client: FrameDocumentClassicScriptSourceLoadClient) -> Self {
        Self { client }
    }

    pub(crate) fn child_handle(&self) -> DomHandle {
        self.client.target().child_handle()
    }

    pub(crate) fn client(&self) -> &FrameDocumentClassicScriptSourceLoadClient {
        &self.client
    }
}

impl FrameDocumentClassicScriptSourceLoadClientTarget {
    pub(crate) fn new(child_handle: DomHandle, owner: FrameDocumentOwner) -> Self {
        Self {
            child_handle,
            owner,
        }
    }

    pub(crate) fn child_handle(&self) -> DomHandle {
        self.child_handle
    }

    pub(crate) fn owner(&self) -> FrameDocumentOwner {
        self.owner
    }
}

impl FrameDocumentClassicScriptSourceLoadRequestTarget {
    pub(crate) fn new(
        child_handle: DomHandle,
        task_owner: FrameDocumentTaskOwner,
        owner_request_id: FrameRequestId,
    ) -> Self {
        Self {
            child_handle,
            task_owner,
            owner_request_id,
        }
    }

    pub(crate) fn child_handle(&self) -> DomHandle {
        self.child_handle
    }

    #[cfg(test)]
    pub(crate) fn owner_document_id(&self) -> DocumentId {
        self.task_owner.document_id
    }

    pub(crate) fn task_owner(&self) -> FrameDocumentTaskOwner {
        self.task_owner
    }

    #[cfg(test)]
    pub(crate) fn owner_request_id(&self) -> FrameRequestId {
        self.owner_request_id
    }
}

impl FrameDocumentClassicScriptSourceLoadCompletionTarget {
    pub(crate) fn new(
        task_owner: FrameDocumentTaskOwner,
        owner_request_id: FrameRequestId,
    ) -> Self {
        Self {
            task_owner,
            owner_request_id,
        }
    }

    pub(crate) fn owner_document_id(&self) -> DocumentId {
        self.task_owner.document_id
    }

    #[cfg(test)]
    pub(crate) fn task_owner(&self) -> FrameDocumentTaskOwner {
        self.task_owner
    }

    pub(crate) fn owner_request_id(&self) -> FrameRequestId {
        self.owner_request_id
    }
}

impl From<(FrameDocumentTaskOwner, FrameRequestId)> for FrameDocumentClassicScriptSourceLoadOwner {
    fn from((task_owner, request_id): (FrameDocumentTaskOwner, FrameRequestId)) -> Self {
        Self {
            task_owner,
            request_id,
        }
    }
}

impl FrameDocumentClassicScriptReadyTarget {
    pub(crate) fn new(
        child_handle: DomHandle,
        task_owner: FrameDocumentTaskOwner,
        realm_id: Option<FrameRealmId>,
        original_owner_document_handle: DomHandle,
    ) -> Self {
        Self {
            child_handle,
            task_owner,
            realm_id,
            original_owner_document_handle,
            scheduling: FrameDocumentClassicScriptScheduling::ParserBlocking,
            pending_script_key: None,
            load_delay_token: None,
        }
    }

    pub(crate) fn with_scheduling(
        mut self,
        scheduling: FrameDocumentClassicScriptScheduling,
    ) -> Self {
        self.scheduling = scheduling;
        self
    }

    pub(crate) fn child_handle(&self) -> DomHandle {
        self.child_handle
    }

    pub(crate) fn task_owner(&self) -> FrameDocumentTaskOwner {
        self.task_owner
    }

    pub(crate) fn realm_id(&self) -> Option<FrameRealmId> {
        self.realm_id
    }

    pub(crate) fn original_owner_document_handle(&self) -> DomHandle {
        self.original_owner_document_handle
    }

    pub(crate) fn scheduling(&self) -> FrameDocumentClassicScriptScheduling {
        self.scheduling
    }

    pub(crate) fn with_pending_script_key(
        mut self,
        pending_script_key: Option<ParserPendingScriptKey>,
    ) -> Self {
        self.pending_script_key = pending_script_key;
        self
    }

    pub(crate) fn pending_script_key(&self) -> Option<ParserPendingScriptKey> {
        self.pending_script_key
    }

    pub(crate) fn with_load_delay_token(
        mut self,
        load_delay_token: Option<DocumentLoadDelayTokenId>,
    ) -> Self {
        self.load_delay_token = load_delay_token;
        self
    }

    pub(crate) fn load_delay_token(&self) -> Option<DocumentLoadDelayTokenId> {
        self.load_delay_token
    }
}

impl FrameDocumentClassicScriptTarget {
    pub(crate) fn new(
        child_handle: DomHandle,
        task_owner: FrameDocumentTaskOwner,
        realm_id: Option<FrameRealmId>,
    ) -> Self {
        Self {
            child_handle,
            task_owner,
            realm_id,
            scheduling: FrameDocumentClassicScriptScheduling::ParserBlocking,
            pending_script_key: None,
            load_delay_token: None,
        }
    }

    pub(crate) fn with_scheduling(
        mut self,
        scheduling: FrameDocumentClassicScriptScheduling,
    ) -> Self {
        self.scheduling = scheduling;
        self
    }

    pub(crate) fn child_handle(&self) -> DomHandle {
        self.child_handle
    }

    pub(crate) fn owner(&self) -> FrameDocumentOwner {
        self.task_owner.document_owner()
    }

    pub(crate) fn task_owner(&self) -> FrameDocumentTaskOwner {
        self.task_owner
    }

    pub(crate) fn realm_id(&self) -> Option<FrameRealmId> {
        self.realm_id
    }

    pub(crate) fn scheduling(&self) -> FrameDocumentClassicScriptScheduling {
        self.scheduling
    }

    pub(crate) fn with_pending_script_key(
        mut self,
        pending_script_key: Option<ParserPendingScriptKey>,
    ) -> Self {
        self.pending_script_key = pending_script_key;
        self
    }

    pub(crate) fn pending_script_key(&self) -> Option<ParserPendingScriptKey> {
        self.pending_script_key
    }

    pub(crate) fn with_load_delay_token(
        mut self,
        load_delay_token: Option<DocumentLoadDelayTokenId>,
    ) -> Self {
        self.load_delay_token = load_delay_token;
        self
    }

    pub(crate) fn load_delay_token(&self) -> Option<DocumentLoadDelayTokenId> {
        self.load_delay_token
    }
}

impl FrameDocumentClassicScriptCompletionTarget {
    pub(crate) fn new(
        child_handle: DomHandle,
        task_owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
    ) -> Self {
        Self {
            child_handle,
            task_owner,
            realm_id,
            scheduling: FrameDocumentClassicScriptScheduling::ParserBlocking,
            pending_script_key: None,
            load_delay_token: None,
        }
    }

    pub(crate) fn with_scheduling(
        mut self,
        scheduling: FrameDocumentClassicScriptScheduling,
    ) -> Self {
        self.scheduling = scheduling;
        self
    }

    pub(crate) fn child_handle(&self) -> DomHandle {
        self.child_handle
    }

    pub(crate) fn owner(&self) -> FrameDocumentOwner {
        self.task_owner.document_owner()
    }

    pub(crate) fn task_owner(&self) -> FrameDocumentTaskOwner {
        self.task_owner
    }

    pub(crate) fn realm_id(&self) -> FrameRealmId {
        self.realm_id
    }

    pub(crate) fn scheduling(&self) -> FrameDocumentClassicScriptScheduling {
        self.scheduling
    }

    pub(crate) fn with_pending_script_key(
        mut self,
        pending_script_key: Option<ParserPendingScriptKey>,
    ) -> Self {
        self.pending_script_key = pending_script_key;
        self
    }

    pub(crate) fn pending_script_key(&self) -> Option<ParserPendingScriptKey> {
        self.pending_script_key
    }

    pub(crate) fn with_load_delay_token(
        mut self,
        load_delay_token: Option<DocumentLoadDelayTokenId>,
    ) -> Self {
        self.load_delay_token = load_delay_token;
        self
    }

    pub(crate) fn load_delay_token(&self) -> Option<DocumentLoadDelayTokenId> {
        self.load_delay_token
    }
}

impl FrameDocumentClassicScriptSourceFailureTarget {
    pub(crate) fn new(
        child_handle: DomHandle,
        task_owner: FrameDocumentTaskOwner,
        realm_id: Option<FrameRealmId>,
    ) -> Self {
        Self {
            child_handle,
            task_owner,
            realm_id,
            scheduling: FrameDocumentClassicScriptScheduling::ParserBlocking,
            pending_script_key: None,
            load_delay_token: None,
        }
    }

    pub(crate) fn with_scheduling(
        mut self,
        scheduling: FrameDocumentClassicScriptScheduling,
    ) -> Self {
        self.scheduling = scheduling;
        self
    }

    pub(crate) fn child_handle(&self) -> DomHandle {
        self.child_handle
    }

    pub(crate) fn task_owner(&self) -> FrameDocumentTaskOwner {
        self.task_owner
    }

    pub(crate) fn realm_id(&self) -> Option<FrameRealmId> {
        self.realm_id
    }

    pub(crate) fn scheduling(&self) -> FrameDocumentClassicScriptScheduling {
        self.scheduling
    }

    pub(crate) fn with_pending_script_key(
        mut self,
        pending_script_key: Option<ParserPendingScriptKey>,
    ) -> Self {
        self.pending_script_key = pending_script_key;
        self
    }

    pub(crate) fn pending_script_key(&self) -> Option<ParserPendingScriptKey> {
        self.pending_script_key
    }

    pub(crate) fn with_load_delay_token(
        mut self,
        load_delay_token: Option<DocumentLoadDelayTokenId>,
    ) -> Self {
        self.load_delay_token = load_delay_token;
        self
    }

    pub(crate) fn load_delay_token(&self) -> Option<DocumentLoadDelayTokenId> {
        self.load_delay_token
    }
}

pub(crate) fn frame_document_classic_script_begin_execution_action(
    child_handle: DomHandle,
    task_owner: FrameDocumentTaskOwner,
    realm_id: Option<FrameRealmId>,
    scheduling: FrameDocumentClassicScriptScheduling,
    pending_script_key: Option<ParserPendingScriptKey>,
    load_delay_token: Option<DocumentLoadDelayTokenId>,
    action: ParserPendingClassicScriptBeginExecutionAction,
) -> FrameDocumentClassicScriptBeginExecutionAction {
    ParserClassicScriptBeginExecutionAction::from_pending_begin_execution_action(
        FrameDocumentClassicScriptTarget::new(child_handle, task_owner, realm_id)
            .with_scheduling(scheduling)
            .with_pending_script_key(pending_script_key)
            .with_load_delay_token(load_delay_token),
        action,
    )
}

pub(crate) fn frame_script_job_kind_from_parser_classic_ready_kind(
    ready_kind: ParserPendingClassicScriptReadyKind,
) -> FrameScriptJobKind {
    match ready_kind {
        ParserPendingClassicScriptReadyKind::ParserConnected => FrameScriptJobKind::ParserClassic,
        ParserPendingClassicScriptReadyKind::External => FrameScriptJobKind::ExternalClassic,
    }
}

pub(crate) fn frame_document_classic_script_source_load_client_action(
    child_handle: DomHandle,
    owner: FrameDocumentOwner,
    client: ParserPendingClassicScriptSourceLoadClientAction<'_>,
) -> FrameDocumentClassicScriptSourceLoadClient {
    ParserClassicScriptSourceLoadClientAction::from_pending_source_load_client_action(
        FrameDocumentClassicScriptSourceLoadClientTarget::new(child_handle, owner),
        client,
    )
}

pub(crate) fn frame_document_classic_script_source_load_request_action(
    child_handle: DomHandle,
    task_owner: FrameDocumentTaskOwner,
    owner_request_id: FrameRequestId,
    action: ParserPendingClassicScriptSourceLoadAction,
) -> FrameDocumentClassicScriptSourceLoadRequest {
    ParserClassicScriptSourceLoadRequestAction::from_pending_source_load_action(
        FrameDocumentClassicScriptSourceLoadRequestTarget::new(
            child_handle,
            task_owner,
            owner_request_id,
        ),
        action,
    )
}

pub(crate) fn frame_document_classic_script_source_load_completion_action(
    task_owner: FrameDocumentTaskOwner,
    owner_request_id: FrameRequestId,
    action: ParserPendingClassicScriptSourceLoadCompletionAction<
        FrameDocumentClassicScriptSourceLoadOwner,
    >,
) -> FrameDocumentClassicScriptSourceLoadCompletionAction {
    ParserClassicScriptSourceLoadCompletionAction::from_pending_source_load_completion_action(
        FrameDocumentClassicScriptSourceLoadCompletionTarget::new(task_owner, owner_request_id),
        action,
    )
}

impl FrameDocumentClassicScriptSourceLoadTask {
    pub(crate) fn from_source_load_client(
        owner: FrameDocumentTaskOwner,
        realm_id: FrameRealmId,
        client: FrameDocumentClassicScriptSourceLoadClient,
    ) -> Self {
        assert_eq!(
            client.target().owner(),
            owner.document_owner(),
            "classic source-load client and task must name the same Document owner"
        );
        Self::new(
            owner,
            realm_id,
            FrameDocumentClassicScriptSourceLoadTaskPayload::new(client),
        )
    }

    pub(crate) fn child_handle(&self) -> DomHandle {
        self.payload().child_handle()
    }

    pub(crate) fn client(&self) -> &FrameDocumentClassicScriptSourceLoadClient {
        self.payload().client()
    }
}

#[cfg(test)]
mod tests {
    use super::super::records::{FrameSchedulerLaneId, LocalWindowId};
    use super::*;

    #[test]
    fn frame_document_classic_completion_finish_action_retains_event_and_target() {
        let task_owner =
            FrameDocumentTaskOwner::new(FrameSchedulerLaneId(1), LocalWindowId(2), DocumentId(3));
        let target = FrameDocumentClassicScriptCompletionTarget::new(
            DomHandle::new(1),
            task_owner,
            FrameRealmId(4),
        );
        let event = FrameDocumentScriptElementEvent::load(
            DomHandle::new(1),
            target.owner(),
            DomHandle::new(4),
        );
        let action = FrameDocumentClassicCompletionFinishAction::from_completion(
            FrameDocumentClassicScriptCompletionAction::new(target, Some(event)),
        );

        let event_action = action
            .script_element_event_action()
            .expect("completion should expose script event action");
        assert_eq!(event_action.target(), target);
        assert_eq!(event_action.event(), event);
        assert_eq!(action.target(), target);
    }

    #[test]
    fn frame_document_classic_execution_finish_projects_owner_realm_target() {
        let task_owner =
            FrameDocumentTaskOwner::new(FrameSchedulerLaneId(1), LocalWindowId(2), DocumentId(3));
        let script_url =
            url::Url::parse("https://child-classic-finish.test/script.js").expect("valid URL");
        let finish = FrameDocumentClassicScriptExecutionFinish {
            child_handle: DomHandle::new(1),
            owner: task_owner.document_owner(),
            task_owner,
            realm_id: FrameRealmId(4),
            script_handle: DomHandle::new(5),
            script_url: script_url.clone(),
            script_base_url: script_url,
            scheduling: FrameDocumentClassicScriptScheduling::ParserBlocking,
            pending_script_key: None,
            load_delay_token: None,
        };

        let target = finish.target();

        assert_eq!(target.child_handle(), DomHandle::new(1));
        assert_eq!(target.owner(), task_owner.document_owner());
        assert_eq!(target.task_owner(), task_owner);
        assert_eq!(target.realm_id(), FrameRealmId(4));
    }

    #[test]
    fn frame_document_classic_completion_followup_tracks_finish_steps() {
        let mut script_event_followup =
            FrameDocumentClassicCompletionScriptEventFollowup::default();
        let mut lifecycle_followup = FrameDocumentClassicCompletionLifecycleFollowup::default();
        let empty_followup = FrameDocumentClassicCompletionFollowup::from_parts(
            script_event_followup,
            lifecycle_followup,
        );

        assert!(!empty_followup.made_progress());
        script_event_followup.note_script_event_dispatched();
        lifecycle_followup.note_parser_resume_attempted();
        lifecycle_followup.note_parser_resumed();
        lifecycle_followup.note_document_script_ready_queued();
        lifecycle_followup.note_domcontentloaded_queued();

        let followup = FrameDocumentClassicCompletionFollowup::from_parts(
            script_event_followup,
            lifecycle_followup,
        );

        assert!(followup.made_progress());
        assert!(followup.script_event_was_dispatched());
        assert!(
            followup
                .script_event_followup()
                .script_event_was_dispatched()
        );
        assert!(followup.parser_resume_was_attempted());
        assert!(followup.parser_was_resumed());
        assert!(followup.lifecycle_followup().parser_resume_was_attempted());
        assert!(followup.document_script_ready_was_queued());
        assert!(followup.domcontentloaded_was_queued());
    }

    #[test]
    fn frame_document_classic_completion_followup_tracks_parser_resume_skip_reason() {
        let mut lifecycle_followup = FrameDocumentClassicCompletionLifecycleFollowup::default();

        lifecycle_followup.note_parser_resume_attempted();
        lifecycle_followup.note_parser_resume_skipped(
            FrameDocumentClassicParserResumeSkipReason::StaleDocumentOwner,
        );
        let followup = FrameDocumentClassicCompletionFollowup::from_parts(
            FrameDocumentClassicCompletionScriptEventFollowup::default(),
            lifecycle_followup,
        );

        assert!(followup.made_progress());
        assert_eq!(
            followup.parser_resume_skip_reason(),
            Some(FrameDocumentClassicParserResumeSkipReason::StaleDocumentOwner)
        );
        assert!(!followup.parser_was_resumed());
    }

    #[test]
    fn frame_document_classic_completion_followup_tracks_parser_resume_stale_realm() {
        let mut lifecycle_followup = FrameDocumentClassicCompletionLifecycleFollowup::default();

        lifecycle_followup.note_parser_resume_attempted();
        lifecycle_followup
            .note_parser_resume_skipped(FrameDocumentClassicParserResumeSkipReason::StaleRealm);
        let followup = FrameDocumentClassicCompletionFollowup::from_parts(
            FrameDocumentClassicCompletionScriptEventFollowup::default(),
            lifecycle_followup,
        );

        assert!(followup.made_progress());
        assert_eq!(
            followup.parser_resume_skip_reason(),
            Some(FrameDocumentClassicParserResumeSkipReason::StaleRealm)
        );
        assert!(!followup.parser_was_resumed());
    }

    #[test]
    fn frame_document_classic_prepare_application_tracks_drop_reason() {
        let dropped = FrameDocumentClassicPrepareApplication::dropped(
            FrameDocumentClassicPrepareDropReason::StaleRunnerOwner,
        );

        assert_eq!(
            dropped.drop_reason(),
            Some(FrameDocumentClassicPrepareDropReason::StaleRunnerOwner)
        );
        assert!(matches!(
            dropped.into_start(),
            FrameClassicDocumentScriptExecutionStart::Dropped
        ));
    }

    #[test]
    fn frame_document_classic_prepare_followup_tracks_prepared_execution() {
        let mut followup = FrameDocumentClassicPrepareFollowup::default();

        assert!(!followup.made_progress());
        followup.note_realm_materialization_attempted();
        followup.note_realm_materialized();
        followup.note_execution_prepared();

        assert!(followup.made_progress());
        assert!(followup.execution_was_prepared());
        assert_eq!(followup.drop_reason(), None);
    }

    #[test]
    fn frame_document_classic_prepare_followup_tracks_drop_reason() {
        let mut followup = FrameDocumentClassicPrepareFollowup::default();

        followup.note_realm_materialization_attempted();
        followup.note_dropped(FrameDocumentClassicPrepareDropReason::StaleRealm);

        assert!(followup.made_progress());
        assert_eq!(
            followup.drop_reason(),
            Some(FrameDocumentClassicPrepareDropReason::StaleRealm)
        );
        assert!(!followup.execution_was_prepared());
        assert!(!followup.completion_was_produced());
    }

    #[test]
    fn frame_document_classic_prepare_followup_tracks_context_host_drop_reason() {
        let mut followup = FrameDocumentClassicPrepareFollowup::default();

        followup.note_dropped(FrameDocumentClassicPrepareDropReason::BeginExecutionUnavailable);

        assert!(followup.made_progress());
        assert_eq!(
            followup.drop_reason(),
            Some(FrameDocumentClassicPrepareDropReason::BeginExecutionUnavailable)
        );
    }

    #[test]
    fn frame_document_classic_execution_followup_tracks_completion() {
        let mut followup = FrameDocumentClassicExecutionFollowup::default();

        assert!(!followup.made_progress());
        followup.note_script_job_attempted();
        followup.note_completion_produced();

        assert!(followup.made_progress());
        assert!(followup.script_job_was_attempted());
        assert!(!followup.script_job_failed());
        assert!(followup.completion_was_produced());
    }

    #[test]
    fn frame_document_classic_execution_followup_tracks_script_failure() {
        let mut followup = FrameDocumentClassicExecutionFollowup::default();

        followup.note_script_job_attempted();
        followup.note_script_job_failed();

        assert!(followup.made_progress());
        assert!(followup.script_job_was_attempted());
        assert!(followup.script_job_failed());
        assert!(!followup.completion_was_produced());
    }

    #[test]
    fn frame_document_classic_source_failure_application_tracks_skip_reason() {
        let skipped = FrameDocumentClassicSourceFailureReportApplication::skipped(
            FrameDocumentClassicSourceFailureReportSkipReason::StaleRealm,
        );

        assert_eq!(
            skipped.skip_reason(),
            Some(FrameDocumentClassicSourceFailureReportSkipReason::StaleRealm)
        );
        assert!(skipped.into_completion().is_none());
    }

    #[test]
    fn frame_document_classic_source_failure_followup_tracks_report_steps() {
        let mut followup = FrameDocumentClassicSourceFailureReportFollowup::default();

        assert!(!followup.made_progress());
        followup.note_failure_logged();
        followup.note_completion_produced();

        assert!(followup.made_progress());
        assert!(followup.failure_was_logged());
        assert!(followup.completion_was_produced());
        assert_eq!(followup.skip_reason(), None);
    }

    #[test]
    fn frame_document_classic_source_failure_followup_tracks_skip_reason() {
        let mut followup = FrameDocumentClassicSourceFailureReportFollowup::default();

        followup.note_failure_logged();
        followup.note_skipped(
            FrameDocumentClassicSourceFailureReportSkipReason::RealmMaterializationFailed,
        );

        assert!(followup.made_progress());
        assert!(followup.failure_was_logged());
        assert!(!followup.completion_was_produced());
        assert_eq!(
            followup.skip_reason(),
            Some(FrameDocumentClassicSourceFailureReportSkipReason::RealmMaterializationFailed)
        );
    }

    #[test]
    fn frame_document_classic_source_failure_skip_reasons_cover_currentness() {
        let skip_reasons = [
            FrameDocumentClassicSourceFailureReportSkipReason::RealmMaterializationFailed,
            FrameDocumentClassicSourceFailureReportSkipReason::MissingCurrentRealm,
            FrameDocumentClassicSourceFailureReportSkipReason::StaleRealm,
            FrameDocumentClassicSourceFailureReportSkipReason::StaleRunnerOwner,
        ];

        for reason in skip_reasons {
            let application = FrameDocumentClassicSourceFailureReportApplication::skipped(reason);
            assert_eq!(application.skip_reason(), Some(reason));
            assert!(application.into_completion().is_none());
        }
    }
}
