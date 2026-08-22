use std::{
    collections::{HashSet, VecDeque},
    future::Future,
    pin::Pin,
};

use moli_core::{
    RendererOutputItem, RendererOutputStreamIdentity, RendererOutputTransportMessage,
    RendererProtocolObservation,
    browser_host::{BrowserFactSequence, BrowserFactWakeSubscriber},
};
use moli_protocol::{
    BackgroundNavigationCompletion, BackgroundProtocolEvent, BrowserBackgroundOutputReceiver,
    CompletedBrowserHostTurn, CompletedDevToolsBrowserOwnerNavigationCommand,
};
use tokio::sync::mpsc;

use crate::browser_host::{BrowserHostOwnerLane, BrowserHostOwnerWake};

pub(crate) type CdpBackgroundEventReceiver = BrowserBackgroundOutputReceiver;
pub(crate) type CdpBackgroundNavigationCompletionReceiver =
    mpsc::UnboundedReceiver<BackgroundNavigationCompletion>;
pub(crate) type CdpRendererPublicationReceiver = moli_core::RendererOutputTransportReceiver;

/// Holds only renderer publications whose main-Document fact arrived before
/// the independently transported Browser commit.
///
/// Stream controls are never parked here, and fences for other streams can
/// skip a held publication without reordering records inside either stream.
/// The exact navigation boundary releases only its own stream.
#[derive(Default)]
pub(crate) struct NavigationRendererPublicationBuffer {
    publications: VecDeque<RendererOutputTransportMessage>,
    blocked_streams: HashSet<RendererOutputStreamIdentity>,
}

impl NavigationRendererPublicationBuffer {
    pub(crate) fn take_releasable(
        &mut self,
        navigation_gate_open: bool,
    ) -> Option<RendererOutputTransportMessage> {
        if !navigation_gate_open {
            self.blocked_streams.clear();
            return self.publications.pop_front();
        }
        let position = self.publications.iter().position(|publication| {
            renderer_publication_stream(publication)
                .is_none_or(|stream| !self.blocked_streams.contains(&stream))
        })?;
        self.publications.remove(position)
    }

    pub(crate) fn admit_or_buffer(
        &mut self,
        publication: RendererOutputTransportMessage,
        navigation_gate_open: bool,
        released_navigation_stream: Option<RendererOutputStreamIdentity>,
    ) -> Option<RendererOutputTransportMessage> {
        let Some(stream) = renderer_publication_stream(&publication) else {
            return Some(publication);
        };
        if released_navigation_stream == Some(stream) {
            self.blocked_streams.remove(&stream);
            return Some(publication);
        }
        if !navigation_gate_open {
            return Some(publication);
        }
        if renderer_main_document_commit_stream(&publication) == Some(stream) {
            self.blocked_streams.insert(stream);
        }
        if !self.blocked_streams.contains(&stream) {
            return Some(publication);
        }
        self.publications.push_back(publication);
        None
    }

    pub(crate) fn take_for_predecessor(
        &mut self,
        predecessor_stream: RendererOutputStreamIdentity,
        releases_navigation_stream: bool,
    ) -> Option<RendererOutputTransportMessage> {
        if releases_navigation_stream {
            self.blocked_streams.remove(&predecessor_stream);
        }
        let position = self.publications.iter().position(|publication| {
            renderer_publication_stream(publication) == Some(predecessor_stream)
                && !self.blocked_streams.contains(&predecessor_stream)
        })?;
        self.publications.remove(position)
    }
}

/// CDP/frontend input mux around the independent Browser Host owner lane.
///
/// Detached navigation command completions remain frontend correlation state;
/// they are intentionally stored here rather than in `BrowserHostOwnerLane`.
pub(crate) struct BrowserHostExecutionLane {
    owner: BrowserHostOwnerLane,
    detached_navigation_completions: Vec<
        Pin<Box<dyn Future<Output = CompletedDevToolsBrowserOwnerNavigationCommand> + 'static>>,
    >,
}

