//! Protocol-neutral DevTools owner and dispatch layer for Moli.
//!
//! This crate is being split away from the Chrome DevTools Protocol wire shape.
//! It still contains transitional CDP-named owner types, but protocol-specific
//! parsing and Chrome protocol metadata belong in
//! `moli-protocol-cdp`.

mod cdp_projection;
pub mod conn;
mod devtools_host_adapter;
pub mod devtools_runtime;
pub mod domains;
#[cfg(feature = "test-support")]
pub mod test_support;
#[cfg(test)]
pub mod testing;
pub mod version;

pub use devtools_host_adapter::{
    DevToolsCommandDispatch, DevToolsHostAdapter, DevToolsHostControl, DevToolsHostView,
    DevToolsSchedulerProjection,
};
pub use devtools_runtime::*;

pub use conn::{
    BackgroundCommandResponsePayload, BackgroundEventSender, BackgroundNavigationCancellation,
    BackgroundNavigationGateKey, BackgroundOutputClosed, BackgroundProtocolEvent,
    BrowserBackgroundOutputReceiver, BrowserHostTurnDispatch, BrowserHostTurnExecutorOwner,
    CdpCommandTaskStep, CdpConnection, CdpInitialStoragePartition, CdpRendererCommandAccess,
    CdpRendererCommandReplacement, CdpRendererCommandReplayDispatch, CdpRendererOwnerTurnOutcome,
    CdpSchedulerEvent, CdpTargetHostLifecycleDelta, CdpTargetHostLifecycleObserver, CdpTurnOutcome,
    CommandDispatchContext, CommandResponseFlushContext, CommandResponseFlushPermit,
    CompletedBrowserHostTurn, CompletedCdpCommandDispatch,
    CompletedDeferredMainDocumentLoadCompletion, CompletedRuntimeProtocolMessageDispatch,
    DEFAULT_CDP_PAGE_TARGET_ID, DEFAULT_CDP_TAB_TARGET_ID,
    DeferredMainDocumentLoadCompletionOutputAction,
    DeferredMainDocumentLoadCompletionOutputInterest, DeferredMainDocumentLoadCompletionReadiness,
    DeferredMainDocumentLoadObservationId, DeferredMainDocumentLoadPredecessorCandidate,
    DevToolsCommandDispatchOutcome, DevToolsDocumentLifecycleWaitKey,
    DevToolsDocumentLifecycleWaitState, DevToolsDocumentNavigationState,
    DevToolsPageResidenceIdentity, ParsedCdpCommand, PendingBrowserHostTurn,
    PendingCdpCommandDispatch, PendingDeferredMainDocumentLoadCompletion,
    PendingRuntimeProtocolMessageDispatch, browser_background_output_channel,
};
pub use domains::activity::{
    ProtocolSchedulerWork, ProtocolSchedulerWorkKind, ProtocolWorkPublishSequence,
    RuntimeCommandOutputBarrierCompletion, RuntimeCommandOutputBarrierPermit,
    RuntimeCommandOutputBarrierTerminal, RuntimeCommandOutputBarriers,
};
pub use domains::fetch::{
    CompletedDevToolsFetchCommand, DevToolsFetchCommandTaskStep, PendingDevToolsFetchCommand,
};
pub use domains::page::{
    BackgroundNavigationCompletion, BackgroundNavigationParticipantCompletion,
    BackgroundNavigationTurnDisposition, CompletedDevToolsBrowserOwnerNavigationCommand,
    CompletedPageScreencastCapture, DevToolsBrowserOwnerNavigationCommandTaskStep,
    PageScreencastCaptureCompletion, PageScreencastCaptureStart, PageScreencastRegistration,
    PageScreencastSubscriptionStatus, PendingDevToolsBrowserOwnerNavigationCommand,
    PendingPageScreencastCapture, build_default_raster_pdf,
};
pub use domains::runtime::{
    CompletedDevToolsRuntimeCommandDispatch, DevToolsRuntimeCommandTaskStep,
    PendingDevToolsRuntimeCommandDispatch,
};
pub use domains::target::{
    CompletedDevToolsBrowserOwnerContextDisposalCommand,
    DevToolsBrowserOwnerContextDisposalCommandTaskStep,
    PendingDevToolsBrowserOwnerContextDisposalCommand,
};
