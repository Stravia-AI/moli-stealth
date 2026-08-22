use moli_core::{
    browser_host::{
        BrowserPageResidenceHandle, BrowserPageResidenceTransition,
        BrowserPageResidenceTransitionKind, BrowserPageRuntimeAccess, BrowserPageRuntimeLease,
    },
    page::{
        Page, RendererDocumentLifecycleEvent, RendererDocumentLifecycleEventKind,
        RendererDocumentLifecycleIdentity, RendererDocumentLifecycleMilestone,
        RendererDocumentLifecycleSnapshot, RendererDocumentToken, RendererFrameToken,
        RendererLifecycleEpoch, RendererLifecycleEventStamp, RendererLifecycleStartReason,
        RendererLifecycleTerminationStamp, RendererPageCreationArtifacts,
    },
};
use tokio::sync::watch;

use super::page_residence_token::{TargetPageResidencePublisher, TargetPageResidenceToken};
use super::{DocumentNavigationToken, RendererPageResidenceIdentity, TargetPageAttachmentId};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TargetPageAbsenceReason {
    #[default]
    NoTarget,
    InitialDocumentPageBuildPending,
    InitialDocumentPageBuildInProgress,
    NavigationFailed,
    TargetClosed,
    TargetCrashed,
    #[cfg(test)]
    TestFixture,
}

impl TargetPageAbsenceReason {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NoTarget => "no-target",
            Self::InitialDocumentPageBuildPending => "initial-document-page-build-pending",
            Self::InitialDocumentPageBuildInProgress => "initial-document-page-build-in-progress",
            Self::NavigationFailed => "navigation-failed",
            Self::TargetClosed => "target-closed",
            Self::TargetCrashed => "target-crashed",
            #[cfg(test)]
            Self::TestFixture => "test-fixture",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommittedRendererDocumentBinding {
    pub(crate) renderer_frame: RendererFrameToken,
    pub(crate) renderer_document: RendererDocumentToken,
    pub(crate) renderer_epoch: RendererLifecycleEpoch,
    pub(crate) navigation: Option<DocumentNavigationToken>,
    pub(crate) frame_id: String,
    pub(crate) loader_id: String,
    pub(crate) page_attachment_id: TargetPageAttachmentId,
    pub(crate) document_open_replacement_epoch: Option<RendererLifecycleEpoch>,
}

impl CommittedRendererDocumentBinding {
    pub(crate) fn renderer_document_identity(&self) -> RendererDocumentLifecycleIdentity {
        RendererDocumentLifecycleIdentity {
            frame: self.renderer_frame,
            document: self.renderer_document,
            epoch: self.renderer_epoch,
        }
    }
}

#[derive(Debug, Default)]
struct RendererDocumentLifecycleProtocolState {
    binding: Option<CommittedRendererDocumentBinding>,
    authoritative: RendererDocumentLifecycleProtocolCursor,
    visible: RendererDocumentLifecycleProtocolCursor,
    load_visibility: RendererDocumentLoadVisibility,
}

#[derive(Clone, Copy, Debug, Default)]
struct RendererDocumentLifecycleProtocolCursor {
    snapshot: Option<RendererDocumentLifecycleSnapshot>,
    last_sequence: Option<u64>,
}

/// One authoritative renderer lifecycle ingress and the subset immediately
/// visible to protocol projection.
///
/// A command-response visibility barrier may hold back `visible`, but it must
/// not change which concrete renderer records became browser facts.
#[derive(Debug, Default)]
pub(crate) struct RendererDocumentLifecycleIngress {
    authoritative: Vec<RendererDocumentLifecycleEvent>,
    visible: Vec<RendererDocumentLifecycleEvent>,
}

impl RendererDocumentLifecycleIngress {
    pub(crate) fn authoritative(&self) -> &[RendererDocumentLifecycleEvent] {
        &self.authoritative
    }

    pub(crate) fn into_visible(self) -> Vec<RendererDocumentLifecycleEvent> {
        self.visible
    }
}

impl RendererDocumentLifecycleProtocolCursor {
    fn from_snapshot(snapshot: RendererDocumentLifecycleSnapshot) -> Self {
        Self {
            snapshot: Some(snapshot),
            last_sequence: None,
        }
    }

    fn observe(&mut self, event: RendererDocumentLifecycleEvent) {
        debug_assert!(
            self.last_sequence
                .is_none_or(|sequence| event.sequence > sequence),
            "renderer lifecycle protocol cursors must advance monotonically"
        );
        self.last_sequence = Some(event.sequence);
        let Some(snapshot) = self.snapshot.as_mut() else {
            return;
        };
        apply_renderer_document_lifecycle_event_to_snapshot(snapshot, event);
    }
}

fn apply_renderer_document_lifecycle_event_to_snapshot(
    snapshot: &mut RendererDocumentLifecycleSnapshot,
    event: RendererDocumentLifecycleEvent,
) {
    match event.kind {
        RendererDocumentLifecycleEventKind::Started { .. } => {
            snapshot.frame = event.frame;
            snapshot.document = event.document;
            snapshot.epoch = event.epoch;
            snapshot.started = RendererLifecycleEventStamp {
                sequence: event.sequence,
                timestamp_micros: event.timestamp_micros,
            };
            snapshot.dom_content_loaded = None;
            snapshot.load = None;
            snapshot.terminated = None;
        }
        RendererDocumentLifecycleEventKind::Milestone(
            RendererDocumentLifecycleMilestone::DomContentLoaded,
        ) => {
            snapshot.dom_content_loaded = Some(RendererLifecycleEventStamp {
                sequence: event.sequence,
                timestamp_micros: event.timestamp_micros,
            });
        }
        RendererDocumentLifecycleEventKind::Milestone(RendererDocumentLifecycleMilestone::Load) => {
            snapshot.load = Some(RendererLifecycleEventStamp {
                sequence: event.sequence,
                timestamp_micros: event.timestamp_micros,
            });
        }
        RendererDocumentLifecycleEventKind::Terminated { reason, .. } => {
            snapshot.terminated = Some(RendererLifecycleTerminationStamp {
                sequence: event.sequence,
                timestamp_micros: event.timestamp_micros,
                reason,
            });
        }
    }
}

#[derive(Debug, Default)]
struct RendererDocumentLoadVisibility {
    barrier_loader_id: Option<String>,
    deferred_tail: Vec<RendererDocumentLifecycleEvent>,
}

#[derive(Debug)]
struct RootPostLoadObservation {
    binding: CommittedRendererDocumentBinding,
    frame_stopped_loading_pending: bool,
    network_idle_pending: bool,
}

pub type IsolatedWorldDefinition = moli_core::page::RuntimeIsolatedWorldDefinition;
pub type RuntimeBindingDefinition = moli_core::page::RuntimeBindingRegistration;
pub type DocumentStartScript = moli_core::page::DocumentStartScript;

#[derive(Debug, Clone)]
pub(crate) struct InitialDocumentPageBuildWaiter {
    receiver: watch::Receiver<Option<Result<(), String>>>,
}

impl InitialDocumentPageBuildWaiter {
    pub(crate) async fn wait(mut self) -> Result<(), String> {
        loop {
            if let Some(result) = self.receiver.borrow().clone() {
                return result;
            }
            self.receiver
                .changed()
                .await
                .map_err(|_| "InitialDocumentPageBuildCancelled".to_owned())?;
        }
    }
}

/// Exact renderer Page that is allowed to publish while its protocol target
/// has not installed the resulting [`Page`] yet.
///
/// Initial construction and cross-document navigation have different
/// retirement authorities, so the binding records which transition owns it.
/// A later navigation can never inherit an earlier Page reservation merely
/// because both builds used the same target/session.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingRendererPageBinding {
    InitialDocumentBuild {
        renderer_page: RendererPageResidenceIdentity,
    },
    DocumentNavigation {
        navigation: DocumentNavigationToken,
        renderer_page: RendererPageResidenceIdentity,
    },
}

