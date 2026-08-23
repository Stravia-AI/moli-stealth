use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use moli_cookie_jar::{StoredCookie, StoredCookieQueryReport};
use moli_fetch::FetchConfig;
use parking_lot::Mutex;
use serde_json::json;

use crate::devtools_runtime::{
    DevToolsCommandContext, DevToolsTargetFilterEntry, DevToolsTargetInfo, DevToolsTargetKind,
};
use crate::domains::command_output::BackgroundProtocolEventBuffer;
#[cfg(test)]
use crate::domains::command_output::CommandOutputBuffer;

use moli_core::{
    LayoutPolicy, OptionalResourceFetchMask, RendererOutputPublicationOrdering,
    RendererOutputTransportMessage,
    browser_host::{
        BrowserCommandId, BrowserContextRegistrationMetadata, BrowserDownloadPolicy,
        BrowserDownloadPolicyUpdate, BrowserDownloadRegistry, BrowserHostHandle,
        BrowserHostNetworkPolicySnapshot, BrowserHostPolicyState, BrowserHostPolicyUpdate,
        BrowserHostState, BrowserInitialEmptyDocumentSeed, BrowserPermissionOverride,
        BrowserTargetCreationMetadata,
    },
    network::{SharedWebStorageStore, new_shared_web_storage_store},
    page::{NavigationResponse, Page, SubresourceAuthCredentials},
    runtime::{
        NavigationEngine, NavigationRuntimeConfig, storage_partition::StoragePartitionState,
    },
};

pub const DEFAULT_CDP_PAGE_TARGET_ID: &str = "moli-default";
pub const DEFAULT_CDP_TAB_TARGET_ID: &str = "moli-default-tab";

mod activity_source;
mod background_output;
mod bidi_channel_work;
mod body_spool;
mod browser_context;
mod browser_document_navigation;
mod browser_document_page_builder;
mod browser_fact_projection;
mod browser_host_executor_residence;
mod browser_host_turn_executor;
mod browser_initial_document;
mod browser_navigation_history;
mod browser_owner_input;
mod browser_page_replacement;
mod browser_target_metadata_projection;
pub(crate) use browser_page_replacement::{
    LoadedPageReplacementOutcome, LoadedPageReplacementStart, PendingLoadedPageReplacement,
};
pub(crate) mod browser_target_engine_handoff;
mod browser_target_termination;
mod command_owner_scope;
mod command_view;
mod cookie_manager_surface;
mod cookie_owner;
mod cookie_policy_surface;
#[cfg(test)]
mod cookie_store_boundary;
mod devtools_command;
mod dispatch;
mod download_event_projection;
mod downloads;
mod fetch_support;
mod inspector_route;
mod output;
mod page_state;
mod protocol_output;
mod renderer_command_turn;
mod resource_runtime_support;
mod runtime_eval;
mod runtime_load;
mod scheduler_hooks;
mod scheduler_state;
mod settings;
#[cfg(test)]
mod site_data_manager_surface;
mod state;
mod target;

pub use crate::domains::network::IoStreamState;
pub use background_output::{
    BackgroundEventSender, BackgroundOutputClosed, BrowserBackgroundOutputReceiver,
    browser_background_output_channel,
};
#[cfg(test)]
pub(crate) use bidi_channel_work::BidiChannelOwnerActionKind;
pub(crate) use bidi_channel_work::{
    BidiChannelListenerResidence, BidiChannelOwnerAction, BidiChannelOwnerActionBody,
    BidiChannelPageOwner,
};
pub(crate) use body_spool::{CapturedBody, CapturedBodyWriter};
pub(crate) use browser_context::{
    ClosedPageTarget, PageLifecycleEventsEnableResult, ProjectedActiveTargetClose,
    ProjectedClosedPageTarget, SessionOwnerInspectorEnableResult,
    SessionOwnerRuntimeFrontendEnableResult, TargetEmulationSessionStateMut,
    TargetLoadedNavigationCommitState, TargetNavigationLoadInputs, TargetProjectionError,
};
pub use browser_fact_projection::BrowserFactProjectionError;
pub(crate) use browser_fact_projection::BrowserTargetCreatedFactProjection;
pub(crate) use browser_host_executor_residence::BrowserHostTurnExecution;
pub use browser_host_executor_residence::BrowserHostTurnExecutorOwner;
pub use browser_host_turn_executor::{
    BrowserHostTurnDispatch, CompletedBrowserHostTurn, PendingBrowserHostTurn,
};
pub(crate) use browser_owner_input::{
    BrowserOwnerInputPublicationError, BrowserOwnerPausedNavigationSidecar,
    CompletedBrowserOwnerContextDisposalCommand,
    CompletedBrowserOwnerInitialTargetNavigationCommand, CompletedBrowserOwnerNavigationCommand,
    CompletedBrowserOwnerPausedNavigationDecisionCommand, CompletedBrowserOwnerStopLoadingCommand,
    PendingBrowserOwnerContextDisposalCommand, PendingBrowserOwnerInitialTargetNavigationCommand,
    PendingBrowserOwnerNavigationCommand, PendingBrowserOwnerPausedNavigationDecisionCommand,
    PendingBrowserOwnerStopLoadingCommand,
};
pub(crate) use browser_target_termination::{
    BrowserPageTargetTerminationStart, BrowserTargetCloseStart, BrowserTargetTerminationProjection,
    BrowserTargetTerminationProjectionKind, CompletedBrowserTargetClose,
    PendingBrowserPageTargetTermination, PendingBrowserTargetClose,
};
pub(crate) use command_owner_scope::CommandOwnerScope;
pub use command_view::Cmd;
pub(crate) use cookie_manager_surface::BrowserContextCookieManagerSurfaceSnapshot;
#[cfg(test)]
pub(crate) use cookie_manager_surface::{
    BrowserContextCookieBackendConnectionState, BrowserContextDefaultCookieWriteUrlSource,
    BrowserContextDocumentCookieCacheLookupResult, BrowserContextFirstCookieRequest,
    BrowserContextStructuredCookieCommandVerdict, BrowserContextStructuredCookieWriteBackendStatus,
    BrowserContextStructuredCookieWriteReadinessStatus,
};
#[cfg(test)]
pub(crate) use cookie_owner::{
    BrowserContextCookieGetFreshnessStatus, BrowserContextCookieSetReadinessStatus,
};
pub use devtools_command::DevToolsCommandDispatchOutcome;
pub(crate) use devtools_command::DevToolsCommandExecutionOutput;
pub use dispatch::{CdpCommandTaskStep, CompletedCdpCommandDispatch, PendingCdpCommandDispatch};
pub(crate) use fetch_support::PendingStreamingDocumentResponseNavigation;
pub(crate) use fetch_support::{
    ClaimedSubresourceContinueRequest, CompletedFetchResponseBodyStreamReadDispatch,
    PendingFetchResponseBodyStreamRead, PendingFetchResponseBodyStreamReadDispatch,
    PendingFetchResponseBodyStreamReadStart, PendingSubresourceFetchResidence,
};
pub use fetch_support::{
    DocumentBodySource, FetchAuthChallenge, FetchInterceptionPattern, FetchRequestStage,
    FetchResourceTypeFilter, InFlightSubresourceFetchRequest, PausedDocumentTransfer,
    PausedDocumentTransfers, PendingFetchAuthNavigation, PendingFetchNavigation,
    PendingFetchResponseOpenedBodyStream, PendingSubresourceFetchAuthRequest,
    PendingSubresourceFetchAuthStage, PendingSubresourceFetchAuthStageChain,
    PendingSubresourceFetchOwnerKind, PendingSubresourceFetchRequest,
    PendingSubresourceFetchRequestStage, PendingSubresourceFetchRequestStageChain,
    PendingSubresourceFetchResponseRequest, PendingSubresourceFetchResponseStage,
    PendingSubresourceFetchResponseStageChain, ResponseStageUrlMatchPolicy,
    fetch_subresource_interception_config, fetch_subresource_interception_config_for_patterns,
};
pub use moli_protocol_cdp::{
    CdpRendererCommandAccess, CdpRendererCommandPolicy, CdpRendererCommandReplacement,
    CdpRendererCommandReplayDispatch, CdpRequest, ParsedCdpCommand,
};
pub(crate) use target::{CdpSessionRoute, TargetActivationTransition, TargetHandlerAccessMode};

#[derive(Clone, Debug)]
pub enum CdpTargetHostLifecycleDelta {
    Created(DevToolsTargetInfo),
    InfoChanged(DevToolsTargetInfo),
    Activated { target_id: String },
    Destroyed { target_id: String },
}

#[derive(Clone)]
pub struct CdpTargetHostLifecycleObserver {
    callback: Arc<dyn Fn(CdpTargetHostLifecycleDelta) + Send + Sync>,
}

impl CdpTargetHostLifecycleObserver {
    pub fn new(callback: impl Fn(CdpTargetHostLifecycleDelta) + Send + Sync + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }

    fn notify(&self, delta: CdpTargetHostLifecycleDelta) {
        (self.callback)(delta);
    }
}

/// The unique authority to publish that one command response has entered the
/// protocol output sequence.
///
/// This value is deliberately not `Clone`: observers may be cloned freely,
/// but only the command dispatcher may release (or drop/cancel) the waiters
/// associated with this exact command.
#[must_use = "dropping the permit cancels observers waiting for this command response"]
pub struct CommandResponseFlushPermit {
    sender: tokio::sync::watch::Sender<bool>,
    deferred_releases: Arc<Mutex<CommandResponseFlushDeferredReleases>>,
}

struct CommandResponseFlushRelease {
    release: Option<Box<dyn FnOnce() + Send + 'static>>,
}

impl CommandResponseFlushRelease {
    fn new(release: impl FnOnce() + Send + 'static) -> Self {
        Self {
            release: Some(Box::new(release)),
        }
    }

    fn run(mut self) {
        self.run_inner();
    }