pub(crate) enum BrowserHostExecutionWake {
    TurnSelected,
    ParticipantCompleted(Box<CompletedBrowserHostTurn>),
    DetachedNavigationCompleted(Box<CompletedDevToolsBrowserOwnerNavigationCommand>),
    Closed,
}

impl BrowserHostExecutionLane {
    pub(crate) fn new(actor: moli_core::browser_host::BrowserHostActor) -> Self {
        Self {
            owner: BrowserHostOwnerLane::new(actor),
            detached_navigation_completions: Vec::new(),
        }
    }

    pub(crate) fn has_ready_input(&self) -> bool {
        self.owner.has_ready_input()
    }

    pub(crate) fn has_detached_navigation_wait(&self) -> bool {
        !self.detached_navigation_completions.is_empty()
    }

    pub(crate) fn detach_navigation_completion(
        &mut self,
        completion: Pin<
            Box<dyn Future<Output = CompletedDevToolsBrowserOwnerNavigationCommand> + 'static>,
        >,
    ) {
        self.detached_navigation_completions.push(completion);
    }

    pub(crate) async fn recv_wake(&mut self) -> BrowserHostExecutionWake {
        let owner = &mut self.owner;
        let detached_navigation_completions = &mut self.detached_navigation_completions;
        tokio::select! {
            biased;
            completed = poll_detached_navigation_completion(detached_navigation_completions), if !detached_navigation_completions.is_empty() => {
                BrowserHostExecutionWake::DetachedNavigationCompleted(Box::new(completed))
            }
            wake = owner.recv_wake() => {
                match wake {
                    BrowserHostOwnerWake::TurnSelected => BrowserHostExecutionWake::TurnSelected,
                    BrowserHostOwnerWake::ParticipantCompleted(completed) => {
                        BrowserHostExecutionWake::ParticipantCompleted(completed)
                    }
                    BrowserHostOwnerWake::Closed => BrowserHostExecutionWake::Closed,
                }
            }
        }
    }

    pub(crate) fn start_next_turn(
        &mut self,
        host_adapter: &mut moli_protocol::DevToolsHostAdapter,
    ) -> Option<moli_protocol::CdpTurnOutcome> {
        self.owner.start_next_turn(host_adapter)
    }

    pub(crate) async fn complete_turn(
        &mut self,
        host_adapter: &mut moli_protocol::DevToolsHostAdapter,
        completed: CompletedBrowserHostTurn,
    ) -> moli_protocol::CdpTurnOutcome {
        self.owner.complete_turn(host_adapter, completed).await
    }
}

async fn poll_detached_navigation_completion(
    completions: &mut Vec<
        Pin<Box<dyn Future<Output = CompletedDevToolsBrowserOwnerNavigationCommand> + 'static>>,
    >,
) -> CompletedDevToolsBrowserOwnerNavigationCommand {
    std::future::poll_fn(|cx| {
        for index in 0..completions.len() {
            if let std::task::Poll::Ready(completed) = completions[index].as_mut().poll(cx) {
                drop(completions.swap_remove(index));
                return std::task::Poll::Ready(completed);
            }
        }
        std::task::Poll::Pending
    })
    .await
}

/// Application-side input residences that can wake one browser/protocol owner
/// loop.
///
/// The Browser Host actor deliberately lives here rather than in
/// `CdpScheduler`: the scheduler is a physical/protocol projection adapter,
/// while this composition-side value owns the mailbox receiver and selects
/// exact Browser Host turns independently of frontend input readiness.
pub(crate) struct CdpSchedulerEventReceivers {
    pub(crate) browser_host: BrowserHostExecutionLane,
    pub(crate) browser_fact_wake_rx: BrowserFactWakeSubscriber,
    pub(crate) background_event_rx: CdpBackgroundEventReceiver,
    pub(crate) background_navigation_completion_rx: CdpBackgroundNavigationCompletionReceiver,
    pub(crate) renderer_publication_rx: CdpRendererPublicationReceiver,
    pub(crate) navigation_renderer_publications: NavigationRendererPublicationBuffer,
}

