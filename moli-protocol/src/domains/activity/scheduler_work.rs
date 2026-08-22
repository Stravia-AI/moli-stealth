use std::fmt;

use moli_core::RendererOutputTransportMessage;

use crate::{
    conn::{
        BidiChannelOwnerAction, CdpConnection, DeferredMainDocumentLoadCompletionOutputAction,
        DeferredMainDocumentLoadCompletionOutputInterest,
        PendingDeferredMainDocumentLoadCompletion,
    },
    devtools_runtime::DevToolsCommandContext,
};

use super::{
    main_document::DeferredMainDocumentLoadCompletionActivity, output_work::ProtocolOutputWork,
};

/// Monotonic sequence assigned when protocol-owned scheduler work becomes
/// durable.
///
/// This sequence orders work published by one `CdpConnection`; it is not an
/// HTML task sequence and is not comparable with a renderer stream-local
/// `RendererOutputCursor`. Cross-owner ordering must therefore use an explicit
/// predecessor rather than comparing unrelated counters.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProtocolWorkPublishSequence(u64);

impl ProtocolWorkPublishSequence {
    pub(crate) fn new(value: u64) -> Self {
        assert_ne!(value, 0, "protocol work publish sequence starts at one");
        Self(value)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// The semantic responsibility carried by one durable protocol work item.
///
/// A protocol observation projects an already-frozen event; the load fact
/// projection consumes the neutral Browser journal; an explicit owner action
/// must remain resident even when no frontend is listening.
///
/// Renderer-sourced navigation and Page/Target termination are intentionally
/// absent. Their exact inputs publish directly to the Core `BrowserHostActor`;
/// adding a protocol work variant here would recreate the removed admission or
/// execution fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolSchedulerWorkKind {
    ProtocolObservation,
    MainDocumentLoadFactProjection,
    BidiChannelOwnerAction,
}

/// Durable protocol-owned work with concrete payload, exact route and one
/// connection-local publication sequence.
///
/// This move-only value never asks a later turn to scan a source. The private
/// payload is a ready protocol observation, an exact Browser-fact projection,
/// or an explicit owner continuation. The common wrapper exists only to give
/// them one scheduler residence and one ordering contract.
pub struct ProtocolSchedulerWork {
    publish_sequence: ProtocolWorkPublishSequence,
    payload: ProtocolSchedulerWorkPayload,
}

enum ProtocolSchedulerWorkPayload {
    ProtocolObservation(ProtocolOutputWork),
    MainDocumentLoadFactProjection(Box<DeferredMainDocumentLoadCompletionActivity>),
    BidiChannelOwnerAction(BidiChannelOwnerAction),
}

impl fmt::Debug for ProtocolSchedulerWork {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("ProtocolSchedulerWork");
        debug
            .field("publish_sequence", &self.publish_sequence)
            .field("kind", &self.kind());
        match &self.payload {
            ProtocolSchedulerWorkPayload::ProtocolObservation(output) => {
                debug.field("payload", output);
            }
            ProtocolSchedulerWorkPayload::MainDocumentLoadFactProjection(completion) => {
                debug
                    .field("observation_id", &completion.observation_id())
                    .field("session_id", &completion.session_id())
                    .field(
                        "renderer_page",
                        &completion.renderer_page_residence_identity(),
                    )
                    .field(
                        "renderer_document",
                        &completion.renderer_document_identity(),
                    )
                    .field("terminal", &completion.has_terminal_browser_fact());
            }
            ProtocolSchedulerWorkPayload::BidiChannelOwnerAction(action) => {
                debug
                    .field("action", &action.kind())
                    .field("session_id", &action.owner().session_id());
            }
        }
        debug.finish()
    }
}

impl ProtocolSchedulerWork {
    pub(crate) fn protocol_observation(
        publish_sequence: ProtocolWorkPublishSequence,
        output: ProtocolOutputWork,
    ) -> Self {
        Self {
            publish_sequence,
            payload: ProtocolSchedulerWorkPayload::ProtocolObservation(output),
        }
    }

    pub(crate) fn main_document_load_fact_projection(
        publish_sequence: ProtocolWorkPublishSequence,
        completion: DeferredMainDocumentLoadCompletionActivity,
    ) -> Self {
        Self {
            publish_sequence,
            payload: ProtocolSchedulerWorkPayload::MainDocumentLoadFactProjection(Box::new(
                completion,
            )),
        }
    }

    pub(crate) fn bidi_channel_owner_action(
        publish_sequence: ProtocolWorkPublishSequence,
        action: BidiChannelOwnerAction,
    ) -> Self {
        Self {
            publish_sequence,
            payload: ProtocolSchedulerWorkPayload::BidiChannelOwnerAction(action),
        }
    }

    pub fn publish_sequence(&self) -> ProtocolWorkPublishSequence {
        self.publish_sequence
    }

    pub fn kind(&self) -> ProtocolSchedulerWorkKind {
        match &self.payload {
            ProtocolSchedulerWorkPayload::ProtocolObservation(_) => {
                ProtocolSchedulerWorkKind::ProtocolObservation
            }
            ProtocolSchedulerWorkPayload::MainDocumentLoadFactProjection(_) => {
                ProtocolSchedulerWorkKind::MainDocumentLoadFactProjection
            }
            ProtocolSchedulerWorkPayload::BidiChannelOwnerAction(_) => {
                ProtocolSchedulerWorkKind::BidiChannelOwnerAction
            }
        }
    }