impl PendingRendererPageBinding {
    fn renderer_page(&self) -> RendererPageResidenceIdentity {
        match self {
            Self::InitialDocumentBuild { renderer_page }
            | Self::DocumentNavigation { renderer_page, .. } => *renderer_page,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct TargetPageSlot {
    loaded_page: Option<BrowserPageRuntimeAccess>,
    loaded_page_absence_reason: TargetPageAbsenceReason,
    page_attachment_id: Option<TargetPageAttachmentId>,
    page_residence: BrowserPageResidenceHandle,
    page_residence_publisher: Option<TargetPageResidencePublisher>,
    renderer_document_lifecycle: RendererDocumentLifecycleProtocolState,
    root_post_load_observation: Option<RootPostLoadObservation>,
    initial_document_page_build_completion: Option<watch::Sender<Option<Result<(), String>>>>,
    pending_renderer_page: Option<PendingRendererPageBinding>,
}

impl TargetPageSlot {
    #[cfg(test)]
    pub(crate) fn empty_for_initial_document_page_build() -> Self {
        Self::empty_for_initial_document_page_build_with_residence(
            BrowserPageResidenceHandle::default(),
        )
    }

    pub(crate) fn empty_for_initial_document_page_build_with_residence(
        page_residence: BrowserPageResidenceHandle,
    ) -> Self {
        Self {
            loaded_page: None,
            loaded_page_absence_reason: TargetPageAbsenceReason::InitialDocumentPageBuildPending,
            page_residence,
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn empty_for_test_fixture() -> Self {
        Self {
            loaded_page: None,
            loaded_page_absence_reason: TargetPageAbsenceReason::TestFixture,
            ..Default::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn with_loaded_page_for_test(loaded_page: Page) -> Self {
        Self {
            loaded_page: Some(BrowserPageRuntimeAccess::from_page_for_test_fixture(
                loaded_page,
            )),
            page_attachment_id: Some(TargetPageAttachmentId::allocate()),
            ..Default::default()
        }
    }

    pub(crate) fn loaded_page(&self) -> Option<BrowserPageRuntimeLease> {
        self.loaded_page
            .as_ref()
            .and_then(BrowserPageRuntimeAccess::checkout_page)
    }

    pub(crate) fn loaded_renderer_page_residence(&self) -> Option<RendererPageResidenceIdentity> {
        self.loaded_page.as_ref().map(|page| {
            RendererPageResidenceIdentity::from_parts(
                page.renderer_owner_local_host_id(),
                page.renderer_page_id(),
            )
        })
    }

    pub(crate) fn loaded_page_mut(&self) -> Option<BrowserPageRuntimeLease> {
        self.loaded_page()
    }

    pub(crate) fn has_loaded_page(&self) -> bool {
        self.loaded_page
            .as_ref()
            .is_some_and(BrowserPageRuntimeAccess::is_live)
    }

    #[cfg(test)]
    pub(crate) fn loaded_page_runtime_access_for_test(&self) -> Option<BrowserPageRuntimeAccess> {
        self.loaded_page.clone()
    }

    pub(crate) fn loaded_page_absence_reason(&self) -> Option<TargetPageAbsenceReason> {
        (!self.has_loaded_page()).then_some(self.loaded_page_absence_reason)
    }

    pub(crate) fn mark_loaded_page_absent(&mut self, reason: TargetPageAbsenceReason) {
        if !self.has_loaded_page() {
            if self.loaded_page_absence_reason
                == TargetPageAbsenceReason::InitialDocumentPageBuildInProgress
                && reason != TargetPageAbsenceReason::InitialDocumentPageBuildInProgress
            {
                self.fail_initial_document_page_build("InitialDocumentPageBuildCancelled".into());
            }
            self.loaded_page_absence_reason = reason;
        }
    }

    pub(crate) fn start_initial_document_page_build(&mut self) {
        if !self.has_loaded_page() {
            self.loaded_page_absence_reason =
                TargetPageAbsenceReason::InitialDocumentPageBuildInProgress;
        }
        self.pending_renderer_page = None;
        let (sender, _receiver) = watch::channel(None);
        self.initial_document_page_build_completion = Some(sender);
    }

    pub(crate) fn bind_initial_document_page_build_renderer_page(
        &mut self,
        renderer_page: RendererPageResidenceIdentity,
    ) -> bool {
        if self.has_loaded_page()
            || self.loaded_page_absence_reason
                != TargetPageAbsenceReason::InitialDocumentPageBuildInProgress
            || self.initial_document_page_build_completion.is_none()
            || self.pending_renderer_page.is_some()
        {
            return false;
        }
        self.pending_renderer_page =
            Some(PendingRendererPageBinding::InitialDocumentBuild { renderer_page });
        true
    }

    pub(crate) fn initial_document_page_build_waiter(
        &self,
    ) -> Option<InitialDocumentPageBuildWaiter> {
        self.initial_document_page_build_completion
            .as_ref()
            .map(|sender| InitialDocumentPageBuildWaiter {
                receiver: sender.subscribe(),
            })
    }

    pub(crate) fn complete_initial_document_page_build(&mut self) {
        if matches!(
            self.pending_renderer_page.as_ref(),
            Some(PendingRendererPageBinding::InitialDocumentBuild { .. })
        ) {
            self.pending_renderer_page = None;
        }
        if let Some(sender) = self.initial_document_page_build_completion.take() {
            let _ = sender.send(Some(Ok(())));
        }
    }

    pub(crate) fn fail_initial_document_page_build(&mut self, message: String) {
        if matches!(
            self.pending_renderer_page.as_ref(),
            Some(PendingRendererPageBinding::InitialDocumentBuild { .. })
        ) {
            self.pending_renderer_page = None;
        }
        if let Some(sender) = self.initial_document_page_build_completion.take() {
            let _ = sender.send(Some(Err(message)));
        }
    }

    fn replace_loaded_page_fields(
        &mut self,
        page: Option<BrowserPageRuntimeAccess>,
        absence_reason: TargetPageAbsenceReason,
    ) -> Option<BrowserPageRuntimeAccess> {
        self.pending_renderer_page = None;
        if page.is_some() {
            self.complete_initial_document_page_build();
            self.loaded_page_absence_reason = TargetPageAbsenceReason::NoTarget;
            self.page_attachment_id = Some(TargetPageAttachmentId::allocate());
        } else {
            if self.loaded_page_absence_reason
                == TargetPageAbsenceReason::InitialDocumentPageBuildInProgress
            {
                self.fail_initial_document_page_build("InitialDocumentPageBuildCancelled".into());
            }
            self.loaded_page_absence_reason = absence_reason;
            self.page_attachment_id = None;
        }
        let previous = std::mem::replace(&mut self.loaded_page, page);
        self.supersede_page_residence();
        previous
    }

    #[cfg(test)]
    pub(crate) fn replace_loaded_page_with_reason(
        &mut self,
        page: Option<Page>,
        absence_reason: TargetPageAbsenceReason,
    ) -> Option<Page> {
        let page_residence_changes = self.has_loaded_page() || page.is_some();
        let page = page.map(BrowserPageRuntimeAccess::from_page_for_test_fixture);
        let previous = self.replace_loaded_page_fields(page, absence_reason);
        if page_residence_changes {
            self.page_residence.advance_generation_for_test_fixture();
        }
        previous.and_then(BrowserPageRuntimeAccess::retire_and_take_page_for_test_fixture)
    }

    fn project_page_residence_transition_after_browser_owner_commit(
        &mut self,
        page: Option<BrowserPageRuntimeAccess>,
        absence_reason: TargetPageAbsenceReason,
        transition: &BrowserPageResidenceTransition,
        expected_kind: BrowserPageResidenceTransitionKind,
    ) {
        debug_assert_eq!(
            transition.kind(),
            expected_kind,
            "physical Page projection must match the Browser Owner transition kind"
        );
        debug_assert!(
            self.page_residence.is_current(transition.current_page()),
            "physical Page projection must match the committed Browser Owner residence"
        );
        let _ = self.replace_loaded_page_fields(page, absence_reason);
    }

    pub(crate) fn project_initial_document_page_after_browser_owner_commit(
        &mut self,
        transition: &BrowserPageResidenceTransition,
    ) {
        let page = transition.current_page_runtime().cloned();
        debug_assert!(
            page.is_some(),
            "initial Document commit must own a Page runtime"
        );
        self.project_page_residence_transition_after_browser_owner_commit(
            page,
            TargetPageAbsenceReason::NoTarget,
            transition,
            BrowserPageResidenceTransitionKind::InitialDocumentMaterialization,
        )
    }

    pub(crate) fn project_failed_navigation_page_absence_after_browser_owner_commit(
        &mut self,
        transition: &BrowserPageResidenceTransition,
    ) {
        self.project_page_residence_transition_after_browser_owner_commit(
            None,
            TargetPageAbsenceReason::NavigationFailed,
            transition,
            BrowserPageResidenceTransitionKind::FailedNavigationDiscard,
        )
    }

    /// Drops a physical Page after Browser Core has already forgotten the
    /// owning Target/Context. Exact owner lookup is already impossible, so the
    /// orphaned capability must not be advanced by the projector.
    pub(crate) fn retire_page_projection_after_browser_owner_forget(&mut self) -> Option<Page> {
        let previous = self.replace_loaded_page_fields(None, TargetPageAbsenceReason::TargetClosed);
        #[cfg(any(test, feature = "test-support"))]
        {
            previous.and_then(BrowserPageRuntimeAccess::retire_and_take_page_for_test_fixture)
        }
        #[cfg(not(any(test, feature = "test-support")))]
        {
            drop(previous);
            None
        }
    }

    /// Projects a Page replacement already committed by Browser Core.
    ///
    /// The authoritative operation has advanced this slot's shared residence
    /// capability. Protocol storage must install the physical Page without
    /// advancing it a second time.
    pub(crate) fn project_loaded_page_after_browser_owner_commit(
        &mut self,
        replacement: &moli_core::browser_host::BrowserPageReplacement,
    ) {
        assert!(
            self.page_residence.is_current(replacement.current_page()),
            "protocol Page projection must match the committed Browser Owner residence"
        );
        let page = replacement.current_page_runtime().cloned();
        debug_assert!(page.is_some(), "loaded Page commit must own a Page runtime");
        let _ = self.replace_loaded_page_fields(page, TargetPageAbsenceReason::NoTarget);
    }

    /// Projects Page absence after Browser Core has committed a Target
    /// terminal transition and advanced this shared residence capability.
    pub(crate) fn project_page_absence_after_browser_owner_termination(
        &mut self,
        reason: TargetPageAbsenceReason,
        terminal_page: &moli_core::browser_host::PageResidenceIdentity,
    ) {
        assert!(
            matches!(
                reason,
                TargetPageAbsenceReason::TargetClosed | TargetPageAbsenceReason::TargetCrashed
            ),
            "Browser Owner Target termination must project a terminal Page absence"
        );
        assert!(
            self.page_residence.is_current(terminal_page),
            "protocol Target termination projection must match the committed Browser Owner residence"
        );
        let _ = self.replace_loaded_page_fields(None, reason);
    }

    #[cfg(test)]
    pub(crate) fn replace_loaded_page(&mut self, page: Option<Page>) -> Option<Page> {
        let Some(page) = page else {
            panic!(
                "replace_loaded_page(None) is not a valid production transition; use clear_loaded_page_with_reason"
            );
        };
        self.replace_loaded_page_with_reason(Some(page), TargetPageAbsenceReason::NoTarget)
    }

    pub(crate) fn page_attachment_id(&self) -> Option<TargetPageAttachmentId> {
        self.page_attachment_id
    }

    #[cfg(test)]
    pub(crate) fn loaded_page_generation(&self) -> u64 {
        self.page_residence.generation()
    }

    pub(crate) fn page_residence_handle(&self) -> &BrowserPageResidenceHandle {
        &self.page_residence
    }

    pub(crate) fn page_residence_token(&mut self) -> Option<TargetPageResidenceToken> {
        let attachment_id = self.page_attachment_id?;
        let publisher = self
            .page_residence_publisher
            .get_or_insert_with(|| TargetPageResidencePublisher::new(attachment_id));
        Some(publisher.token())
    }

    fn supersede_page_residence(&mut self) {
        if let Some(publisher) = self.page_residence_publisher.take() {
            publisher.supersede();
        }
    }

    pub(crate) fn prepare_for_new_target(&mut self, page_residence: BrowserPageResidenceHandle) {
        debug_assert!(
            !self.has_loaded_page(),
            "a loaded Page residence must move with its target slot"
        );
        self.page_residence = page_residence;
    }

    #[cfg(test)]
    pub(crate) fn replace_page_residence_handle(
        &mut self,
        page_residence: BrowserPageResidenceHandle,
    ) {
        self.page_residence = page_residence;
    }

    #[cfg(test)]
    fn advance_loaded_page_generation(&mut self) {
        self.page_residence.advance_generation_for_test_fixture();
    }

    #[cfg(test)]
    pub(crate) fn bump_loaded_page_generation(&mut self) {
        self.advance_loaded_page_generation();
    }

    #[cfg(test)]
    pub(crate) fn set_loaded_page_generation(&mut self, generation: u64) {
        self.page_residence
            .set_generation_for_test_fixture(generation);
    }

    #[cfg(test)]
    pub(crate) fn set_page_attachment_id_for_test(&mut self, raw: u64) -> TargetPageAttachmentId {
        let attachment_id = TargetPageAttachmentId::from_raw_for_test(raw);
        let attachment_changed = self.page_attachment_id != Some(attachment_id);
        self.page_attachment_id = Some(attachment_id);
        if attachment_changed {
            self.supersede_page_residence();
        }
        attachment_id
    }

    pub(crate) fn begin_document_navigation_protocol_state(&mut self) {
        self.pending_renderer_page = None;
    }

    pub(crate) fn bind_document_navigation_renderer_page(
        &mut self,
        token: &DocumentNavigationToken,
        renderer_page: RendererPageResidenceIdentity,
    ) -> bool {
        if self.pending_renderer_page.is_some() {
            return false;
        }
        self.pending_renderer_page = Some(PendingRendererPageBinding::DocumentNavigation {
            navigation: token.clone(),
            renderer_page,
        });
        true
    }

    pub(crate) fn routes_renderer_page(
        &self,
        renderer_page: RendererPageResidenceIdentity,
    ) -> bool {
        self.loaded_renderer_page_residence() == Some(renderer_page)
            || self
                .pending_renderer_page
                .as_ref()
                .is_some_and(|binding| binding.renderer_page() == renderer_page)
    }

    pub(crate) fn clear_pending_renderer_page_if_loader_matches(
        &mut self,
        loader_id: &str,
    ) -> bool {
        if !matches!(
            self.pending_renderer_page.as_ref(),
            Some(PendingRendererPageBinding::DocumentNavigation {
                navigation,
                ..
            }) if navigation.loader_id() == loader_id
        ) {
            return false;
        }
        self.pending_renderer_page = None;
        true
    }

    pub(crate) fn clear_renderer_document_protocol_state(&mut self) {
        self.pending_renderer_page = None;
        self.renderer_document_lifecycle = RendererDocumentLifecycleProtocolState::default();
        self.root_post_load_observation = None;
    }

    #[cfg(test)]
    pub(crate) fn bind_renderer_document_lifecycle(
        &mut self,
        artifacts: RendererPageCreationArtifacts,
        navigation: Option<DocumentNavigationToken>,
        frame_id: String,
        loader_id: String,
    ) -> Vec<RendererDocumentLifecycleEvent> {
        self.bind_renderer_document_lifecycle_with_ingress(
            artifacts, navigation, frame_id, loader_id,
        )
        .into_visible()
    }

    pub(crate) fn bind_renderer_document_lifecycle_with_ingress(
        &mut self,
        artifacts: RendererPageCreationArtifacts,
        navigation: Option<DocumentNavigationToken>,
        frame_id: String,
        loader_id: String,
    ) -> RendererDocumentLifecycleIngress {
        let RendererPageCreationArtifacts {
            active_document,
            active_epoch,
            lifecycle_snapshot,
            initial_lifecycle_events,
        } = artifacts;
        if lifecycle_snapshot.document != active_document
            || lifecycle_snapshot.epoch != active_epoch
        {
            tracing::warn!(
                ?active_document,
                ?active_epoch,
                snapshot_document = ?lifecycle_snapshot.document,
                snapshot_epoch = ?lifecycle_snapshot.epoch,
                "rejecting inconsistent renderer page creation lifecycle artifacts"
            );
            return RendererDocumentLifecycleIngress::default();
        }
        let Some(page_attachment_id) = self.page_attachment_id else {
            tracing::debug!(
                ?active_document,
                ?active_epoch,
                "dropping renderer lifecycle artifacts without a current Page attachment"
            );
            return RendererDocumentLifecycleIngress::default();
        };
        let initial_snapshot = initial_lifecycle_events
            .iter()
            .find(|event| {
                event.frame == lifecycle_snapshot.frame
                    && event.document == active_document
                    && matches!(
                        event.kind,
                        RendererDocumentLifecycleEventKind::Started { .. }
                    )
            })
            .map(|event| RendererDocumentLifecycleSnapshot {
                frame: event.frame,
                document: event.document,
                epoch: event.epoch,
                started: RendererLifecycleEventStamp {
                    sequence: event.sequence,
                    timestamp_micros: event.timestamp_micros,
                },
                dom_content_loaded: None,
                load: None,
                terminated: None,
            })
            .unwrap_or(lifecycle_snapshot);
        let binding = CommittedRendererDocumentBinding {
            renderer_frame: lifecycle_snapshot.frame,
            renderer_document: active_document,
            renderer_epoch: initial_snapshot.epoch,
            navigation,
            frame_id,
            loader_id,
            page_attachment_id,
            document_open_replacement_epoch: None,
        };
        tracing::trace!(
            target: "moli_renderer_document_lifecycle",
            renderer_document = ?active_document,
            renderer_lifecycle_epoch = active_epoch.0,
            frame_id = binding.frame_id,
            loader_id = binding.loader_id,
            page_attachment_id = binding.page_attachment_id.get(),
            "bound renderer document lifecycle to committed protocol document"
        );
        let lifecycle_cursor =
            RendererDocumentLifecycleProtocolCursor::from_snapshot(initial_snapshot);
        self.renderer_document_lifecycle = RendererDocumentLifecycleProtocolState {
            binding: Some(binding),
            authoritative: lifecycle_cursor,
            visible: lifecycle_cursor,
            load_visibility: RendererDocumentLoadVisibility::default(),
        };
        self.root_post_load_observation = None;
        self.ingest_renderer_document_lifecycle_events_with_ingress(initial_lifecycle_events)
    }

    pub(crate) fn begin_renderer_document_load_visibility_barrier(
        &mut self,
        loader_id: &str,
    ) -> bool {
        let binding_matches = self
            .renderer_document_lifecycle
            .binding
            .as_ref()
            .is_some_and(|binding| binding.loader_id == loader_id);
        if !binding_matches {
            return false;
        }
        if let Some(active_loader_id) = self
            .renderer_document_lifecycle
            .load_visibility
            .barrier_loader_id
            .as_deref()
        {
            return active_loader_id == loader_id;
        }
        debug_assert!(
            self.renderer_document_lifecycle
                .load_visibility
                .deferred_tail
                .is_empty(),
            "a new load visibility barrier must not inherit deferred events"
        );
        self.renderer_document_lifecycle
            .load_visibility
            .barrier_loader_id = Some(loader_id.to_owned());
        true
    }

    pub(crate) fn release_renderer_document_load_visibility_barrier(
        &mut self,
        loader_id: &str,
    ) -> Option<Vec<RendererDocumentLifecycleEvent>> {
        if self
            .renderer_document_lifecycle
            .load_visibility
            .barrier_loader_id
            .as_deref()
            != Some(loader_id)
        {
            return None;
        }
        self.renderer_document_lifecycle
            .load_visibility
            .barrier_loader_id = None;
        let deferred_tail = std::mem::take(
            &mut self
                .renderer_document_lifecycle
                .load_visibility
                .deferred_tail,
        );
        for event in &deferred_tail {
            self.renderer_document_lifecycle.visible.observe(*event);
        }
        Some(deferred_tail)
    }

    pub(crate) fn cancel_renderer_document_load_visibility_barrier(
        &mut self,
        loader_id: &str,
    ) -> bool {
        if self
            .renderer_document_lifecycle
            .load_visibility
            .barrier_loader_id
            .as_deref()
            != Some(loader_id)
        {
            return false;
        }
        self.renderer_document_lifecycle
            .load_visibility
            .barrier_loader_id = None;
        self.renderer_document_lifecycle
            .load_visibility
            .deferred_tail
            .clear();
        true
    }

    #[cfg(test)]
    fn renderer_document_load_visibility_barrier_active(&self) -> bool {
        self.renderer_document_lifecycle
            .load_visibility
            .barrier_loader_id
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn ingest_renderer_document_lifecycle_events(
        &mut self,
        events: Vec<RendererDocumentLifecycleEvent>,
    ) -> Vec<RendererDocumentLifecycleEvent> {
        self.ingest_renderer_document_lifecycle_events_with_ingress(events)
            .into_visible()
    }

    pub(crate) fn ingest_renderer_document_lifecycle_events_with_ingress(
        &mut self,
        events: Vec<RendererDocumentLifecycleEvent>,
    ) -> RendererDocumentLifecycleIngress {
        let binding_is_current = self
            .renderer_document_lifecycle
            .binding
            .as_ref()
            .is_some_and(|binding| Some(binding.page_attachment_id) == self.page_attachment_id);
        if !binding_is_current {
            if !events.is_empty() {
                tracing::debug!(
                    event_count = events.len(),
                    "dropping renderer lifecycle events for stale protocol binding"
                );
            }
            return RendererDocumentLifecycleIngress::default();
        }
        let mut ingress = RendererDocumentLifecycleIngress::default();
        for event in events {
            let load_visibility_barrier_active = self
                .renderer_document_lifecycle
                .load_visibility
                .barrier_loader_id
                .is_some();
            let load_visibility_tail_started = !self
                .renderer_document_lifecycle
                .load_visibility
                .deferred_tail
                .is_empty();
            let defer_load_visibility = load_visibility_barrier_active
                && (load_visibility_tail_started
                    || matches!(
                        event.kind,
                        RendererDocumentLifecycleEventKind::Milestone(
                            RendererDocumentLifecycleMilestone::Load
                        )
                    ));
            let Some(binding) = self.renderer_document_lifecycle.binding.as_ref().cloned() else {
                tracing::debug!(
                    sequence = event.sequence,
                    "dropping renderer lifecycle event without committed binding"
                );
                continue;
            };
            if event.frame != binding.renderer_frame || event.document != binding.renderer_document
            {
                tracing::debug!(
                    sequence = event.sequence,
                    event_document = ?event.document,
                    bound_document = ?binding.renderer_document,
                    "dropping stale renderer lifecycle event for another document"
                );
                continue;
            }
            if self
                .renderer_document_lifecycle
                .authoritative
                .last_sequence
                .is_some_and(|sequence| event.sequence <= sequence)
            {
                tracing::debug!(
                    sequence = event.sequence,
                    "dropping duplicate or reordered renderer lifecycle event"
                );
                continue;
            }
            let restarts_same_document = event.epoch != binding.renderer_epoch
                && matches!(
                    event.kind,
                    RendererDocumentLifecycleEventKind::Started { .. }
                )
                && event.epoch.0 > binding.renderer_epoch.0
                && self
                    .renderer_document_lifecycle
                    .authoritative
                    .snapshot
                    .is_some_and(|snapshot| snapshot.terminated.is_some());
            if event.epoch != binding.renderer_epoch && !restarts_same_document {
                tracing::debug!(
                    sequence = event.sequence,
                    event_epoch = event.epoch.0,
                    bound_epoch = binding.renderer_epoch.0,
                    "dropping stale renderer lifecycle event for another epoch"
                );
                continue;
            }
            if restarts_same_document {
                self.renderer_document_lifecycle
                    .binding
                    .as_mut()
                    .expect("validated lifecycle binding")
                    .renderer_epoch = event.epoch;
            }
            if let RendererDocumentLifecycleEventKind::Started { reason } = event.kind {
                self.renderer_document_lifecycle
                    .binding
                    .as_mut()
                    .expect("validated lifecycle binding")
                    .document_open_replacement_epoch = matches!(
                    reason,
                    RendererLifecycleStartReason::ExplicitDocumentOpen
                        | RendererLifecycleStartReason::JavascriptDocumentReplacement
                )
                .then_some(event.epoch);
            }
            self.renderer_document_lifecycle
                .authoritative
                .observe(event);
            ingress.authoritative.push(event);
            if defer_load_visibility {
                self.renderer_document_lifecycle
                    .load_visibility
                    .deferred_tail
                    .push(event);
            } else {
                self.renderer_document_lifecycle.visible.observe(event);
                ingress.visible.push(event);
            }
        }
        ingress
    }

    pub(crate) fn renderer_document_lifecycle_binding(
        &self,
    ) -> Option<&CommittedRendererDocumentBinding> {
        self.renderer_document_lifecycle
            .binding
            .as_ref()
            .filter(|binding| Some(binding.page_attachment_id) == self.page_attachment_id)
    }

    #[cfg(test)]
    pub(crate) fn renderer_document_lifecycle_authoritative_snapshot(
        &self,
    ) -> Option<RendererDocumentLifecycleSnapshot> {
        self.renderer_document_lifecycle.authoritative.snapshot
    }

    pub(crate) fn renderer_document_lifecycle_visible_snapshot(
        &self,
    ) -> Option<RendererDocumentLifecycleSnapshot> {
        self.renderer_document_lifecycle.visible.snapshot
    }

    pub(crate) fn arm_root_post_load_observation(&mut self, loader_id: &str) -> bool {
        let Some(binding) = self
            .renderer_document_lifecycle
            .binding
            .as_ref()
            .filter(|binding| {
                binding.loader_id == loader_id
                    && Some(binding.page_attachment_id) == self.page_attachment_id
            })
            .cloned()
        else {
            return false;
        };
        let snapshot_reached_load = self
            .renderer_document_lifecycle
            .authoritative
            .snapshot
            .is_some_and(|snapshot| {
                snapshot.document == binding.renderer_document
                    && snapshot.epoch == binding.renderer_epoch
                    && snapshot.load.is_some()
                    && snapshot.terminated.is_none()
            });
        if !snapshot_reached_load {
            return false;
        }
        if self
            .root_post_load_observation
            .as_ref()
            .is_some_and(|observation| observation.binding == binding)
        {
            return false;
        }
        self.root_post_load_observation = Some(RootPostLoadObservation {
            binding,
            frame_stopped_loading_pending: true,
            network_idle_pending: true,
        });
        true
    }

    pub(crate) fn take_root_frame_stopped_loading_binding(
        &mut self,
    ) -> Option<CommittedRendererDocumentBinding> {
        if !self.root_post_load_binding_is_current() {
            self.root_post_load_observation = None;
            return None;
        }
        let observation = self.root_post_load_observation.as_mut()?;
        if !observation.frame_stopped_loading_pending {
            return None;
        }
        observation.frame_stopped_loading_pending = false;
        Some(observation.binding.clone())
    }

    pub(crate) fn take_root_network_idle_binding(
        &mut self,
        has_pending_document_navigation: bool,
    ) -> Option<CommittedRendererDocumentBinding> {
        if has_pending_document_navigation {
            return None;
        }
        if !self.root_post_load_binding_is_current() {
            self.root_post_load_observation = None;
            return None;
        }
        if !self.root_network_idle_snapshot_is_eligible() {
            if let Some(observation) = self.root_post_load_observation.as_mut() {
                observation.network_idle_pending = false;
            }
            return None;
        }
        let observation = self.root_post_load_observation.as_mut()?;
        if !observation.network_idle_pending {
            return None;
        }
        observation.network_idle_pending = false;
        Some(observation.binding.clone())
    }

    fn root_post_load_binding_is_current(&self) -> bool {
        let Some(observation) = self.root_post_load_observation.as_ref() else {
            return false;
        };
        self.renderer_document_lifecycle.binding.as_ref() == Some(&observation.binding)
    }

    fn root_network_idle_snapshot_is_eligible(&self) -> bool {
        let Some(observation) = self.root_post_load_observation.as_ref() else {
            return false;
        };
        self.renderer_document_lifecycle
            .authoritative
            .snapshot
            .is_some_and(|snapshot| {
                snapshot.document == observation.binding.renderer_document
                    && snapshot.epoch == observation.binding.renderer_epoch
                    && snapshot.load.is_some()
                    && snapshot.terminated.is_none()
            })
    }
}

#[cfg(test)]
mod page_residence_tests {
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
    };

    use super::*;
    use crate::conn::TargetPageResidenceObservation;

    #[test]
    fn attachment_token_ignores_generation_and_terminates_on_attachment_replacement() {
        let mut slot = TargetPageSlot::default();
        slot.set_page_attachment_id_for_test(91);
        let token = slot
            .page_residence_token()
            .expect("the installed attachment should expose its lifetime token");

        slot.bump_loaded_page_generation();

        let mut wait = Box::pin(token.wait());
        let mut context = Context::from_waker(Waker::noop());
        assert!(
            matches!(wait.as_mut().poll(&mut context), Poll::Pending),
            "changing only the slot generation must not terminate an attachment token"
        );

        slot.set_page_attachment_id_for_test(92);

        assert!(matches!(
            wait.as_mut().poll(&mut context),
            Poll::Ready(TargetPageResidenceObservation::Superseded)
        ));
    }
}

#[cfg(test)]
mod pending_renderer_page_tests {
    use super::*;

    fn renderer_page(owner: u64, page: u64) -> RendererPageResidenceIdentity {
        RendererPageResidenceIdentity::new(
            moli_core::RendererOwnerLocalHostId::new_for_testing(owner),
            moli_core::PageId::new_for_testing(page),
        )
    }

    #[test]
    fn initial_build_binding_is_exact_and_retires_with_build() {
        let mut slot = TargetPageSlot::empty_for_initial_document_page_build();
        slot.start_initial_document_page_build();
        let expected = renderer_page(7, 11);
        let peer = renderer_page(7, 12);

        assert!(slot.bind_initial_document_page_build_renderer_page(expected));
        assert!(slot.routes_renderer_page(expected));
        assert!(!slot.routes_renderer_page(peer));
        assert!(
            !slot.bind_initial_document_page_build_renderer_page(peer),
            "an initial build owns exactly one renderer Page reservation"
        );

        slot.complete_initial_document_page_build();
        assert!(!slot.routes_renderer_page(expected));
    }

    #[test]
    fn navigation_binding_cannot_follow_a_superseding_navigation() {
        let mut slot = TargetPageSlot::default();
        let first = DocumentNavigationToken::new("TID-pending-page", "LOADER-first");
        slot.begin_document_navigation_protocol_state();
        let first_page = renderer_page(8, 21);
        assert!(slot.bind_document_navigation_renderer_page(&first, first_page));
        assert!(slot.routes_renderer_page(first_page));
        assert!(
            !slot.bind_document_navigation_renderer_page(&first, renderer_page(8, 23),),
            "one navigation generation cannot replace its bound renderer Page"
        );

        let second = DocumentNavigationToken::new("TID-pending-page", "LOADER-second");
        slot.begin_document_navigation_protocol_state();
        assert!(
            !slot.routes_renderer_page(first_page),
            "a new navigation must retire the prior pending renderer Page route"
        );

        let second_page = renderer_page(8, 22);
        assert!(slot.bind_document_navigation_renderer_page(&second, second_page));
        assert!(slot.clear_pending_renderer_page_if_loader_matches("LOADER-second"));
        assert!(!slot.routes_renderer_page(second_page));
    }
}

#[cfg(test)]
mod renderer_document_lifecycle_tests {
    use super::*;
    use moli_core::page::{
        RendererDocumentLifecycleEventKind, RendererDocumentTerminationReason,
        RendererLifecycleStartReason,
    };

    fn event(
        document: RendererDocumentToken,
        epoch: RendererLifecycleEpoch,
        sequence: u64,
        kind: RendererDocumentLifecycleEventKind,
    ) -> RendererDocumentLifecycleEvent {
        RendererDocumentLifecycleEvent {
            frame: RendererFrameToken {
                page_id: document.page_id,
            },
            document,
            epoch,
            sequence,
            timestamp_micros: sequence * 10,
            kind,
        }
    }

    fn page_slot_with_attachment() -> TargetPageSlot {
        TargetPageSlot {
            page_attachment_id: Some(TargetPageAttachmentId::allocate()),
            ..Default::default()
        }
    }

    #[test]
    fn lifecycle_binding_requires_and_tracks_the_current_page_attachment() {
        let page_id = moli_core::PageId::new_for_testing(8);
        let document = RendererDocumentToken::new_for_testing(page_id, 1);
        let epoch = RendererLifecycleEpoch(1);
        let started = event(
            document,
            epoch,
            1,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::InitialDocument,
            },
        );
        let artifacts = RendererPageCreationArtifacts {
            active_document: document,
            active_epoch: epoch,
            lifecycle_snapshot: RendererDocumentLifecycleSnapshot {
                frame: started.frame,
                document,
                epoch,
                started: RendererLifecycleEventStamp {
                    sequence: 1,
                    timestamp_micros: 10,
                },
                dom_content_loaded: None,
                load: None,
                terminated: None,
            },
            initial_lifecycle_events: vec![started],
        };

        let mut slot = TargetPageSlot::default();
        assert!(
            slot.bind_renderer_document_lifecycle(
                artifacts.clone(),
                None,
                "FRAME-8".to_owned(),
                "LOADER-8".to_owned(),
            )
            .is_empty()
        );
        assert!(slot.renderer_document_lifecycle_binding().is_none());

        slot.set_page_attachment_id_for_test(8);
        assert_eq!(
            slot.bind_renderer_document_lifecycle(
                artifacts,
                None,
                "FRAME-8".to_owned(),
                "LOADER-8".to_owned(),
            ),
            vec![started]
        );
        assert!(slot.renderer_document_lifecycle_binding().is_some());

        slot.page_attachment_id = None;
        assert!(
            slot.renderer_document_lifecycle_binding().is_none(),
            "a binding from a removed Page attachment must never remain current"
        );
    }

    #[test]
    fn binding_accepts_current_identity_and_rejects_stale_document() {
        let page_id = moli_core::PageId::new_for_testing(9);
        let document = RendererDocumentToken::new_for_testing(page_id, 1);
        let epoch = RendererLifecycleEpoch(1);
        let started = event(
            document,
            epoch,
            1,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::InitialDocument,
            },
        );
        let dcl = event(
            document,
            epoch,
            2,
            RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        );
        let mut slot = page_slot_with_attachment();
        slot.set_page_attachment_id_for_test(4);
        let navigation = DocumentNavigationToken::new("FRAME-9", "LOADER-9");
        let accepted = slot.bind_renderer_document_lifecycle(
            RendererPageCreationArtifacts {
                active_document: document,
                active_epoch: epoch,
                lifecycle_snapshot: RendererDocumentLifecycleSnapshot {
                    frame: started.frame,
                    document,
                    epoch,
                    started: RendererLifecycleEventStamp {
                        sequence: 1,
                        timestamp_micros: 10,
                    },
                    dom_content_loaded: Some(RendererLifecycleEventStamp {
                        sequence: 2,
                        timestamp_micros: 20,
                    }),
                    load: None,
                    terminated: None,
                },
                initial_lifecycle_events: vec![started, dcl],
            },
            Some(navigation),
            "FRAME-9".to_owned(),
            "LOADER-9".to_owned(),
        );
        assert_eq!(accepted, vec![started, dcl]);
        assert!(slot.begin_renderer_document_load_visibility_barrier("LOADER-9"));
        assert!(slot.renderer_document_load_visibility_barrier_active());
        assert!(
            slot.release_renderer_document_load_visibility_barrier("LOADER-stale")
                .is_none()
        );
        assert!(slot.renderer_document_load_visibility_barrier_active());
        assert_eq!(
            slot.release_renderer_document_load_visibility_barrier("LOADER-9"),
            Some(Vec::new())
        );
        assert!(!slot.renderer_document_load_visibility_barrier_active());
        assert_eq!(
            slot.renderer_document_lifecycle_binding()
                .unwrap()
                .page_attachment_id
                .get(),
            4
        );

        let stale = event(
            document.successor_for_testing(),
            epoch,
            3,
            RendererDocumentLifecycleEventKind::Milestone(RendererDocumentLifecycleMilestone::Load),
        );
        assert!(
            slot.ingest_renderer_document_lifecycle_events(vec![stale])
                .is_empty()
        );
        assert!(
            slot.renderer_document_lifecycle_authoritative_snapshot()
                .unwrap()
                .load
                .is_none()
        );
    }

    #[test]
    fn load_visibility_barrier_exposes_dcl_and_defers_only_load_delivery() {
        let page_id = moli_core::PageId::new_for_testing(10);
        let document = RendererDocumentToken::new_for_testing(page_id, 1);
        let epoch = RendererLifecycleEpoch(1);
        let started = event(
            document,
            epoch,
            1,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::InitialDocument,
            },
        );
        let dcl = event(
            document,
            epoch,
            2,
            RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        );
        let load = event(
            document,
            epoch,
            3,
            RendererDocumentLifecycleEventKind::Milestone(RendererDocumentLifecycleMilestone::Load),
        );
        let terminated = event(
            document,
            epoch,
            4,
            RendererDocumentLifecycleEventKind::Terminated {
                last_reached: Some(RendererDocumentLifecycleMilestone::Load),
                reason: RendererDocumentTerminationReason::Stopped,
            },
        );
        let mut slot = page_slot_with_attachment();
        slot.set_page_attachment_id_for_test(5);
        let navigation = DocumentNavigationToken::new("FRAME-10", "LOADER-10");
        assert_eq!(
            slot.bind_renderer_document_lifecycle(
                RendererPageCreationArtifacts {
                    active_document: document,
                    active_epoch: epoch,
                    lifecycle_snapshot: RendererDocumentLifecycleSnapshot {
                        frame: started.frame,
                        document,
                        epoch,
                        started: RendererLifecycleEventStamp {
                            sequence: 1,
                            timestamp_micros: 10,
                        },
                        dom_content_loaded: None,
                        load: None,
                        terminated: None,
                    },
                    initial_lifecycle_events: vec![started],
                },
                Some(navigation),
                "FRAME-10".to_owned(),
                "LOADER-10".to_owned(),
            ),
            vec![started]
        );
        assert!(slot.begin_renderer_document_load_visibility_barrier("LOADER-10"));
        assert_eq!(
            slot.ingest_renderer_document_lifecycle_events(vec![dcl, load, terminated]),
            vec![dcl],
            "DOMContentLoaded remains visible while the ordered tail from load is gated"
        );
        assert_eq!(
            slot.renderer_document_lifecycle_authoritative_snapshot()
                .and_then(|snapshot| snapshot.load),
            Some(RendererLifecycleEventStamp {
                sequence: 3,
                timestamp_micros: 30,
            }),
            "load readiness is authoritative even while its protocol event is hidden"
        );
        let visible_before_release = slot
            .renderer_document_lifecycle_visible_snapshot()
            .expect("visible lifecycle cursor");
        assert_eq!(
            visible_before_release.dom_content_loaded,
            Some(RendererLifecycleEventStamp {
                sequence: 2,
                timestamp_micros: 20,
            })
        );
        assert_eq!(visible_before_release.load, None);
        assert_eq!(visible_before_release.terminated, None);
        assert_eq!(
            slot.release_renderer_document_load_visibility_barrier("LOADER-10"),
            Some(vec![load, terminated]),
            "events after load must not overtake the delayed load milestone"
        );
        let visible_after_release = slot
            .renderer_document_lifecycle_visible_snapshot()
            .expect("released visible lifecycle cursor");
        assert_eq!(
            visible_after_release.load,
            Some(RendererLifecycleEventStamp {
                sequence: 3,
                timestamp_micros: 30,
            })
        );
        assert_eq!(
            visible_after_release.terminated,
            Some(RendererLifecycleTerminationStamp {
                sequence: 4,
                timestamp_micros: 40,
                reason: RendererDocumentTerminationReason::Stopped,
            })
        );
    }

    #[test]
    fn cancelling_load_visibility_barrier_discards_tail_without_revealing_it() {
        let page_id = moli_core::PageId::new_for_testing(16);
        let document = RendererDocumentToken::new_for_testing(page_id, 1);
        let epoch = RendererLifecycleEpoch(1);
        let started = event(
            document,
            epoch,
            1,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::InitialDocument,
            },
        );
        let load = event(
            document,
            epoch,
            2,
            RendererDocumentLifecycleEventKind::Milestone(RendererDocumentLifecycleMilestone::Load),
        );
        let mut slot = page_slot_with_attachment();
        slot.bind_renderer_document_lifecycle(
            RendererPageCreationArtifacts {
                active_document: document,
                active_epoch: epoch,
                lifecycle_snapshot: RendererDocumentLifecycleSnapshot {
                    frame: started.frame,
                    document,
                    epoch,
                    started: RendererLifecycleEventStamp {
                        sequence: 1,
                        timestamp_micros: 10,
                    },
                    dom_content_loaded: None,
                    load: None,
                    terminated: None,
                },
                initial_lifecycle_events: vec![started],
            },
            None,
            "FRAME-16".to_owned(),
            "LOADER-16".to_owned(),
        );
        assert!(slot.begin_renderer_document_load_visibility_barrier("LOADER-16"));
        assert!(
            slot.ingest_renderer_document_lifecycle_events(vec![load])
                .is_empty()
        );
        assert!(
            slot.renderer_document_lifecycle_authoritative_snapshot()
                .is_some_and(|snapshot| snapshot.load.is_some())
        );
        assert!(slot.cancel_renderer_document_load_visibility_barrier("LOADER-16"));
        assert!(!slot.renderer_document_load_visibility_barrier_active());
        assert!(
            slot.renderer_document_lifecycle_visible_snapshot()
                .is_some_and(|snapshot| snapshot.load.is_none()),
            "discarding a stale output tail must not make it replayable"
        );
        assert!(
            slot.release_renderer_document_load_visibility_barrier("LOADER-16")
                .is_none()
        );
        assert!(!slot.cancel_renderer_document_load_visibility_barrier("LOADER-16"));
    }

    #[test]
    fn load_visibility_barrier_keeps_later_epoch_behind_deferred_load_tail() {
        let page_id = moli_core::PageId::new_for_testing(11);
        let document = RendererDocumentToken::new_for_testing(page_id, 1);
        let first_epoch = RendererLifecycleEpoch(1);
        let second_epoch = RendererLifecycleEpoch(2);
        let started = event(
            document,
            first_epoch,
            1,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::InitialDocument,
            },
        );
        let dcl = event(
            document,
            first_epoch,
            2,
            RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        );
        let load = event(
            document,
            first_epoch,
            3,
            RendererDocumentLifecycleEventKind::Milestone(RendererDocumentLifecycleMilestone::Load),
        );
        let terminated = event(
            document,
            first_epoch,
            4,
            RendererDocumentLifecycleEventKind::Terminated {
                last_reached: Some(RendererDocumentLifecycleMilestone::Load),
                reason: RendererDocumentTerminationReason::RestartedByDocumentOpen,
            },
        );
        let restarted = event(
            document,
            second_epoch,
            5,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::ExplicitDocumentOpen,
            },
        );
        let restarted_dcl = event(
            document,
            second_epoch,
            6,
            RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        );
        let mut slot = page_slot_with_attachment();
        slot.set_page_attachment_id_for_test(6);
        let navigation = DocumentNavigationToken::new("FRAME-11", "LOADER-11");
        assert_eq!(
            slot.bind_renderer_document_lifecycle(
                RendererPageCreationArtifacts {
                    active_document: document,
                    active_epoch: first_epoch,
                    lifecycle_snapshot: RendererDocumentLifecycleSnapshot {
                        frame: started.frame,
                        document,
                        epoch: first_epoch,
                        started: RendererLifecycleEventStamp {
                            sequence: 1,
                            timestamp_micros: 10,
                        },
                        dom_content_loaded: None,
                        load: None,
                        terminated: None,
                    },
                    initial_lifecycle_events: vec![started, dcl],
                },
                Some(navigation),
                "FRAME-11".to_owned(),
                "LOADER-11".to_owned(),
            ),
            vec![started, dcl]
        );
        assert!(slot.begin_renderer_document_load_visibility_barrier("LOADER-11"));
        assert!(
            slot.ingest_renderer_document_lifecycle_events(vec![
                load,
                terminated,
                restarted,
                restarted_dcl,
            ])
            .is_empty(),
            "nothing after the hidden load may overtake its visibility boundary"
        );

        let authoritative = slot
            .renderer_document_lifecycle_authoritative_snapshot()
            .expect("authoritative restarted lifecycle");
        assert_eq!(authoritative.epoch, second_epoch);
        assert_eq!(
            authoritative.dom_content_loaded,
            Some(RendererLifecycleEventStamp {
                sequence: 6,
                timestamp_micros: 60,
            })
        );
        let visible = slot
            .renderer_document_lifecycle_visible_snapshot()
            .expect("visible lifecycle before release");
        assert_eq!(visible.epoch, first_epoch);
        assert_eq!(visible.dom_content_loaded.unwrap().sequence, 2);
        assert_eq!(visible.load, None);

        assert_eq!(
            slot.release_renderer_document_load_visibility_barrier("LOADER-11"),
            Some(vec![load, terminated, restarted, restarted_dcl])
        );
        let visible = slot
            .renderer_document_lifecycle_visible_snapshot()
            .expect("visible lifecycle after release");
        assert_eq!(visible.epoch, second_epoch);
        assert_eq!(
            visible.dom_content_loaded,
            Some(RendererLifecycleEventStamp {
                sequence: 6,
                timestamp_micros: 60,
            })
        );
        assert_eq!(visible.load, None);
        assert_eq!(visible.terminated, None);
    }

    #[test]
    fn same_document_restart_advances_epoch_without_rebinding_loader() {
        let page_id = moli_core::PageId::new_for_testing(10);
        let document = RendererDocumentToken::new_for_testing(page_id, 1);
        let first_epoch = RendererLifecycleEpoch(1);
        let started = event(
            document,
            first_epoch,
            1,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::InitialDocument,
            },
        );
        let mut slot = page_slot_with_attachment();
        slot.bind_renderer_document_lifecycle(
            RendererPageCreationArtifacts {
                active_document: document,
                active_epoch: first_epoch,
                lifecycle_snapshot: RendererDocumentLifecycleSnapshot {
                    frame: started.frame,
                    document,
                    epoch: first_epoch,
                    started: RendererLifecycleEventStamp {
                        sequence: 1,
                        timestamp_micros: 10,
                    },
                    dom_content_loaded: None,
                    load: None,
                    terminated: None,
                },
                initial_lifecycle_events: vec![started],
            },
            None,
            "FRAME-10".to_owned(),
            "LOADER-10".to_owned(),
        );
        let terminated = event(
            document,
            first_epoch,
            2,
            RendererDocumentLifecycleEventKind::Terminated {
                last_reached: None,
                reason: RendererDocumentTerminationReason::RestartedByDocumentOpen,
            },
        );
        let second_epoch = RendererLifecycleEpoch(2);
        let restarted = event(
            document,
            second_epoch,
            3,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::ExplicitDocumentOpen,
            },
        );
        let dcl = event(
            document,
            second_epoch,
            4,
            RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        );
        assert_eq!(
            slot.ingest_renderer_document_lifecycle_events(vec![terminated, restarted, dcl]),
            vec![terminated, restarted, dcl]
        );
        assert_eq!(
            slot.renderer_document_lifecycle_binding()
                .unwrap()
                .renderer_epoch,
            second_epoch
        );
        assert_eq!(
            slot.renderer_document_lifecycle_authoritative_snapshot()
                .unwrap()
                .epoch,
            second_epoch
        );
    }

    #[test]
    fn creation_handoff_preserves_completed_epochs_before_the_active_epoch() {
        let page_id = moli_core::PageId::new_for_testing(11);
        let document = RendererDocumentToken::new_for_testing(page_id, 1);
        let first_epoch = RendererLifecycleEpoch(1);
        let second_epoch = RendererLifecycleEpoch(2);
        let first_started = event(
            document,
            first_epoch,
            1,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::InitialDocument,
            },
        );
        let first_dcl = event(
            document,
            first_epoch,
            2,
            RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        );
        let first_terminated = event(
            document,
            first_epoch,
            3,
            RendererDocumentLifecycleEventKind::Terminated {
                last_reached: Some(RendererDocumentLifecycleMilestone::DomContentLoaded),
                reason: RendererDocumentTerminationReason::RestartedByDocumentOpen,
            },
        );
        let second_started = event(
            document,
            second_epoch,
            4,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::ExplicitDocumentOpen,
            },
        );
        let second_dcl = event(
            document,
            second_epoch,
            5,
            RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        );
        let initial_events = vec![
            first_started,
            first_dcl,
            first_terminated,
            second_started,
            second_dcl,
        ];

        let mut slot = page_slot_with_attachment();
        let accepted = slot.bind_renderer_document_lifecycle(
            RendererPageCreationArtifacts {
                active_document: document,
                active_epoch: second_epoch,
                lifecycle_snapshot: RendererDocumentLifecycleSnapshot {
                    frame: second_started.frame,
                    document,
                    epoch: second_epoch,
                    started: RendererLifecycleEventStamp {
                        sequence: 4,
                        timestamp_micros: 40,
                    },
                    dom_content_loaded: Some(RendererLifecycleEventStamp {
                        sequence: 5,
                        timestamp_micros: 50,
                    }),
                    load: None,
                    terminated: None,
                },
                initial_lifecycle_events: initial_events.clone(),
            },
            None,
            "FRAME-11".to_owned(),
            "LOADER-11".to_owned(),
        );

        assert_eq!(accepted, initial_events);
        let snapshot = slot
            .renderer_document_lifecycle_authoritative_snapshot()
            .expect("active lifecycle snapshot");
        assert_eq!(snapshot.epoch, second_epoch);
        assert_eq!(snapshot.dom_content_loaded.unwrap().sequence, 5);
    }

    #[test]
    fn successor_document_binding_discards_deferred_tail_and_resets_projection() {
        let page_id = moli_core::PageId::new_for_testing(14);
        let document = RendererDocumentToken::new_for_testing(page_id, 1);
        let epoch = RendererLifecycleEpoch(1);
        let started = event(
            document,
            epoch,
            1,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::InitialDocument,
            },
        );
        let dcl = event(
            document,
            epoch,
            2,
            RendererDocumentLifecycleEventKind::Milestone(
                RendererDocumentLifecycleMilestone::DomContentLoaded,
            ),
        );
        let mut slot = page_slot_with_attachment();
        slot.bind_renderer_document_lifecycle(
            RendererPageCreationArtifacts {
                active_document: document,
                active_epoch: epoch,
                lifecycle_snapshot: RendererDocumentLifecycleSnapshot {
                    frame: started.frame,
                    document,
                    epoch,
                    started: RendererLifecycleEventStamp {
                        sequence: 1,
                        timestamp_micros: 10,
                    },
                    dom_content_loaded: Some(RendererLifecycleEventStamp {
                        sequence: 2,
                        timestamp_micros: 20,
                    }),
                    load: None,
                    terminated: None,
                },
                initial_lifecycle_events: vec![started, dcl],
            },
            None,
            "FRAME-14".to_owned(),
            "LOADER-14".to_owned(),
        );
        let load = event(
            document,
            epoch,
            3,
            RendererDocumentLifecycleEventKind::Milestone(RendererDocumentLifecycleMilestone::Load),
        );
        assert!(slot.begin_renderer_document_load_visibility_barrier("LOADER-14"));
        assert_eq!(
            slot.ingest_renderer_document_lifecycle_events(vec![load]),
            Vec::new()
        );
        assert!(
            slot.renderer_document_lifecycle_authoritative_snapshot()
                .is_some_and(|snapshot| snapshot.load.is_some())
        );
        assert!(
            slot.renderer_document_lifecycle_visible_snapshot()
                .is_some_and(|snapshot| snapshot.load.is_none())
        );

        let successor = RendererDocumentToken::new_for_testing(page_id, 2);
        let successor_epoch = RendererLifecycleEpoch(2);
        let successor_started = event(
            successor,
            successor_epoch,
            4,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::CrossDocumentCommit,
            },
        );
        slot.bind_renderer_document_lifecycle(
            RendererPageCreationArtifacts {
                active_document: successor,
                active_epoch: successor_epoch,
                lifecycle_snapshot: RendererDocumentLifecycleSnapshot {
                    frame: successor_started.frame,
                    document: successor,
                    epoch: successor_epoch,
                    started: RendererLifecycleEventStamp {
                        sequence: 4,
                        timestamp_micros: 40,
                    },
                    dom_content_loaded: None,
                    load: None,
                    terminated: None,
                },
                initial_lifecycle_events: vec![successor_started],
            },
            None,
            "FRAME-14".to_owned(),
            "LOADER-15".to_owned(),
        );

        assert!(!slot.renderer_document_load_visibility_barrier_active());
        assert!(
            slot.release_renderer_document_load_visibility_barrier("LOADER-14")
                .is_none(),
            "a successor binding must discard the previous document's deferred tail"
        );
        assert!(
            slot.renderer_document_lifecycle_visible_snapshot()
                .is_some_and(|snapshot| snapshot.document == successor && snapshot.load.is_none())
        );
    }

    #[test]
    fn post_load_observers_are_armed_once_and_bound_to_the_loaded_document() {
        let page_id = moli_core::PageId::new_for_testing(12);
        let document = RendererDocumentToken::new_for_testing(page_id, 1);
        let epoch = RendererLifecycleEpoch(1);
        let started = event(
            document,
            epoch,
            1,
            RendererDocumentLifecycleEventKind::Started {
                reason: RendererLifecycleStartReason::InitialDocument,
            },
        );
        let load = event(
            document,
            epoch,
            2,
            RendererDocumentLifecycleEventKind::Milestone(RendererDocumentLifecycleMilestone::Load),
        );
        let mut slot = page_slot_with_attachment();
        slot.bind_renderer_document_lifecycle(
            RendererPageCreationArtifacts {
                active_document: document,
                active_epoch: epoch,
                lifecycle_snapshot: RendererDocumentLifecycleSnapshot {
                    frame: started.frame,
                    document,
                    epoch,
                    started: RendererLifecycleEventStamp {
                        sequence: 1,
                        timestamp_micros: 10,
                    },
                    dom_content_loaded: None,
                    load: Some(RendererLifecycleEventStamp {
                        sequence: 2,
                        timestamp_micros: 20,
                    }),
                    terminated: None,
                },
                initial_lifecycle_events: vec![started, load],
            },
            None,
            "FRAME-12".to_owned(),
            "LOADER-12".to_owned(),
        );

        assert!(slot.arm_root_post_load_observation("LOADER-12"));
        assert!(!slot.arm_root_post_load_observation("LOADER-12"));
        assert!(slot.take_root_network_idle_binding(true).is_none());
        assert_eq!(
            slot.take_root_frame_stopped_loading_binding()
                .expect("frame-stop observation")
                .loader_id,
            "LOADER-12"
        );
        assert!(slot.take_root_frame_stopped_loading_binding().is_none());
        assert_eq!(
            slot.take_root_network_idle_binding(false)
                .expect("network-idle observation after provisional navigation failure")
                .frame_id,
            "FRAME-12"
        );
        assert!(slot.take_root_network_idle_binding(false).is_none());
    }
}
