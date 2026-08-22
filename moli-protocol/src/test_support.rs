//! Cross-crate fixture construction for protocol scheduler tests.
//!
//! This module is available only with the `test-support` feature. It keeps
//! opaque protocol identities and work payloads private while allowing the
//! top-level adapter scheduler tests to construct exact inputs. Production
//! callers must obtain these values from their owning connection, navigation,
//! lifecycle observer, or publication boundary.

use moli_core::{RendererDocumentLifecycleIdentity, RendererOutputResidenceIdentity};

use crate::{
    BackgroundNavigationGateKey, DeferredMainDocumentLoadCompletionOutputInterest,
    DeferredMainDocumentLoadCompletionReadiness, DeferredMainDocumentLoadObservationId,
    ProtocolSchedulerWork, conn::RendererPageResidenceIdentity,
};

/// Constructs the exact key carried by a synthetic background-navigation
/// completion in scheduler-only tests.
pub fn background_navigation_gate_key(
    target_id: Option<String>,
    session_id: Option<String>,
    frame_id: String,
    loader_id: String,
    navigation_request_id: Option<u64>,
) -> BackgroundNavigationGateKey {
    BackgroundNavigationGateKey::from_test_parts(
        target_id,
        session_id,
        frame_id,
        loader_id,
        navigation_request_id,
    )
}

/// Constructs an already-reached exact load readiness probe for adapter-only
/// scheduling fixtures.
pub fn reached_deferred_main_document_load_readiness() -> DeferredMainDocumentLoadCompletionReadiness
{
    DeferredMainDocumentLoadCompletionReadiness::reached_for_test_support()
}

/// Constructs one nonzero deferred-load observation identity.
pub fn deferred_main_document_load_observation_id(
    value: u64,
) -> DeferredMainDocumentLoadObservationId {
    DeferredMainDocumentLoadObservationId::from_test_value(value)
}

/// Freezes the Page and optional Document observed by a scheduler-only load
/// wait fixture.
pub fn deferred_main_document_load_output_interest(
    renderer_residence: RendererOutputResidenceIdentity,
    renderer_document: Option<RendererDocumentLifecycleIdentity>,
) -> DeferredMainDocumentLoadCompletionOutputInterest {
    let renderer_page = RendererPageResidenceIdentity::from_residence(renderer_residence)
        .expect("deferred main-document load fixtures require a Page residence");
    DeferredMainDocumentLoadCompletionOutputInterest::from_test_residence(
        renderer_page,
        renderer_document,
    )
}

/// Constructs concrete stopped-loading observation work for scheduler ordering
/// tests without exposing its private payload or attachment representation.
pub fn root_frame_stopped_loading_work(
    publish_sequence: u64,
    session_ids: Vec<Option<String>>,
    frame_id: String,
    loader_id: String,
) -> ProtocolSchedulerWork {
    ProtocolSchedulerWork::root_frame_stopped_loading_for_test_support(
        publish_sequence,
        session_ids,
        frame_id,
        loader_id,
    )
}
