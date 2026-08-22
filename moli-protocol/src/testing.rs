//! Test helpers for exercising the CDP connection and its scheduler.
//!
//! `TestContext` owns a `CdpConnection`, accepts JSON commands, and exposes
//! focused assertions over emitted results, events, and errors.

use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};
use std::time::Duration;

#[cfg(test)]
use super::conn::RendererCommandDescriptor;
use super::conn::{
    BackgroundEventSender, BackgroundNavigationGateKey, BackgroundProtocolEvent,
    BrowserBackgroundOutputReceiver, CdpCommandTaskStep, CdpConnection, CdpInitialStoragePartition,
    CdpRendererOwnerTurnOutcome, CdpSchedulerEvent, CommandDispatchContext,
    CommandResponseFlushPermit, LoadedNavigationRendererAttachmentCommit, ParsedCdpCommand,
    PendingCdpCommandDispatch, RuntimeInspectorResponseReady, browser_background_output_channel,
};
use crate::devtools_runtime::{DevToolsCommand, DevToolsCommandResult, DevToolsError};
use crate::domains::activity::{
    ProtocolSchedulerWork, RuntimeCommandOutputBarrierCompletion,
    RuntimeCommandOutputBarrierPermit, RuntimeCommandOutputBarriers,
};
use moli_core::{
    LayoutPolicy, OptionalResourceFetchMask, RendererOutputFence, RendererOutputItem,
    RendererOutputStreamControl, RendererOutputStreamIdentity, RendererOutputTransportMessage,
    RendererProtocolObservation, browser_host::BrowserHostActor, runtime::NavigationRuntimeConfig,
};
use moli_fetch::FetchConfig;
#[cfg(test)]
use std::net::SocketAddr;
#[cfg(test)]
use tokio::io::AsyncWriteExt;
#[cfg(test)]
use tokio::net::TcpListener;
#[cfg(test)]
use tokio::task::JoinHandle;

#[cfg(test)]
// Full-workspace runs execute CPU-heavy crypto vectors beside protocol tests.
// Keep this as a bounded diagnostic guard, but leave enough headroom for a
// real renderer-owner wake to be scheduled under that contention.
const TEST_SCHEDULER_INPUT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct TestContext {
    pub conn: CdpConnection,
    pub sent: Vec<Value>,
    pending_runtime_deferred_replies: VecDeque<PendingTestRuntimeDeferredReply>,
    browser_host_actor: BrowserHostActor,
    pending_protocol_scheduler_work: VecDeque<ProtocolSchedulerWork>,
    runtime_command_output_barriers: RuntimeCommandOutputBarriers,
    runtime_inspector_response_ready_rx:
        tokio::sync::mpsc::UnboundedReceiver<RuntimeInspectorResponseReady>,
    renderer_publication_rx: moli_core::RendererOutputTransportReceiver,
    background_event_tx: BackgroundEventSender,
    background_event_rx: BrowserBackgroundOutputReceiver,
    background_navigation_completion_tx:
        tokio::sync::mpsc::UnboundedSender<crate::domains::page::BackgroundNavigationCompletion>,
    background_navigation_completion_rx:
        tokio::sync::mpsc::UnboundedReceiver<crate::domains::page::BackgroundNavigationCompletion>,
    background_navigation_scheduler_enabled: bool,
    background_navigation_gate: HashSet<BackgroundNavigationGateKey>,
    pending_navigation_renderer_publications: VecDeque<RendererOutputTransportMessage>,
    blocked_navigation_renderer_streams: HashSet<RendererOutputStreamIdentity>,
}

struct PendingTestRuntimeDeferredReply {
    pending: PendingCdpCommandDispatch,
    command_context: CommandDispatchContext,
    runtime_output_barrier: Option<RuntimeCommandOutputBarrierPermit>,
}

impl PendingTestRuntimeDeferredReply {
    fn new(
        pending: PendingCdpCommandDispatch,
        command_context: CommandDispatchContext,
        runtime_output_barrier: Option<RuntimeCommandOutputBarrierPermit>,
    ) -> Self {
        Self {
            pending,
            command_context,
            runtime_output_barrier,
        }
    }

    fn command_id(&self) -> Option<u64> {
        self.pending.command_id()
    }
}