/// One move-owned input selected while direct CDP/WebDriver execution waits
/// for browser/protocol progress.
///
/// Receiving and applying remain separate: once a branch dequeues a concrete
/// renderer/protocol value, the caller completes it before selecting again.
/// The Browser Host start variant contains no raw owner input. Its exact turn
/// stays inside `BrowserHostActor` until the caller invokes the synchronous
/// start adapter. A participant completion is already move-owned and can only
/// resume the exact pending operation that produced it.
pub(crate) enum CdpSchedulerInterleavedInput {
    BrowserHostTurn,
    BrowserHostCompletion(Box<CompletedBrowserHostTurn>),
    DetachedNavigationCompletion(Box<CompletedDevToolsBrowserOwnerNavigationCommand>),
    BrowserFactWake(BrowserFactSequence),
    BackgroundNavigationCompletion(BackgroundNavigationCompletion),
    BackgroundEvent(BackgroundProtocolEvent),
    RendererPublication {
        publication: RendererOutputTransportMessage,
        navigation_gate_open: bool,
    },
}

impl CdpSchedulerEventReceivers {
    pub(crate) fn renderer_navigation_gate_open(
        &self,
        background_navigation_gate_open: bool,
    ) -> bool {
        background_navigation_gate_open || self.browser_host.has_detached_navigation_wait()
    }

    pub(crate) fn take_buffered_renderer_publication(
        &mut self,
        navigation_gate_open: bool,
    ) -> Option<RendererOutputTransportMessage> {
        self.navigation_renderer_publications
            .take_releasable(navigation_gate_open)
    }

    pub(crate) fn admit_or_buffer_navigation_renderer_publication(
        &mut self,
        publication: RendererOutputTransportMessage,
        navigation_gate_open: bool,
        released_navigation_stream: Option<RendererOutputStreamIdentity>,
    ) -> Option<RendererOutputTransportMessage> {
        self.navigation_renderer_publications.admit_or_buffer(
            publication,
            navigation_gate_open,
            released_navigation_stream,
        )
    }

    pub(crate) async fn recv_concrete_renderer_transport(
        &mut self,
        predecessor_stream: RendererOutputStreamIdentity,
        releases_navigation_stream: bool,
    ) -> Option<RendererOutputTransportMessage> {
        if let Some(publication) = self
            .navigation_renderer_publications
            .take_for_predecessor(predecessor_stream, releases_navigation_stream)
        {
            return Some(publication);
        }
        self.renderer_publication_rx.recv().await
    }

    pub(crate) async fn recv_interleaved_input(
        &mut self,
        navigation_gate_open: bool,
    ) -> Option<CdpSchedulerInterleavedInput> {
        let navigation_gate_open = self.renderer_navigation_gate_open(navigation_gate_open);
        if let Some(publication) = self.take_buffered_renderer_publication(navigation_gate_open) {
            return Some(CdpSchedulerInterleavedInput::RendererPublication {
                publication,
                navigation_gate_open,
            });
        }
        tokio::select! {
            biased;
            wake = self.browser_host.recv_wake() => {
                match wake {
                    BrowserHostExecutionWake::TurnSelected => {
                        Some(CdpSchedulerInterleavedInput::BrowserHostTurn)
                    }
                    BrowserHostExecutionWake::ParticipantCompleted(completed) => {
                        Some(CdpSchedulerInterleavedInput::BrowserHostCompletion(completed))
                    }
                    BrowserHostExecutionWake::DetachedNavigationCompleted(completed) => {
                        Some(CdpSchedulerInterleavedInput::DetachedNavigationCompletion(completed))
                    }
                    BrowserHostExecutionWake::Closed => None,
                }
            }
            wake = self.browser_fact_wake_rx.recv() => {
                wake.ok().map(CdpSchedulerInterleavedInput::BrowserFactWake)
            }
            maybe_completion = self.background_navigation_completion_rx.recv() => {
                maybe_completion.map(
                    CdpSchedulerInterleavedInput::BackgroundNavigationCompletion,
                )
            }
            maybe_event = self.background_event_rx.recv() => {
                maybe_event.map(CdpSchedulerInterleavedInput::BackgroundEvent)
            }
            // A navigation gate controls admission of the exact replacement
            // stream, not receipt of every renderer stream. Old-Document
            // terminal facts and stream controls must keep reaching the
            // protocol owner while the replacement participant is pending.
            maybe_publication = self.renderer_publication_rx.recv() => {
                maybe_publication.map(|publication| {
                    CdpSchedulerInterleavedInput::RendererPublication {
                        publication,
                        navigation_gate_open,
                    }
                })
            }
        }
    }
}