    /// Reports whether this work can be completed without blocking its
    /// scheduler.
    ///
    /// Protocol observations and already-materialized BiDi owner actions are
    /// intrinsically ready. A main-document load projection becomes ready
    /// only after its exact Browser fact ticket consumes a typed terminal.
    pub fn is_ready(&self) -> bool {
        match &self.payload {
            ProtocolSchedulerWorkPayload::ProtocolObservation(_) => true,
            ProtocolSchedulerWorkPayload::MainDocumentLoadFactProjection(completion) => {
                completion.has_terminal_browser_fact()
            }
            ProtocolSchedulerWorkPayload::BidiChannelOwnerAction(_) => true,
        }
    }

    /// Reports owner work that must complete inside the producing command's
    /// turn.
    ///
    /// Popup navigation is safe to start while completing the producing
    /// command: it retains its exact target route and cannot replace the
    /// command's active renderer. Popup activation is deliberately excluded.
    /// It can replace that renderer and must therefore cross the ordinary
    /// client-turn predecessor so the opener's command result is collected
    /// from its original owner first.
    pub fn is_command_followup(&self) -> bool {
        matches!(
            &self.payload,
            ProtocolSchedulerWorkPayload::BidiChannelOwnerAction(_)
        )
    }

    #[cfg(test)]
    pub(crate) fn bidi_channel_owner_action_kind(
        &self,
    ) -> Option<crate::conn::BidiChannelOwnerActionKind> {
        let ProtocolSchedulerWorkPayload::BidiChannelOwnerAction(action) = &self.payload else {
            return None;
        };
        Some(action.kind())
    }

    pub fn is_root_frame_stopped_loading(&self) -> bool {
        matches!(
            &self.payload,
            ProtocolSchedulerWorkPayload::ProtocolObservation(output)
                if output.is_root_frame_stopped_loading()
        )
    }

    pub fn main_document_load_output_interest(
        &self,
    ) -> Option<DeferredMainDocumentLoadCompletionOutputInterest> {
        let ProtocolSchedulerWorkPayload::MainDocumentLoadFactProjection(completion) =
            &self.payload
        else {
            return None;
        };
        Some(DeferredMainDocumentLoadCompletionOutputInterest::new(
            completion.renderer_page_residence_identity(),
            completion.renderer_document_identity(),
        ))
    }

    pub fn main_document_load_observation_id(
        &self,
    ) -> Option<crate::conn::DeferredMainDocumentLoadObservationId> {
        let ProtocolSchedulerWorkPayload::MainDocumentLoadFactProjection(completion) =
            &self.payload
        else {
            return None;
        };
        Some(completion.observation_id())
    }

    #[cfg(test)]
    pub(crate) fn main_document_load_session_id(&self) -> Option<&str> {
        let ProtocolSchedulerWorkPayload::MainDocumentLoadFactProjection(completion) =
            &self.payload
        else {
            return None;
        };
        completion.session_id()
    }

    pub fn route_renderer_output_while_main_document_load_waits(
        &self,
        output: &RendererOutputTransportMessage,
    ) -> Option<DeferredMainDocumentLoadCompletionOutputAction> {
        self.main_document_load_output_interest()
            .map(|interest| interest.route_output_while_waiting(output))
    }

    pub fn observes_main_document_load_for_devtools_context(
        &self,
        conn: &CdpConnection,
        context: &DevToolsCommandContext,
    ) -> bool {
        let ProtocolSchedulerWorkPayload::MainDocumentLoadFactProjection(completion) =
            &self.payload
        else {
            return false;
        };
        conn.command_owner_scope_for_devtools_context(context)
            .is_some_and(|owner_scope| completion.owner_scope() == &owner_scope)
    }

    pub fn start_main_document_load_wait(self) -> PendingDeferredMainDocumentLoadCompletion {
        match self.payload {
            ProtocolSchedulerWorkPayload::MainDocumentLoadFactProjection(completion) => {
                PendingDeferredMainDocumentLoadCompletion::new((*completion).start_scheduler_step())
            }
            ProtocolSchedulerWorkPayload::ProtocolObservation(_)
            | ProtocolSchedulerWorkPayload::BidiChannelOwnerAction(_) => {
                panic!("only main-document load owner work can start a lifecycle wait")
            }
        }
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn root_frame_stopped_loading_for_test_support(
        publish_sequence: u64,
        session_ids: Vec<Option<String>>,
        frame_id: String,
        loader_id: String,
    ) -> Self {
        Self::protocol_observation(
            ProtocolWorkPublishSequence::new(publish_sequence),
            ProtocolOutputWork::root_frame_stopped_loading_for_test_support(
                session_ids,
                frame_id,
                loader_id,
            ),
        )
    }
}

pub(crate) enum ReadyProtocolSchedulerWork {
    ProtocolObservation(ProtocolOutputWork),
    MainDocumentLoadFactProjection(
        Box<super::main_document::CompletedDeferredMainDocumentLoadCompletionActivity>,
    ),
    BidiChannelOwnerAction(BidiChannelOwnerAction),
}

impl ProtocolSchedulerWork {
    pub(crate) fn into_ready(self) -> ReadyProtocolSchedulerWork {
        match self.payload {
            ProtocolSchedulerWorkPayload::ProtocolObservation(output) => {
                ReadyProtocolSchedulerWork::ProtocolObservation(output)
            }
            ProtocolSchedulerWorkPayload::MainDocumentLoadFactProjection(completion) => {
                let completion = completion.try_complete().unwrap_or_else(|_| {
                    panic!(
                        "pending main-document load work cannot be completed by a nonblocking scheduler turn"
                    )
                });
                ReadyProtocolSchedulerWork::MainDocumentLoadFactProjection(Box::new(completion))
            }
            ProtocolSchedulerWorkPayload::BidiChannelOwnerAction(action) => {
                ReadyProtocolSchedulerWork::BidiChannelOwnerAction(action)
            }
        }
    }
}