    fn run_inner(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

impl Drop for CommandResponseFlushRelease {
    fn drop(&mut self) {
        self.run_inner();
    }
}

#[derive(Default)]
struct CommandResponseFlushDeferredReleases {
    finished: bool,
    releases: Vec<CommandResponseFlushRelease>,
}

#[derive(Debug)]
pub struct DevToolsDocumentLifecycleWaitKey {
    lifecycle_wait: moli_core::browser_host::BrowserDocumentLifecycleWaitTicket,
    renderer_document: moli_core::page::RendererDocumentToken,
    renderer_epoch: moli_core::page::RendererLifecycleEpoch,
    milestone: moli_core::page::RendererDocumentLifecycleMilestone,
    frame_id: String,
    loader_id: String,
}

impl DevToolsDocumentLifecycleWaitKey {
    pub fn frame_id(&self) -> &str {
        self.frame_id.as_str()
    }

    pub fn milestone(&self) -> moli_core::page::RendererDocumentLifecycleMilestone {
        self.milestone
    }

    pub fn state(&self) -> DevToolsDocumentLifecycleWaitState {
        match self.lifecycle_wait.outcome() {
            None => DevToolsDocumentLifecycleWaitState::Pending,
            Some(moli_core::browser_host::BrowserDocumentLifecycleWaitOutcome::Reached) => {
                DevToolsDocumentLifecycleWaitState::Reached
            }
            Some(moli_core::browser_host::BrowserDocumentLifecycleWaitOutcome::Interrupted {
                ..
            }) => DevToolsDocumentLifecycleWaitState::Interrupted,
            Some(moli_core::browser_host::BrowserDocumentLifecycleWaitOutcome::Superseded) => {
                DevToolsDocumentLifecycleWaitState::Superseded
            }
            Some(moli_core::browser_host::BrowserDocumentLifecycleWaitOutcome::Unavailable(_)) => {
                DevToolsDocumentLifecycleWaitState::Unavailable
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn resolved_for_event_match_test(
        renderer_document: moli_core::page::RendererDocumentToken,
        renderer_epoch: moli_core::page::RendererLifecycleEpoch,
        milestone: moli_core::page::RendererDocumentLifecycleMilestone,
        frame_id: impl Into<String>,
        loader_id: impl Into<String>,
    ) -> Self {
        Self {
            lifecycle_wait: moli_core::browser_host::BrowserDocumentLifecycleWaitTicket::resolved(
                None,
                None,
                milestone,
                moli_core::browser_host::BrowserDocumentLifecycleWaitOutcome::Reached,
            ),
            renderer_document,
            renderer_epoch,
            milestone,
            frame_id: frame_id.into(),
            loader_id: loader_id.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevToolsDocumentLifecycleWaitState {
    Pending,
    Reached,
    Interrupted,
    Superseded,
    Unavailable,
}

/// Current top-level Document readiness for one exact DevTools target route.
///
/// This deliberately distinguishes a live target that has not committed its
/// next Document from a target that no longer exists. WebDriver uses the
/// distinction to wait at the browsing-context boundary instead of probing a
/// renderer command until it happens to stop returning `NoDocumentLoaded`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DevToolsDocumentNavigationState {
    Unavailable,
    PendingNavigation,
    AwaitingCommit,
    Committed { loader_id: String },
}

impl CommandResponseFlushPermit {
    fn finish_deferred_releases(&self) -> Vec<CommandResponseFlushRelease> {
        let mut deferred = self.deferred_releases.lock();
        if deferred.finished {
            Vec::new()
        } else {
            deferred.finished = true;
            std::mem::take(&mut deferred.releases)
        }
    }

    pub fn finish(self) {
        let releases = self.finish_deferred_releases();
        let _ = self.sender.send(true);
        for release in releases {
            release.run();
        }
    }
}

impl Drop for CommandResponseFlushPermit {
    fn drop(&mut self) {
        for release in self.finish_deferred_releases() {
            release.run();
        }
    }
}

/// Cloneable, read-only observation of one command response flush.
///
/// Cloning this context creates another observer of the same command. It never
/// creates another authority capable of releasing that command's waiters.
#[derive(Clone, Default)]
pub struct CommandResponseFlushContext {
    receiver: Option<tokio::sync::watch::Receiver<bool>>,
    deferred_releases: Option<Arc<Mutex<CommandResponseFlushDeferredReleases>>>,
}

impl CommandResponseFlushContext {
    fn new(
        receiver: tokio::sync::watch::Receiver<bool>,
        deferred_releases: Arc<Mutex<CommandResponseFlushDeferredReleases>>,
    ) -> Self {
        Self {
            receiver: Some(receiver),
            deferred_releases: Some(deferred_releases),
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.receiver.is_some()
    }

    pub(crate) fn receiver(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.receiver.clone()
    }

    pub(crate) fn defer_until_response_flush(&self, release: impl FnOnce() + Send + 'static) {
        let release = CommandResponseFlushRelease::new(release);
        let immediate = match &self.deferred_releases {
            Some(deferred_releases) => {
                let mut deferred = deferred_releases.lock();
                if deferred.finished {
                    Some(release)
                } else {
                    deferred.releases.push(release);
                    None
                }
            }
            None => Some(release),
        };
        if let Some(release) = immediate {
            release.run();
        }
    }
}

#[derive(Clone, Default)]
pub struct CommandDispatchContext {
    response_flush: CommandResponseFlushContext,
    terminal_response_delivery_override: Option<moli_page_types::RendererInspectorResponseDelivery>,
    protocol_events: Vec<BackgroundProtocolEvent>,
    post_renderer_output_events: Vec<BackgroundProtocolEvent>,
    renderer_output_boundary: Option<moli_core::RendererOutputFence>,
    post_response_events: Vec<BackgroundProtocolEvent>,
    renderer_output_predecessor: Option<moli_core::RendererOutputFence>,
}

impl CommandDispatchContext {
    pub fn new(response_flush: CommandResponseFlushContext) -> Self {
        Self {
            response_flush,
            terminal_response_delivery_override: None,
            protocol_events: Vec::new(),
            post_renderer_output_events: Vec::new(),
            renderer_output_boundary: None,
            post_response_events: Vec::new(),
            renderer_output_predecessor: None,
        }
    }

    pub(crate) fn response_flush(&self) -> &CommandResponseFlushContext {
        &self.response_flush
    }

    pub(crate) fn set_terminal_response_delivery_override(
        &mut self,
        response_delivery: moli_page_types::RendererInspectorResponseDelivery,
    ) {
        self.terminal_response_delivery_override = Some(response_delivery);
    }

    pub(crate) const fn terminal_response_delivery_override(
        &self,
    ) -> Option<moli_page_types::RendererInspectorResponseDelivery> {
        self.terminal_response_delivery_override
    }

    /// Starts a move-owned participant projection with the same response
    /// flush lifetime but without copying already-produced command output.
    pub(crate) fn detached_participant_context(&self) -> Self {
        Self::new(self.response_flush.clone())
    }

    pub(crate) fn push_protocol_event(&mut self, event: BackgroundProtocolEvent) {
        self.protocol_events_mut().push(event);
    }

    pub(crate) fn protocol_events_mut(&mut self) -> &mut Vec<BackgroundProtocolEvent> {
        if self.renderer_output_boundary.is_some() {
            &mut self.post_renderer_output_events
        } else {
            &mut self.protocol_events
        }
    }

    pub(crate) fn protocol_events_len(&self) -> usize {
        self.protocol_events.len() + self.post_renderer_output_events.len()
    }

    pub(crate) fn take_protocol_events(&mut self) -> Vec<BackgroundProtocolEvent> {
        assert!(
            self.renderer_output_boundary.is_none(),
            "an exact renderer boundary must be consumed with both protocol-event segments"
        );
        std::mem::take(&mut self.protocol_events)
    }

    pub(crate) fn append_renderer_fenced_protocol_events(
        &mut self,
        before_boundary: Vec<BackgroundProtocolEvent>,
        boundary: Option<moli_core::RendererOutputFence>,
        after_boundary: Vec<BackgroundProtocolEvent>,
    ) {
        self.protocol_events_mut().extend(before_boundary);
        let Some(boundary) = boundary else {
            assert!(
                after_boundary.is_empty(),
                "post-renderer events require an exact renderer boundary"
            );
            return;
        };
        assert!(
            self.renderer_output_boundary.is_none(),
            "one command turn cannot contain multiple renderer insertion boundaries"
        );
        self.renderer_output_boundary = Some(boundary);
        self.post_renderer_output_events.extend(after_boundary);
    }

    pub(crate) fn take_renderer_fenced_protocol_events(
        &mut self,
    ) -> (
        Vec<BackgroundProtocolEvent>,
        Option<moli_core::RendererOutputFence>,
        Vec<BackgroundProtocolEvent>,
    ) {
        (
            std::mem::take(&mut self.protocol_events),
            self.renderer_output_boundary.take(),
            std::mem::take(&mut self.post_renderer_output_events),
        )
    }

    pub(crate) fn extend_post_response_events(
        &mut self,
        events: impl IntoIterator<Item = BackgroundProtocolEvent>,
    ) {
        self.post_response_events.extend(events);
    }

    pub(crate) fn take_protocol_events_before_events(
        &mut self,
        events: Vec<BackgroundProtocolEvent>,
    ) -> Vec<BackgroundProtocolEvent> {
        assert!(
            self.renderer_output_boundary.is_none(),
            "an exact renderer boundary cannot be flattened into protocol events"
        );
        let mut protocol_events = self.take_protocol_events();
        protocol_events.extend(events);
        protocol_events
    }

    pub(crate) fn take_post_response_events(&mut self) -> Vec<BackgroundProtocolEvent> {
        std::mem::take(&mut self.post_response_events)
    }

    /// Adds one exact concrete renderer cursor that must cross protocol
    /// ingress before this command's response is exposed.
    ///
    /// A cursor is source-stream scoped. Deduplication is exact and never
    /// widens the fence into a Page- or process-wide watermark.
    pub(crate) fn set_renderer_output_predecessor(
        &mut self,
        predecessor: moli_core::RendererOutputFence,
    ) {
        predecessor.merge_into_same_stream_tail(&mut self.renderer_output_predecessor);
    }

    #[doc(hidden)]
    pub fn take_renderer_output_predecessor(&mut self) -> Option<moli_core::RendererOutputFence> {
        self.renderer_output_predecessor.take()
    }

    /// Reattaches output accumulated while Browser Host owned a command
    /// participant to the frontend's original response boundary.
    ///
    /// The normal Page.navigate path detaches before producing any output, so
    /// the frontend context contains only its response-flush capability. The
    /// merge fallback deliberately preserves both event segments without a
    /// release-build panic if a future caller adds an earlier projection.
    pub(crate) fn absorb_detached_participant_projection(
        &mut self,
        mut detached: CommandDispatchContext,
    ) {
        let has_projection = self.protocol_events_len() != 0
            || self.renderer_output_boundary.is_some()
            || !self.post_response_events.is_empty()
            || self.renderer_output_predecessor.is_some();
        if !has_projection {
            detached.response_flush = self.response_flush.clone();
            *self = detached;
            return;
        }

        tracing::error!(
            "frontend command produced output before its Browser Host participant projection"
        );
        let (before_boundary, boundary, after_boundary) =
            detached.take_renderer_fenced_protocol_events();
        if self.renderer_output_boundary.is_none() {
            self.protocol_events.extend(before_boundary);
            self.renderer_output_boundary = boundary;
            self.post_renderer_output_events.extend(after_boundary);
        } else {
            if boundary.is_some() {
                tracing::error!(
                    "detached Browser Host projection introduced a second renderer boundary"
                );
            }
            self.post_renderer_output_events.extend(before_boundary);
            self.post_renderer_output_events.extend(after_boundary);
        }
        self.post_response_events
            .extend(detached.take_post_response_events());
        if let Some(predecessor) = detached.take_renderer_output_predecessor() {
            self.set_renderer_output_predecessor(predecessor);
        }
    }
}

pub(crate) use moli_protocol_cdp::{DEFAULT_LOADER_ID, monotonic_timestamp_seconds};
pub(crate) use output::NavigationBackgroundEvent;
pub use output::{
    BackgroundCommandResponsePayload, BackgroundProtocolEvent, PageScreencastFrameMetadata,
    RuntimeInspectorAsyncCompletionReceiver, RuntimeInspectorResponseReady,
    RuntimeInspectorResponseReadySender, build_event,
};
pub(crate) use output::{
    BackgroundCommandResponsePayloadRef, BackgroundServiceWorkerErrorMessage,
    BackgroundServiceWorkerRegistration, BackgroundServiceWorkerVersion,
    build_command_success_response,
};
pub(crate) use page_state::{
    LoadedNavigationRendererAttachmentCommit, PreparedLoadedNavigationPageCommit,
};
pub(crate) use runtime_eval::{
    BidiChannelOwnerActionStep, ClaimedPendingInspectorAwait, ClaimedPendingInspectorAwaitOwner,
    CompletedBidiChannelOwnerAction, CompletedRendererCallReplayBatch,
    CompletedRuntimeProtocolMessageNormalization, PendingBidiChannelOwnerAction,
    PendingRendererCallReplayBatch, PendingRuntimeProtocolMessageNormalization,
    RuntimeBindingCallEvent, RuntimeEnableReplayEvent, RuntimeProtocolMessageCompletionStep,
    renderer_command_turn_frontend_protocol_response, runtime_remote_object_ids_in_map,
};
pub use runtime_eval::{
    CompletedMoliDiagnosticsDispatch, CompletedRuntimeBindingPageCommandDispatch,
    CompletedRuntimeChildDefaultContextLookupDispatch, CompletedRuntimeEnableEventsDispatch,
    CompletedRuntimeProtocolMessageDispatch, CompletedServiceWorkerRuntimeProtocolMessageDispatch,
    CompletedSharedWorkerRuntimeProtocolMessageDispatch, PendingMoliDiagnosticsDispatch,
    PendingRuntimeBindingPageCommandDispatch, PendingRuntimeChildDefaultContextLookupDispatch,
    PendingRuntimeEnableEventsDispatch, PendingRuntimeProtocolMessageDispatch,
    PendingServiceWorkerRuntimeProtocolMessageDispatch,
    PendingSharedWorkerRuntimeProtocolMessageDispatch,
};
pub(crate) use runtime_load::decode_data_url_response;
pub(crate) use runtime_load::{
    BackgroundBufferedResponseNavigationLoadJob, BackgroundCapturedResponseNavigationLoadJob,
    BackgroundInterceptedNavigationFetchJob, BackgroundInterceptedNavigationFetchMode,
    BackgroundInterceptedNavigationFetchResult, BackgroundNavigationBodyCompletionSink,
    BackgroundNavigationEarlyOutcome, BackgroundNavigationLoadJob,
    BackgroundPausedStreamingResponseNavigationPreparationJob,
    BackgroundStreamingResponseCollectionJob, BackgroundStreamingResponseNavigationLoadJob,
    CompletedInitialDocumentPageBuild, FailedInitialDocumentPageBuild,
    InitialDocumentPageInstallResult, InitialDocumentPageOwner, PausedResponsePreparedDocument,
    PendingInitialDocumentPageBuild, ResponseCommitReady,
};
use scheduler_hooks::CdpSchedulerHooks;
use scheduler_state::CdpConnectionSchedulerState;
pub use scheduler_state::{
    BackgroundNavigationCancellation, BackgroundNavigationGateKey, CdpSchedulerEvent,
    CdpRendererOwnerTurnOutcome, CdpTurnOutcome,
};
#[cfg(test)]
pub(crate) use site_data_manager_surface::{
    BrowserContextReservedSiteDataOwnerState, BrowserContextSiteDataManagerOwnerState,
};
pub use state::{
    BackgroundTarget, BrowserContext, BrowserWindowBounds, DevToolsPageResidenceIdentity,
    DocumentStartScript, DownloadNavigation, EmulatedDeviceMetrics, EmulatedGeolocationOverride,
    EmulatedGeolocationOverrideState, EmulatedMediaOverrides, IsolatedWorldDefinition,
    LoadedNavigation, NavigationDispatchState, NavigationLoadOutcome, NavigationRequestLoadPolicy,
    PageNavigationHistoryEntry, ParkedFetchState, ParkedNetworkArtifacts, ParkedPageSessionState,
    PendingNavigationHistoryUpdate, RuntimeBindingDefinition, TargetInfo, URL_BASE,
};
pub(crate) use state::{
    BrowserContextPageStorageHandles, BrowserContextResourceStorageHandles,
    BrowserContextStoragePartitionHandles, CommittedRendererAgentAttachment,
    CommittedRendererDocumentBinding, CompletedDownloadBody, CompletedDownloadBodyArtifact,
    DedicatedWorkerMainScriptOutcome, DedicatedWorkerMainScriptSnapshot,
    DedicatedWorkerTargetState, DevToolsConsoleOutputSessionState, DevToolsLogViolationThreshold,
    DocumentNavigationToken, DuplicatePendingRendererCommand, EmulatedNetworkConditions,
    EmulatedViewportSurface, InspectorCommandDispatch, NETWORK_ERROR_PAGE_URL,
    NavigationResultProjection, NavigationSourceDocumentSecurityContext,
    NetworkErrorPageNavigation, PageScreencastConfig, PageScreencastFormat, ParkedTargetAuxState,
    ParkedTargetOwnerState, PendingBidiChannelListener, PendingInspectorAwait,
    PendingRendererCommandKey, PerformanceTimeDomain, PreparedRendererCallDispatch, ProfilerAction,
    ProfilerInspectorCommand, RendererCommandCorrelation, RendererCommandDescriptor,
    RendererCommandReplay, RendererMainDocumentCommitSeed, RendererPageResidenceIdentity,
    ServiceWorkerRuntimeExceptionSnapshot, ServiceWorkerTargetState, SharedWorkerTargetState,
    SiteDataClearOptions, TargetIdentityState, TargetInitialEmptyDocumentCreator, TargetOwnerState,
    TargetPageAttachmentId, TargetPageProtocolAttachmentIdentity, TargetPageResidenceIdentity,
    TargetPageResidenceObservation, TargetPageResidenceToken, TargetPageSessionState,
    TargetPreparedJavaScriptDialog, TargetPreparedJavaScriptDialogRoute,
    TargetRootDocumentProtocolAttachmentIdentity, TargetRuntimeSlot,
    TargetServiceWorkerProtocolAttachmentIdentity, TargetServiceWorkerProtocolAttachmentRetirement,
    TargetServiceWorkerRunIdentity, TargetServiceWorkerRunRetirement,
    TargetServiceWorkerRuntimeAttachmentIdentity, TargetServiceWorkerVersionIdentity,
    TargetServiceWorkerVersionRetirement, TargetSharedWorkerProtocolAttachmentIdentity,
    TargetSharedWorkerProtocolAttachmentRetirement, TargetSlotState, TargetWindowSurfaceState,
    viewport_surface_install_script,
};
#[cfg(test)]
pub(crate) use state::{
    DevToolsSessionState, TargetJavaScriptDialog, TargetJavaScriptDialogScopeObserver,
    TargetPageSlot, TargetRuntimeSessionState,
};
pub(crate) use target::{
    PreparedTargetAttach, PreparedTargetHostClosure, PreparedTargetHostDelta,
    TargetAttachSessionCommit, TargetBindingCleanupAction, TargetClosureCleanupPlan,
    TargetSessionDetachCleanupPlan,
};
use target::{
    TargetClosurePlan, TargetControlPlane, TargetEventPlan, TargetHostDelta,
    target_destroyed_automation_events,
};

pub struct PendingDeferredMainDocumentLoadCompletion {
    inner: crate::domains::activity::PendingDeferredMainDocumentLoadCompletionActivity,
}

/// Read-only readiness for one exact deferred main-document load terminal.
///
/// This probe lets an application adapter preserve causal ordering after the
/// one-shot completion has moved into its async wait task. It carries no Page
/// capability and cannot consume or publish the lifecycle terminal.
#[derive(Clone, Debug)]
pub struct DeferredMainDocumentLoadCompletionReadiness {
    inner: moli_core::browser_host::BrowserDocumentLifecycleWaitReadiness,
}

pub struct CompletedDeferredMainDocumentLoadCompletion {
    inner: crate::domains::activity::CompletedDeferredMainDocumentLoadCompletionActivity,
}

/// Stable identity of one exact deferred-load lifecycle observation.
///
/// The protocol owner allocates this identity before an adapter starts an
/// asynchronous wait. CDP, BiDi, and Classic carry it through the typed
/// completion instead of manufacturing adapter-local observation generations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferredMainDocumentLoadObservationId(u64);

impl DeferredMainDocumentLoadObservationId {
    #[cfg(feature = "test-support")]
    pub(crate) fn from_test_value(value: u64) -> Self {
        assert_ne!(value, 0, "load observation identity starts at one");
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredMainDocumentLoadCompletionOutputInterest {
    renderer_page: Option<RendererPageResidenceIdentity>,
    renderer_document: Option<moli_core::RendererDocumentLifecycleIdentity>,
}

/// Exact scope of concrete renderer output that may still acquire a
/// main-document load predecessor from the command turn currently completing.
///
/// This value is derived while consuming a one-shot renderer publication. It
/// retains only the Page/Document identity needed for a later load action to
/// prove causality; it carries neither a renderer source capability nor
/// permission to rescan Page state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferredMainDocumentLoadPredecessorCandidate {
    renderer_page: RendererPageResidenceIdentity,
    renderer_document: moli_core::RendererDocumentLifecycleIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredMainDocumentLoadCompletionOutputAction {
    ProcessNow,
    Queue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IdleNavigationEngineReleaseResult {
    pub(crate) reset: bool,
    pub(crate) reason: &'static str,
    pub(crate) loaded_browser_context_count: usize,
    pub(crate) live_target_browser_context_count: usize,
    pub(crate) retained_background_navigation_engine_count: usize,
}

impl IdleNavigationEngineReleaseResult {
    pub(crate) fn to_protocol_json(self) -> serde_json::Value {
        json!({
            "reset": self.reset,
            "reason": self.reason,
            "loadedBrowserContextCount": self.loaded_browser_context_count,
            "liveTargetBrowserContextCount": self.live_target_browser_context_count,
            "retainedBackgroundNavigationEngineCount": self.retained_background_navigation_engine_count,
        })
    }
}

impl PendingDeferredMainDocumentLoadCompletion {
    pub(crate) fn new(
        inner: crate::domains::activity::PendingDeferredMainDocumentLoadCompletionActivity,
    ) -> Self {
        Self { inner }
    }

    pub fn session_id(&self) -> Option<&str> {
        self.inner.session_id()
    }

    pub fn output_interest(&self) -> DeferredMainDocumentLoadCompletionOutputInterest {
        DeferredMainDocumentLoadCompletionOutputInterest::new(
            self.inner.renderer_page_residence_identity(),
            self.inner.renderer_document_identity(),
        )
    }

    pub fn observation_id(&self) -> DeferredMainDocumentLoadObservationId {
        self.inner.observation_id()
    }

    pub fn readiness(&self) -> DeferredMainDocumentLoadCompletionReadiness {
        DeferredMainDocumentLoadCompletionReadiness {
            inner: self.inner.fact_readiness(),
        }
    }

    pub async fn wait(self) -> CompletedDeferredMainDocumentLoadCompletion {
        CompletedDeferredMainDocumentLoadCompletion {
            inner: self.inner.wait().await,
        }
    }
}

impl DeferredMainDocumentLoadCompletionReadiness {
    pub fn is_terminal(&self) -> bool {
        self.inner.is_terminal()
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn reached_for_test_support() -> Self {
        Self {
            inner: moli_core::browser_host::BrowserDocumentLifecycleWaitReadiness::terminal_for_test_support(),
        }
    }
}

impl CompletedDeferredMainDocumentLoadCompletion {
    pub(crate) fn new(
        inner: crate::domains::activity::CompletedDeferredMainDocumentLoadCompletionActivity,
    ) -> Self {
        Self { inner }
    }

    pub fn session_id(&self) -> Option<&str> {
        self.inner.session_id()
    }

    pub fn observation_id(&self) -> DeferredMainDocumentLoadObservationId {
        self.inner.observation_id()
    }
}

impl DeferredMainDocumentLoadCompletionOutputInterest {
    pub(crate) fn new(
        renderer_page: Option<RendererPageResidenceIdentity>,
        renderer_document: Option<moli_core::RendererDocumentLifecycleIdentity>,
    ) -> Self {
        Self {
            renderer_page,
            renderer_document,
        }
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn from_test_residence(
        renderer_page: RendererPageResidenceIdentity,
        renderer_document: Option<moli_core::RendererDocumentLifecycleIdentity>,
    ) -> Self {
        Self::new(Some(renderer_page), renderer_document)
    }

    pub fn route_output_while_waiting(
        &self,
        message: &RendererOutputTransportMessage,
    ) -> DeferredMainDocumentLoadCompletionOutputAction {
        let RendererOutputTransportMessage::Publication(publication) = message else {
            return DeferredMainDocumentLoadCompletionOutputAction::ProcessNow;
        };
        let residence = publication.cursor().stream().residence();
        if !self
            .renderer_page
            .is_some_and(|renderer_page| renderer_page.matches_residence(residence))
        {
            return DeferredMainDocumentLoadCompletionOutputAction::ProcessNow;
        }
        match publication.ordering() {
            RendererOutputPublicationOrdering::AfterPendingPageLoad { source_document }
                if self.renderer_document == Some(source_document) =>
            {
                DeferredMainDocumentLoadCompletionOutputAction::Queue
            }
            RendererOutputPublicationOrdering::Unconstrained
            | RendererOutputPublicationOrdering::AfterPendingPageLoad { .. } => {
                DeferredMainDocumentLoadCompletionOutputAction::ProcessNow
            }
        }
    }

    pub fn observes_predecessor_candidate(
        &self,
        candidate: DeferredMainDocumentLoadPredecessorCandidate,
    ) -> bool {
        self.renderer_page == Some(candidate.renderer_page)
            && self.renderer_document == Some(candidate.renderer_document)
    }
}

impl DeferredMainDocumentLoadPredecessorCandidate {
    /// Selects only work whose browser-visible effects are ordered after the
    /// exact Page's load boundary.
    ///
    /// Parser, module, child-frame and ordinary lifecycle output are load
    /// prerequisites and therefore return `None`. A timer is Page-scoped;
    /// lifecycle action output additionally carries its exact source
    /// Document.
    pub fn from_renderer_publication(publication: &RendererOutputTransportMessage) -> Option<Self> {
        let RendererOutputTransportMessage::Publication(publication) = publication else {
            return None;
        };
        let RendererOutputPublicationOrdering::AfterPendingPageLoad { source_document } =
            publication.ordering()
        else {
            return None;
        };
        Some(Self {
            renderer_page: RendererPageResidenceIdentity::from_residence(
                publication.cursor().stream().residence(),
            )
            .expect("post-load publication ordering is only valid for a Page stream"),
            renderer_document: source_document,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeAwaitJob {
    command_id: u64,
    session_id: Option<String>,
    owner_route: Option<CdpSessionRoute>,
    object_group: Option<String>,
    action: &'static str,
}

impl RuntimeAwaitJob {
    pub(crate) fn new(
        command_id: u64,
        session_id: Option<&str>,
        owner_route: Option<CdpSessionRoute>,
        object_group: Option<&str>,
        action: &'static str,
    ) -> Self {
        Self {
            command_id,
            session_id: session_id.map(str::to_owned),
            owner_route,
            object_group: object_group.map(str::to_owned),
            action,
        }
    }

    pub(crate) fn trace_fields(&self) -> serde_json::Value {
        json!({
            "commandId": self.command_id,
            "sessionId": self.session_id,
            "ownerRoute": self.owner_route.as_ref().map(|route| format!("{route:?}")),
            "objectGroup": self.object_group,
            "action": self.action,
        })
    }

    pub(crate) fn session_id(&self) -> Option<String> {
        self.session_id.clone()
    }
}

#[derive(Clone)]
pub struct CdpInitialStoragePartition {
    handles: BrowserContextStoragePartitionHandles,
    fallback_session_storage_store: SharedWebStorageStore,
}

impl CdpInitialStoragePartition {
    pub fn memory() -> Self {
        Self::new(BrowserContextStoragePartitionHandles::memory())
    }

    pub fn with_cookies(cookies: Vec<StoredCookie>) -> Self {
        Self::new(BrowserContextStoragePartitionHandles::with_initial_cookies(
            cookies,
        ))
    }

    fn new(handles: BrowserContextStoragePartitionHandles) -> Self {
        Self {
            handles,
            fallback_session_storage_store: new_shared_web_storage_store(),
        }
    }

    /// Creates a default-partition view over the Browser-owned stores.
    ///
    /// The returned handle shares the live cookie store instead of taking a
    /// connection-local snapshot that must be merged back on frontend
    /// teardown. The application-owned [`StoragePartitionState`] therefore
    /// remains the only profile cookie authority.
    pub fn from_storage_partition(storage_partition: &StoragePartitionState) -> Self {
        Self::new(BrowserContextStoragePartitionHandles::from_storage_partition(storage_partition))
    }

    fn into_parts(self) -> (BrowserContextStoragePartitionHandles, SharedWebStorageStore) {
        (self.handles, self.fallback_session_storage_store)
    }
}

struct CdpInitialStoragePartitionOwner {
    handles: BrowserContextStoragePartitionHandles,
    fallback_session_storage_store: SharedWebStorageStore,
}

impl CdpInitialStoragePartitionOwner {
    fn new(
        handles: BrowserContextStoragePartitionHandles,
        fallback_session_storage_store: SharedWebStorageStore,
    ) -> Self {
        Self {
            handles,
            fallback_session_storage_store,
        }
    }

    fn from_initial_storage_partition(
        initial_storage_partition: CdpInitialStoragePartition,
    ) -> Self {
        let (handles, fallback_session_storage_store) = initial_storage_partition.into_parts();
        Self::new(handles, fallback_session_storage_store)
    }

    fn new_default_browser_context(
        &self,
        id: String,
        http_cache_root: Option<PathBuf>,
        http_cache_max_bytes: Option<u64>,
    ) -> BrowserContext {
        BrowserContext::new_with_storage_partition_handles_and_http_cache(
            id,
            self.handles.clone(),
            http_cache_root,
            http_cache_max_bytes,
        )
    }

    fn resource_storage_handles(&self) -> BrowserContextResourceStorageHandles {
        self.handles
            .resource_storage_handles(self.fallback_session_storage_store.clone())
    }

    fn page_storage_handles(&self) -> BrowserContextPageStorageHandles {
        self.handles
            .page_storage_handles(self.fallback_session_storage_store.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AutoAttachOwnerPolicy {
    wait_for_debugger_on_start: bool,
    target_filter: CdpTargetFilter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CdpTargetFilterEntry {
    pub(crate) exclude: bool,
    pub(crate) target_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CdpTargetFilter {
    entries: Vec<CdpTargetFilterEntry>,
}

impl CdpTargetFilter {
    pub(crate) fn from_entries(entries: Vec<CdpTargetFilterEntry>) -> Self {
        Self { entries }
    }

    pub(crate) fn from_devtools_entries(entries: Vec<DevToolsTargetFilterEntry>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|entry| CdpTargetFilterEntry {
                    exclude: entry.exclude,
                    target_type: entry.target_type,
                })
                .collect(),
        }
    }

    pub(crate) fn to_devtools_entries(&self) -> Vec<DevToolsTargetFilterEntry> {
        self.entries
            .iter()
            .map(|entry| DevToolsTargetFilterEntry {
                exclude: entry.exclude,
                target_type: entry.target_type.clone(),
            })
            .collect()
    }

    pub(crate) fn default_target_discovery() -> Self {
        Self::default_auto_attach()
    }

    pub(crate) fn default_auto_attach() -> Self {
        Self {
            entries: vec![
                CdpTargetFilterEntry {
                    exclude: true,
                    target_type: Some("browser".to_owned()),
                },
                CdpTargetFilterEntry {
                    exclude: true,
                    target_type: Some("tab".to_owned()),
                },
                CdpTargetFilterEntry {
                    exclude: false,
                    target_type: None,
                },
            ],
        }
    }

    pub(crate) fn matches(&self, target_type: &str) -> bool {
        for entry in &self.entries {
            if entry
                .target_type
                .as_deref()
                .is_none_or(|entry_type| entry_type == target_type)
            {
                return !entry.exclude;
            }
        }
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ServiceWorkerAutoAttachRelatedOwner {
    owner_session_id: Option<String>,
    browser_context_id: String,
    registration_id: u64,
    base_version_id: u64,
    script_url: String,
    scope_url: String,
    allow_service_worker_targets: bool,
    wait_for_debugger_on_start: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServiceWorkerAutoAttachRelatedOwnerSession {
    pub(crate) owner_session_id: Option<String>,
    pub(crate) wait_for_debugger_on_start: bool,
}

/// Persistent renderer/DevTools projection for one application-owned Host
/// adapter.
///
/// The type name is retained for direct typed APIs and tests, but production
/// socket frontends do not own this value. The application wraps it in
/// [`crate::DevToolsHostAdapter`]; CDP endpoints attach through their own
/// command and output queues.
pub struct CdpConnection {
    // Transitional physical Context/Target/Page projections. Authoritative
    // topology, renderer Page lifetime owners and per-Context runtime roots
    // live in BrowserHostState; these values still combine remaining physical
    // command/cache payload with CDP session and subscription projection.
    pub browser_context: Option<BrowserContext>,
    pub inactive_browser_contexts: Vec<BrowserContext>,
    // Browser/session routing state.
    browser_session_ids: HashSet<String>,
    /// When true, every newly-created target is immediately auto-attached.
    pub auto_attach: bool,
    /// Root/browser owner Target discovery mirror for diagnostics and
    /// cross-crate schedulers. Event routing uses `target_handlers`.
    pub(crate) target_discovery_enabled: bool,
    /// Whether URL/title changes should be surfaced through
    /// Target.targetInfoChanged for the root/browser owner. Event routing uses
    /// `target_handlers`.
    pub(crate) target_info_change_events_enabled: bool,
    pub(crate) target_discovery_filter: Option<Vec<DevToolsTargetFilterEntry>>,
    /// Chromium/Playwright auto-attach can ask new targets to wait until
    /// Runtime.runIfWaitingForDebugger before their initial document proceeds.
    pub auto_attach_wait_for_debugger_on_start: bool,
    auto_attach_owner_sessions: HashMap<Option<String>, AutoAttachOwnerPolicy>,
    target_control: TargetControlPlane,
    webdriver_bidi_target_lifecycle_projection_enabled: bool,
    service_worker_auto_attach_related_owners: Vec<ServiceWorkerAutoAttachRelatedOwner>,
    service_worker_pause_on_start_owner_sessions: HashSet<Option<String>>,
    dedicated_worker_pause_on_start_owner_sessions: HashSet<Option<String>>,
    install_default_target_on_auto_attach: bool,
    // BrowserContext, Target and Browser command sequences live in the Host.
    // These remaining counters identify only this frontend/session/runtime.
    next_session_id: u32,
    next_page_domain_subscription_generation: u64,
    next_internal_runtime_command_id: u64,
    prepared_browser_owner_commands:
        HashMap<BrowserCommandId, browser_owner_input::PreparedBrowserOwnerCommand>,
    none_session_owner_route_override: Option<CdpSessionRoute>,
    pending_runtime_await_jobs: HashMap<PendingRendererCommandKey, RuntimeAwaitJob>,
    claimed_pending_inspector_await_owners:
        HashMap<PendingRendererCommandKey, ClaimedPendingInspectorAwaitOwner>,

    // Browser download policy and active artifact registry live in
    // BrowserHostState. Only CDP/BiDi event subscriptions are connection-local.
    download_event_subscriptions: BrowserDownloadEventSubscriptions,
    next_global_io_stream_id: u64,
    pub(crate) network_data_collectors: crate::domains::network::NetworkDataCollectorStore,
    initial_storage_partition: CdpInitialStoragePartitionOwner,
    pub(crate) global_io_streams: HashMap<String, IoStreamState>,
    pub(crate) tracing_state: crate::domains::tracing::TracingState,

    // Transport/scheduler integration hooks. These are channels out of the
    // renderer/browser owner into the outer CDP scheduler; they should not grow
    // into protocol routing state.
    scheduler_hooks: CdpSchedulerHooks,
    /// Migration-period application handle for protocol-neutral Browser Owner
    /// input. Protocol may publish through it but cannot select or execute a
    /// Browser Host turn.
    browser_host_handle: Option<BrowserHostHandle>,
    target_host_lifecycle_observer: Option<CdpTargetHostLifecycleObserver>,

    // Scheduler-visible queues that are still stored on the connection while
    // source-specific queue ownership is being migrated outward.
    scheduler_state: CdpConnectionSchedulerState,

    // Frontend-owned cursor and projection state over immutable Browser facts.
    // This cursor may lag or close without backpressuring Browser Owner state.
    browser_fact_projector: browser_fact_projection::CdpBrowserFactProjector,

    // Access capability for the application-owned Browser Host residence.
    // The authoritative registries, renderer engines and fact journal live in
    // `BrowserHostState`; they are no longer embedded in this Protocol value.
    browser_host_state: BrowserHostState,
}

impl Default for CdpConnection {
    fn default() -> Self {
        Self::new()
    }
}

impl CdpConnection {
    pub fn new() -> Self {
        Self::new_with_initial_storage_partition(CdpInitialStoragePartition::memory())
    }

    pub(crate) fn layout_policy(&self) -> LayoutPolicy {
        self.browser_host_state
            .navigation_owner()
            .active_runtime_config()
            .layout_policy()
    }

    pub fn has_pending_javascript_dialog(&self) -> bool {
        self.browser_context
            .iter()
            .chain(self.inactive_browser_contexts.iter())
            .any(BrowserContext::has_pending_javascript_dialog)
    }

    pub fn set_automation_javascript_dialog_handler_enabled(&mut self, enabled: bool) -> bool {
        let Some(browser_context) = self.browser_context.as_ref() else {
            return false;
        };
        browser_context
            .renderer_runtime()
            .set_javascript_dialog_handler_enabled(enabled);
        true
    }

    pub fn new_with_initial_cookies(initial_cookies: Vec<StoredCookie>) -> Self {
        Self::new_with_initial_storage_partition(CdpInitialStoragePartition::with_cookies(
            initial_cookies,
        ))
    }

    pub fn new_with_fetch_config(fetch_config: FetchConfig) -> Self {
        Self::new_with_initial_storage_partition_and_fetch_config(
            CdpInitialStoragePartition::memory(),
            fetch_config,
        )
    }

    pub fn enable_webdriver_bidi_download_events(&mut self) -> bool {
        self.download_event_subscriptions
            .enable_webdriver_bidi_events()
    }

    pub fn disable_webdriver_bidi_download_events(&mut self) -> bool {
        self.download_event_subscriptions
            .disable_webdriver_bidi_events()
    }

    pub(crate) fn set_browser_download_events_enabled_for_session(
        &mut self,
        session_id: Option<&str>,
        enabled: bool,
    ) {
        self.download_event_subscriptions
            .set_browser_events_enabled_for_session(session_id, enabled);
    }

    pub(crate) fn set_automation_download_events_enabled_for_browser_context(
        &mut self,
        browser_context_id: Option<&str>,
        enabled: bool,
    ) {
        self.download_event_subscriptions
            .set_automation_events_enabled_for_browser_context(browser_context_id, enabled);
    }

    pub(crate) fn clear_automation_download_events_for_browser_context(
        &mut self,
        browser_context_id: &str,
    ) {
        self.download_event_subscriptions
            .clear_automation_events_for_browser_context(browser_context_id);
    }

    pub(crate) fn ensure_automation_download_event_override_for_browser_context(
        &mut self,
        browser_context_id: &str,
    ) {
        self.download_event_subscriptions
            .ensure_automation_event_override_for_browser_context(browser_context_id);
    }

    pub(crate) fn automation_download_events_enabled_for_browser_context(
        &self,
        browser_context_id: Option<&str>,
    ) -> bool {
        self.download_event_subscriptions
            .automation_events_enabled_for_browser_context(browser_context_id)
    }

    #[cfg(test)]
    pub(crate) fn browser_download_event_session_ids_for_test(&self) -> Vec<Option<String>> {
        self.download_event_subscriptions
            .browser_event_session_ids()
    }

    pub fn enable_webdriver_bidi_target_lifecycle_projection(&mut self) -> bool {
        let changed = !self.webdriver_bidi_target_lifecycle_projection_enabled;
        self.webdriver_bidi_target_lifecycle_projection_enabled = true;
        changed
    }

    pub fn disable_webdriver_bidi_target_lifecycle_projection(&mut self) -> bool {
        let changed = self.webdriver_bidi_target_lifecycle_projection_enabled;
        self.webdriver_bidi_target_lifecycle_projection_enabled = false;
        changed
    }

    pub(crate) fn webdriver_bidi_target_lifecycle_projection_enabled(&self) -> bool {
        self.webdriver_bidi_target_lifecycle_projection_enabled
    }

    pub fn new_with_initial_storage_partition(
        initial_storage_partition: CdpInitialStoragePartition,
    ) -> Self {
        Self::new_with_initial_storage_partition_and_fetch_config(
            initial_storage_partition,
            FetchConfig::default(),
        )
    }

    pub fn new_with_initial_storage_partition_and_fetch_config(
        initial_storage_partition: CdpInitialStoragePartition,
        fetch_config: FetchConfig,
    ) -> Self {
        Self::new_with_initial_storage_partition_fetch_config_and_resource_loading(
            initial_storage_partition,
            fetch_config,
            OptionalResourceFetchMask::NONE,
            true,
        )
    }

    pub fn new_with_initial_storage_partition_fetch_config_and_image_fetch_enabled(
        initial_storage_partition: CdpInitialStoragePartition,
        fetch_config: FetchConfig,
        image_fetch_enabled: bool,
    ) -> Self {
        let optional_resource_fetch_mask = if image_fetch_enabled {
            OptionalResourceFetchMask::IMAGE
        } else {
            OptionalResourceFetchMask::NONE
        };
        Self::new_with_initial_storage_partition_fetch_config_and_resource_loading(
            initial_storage_partition,
            fetch_config,
            optional_resource_fetch_mask,
            true,
        )
    }

    pub fn new_with_initial_storage_partition_fetch_config_and_resource_loading(
        initial_storage_partition: CdpInitialStoragePartition,
        fetch_config: FetchConfig,
        optional_resource_fetch_mask: OptionalResourceFetchMask,
        subframe_loading_enabled: bool,
    ) -> Self {
        Self::new_with_initial_storage_partition_and_runtime_config(
            initial_storage_partition,
            NavigationRuntimeConfig::new(
                fetch_config,
                optional_resource_fetch_mask,
                subframe_loading_enabled,
                LayoutPolicy::default(),
            ),
        )
    }

    pub fn new_with_initial_storage_partition_and_runtime_config(
        initial_storage_partition: CdpInitialStoragePartition,
        navigation_runtime_config: NavigationRuntimeConfig,
    ) -> Self {
        let browser_host_state = BrowserHostState::new(NavigationEngine::new_with_runtime_config(
            navigation_runtime_config,
        ));
        Self::new_with_browser_host_state_and_initial_storage_partition(
            browser_host_state,
            initial_storage_partition,
        )
    }

    /// Builds one Protocol adapter over an already-created Browser Host
    /// residence. Application composition roots use this constructor so the
    /// Host allocation is established before the frontend adapter.
    #[doc(hidden)]
    pub fn new_with_browser_host_state_and_initial_storage_partition(
        browser_host_state: BrowserHostState,
        initial_storage_partition: CdpInitialStoragePartition,
    ) -> Self {
        Self::new_with_browser_host_state_and_initial_storage_partition_owner(
            browser_host_state,
            CdpInitialStoragePartitionOwner::from_initial_storage_partition(
                initial_storage_partition,
            ),
        )
    }

    fn new_with_browser_host_state_and_initial_storage_partition_owner(
        browser_host_state: BrowserHostState,
        initial_storage_partition: CdpInitialStoragePartitionOwner,
    ) -> Self {
        let browser_fact_projector = browser_fact_projection::CdpBrowserFactProjector::new(
            browser_host_state.subscribe_browser_facts(),
        );
        Self {
            browser_context: None,
            inactive_browser_contexts: Vec::new(),
            browser_session_ids: HashSet::new(),
            auto_attach: false,
            target_discovery_enabled: false,
            target_info_change_events_enabled: false,
            target_discovery_filter: None,
            auto_attach_wait_for_debugger_on_start: false,
            auto_attach_owner_sessions: HashMap::new(),
            target_control: TargetControlPlane::default(),
            webdriver_bidi_target_lifecycle_projection_enabled: false,
            service_worker_auto_attach_related_owners: Vec::new(),
            service_worker_pause_on_start_owner_sessions: HashSet::new(),
            dedicated_worker_pause_on_start_owner_sessions: HashSet::new(),
            install_default_target_on_auto_attach: false,
            download_event_subscriptions: BrowserDownloadEventSubscriptions::default(),
            next_global_io_stream_id: 0,
            next_session_id: 0,
            next_page_domain_subscription_generation: 0,
            next_internal_runtime_command_id: 902_000_000,
            prepared_browser_owner_commands: HashMap::new(),
            pending_runtime_await_jobs: HashMap::new(),
            claimed_pending_inspector_await_owners: HashMap::new(),
            network_data_collectors: crate::domains::network::NetworkDataCollectorStore::default(),
            initial_storage_partition,
            global_io_streams: HashMap::new(),
            tracing_state: crate::domains::tracing::TracingState::default(),
            scheduler_hooks: CdpSchedulerHooks::default(),
            browser_host_handle: None,
            target_host_lifecycle_observer: None,
            scheduler_state: CdpConnectionSchedulerState::default(),
            none_session_owner_route_override: None,
            browser_fact_projector,
            browser_host_state,
        }
    }

    pub fn browser_host_state(&self) -> BrowserHostState {
        self.browser_host_state.clone()
    }

    pub(crate) fn browser_host_policy_snapshot(&self) -> BrowserHostPolicyState {
        self.browser_host_state.policy_snapshot()
    }

    #[cfg(test)]
    pub(crate) fn browser_download_policy_snapshot(
        &self,
    ) -> moli_core::browser_host::BrowserDownloadPolicyState {
        self.browser_host_state.download_policy_snapshot()
    }

    pub(crate) fn effective_browser_download_policy(
        &self,
        browser_context_id: Option<&str>,
    ) -> BrowserDownloadPolicy {
        self.browser_host_state
            .effective_download_policy(browser_context_id)
    }

    pub(crate) fn apply_browser_download_policy_update(&self, update: BrowserDownloadPolicyUpdate) {
        self.browser_host_state.apply_download_policy_update(update);
    }

    pub(crate) fn browser_download_registry(&self) -> BrowserDownloadRegistry {
        self.browser_host_state.download_registry()
    }

    pub(crate) fn browser_host_network_policy_snapshot(&self) -> BrowserHostNetworkPolicySnapshot {
        self.browser_host_state.network_policy_snapshot()
    }

    pub(crate) fn apply_browser_host_policy_update(&self, update: BrowserHostPolicyUpdate) {
        self.browser_host_state.apply_policy_update(update);
    }

    pub fn set_background_event_sender(&mut self, sender: BackgroundEventSender) {
        self.scheduler_hooks.set_background_event_sender(sender);
    }

    pub fn set_target_host_lifecycle_observer(&mut self, observer: CdpTargetHostLifecycleObserver) {
        self.target_host_lifecycle_observer = Some(observer);
    }

    pub fn set_runtime_inspector_response_ready_sender(
        &mut self,
        sender: RuntimeInspectorResponseReadySender,
    ) {
        self.scheduler_hooks
            .set_runtime_inspector_response_ready_sender(sender);
    }

    pub fn set_background_navigation_completion_sender(
        &mut self,
        sender: tokio::sync::mpsc::UnboundedSender<
            crate::domains::page::BackgroundNavigationCompletion,
        >,
    ) {
        self.scheduler_hooks
            .set_background_navigation_completion_sender(sender);
    }

    pub fn set_renderer_publication_sender(
        &mut self,
        sender: moli_core::RendererOutputTransportSender,
    ) {
        self.browser_host_state
            .set_renderer_output_transport_sender(sender);
    }

    pub(crate) fn background_event_sender(&self) -> Option<BackgroundEventSender> {
        self.scheduler_hooks.background_event_sender()
    }

    pub(crate) fn runtime_inspector_response_ready_sender(
        &self,
    ) -> Option<RuntimeInspectorResponseReadySender> {
        self.scheduler_hooks
            .runtime_inspector_response_ready_sender()
    }

    pub(crate) fn record_background_navigation_started_scheduler_event(
        &mut self,
        token: &DocumentNavigationToken,
        state: &NavigationDispatchState,
        cancellation: BackgroundNavigationCancellation,
    ) {
        let key = BackgroundNavigationGateKey::for_navigation(token, state);
        self.scheduler_state
            .push_scheduler_event(CdpSchedulerEvent::BackgroundNavigationStarted {
                key,
                cancellation,
            });
    }

    pub fn renderer_document_navigation_is_suspended_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> bool {
        self.runtime_session_owner_slot(session_id)
            .is_ok_and(TargetRuntimeSlot::renderer_document_navigation_is_suspended)
    }

    pub(crate) fn bind_renderer_document_lifecycle_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        artifacts: moli_core::page::RendererPageCreationArtifacts,
        navigation: Option<DocumentNavigationToken>,
        frame_id: String,
        loader_id: String,
    ) -> (
        Option<CommittedRendererDocumentBinding>,
        Vec<moli_core::page::RendererDocumentLifecycleEvent>,
    ) {
        if navigation.as_ref().is_some_and(|navigation| {
            !self.accepts_committed_document_navigation_for_session_owner(session_id, navigation)
        }) {
            return (None, Vec::new());
        }
        let fact_page = self.target_page_residence_identity_for_session(session_id);
        let previous_document_scope = self
            .committed_renderer_document_binding_for_session_owner(session_id)
            .map(CommittedRendererDocumentBinding::renderer_document_identity);
        let Ok(slot) = self.runtime_session_owner_slot_mut(session_id) else {
            return (None, Vec::new());
        };
        let ingress = slot
            .page_slot_mut()
            .bind_renderer_document_lifecycle_with_ingress(
                artifacts, navigation, frame_id, loader_id,
            );
        let binding = self
            .committed_renderer_document_binding_for_session_owner(session_id)
            .cloned();
        let current_document_scope = binding
            .as_ref()
            .map(CommittedRendererDocumentBinding::renderer_document_identity);
        let document_scope_changed = current_document_scope != previous_document_scope;
        if document_scope_changed {
            self.retire_javascript_dialogs_for_session_owner(session_id);
        }
        self.record_authoritative_renderer_document_lifecycle_facts(
            fact_page.as_ref(),
            binding.as_ref(),
            ingress.authoritative(),
        );
        (binding, ingress.into_visible())
    }

    pub(crate) fn ingest_renderer_document_lifecycle_events_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        events: Vec<moli_core::page::RendererDocumentLifecycleEvent>,
    ) -> (
        Option<CommittedRendererDocumentBinding>,
        Vec<moli_core::page::RendererDocumentLifecycleEvent>,
    ) {
        let fact_page = self.target_page_residence_identity_for_session(session_id);
        let Some(previous_binding) = self
            .committed_renderer_document_binding_for_session_owner(session_id)
            .cloned()
        else {
            return (None, Vec::new());
        };
        let previous_document_scope = previous_binding.renderer_document_identity();
        let Ok(slot) = self.runtime_session_owner_slot_mut(session_id) else {
            return (None, Vec::new());
        };
        let ingress = slot
            .page_slot_mut()
            .ingest_renderer_document_lifecycle_events_with_ingress(events);
        let binding = self
            .committed_renderer_document_binding_for_session_owner(session_id)
            .cloned();
        let current_document_scope = binding
            .as_ref()
            .map(CommittedRendererDocumentBinding::renderer_document_identity);
        let document_scope_changed = current_document_scope != Some(previous_document_scope);
        let document_input_stream_opened = binding.as_ref().is_some_and(|binding| {
            binding.document_open_replacement_epoch == Some(binding.renderer_epoch)
        });
        if document_scope_changed {
            self.retire_javascript_dialogs_for_session_owner(session_id);
        }
        if document_input_stream_opened {
            // The concrete lifecycle record is the authoritative notification
            // that `document.open()` replaced the initial empty Document. Do
            // not defer this state transition to a later diagnostics snapshot:
            // that would make a later owner turn rediscover and settle output
            // produced by this renderer turn.
            self.mark_target_initial_empty_document_exited_for_session_owner(session_id);
        }
        self.record_authoritative_renderer_document_lifecycle_facts(
            fact_page.as_ref(),
            binding.as_ref(),
            ingress.authoritative(),
        );
        (binding, ingress.into_visible())
    }

    fn retire_javascript_dialogs_for_session_owner(&mut self, session_id: Option<&str>) {
        let event_session_ids = self.page_event_session_ids_for_session_owner(session_id);
        if let Ok(slot) = self.runtime_session_owner_slot_mut(session_id) {
            slot.retire_javascript_dialog_scope();
        }
        for event_session_id in event_session_ids {
            let _ = self.with_target_devtools_session_state_for_session_mut(
                event_session_id.as_deref(),
                |state| state.page_session_state.javascript_dialog_state.clear(),
            );
        }
    }

    pub(crate) fn begin_renderer_document_load_visibility_barrier_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        loader_id: &str,
    ) -> bool {
        if self
            .committed_renderer_document_binding_for_session_owner(session_id)
            .is_none()
        {
            return false;
        }
        self.runtime_session_owner_slot_mut(session_id)
            .is_ok_and(|slot| {
                slot.page_slot_mut()
                    .begin_renderer_document_load_visibility_barrier(loader_id)
            })
    }

    pub(crate) fn release_renderer_document_load_visibility_barrier_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        loader_id: &str,
    ) -> Option<Vec<moli_core::page::RendererDocumentLifecycleEvent>> {
        self.committed_renderer_document_binding_for_session_owner(session_id)?;
        self.runtime_session_owner_slot_mut(session_id)
            .ok()?
            .page_slot_mut()
            .release_renderer_document_load_visibility_barrier(loader_id)
    }

    pub(crate) fn cancel_renderer_document_load_visibility_barrier_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        loader_id: &str,
    ) -> bool {
        if self
            .committed_renderer_document_binding_for_session_owner(session_id)
            .is_none()
        {
            return false;
        }
        self.runtime_session_owner_slot_mut(session_id)
            .is_ok_and(|slot| {
                slot.page_slot_mut()
                    .cancel_renderer_document_load_visibility_barrier(loader_id)
            })
    }

    #[cfg(test)]
    pub(crate) fn renderer_document_lifecycle_authoritative_state_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<(
        CommittedRendererDocumentBinding,
        moli_core::page::RendererDocumentLifecycleSnapshot,
    )> {
        let binding = self
            .committed_renderer_document_binding_for_session_owner(session_id)?
            .clone();
        let page_slot = self
            .runtime_session_owner_slot(session_id)
            .ok()?
            .page_slot();
        Some((
            binding,
            page_slot.renderer_document_lifecycle_authoritative_snapshot()?,
        ))
    }

    pub(crate) fn exact_renderer_document_lifecycle_fact_wait_for_session_owner(
        &self,
        session_id: Option<&str>,
        expected_binding: Option<&CommittedRendererDocumentBinding>,
        milestone: moli_core::page::RendererDocumentLifecycleMilestone,
    ) -> moli_core::browser_host::BrowserDocumentLifecycleWaitTicket {
        let Some(expected_binding) = expected_binding else {
            return moli_core::browser_host::BrowserDocumentLifecycleWaitTicket::resolved(
                None,
                None,
                milestone,
                moli_core::browser_host::BrowserDocumentLifecycleWaitOutcome::Unavailable(
                    moli_core::browser_host::BrowserDocumentLifecycleWaitUnavailableReason::NoCurrentPage,
                ),
            );
        };
        let expected_document = expected_binding.renderer_document_identity();
        if !self
            .renderer_document_binding_is_current_for_session_owner(session_id, expected_binding)
        {
            return moli_core::browser_host::BrowserDocumentLifecycleWaitTicket::resolved(
                self.target_page_residence_identity_for_session(session_id),
                Some(expected_document),
                milestone,
                moli_core::browser_host::BrowserDocumentLifecycleWaitOutcome::Superseded,
            );
        }
        let Some(expected_page) = self.target_page_residence_identity_for_session(session_id)
        else {
            return moli_core::browser_host::BrowserDocumentLifecycleWaitTicket::resolved(
                None,
                Some(expected_document),
                milestone,
                moli_core::browser_host::BrowserDocumentLifecycleWaitOutcome::Unavailable(
                    moli_core::browser_host::BrowserDocumentLifecycleWaitUnavailableReason::NoCurrentPage,
                ),
            );
        };
        self.browser_host_state
            .navigation_owner()
            .capture_document_lifecycle_wait(expected_page, expected_document, milestone)
    }

    pub(crate) fn renderer_document_lifecycle_visible_state_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<(
        CommittedRendererDocumentBinding,
        moli_core::page::RendererDocumentLifecycleSnapshot,
    )> {
        let binding = self
            .committed_renderer_document_binding_for_session_owner(session_id)?
            .clone();
        let page_slot = self
            .runtime_session_owner_slot(session_id)
            .ok()?
            .page_slot();
        Some((
            binding,
            page_slot.renderer_document_lifecycle_visible_snapshot()?,
        ))
    }

    pub(crate) fn arm_root_post_load_observation_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        loader_id: &str,
    ) -> bool {
        if self
            .committed_renderer_document_binding_for_session_owner(session_id)
            .is_none()
        {
            return false;
        }
        self.runtime_session_owner_slot_mut(session_id)
            .is_ok_and(|slot| {
                slot.page_slot_mut()
                    .arm_root_post_load_observation(loader_id)
            })
    }

    /// Consumes the exact stopped-loading fact owned by an armed root post-load
    /// observation and, when `Page` has subscribers, publishes its frozen
    /// protocol output.
    ///
    /// Having no `Page` subscriber is a normal terminal outcome. The binding
    /// must still be consumed so enabling `Page` later cannot replay a
    /// historical event from an earlier navigation.
    pub(crate) fn settle_root_frame_stopped_loading_observation(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<
        crate::domains::activity::RootFrameStoppedLoadingSettlement,
        crate::domains::activity::RootFrameStoppedLoadingSettlementError,
    > {
        use crate::domains::activity::{
            RootFrameStoppedLoadingSettlement as Settlement,
            RootFrameStoppedLoadingSettlementError as SettlementError,
        };

        if self
            .committed_renderer_document_binding_for_session_owner(session_id)
            .is_none()
        {
            return Err(SettlementError::MissingArmedObservation);
        }
        let binding = self
            .runtime_session_owner_slot_mut(session_id)
            .ok()
            .and_then(|slot| {
                slot.page_slot_mut()
                    .take_root_frame_stopped_loading_binding()
            });
        let Some(binding) = binding else {
            return Err(SettlementError::MissingArmedObservation);
        };
        if self
            .subscribed_page_event_session_ids_for_session_owner(session_id)
            .is_empty()
        {
            return Ok(Settlement::Unobserved);
        }
        let attachments = self
            .page_event_protocol_attachments_for_session_owner(session_id)
            .ok_or(SettlementError::SubscribedAttachmentUnavailable)?;
        let publish_sequence = self
            .scheduler_state
            .allocate_protocol_work_publish_sequence();
        let output = crate::domains::activity::ProtocolOutputWork::root_frame_stopped_loading(
            attachments,
            binding.frame_id,
            binding.loader_id,
        );
        let work = crate::domains::activity::ProtocolSchedulerWork::protocol_observation(
            publish_sequence,
            output,
        );
        self.scheduler_state
            .push_scheduler_event(CdpSchedulerEvent::ProtocolWorkPublished { work });
        Ok(Settlement::Published)
    }

    pub(crate) fn emit_root_network_idle_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        out: &mut Vec<BackgroundProtocolEvent>,
    ) -> bool {
        if !self
            .runtime_session_owner_slot(session_id)
            .is_ok_and(|slot| slot.renderer_subresources_are_idle())
        {
            return false;
        }
        if self
            .committed_renderer_document_binding_for_session_owner(session_id)
            .is_none()
        {
            return false;
        }
        let has_pending_document_navigation =
            self.has_pending_document_navigation_for_session_owner(session_id);
        let binding = self
            .runtime_session_owner_slot_mut(session_id)
            .ok()
            .and_then(|slot| {
                slot.page_slot_mut()
                    .take_root_network_idle_binding(has_pending_document_navigation)
            });
        let Some(binding) = binding else {
            return false;
        };
        let timestamp = monotonic_timestamp_seconds();
        for event_session_id in self.page_event_session_ids_for_session_owner(session_id) {
            let lifecycle_enabled = self
                .target_page_session_state_for_session(event_session_id.as_deref())
                .is_some_and(|state| state.page_lifecycle_events);
            crate::domains::page::emit_navigation_network_idle_background_events(
                out,
                event_session_id.as_deref(),
                lifecycle_enabled,
                &binding.frame_id,
                &binding.loader_id,
                timestamp,
            );
        }
        true
    }

    pub fn devtools_context_routes_to_top_level_target(
        &self,
        context: &DevToolsCommandContext,
    ) -> bool {
        context.target_id.as_ref().is_some_and(|target_id| {
            self.target_session_route_for_target_id(target_id.as_str())
                .is_some()
        })
    }

    pub(crate) fn command_owner_scope_for_devtools_context(
        &self,
        context: &DevToolsCommandContext,
    ) -> Option<CommandOwnerScope> {
        if let Some(target_id) = context.target_id.as_ref() {
            let route = self
                .target_session_route_for_target_id(target_id.as_str())
                .or_else(|| self.target_session_route_for_child_frame_id(target_id.as_str()))?;
            return Some(CommandOwnerScope::from_session_and_owner_route(
                None,
                Some(route),
            ));
        }
        Some(CommandOwnerScope::from_session_and_owner_route(
            context
                .session_id
                .as_ref()
                .map(|session_id| session_id.as_str()),
            None,
        ))
    }

    /// Captures the exact Browser Core-owned Page currently addressed by a
    /// protocol-neutral command context.
    pub fn page_residence_identity_for_devtools_context(
        &mut self,
        context: &DevToolsCommandContext,
    ) -> Option<DevToolsPageResidenceIdentity> {
        let owner_scope = self.command_owner_scope_for_devtools_context(context)?;
        let session_id = owner_scope.session_id().map(str::to_owned);
        let mut route_scope = owner_scope.enter(self);
        route_scope
            .conn_mut()
            .target_page_residence_identity_for_session(session_id.as_deref())
    }

    fn capture_document_lifecycle_wait_for_session_owner(
        &self,
        session_id: Option<&str>,
        expected_loader_id: &str,
        milestone: moli_core::page::RendererDocumentLifecycleMilestone,
    ) -> Option<DevToolsDocumentLifecycleWaitKey> {
        let binding = self
            .committed_renderer_document_binding_for_session_owner(session_id)?
            .clone();
        if binding.loader_id != expected_loader_id {
            return None;
        }
        let lifecycle_wait = self.exact_renderer_document_lifecycle_fact_wait_for_session_owner(
            session_id,
            Some(&binding),
            milestone,
        );
        Some(DevToolsDocumentLifecycleWaitKey {
            lifecycle_wait,
            renderer_document: binding.renderer_document,
            renderer_epoch: binding.renderer_epoch,
            milestone,
            frame_id: binding.frame_id,
            loader_id: binding.loader_id,
        })
    }

    /// Resolves frontend routing once, then captures an exact Browser-owned
    /// lifecycle fact wait. Dropping the returned key releases its fact
    /// subscribers; no callback is registered in the physical Page slot.
    pub fn capture_devtools_document_lifecycle_wait_key(
        &mut self,
        context: &DevToolsCommandContext,
        expected_loader_id: &str,
        milestone: moli_core::page::RendererDocumentLifecycleMilestone,
    ) -> Option<DevToolsDocumentLifecycleWaitKey> {
        if let Some(target_id) = context.target_id.as_ref() {
            let route = self.target_session_route_for_target_id(target_id.as_str())?;
            let mut route_scope = self.scoped_none_session_owner_route_override(route);
            return route_scope
                .conn_mut()
                .capture_document_lifecycle_wait_for_session_owner(
                    None,
                    expected_loader_id,
                    milestone,
                );
        }
        self.capture_document_lifecycle_wait_for_session_owner(
            context
                .session_id
                .as_ref()
                .map(|session_id| session_id.as_str()),
            expected_loader_id,
            milestone,
        )
    }

    pub fn take_scheduler_events(&mut self) -> Vec<CdpSchedulerEvent> {
        self.scheduler_state.take_scheduler_events()
    }

    pub(crate) fn push_scheduler_event(&mut self, event: CdpSchedulerEvent) {
        self.scheduler_state.push_scheduler_event(event);
    }

    pub fn begin_command_response_flush_permit(
        &mut self,
    ) -> (CommandResponseFlushPermit, CommandResponseFlushContext) {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let deferred_releases: Arc<Mutex<CommandResponseFlushDeferredReleases>> = Arc::default();
        (
            CommandResponseFlushPermit {
                sender,
                deferred_releases: deferred_releases.clone(),
            },
            CommandResponseFlushContext::new(receiver, deferred_releases),
        )
    }

    pub(crate) fn extend_scheduler_events(&mut self, events: Vec<CdpSchedulerEvent>) {
        self.scheduler_state.extend_scheduler_events(events);
    }

    pub(crate) fn record_scheduler_activity_trace(&mut self, event: serde_json::Value) {
        self.scheduler_state.push_activity_trace(event);
    }

    pub(crate) fn scheduler_activity_trace_enabled(&self) -> bool {
        moli_trace::cdp_nav_timing_enabled()
    }

    pub(crate) fn runtime_await_trace_enabled(&self) -> bool {
        moli_trace::cdp_runtime_trace_enabled() || self.scheduler_activity_trace_enabled()
    }

    pub(crate) fn record_runtime_await_trace(
        &mut self,
        event: &'static str,
        command_id: Option<u64>,
        session_id: Option<&str>,
        fields: serde_json::Value,
    ) {
        if !self.runtime_await_trace_enabled() {
            return;
        }
        self.record_scheduler_activity_trace(json!({
            "kind": event,
            "commandId": command_id,
            "sessionId": session_id,
            "fields": fields,
            "pendingRuntimeAwaitJobCount": self.pending_runtime_await_jobs.len(),
        }));
    }

    pub(crate) fn background_navigation_completion_sender_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<
        tokio::sync::mpsc::UnboundedSender<crate::domains::page::BackgroundNavigationCompletion>,
    > {
        if !self.can_run_background_navigation_for_session_owner(session_id) {
            return None;
        }
        self.scheduler_hooks
            .background_navigation_completion_sender()
    }

    fn can_run_background_navigation_for_session_owner(&self, session_id: Option<&str>) -> bool {
        if !self
            .scheduler_hooks
            .has_background_navigation_completion_sender()
        {
            return false;
        }
        self.target_owner_identity_for_session(session_id)
            .is_some_and(|(_, target_id)| target_id.is_some())
    }

    fn can_run_background_navigation_for_active_session(&self) -> bool {
        if !self
            .scheduler_hooks
            .has_background_navigation_completion_sender()
            || !self.inactive_browser_contexts.is_empty()
        {
            return false;
        }
        self.browser_context
            .as_ref()
            .is_some_and(|browser_context| browser_context.background_targets.is_empty())
    }

    pub(crate) fn can_defer_initial_document_page_build(&self) -> bool {
        self.can_run_background_navigation_for_active_session()
    }

    pub async fn drain_background_navigation_completion_turn_async(
        &mut self,
        completion: crate::domains::page::BackgroundNavigationCompletion,
    ) -> (
        CdpRendererOwnerTurnOutcome,
        crate::domains::page::BackgroundNavigationTurnDisposition,
    ) {
        let gate_key = completion.background_navigation_gate_key();
        let (mut command_context, protocol_events, terminal) = self
            .drain_background_navigation_completion_events_with_context(completion, gate_key)
            .await;
        command_context
            .protocol_events_mut()
            .extend(protocol_events);
        let (protocol_events, renderer_output_boundary, post_renderer_output_events) =
            command_context.take_renderer_fenced_protocol_events();
        (
            CdpTurnOutcome::new_with_protocol_and_post_response_events(
                protocol_events,
                command_context.take_post_response_events(),
                self.take_scheduler_events(),
            )
            .with_renderer_output_boundary(renderer_output_boundary, post_renderer_output_events)
            .with_renderer_output_predecessor(command_context.take_renderer_output_predecessor()),
            if terminal {
                crate::domains::page::BackgroundNavigationTurnDisposition::Terminal
            } else {
                crate::domains::page::BackgroundNavigationTurnDisposition::ParticipantPending
            },
        )
    }

    async fn drain_background_navigation_completion_events_with_context(
        &mut self,
        completion: crate::domains::page::BackgroundNavigationCompletion,
        gate_key: Option<BackgroundNavigationGateKey>,
    ) -> (CommandDispatchContext, Vec<BackgroundProtocolEvent>, bool) {
        let (
            mut completed,
            mut command_context,
            command_id,
            command_session_id,
            none_session_owner_route,
            requested_url,
            gate_key,
        ) = match completion {
            crate::domains::page::BackgroundNavigationCompletion::Lifecycle(completion) => {
                let none_session_owner_route = completion.none_session_owner_route();
                let requested_url = completion.requested_url().to_owned();
                let timing_started =
                    moli_trace::cdp_nav_timing_enabled().then(std::time::Instant::now);
                if timing_started.is_some() {
                    tracing::info!(
                        target: "moli_cdp_nav_timing",
                        url = %requested_url,
                        stage = "background_completion_enqueue_start",
                        ready_to_enqueue_ms = completion.ready_elapsed_ms(),
                    );
                }
                // Always materialize stale completions so an outstanding
                // Page.navigate receives its terminal abort response. Only
                // retention of the detached engine is conditional.
                let is_current = completion.is_current_for_connection(self);
                let completion = completion.materialize_with_engine_retention(self, is_current);
                if let Some(started) = timing_started {
                    tracing::info!(
                        target: "moli_cdp_nav_timing",
                        stage = "background_completion_materialized",
                        phase_ms = started.elapsed().as_millis(),
                    );
                }
                let command_id = completion.navigate_id();
                let command_session_id = completion.navigate_session_id().map(str::to_owned);
                (
                    crate::domains::page::CompletedNavigateCommand::materialized(completion),
                    CommandDispatchContext::default(),
                    command_id,
                    command_session_id,
                    none_session_owner_route,
                    requested_url,
                    gate_key,
                )
            }
            crate::domains::page::BackgroundNavigationCompletion::Participant(completion) => {
                completion.into_parts()
            }
            crate::domains::page::BackgroundNavigationCompletion::MainDocumentBody(completion) => {
                let mut command_context = CommandDispatchContext::default();
                let previous_none_session_owner_route =
                    completion.navigate_session_id().is_none().then(|| {
                        self.replace_none_session_owner_route_override(
                            completion.none_session_owner_route(),
                        )
                    });
                completion.record_if_current(self);
                if let Some(previous) = previous_none_session_owner_route {
                    self.replace_none_session_owner_route_override(previous);
                }
                let protocol_events = command_context.take_protocol_events();
                return (command_context, protocol_events, true);
            }
        };

        loop {
            let previous_none_session_owner_route = command_session_id.is_none().then(|| {
                self.replace_none_session_owner_route_override(none_session_owner_route.clone())
            });
            let step = crate::domains::page::complete_pending_navigate_command(
                self,
                completed,
                &mut command_context,
            )
            .await;
            if let Some(previous) = previous_none_session_owner_route {
                self.replace_none_session_owner_route_override(previous);
            }
            match step {
                crate::domains::page::NavigateCommandCompletion::Complete(plan) => {
                    let protocol_events =
                        Self::project_navigation_completion_plan_background_events(
                            plan,
                            command_id,
                            command_session_id.as_deref(),
                            &mut command_context,
                        );
                    return (command_context, protocol_events, true);
                }
                crate::domains::page::NavigateCommandCompletion::Pending(pending) => {
                    let Some(sender) = self
                        .scheduler_hooks
                        .background_navigation_completion_sender()
                        .filter(|sender| !sender.is_closed())
                    else {
                        // This fallback is reachable only in direct tests or
                        // during frontend teardown. It preserves completion
                        // correctness without inventing a timer/retry policy.
                        completed = pending.wait().await;
                        continue;
                    };
                    tokio::task::spawn_local(async move {
                        let completed = pending.wait().await;
                        let completion =
                            crate::domains::page::BackgroundNavigationParticipantCompletion::new(
                                completed,
                                command_context,
                                command_id,
                                command_session_id,
                                none_session_owner_route,
                                requested_url,
                                gate_key,
                            );
                        let _ = sender.send(
                            crate::domains::page::BackgroundNavigationCompletion::participant(
                                completion,
                            ),
                        );
                    });
                    return (CommandDispatchContext::default(), Vec::new(), false);
                }
            }
        }
    }

    pub(crate) fn enqueue_deferred_main_document_load_completion(
        &mut self,
        admission: crate::domains::activity::DeferredMainDocumentLoadCompletionAdmission,
    ) {
        if !admission.is_still_current_for_scheduler(self) {
            tracing::debug!(
                session_id = admission.session_id(),
                "dropping obsolete deferred main-document load completion before enqueue"
            );
            return;
        }
        let owner_scope = admission.owner_scope().clone();
        let observation_id = self
            .scheduler_state
            .allocate_deferred_main_document_load_observation_id();
        let completion = {
            let mut route_scope = owner_scope.enter(self);
            admission.bind_fact_wait_ticket(route_scope.conn_mut(), observation_id)
        };
        let publish_sequence = self
            .scheduler_state
            .allocate_protocol_work_publish_sequence();
        let work =
            crate::domains::activity::ProtocolSchedulerWork::main_document_load_fact_projection(
                publish_sequence,
                completion,
            );
        self.scheduler_state
            .push_scheduler_event(CdpSchedulerEvent::ProtocolWorkPublished { work });
    }

    pub async fn complete_deferred_main_document_load_completion_for_scheduler(
        &mut self,
        completion: CompletedDeferredMainDocumentLoadCompletion,
    ) -> CdpTurnOutcome {
        let owner_scope = completion.inner.owner_scope().clone();
        let mut output = BackgroundProtocolEventBuffer::default();
        {
            let mut route_scope = owner_scope.enter(self);
            completion
                .inner
                .emit_async(route_scope.conn_mut(), &mut output)
                .await;
        }
        CdpTurnOutcome::new_with_protocol_and_post_response_events(
            output.into_events(),
            Vec::new(),
            self.take_scheduler_events(),
        )
    }

    pub(crate) fn enqueue_navigation_background_event(&mut self, event: NavigationBackgroundEvent) {
        self.scheduler_state.push_navigation_background_event(event);
    }

    pub(crate) fn enqueue_navigation_background_protocol_event(
        &mut self,
        token: DocumentNavigationToken,
        event: BackgroundProtocolEvent,
    ) {
        self.enqueue_navigation_background_event(NavigationBackgroundEvent::background_event(
            token, event,
        ));
    }

    pub(crate) fn send_navigation_background_protocol_event(
        &mut self,
        token: DocumentNavigationToken,
        event: BackgroundProtocolEvent,
    ) {
        self.enqueue_navigation_background_protocol_event(token, event);
        self.flush_navigation_background_events_to_sender();
    }

    fn drain_navigation_background_protocol_events(&mut self) -> Vec<BackgroundProtocolEvent> {
        let events = self.scheduler_state.take_navigation_background_events();
        events
            .into_iter()
            .filter_map(|event| {
                let is_current = self.accepts_pending_document_navigation_token(event.token());
                event.into_background_protocol_event_if_current(is_current)
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn drain_navigation_background_events(&mut self) -> Vec<serde_json::Value> {
        self.drain_navigation_background_protocol_events()
            .into_iter()
            .map(BackgroundProtocolEvent::into_protocol_message)
            .collect()
    }

    pub(crate) fn flush_navigation_background_events_to_sender(&mut self) {
        let Some(sender) = self.scheduler_hooks.background_event_sender() else {
            return;
        };
        for event in self.drain_navigation_background_protocol_events() {
            let _ = sender.send(event);
        }
    }

    #[cfg(test)]
    pub(crate) async fn drain_materialized_navigation_completion_background_events(
        &mut self,
        completion: crate::domains::page::MaterializedNavigationCompletion,
        command_context: &mut CommandDispatchContext,
    ) -> Vec<BackgroundProtocolEvent> {
        let command_id = completion.navigate_id();
        let command_session_id = completion.navigate_session_id().map(str::to_owned);
        let mut output = CommandOutputBuffer::default();
        self.drain_materialized_navigation_completion_into_buffer(
            &mut output,
            completion,
            command_context,
        )
        .await;
        Self::project_navigation_completion_plan_background_events(
            output.into_plan(),
            command_id,
            command_session_id.as_deref(),
            command_context,
        )
    }

    fn project_navigation_completion_plan_background_events(
        plan: crate::domains::command_output::CommandOutputPlan,
        command_id: Option<u64>,
        command_session_id: Option<&str>,
        command_context: &mut CommandDispatchContext,
    ) -> Vec<BackgroundProtocolEvent> {
        let (
            before_renderer_output,
            renderer_output_boundary,
            after_renderer_output,
            post_response_events,
        ) = plan.into_renderer_fenced_background_and_post_response_events(
            command_id,
            command_session_id,
        );
        command_context.append_renderer_fenced_protocol_events(
            before_renderer_output,
            renderer_output_boundary,
            after_renderer_output,
        );
        command_context.extend_post_response_events(post_response_events);
        Vec::new()
    }

    #[cfg(test)]
    pub(crate) async fn drain_materialized_navigation_completion_into(
        &mut self,
        out: &mut Vec<serde_json::Value>,
        completion: crate::domains::page::MaterializedNavigationCompletion,
        command_context: &mut CommandDispatchContext,
    ) {
        let mut events = self
            .drain_materialized_navigation_completion_background_events(completion, command_context)
            .await;
        let (before_renderer_output, renderer_output_boundary, after_renderer_output) =
            command_context.take_renderer_fenced_protocol_events();
        assert!(
            renderer_output_boundary.is_none(),
            "message-only navigation helper cannot flatten a renderer output boundary"
        );
        events.extend(before_renderer_output);
        events.extend(after_renderer_output);
        events.extend(command_context.take_post_response_events());
        out.extend(
            events
                .into_iter()
                .map(BackgroundProtocolEvent::into_protocol_message),
        );
    }

    #[cfg(test)]
    pub(crate) async fn drain_materialized_navigation_completion_into_buffer(
        &mut self,
        out: &mut CommandOutputBuffer,
        completion: crate::domains::page::MaterializedNavigationCompletion,
        command_context: &mut CommandDispatchContext,
    ) {
        let timing_started = moli_trace::cdp_nav_timing_enabled().then(std::time::Instant::now);
        if timing_started.is_some() {
            tracing::info!(
                target: "moli_cdp_nav_timing",
                url = %completion.requested_url(),
                stage = "materialized_completion_drain_start",
            );
        }
        let is_current = completion.is_current_for_connection(self);
        let (token, state, navigation, engine) = completion.into_parts();
        if !is_current {
            if state.navigate_id.is_some() {
                out.push_error_after_messages(-32000, "Navigation aborted");
            }
            return;
        }
        let committed_owner =
            crate::domains::page::complete_materialized_navigation_into_buffer_async(
                self,
                out,
                token.clone(),
                state,
                navigation,
                command_context,
            )
            .await;
        self.adopt_materialized_navigation_engine(engine, committed_owner);
        if let Some(started) = timing_started {
            tracing::info!(
                target: "moli_cdp_nav_timing",
                stage = "materialized_completion_drain_end",
                phase_ms = started.elapsed().as_millis(),
            );
        }
    }

    pub(crate) fn adopt_materialized_navigation_engine(
        &mut self,
        engine: Option<NavigationEngine>,
        committed_owner: Option<moli_core::browser_host::BrowserPageOwnerKey>,
    ) {
        match (engine, committed_owner) {
            (Some(engine), Some(owner)) => {
                if let Err(error) =
                    self.adopt_loaded_navigation_engine_for_target_owner(owner, engine)
                {
                    tracing::warn!(
                        %error,
                        "materialized navigation engine adoption rejected by Browser Owner"
                    );
                }
            }
            (Some(_engine), None) => {
                tracing::warn!("materialized navigation engine has no committed Browser owner");
            }
            (None, _) => {}
        }
    }

    pub(crate) fn none_session_owner_route_override(&self) -> Option<CdpSessionRoute> {
        self.none_session_owner_route_override.clone()
    }

    pub(crate) fn replace_none_session_owner_route_override(
        &mut self,
        route: Option<CdpSessionRoute>,
    ) -> Option<CdpSessionRoute> {
        std::mem::replace(&mut self.none_session_owner_route_override, route)
    }

    pub(crate) fn scoped_none_session_owner_route_override(
        &mut self,
        route: CdpSessionRoute,
    ) -> NoneSessionOwnerRouteOverrideScope<'_> {
        NoneSessionOwnerRouteOverrideScope::enter(self, Some(route))
    }

    pub(crate) fn scoped_optional_none_session_owner_route_override(
        &mut self,
        route: Option<CdpSessionRoute>,
    ) -> NoneSessionOwnerRouteOverrideScope<'_> {
        NoneSessionOwnerRouteOverrideScope::enter(self, route)
    }

    pub(crate) fn response_body_materialize_limit(&self) -> usize {
        self.browser_host_state
            .navigation_owner()
            .active_fetch_config()
            .http_max_response_size()
            .unwrap_or(body_spool::DEFAULT_BODY_MATERIALIZE_LIMIT)
    }

    pub(crate) fn moli_memory_diagnostics(&self) -> serde_json::Value {
        let active_browser_context = self
            .browser_context
            .as_ref()
            .map(BrowserContext::moli_memory_diagnostics);
        let inactive_browser_contexts = self
            .inactive_browser_contexts
            .iter()
            .map(BrowserContext::moli_memory_diagnostics)
            .collect::<Vec<_>>();
        let retained_engine_keys = self
            .browser_host_state
            .navigation_owner()
            .retained_background_engine_keys()
            .map(|key| {
                json!({
                    "browserContextId": key.browser_context_id(),
                    "targetId": key.target_id(),
                })
            })
            .collect::<Vec<_>>();
        let mut browser_owner_initial_empty_documents = self
            .browser_host_state
            .navigation_owner()
            .initial_empty_documents()
            .map(|state| {
                let creator = state.creator().map(|creator| {
                    json!({
                        "targetId": creator.target_id(),
                        "securityOrigin": creator.security_origin(),
                        "secureContextType": creator.secure_context_type(),
                    })
                });
                json!({
                    "targetId": state.target_id(),
                    "loaderId": state.loader_id(),
                    "initialUrl": state.initial_url(),
                    "creator": creator,
                    "materialized": state.materialized(),
                    "exited": state.exited(),
                    "pendingCrossDocumentNavigation": state.pending_cross_document_navigation(),
                    "isOnInitialEmptyDocument": state.is_on_initial_empty_document(),
                })
            })
            .collect::<Vec<_>>();
        browser_owner_initial_empty_documents
            .sort_by(|left, right| left["targetId"].as_str().cmp(&right["targetId"].as_str()));
        let browser_context_count = self.browser_contexts().count();
        let loaded_document_page_count = self
            .browser_contexts()
            .map(BrowserContext::loaded_document_page_count)
            .sum::<usize>();
        let pending_document_page_build_count = self
            .browser_contexts()
            .map(BrowserContext::pending_document_page_build_count)
            .sum::<usize>();
        let mut loaded_document_renderer_owner_ids = HashSet::new();
        let mut document_renderer_owner_ids = HashSet::new();
        for browser_context in self.browser_contexts() {
            loaded_document_renderer_owner_ids
                .extend(browser_context.loaded_document_renderer_owner_ids_for_diagnostics());
            document_renderer_owner_ids
                .extend(browser_context.document_renderer_owner_ids_for_diagnostics());
        }
        let loaded_document_renderer_owner_count = loaded_document_renderer_owner_ids.len();
        let shared_worker_target_count = self
            .browser_contexts()
            .map(|context| context.shared_worker_targets.len())
            .sum::<usize>();
        let service_worker_target_count = self
            .browser_contexts()
            .map(|context| context.service_worker_targets.len())
            .sum::<usize>();
        let page_target_pending_inspector_await_count = self
            .browser_contexts()
            .map(BrowserContext::page_target_pending_inspector_await_count_for_diagnostics)
            .sum::<usize>();
        let page_target_with_pending_inspector_await_count = self
            .browser_contexts()
            .map(BrowserContext::page_target_with_pending_inspector_await_count_for_diagnostics)
            .sum::<usize>();
        let shared_worker_target_pending_inspector_await_count = self
            .browser_contexts()
            .map(BrowserContext::shared_worker_target_pending_inspector_await_count_for_diagnostics)
            .sum::<usize>();
        let shared_worker_target_with_pending_inspector_await_count = self
            .browser_contexts()
            .map(
                BrowserContext::shared_worker_target_with_pending_inspector_await_count_for_diagnostics,
            )
            .sum::<usize>();
        let service_worker_target_pending_inspector_await_count = self
            .browser_contexts()
            .map(
                BrowserContext::service_worker_target_pending_inspector_await_count_for_diagnostics,
            )
            .sum::<usize>();
        let service_worker_target_with_pending_inspector_await_count = self
            .browser_contexts()
            .map(
                BrowserContext::service_worker_target_with_pending_inspector_await_count_for_diagnostics,
            )
            .sum::<usize>();
        let pending_inspector_await_count = page_target_pending_inspector_await_count
            + shared_worker_target_pending_inspector_await_count
            + service_worker_target_pending_inspector_await_count;
        let dedicated_worker_running_worker_isolate_count = self
            .browser_contexts()
            .map(BrowserContext::dedicated_worker_running_worker_isolate_count_for_diagnostics)
            .sum::<usize>();
        let mut shared_worker_matching_entry_count = 0;
        let mut shared_worker_loading_instance_count = 0;
        let mut shared_worker_running_instance_count = 0;
        let mut shared_worker_client_count = 0;
        let mut shared_worker_loading_host_count = 0;
        let mut shared_worker_running_worker_isolate_count = 0;
        let mut shared_worker_pending_service_lane_event_count = 0;
        for shared_worker_diagnostics in self
            .browser_contexts()
            .map(BrowserContext::shared_worker_runtime_diagnostics_for_diagnostics)
        {
            shared_worker_matching_entry_count += shared_worker_diagnostics.matching_entry_count;
            shared_worker_loading_instance_count +=
                shared_worker_diagnostics.loading_instance_count;
            shared_worker_running_instance_count +=
                shared_worker_diagnostics.running_instance_count;
            shared_worker_client_count += shared_worker_diagnostics.client_count;
            shared_worker_loading_host_count += shared_worker_diagnostics.loading_host_count;
            shared_worker_running_worker_isolate_count +=
                shared_worker_diagnostics.running_worker_isolate_count;
            shared_worker_pending_service_lane_event_count +=
                shared_worker_diagnostics.pending_service_lane_event_count;
        }
        let retained_background_navigation_engine_count = self
            .browser_host_state
            .navigation_owner()
            .retained_background_engine_count();
        let active_renderer_owner_id = self
            .browser_host_state
            .navigation_owner()
            .active_renderer_owner_id_for_diagnostics();
        let mut retained_background_navigation_engine_renderer_owner_ids = HashSet::new();
        let mut estimated_renderer_owner_ids = HashSet::new();
        estimated_renderer_owner_ids.insert(active_renderer_owner_id);
        estimated_renderer_owner_ids.extend(document_renderer_owner_ids.iter().copied());
        for renderer_owner_id in self
            .browser_host_state
            .navigation_owner()
            .retained_renderer_owner_ids_for_diagnostics()
        {
            if renderer_owner_id != active_renderer_owner_id {
                retained_background_navigation_engine_renderer_owner_ids.insert(renderer_owner_id);
            }
            estimated_renderer_owner_ids.insert(renderer_owner_id);
        }
        let retained_background_navigation_engine_renderer_owner_count =
            retained_background_navigation_engine_renderer_owner_ids.len();
        let estimated_renderer_owner_count = estimated_renderer_owner_ids.len();
        let document_isolate_model = self
            .browser_host_state
            .navigation_owner()
            .active_document_isolate_model_for_diagnostics();
        let estimated_document_isolate_count =
            loaded_document_page_count + pending_document_page_build_count;
        let document_isolate_accounting = self
            .browser_host_state
            .navigation_owner()
            .active_document_isolate_accounting_for_diagnostics();
        let document_isolate_accounting = json!({
            "scope": "renderer-process",
            "created": document_isolate_accounting.created,
            "destroyed": document_isolate_accounting.destroyed,
            "live": document_isolate_accounting.live,
            "reserved": document_isolate_accounting.reserved,
        });
        let estimated_worker_isolate_count = dedicated_worker_running_worker_isolate_count
            + shared_worker_running_worker_isolate_count;
        let estimated_live_v8_isolate_count =
            estimated_document_isolate_count + estimated_worker_isolate_count;
        let active_navigation_engine_resource_runtime = self
            .browser_host_state
            .navigation_owner()
            .active_resource_request_client()
            .map(|client| client.resource_runtime_diagnostics());
        let active_navigation_engine_resource_runtime_id =
            active_navigation_engine_resource_runtime
                .as_ref()
                .map(|diagnostics| diagnostics.runtime_id);
        let active_navigation_engine_memory_cache =
            active_navigation_engine_resource_runtime.map(|diagnostics| diagnostics.memory_cache);
        let browser_host_policy = self.browser_host_policy_snapshot();
        json!({
            "connection": {
                "hasActiveBrowserContext": self.browser_context.is_some(),
                "inactiveBrowserContextCount": self.inactive_browser_contexts.len(),
                "browserSessionIdCount": self.browser_session_ids.len(),
                "globalIoStreamCount": self.global_io_streams.len(),
                "tracing": self.tracing_state.diagnostics(),
                "permissionOverrideCount": browser_host_policy.permission_overrides().len(),
                "retainedBackgroundNavigationEngineCount": retained_background_navigation_engine_count,
                "retainedBackgroundNavigationEngineKeys": retained_engine_keys,
                "browserOwnerInitialEmptyDocuments": browser_owner_initial_empty_documents,
                "autoAttach": self.auto_attach,
                "targetDiscoveryEnabled": self.target_discovery_enabled,
                "targetInfoChangeEventsEnabled": self.target_info_change_events_enabled,
                "activeNavigationEngine": {
                    "imageFetchEnabled": self.browser_host_state.navigation_owner().active_image_fetch_enabled(),
                    "optionalResourceFetchMask": self.browser_host_state.navigation_owner().active_optional_resource_fetch_mask().bits(),
                    "subframeLoadingEnabled": self.browser_host_state.navigation_owner().active_subframe_loading_enabled(),
                    "resourceRuntimeId": active_navigation_engine_resource_runtime_id,
                    "networkMemoryCache": active_navigation_engine_memory_cache,
                    "browserContextRuntime": self.browser_host_state.navigation_owner()
                        .active_browser_context_runtime()
                        .moli_memory_diagnostics(),
                },
            },
            "isolateScope": {
                "documentIsolateModel": document_isolate_model,
                "workerIsolateModel": "per-worker-thread",
                "activeNavigationEngineRendererOwnerCount": 1,
                "retainedBackgroundNavigationEngineRendererOwnerCount": retained_background_navigation_engine_renderer_owner_count,
                "estimatedRendererOwnerCount": estimated_renderer_owner_count,
                "browserContextCount": browser_context_count,
                "loadedDocumentPageCount": loaded_document_page_count,
                "loadedDocumentRendererOwnerCount": loaded_document_renderer_owner_count,
                "pendingDocumentPageBuildCount": pending_document_page_build_count,
                "estimatedDocumentIsolateCount": estimated_document_isolate_count,
                "documentIsolateAccounting": document_isolate_accounting,
                "estimatedWorkerIsolateCount": estimated_worker_isolate_count,
                "estimatedLiveV8IsolateCount": estimated_live_v8_isolate_count,
                "runtimeGetHeapUsageV8HeapScope": "page-vm-document-isolate",
                "runtimeGetHeapUsageV8HeapIsTargetLocal": true,
                "runtimeGetHeapUsageMoliCountersScope": "target-document",
                "runtimeCollectGarbageScope": "page-vm-document-isolate",
                "v8ForegroundTaskWakeScope": "page-vm-document-isolate",
                "v8ForegroundTaskWakeContextGroupIdAvailable": false,
                "v8ForegroundTaskWakeInternalPolicy": "page-runtime-queue-and-owner-page-tick",
                "v8ForegroundTaskWakeExternalPolicy": "page-owner-runtime-wake",
                "pendingInspectorAwaitCount": pending_inspector_await_count,
                "pageTargetPendingInspectorAwaitCount": page_target_pending_inspector_await_count,
                "pageTargetWithPendingInspectorAwaitCount": page_target_with_pending_inspector_await_count,
                "sharedWorkerTargetPendingInspectorAwaitCount": shared_worker_target_pending_inspector_await_count,
                "sharedWorkerTargetWithPendingInspectorAwaitCount": shared_worker_target_with_pending_inspector_await_count,
                "serviceWorkerTargetPendingInspectorAwaitCount": service_worker_target_pending_inspector_await_count,
                "serviceWorkerTargetWithPendingInspectorAwaitCount": service_worker_target_with_pending_inspector_await_count,
                "sharedWorkerTargetCount": shared_worker_target_count,
                "serviceWorkerTargetCount": service_worker_target_count,
                "sharedWorkerMatchingEntryCount": shared_worker_matching_entry_count,
                "sharedWorkerLoadingInstanceCount": shared_worker_loading_instance_count,
                "sharedWorkerRunningInstanceCount": shared_worker_running_instance_count,
                "sharedWorkerClientCount": shared_worker_client_count,
                "sharedWorkerLoadingHostCount": shared_worker_loading_host_count,
                "sharedWorkerRunningWorkerIsolateCount": shared_worker_running_worker_isolate_count,
                "sharedWorkerPendingServiceLaneEventCount": shared_worker_pending_service_lane_event_count,
                "sharedWorkerProtocolDispatchRequiresLiveOwnerPageCommand": false,
            },
            "scheduler": self.scheduler_state.moli_memory_diagnostics(),
            "activeBrowserContext": active_browser_context,
            "inactiveBrowserContexts": inactive_browser_contexts,
        })
    }

    fn idle_navigation_engine_release_counts(&self) -> (usize, usize, usize) {
        let loaded_browser_context_count = self
            .browser_contexts()
            .filter(|browser_context| {
                browser_context.has_loaded_page()
                    || browser_context
                        .background_targets
                        .iter()
                        .any(BackgroundTarget::has_loaded_page)
            })
            .count();

        let live_target_browser_context_count = self
            .browser_contexts()
            .filter(|browser_context| {
                browser_context.has_active_target()
                    || !browser_context.background_targets.is_empty()
            })
            .count();

        (
            loaded_browser_context_count,
            live_target_browser_context_count,
            self.browser_host_state
                .navigation_owner()
                .retained_background_engine_count(),
        )
    }

    pub(crate) fn release_idle_navigation_engine_memory_if_idle(
        &mut self,
    ) -> IdleNavigationEngineReleaseResult {
        let (
            loaded_browser_context_count,
            live_target_browser_context_count,
            retained_background_navigation_engine_count,
        ) = self.idle_navigation_engine_release_counts();
        let eligible = loaded_browser_context_count == 0
            && live_target_browser_context_count == 0
            && retained_background_navigation_engine_count == 0;
        if !eligible {
            return IdleNavigationEngineReleaseResult {
                reset: false,
                reason: "not-idle",
                loaded_browser_context_count,
                live_target_browser_context_count,
                retained_background_navigation_engine_count,
            };
        }

        let replacement = NavigationEngine::new_with_runtime_config(
            self.browser_host_state
                .navigation_owner()
                .active_runtime_config(),
        );
        if let Err(error) = self.adopt_navigation_engine_for_current_owner(replacement) {
            tracing::warn!(
                %error,
                "idle NavigationEngine replacement rejected by Browser Owner"
            );
            return IdleNavigationEngineReleaseResult {
                reset: false,
                reason: "engine-owner-diverged",
                loaded_browser_context_count,
                live_target_browser_context_count,
                retained_background_navigation_engine_count,
            };
        }
        IdleNavigationEngineReleaseResult {
            reset: true,
            reason: "idle-engine-replaced",
            loaded_browser_context_count,
            live_target_browser_context_count,
            retained_background_navigation_engine_count,
        }
    }

    pub(crate) fn release_idle_navigation_engine_memory_after_target_close(&mut self) {
        let result = self.release_idle_navigation_engine_memory_if_idle();
        if result.reset {
            tracing::debug!(
                target: "moli_cdp_memory",
                reason = result.reason,
                "released idle navigation engine after final target close"
            );
        }
    }

    pub(crate) fn moli_reset_idle_navigation_engine_for_diagnostics(
        &mut self,
    ) -> serde_json::Value {
        self.release_idle_navigation_engine_memory_if_idle()
            .to_protocol_json()
    }

    pub(crate) fn new_browser_context(&self, id: String) -> BrowserContext {
        let mut browser_context = self.initial_storage_partition.new_default_browser_context(
            id,
            self.fetch_config().http_cache_dir().map(PathBuf::from),
            self.fetch_config().http_cache_max_bytes(),
        );
        self.apply_global_browser_context_state(&mut browser_context);
        browser_context
    }

    fn apply_global_browser_context_state(&self, browser_context: &mut BrowserContext) {
        let policy = self.browser_host_network_policy_snapshot();
        browser_context
            .network_policy
            .set_cache_disabled(policy.global_cache_disabled());
        browser_context.global_extra_headers = policy.global_extra_headers().to_vec();
        browser_context.global_network_conditions = policy.global_network_conditions();
        browser_context.global_geolocation_override = policy.global_geolocation_override().cloned();
    }

    pub(crate) fn new_ephemeral_browser_context(&self, id: String) -> BrowserContext {
        let mut browser_context = BrowserContext::new_ephemeral_with_http_cache(
            id,
            self.fetch_config().http_cache_dir().map(PathBuf::from),
            self.fetch_config().http_cache_max_bytes(),
        );
        self.apply_global_browser_context_state(&mut browser_context);
        browser_context
    }

    pub fn snapshot_cookies(&mut self) -> Vec<StoredCookie> {
        self.browser_context
            .iter()
            .chain(self.inactive_browser_contexts.iter())
            .flat_map(BrowserContext::snapshot_cookies)
            .collect()
    }

    // ── ID generators ────────────────────────────────────────────────────────

    pub fn gen_bc_id(&mut self) -> String {
        format!(
            "BID-{}",
            self.browser_host_state.allocate_browser_context_sequence()
        )
    }

    pub fn gen_user_browser_context_id(&mut self) -> String {
        loop {
            let id = format!(
                "user-context-{}",
                self.browser_host_state.allocate_browser_context_sequence()
            );
            if !self.has_browser_context_id(&id) {
                return id;
            }
        }
    }

    pub fn default_browser_context_id(&self) -> &'static str {
        "BID-default"
    }

    pub fn default_target_id(&self) -> &'static str {
        DEFAULT_CDP_PAGE_TARGET_ID
    }

    pub fn default_tab_target_id(&self) -> &'static str {
        DEFAULT_CDP_TAB_TARGET_ID
    }

    pub(crate) fn register_top_level_page_target(&mut self, page_target_id: &str) -> String {
        let tab_target_id = if page_target_id == self.default_target_id() {
            self.default_tab_target_id().to_owned()
        } else {
            self.gen_tab_target_id()
        };
        self.target_control
            .register_tab(tab_target_id.clone(), page_target_id.to_owned());
        for target_id in [&tab_target_id, page_target_id] {
            if let Some(target_info) = self.target_info_for_host_delta(target_id) {
                self.notify_target_host_lifecycle(CdpTargetHostLifecycleDelta::Created(
                    target_info,
                ));
            }
        }
        tab_target_id
    }

    pub(crate) fn register_worker_target_host(
        &mut self,
        target_id: &str,
        kind: DevToolsTargetKind,
    ) {
        self.target_control
            .register_worker(target_id.to_owned(), kind);
    }

    pub(crate) fn remove_worker_target_host(&mut self, target_id: &str) -> bool {
        self.target_control.remove_worker(target_id)
    }

    #[doc(hidden)]
    pub fn tab_target_id_for_page_target_id(&self, page_target_id: &str) -> Option<&str> {
        self.target_control
            .tab_target_id_for_page_target_id(page_target_id)
    }

    pub(crate) fn primary_page_target_id_for_tab_target_id(
        &self,
        tab_target_id: &str,
    ) -> Option<&str> {
        self.target_control
            .primary_page_target_id_for_tab_target_id(tab_target_id)
    }

    pub(crate) fn primary_session_id_for_tab_target_id(&self, tab_target_id: &str) -> Option<&str> {
        self.target_control
            .primary_session_id_for_tab_target_id(tab_target_id)
    }

    pub(crate) fn assign_session_to_tab_target(
        &mut self,
        tab_target_id: &str,
        session_id: String,
        auxiliary: bool,
    ) -> bool {
        self.target_control
            .assign_session_to_tab_target(tab_target_id, session_id, auxiliary)
    }

    pub(crate) fn remove_tab_session(&mut self, session_id: &str) -> Option<String> {
        self.target_control.remove_tab_session(session_id)
    }

    pub(crate) fn remove_tab_for_page_target(
        &mut self,
        page_target_id: &str,
    ) -> Option<TargetClosurePlan> {
        let closure_plan = self
            .target_control
            .remove_tab_by_page_target_id(page_target_id)?;
        for target_id in closure_plan.destroyed_target_ids() {
            self.notify_target_host_lifecycle(CdpTargetHostLifecycleDelta::Destroyed {
                target_id: target_id.to_owned(),
            });
        }
        Some(closure_plan)
    }

    pub(crate) fn detach_closed_top_level_target_sessions_event_plan(
        &mut self,
        page_target_id: &str,
        reason: Option<&str>,
    ) -> TargetEventPlan {
        let Some(closure_plan) = self.remove_tab_for_page_target(page_target_id) else {
            return TargetEventPlan::default();
        };
        debug_assert!(
            closure_plan
                .destroyed_target_ids()
                .any(|target_id| target_id == page_target_id)
        );
        let target = closure_plan.tab_target();
        let tab_target_id = target.id().to_owned();
        let tab_session_ids = target.session_ids();
        self.detach_target_closure_cleanup_event_plan(
            TargetClosureCleanupPlan::new(tab_target_id, reason, tab_session_ids),
            None,
        )
    }

    pub(crate) fn rollback_top_level_target_tab_sessions_without_event(
        &mut self,
        page_target_id: &str,
    ) {
        let Some(closure_plan) = self.remove_tab_for_page_target(page_target_id) else {
            return;
        };
        for session_id in closure_plan.tab_target().session_ids() {
            self.clear_auto_attach_owner(Some(&session_id));
            self.rollback_attached_session_without_event(&session_id);
        }
    }

    pub(crate) fn tab_target_id_for_session_id(&self, session_id: &str) -> Option<&str> {
        self.target_control.tab_target_id_for_session_id(session_id)
    }

    pub(crate) fn browser_context_id_for_tab_target_id(
        &self,
        tab_target_id: &str,
    ) -> Option<String> {
        let page_target_id = self.primary_page_target_id_for_tab_target_id(tab_target_id)?;
        self.browser_contexts()
            .find(|browser_context| {
                browser_context
                    .devtools_target_info(page_target_id)
                    .is_some()
            })
            .map(|browser_context| browser_context.id.clone())
    }

    pub(crate) fn tab_target_info_for_page_target_info(
        &self,
        page_target_info: &DevToolsTargetInfo,
    ) -> Option<DevToolsTargetInfo> {
        if page_target_info.kind != DevToolsTargetKind::Page {
            return None;
        }
        self.target_control
            .tab_target_info_for_page_target_info(page_target_info.clone())
    }

    pub(crate) fn tab_target_info(&self, tab_target_id: &str) -> Option<DevToolsTargetInfo> {
        let page_target_id = self.primary_page_target_id_for_tab_target_id(tab_target_id)?;
        let page_target_info = self
            .browser_contexts()
            .find_map(|browser_context| browser_context.devtools_target_info(page_target_id))?;
        self.tab_target_info_for_page_target_info(&page_target_info)
    }

    pub(crate) fn set_target_discovery_for_owner(
        &mut self,
        owner_session_id: Option<&str>,
        filter: CdpTargetFilter,
    ) {
        let root_filter = owner_session_id
            .is_none()
            .then(|| filter.to_devtools_entries());
        self.target_control
            .set_discover_targets(owner_session_id, filter);
        if let Some(root_filter) = root_filter {
            self.target_discovery_enabled = true;
            self.target_info_change_events_enabled = true;
            self.target_discovery_filter = Some(root_filter);
        }
    }

    pub(crate) fn set_target_discovery_for_owner_from_devtools_filter(
        &mut self,
        owner_session_id: Option<&str>,
        filter: Option<Vec<DevToolsTargetFilterEntry>>,
    ) {
        let handler_filter = filter
            .clone()
            .map(CdpTargetFilter::from_devtools_entries)
            .unwrap_or_else(CdpTargetFilter::default_target_discovery);
        self.set_target_discovery_for_owner(owner_session_id, handler_filter);
        if owner_session_id.is_none() {
            self.target_discovery_filter = filter;
        }
    }

    pub(crate) fn clear_target_discovery_for_owner(&mut self, owner_session_id: Option<&str>) {
        self.target_control.clear_discover_targets(owner_session_id);
        if owner_session_id.is_none() {
            self.target_discovery_enabled = false;
            self.target_info_change_events_enabled = false;
            self.target_discovery_filter = None;
        }
    }

    pub fn root_target_discovery_enabled(&self) -> bool {
        self.target_discovery_enabled
    }

    pub fn replace_root_target_discovery_enabled(&mut self, enabled: bool) -> bool {
        let previous = self.target_discovery_enabled;
        if previous != enabled {
            self.set_root_target_discovery_enabled(enabled);
        }
        previous
    }

    pub fn set_root_target_discovery_enabled(&mut self, enabled: bool) {
        if enabled {
            self.set_target_discovery_for_owner(None, CdpTargetFilter::default_target_discovery());
        } else {
            self.clear_target_discovery_for_owner(None);
        }
    }

    pub(crate) fn target_discovery_filter_for_owner(
        &self,
        owner_session_id: Option<&str>,
    ) -> Option<Vec<DevToolsTargetFilterEntry>> {
        self.target_control
            .discover_filter_entries(owner_session_id)
    }

    pub(crate) fn initial_target_created_events_for_discovery_owner(
        &mut self,
        owner_session_id: Option<&str>,
        target_infos: Vec<DevToolsTargetInfo>,
    ) -> Vec<BackgroundProtocolEvent> {
        self.target_control
            .initial_target_created_events_for_owner(owner_session_id, target_infos)
    }

    pub(crate) fn has_any_target_discovery(&self) -> bool {
        self.target_control.has_any_discovery()
    }

    pub(crate) fn has_any_target_info_observer(&self) -> bool {
        self.target_control.has_any_target_info_observer()
    }

    fn exact_target_created_events_for_all_discovery_owners(
        &mut self,
        target_info: DevToolsTargetInfo,
    ) -> Vec<BackgroundProtocolEvent> {
        self.target_control
            .target_created_events_for_all_discovery_owners(target_info)
    }

    pub(crate) fn target_created_event_plan(&mut self, target_id: &str) -> TargetEventPlan {
        self.target_created_event_plan_for_target_delta(target_id)
    }

    fn target_created_event_plan_for_target_delta(&mut self, target_id: &str) -> TargetEventPlan {
        let deltas = self.target_deltas_for_target_id(target_id, TargetHostDelta::created);
        self.target_host_delta_events(deltas)
    }

    fn target_deltas_for_target_id(
        &self,
        target_id: &str,
        build_delta: fn(String) -> TargetHostDelta,
    ) -> Vec<TargetHostDelta> {
        self.target_control
            .target_deltas_for_target_id(target_id, build_delta)
    }

    fn target_host_delta_events(
        &mut self,
        deltas: impl IntoIterator<Item = TargetHostDelta>,
    ) -> TargetEventPlan {
        self.prepared_target_host_delta_events(
            deltas
                .into_iter()
                .map(PreparedTargetHostDelta::without_snapshot),
        )
    }

    pub(crate) fn prepared_target_host_delta_event_plan(
        &mut self,
        prepared_delta: PreparedTargetHostDelta,
    ) -> TargetEventPlan {
        self.prepared_target_host_deltas_event_plan([prepared_delta])
    }

    pub(crate) fn prepared_target_info_changed_event_plan_for_discovery_owners(
        &self,
        prepared_delta: PreparedTargetHostDelta,
    ) -> TargetEventPlan {
        let (delta, prepared_snapshot) = prepared_delta.into_parts();
        let TargetHostDelta::InfoChanged { target_id } = delta else {
            debug_assert!(false, "expected a prepared targetInfoChanged delta");
            return TargetEventPlan::default();
        };
        let Some(target_info) =
            prepared_snapshot.or_else(|| self.target_info_for_host_delta(&target_id))
        else {
            return TargetEventPlan::default();
        };
        TargetEventPlan::from_background_events(
            self.target_control
                .target_info_changed_events_for_all_discovery_owners(target_info),
        )
    }

    pub(crate) fn prepared_target_host_deltas_event_plan(
        &mut self,
        prepared_deltas: impl IntoIterator<Item = PreparedTargetHostDelta>,
    ) -> TargetEventPlan {
        self.prepared_target_host_delta_events(prepared_deltas)
    }

    pub(crate) fn prepared_top_level_target_host_deltas_event_plan(
        &mut self,
        prepared_deltas: impl IntoIterator<Item = PreparedTargetHostDelta>,
    ) -> TargetEventPlan {
        self.prepared_target_host_delta_events(prepared_deltas)
            .without_target_lifecycle_automation_sidecars()
    }

    pub(crate) fn prepare_destroyed_target_host_delta(
        &self,
        target_id: &str,
    ) -> Option<PreparedTargetHostDelta> {
        self.target_info_for_host_delta(target_id)
            .map(|target_info| {
                PreparedTargetHostDelta::destroyed(target_id.to_owned(), Some(target_info))
            })
    }

    pub(crate) fn prepare_target_host_closure(&self, target_id: &str) -> PreparedTargetHostClosure {
        let mut detached_info_deltas = Vec::new();
        let mut destroyed_deltas = Vec::new();
        for delta in self.target_control.target_destroyed_deltas(target_id) {
            let target_id = delta.target_id().to_owned();
            let Some(target_info) = self.target_info_for_host_delta(&target_id) else {
                continue;
            };
            if target_info.attached {
                let mut detached_target_info = target_info.clone();
                detached_target_info.attached = false;
                detached_info_deltas.push(PreparedTargetHostDelta::info_changed(
                    target_id.clone(),
                    Some(detached_target_info),
                ));
            }
            destroyed_deltas.push(PreparedTargetHostDelta::destroyed(
                target_id,
                Some(target_info),
            ));
        }
        PreparedTargetHostClosure::new(detached_info_deltas, destroyed_deltas)
    }

    fn prepared_target_host_delta_events(
        &mut self,
        deltas: impl IntoIterator<Item = PreparedTargetHostDelta>,
    ) -> TargetEventPlan {
        TargetEventPlan::from_background_events(
            deltas
                .into_iter()
                .flat_map(|delta| self.single_prepared_target_host_delta_events(delta))
                .collect(),
        )
    }

    fn single_prepared_target_host_delta_events(
        &mut self,
        prepared_delta: PreparedTargetHostDelta,
    ) -> Vec<BackgroundProtocolEvent> {
        let (delta, prepared_snapshot) = prepared_delta.into_parts();
        match delta {
            TargetHostDelta::Created { target_id } => {
                let Some(target_info) =
                    prepared_snapshot.or_else(|| self.target_info_for_host_delta(&target_id))
                else {
                    return Vec::new();
                };
                self.exact_target_created_events_for_all_discovery_owners(target_info)
            }
            TargetHostDelta::InfoChanged { target_id } => {
                let Some(target_info) =
                    prepared_snapshot.or_else(|| self.target_info_for_host_delta(&target_id))
                else {
                    return Vec::new();
                };
                self.exact_target_info_changed_events_for_all_observer_owners(target_info)
            }
            TargetHostDelta::Destroyed { target_id } => {
                let Some(target_info) =
                    prepared_snapshot.or_else(|| self.target_info_for_host_delta(&target_id))
                else {
                    return Vec::new();
                };
                self.exact_target_destroyed_events_for_all_discovery_owners(target_info)
            }
        }
    }

    fn target_info_for_host_delta(&self, target_id: &str) -> Option<DevToolsTargetInfo> {
        self.tab_target_info(target_id).or_else(|| {
            self.browser_contexts()
                .find_map(|browser_context| browser_context.devtools_target_info(target_id))
        })
    }

    fn exact_target_info_changed_events_for_all_observer_owners(
        &self,
        target_info: DevToolsTargetInfo,
    ) -> Vec<BackgroundProtocolEvent> {
        self.target_control
            .target_info_changed_events_for_all_observer_owners(target_info)
    }

    fn exact_target_destroyed_events_for_all_discovery_owners(
        &mut self,
        target_info: DevToolsTargetInfo,
    ) -> Vec<BackgroundProtocolEvent> {
        self.target_control
            .target_destroyed_events_for_all_discovery_owners(target_info)
    }

    pub(crate) fn target_crashed_events_for_all_discovery_owners(
        &self,
        target_id: &str,
        status: &str,
        error_code: i32,
    ) -> Vec<BackgroundProtocolEvent> {
        self.target_control
            .target_crashed_events_for_all_discovery_owners(target_id, status, error_code)
    }

    pub(crate) fn target_destroyed_automation_events(
        &self,
        target_info: DevToolsTargetInfo,
    ) -> Vec<BackgroundProtocolEvent> {
        target_destroyed_automation_events(
            self.project_page_tab_target_infos_for_destruction(target_info),
        )
    }

    fn project_page_tab_target_infos_for_destruction(
        &self,
        target_info: DevToolsTargetInfo,
    ) -> Vec<DevToolsTargetInfo> {
        self.target_control
            .project_page_tab_target_infos_for_destruction(target_info)
    }

    #[cfg(test)]
    pub(crate) fn tab_target_count(&self) -> usize {
        self.target_control.len()
    }

    #[cfg(test)]
    pub(crate) fn target_registry_host_kind(&self, target_id: &str) -> Option<DevToolsTargetKind> {
        self.target_control.host_kind(target_id)
    }

    pub fn install_default_browser_target(&mut self) {
        self.debug_assert_browser_context_topology_projection();
        if self.registered_browser_context_count() != 0 {
            return;
        }

        let mut browser_context =
            self.new_browser_context(self.default_browser_context_id().to_owned());
        browser_context.stage_active_target_for_browser_context_registration(
            self.default_target_id().to_owned(),
        );
        browser_context.set_target_url("about:blank".to_owned());
        browser_context.mark_active_initial_document_page_build_pending();
        let registration_metadata = BrowserContextRegistrationMetadata::with_active_target_creation(
            BrowserTargetCreationMetadata::with_initial_empty_document(
                BrowserInitialEmptyDocumentSeed::new("about:blank"),
            ),
        );
        if let Err(error) =
            self.try_insert_browser_context_with_metadata(browser_context, registration_metadata)
        {
            tracing::warn!(
                error = %error,
                "default BrowserContext registration projection was rejected"
            );
            return;
        }
        let default_target_id = self.default_target_id().to_owned();
        self.register_top_level_page_target(&default_target_id);
        self.notify_target_host_activated(&default_target_id);
    }

    pub fn enable_default_target_on_auto_attach(&mut self) {
        self.install_default_target_on_auto_attach = true;
    }

    pub(crate) fn install_default_browser_target_for_auto_attach_if_enabled(&mut self) {
        if self.install_default_target_on_auto_attach {
            self.install_default_browser_target();
        }
    }

    pub fn gen_target_id(&mut self) -> String {
        loop {
            let id = self.browser_host_state.allocate_target_sequence();
            let target_id = format!("TID-{id}");
            // Target ids supplied while restoring or embedding an existing
            // target share the same CDP namespace as ids allocated here.
            // Never let a later worker/page allocation alias such a target:
            // looking it up would otherwise return the pre-existing target's
            // kind and state even though the renderer record names a worker.
            let target_id_is_live = self
                .browser_host_state
                .navigation_owner()
                .has_target(&target_id)
                || self.target_control.host_kind(&target_id).is_some()
                || self.browser_contexts().any(|context| {
                    context.has_shared_worker_target(&target_id)
                        || context.has_service_worker_target(&target_id)
                });
            if !target_id_is_live {
                return target_id;
            }
        }
    }

    fn gen_tab_target_id(&mut self) -> String {
        loop {
            let id = if let Some(allocator) = self.shared_tab_target_id_allocator.as_ref() {
                allocator
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                        current.checked_add(1)
                    })
                    .expect("shared tab target id space exhausted")
                    + 1
            } else {
                self.next_tab_target_id = self
                    .next_tab_target_id
                    .checked_add(1)
                    .expect("tab target id space exhausted");
                u64::from(self.next_tab_target_id)
            };
            let target_id = format!("TAB-{id}");
            if self.target_control.host_kind(&target_id).is_none() {
                return target_id;
            }
        }
    }

    pub fn gen_session_id(&mut self) -> String {
        loop {
            self.next_session_id = self
                .next_session_id
                .checked_add(1)
                .expect("DevTools session id space exhausted");
            let session_id = format!("SID-{}", self.next_session_id);
            // Embedded callers and test/protocol bootstrap paths may install a
            // caller-supplied session id without advancing this allocator.
            // A generated id must therefore be unique in the live CDP
            // namespace, not merely unique among earlier generated ids.
            //
            // Chromium sidesteps this collision class by assigning each
            // attached DevTools session a fresh UnguessableToken. Moli
            // keeps readable ids, so it must explicitly skip occupied ones.
            if self.session_route(Some(&session_id)).is_none() {
                return session_id;
            }
        }
    }

    pub(crate) fn open_global_io_stream(&mut self, bytes: Vec<u8>) -> String {
        self.next_global_io_stream_id = self
            .next_global_io_stream_id
            .checked_add(1)
            .expect("global IO stream id space exhausted");
        let handle = format!("BROWSER-STREAM-{}", self.next_global_io_stream_id);
        self.global_io_streams
            .insert(handle.clone(), IoStreamState::from_bytes(bytes, 0));
        handle
    }

    fn notify_target_host_lifecycle(&self, delta: CdpTargetHostLifecycleDelta) {
        if let Some(observer) = self.target_host_lifecycle_observer.as_ref() {
            observer.notify(delta);
        }
    }

    pub(crate) fn notify_target_host_activated(&self, target_id: &str) {
        self.notify_target_host_lifecycle(CdpTargetHostLifecycleDelta::Activated {
            target_id: target_id.to_owned(),
        });
        if let Some(tab_target_id) = self.tab_target_id_for_page_target_id(target_id) {
            self.notify_target_host_lifecycle(CdpTargetHostLifecycleDelta::Activated {
                target_id: tab_target_id.to_owned(),
            });
        }
    }
}

pub(crate) struct NoneSessionOwnerRouteOverrideScope<'a> {
    conn: &'a mut CdpConnection,
    previous_route: Option<Option<CdpSessionRoute>>,
}

impl<'a> NoneSessionOwnerRouteOverrideScope<'a> {
    fn enter(conn: &'a mut CdpConnection, target_route: Option<CdpSessionRoute>) -> Self {
        let previous_route = conn.replace_none_session_owner_route_override(target_route);
        Self {
            conn,
            previous_route: Some(previous_route),
        }
    }

    pub(crate) fn conn_mut(&mut self) -> &mut CdpConnection {
        self.conn
    }

    pub(crate) fn restore(&mut self) {
        if let Some(previous_route) = self.previous_route.take() {
            self.conn
                .replace_none_session_owner_route_override(previous_route);
        }
    }
}

impl Drop for NoneSessionOwnerRouteOverrideScope<'_> {
    fn drop(&mut self) {
        self.restore();
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BrowserDownloadEventSubscriptions {
    pub webdriver_bidi_events_enabled: bool,
    automation_events_enabled: bool,
    browser_context_automation_events_enabled: HashMap<String, bool>,
    browser_event_subscription_generations: HashMap<Option<String>, u64>,
    next_browser_event_subscription_generation: u64,
}

impl BrowserDownloadEventSubscriptions {
    pub(crate) fn enable_webdriver_bidi_events(&mut self) -> bool {
        let changed = !self.webdriver_bidi_events_enabled;
        self.webdriver_bidi_events_enabled = true;
        changed
    }

    pub(crate) fn disable_webdriver_bidi_events(&mut self) -> bool {
        let changed = self.webdriver_bidi_events_enabled;
        self.webdriver_bidi_events_enabled = false;
        changed
    }

    fn set_automation_events_enabled_for_browser_context(
        &mut self,
        browser_context_id: Option<&str>,
        enabled: bool,
    ) {
        if let Some(browser_context_id) = browser_context_id {
            self.browser_context_automation_events_enabled
                .insert(browser_context_id.to_owned(), enabled);
        } else {
            self.automation_events_enabled = enabled;
        }
    }

    fn ensure_automation_event_override_for_browser_context(&mut self, browser_context_id: &str) {
        self.browser_context_automation_events_enabled
            .entry(browser_context_id.to_owned())
            .or_insert(false);
    }

    fn clear_automation_events_for_browser_context(&mut self, browser_context_id: &str) {
        self.browser_context_automation_events_enabled
            .remove(browser_context_id);
    }

    fn automation_events_enabled_for_browser_context(
        &self,
        browser_context_id: Option<&str>,
    ) -> bool {
        browser_context_id
            .and_then(|browser_context_id| {
                self.browser_context_automation_events_enabled
                    .get(browser_context_id)
            })
            .copied()
            .unwrap_or(self.automation_events_enabled)
    }

    pub(crate) fn set_browser_events_enabled_for_session(
        &mut self,
        session_id: Option<&str>,
        enabled: bool,
    ) {
        self.next_browser_event_subscription_generation = self
            .next_browser_event_subscription_generation
            .wrapping_add(1);
        let session_id = session_id.map(str::to_owned);
        if enabled {
            self.browser_event_subscription_generations
                .insert(session_id, self.next_browser_event_subscription_generation);
        } else {
            self.browser_event_subscription_generations
                .remove(&session_id);
        }
    }

    pub(crate) fn browser_event_observers(&self) -> Vec<(Option<String>, u64)> {
        let mut observers = self
            .browser_event_subscription_generations
            .iter()
            .map(|(session_id, generation)| (session_id.clone(), *generation))
            .collect::<Vec<_>>();
        observers.sort_by(|left, right| left.0.cmp(&right.0));
        observers
    }

    #[cfg(test)]
    fn browser_event_session_ids(&self) -> Vec<Option<String>> {
        self.browser_event_observers()
            .into_iter()
            .map(|(session_id, _)| session_id)
            .collect()
    }

    pub(crate) fn browser_event_subscription_is_current(
        &self,
        session_id: Option<&str>,
        generation: u64,
    ) -> bool {
        self.browser_event_subscription_generations
            .get(&session_id.map(str::to_owned))
            .is_some_and(|current| *current == generation)
    }
}

pub type PermissionOverride = BrowserPermissionOverride;

#[cfg(test)]
mod tests;
