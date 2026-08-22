//! Protocol-neutral browser owner identities and contracts.
//!
//! The first migration step keeps the existing protocol scheduler, but moves
//! identities used to authorize browser actions out of the CDP connection
//! model. Later Browser Host state and queues build on this boundary.

mod actor;
mod auxiliary_navigation;
mod command_outcome;
mod context_handle;
mod context_runtime_registry;
mod document_lifecycle_wait;
mod download;
mod fact_journal;
mod handle;
mod identity;
mod identity_allocator;
mod initial_target_navigation;
mod navigation_owner;
mod network_artifacts;
mod network_body;
mod owner_input;
mod page_payload;
mod page_residence;
mod policy;
mod state;
mod target_handle;
mod target_session_storage;
mod turn;

pub use actor::{BrowserHostActor, BrowserHostTurnSelection};
pub use auxiliary_navigation::{BrowserAuxiliaryNavigationInput, BrowserAuxiliaryNavigationKind};
pub use command_outcome::{
    BrowserHistoryTraversalResult, BrowserNavigateCommandError, BrowserNavigateCommandErrorKind,
    BrowserNavigateCommandOutcome, BrowserNavigateCommandResult,
};
pub use context_handle::BrowserContextHandle;
pub use context_runtime_registry::BrowserContextRuntimeRegistryError;
pub use document_lifecycle_wait::{
    BrowserDocumentLifecycleWaitOutcome, BrowserDocumentLifecycleWaitReadiness,
    BrowserDocumentLifecycleWaitTicket, BrowserDocumentLifecycleWaitUnavailableReason,
};
pub use download::{
    BrowserDownloadArtifactOutcome, BrowserDownloadBehavior, BrowserDownloadCancelOutcome,
    BrowserDownloadPolicy, BrowserDownloadPolicyState, BrowserDownloadPolicyUpdate,
    BrowserDownloadRegistry,
};
pub use fact_journal::{
    BrowserFact, BrowserFactEnvelope, BrowserFactPublishError, BrowserFactReceiveError,
    BrowserFactSequence, BrowserFactSubscriber, BrowserFactTryReceiveError,
    BrowserFactWakeReceiveError, BrowserFactWakeSubscriber, BrowserFactWakeTryReceiveError,
};
pub use handle::{BrowserHostHandle, BrowserHostInputPublishError};
pub use identity::{BrowserContextId, BrowserTargetId};
pub use identity_allocator::BrowserTargetIdAllocator;
pub use initial_target_navigation::{
    BrowserInitialTargetNavigationCommandInput, BrowserInitialTargetNavigationInput,
};
pub use navigation_owner::{
    BrowserContextActivation, BrowserContextDisposalReservation, BrowserContextRegistration,
    BrowserContextRegistrationMetadata, BrowserContextRegistryError, BrowserContextRemoval,
    BrowserContextRemovalPermit, BrowserContextSelectionProjection, BrowserContextTargetSnapshot,
    BrowserDocumentNavigation, BrowserExactHistoryTraversalResolutionError,
    BrowserHistoryTraversalDestination, BrowserHistoryTraversalResolution,
    BrowserHistoryTraversalResolutionError, BrowserInitialEmptyDocumentCreator,
    BrowserInitialEmptyDocumentSeed, BrowserInitialEmptyDocumentSnapshot, BrowserInstanceId,
    BrowserNavigationFailure, BrowserNavigationHistory, BrowserNavigationHistoryEntry,
    BrowserNavigationHistoryPageSnapshot, BrowserNavigationHistorySeed,
    BrowserNavigationHistoryUpdate, BrowserNavigationOwner, BrowserNavigationRequestId,
    BrowserNavigationTraceContext, BrowserNavigationTraceEvent, BrowserNavigationTraceSource,
    BrowserPageFetchConfiguration, BrowserPageOwnerKey, BrowserPageReplacement,
    BrowserPageReplacementCommitError, BrowserPageReplacementPermit,
    BrowserPageResidenceRegistryError, BrowserPageResidenceTransition,
    BrowserPageResidenceTransitionCommitError, BrowserPageResidenceTransitionKind,
    BrowserPageResidenceTransitionPermit, BrowserSameDocumentHistoryUpdateError,
    BrowserSameDocumentNavigationCommitError, BrowserSelectedTargetEngineDisposition,
    BrowserTargetActivation, BrowserTargetCreationMetadata, BrowserTargetEngineAdoptionError,
    BrowserTargetEngineContextMismatch, BrowserTargetEngineHandoff,
    BrowserTargetEngineHandoffOutcome, BrowserTargetEngineOwnerMismatch,
    BrowserTargetEngineResidence, BrowserTargetMetadataTransition, BrowserTargetRegistration,
    BrowserTargetRegistryError, BrowserTargetResidence, BrowserTargetSlotProjection,
    BrowserTargetStateSnapshot, BrowserTargetTermination, BrowserTargetTerminationCommitError,
    BrowserTargetTerminationKind, BrowserTargetTerminationPermit, BrowserTargetTerminationRequest,
    BrowserTargetTopologyProjection, BrowserTopLevelTargetSnapshot,
};
pub use network_artifacts::{BrowserNetworkArtifactStore, BrowserNetworkResponseBody};
pub use network_body::{
    CapturedBody as BrowserNetworkBody, CapturedBodyChunkReader as BrowserNetworkBodyChunkReader,
    CapturedBodyWriter as BrowserNetworkBodyWriter,
    DEFAULT_BODY_MATERIALIZE_LIMIT as DEFAULT_BROWSER_NETWORK_BODY_MATERIALIZE_LIMIT,
    ensure_materialize_limit as ensure_browser_network_body_materialize_limit,
};
pub use owner_input::{
    BrowserCommandId, BrowserContextDisposalCommandInput, BrowserFrontendCommand,
    BrowserHistoryTraversalCommandInput, BrowserNavigateCommandInput, BrowserOwnerInput,
    BrowserOwnerInputKind, BrowserPageTerminationInput, BrowserPausedNavigationAuthDecision,
    BrowserPausedNavigationContinueDecision, BrowserPausedNavigationDecision,
    BrowserPausedNavigationDecisionInput, BrowserPausedNavigationFulfillDecision,
    BrowserPausedNavigationResponseDecision, BrowserReloadCommandInput,
    BrowserStopLoadingCommandInput, BrowserTargetTerminationInput, RendererBrowserIntent,
    RendererTopLevelHistoryTraversalInput, RendererTopLevelLocationNavigationInput,
};
pub use page_payload::{
    BrowserPageRuntimeAccess, BrowserPageRuntimeLease, BrowserPageRuntimeOwner,
};
pub use page_residence::{BrowserPageResidenceHandle, PageResidenceIdentity};
pub use policy::{
    BrowserHostNetworkPolicySnapshot, BrowserHostPolicyState, BrowserHostPolicyUpdate,
    BrowserPermissionOverride, BrowserWindowBounds, EmulatedGeolocationOverride,
    EmulatedGeolocationOverrideState, EmulatedNetworkConditions,
};
pub use state::BrowserHostState;
pub use target_handle::BrowserTargetHandle;
pub use target_session_storage::BrowserTargetSessionStorageAccess;
pub(crate) use target_session_storage::BrowserTargetSessionStorageSeed;
pub use turn::{BrowserHostTurn, BrowserHostTurnExecutor};