enum TestSchedulerWork {
    BrowserOwnerTurn,
    ProtocolEvents(Vec<BackgroundProtocolEvent>),
    SchedulerEvents(Vec<CdpSchedulerEvent>),
    BackgroundEvent(BackgroundProtocolEvent),
    BackgroundNavigationCompletion(crate::domains::page::BackgroundNavigationCompletion),
    RuntimeDeferredReplyReady(RuntimeInspectorResponseReady),
    RendererPublication(RendererOutputTransportMessage),
    ReleaseRuntimeOutputBarrier(RuntimeCommandOutputBarrierPermit),
    CancelRuntimeOutputBarrier(RuntimeCommandOutputBarrierPermit),
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

#[must_use = "the held test command response must be released exactly once"]
pub(crate) struct TestCommandResponseFlushPermit {
    response_flush: CommandResponseFlushPermit,
    runtime_output_barrier: Option<RuntimeCommandOutputBarrierPermit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestSchedulerTurnOutcome {
    Idle,
    Processed(TestSchedulerInputKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestSchedulerInputKind {
    BrowserOwnerInput,
    BackgroundEvent,
    BackgroundNavigationCompletion,
    RuntimeDeferredReply,
    RendererPublication,
}

impl Default for TestContext {
    fn default() -> Self {
        Self::new()
    }
}

fn set_test_target_discovery(conn: &mut CdpConnection, enabled: bool) {
    conn.set_root_target_discovery_enabled(enabled);
}

fn real_layout_test_runtime_config(
    optional_resource_fetch_mask: OptionalResourceFetchMask,
) -> NavigationRuntimeConfig {
    NavigationRuntimeConfig::new(
        FetchConfig::default(),
        optional_resource_fetch_mask,
        true,
        LayoutPolicy::OnDemand,
    )
}

pub(crate) fn real_layout_test_connection() -> CdpConnection {
    CdpConnection::new_with_initial_storage_partition_and_runtime_config(
        CdpInitialStoragePartition::memory(),
        real_layout_test_runtime_config(OptionalResourceFetchMask::NONE),
    )
}

impl TestContext {
    const INACTIVE_FIXTURE_SELECTED_CONTEXT_ID: &'static str = "BID-test-context-selected-fixture";

    /// Build the default CDP test harness used by older internal unit tests.
    ///
    /// Historically those tests treated `Target.targetCreated` as a baseline
    /// event after `Target.createTarget`. Real CDP clients only receive target
    /// discovery events after enabling discovery, so keep that convenience in
    /// `TestContext` instead of changing `CdpConnection::new()`.
    pub fn new() -> Self {
        Self::new_with_target_discovery(true)
    }

    /// Build a CDP test harness with explicit Target discovery state.
    ///
    /// Use `true` for internal tests that assert Target-domain event payloads
    /// without spelling out the setup command. Use `false` for Chromium-parity
    /// tests that need to verify the default protocol behavior before
    /// `Target.setDiscoverTargets(true)` is called.
    pub fn new_with_target_discovery(target_discovery_enabled: bool) -> Self {
        let mut conn = real_layout_test_connection();
        set_test_target_discovery(&mut conn, target_discovery_enabled);
        Self::from_conn(conn)
    }

    pub fn new_with_layout_policy(layout_policy: LayoutPolicy) -> Self {
        let conn = CdpConnection::new_with_initial_storage_partition_and_runtime_config(
            CdpInitialStoragePartition::memory(),
            NavigationRuntimeConfig::new(
                FetchConfig::default(),
                OptionalResourceFetchMask::NONE,
                true,
                layout_policy,
            ),
        );
        Self::from_conn(conn)
    }

    pub fn new_with_target_discovery_and_image_fetch(
        target_discovery_enabled: bool,
        image_fetch_enabled: bool,
    ) -> Self {
        Self::new_with_target_discovery_and_optional_resource_fetch_mask(
            target_discovery_enabled,
            if image_fetch_enabled {
                OptionalResourceFetchMask::IMAGE
            } else {
                OptionalResourceFetchMask::NONE
            },
        )
    }

    pub fn new_with_target_discovery_and_optional_resource_fetch_mask(
        target_discovery_enabled: bool,
        optional_resource_fetch_mask: OptionalResourceFetchMask,
    ) -> Self {
        let mut conn = CdpConnection::new_with_initial_storage_partition_and_runtime_config(
            CdpInitialStoragePartition::memory(),
            real_layout_test_runtime_config(optional_resource_fetch_mask),
        );
        set_test_target_discovery(&mut conn, target_discovery_enabled);
        Self::from_conn(conn)
    }

    pub fn from_conn(mut conn: CdpConnection) -> Self {
        let (browser_host_actor, browser_host_handle) =
            BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(browser_host_handle);
        let (renderer_publication_tx, renderer_publication_rx) =
            moli_core::renderer_output_transport_channel();
        let (runtime_inspector_response_ready_tx, runtime_inspector_response_ready_rx) =
            tokio::sync::mpsc::unbounded_channel();
        let (background_event_tx, background_event_rx) = browser_background_output_channel();
        let (background_navigation_completion_tx, background_navigation_completion_rx) =
            tokio::sync::mpsc::unbounded_channel();
        conn.set_renderer_publication_sender(renderer_publication_tx);
        conn.set_runtime_inspector_response_ready_sender(runtime_inspector_response_ready_tx);
        Self {
            conn,
            sent: Vec::new(),
            pending_runtime_deferred_replies: VecDeque::new(),
            browser_host_actor,
            pending_protocol_scheduler_work: VecDeque::new(),
            runtime_command_output_barriers: RuntimeCommandOutputBarriers::default(),
            runtime_inspector_response_ready_rx,
            renderer_publication_rx,
            background_event_tx,
            background_event_rx,
            background_navigation_completion_tx,
            background_navigation_completion_rx,
            background_navigation_scheduler_enabled: false,
            background_navigation_gate: HashSet::new(),
            pending_navigation_renderer_publications: VecDeque::new(),
            blocked_navigation_renderer_streams: HashSet::new(),
        }
    }

    /// Enables the same asynchronous navigation channels used by the socket
    /// scheduler.
    ///
    /// Most protocol unit tests intentionally dispatch a domain command to
    /// completion without owning an actor. Chromium-ordering and lifecycle
    /// tests must opt into this production boundary: Page.navigate emits its
    /// start/early response first, while the later renderer Page commit arrives
    /// independently and is joined by its exact concrete-output cursor.
    pub(crate) fn enable_background_navigation_scheduler_for_test(&mut self) {
        if self.background_navigation_scheduler_enabled {
            return;
        }
        self.conn
            .set_background_event_sender(self.background_event_tx.clone());
        self.conn.set_background_navigation_completion_sender(
            self.background_navigation_completion_tx.clone(),
        );
        self.background_navigation_scheduler_enabled = true;
    }

    /// Registers an inactive BrowserContext through the same Core-owned
    /// topology transaction as production while retaining a stable unrelated
    /// selected Context for assertions about non-promotion.
    pub(crate) fn insert_inactive_browser_context_fixture(
        &mut self,
        browser_context: crate::conn::BrowserContext,
    ) {
        let browser_context_id = browser_context.id.clone();
        if self.conn.browser_context.is_none() {
            self.conn
                .insert_browser_context(crate::conn::BrowserContext::new(
                    Self::INACTIVE_FIXTURE_SELECTED_CONTEXT_ID.to_owned(),
                ));
        }
        self.conn.insert_browser_context(browser_context);
        assert!(
            self.conn
                .inactive_browser_contexts
                .iter()
                .any(|context| context.id == browser_context_id),
            "fixture BrowserContext must remain inactive after Core registration"
        );
    }

    pub(crate) fn inactive_browser_context_fixture_remains_unselected(&self) -> bool {
        self.conn
            .browser_context
            .as_ref()
            .is_some_and(|context| context.id == Self::INACTIVE_FIXTURE_SELECTED_CONTEXT_ID)
    }

    /// Loads and installs one production-shaped navigation fixture for the
    /// exact Page target addressed by `session_id`.
    ///
    /// Tests that expect owner-produced lifecycle or child-frame output must
    /// not insert a bare `Page` into the runtime slot: production installs the
    /// renderer Page and its exact root-Document lifecycle binding together.
    /// Keeping that invariant in one helper prevents protocol tests from
    /// accidentally depending on a state that the real navigation path never
    /// exposes.
    pub(crate) async fn install_navigation_fixture_for_session_owner(
        &mut self,
        raw_url: &str,
        session_id: Option<&str>,
    ) {
        self.conn.adopt_direct_browser_context_fixture_attachments();
        let navigation = self
            .conn
            .load_navigation_via_runtime_for_session_owner_async(session_id, raw_url)
            .await
            .expect("navigation fixture should load");
        self.install_loaded_navigation_fixture_for_session_owner(navigation, session_id)
            .await;
    }

    /// Installs an in-memory response through the same target/Page ownership
    /// transaction as a real navigation.
    ///
    /// Tests that need a non-fetchable origin (for example an IndexedDB
    /// fixture at `https://example.test`) use this entry point. Building the
    /// renderer Page first and assigning it directly to `TargetRuntimeSlot`
    /// skips the concrete-output owner binding and is not a state production
    /// can expose.
    pub(crate) async fn install_buffered_navigation_fixture_for_session_owner(
        &mut self,
        requested_url: url::Url,
        response_body: String,
        session_id: Option<&str>,
    ) {
        let navigation = self
            .conn
            .build_loaded_navigation_from_buffered_response_for_session_owner_async(
                session_id,
                requested_url,
                "GET".into(),
                Vec::new(),
                200,
                Vec::new(),
                response_body,
            )
            .await
            .expect("buffered navigation fixture should build");
        self.install_loaded_navigation_fixture_for_session_owner(navigation, session_id)
            .await;
    }

    async fn install_loaded_navigation_fixture_for_session_owner(
        &mut self,
        mut navigation: crate::conn::LoadedNavigation,
        session_id: Option<&str>,
    ) {
        let (_, target_id) = self
            .conn
            .target_owner_identity_for_session(session_id)
            .expect("navigation fixture requires an installed browser context");
        let target_id = target_id.expect("navigation fixture requires an exact target");
        let renderer_output_predecessor = navigation.renderer_output_predecessor;
        let post_response_continuation = navigation
            .page
            .take_committed_document_post_response_continuation();
        let main_document_commit = navigation
            .main_document_commit
            .clone()
            .expect("navigation fixture must freeze its main Document commit");
        let navigation_token = self
            .conn
            .start_document_navigation_for_session_owner(
                session_id,
                main_document_commit.loader_id.clone(),
            )
            .expect("navigation fixture target must admit its Browser-owned navigation");
        let renderer_agent_candidate = self
            .conn
            .prepare_renderer_agent_candidate_for_session_owner(
                session_id,
                &navigation_token,
                &mut navigation.page,
            )
            .expect("navigation fixture must prepare its exact renderer attachment");
        let navigation_engine = navigation.navigation_engine.take();
        let page_creation_artifacts = navigation.page_creation_artifacts;
        let final_url = navigation.final_url;
        let replacement = self
            .conn
            .commit_loaded_page_replacement_for_session_owner_async(
                session_id,
                &navigation_token,
                navigation.page,
                &final_url,
                &main_document_commit,
                LoadedNavigationRendererAttachmentCommit::Prepare(Some(renderer_agent_candidate)),
            )
            .await
            .expect("navigation fixture replacement owner must remain current")
            .expect("navigation fixture Page replacement must commit");
        let renderer_page = self
            .conn
            .renderer_page_residence_identity_for_session_owner(session_id)
            .expect("navigation fixture must install an exact renderer Page");
        let page_owner = self
            .conn
            .target_page_owner_key_for_session(session_id)
            .expect("navigation fixture must retain its exact target owner");
        assert_eq!(
            replacement.owner(),
            &page_owner,
            "navigation fixture replacement must preserve its exact target owner"
        );
        self.conn
            .bind_renderer_page_output_target(renderer_page, page_owner.clone());
        let (binding, _) = self
            .conn
            .bind_renderer_document_lifecycle_for_session_owner(
                session_id,
                page_creation_artifacts,
                Some(navigation_token),
                target_id.clone(),
                main_document_commit.loader_id.clone(),
            );
        let binding =
            binding.expect("navigation fixture must install its exact renderer Document binding");
        if let Some(navigation_engine) = navigation_engine {
            self.conn
                .adopt_loaded_navigation_engine_for_target_owner(page_owner, navigation_engine)
                .expect("navigation fixture engine owner must remain exact");
        }
        assert_eq!(
            self.conn
                .target_root_document_lifecycle_identity_for_session(session_id),
            Some(binding.renderer_document_identity()),
            "navigation fixture must retain its exact renderer Document binding"
        );
        let primary_session_id = self
            .conn
            .runtime_session_owner_primary_session_id(session_id)
            .or_else(|| session_id.map(str::to_owned));
        let finished = self
            .conn
            .finish_renderer_document_navigation_for_session_owner(
                session_id,
                binding
                    .navigation
                    .as_ref()
                    .expect("navigation fixture lifecycle binding must retain its token"),
            )
            .expect("navigation fixture renderer channel must finish its exact transition");
        assert!(
            finished
                .renderer_call_replacements
                .as_ref()
                .is_none_or(|replacements| replacements.is_empty()),
            "navigation setup fixture cannot own pending renderer command replays"
        );
        if !finished.released_output.is_empty() {
            let mut events = Vec::new();
            crate::domains::runtime::push_routed_renderer_runtime_inspector_message_batch_background_events(
                &mut self.conn,
                &mut events,
                finished.released_output,
                primary_session_id.as_deref(),
            );
            let mut work = VecDeque::new();
            self.route_protocol_events_like_scheduler(events, &mut work)
                .await;
            self.route_test_scheduler_work_queue(&mut work).await;
        }
        self.conn
            .clear_document_navigation_protocol_tail_for_session_owner_if_loader_matches(
                session_id,
                &binding.loader_id,
            );
        // The fixture has no frontend response to flush. Once the Page and its
        // output route are installed, release the same post-commit parser
        // continuation that production releases at its response boundary.
        // Otherwise DocumentCommit is visible but parser/DCL/load remain
        // parked forever and the fixture represents no reachable browser
        // state.
        if let Some(continuation) = post_response_continuation {
            continuation.release();
        }
        if let Some(predecessor) = renderer_output_predecessor {
            // Production does not expose a completed navigation response until
            // the Page-creation cursor has crossed ordered protocol ingress.
            // Mirror that boundary here so a later enable command observes
            // the already-loaded target tail instead of racing publications
            // that merely happen to remain queued in the test transport.
            self.route_renderer_output_predecessor_before_command_response(predecessor)
                .await;
        }
        let loader_id = binding.loader_id.clone();
        let description = format!("navigation fixture load for {target_id}/{loader_id}");
        self.wait_until_scheduler_state(&description, |conn| {
            conn.renderer_document_lifecycle_authoritative_state_for_session_owner(session_id)
                .is_some_and(|(current, snapshot)| {
                    current.frame_id == target_id
                        && current.loader_id == loader_id
                        && snapshot.load.is_some()
                })
        })
        .await;
    }

    /// Feed a JSON-serialisable message through the async CDP entrypoint and
    /// route the scheduler work directly requested by that command. Tests
    /// waiting for later external lifecycle input must use the event-wait
    /// helpers, which consume one renderer wake or runtime reply at a time.
    pub async fn process_async(&mut self, msg: impl serde::Serialize) {
        self.conn.adopt_direct_browser_context_fixture_attachments();
        let command = ParsedCdpCommand::from_serializable(msg)
            .expect("test message must be a valid serialisable CDP command");
        let command_id = command.request().id();
        let session_id = command.request().session_id().map(str::to_owned);
        let response_start = self.sent.len();
        Box::pin(self.process_parsed_command_like_scheduler(&command, true)).await;
        Box::pin(self.route_ready_test_command_response(command_id, response_start)).await;
        if self
            .conn
            .renderer_runtime_command_cause_for_frontend(session_id.as_deref(), command_id)
            .is_some()
            && !self
                .pending_runtime_deferred_replies
                .iter()
                .any(|pending| pending.command_id() == Some(command_id))
        {
            Box::pin(self.wait_for_test_command_response(command_id, response_start)).await;
        }
    }

    /// Dispatch one command and keep running the real scheduler inputs until
    /// that command's response is routed. This mirrors Chromium's synchronous
    /// DevTools test client without draining unrelated page work to idle.
    pub async fn process_and_wait_for_response_async(&mut self, msg: impl serde::Serialize) {
        self.conn.adopt_direct_browser_context_fixture_attachments();
        let command = ParsedCdpCommand::from_serializable(msg)
            .expect("test message must be a valid serialisable CDP command");
        let command_id = command.request().id();
        let response_start = self.sent.len();
        Box::pin(self.process_parsed_command_like_scheduler(&command, true)).await;
        Box::pin(self.wait_for_test_command_response(command_id, response_start)).await;
    }

    #[cfg(test)]
    pub(crate) fn enable_page_events_for_test(&mut self, session_id: Option<&str>) {
        assert!(
            self.conn
                .set_page_domain_enabled_for_session_owner(session_id, true),
            "test Page subscription requires a loaded target session owner"
        );
    }

    #[cfg(test)]
    pub(crate) fn enable_dom_events_for_test(&mut self, session_id: Option<&str>) {
        assert!(
            self.conn
                .with_target_devtools_session_state_for_session_mut(session_id, |state| {
                    state.dom_session_state.enabled = true;
                })
                .is_some(),
            "test DOM subscription requires a loaded target session owner"
        );
    }

    /// Wait for and remove one protocol message produced by a real scheduler
    /// input. It does not synthesize a capture or ask a command path to advance
    /// renderer work: it only routes renderer publications and deferred
    /// inspector replies already published by the production owner scheduler.
    #[cfg(test)]
    pub(crate) async fn wait_for_scheduler_message(
        &mut self,
        description: &str,
        mut matches: impl FnMut(&Value) -> bool,
    ) -> Value {
        let message = tokio::time::timeout(TEST_SCHEDULER_INPUT_TIMEOUT, async {
            loop {
                if let Some(position) = self.sent.iter().position(&mut matches) {
                    return self.sent.remove(position);
                }
                assert!(
                    matches!(
                        Box::pin(self.wait_for_one_test_scheduler_turn()).await,
                        TestSchedulerTurnOutcome::Processed(_)
                    ),
                    "test scheduler lost all external input while waiting for {description}"
                );
            }
        })
        .await;

        match message {
            Ok(message) => message,
            Err(_) => panic!(
                "timed out waiting for {description} from a real scheduler input; sent={:?}",
                self.sent
            ),
        }
    }

    /// Wait until routing real scheduler input makes connection state satisfy
    /// `predicate`. This observes owner-published wakes only; it does not
    /// synthesize a capture or ask protocol code to advance renderer lifecycle.
    #[cfg(test)]
    pub(crate) async fn wait_until_scheduler_state(
        &mut self,
        description: &str,
        predicate: impl Fn(&CdpConnection) -> bool,
    ) {
        let waited = tokio::time::timeout(TEST_SCHEDULER_INPUT_TIMEOUT, async {
            loop {
                if predicate(&self.conn) {
                    return;
                }
                assert!(
                    matches!(
                        Box::pin(self.wait_for_one_test_scheduler_turn()).await,
                        TestSchedulerTurnOutcome::Processed(_)
                    ),
                    "test scheduler lost all external input while waiting for {description}"
                );
            }
        })
        .await;

        if waited.is_err() {
            panic!(
                "timed out waiting for {description} from a real scheduler input; sent={:?}; diagnostics={}",
                self.sent,
                self.conn.moli_memory_diagnostics()
            );
        }
    }

    /// Waits for the one exact background navigation already admitted by the
    /// fixture to reach its terminal completion input.
    ///
    /// A committed Page residence is intentionally visible before retired
    /// Page disposal and renderer replay settle. Tests that issue a command to
    /// the successor Page must therefore wait on the navigation gate, not use
    /// the installed Page as an implicit completion signal.
    #[cfg(test)]
    pub(crate) async fn wait_for_only_background_navigation_gate_to_settle(
        &mut self,
        description: &str,
    ) {
        assert_eq!(
            self.background_navigation_gate.len(),
            1,
            "{description} requires one exact admitted background navigation gate"
        );
        let gate_key = self
            .background_navigation_gate
            .iter()
            .next()
            .expect("one background navigation gate")
            .clone();
        let waited = tokio::time::timeout(TEST_SCHEDULER_INPUT_TIMEOUT, async {
            while self.background_navigation_gate.contains(&gate_key) {
                assert!(
                    matches!(
                        Box::pin(self.wait_for_one_test_scheduler_turn()).await,
                        TestSchedulerTurnOutcome::Processed(_)
                    ),
                    "test scheduler lost all external input while waiting for {description}"
                );
            }
        })
        .await;

        if waited.is_err() {
            panic!(
                "timed out waiting for {description} terminal input; gate={gate_key:?}; sent={:?}; diagnostics={}",
                self.sent,
                self.conn.moli_memory_diagnostics()
            );
        }
    }

    /// Feed one command through the async CDP entrypoint without completing
    /// deferred protocol residences afterwards, returning the scheduler events
    /// that production would handle after the command response turn.
    ///
    /// Use this for protocol sequences that intentionally send another command
    /// before idle/runtime work gets a chance to run. The production socket
    /// actor has a client-turn boundary between command output and deferred
    /// CDP activity; eager test draining would otherwise make the test observe
    /// a stronger, more synchronous ordering than real clients get.
    #[cfg(test)]
    pub(crate) async fn process_command_only_async(
        &mut self,
        msg: impl serde::Serialize,
    ) -> Vec<CdpSchedulerEvent> {
        let command = ParsedCdpCommand::from_serializable(msg)
            .expect("test message must be a valid serialisable CDP command");
        Box::pin(self.process_parsed_command_like_scheduler(&command, false)).await
    }

    /// Dispatch one command through the production response-flush boundary,
    /// but leave that boundary held so tests can inspect causally later page
    /// work before the wire response is considered flushed.
    #[cfg(test)]
    pub(crate) async fn process_command_holding_response_flush_for_test(
        &mut self,
        msg: impl serde::Serialize,
    ) -> TestCommandResponseFlushPermit {
        let command = ParsedCdpCommand::from_serializable(msg)
            .expect("test message must be a valid serialisable CDP command");
        let command_id = command.request().id();
        let response_start = self.sent.len();
        let (response_flush_permit, response_flush_context) =
            self.conn.begin_command_response_flush_permit();
        let mut command_context = CommandDispatchContext::new(response_flush_context);
        let step = self
            .conn
            .start_parsed_command_dispatch_with_context(&command, &mut command_context);
        let mut runtime_output_barrier = self.admit_runtime_output_barrier(&command);
        let mut protocol_events = Vec::new();
        let mut scheduler_events = Vec::new();
        let completed = Box::pin(self.complete_command_step_like_scheduler(
            step,
            &mut command_context,
            &mut protocol_events,
            &mut scheduler_events,
            &mut runtime_output_barrier,
        ))
        .await;
        assert!(
            completed,
            "held-flush test helper requires an immediate command response boundary"
        );
        protocol_events.extend(command_context.take_protocol_events());
        protocol_events.extend(command_context.take_post_response_events());
        scheduler_events.extend(self.conn.take_scheduler_events());
        Box::pin(self.route_test_scheduler_causal_batch(protocol_events, scheduler_events)).await;
        Box::pin(self.route_ready_test_command_response(command_id, response_start)).await;
        TestCommandResponseFlushPermit {
            response_flush: response_flush_permit,
            runtime_output_barrier,
        }
    }

    /// Releases a response boundary held by
    /// [`Self::process_command_holding_response_flush_for_test`].
    ///
    /// The response-flush permit becomes visible before command-owned
    /// after-response output, matching the production scheduler's
    /// `finish_command_dispatch_output_flush` ordering.
    pub(crate) async fn finish_held_command_response_flush_for_test(
        &mut self,
        permit: TestCommandResponseFlushPermit,
    ) {
        permit.response_flush.finish();
        if let Some(runtime_output_barrier) = permit.runtime_output_barrier {
            Box::pin(
                self.release_runtime_output_barrier_like_scheduler(runtime_output_barrier, true),
            )
            .await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn complete_command_task_step_for_test(
        &mut self,
        step: CdpCommandTaskStep,
    ) -> (Vec<Value>, Vec<CdpSchedulerEvent>) {
        let sent_start = self.sent.len();
        let command_id = match &step {
            CdpCommandTaskStep::Pending(pending) => pending.command_id(),
            CdpCommandTaskStep::Complete(_) => None,
        };
        let mut protocol_events = Vec::new();
        let mut scheduler_events = Vec::new();
        let mut runtime_output_barrier = None;
        let completed = Box::pin(self.complete_command_step_like_scheduler(
            step,
            &mut crate::conn::CommandDispatchContext::default(),
            &mut protocol_events,
            &mut scheduler_events,
            &mut runtime_output_barrier,
        ))
        .await;
        if completed {
            let mut messages = protocol_events_into_messages(protocol_events);
            if let Some(command_id) = command_id
                && !messages
                    .iter()
                    .any(|message| message.get("id").and_then(Value::as_u64) == Some(command_id))
            {
                // Synchronous renderer-owned Runtime commands publish their
                // terminal response through the DevTools-session output
                // transport. The command completion only proves that the
                // publication was committed; admit that independently
                // transported response just as `process_async` does, while
                // leaving sibling renderer events in the scheduler queue.
                Box::pin(self.route_ready_test_command_response(command_id, sent_start)).await;
                if let Some(position) = self.sent.get(sent_start..).and_then(|sent| {
                    sent.iter()
                        .position(|message| {
                            message.get("id").and_then(Value::as_u64) == Some(command_id)
                        })
                        .map(|position| sent_start + position)
                }) {
                    messages.push(self.sent.remove(position));
                }
            }
            return (messages, scheduler_events);
        }

        Box::pin(self.route_test_scheduler_causal_batch(protocol_events, scheduler_events)).await;
        if !self.pending_runtime_deferred_replies.is_empty() {
            let _ = Box::pin(self.run_one_ready_test_scheduler_turn()).await;
        }
        let messages = self.sent.drain(sent_start..).collect();
        (messages, Vec::new())
    }

    /// Routes the output of one direct protocol-neutral command through the
    /// stateful test scheduler.
    ///
    /// Direct `DevToolsCommand` fixtures bypass parsed CDP dispatch but can
    /// still publish protocol work whose exact owner action is not ready yet.
    /// Keeping that work resident here mirrors the production scheduler;
    /// callers must use the scheduler wait helpers for later renderer input.
    #[cfg(test)]
    pub(crate) async fn route_direct_command_output_for_test(
        &mut self,
        protocol_events: Vec<BackgroundProtocolEvent>,
        scheduler_events: Vec<CdpSchedulerEvent>,
    ) {
        Box::pin(self.route_test_scheduler_causal_batch(protocol_events, scheduler_events)).await;
    }

    /// Waits until every concrete scheduler work item published by a direct
    /// command has completed.
    ///
    /// This is the protocol-neutral counterpart of waiting for a CDP event:
    /// WebDriver commands can require the same owner action without enabling a
    /// CDP domain, so their tests must synchronize with the work residence
    /// itself rather than manufacture a frontend subscription.
    #[cfg(test)]
    pub(crate) async fn wait_for_direct_command_work_completion_for_test(
        &mut self,
        description: &str,
    ) {
        let waited = tokio::time::timeout(TEST_SCHEDULER_INPUT_TIMEOUT, async {
            while self.browser_host_actor.has_ready_input()
                || !self.pending_protocol_scheduler_work.is_empty()
            {
                assert!(
                    matches!(
                        Box::pin(self.wait_for_one_test_scheduler_turn()).await,
                        TestSchedulerTurnOutcome::Processed(_)
                    ),
                    "test scheduler lost all external input while waiting for {description}"
                );
            }
        })
        .await;
        if waited.is_err() {
            panic!(
                "timed out waiting for {description}; pending Browser Owner inputs={}; pending protocol work={:?}",
                self.browser_host_actor.ready_len(),
                self.pending_protocol_scheduler_work,
            );
        }
    }

    async fn process_parsed_command_like_scheduler(
        &mut self,
        command: &ParsedCdpCommand,
        drain_after_command: bool,
    ) -> Vec<CdpSchedulerEvent> {
        let browser_owner_ready_before = self.browser_host_actor.ready_len();
        let output_session_id = command.command_output_session_id().map(str::to_owned);
        let mut protocol_events = Vec::new();
        let mut scheduler_events = Vec::new();
        let mut command_context = crate::conn::CommandDispatchContext::default();
        let step = self
            .conn
            .start_parsed_command_dispatch_with_context(command, &mut command_context);
        let mut runtime_output_barrier = self.admit_runtime_output_barrier(command);
        let completed = Box::pin(self.complete_command_step_like_scheduler(
            step,
            &mut command_context,
            &mut protocol_events,
            &mut scheduler_events,
            &mut runtime_output_barrier,
        ))
        .await;

        if drain_after_command && completed {
            crate::domains::activity::project_protocol_local_command_outputs(
                &mut self.conn,
                output_session_id.as_deref(),
                &mut command_context,
            )
            .await;
            protocol_events.extend(command_context.take_protocol_events());
            protocol_events.extend(command_context.take_post_response_events());
            scheduler_events.extend(self.conn.take_scheduler_events());
        }

        if drain_after_command {
            Box::pin(self.route_test_scheduler_causal_batch(protocol_events, scheduler_events))
                .await;
            Box::pin(
                self.route_new_browser_owner_inputs_after_frontend_turn(browser_owner_ready_before),
            )
            .await;
            if completed {
                if let Some(runtime_output_barrier) = runtime_output_barrier {
                    Box::pin(self.release_runtime_output_barrier_like_scheduler(
                        runtime_output_barrier,
                        true,
                    ))
                    .await;
                }
            } else {
                assert!(
                    runtime_output_barrier.is_none(),
                    "a pending Runtime command must transfer its output barrier to the pending reply"
                );
            }
            if !completed && !self.pending_runtime_deferred_replies.is_empty() {
                let _ = Box::pin(self.run_one_ready_test_scheduler_turn()).await;
            }
            Vec::new()
        } else {
            Box::pin(self.route_test_scheduler_causal_batch(protocol_events, Vec::new())).await;
            if completed {
                if let Some(runtime_output_barrier) = runtime_output_barrier {
                    let mut release_scheduler_events =
                        Box::pin(self.release_runtime_output_barrier_like_scheduler(
                            runtime_output_barrier,
                            false,
                        ))
                        .await;
                    scheduler_events.append(&mut release_scheduler_events);
                }
            } else {
                assert!(
                    runtime_output_barrier.is_none(),
                    "a pending Runtime command must transfer its output barrier to the pending reply"
                );
            }
            if !completed && !self.pending_runtime_deferred_replies.is_empty() {
                let _ = Box::pin(self.run_one_ready_test_scheduler_turn()).await;
            }
            scheduler_events
        }
    }

    async fn route_new_browser_owner_inputs_after_frontend_turn(&mut self, ready_before: usize) {
        let newly_ready = self
            .browser_host_actor
            .ready_len()
            .saturating_sub(ready_before);
        if newly_ready == 0 {
            return;
        }
        let mut work = VecDeque::with_capacity(newly_ready);
        for _ in 0..newly_ready {
            // Production receives mailbox wake independently of the frontend
            // completion. Route the response first, then preserve one later
            // concrete owner turn for each input published by that command.
            work.push_back(TestSchedulerWork::BrowserOwnerTurn);
        }
        Box::pin(self.route_test_scheduler_work_queue(&mut work)).await;
    }

    fn admit_runtime_output_barrier(
        &mut self,
        command: &ParsedCdpCommand,
    ) -> Option<RuntimeCommandOutputBarrierPermit> {
        command
            .runtime_command_executes_page_javascript()
            .then(|| {
                self.runtime_command_output_barriers.admit(
                    &self.conn,
                    command.request().id(),
                    command.command_output_session_id(),
                )
            })
            .flatten()
    }

    async fn complete_command_step_like_scheduler(
        &mut self,
        mut step: CdpCommandTaskStep,
        command_context: &mut CommandDispatchContext,
        protocol_events: &mut Vec<BackgroundProtocolEvent>,
        scheduler_events: &mut Vec<CdpSchedulerEvent>,
        runtime_output_barrier: &mut Option<RuntimeCommandOutputBarrierPermit>,
    ) -> bool {
        loop {
            match step {
                CdpCommandTaskStep::Complete(outcome) => {
                    let (
                        mut events,
                        mut post_renderer_output_events,
                        renderer_output_boundary,
                        mut post_response_events,
                        mut new_scheduler_events,
                        mut renderer_output_predecessor,
                    ) = outcome.into_renderer_owner_turn_parts();
                    if let Some(command_predecessor) =
                        command_context.take_renderer_output_predecessor()
                    {
                        command_predecessor
                            .merge_into_same_stream_tail(&mut renderer_output_predecessor);
                    }
                    if let Some(predecessor) = renderer_output_predecessor {
                        Box::pin(
                            self.route_renderer_output_predecessor_before_command_response(
                                predecessor,
                            ),
                        )
                        .await;
                    }
                    protocol_events.append(&mut events);
                    if let Some(renderer_output_boundary) = renderer_output_boundary {
                        // The production actor flushes the already-materialized
                        // prefix, admits the exact renderer publication, then
                        // continues with the suffix. Do the same here instead
                        // of letting the test harness's final batch flatten the
                        // independently transported commit in front of its
                        // Page.navigate/Fetch responses.
                        Box::pin(self.route_test_scheduler_causal_batch(
                            std::mem::take(protocol_events),
                            Vec::new(),
                        ))
                        .await;
                        Box::pin(
                            self.route_renderer_output_predecessor_before_command_response(
                                renderer_output_boundary,
                            ),
                        )
                        .await;
                    }
                    protocol_events.append(&mut post_renderer_output_events);
                    protocol_events.append(&mut post_response_events);
                    scheduler_events.append(&mut new_scheduler_events);
                    return true;
                }
                CdpCommandTaskStep::Pending(mut pending)
                    if pending.waits_for_scheduler_deferred_inspector_reply() =>
                {
                    let session_id = pending.session_id().map(str::to_owned);
                    protocol_events
                        .extend(pending.take_scheduler_deferred_inspector_reply_events());
                    crate::domains::activity::project_protocol_local_command_outputs(
                        &mut self.conn,
                        session_id.as_deref(),
                        command_context,
                    )
                    .await;
                    protocol_events.extend(command_context.take_protocol_events());
                    protocol_events.extend(command_context.take_post_response_events());
                    scheduler_events.extend(self.conn.take_scheduler_events());
                    self.enqueue_pending_runtime_deferred_reply(
                        *pending,
                        std::mem::take(command_context),
                        runtime_output_barrier.take(),
                    );
                    return false;
                }
                CdpCommandTaskStep::Pending(pending) => {
                    while self.browser_host_actor.has_ready_input() {
                        // Production polls Browser Host independently while a
                        // frontend command waits. In particular, Phase 4
                        // Page.navigate admission cannot resolve until the
                        // actor selects and starts its queued Browser command.
                        let mut owner_work = VecDeque::from([TestSchedulerWork::BrowserOwnerTurn]);
                        Box::pin(self.route_test_scheduler_work_queue(&mut owner_work)).await;
                    }
                    let completed = pending.wait().await;
                    step = self
                        .conn
                        .complete_pending_command_dispatch_with_context(completed, command_context)
                        .await;
                }
            }
        }
    }

    /// Project the exact concrete renderer output owned by a command before
    /// exposing that command's response.
    ///
    /// Production performs the same fence in
    /// `flush_renderer_publication_predecessor`: the command carries a cursor,
    /// while the publication itself arrives over the renderer transport. The
    /// test scheduler must consume that real transport input rather than
    /// rescanning renderer state or draining unrelated work. Merely removing
    /// the publication from the ordered transport is insufficient: async
    /// owner actions in that batch must have returned as well.
    async fn route_renderer_output_predecessor_before_command_response(
        &mut self,
        predecessor: RendererOutputFence,
    ) {
        Box::pin(self.route_renderer_output_predecessor(predecessor, false)).await;
    }

    async fn route_navigation_renderer_output_boundary(&mut self, boundary: RendererOutputFence) {
        Box::pin(self.route_renderer_output_predecessor(boundary, true)).await;
    }

    async fn route_renderer_output_predecessor(
        &mut self,
        predecessor: RendererOutputFence,
        releases_navigation_stream: bool,
    ) {
        let mut observed_transport = Vec::new();
        let cursor = predecessor.cursor();
        let predecessor_stream = cursor.stream();
        if releases_navigation_stream {
            self.blocked_navigation_renderer_streams
                .remove(&predecessor_stream);
        }
        let projected = tokio::time::timeout(TEST_SCHEDULER_INPUT_TIMEOUT, async {
            while !self.conn.renderer_output_cursor_is_projected(cursor) {
                // A command may address the replacement Page after Browser
                // Core has installed it but before the navigation's remaining
                // participants have published the renderer insertion
                // boundary. Production keeps driving the independent Browser
                // Owner lane in that interval. The stateful test scheduler
                // must do the same instead of waiting only on the renderer
                // transport while that exact stream is deliberately parked.
                if self
                    .blocked_navigation_renderer_streams
                    .contains(&predecessor_stream)
                    && matches!(
                        Box::pin(self.wait_for_one_test_scheduler_turn()).await,
                        TestSchedulerTurnOutcome::Processed(_)
                    )
                {
                    continue;
                }
                let buffered_position = self
                    .pending_navigation_renderer_publications
                    .iter()
                    .position(|publication| {
                        renderer_publication_stream(publication) == Some(predecessor_stream)
                            && !self
                                .blocked_navigation_renderer_streams
                                .contains(&predecessor_stream)
                    });
                let publication = match buffered_position {
                    Some(position) => self
                        .pending_navigation_renderer_publications
                        .remove(position)
                        .expect("buffered renderer publication position should remain valid"),
                    None => self
                        .renderer_publication_rx
                        .recv()
                        .await
                        .expect("renderer output transport closed before command predecessor"),
                };
                observed_transport.push(format!("{publication:?}"));
                let mut work = VecDeque::new();
                Box::pin(self.ingest_renderer_publication_with_navigation_release(
                    publication,
                    &mut work,
                    releases_navigation_stream.then_some(predecessor_stream),
                ))
                .await;
                Box::pin(self.route_test_scheduler_work_queue(&mut work)).await;
            }
        })
        .await;
        assert!(
            projected.is_ok(),
            "timed out waiting for renderer output predecessor {predecessor:?}; \
             observed transport={observed_transport:#?}"
        );
    }

    /// Admits one direct command's exact renderer fence through the production-
    /// shaped transport and ordered ingress.
    ///
    /// Protocol-neutral command tests do not have a parsed CDP response for
    /// `complete_command_step_like_scheduler()` to hold. They still must route
    /// the cursor rather than inspecting renderer state or manufacturing the
    /// owner action that the concrete publication contains.
    #[cfg(test)]
    pub(crate) async fn route_direct_command_renderer_predecessor_for_test(
        &mut self,
        predecessor: RendererOutputFence,
    ) {
        Box::pin(self.route_renderer_output_predecessor_before_command_response(predecessor)).await;
    }

    /// Completes one protocol-neutral command across the same concrete
    /// renderer-output boundary as the production actor.
    pub(crate) async fn execute_devtools_command_through_renderer_fence_for_test(
        &mut self,
        command: DevToolsCommand,
    ) -> Result<DevToolsCommandResult, DevToolsError> {
        let (result, scheduler_events, protocol_events, renderer_output_predecessor) = self
            .conn
            .execute_devtools_command(command)
            .await
            .into_complete_parts();
        if let Some(predecessor) = renderer_output_predecessor {
            self.route_direct_command_renderer_predecessor_for_test(predecessor)
                .await;
        }
        self.route_direct_command_output_for_test(protocol_events, scheduler_events)
            .await;
        result
    }

    /// Routes an explicitly completed command turn through the same ordered
    /// renderer boundary used by the production actor.
    ///
    /// A few ownership tests drive `PendingCdpCommandDispatch` directly so
    /// they can inspect its scheduler sidecars. They must still admit concrete
    /// renderer output at its exact position rather than flattening the
    /// outcome into a message-only vector.
    pub(crate) async fn route_completed_command_outcome_for_test(
        &mut self,
        outcome: impl Into<CdpRendererOwnerTurnOutcome>,
    ) -> (Vec<Value>, Vec<CdpSchedulerEvent>) {
        let sent_start = self.sent.len();
        let (
            before_renderer_output,
            post_renderer_output,
            renderer_output_boundary,
            post_response_events,
            scheduler_events,
            renderer_output_predecessor,
        ) = outcome.into().into_renderer_owner_turn_parts();
        if let Some(predecessor) = renderer_output_predecessor {
            Box::pin(self.route_renderer_output_predecessor_before_command_response(predecessor))
                .await;
        }
        Box::pin(self.route_test_scheduler_causal_batch(before_renderer_output, Vec::new())).await;
        if let Some(boundary) = renderer_output_boundary {
            Box::pin(self.route_renderer_output_predecessor_before_command_response(boundary))
                .await;
        } else {
            assert!(
                post_renderer_output.is_empty(),
                "post-renderer output requires an exact boundary"
            );
        }
        let mut suffix = post_renderer_output;
        suffix.extend(post_response_events);
        Box::pin(self.route_test_scheduler_causal_batch(suffix, Vec::new())).await;
        (self.sent.drain(sent_start..).collect(), scheduler_events)
    }

    async fn release_runtime_output_barrier_like_scheduler(
        &mut self,
        permit: RuntimeCommandOutputBarrierPermit,
        route_scheduler_events: bool,
    ) -> Vec<CdpSchedulerEvent> {
        let completion = self
            .conn
            .release_runtime_command_output_barrier_turn_async(
                &mut self.runtime_command_output_barriers,
                permit,
            )
            .await;
        let (protocol_events, scheduler_events) =
            completion.into_outcome().into_protocol_event_parts();
        if route_scheduler_events {
            Box::pin(self.route_test_scheduler_causal_batch(protocol_events, scheduler_events))
                .await;
            Vec::new()
        } else {
            Box::pin(self.route_test_scheduler_causal_batch(protocol_events, Vec::new())).await;
            scheduler_events
        }
    }

    fn enqueue_runtime_output_barrier_completion(
        &mut self,
        completion: RuntimeCommandOutputBarrierCompletion,
        work: &mut VecDeque<TestSchedulerWork>,
    ) {
        let (protocol_events, scheduler_events) =
            completion.into_outcome().into_protocol_event_parts();
        if !scheduler_events.is_empty() {
            work.push_front(TestSchedulerWork::SchedulerEvents(scheduler_events));
        }
        if !protocol_events.is_empty() {
            work.push_front(TestSchedulerWork::ProtocolEvents(protocol_events));
        }
    }

    async fn route_test_scheduler_causal_batch(
        &mut self,
        initial_events: Vec<BackgroundProtocolEvent>,
        initial_scheduler_events: Vec<CdpSchedulerEvent>,
    ) {
        // Route only output caused by the current scheduler input. Deferred
        // external inputs remain separate turns, matching the actor.
        let mut work = VecDeque::new();
        if !initial_events.is_empty() {
            work.push_back(TestSchedulerWork::ProtocolEvents(initial_events));
        }
        if !initial_scheduler_events.is_empty() {
            work.push_back(TestSchedulerWork::SchedulerEvents(initial_scheduler_events));
        }
        Box::pin(self.route_test_scheduler_work_queue(&mut work)).await;
    }

    /// Routes one explicit renderer publication through the same capture,
    /// barrier, and concrete-residence path as the production adapter.
    ///
    /// This helper is for boundary tests that manufacture a typed publication
    /// instead of receiving it from a live renderer. It does not provide a
    /// direct output drain or a broad source scan.
    pub(crate) async fn route_renderer_publication_for_test(
        &mut self,
        publication: RendererOutputTransportMessage,
    ) -> Vec<Value> {
        let sent_start = self.sent.len();
        let mut work = VecDeque::from([TestSchedulerWork::RendererPublication(publication)]);
        Box::pin(self.route_test_scheduler_work_queue(&mut work)).await;
        self.sent.drain(sent_start..).collect()
    }

    fn enqueue_pending_runtime_deferred_reply(
        &mut self,
        mut pending: PendingCdpCommandDispatch,
        command_context: CommandDispatchContext,
        runtime_output_barrier: Option<RuntimeCommandOutputBarrierPermit>,
    ) {
        if let Some(command_id) = pending.command_id()
            && let Some(response_rx) = pending.take_scheduler_deferred_inspector_reply_receiver()
        {
            let session_id = pending.session_id().map(str::to_owned);
            let response_tx = self
                .conn
                .runtime_inspector_response_ready_sender()
                .expect("test scheduler should install its typed runtime-response channel");
            tokio::spawn(async move {
                let response = response_rx
                    .await
                    .map_err(|_| "RuntimeDeferredInspectorResponseCanceled".to_owned());
                let _ = response_tx.send(RuntimeInspectorResponseReady::new(
                    command_id,
                    session_id.as_deref(),
                    response,
                ));
            });
        }
        self.pending_runtime_deferred_replies
            .push_back(PendingTestRuntimeDeferredReply::new(
                pending,
                command_context,
                runtime_output_barrier,
            ));
    }

    async fn route_test_scheduler_work_queue(&mut self, work: &mut VecDeque<TestSchedulerWork>) {
        self.route_ready_protocol_scheduler_work_for_test_context(work)
            .await;
        while let Some(item) = work.pop_front() {
            match item {
                TestSchedulerWork::BrowserOwnerTurn => {
                    let dispatch = self
                        .browser_host_actor
                        .complete_next_turn(&mut self.conn)
                        .expect("ready Browser Host marker must select one exact turn");
                    let outcome = self.conn.finish_browser_host_turn_for_test(dispatch).await;
                    let (
                        before_renderer_output,
                        mut post_renderer_output,
                        renderer_output_boundary,
                        mut post_response_events,
                        scheduler_events,
                        renderer_output_predecessor,
                    ) = outcome.into_renderer_owner_turn_parts();
                    if let Some(predecessor) = renderer_output_predecessor {
                        Box::pin(
                            self.route_renderer_output_predecessor_before_command_response(
                                predecessor,
                            ),
                        )
                        .await;
                    }
                    if !before_renderer_output.is_empty() {
                        Box::pin(
                            self.route_protocol_events_like_scheduler(before_renderer_output, work),
                        )
                        .await;
                    }
                    if let Some(boundary) = renderer_output_boundary {
                        Box::pin(self.route_navigation_renderer_output_boundary(boundary)).await;
                    } else {
                        assert!(
                            post_renderer_output.is_empty(),
                            "post-renderer output requires an exact boundary"
                        );
                    }
                    post_renderer_output.append(&mut post_response_events);
                    if !post_renderer_output.is_empty() {
                        work.push_back(TestSchedulerWork::ProtocolEvents(post_renderer_output));
                    }
                    if !scheduler_events.is_empty() {
                        work.push_back(TestSchedulerWork::SchedulerEvents(scheduler_events));
                    }
                }
                TestSchedulerWork::ProtocolEvents(events) => {
                    Box::pin(self.route_protocol_events_like_scheduler(events, work)).await;
                }
                TestSchedulerWork::SchedulerEvents(scheduler_events) => {
                    Box::pin(self.route_scheduler_events_for_test_context(scheduler_events, work))
                        .await;
                }
                TestSchedulerWork::BackgroundEvent(event) => {
                    Box::pin(self.route_protocol_events_like_scheduler(vec![event], work)).await;
                }
                TestSchedulerWork::BackgroundNavigationCompletion(completion) => {
                    Box::pin(
                        self.route_background_navigation_completion_like_scheduler(
                            completion, work,
                        ),
                    )
                    .await;
                }
                TestSchedulerWork::RuntimeDeferredReplyReady(response) => {
                    Box::pin(self.complete_runtime_response_ready_like_scheduler(
                        response,
                        Vec::new(),
                        work,
                    ))
                    .await;
                }
                TestSchedulerWork::RendererPublication(publication) => {
                    Box::pin(self.ingest_renderer_publication_like_scheduler(publication, work))
                        .await;
                }
                TestSchedulerWork::ReleaseRuntimeOutputBarrier(permit) => {
                    let completion = self
                        .conn
                        .release_runtime_command_output_barrier_turn_async(
                            &mut self.runtime_command_output_barriers,
                            permit,
                        )
                        .await;
                    self.enqueue_runtime_output_barrier_completion(completion, work);
                }
                TestSchedulerWork::CancelRuntimeOutputBarrier(permit) => {
                    let completion = self
                        .conn
                        .cancel_runtime_command_output_barrier_turn_async(
                            &mut self.runtime_command_output_barriers,
                            permit,
                        )
                        .await;
                    self.enqueue_runtime_output_barrier_completion(completion, work);
                }
            }
            self.route_ready_protocol_scheduler_work_for_test_context(work)
                .await;
        }
    }

    async fn route_background_navigation_completion_like_scheduler(
        &mut self,
        completion: crate::domains::page::BackgroundNavigationCompletion,
        work: &mut VecDeque<TestSchedulerWork>,
    ) {
        // Match the production actor's three-part boundary:
        //
        //   already-produced navigation output
        //   -> exact renderer Page cursor
        //   -> navigation commit output
        //
        // The event and renderer transports are independent, so flattening
        // them after the fact would allow the commit cursor to move the new
        // realm in front of frameStartedNavigating/Page.navigate's response.
        let mut prefix = Vec::new();
        while let Ok(event) = self.background_event_rx.try_recv() {
            prefix.push(event);
        }
        if !prefix.is_empty() {
            Box::pin(self.route_protocol_events_like_scheduler(prefix, work)).await;
        }

        let gate_key = completion.background_navigation_gate_key();
        let (outcome, disposition) = self
            .conn
            .drain_background_navigation_completion_turn_async(completion)
            .await;
        if disposition.is_terminal()
            && let Some(gate_key) = gate_key.as_ref()
        {
            self.background_navigation_gate.remove(gate_key);
        }
        let (
            mut completion_prefix,
            mut completion_suffix,
            renderer_output_boundary,
            mut post_response_events,
            scheduler_events,
            renderer_output_predecessor,
        ) = outcome.into_renderer_owner_turn_parts();
        assert!(
            renderer_output_predecessor.is_none(),
            "background navigation completion must use an insertion boundary"
        );
        if !completion_prefix.is_empty() {
            Box::pin(self.route_protocol_events_like_scheduler(
                std::mem::take(&mut completion_prefix),
                work,
            ))
            .await;
        }
        if let Some(boundary) = renderer_output_boundary {
            Box::pin(self.route_navigation_renderer_output_boundary(boundary)).await;
        }
        completion_suffix.append(&mut post_response_events);
        while let Ok(event) = self.background_event_rx.try_recv() {
            completion_suffix.push(event);
        }
        if !completion_suffix.is_empty() {
            work.push_back(TestSchedulerWork::ProtocolEvents(completion_suffix));
        }
        if !scheduler_events.is_empty() {
            work.push_back(TestSchedulerWork::SchedulerEvents(scheduler_events));
        }
        if disposition.is_terminal() {
            if self.background_navigation_gate.is_empty() {
                self.blocked_navigation_renderer_streams.clear();
            }
            let mut retained = VecDeque::new();
            while let Some(publication) = self.pending_navigation_renderer_publications.pop_front()
            {
                let blocked = renderer_publication_stream(&publication).is_some_and(|stream| {
                    self.blocked_navigation_renderer_streams.contains(&stream)
                });
                if blocked {
                    retained.push_back(publication);
                } else {
                    work.push_back(TestSchedulerWork::RendererPublication(publication));
                }
            }
            self.pending_navigation_renderer_publications = retained;
        }
    }

    async fn run_one_ready_test_scheduler_turn(&mut self) -> TestSchedulerTurnOutcome {
        let mut work = VecDeque::new();
        let input_kind = if self.browser_host_actor.has_ready_input() {
            work.push_back(TestSchedulerWork::BrowserOwnerTurn);
            TestSchedulerInputKind::BrowserOwnerInput
        } else if self.background_navigation_scheduler_enabled
            && let Ok(completion) = self.background_navigation_completion_rx.try_recv()
        {
            work.push_back(TestSchedulerWork::BackgroundNavigationCompletion(
                completion,
            ));
            TestSchedulerInputKind::BackgroundNavigationCompletion
        } else if self.background_navigation_scheduler_enabled
            && let Ok(event) = self.background_event_rx.try_recv()
        {
            work.push_back(TestSchedulerWork::BackgroundEvent(event));
            TestSchedulerInputKind::BackgroundEvent
        } else if !self.pending_runtime_deferred_replies.is_empty() {
            match self.runtime_inspector_response_ready_rx.try_recv() {
                Ok(response) => {
                    work.push_back(TestSchedulerWork::RuntimeDeferredReplyReady(response));
                    TestSchedulerInputKind::RuntimeDeferredReply
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    if self.background_navigation_gate.is_empty()
                        && let Ok(publication) = self.renderer_publication_rx.try_recv()
                    {
                        work.push_back(TestSchedulerWork::RendererPublication(publication));
                        TestSchedulerInputKind::RendererPublication
                    } else {
                        return TestSchedulerTurnOutcome::Idle;
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    return TestSchedulerTurnOutcome::Idle;
                }
            }
        } else if self.background_navigation_gate.is_empty()
            && let Ok(publication) = self.renderer_publication_rx.try_recv()
        {
            work.push_back(TestSchedulerWork::RendererPublication(publication));
            TestSchedulerInputKind::RendererPublication
        } else if let Ok(response) = self.runtime_inspector_response_ready_rx.try_recv() {
            work.push_back(TestSchedulerWork::RuntimeDeferredReplyReady(response));
            TestSchedulerInputKind::RuntimeDeferredReply
        } else {
            return TestSchedulerTurnOutcome::Idle;
        };
        Box::pin(self.route_test_scheduler_work_queue(&mut work)).await;
        TestSchedulerTurnOutcome::Processed(input_kind)
    }

    pub(crate) async fn wait_for_test_command_response(
        &mut self,
        command_id: u64,
        response_start: usize,
    ) {
        let response = tokio::time::timeout(TEST_SCHEDULER_INPUT_TIMEOUT, async {
            loop {
                if self
                    .sent
                    .get(response_start..)
                    .unwrap_or_default()
                    .iter()
                    .any(|message| message.get("id").and_then(Value::as_u64) == Some(command_id))
                {
                    return;
                }
                assert!(
                    matches!(
                        Box::pin(self.wait_for_one_test_scheduler_turn()).await,
                        TestSchedulerTurnOutcome::Processed(_)
                    ),
                    "CDP command `{command_id}` lost all scheduler input before its response"
                );
            }
        })
        .await;
        if response.is_err() {
            panic!(
                "timed out waiting for CDP command `{command_id}` response; sent={:?}; diagnostics={}",
                self.sent,
                self.conn.moli_memory_diagnostics()
            );
        }
    }

    async fn route_ready_test_command_response(&mut self, command_id: u64, response_start: usize) {
        while !self
            .sent
            .get(response_start..)
            .unwrap_or_default()
            .iter()
            .any(|message| message.get("id").and_then(Value::as_u64) == Some(command_id))
            && matches!(
                Box::pin(self.run_one_ready_test_scheduler_turn()).await,
                TestSchedulerTurnOutcome::Processed(_)
            )
        {}
    }

    async fn wait_for_one_test_scheduler_turn(&mut self) -> TestSchedulerTurnOutcome {
        let ready = Box::pin(self.run_one_ready_test_scheduler_turn()).await;
        if matches!(ready, TestSchedulerTurnOutcome::Processed(_)) {
            return ready;
        }

        let mut work = VecDeque::new();
        let background_navigation_scheduler_enabled = self.background_navigation_scheduler_enabled;
        let navigation_gate_open = !self.background_navigation_gate.is_empty();
        let input_kind = if !self.pending_runtime_deferred_replies.is_empty() {
            tokio::select! {
                biased;
                maybe_response = self.runtime_inspector_response_ready_rx.recv() => {
                    let Some(response) = maybe_response else {
                        return TestSchedulerTurnOutcome::Idle;
                    };
                    work.push_back(TestSchedulerWork::RuntimeDeferredReplyReady(response));
                    TestSchedulerInputKind::RuntimeDeferredReply
                }
                maybe_completion = self.background_navigation_completion_rx.recv(), if background_navigation_scheduler_enabled => {
                    let Some(completion) = maybe_completion else {
                        return TestSchedulerTurnOutcome::Idle;
                    };
                    work.push_back(TestSchedulerWork::BackgroundNavigationCompletion(completion));
                    TestSchedulerInputKind::BackgroundNavigationCompletion
                }
                maybe_event = self.background_event_rx.recv(), if background_navigation_scheduler_enabled => {
                    let Some(event) = maybe_event else {
                        return TestSchedulerTurnOutcome::Idle;
                    };
                    work.push_back(TestSchedulerWork::BackgroundEvent(event));
                    TestSchedulerInputKind::BackgroundEvent
                }
                maybe_publication = self.renderer_publication_rx.recv(), if !navigation_gate_open => {
                    let Some(publication) = maybe_publication else {
                        return TestSchedulerTurnOutcome::Idle;
                    };
                    work.push_back(TestSchedulerWork::RendererPublication(publication));
                    TestSchedulerInputKind::RendererPublication
                }
            }
        } else {
            tokio::select! {
                biased;
                maybe_completion = self.background_navigation_completion_rx.recv(), if background_navigation_scheduler_enabled => {
                    let Some(completion) = maybe_completion else {
                        return TestSchedulerTurnOutcome::Idle;
                    };
                    work.push_back(TestSchedulerWork::BackgroundNavigationCompletion(completion));
                    TestSchedulerInputKind::BackgroundNavigationCompletion
                }
                maybe_event = self.background_event_rx.recv(), if background_navigation_scheduler_enabled => {
                    let Some(event) = maybe_event else {
                        return TestSchedulerTurnOutcome::Idle;
                    };
                    work.push_back(TestSchedulerWork::BackgroundEvent(event));
                    TestSchedulerInputKind::BackgroundEvent
                }
                maybe_publication = self.renderer_publication_rx.recv(), if !navigation_gate_open => {
                    let Some(publication) = maybe_publication else {
                        return TestSchedulerTurnOutcome::Idle;
                    };
                    work.push_back(TestSchedulerWork::RendererPublication(publication));
                    TestSchedulerInputKind::RendererPublication
                }
                maybe_response = self.runtime_inspector_response_ready_rx.recv() => {
                    let Some(response) = maybe_response else {
                        return TestSchedulerTurnOutcome::Idle;
                    };
                    work.push_back(TestSchedulerWork::RuntimeDeferredReplyReady(response));
                    TestSchedulerInputKind::RuntimeDeferredReply
                }
            }
        };
        Box::pin(self.route_test_scheduler_work_queue(&mut work)).await;
        TestSchedulerTurnOutcome::Processed(input_kind)
    }

    async fn route_protocol_events_like_scheduler(
        &mut self,
        mut events: Vec<BackgroundProtocolEvent>,
        work: &mut VecDeque<TestSchedulerWork>,
    ) {
        loop {
            if events.is_empty() {
                return;
            }
            if let Some(position) = events
                .iter()
                .position(|event| event.as_runtime_inspector_response_ready().is_some())
            {
                self.sent.extend(
                    events
                        .drain(..position)
                        .map(BackgroundProtocolEvent::into_protocol_message),
                );
                let response = events
                    .remove(0)
                    .take_runtime_inspector_response_ready()
                    .expect("runtime response event position should contain typed response");
                Box::pin(
                    self.complete_runtime_response_ready_like_scheduler(response, events, work),
                )
                .await;
                return;
            }
            let pending_ids = self.pending_runtime_deferred_reply_command_ids();
            if pending_ids.is_empty() {
                self.sent.extend(
                    events
                        .into_iter()
                        .map(BackgroundProtocolEvent::into_protocol_message),
                );
                return;
            }
            let Some(position) = events.iter().position(|event| {
                event
                    .protocol_message()
                    .and_then(|message| message.get("id"))
                    .and_then(Value::as_u64)
                    .is_some_and(|id| pending_ids.contains(&id))
            }) else {
                self.sent.extend(
                    events
                        .into_iter()
                        .map(BackgroundProtocolEvent::into_protocol_message),
                );
                return;
            };
            self.sent.extend(
                events
                    .drain(..position)
                    .map(BackgroundProtocolEvent::into_protocol_message),
            );
            let message = events.remove(0).into_protocol_message();
            let Some(command_id) = message.get("id").and_then(Value::as_u64) else {
                self.sent.push(message);
                continue;
            };
            let Some(index) = self
                .pending_runtime_deferred_replies
                .iter()
                .position(|pending| pending.command_id() == Some(command_id))
            else {
                self.sent.push(message);
                continue;
            };
            let mut pending = self
                .pending_runtime_deferred_replies
                .remove(index)
                .expect("pending runtime deferred reply index should exist");
            pending
                .pending
                .forget_scheduler_deferred_inspector_reply(&mut self.conn);
            self.sent.push(json!({
                "id": command_id,
                "error": {
                    "code": -32000,
                    "message": "RuntimeDeferredReplyLooseProtocolResponse",
                },
            }));
            if let Some(runtime_output_barrier) = pending.runtime_output_barrier.take() {
                work.push_front(TestSchedulerWork::CancelRuntimeOutputBarrier(
                    runtime_output_barrier,
                ));
            }
            if !events.is_empty() {
                work.push_front(TestSchedulerWork::ProtocolEvents(events));
            }
            return;
        }
    }

    async fn route_scheduler_events_for_test_context(
        &mut self,
        scheduler_events: Vec<CdpSchedulerEvent>,
        work: &mut VecDeque<TestSchedulerWork>,
    ) {
        for event in scheduler_events {
            match event {
                CdpSchedulerEvent::ProtocolWorkPublished { work } => {
                    self.pending_protocol_scheduler_work.push_back(work);
                }
                CdpSchedulerEvent::BackgroundNavigationStarted { key, .. } => {
                    self.background_navigation_gate.insert(key);
                }
                CdpSchedulerEvent::PageScreencastStarted { .. } => {}
            }
        }
        let scheduler_events = self.conn.take_scheduler_events();
        if !scheduler_events.is_empty() {
            work.push_back(TestSchedulerWork::SchedulerEvents(scheduler_events));
        }
    }

    async fn route_ready_protocol_scheduler_work_for_test_context(
        &mut self,
        work: &mut VecDeque<TestSchedulerWork>,
    ) {
        if !self.background_navigation_gate.is_empty() {
            return;
        }
        while let Some(ready) = self
            .pending_protocol_scheduler_work
            .front()
            .map(ProtocolSchedulerWork::is_ready)
        {
            if !ready {
                return;
            }
            let protocol_work = self
                .pending_protocol_scheduler_work
                .pop_front()
                .expect("ready protocol work must remain resident");
            let browser_owner_ready_before = self.browser_host_actor.ready_len();
            let (events, scheduler_events) = self
                .conn
                .complete_ready_protocol_scheduler_work_turn(protocol_work)
                .await
                .into_protocol_event_parts();
            if !events.is_empty() {
                work.push_back(TestSchedulerWork::ProtocolEvents(events));
            }
            if !scheduler_events.is_empty() {
                work.push_back(TestSchedulerWork::SchedulerEvents(scheduler_events));
            }
            let newly_ready_browser_owner_inputs = self
                .browser_host_actor
                .ready_len()
                .saturating_sub(browser_owner_ready_before);
            for _ in 0..newly_ready_browser_owner_inputs {
                // Production receives the same mailbox wake independently of
                // protocol residence completion. Preserve that extra turn in
                // the stateful fixture instead of executing recursively here.
                work.push_back(TestSchedulerWork::BrowserOwnerTurn);
            }
        }
    }

    async fn ingest_renderer_publication_like_scheduler(
        &mut self,
        publication: RendererOutputTransportMessage,
        work: &mut VecDeque<TestSchedulerWork>,
    ) {
        Box::pin(self.ingest_renderer_publication_with_navigation_release(publication, work, None))
            .await;
    }

    fn navigation_renderer_publication_must_wait(
        &mut self,
        publication: &RendererOutputTransportMessage,
        released_navigation_stream: Option<RendererOutputStreamIdentity>,
    ) -> bool {
        let Some(stream) = renderer_publication_stream(publication) else {
            return false;
        };
        if released_navigation_stream == Some(stream) {
            self.blocked_navigation_renderer_streams.remove(&stream);
            return false;
        }
        if self.background_navigation_gate.is_empty() {
            return false;
        }
        if renderer_main_document_commit_stream(publication) == Some(stream) {
            self.blocked_navigation_renderer_streams.insert(stream);
        }
        self.blocked_navigation_renderer_streams.contains(&stream)
    }

    async fn ingest_renderer_publication_with_navigation_release(
        &mut self,
        publication: RendererOutputTransportMessage,
        work: &mut VecDeque<TestSchedulerWork>,
        released_navigation_stream: Option<RendererOutputStreamIdentity>,
    ) {
        let scheduler_events = self.conn.take_scheduler_events();
        if !scheduler_events.is_empty() {
            if let Some(released_navigation_stream) = released_navigation_stream {
                Box::pin(self.route_scheduler_events_for_test_context(scheduler_events, work))
                    .await;
                Box::pin(self.route_ready_protocol_scheduler_work_for_test_context(work)).await;
                Box::pin(self.ingest_renderer_publication_with_navigation_release(
                    publication,
                    work,
                    Some(released_navigation_stream),
                ))
                .await;
                return;
            }
            work.push_front(TestSchedulerWork::RendererPublication(publication));
            work.push_front(TestSchedulerWork::SchedulerEvents(scheduler_events));
            return;
        }
        // A prepared Document can publish its commit facts before the
        // independently transported terminal navigation participant arrives.
        // Preserve the concrete transport record until Browser state commits;
        // projecting it against the previous frame/loader would discard the
        // exact MainDocumentCommit observation.
        if self.navigation_renderer_publication_must_wait(&publication, released_navigation_stream)
        {
            self.pending_navigation_renderer_publications
                .push_back(publication);
            return;
        }
        let outcome = self
            .conn
            .ingest_renderer_output_turn_async(
                publication,
                &mut self.runtime_command_output_barriers,
            )
            .await;
        let (protocol_events, scheduler_events) = outcome.into_protocol_event_parts();
        if !protocol_events.is_empty() {
            work.push_back(TestSchedulerWork::ProtocolEvents(protocol_events));
        }
        if !scheduler_events.is_empty() {
            work.push_back(TestSchedulerWork::SchedulerEvents(scheduler_events));
        }
    }

    fn pending_runtime_deferred_reply_command_ids(&self) -> Vec<u64> {
        self.pending_runtime_deferred_replies
            .iter()
            .filter_map(PendingTestRuntimeDeferredReply::command_id)
            .collect()
    }

    async fn complete_runtime_response_ready_like_scheduler(
        &mut self,
        response: RuntimeInspectorResponseReady,
        suffix_events: Vec<BackgroundProtocolEvent>,
        work: &mut VecDeque<TestSchedulerWork>,
    ) {
        // Production admits an exact renderer cursor before resolving the
        // response correlation. Keep the test scheduler on the same boundary:
        // a SharedWorker `Destroyed` publication must terminate the pending
        // call with `Target closed` before a later V8 context-destruction
        // response can become visible.
        if let Some(predecessor) = response.renderer_output_predecessor() {
            Box::pin(self.route_renderer_output_predecessor_before_command_response(predecessor))
                .await;
        }
        let command_id = response.command_id();
        let Some(index) = self
            .pending_runtime_deferred_replies
            .iter()
            .position(|pending| pending.command_id() == Some(command_id))
        else {
            let mut response_events = Vec::new();
            let mut background_events = Vec::new();
            self.conn.route_registered_runtime_inspector_response_into(
                response,
                &mut response_events,
                &mut background_events,
            );
            if !background_events.is_empty() {
                work.push_front(TestSchedulerWork::ProtocolEvents(background_events));
            }
            self.sent.extend(
                response_events
                    .into_iter()
                    .map(BackgroundProtocolEvent::into_protocol_message),
            );
            if !suffix_events.is_empty() {
                work.push_front(TestSchedulerWork::ProtocolEvents(suffix_events));
            }
            return;
        };
        let pending = self
            .pending_runtime_deferred_replies
            .remove(index)
            .expect("pending runtime deferred reply index should exist");
        Box::pin(
            self.complete_renderer_runtime_deferred_response_like_scheduler(
                pending,
                response,
                suffix_events,
                work,
            ),
        )
        .await;
    }

    async fn complete_renderer_runtime_deferred_response_like_scheduler(
        &mut self,
        mut pending: PendingTestRuntimeDeferredReply,
        response: RuntimeInspectorResponseReady,
        suffix_events: Vec<BackgroundProtocolEvent>,
        work: &mut VecDeque<TestSchedulerWork>,
    ) {
        if pending.pending.command_id().is_none() {
            self.pending_runtime_deferred_replies.push_back(pending);
            return;
        }
        pending
            .pending
            .route_scheduler_deferred_inspector_response(&mut self.conn, response)
            .await;
        Box::pin(self.complete_runtime_deferred_reply_like_scheduler(pending, suffix_events, work))
            .await;
    }

    async fn complete_runtime_deferred_reply_like_scheduler(
        &mut self,
        mut pending: PendingTestRuntimeDeferredReply,
        suffix_events: Vec<BackgroundProtocolEvent>,
        work: &mut VecDeque<TestSchedulerWork>,
    ) {
        let completed = pending
            .pending
            .complete_scheduler_deferred_inspector_reply(&mut self.conn);
        let step = self
            .conn
            .complete_pending_command_dispatch_with_context(completed, &mut pending.command_context)
            .await;
        let mut protocol_events = Vec::new();
        let mut scheduler_events = Vec::new();
        let completed = Box::pin(self.complete_command_step_like_scheduler(
            step,
            &mut pending.command_context,
            &mut protocol_events,
            &mut scheduler_events,
            &mut pending.runtime_output_barrier,
        ))
        .await;
        if !suffix_events.is_empty() {
            work.push_front(TestSchedulerWork::ProtocolEvents(suffix_events));
        }
        if completed {
            if let Some(runtime_output_barrier) = pending.runtime_output_barrier {
                work.push_front(TestSchedulerWork::ReleaseRuntimeOutputBarrier(
                    runtime_output_barrier,
                ));
            }
        } else {
            assert!(
                pending.runtime_output_barrier.is_none(),
                "a still-pending Runtime command must transfer its output barrier"
            );
        }
        if !scheduler_events.is_empty() {
            work.push_front(TestSchedulerWork::SchedulerEvents(scheduler_events));
        }
        if !protocol_events.is_empty() {
            work.push_front(TestSchedulerWork::ProtocolEvents(protocol_events));
        }
    }

    // ── Assertion helpers ─────────────────────────────────────────────────────

    /// Assert that a `{id, result, sessionId?}` message exists in the sent
    /// queue and remove it.  `session_id = None` means "no sessionId field".
    pub fn expect_result(&mut self, id: u64, result: Value, session_id: Option<&str>) {
        let expected = build_result(id, &result, session_id);
        self.find_and_remove(&expected, "result");
    }

    /// Assert that a `{id, error: {code, message}}` message exists.
    pub fn expect_error(&mut self, id: u64, code: i32, message: &str) {
        let expected = json!({ "id": id, "error": { "code": code, "message": message } });
        self.find_and_remove(&expected, "error");
    }

    /// Assert that an event `{method, params, sessionId?}` message exists.
    /// When `params` is `None`, only the method name is checked.
    pub fn expect_event(&mut self, method: &str, params: Option<&Value>) {
        let pos = self.sent.iter().position(|v| {
            if v["method"].as_str() != Some(method) {
                return false;
            }
            if let Some(expected_params) = params {
                values_subset(expected_params, &v["params"])
            } else {
                true // any params
            }
        });
        match pos {
            Some(i) => {
                self.sent.remove(i);
            }
            None => {
                let queue: String = self.sent.iter().map(|v| format!("  {}\n", v)).collect();
                panic!("expected event '{method}' not found in sent queue:\n{queue}");
            }
        }
    }

    /// Take and return the next message; panics if the queue is empty.
    pub fn take_one(&mut self) -> Value {
        if self.sent.is_empty() {
            panic!("expected a message in the sent queue but it is empty");
        }
        self.sent.remove(0)
    }

    /// Take and return the first sent message matching the predicate.
    pub fn take_first_matching(
        &mut self,
        description: &str,
        matches: impl FnMut(&Value) -> bool,
    ) -> Value {
        let pos = self
            .sent
            .iter()
            .position(matches)
            .unwrap_or_else(|| panic!("expected {description} in sent queue: {:?}", self.sent));
        self.sent.remove(pos)
    }

    /// Take and return the response with the requested id.
    pub fn take_response_by_id(&mut self, id: u64) -> Value {
        let pos = self
            .sent
            .iter()
            .position(|message| message["id"] == json!(id))
            .unwrap_or_else(|| {
                panic!(
                    "expected a response with id {id} in sent queue: {:?}",
                    self.sent
                )
            });
        self.sent.remove(pos)
    }

    /// Drain all pending sent messages (useful to discard setup noise).
    pub fn take_all(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.sent)
    }

    /// Completes at most one scheduler input that is already ready.
    ///
    /// Unlike the removed broad test capture, this cannot snapshot an
    /// arbitrary session. It only consumes a concrete renderer publication, Runtime
    /// response, scheduler event, or ready `ProtocolSchedulerWork` already
    /// resident in the production-shaped harness.
    #[cfg(test)]
    pub(crate) async fn complete_one_ready_scheduler_input_for_test(&mut self) {
        let scheduler_events = self.conn.take_scheduler_events();
        Box::pin(self.route_test_scheduler_causal_batch(Vec::new(), scheduler_events)).await;
        let _ = Box::pin(self.run_one_ready_test_scheduler_turn()).await;
    }

    #[cfg(test)]
    pub(crate) fn browser_host_ready_len_for_test(&self) -> usize {
        self.browser_host_actor.ready_len()
    }

    #[cfg(test)]
    pub(crate) fn stop_browser_host_for_test(&mut self) {
        let (closed_replacement, replacement_handle) =
            BrowserHostActor::new(self.conn.browser_host_state());
        drop(replacement_handle);
        self.browser_host_actor = closed_replacement;
    }

    #[cfg(test)]
    pub(crate) fn start_one_ready_browser_host_turn_for_test(
        &mut self,
    ) -> crate::conn::BrowserHostTurnDispatch {
        self.browser_host_actor
            .complete_next_turn(&mut self.conn)
            .expect("test expected one ready Browser Host turn")
    }

    #[cfg(test)]
    pub(crate) async fn finish_browser_host_turn_for_test(
        &mut self,
        dispatch: crate::conn::BrowserHostTurnDispatch,
    ) -> CdpRendererOwnerTurnOutcome {
        self.conn.finish_browser_host_turn_for_test(dispatch).await
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    fn find_and_remove(&mut self, expected: &Value, kind: &str) {
        let pos = self.sent.iter().position(|v| values_subset(expected, v));
        match pos {
            Some(i) => {
                self.sent.remove(i);
            }
            None => {
                let queue: String = self.sent.iter().map(|v| format!("  {}\n", v)).collect();
                panic!("expected {kind} not found.\nExpected:\n  {expected}\nSent queue:\n{queue}");
            }
        }
    }
}

fn protocol_events_into_messages(events: Vec<BackgroundProtocolEvent>) -> Vec<Value> {
    events
        .into_iter()
        .map(BackgroundProtocolEvent::into_protocol_message)
        .collect()
}

#[cfg(test)]
pub(crate) fn protocol_events_into_internal_messages(
    events: Vec<BackgroundProtocolEvent>,
) -> Vec<Value> {
    events
        .into_iter()
        .map(|event| event.into_parts().0)
        .collect()
}

#[cfg(test)]
pub(crate) async fn drain_scheduler_events_like_scheduler(
    conn: &mut CdpConnection,
    out: &mut Vec<Value>,
    scheduler_events: Vec<CdpSchedulerEvent>,
) {
    drain_scheduler_events_like_scheduler_with_materializer(
        conn,
        out,
        scheduler_events,
        BackgroundProtocolEvent::into_protocol_message,
    )
    .await;
}

#[cfg(test)]
pub(crate) async fn drain_scheduler_events_like_scheduler_preserving_internal_fields(
    conn: &mut CdpConnection,
    out: &mut Vec<Value>,
    scheduler_events: Vec<CdpSchedulerEvent>,
) {
    drain_scheduler_events_like_scheduler_with_materializer(
        conn,
        out,
        scheduler_events,
        protocol_event_into_internal_message,
    )
    .await;
}

#[cfg(test)]
async fn drain_scheduler_events_like_scheduler_with_materializer(
    conn: &mut CdpConnection,
    out: &mut Vec<Value>,
    scheduler_events: Vec<CdpSchedulerEvent>,
    materialize_event: fn(BackgroundProtocolEvent) -> Value,
) {
    let mut queue = VecDeque::new();
    enqueue_scheduler_events_like_scheduler(&mut queue, scheduler_events);
    while let Some(deferred_work) = queue.pop_front() {
        let outcome = match deferred_work {
            TestDeferredSchedulerWork::Protocol(protocol_work) => {
                assert!(
                    protocol_work.is_ready(),
                    "the stateless compatibility materializer cannot own pending protocol work; use TestContext or the production CdpScheduler"
                );
                conn.complete_ready_protocol_scheduler_work_turn(protocol_work)
                    .await
            }
        };
        let (events, nested_scheduler_events) = outcome.into_protocol_event_parts();
        out.extend(events.into_iter().map(materialize_event));
        enqueue_scheduler_events_like_scheduler(&mut queue, nested_scheduler_events);
        enqueue_scheduler_events_like_scheduler(&mut queue, conn.take_scheduler_events());
    }
}

#[cfg(test)]
fn protocol_event_into_internal_message(event: BackgroundProtocolEvent) -> Value {
    event.into_parts().0
}

#[cfg(test)]
/// Compatibility materializer for protocol-domain fixtures.
///
/// It preserves scheduler-event FIFO and concrete work residence, but does not
/// model adapter client-turn predecessors. Tests that claim scheduling or
/// ordering behavior must use the production `CdpScheduler` instead.
enum TestDeferredSchedulerWork {
    Protocol(ProtocolSchedulerWork),
}

#[cfg(test)]
fn enqueue_scheduler_events_like_scheduler(
    queue: &mut VecDeque<TestDeferredSchedulerWork>,
    events: Vec<CdpSchedulerEvent>,
) {
    for event in events {
        match event {
            CdpSchedulerEvent::ProtocolWorkPublished { work } => {
                queue.push_back(TestDeferredSchedulerWork::Protocol(work));
            }
            CdpSchedulerEvent::BackgroundNavigationStarted { .. } => {}
            CdpSchedulerEvent::PageScreencastStarted { .. } => {}
        }
    }
}

#[cfg(test)]
pub(crate) trait TestSessionId<'a> {}

#[cfg(test)]
impl<'a> TestSessionId<'a> for Option<&'a str> {}

#[cfg(test)]
impl<'a> TestSessionId<'a> for &'a str {}

#[cfg(test)]
pub async fn spawn_connection_drop_server() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let _ = stream.shutdown().await;
    });
    (addr, server)
}

#[cfg(test)]
pub(crate) async fn wait_until_message(
    ctx: &mut TestContext,
    session_id: impl TestSessionId<'_> + Copy,
    description: &str,
    predicate: impl Fn(&Value) -> bool,
) {
    wait_until_messages(ctx, session_id, description, |messages| {
        messages.iter().any(&predicate)
    })
    .await;
}

#[cfg(test)]
pub(crate) async fn wait_until_messages(
    ctx: &mut TestContext,
    _session_id: impl TestSessionId<'_> + Copy,
    description: &str,
    predicate: impl Fn(&[Value]) -> bool,
) {
    for _ in 0..256 {
        if predicate(&ctx.sent) {
            return;
        }
        ctx.complete_one_ready_scheduler_input_for_test().await;
        if predicate(&ctx.sent) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    ctx.complete_one_ready_scheduler_input_for_test().await;
    if predicate(&ctx.sent) {
        return;
    }
    panic!("timed out waiting for {description}; sent={:?}", ctx.sent);
}

/// Wait for a protocol message produced by a real scheduler input without
/// synthesizing broad capture turns or moving messages already collected in
/// `ctx.sent`.
///
/// Resource and runtime completions publish typed wakes into `TestContext`.
/// Tests asserting those completion boundaries should block on that channel
/// instead of tying correctness to a polling iteration budget.
#[cfg(test)]
pub(crate) async fn wait_until_scheduler_message(
    ctx: &mut TestContext,
    description: &str,
    predicate: impl Fn(&Value) -> bool,
) {
    let waited = tokio::time::timeout(TEST_SCHEDULER_INPUT_TIMEOUT, async {
        loop {
            if ctx.sent.iter().any(&predicate) {
                return;
            }
            assert!(
                matches!(
                    Box::pin(ctx.wait_for_one_test_scheduler_turn()).await,
                    TestSchedulerTurnOutcome::Processed(_)
                ),
                "test scheduler lost all external input while waiting for {description}"
            );
        }
    })
    .await;

    if waited.is_err() {
        panic!(
            "timed out waiting for {description} from a real scheduler input; sent={:?}",
            ctx.sent
        );
    }
}

/// Wait for the terminal loading event of one concrete frame while preserving
/// the complete event sequence for assertions that follow.
///
/// `Page.navigate` only acknowledges that navigation was accepted; Chromium
/// likewise allows document replacement to continue after that response. DOM
/// tests must synchronize with the frame lifecycle before retaining frontend
/// node ids from the new document.
#[cfg(test)]
pub(crate) async fn wait_until_frame_stopped_loading(ctx: &mut TestContext, frame_id: &str) {
    let description = format!("Page.frameStoppedLoading for {frame_id}");
    wait_until_scheduler_message(ctx, &description, |message| {
        message["method"] == json!("Page.frameStoppedLoading")
            && message["params"]["frameId"] == json!(frame_id)
    })
    .await;
}

/// Wait for the renderer-owned load fact of one exact document generation.
///
/// Unlike `Page.frameStoppedLoading`, the authoritative binding carries the
/// loader id, so a test cannot accidentally accept a terminal event left by an
/// older document in the same frame.
#[cfg(test)]
pub(crate) async fn wait_until_renderer_document_load(
    ctx: &mut TestContext,
    session_id: Option<&str>,
    frame_id: &str,
    loader_id: &str,
) {
    let description = format!("renderer load for {frame_id}/{loader_id}");
    ctx.wait_until_scheduler_state(&description, |conn| {
        conn.renderer_document_lifecycle_authoritative_state_for_session_owner(session_id)
            .is_some_and(|(binding, snapshot)| {
                binding.frame_id == frame_id
                    && binding.loader_id == loader_id
                    && snapshot.load.is_some()
            })
    })
    .await;
}

/// Build a result message, omitting sessionId when it is None.
fn build_result(id: u64, result: &Value, session_id: Option<&str>) -> Value {
    let mut v = json!({ "id": id, "result": result });
    if let Some(sid) = session_id {
        v["sessionId"] = json!(sid);
    }
    v
}

/// Return true when every field of `expected` appears in `actual` with the
/// same value.  Arrays and nested objects are compared recursively.
fn values_subset(expected: &Value, actual: &Value) -> bool {
    match expected {
        Value::Object(exp_map) => {
            let Value::Object(act_map) = actual else {
                return false;
            };
            exp_map.iter().all(|(k, ev)| {
                act_map
                    .get(k)
                    .map(|av| values_subset(ev, av))
                    .unwrap_or(false)
            })
        }
        Value::Array(exp_arr) => {
            let Value::Array(act_arr) = actual else {
                return false;
            };
            if exp_arr.len() != act_arr.len() {
                return false;
            }
            exp_arr
                .iter()
                .zip(act_arr.iter())
                .all(|(e, a)| values_subset(e, a))
        }
        _ => expected == actual,
    }
}

#[cfg(test)]
mod tests {
    use moli_core::{PageId, RendererRuntimeInspectorAsyncCompletion};

    use super::*;

    #[tokio::test]
    async fn test_context_drops_unmatched_typed_runtime_response_like_scheduler() {
        let mut ctx = TestContext::new();
        let response = RuntimeInspectorResponseReady::new(
            42,
            None,
            Ok(
                RendererRuntimeInspectorAsyncCompletion::from_protocol_message(
                    42,
                    json!({
                        "id": 42,
                        "result": {}
                    }),
                ),
            ),
        );

        ctx.complete_runtime_response_ready_like_scheduler(
            response,
            Vec::new(),
            &mut VecDeque::new(),
        )
        .await;

        assert!(
            ctx.sent.is_empty(),
            "unmatched typed runtime completion must stay internal in the test harness too"
        );
    }

    #[tokio::test]
    async fn test_context_consumes_selected_task_output_publications_one_per_turn() {
        let mut conn = CdpConnection::new();
        let (publication_tx, publication_rx) = moli_core::renderer_output_transport_channel();
        let (runtime_response_tx, runtime_response_rx) = tokio::sync::mpsc::unbounded_channel();
        let (background_event_tx, background_event_rx) = browser_background_output_channel();
        let (background_navigation_completion_tx, background_navigation_completion_rx) =
            tokio::sync::mpsc::unbounded_channel();
        let (browser_host_actor, browser_host_handle) =
            BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(browser_host_handle);
        conn.set_renderer_publication_sender(publication_tx.clone());
        conn.set_runtime_inspector_response_ready_sender(runtime_response_tx);
        let mut ctx = TestContext {
            conn,
            sent: Vec::new(),
            pending_runtime_deferred_replies: VecDeque::new(),
            browser_host_actor,
            pending_protocol_scheduler_work: VecDeque::new(),
            runtime_command_output_barriers: RuntimeCommandOutputBarriers::default(),
            runtime_inspector_response_ready_rx: runtime_response_rx,
            renderer_publication_rx: publication_rx,
            background_event_tx,
            background_event_rx,
            background_navigation_completion_tx,
            background_navigation_completion_rx,
            background_navigation_scheduler_enabled: false,
            background_navigation_gate: HashSet::new(),
            pending_navigation_renderer_publications: VecDeque::new(),
            blocked_navigation_renderer_streams: HashSet::new(),
        };
        let opened = |page_id| {
            RendererOutputTransportMessage::from(RendererOutputStreamControl::Opened {
                stream: RendererOutputStreamIdentity::new_page_for_protocol_test(
                    PageId::new_for_testing(page_id),
                ),
            })
        };
        let first = opened(1);
        let second = opened(2);
        let second_stream = match &second {
            RendererOutputTransportMessage::StreamControl(
                RendererOutputStreamControl::Opened { stream },
            ) => *stream,
            RendererOutputTransportMessage::StreamControl(
                RendererOutputStreamControl::Closed { .. },
            )
            | RendererOutputTransportMessage::PageReservationReleased { .. }
            | RendererOutputTransportMessage::CursorLeaseDeclared { .. }
            | RendererOutputTransportMessage::CursorLeaseReleased { .. }
            | RendererOutputTransportMessage::Publication(_) => {
                unreachable!("test input is an opened stream control")
            }
        };
        publication_tx.send(first).expect("first scheduler input");
        publication_tx.send(second).expect("second scheduler input");

        assert_eq!(
            ctx.run_one_ready_test_scheduler_turn().await,
            TestSchedulerTurnOutcome::Processed(TestSchedulerInputKind::RendererPublication)
        );
        let queued = ctx
            .renderer_publication_rx
            .try_recv()
            .expect("second input must remain queued for the next turn");
        assert!(matches!(
            queued,
            RendererOutputTransportMessage::StreamControl(
                RendererOutputStreamControl::Opened { stream }
            ) if stream == second_stream
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn background_navigation_commit_participants_resume_as_separate_inputs() {
        let mut ctx = TestContext::new();
        tokio::task::LocalSet::new()
            .run_until(async {
                ctx.process_async(json!({
                    "id": 1,
                    "method": "Target.createTarget",
                    "params": { "url": "about:blank" }
                }))
                .await;
                let initial_attachment = ctx
                    .conn
                    .current_renderer_agent_attachment_id_for_session_owner(None)
                    .expect("initial target should expose its renderer attachment");
                let prepared_replay = ctx
                    .conn
                    .try_register_renderer_call_for_session_owner(
                        None,
                        70,
                        Some(initial_attachment),
                        RendererCommandDescriptor::from_synthesized_payload(
                            json!({
                                "id": 70,
                                "method": "Console.clearMessages",
                                "params": {},
                            })
                            .to_string(),
                        )
                        .expect("test replay command should be valid"),
                    )
                    .expect("test replay command should register");
                let (_, stale_replay_sender, replay_response_receiver) =
                    prepared_replay.into_parts();
                let replay_response_receiver = replay_response_receiver
                    .expect("command-reply replay should retain its local receiver");
                ctx.enable_background_navigation_scheduler_for_test();

                ctx.process_async(json!({
                    "id": 2,
                    "method": "Page.navigate",
                    "params": {
                        "url": "data:text/html,%3Cmain%3Estaged-background-commit%3C/main%3E"
                    }
                }))
                .await;

                let lifecycle = tokio::time::timeout(
                    TEST_SCHEDULER_INPUT_TIMEOUT,
                    ctx.background_navigation_completion_rx.recv(),
                )
                .await
                .expect("background navigation load should complete")
                .expect("background navigation channel should remain open");
                let replacement_engine_marker = "Moli/Page-Disposal-Adoption-Test";
                let replacement_engine = ctx
                    .conn
                    .navigation_engine_with_user_agent_marker_for_test(replacement_engine_marker);
                let lifecycle = lifecycle
                    .with_navigation_engine_for_test(replacement_engine)
                    .expect("fixture should replace the lifecycle engine");
                assert_eq!(lifecycle.kind(), "lifecycle");
                let gate_key = lifecycle
                    .background_navigation_gate_key()
                    .expect("lifecycle completion should retain its exact gate key");
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(lifecycle)
                    .await;
                assert!(
                    !disposition.is_terminal(),
                    "network completion must register commit configuration instead of draining inline"
                );

                let configured = tokio::time::timeout(
                    TEST_SCHEDULER_INPUT_TIMEOUT,
                    ctx.background_navigation_completion_rx.recv(),
                )
                .await
                .expect("prepared Document configuration should complete")
                .expect("background navigation channel should remain open");
                assert_eq!(configured.kind(), "participant");
                assert_eq!(
                    configured.background_navigation_gate_key().as_ref(),
                    Some(&gate_key)
                );
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(configured)
                    .await;
                assert!(
                    !disposition.is_terminal(),
                    "configuration completion must register renderer Document commit separately"
                );

                let committed = loop {
                    let completion = tokio::time::timeout(
                        TEST_SCHEDULER_INPUT_TIMEOUT,
                        ctx.background_navigation_completion_rx.recv(),
                    )
                    .await
                    .expect("prepared renderer Document commit should complete")
                    .expect("background navigation channel should remain open");
                    if completion.kind() == "main_document_body" {
                        let (_outcome, body_disposition) = ctx
                            .conn
                            .drain_background_navigation_completion_turn_async(completion)
                            .await;
                        assert!(body_disposition.is_terminal());
                        continue;
                    }
                    break completion;
                };
                assert_eq!(committed.kind(), "participant");
                assert_eq!(
                    committed.background_navigation_gate_key().as_ref(),
                    Some(&gate_key)
                );
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(committed)
                    .await;
                assert!(
                    !disposition.is_terminal(),
                    "renderer commit must keep the gate open while its Inspector replay runs"
                );
                assert_eq!(
                    ctx.conn
                        .active_navigation_engine_user_agent_for_test(),
                    replacement_engine_marker,
                    "exact Target engine adoption must commit before retired Page disposal waits"
                );
                assert!(
                    stale_replay_sender
                        .send(json!({ "id": 1, "result": {} }))
                        .is_err(),
                    "Document replacement must retire the old renderer response lease"
                );

                let disposed = loop {
                    let completion = tokio::time::timeout(
                        TEST_SCHEDULER_INPUT_TIMEOUT,
                        ctx.background_navigation_completion_rx.recv(),
                    )
                    .await
                    .expect("retired Page disposal should complete")
                    .expect("background navigation channel should remain open");
                    if completion.kind() == "main_document_body" {
                        let (_outcome, body_disposition) = ctx
                            .conn
                            .drain_background_navigation_completion_turn_async(completion)
                            .await;
                        assert!(body_disposition.is_terminal());
                        continue;
                    }
                    break completion;
                };
                assert_eq!(disposed.kind(), "participant");
                assert_eq!(
                    disposed.background_navigation_gate_key().as_ref(),
                    Some(&gate_key)
                );
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(disposed)
                    .await;
                assert!(
                    !disposition.is_terminal(),
                    "retired Page disposal must apply before the old renderer replay"
                );

                let replayed = loop {
                    let completion = tokio::time::timeout(
                        TEST_SCHEDULER_INPUT_TIMEOUT,
                        ctx.background_navigation_completion_rx.recv(),
                    )
                    .await
                    .expect("renderer Inspector replay should complete")
                    .expect("background navigation channel should remain open");
                    if completion.kind() == "main_document_body" {
                        let (_outcome, body_disposition) = ctx
                            .conn
                            .drain_background_navigation_completion_turn_async(completion)
                            .await;
                        assert!(body_disposition.is_terminal());
                        continue;
                    }
                    break completion;
                };
                assert_eq!(replayed.kind(), "participant");
                assert_eq!(
                    replayed.background_navigation_gate_key().as_ref(),
                    Some(&gate_key)
                );
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(replayed)
                    .await;
                assert!(
                    disposition.is_terminal(),
                    "the exact navigation gate may settle only after replay applies"
                );

                let replay_response = replay_response_receiver
                    .await
                    .expect("replay should complete the original frontend response lease");
                let response = replay_response
                    .output
                    .protocol_response(replay_response.call_id)
                    .expect("Console.clearMessages replay should produce a response");
                assert_eq!(response["result"], json!({}));
            })
            .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn superseded_generic_loaded_restore_cannot_install_stale_page() {
        let mut ctx = TestContext::new();
        tokio::task::LocalSet::new()
            .run_until(async {
                ctx.process_async(json!({
                    "id": 1,
                    "method": "Target.createTarget",
                    "params": { "url": "about:blank" }
                }))
                .await;
                let initial_attachment = ctx
                    .conn
                    .current_renderer_agent_attachment_id_for_session_owner(None)
                    .expect("initial target should expose its renderer attachment");
                ctx.enable_background_navigation_scheduler_for_test();

                ctx.process_async(json!({
                    "id": 2,
                    "method": "Page.navigate",
                    "params": {
                        "url": "data:text/html,%3Cmain%3Estale-restore%3C/main%3E"
                    }
                }))
                .await;
                let lifecycle = tokio::time::timeout(
                    TEST_SCHEDULER_INPUT_TIMEOUT,
                    ctx.background_navigation_completion_rx.recv(),
                )
                .await
                .expect("first background navigation should complete")
                .expect("background navigation channel should remain open")
                .commit_response_ready_as_loaded_for_test(&mut ctx.conn)
                .await
                .expect("fixture should expose a response-ready navigation");
                let first_gate_key = lifecycle
                    .background_navigation_gate_key()
                    .expect("first lifecycle completion should retain its exact gate key");
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(lifecycle)
                    .await;
                assert!(!disposition.is_terminal());
                assert_eq!(
                    ctx.conn
                        .current_renderer_agent_attachment_id_for_session_owner(None),
                    Some(initial_attachment)
                );

                // A later owner input may supersede this navigation while the
                // new Page is held exclusively by its move-owned restore wait.
                ctx.process_async(json!({
                    "id": 3,
                    "method": "Page.navigate",
                    "params": {
                        "url": "data:text/html,%3Cmain%3Esuccessor%3C/main%3E"
                    }
                }))
                .await;

                let mut unrelated_completions = Vec::new();
                let restored = loop {
                    let completion = tokio::time::timeout(
                        TEST_SCHEDULER_INPUT_TIMEOUT,
                        ctx.background_navigation_completion_rx.recv(),
                    )
                    .await
                    .expect("first Page restore should complete")
                    .expect("background navigation channel should remain open");
                    if completion.kind() == "main_document_body" {
                        let (_outcome, body_disposition) = ctx
                            .conn
                            .drain_background_navigation_completion_turn_async(completion)
                            .await;
                        assert!(body_disposition.is_terminal());
                        continue;
                    }
                    if completion.kind() == "participant"
                        && completion.background_navigation_gate_key().as_ref()
                            == Some(&first_gate_key)
                    {
                        break completion;
                    }
                    unrelated_completions.push(completion);
                };
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(restored)
                    .await;
                assert!(
                    !disposition.is_terminal(),
                    "a superseded restore must publish its rejected Page disposal"
                );
                assert_eq!(
                    ctx.conn
                        .current_renderer_agent_attachment_id_for_session_owner(None),
                    Some(initial_attachment),
                    "stale restore completion must not replace the current renderer attachment"
                );

                let disposed = loop {
                    let completion = tokio::time::timeout(
                        TEST_SCHEDULER_INPUT_TIMEOUT,
                        ctx.background_navigation_completion_rx.recv(),
                    )
                    .await
                    .expect("superseded Page disposal should complete")
                    .expect("background navigation channel should remain open");
                    if completion.kind() == "main_document_body" {
                        let (_outcome, body_disposition) = ctx
                            .conn
                            .drain_background_navigation_completion_turn_async(completion)
                            .await;
                        assert!(body_disposition.is_terminal());
                        continue;
                    }
                    if completion.kind() == "participant"
                        && completion.background_navigation_gate_key().as_ref()
                            == Some(&first_gate_key)
                    {
                        break completion;
                    }
                    unrelated_completions.push(completion);
                };
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(disposed)
                    .await;
                assert!(
                    disposition.is_terminal(),
                    "rejected Page disposal must settle without installing or replaying the stale Page"
                );
                assert_eq!(
                    ctx.conn
                        .current_renderer_agent_attachment_id_for_session_owner(None),
                    Some(initial_attachment),
                    "late stale Page disposal must not mutate the current renderer attachment"
                );
                drop(unrelated_completions);
            })
            .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn background_generic_loaded_navigation_tail_resumes_as_participant_input() {
        let mut ctx = TestContext::new();
        tokio::task::LocalSet::new()
            .run_until(async {
                ctx.process_async(json!({
                    "id": 1,
                    "method": "Target.createTarget",
                    "params": { "url": "about:blank" }
                }))
                .await;
                let initial_attachment = ctx
                    .conn
                    .current_renderer_agent_attachment_id_for_session_owner(None)
                    .expect("initial target should expose its renderer attachment");
                let prepared_replay = ctx
                    .conn
                    .try_register_renderer_call_for_session_owner(
                        None,
                        71,
                        Some(initial_attachment),
                        RendererCommandDescriptor::from_synthesized_payload(
                            json!({
                                "id": 71,
                                "method": "Console.clearMessages",
                                "params": {},
                            })
                            .to_string(),
                        )
                        .expect("test replay command should be valid"),
                    )
                    .expect("test replay command should register");
                let (_, stale_replay_sender, replay_response_receiver) =
                    prepared_replay.into_parts();
                let replay_response_receiver = replay_response_receiver
                    .expect("command-reply replay should retain its local receiver");
                ctx.enable_background_navigation_scheduler_for_test();

                ctx.process_async(json!({
                    "id": 2,
                    "method": "Page.navigate",
                    "params": {
                        "url": "data:text/html,%3Cmain%3Egeneric-loaded-tail%3C/main%3E"
                    }
                }))
                .await;

                let lifecycle = tokio::time::timeout(
                    TEST_SCHEDULER_INPUT_TIMEOUT,
                    ctx.background_navigation_completion_rx.recv(),
                )
                .await
                .expect("background navigation load should complete")
                .expect("background navigation channel should remain open")
                .commit_response_ready_as_loaded_for_test(&mut ctx.conn)
                .await
                .expect("fixture should expose a response-ready navigation");
                assert_eq!(lifecycle.kind(), "lifecycle");
                let gate_key = lifecycle
                    .background_navigation_gate_key()
                    .expect("lifecycle completion should retain its exact gate key");
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(lifecycle)
                    .await;
                assert!(
                    !disposition.is_terminal(),
                    "generic Loaded apply must publish its Page restore instead of draining it inline"
                );
                assert_eq!(
                    ctx.conn
                        .current_renderer_agent_attachment_id_for_session_owner(None),
                    Some(initial_attachment),
                    "the move-owned restore wait must not install the replacement Page"
                );

                let restored = loop {
                    let completion = tokio::time::timeout(
                        TEST_SCHEDULER_INPUT_TIMEOUT,
                        ctx.background_navigation_completion_rx.recv(),
                    )
                    .await
                    .expect("generic Loaded Page restore should complete")
                    .expect("background navigation channel should remain open");
                    if completion.kind() == "main_document_body" {
                        let (_outcome, body_disposition) = ctx
                            .conn
                            .drain_background_navigation_completion_turn_async(completion)
                            .await;
                        assert!(body_disposition.is_terminal());
                        continue;
                    }
                    break completion;
                };
                assert_eq!(restored.kind(), "participant");
                assert_eq!(
                    restored.background_navigation_gate_key().as_ref(),
                    Some(&gate_key)
                );
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(restored)
                    .await;
                assert!(
                    !disposition.is_terminal(),
                    "Page restore apply must publish retired Page disposal before settling the gate"
                );
                assert_ne!(
                    ctx.conn
                        .current_renderer_agent_attachment_id_for_session_owner(None),
                    Some(initial_attachment),
                    "the replacement Page must be installed only when restore completion applies"
                );
                assert!(
                    stale_replay_sender
                        .send(json!({ "id": 1, "result": {} }))
                        .is_err(),
                    "restore completion apply must retire the old renderer response lease"
                );

                let disposed = loop {
                    let completion = tokio::time::timeout(
                        TEST_SCHEDULER_INPUT_TIMEOUT,
                        ctx.background_navigation_completion_rx.recv(),
                    )
                    .await
                    .expect("generic Loaded retired Page disposal should complete")
                    .expect("background navigation channel should remain open");
                    if completion.kind() == "main_document_body" {
                        let (_outcome, body_disposition) = ctx
                            .conn
                            .drain_background_navigation_completion_turn_async(completion)
                            .await;
                        assert!(body_disposition.is_terminal());
                        continue;
                    }
                    break completion;
                };
                assert_eq!(disposed.kind(), "participant");
                assert_eq!(
                    disposed.background_navigation_gate_key().as_ref(),
                    Some(&gate_key)
                );
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(disposed)
                    .await;
                assert!(
                    !disposition.is_terminal(),
                    "generic Loaded Page disposal must apply before Inspector replay"
                );

                let replayed = loop {
                    let completion = tokio::time::timeout(
                        TEST_SCHEDULER_INPUT_TIMEOUT,
                        ctx.background_navigation_completion_rx.recv(),
                    )
                    .await
                    .expect("generic Loaded Inspector replay should complete")
                    .expect("background navigation channel should remain open");
                    if completion.kind() == "main_document_body" {
                        let (_outcome, body_disposition) = ctx
                            .conn
                            .drain_background_navigation_completion_turn_async(completion)
                            .await;
                        assert!(body_disposition.is_terminal());
                        continue;
                    }
                    break completion;
                };
                assert_eq!(replayed.kind(), "participant");
                assert_eq!(
                    replayed.background_navigation_gate_key().as_ref(),
                    Some(&gate_key)
                );
                let (_outcome, disposition) = ctx
                    .conn
                    .drain_background_navigation_completion_turn_async(replayed)
                    .await;
                assert!(
                    disposition.is_terminal(),
                    "the generic Loaded gate may settle only after its replay applies"
                );

                let replay_response = replay_response_receiver
                    .await
                    .expect("replay should complete the original frontend response lease");
                let response = replay_response
                    .output
                    .protocol_response(replay_response.call_id)
                    .expect("Console.clearMessages replay should produce a response");
                assert_eq!(response["result"], json!({}));
            })
            .await;
    }
}