fn renderer_publication_stream(
    message: &RendererOutputTransportMessage,
) -> Option<RendererOutputStreamIdentity> {
    match message {
        RendererOutputTransportMessage::Publication(publication) => {
            Some(publication.cursor().stream())
        }
        RendererOutputTransportMessage::StreamControl(_)
        | RendererOutputTransportMessage::PageReservationReleased { .. }
        | RendererOutputTransportMessage::CursorLeaseDeclared { .. }
        | RendererOutputTransportMessage::CursorLeaseReleased { .. } => None,
    }
}

fn renderer_main_document_commit_stream(
    message: &RendererOutputTransportMessage,
) -> Option<RendererOutputStreamIdentity> {
    let RendererOutputTransportMessage::Publication(publication) = message else {
        return None;
    };
    publication
        .records()
        .iter()
        .any(|record| {
            matches!(
                record.item(),
                RendererOutputItem::Observation(RendererProtocolObservation::MainDocumentCommit(_))
            )
        })
        .then(|| publication.cursor().stream())
}

#[cfg(test)]
mod tests {
    use moli_core::{
        PageId, RendererOutputCursor, RendererOutputPublication, RendererOutputRecord,
        RendererOutputStreamControl, page::RendererMainDocumentCommit,
    };

    use super::*;

    #[test]
    fn navigation_publication_buffer_releases_only_the_exact_commit_stream() {
        let navigation_stream =
            RendererOutputStreamIdentity::new_page_for_protocol_test(PageId::new_for_testing(801));
        let unrelated_stream =
            RendererOutputStreamIdentity::new_page_for_protocol_test(PageId::new_for_testing(802));
        let commit_publication: RendererOutputTransportMessage =
            RendererOutputPublication::new_for_test(
                RendererOutputCursor::new_for_test(navigation_stream, 1),
                vec![RendererOutputRecord::new_for_test(
                    RendererOutputItem::Observation(
                        RendererProtocolObservation::MainDocumentCommit(
                            RendererMainDocumentCommit {
                                frame_id: "frame-navigation-buffer".to_owned(),
                                loader_id: "loader-navigation-buffer".to_owned(),
                                url: "https://example.test/navigation-buffer".to_owned(),
                                unreachable_url: None,
                                security_origin: "https://example.test".to_owned(),
                                secure_context_type: "Secure".to_owned(),
                                timestamp: 1.0,
                            },
                        ),
                    ),
                )],
            )
            .into();
        let mut buffer = NavigationRendererPublicationBuffer::default();

        assert!(
            buffer
                .admit_or_buffer(commit_publication.clone(), true, None)
                .is_none(),
            "a main-Document publication must wait for the Browser commit"
        );
        assert!(buffer.take_releasable(true).is_none());
        assert!(
            buffer
                .take_for_predecessor(unrelated_stream, false)
                .is_none(),
            "an unrelated command fence must skip the held navigation stream"
        );

        let control: RendererOutputTransportMessage = RendererOutputStreamControl::Opened {
            stream: unrelated_stream,
        }
        .into();
        assert_eq!(
            buffer.admit_or_buffer(control.clone(), true, None),
            Some(control),
            "stream controls remain admissible while a navigation commit is pending"
        );
        assert_eq!(
            buffer.take_for_predecessor(navigation_stream, true),
            Some(commit_publication),
            "the exact navigation boundary releases its own concrete publication"
        );
    }
}
