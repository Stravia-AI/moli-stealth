use std::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use moli_page_types::{BrowserActionId, RendererAgentAttachmentId};
use moli_trace::{
    BrowserOwnerTraceDocument, BrowserOwnerTraceRecord, emit_browser_owner_trace_record,
};

use crate::{RendererDocumentLifecycleIdentity, browser_host::PageResidenceIdentity};

use super::{
    BrowserDocumentNavigation, BrowserNavigationOwner, BrowserNavigationRequestId,
    BrowserPageOwnerKey,
};

static NEXT_BROWSER_INSTANCE_ID: AtomicU64 = AtomicU64::new(1);

/// Process-local identity of one Browser Owner instance.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BrowserInstanceId(NonZeroU64);

impl BrowserInstanceId {
    pub(in crate::browser_host) fn allocate() -> Self {
        let raw = NEXT_BROWSER_INSTANCE_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("browser instance id exhausted"));
        Self(
            NonZeroU64::new(raw)
                .unwrap_or_else(|| panic!("browser instance id allocator returned zero")),
        )
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Producer category for a navigation trace event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserNavigationTraceSource {
    FrontendCommand,
    RendererIntent,
    Network,
    Lifecycle,
}

impl BrowserNavigationTraceSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::FrontendCommand => "frontend-command",
            Self::RendererIntent => "renderer-intent",
            Self::Network => "network",
            Self::Lifecycle => "lifecycle",
        }
    }
}

/// Bounded correlation sidecar for one browser navigation action.
///
/// The context contains no frontend/session identity. Browser Core stores it
/// only beside the exact pending/committed request, so replacement and target
/// cleanup retire it with the authority it describes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserNavigationTraceContext {
    browser_instance_id: BrowserInstanceId,
    browser_action_id: BrowserActionId,
    origin: BrowserNavigationTraceSource,
    source_page: PageResidenceIdentity,
    source_document: Option<RendererDocumentLifecycleIdentity>,
}

impl BrowserNavigationTraceContext {
    pub(super) fn new(
        browser_instance_id: BrowserInstanceId,
        browser_action_id: BrowserActionId,
        origin: BrowserNavigationTraceSource,
        source_page: PageResidenceIdentity,
        source_document: Option<RendererDocumentLifecycleIdentity>,
    ) -> Self {
        Self {
            browser_instance_id,
            browser_action_id,
            origin,
            source_page,
            source_document,
        }
    }

    pub fn browser_instance_id(&self) -> BrowserInstanceId {
        self.browser_instance_id
    }

    pub fn browser_action_id(&self) -> BrowserActionId {
        self.browser_action_id
    }

    pub fn origin(&self) -> BrowserNavigationTraceSource {
        self.origin
    }

    pub fn source_page(&self) -> &PageResidenceIdentity {
        &self.source_page
    }

    pub fn source_document(&self) -> Option<RendererDocumentLifecycleIdentity> {
        self.source_document
    }

    pub(super) fn addresses_owner(&self, owner: &BrowserPageOwnerKey) -> bool {
        self.source_page.browser_context_id() == owner.browser_context_id()
            && self.source_page.target_id() == Some(owner.target_id())
    }

    pub fn emit(&self, event: BrowserNavigationTraceEvent) {
        if !moli_trace::browser_owner_trace_enabled() {
            return;
        }
        let page = event.page.as_ref().unwrap_or(&self.source_page);
        let document = event.document.or(self.source_document);
        emit_browser_owner_trace_record(
            &BrowserOwnerTraceRecord::new(
                event.stage,
                event.source.label(),
                event.owner_state_before,
                event.owner_state_after,
            )
            .with_browser_instance_id(Some(self.browser_instance_id.get()))
            .with_browser_context_id(Some(page.browser_context_id()))
            .with_target_id(page.target_id())
            .with_page_residence_generation(Some(page.loaded_page_generation()))
            .with_navigation_request_id(
                event
                    .navigation_request_id
                    .map(BrowserNavigationRequestId::get),
            )
            .with_renderer_agent_attachment_id(
                event
                    .renderer_agent_attachment_id
                    .map(RendererAgentAttachmentId::get),
            )
            .with_document_lifecycle_identity(document.map(machine_trace_document))
            .with_browser_action_id(Some(self.browser_action_id.get()))
            .with_browser_fact_sequence(event.browser_fact_sequence)
            .with_navigation_origin(Some(self.origin.label()))
            .with_frontend_projection_sequence(event.frontend_projection_sequence)
            .with_renderer_lifecycle(
                event.renderer_lifecycle_sequence,
                None,
                None,
                None,
            ),
        );
        if !moli_trace::browser_owner_human_trace_enabled() {
            return;
        }
        tracing::info!(
            target: "moli_browser_owner",
            browser_instance_id = self.browser_instance_id.get(),
            browser_context_id = page.browser_context_id(),
            target_id = ?page.target_id(),
            page_residence_generation = page.loaded_page_generation(),
            navigation_request_id = ?event.navigation_request_id.map(BrowserNavigationRequestId::get),
            renderer_agent_attachment_id = ?event.renderer_agent_attachment_id.map(RendererAgentAttachmentId::get),
            document_lifecycle_identity = ?document,
            browser_action_id = self.browser_action_id.get(),
            browser_fact_sequence = ?event.browser_fact_sequence,
            source = event.source.label(),
            navigation_origin = self.origin.label(),
            owner_state_before = event.owner_state_before,
            owner_state_after = event.owner_state_after,
            frontend_projection_sequence = ?event.frontend_projection_sequence,
            renderer_lifecycle_sequence = ?event.renderer_lifecycle_sequence,
            stage = event.stage,
            "browser navigation owner trace"
        );
    }
}

fn machine_trace_document(
    identity: RendererDocumentLifecycleIdentity,
) -> BrowserOwnerTraceDocument {
    BrowserOwnerTraceDocument::new(
        identity.document.page_id.as_u64(),
        identity.document.lifecycle_document_id_for_diagnostics(),
        identity.epoch.0,
    )
}

impl BrowserNavigationOwner {
    /// Creates a trace context for an action already bound to an exact Page
    /// residence. The identity may later be rejected as stale; retaining it is
    /// what makes that rejection diagnosable.
    pub fn renderer_navigation_trace_context(
        &self,
        source_page: &PageResidenceIdentity,
        browser_action_id: BrowserActionId,
        source_document: RendererDocumentLifecycleIdentity,
    ) -> Option<BrowserNavigationTraceContext> {
        moli_trace::browser_owner_trace_enabled().then(|| {
            BrowserNavigationTraceContext::new(
                self.browser_instance_id,
                browser_action_id,
                BrowserNavigationTraceSource::RendererIntent,
                source_page.clone(),
                Some(source_document),
            )
        })
    }

    /// Creates a trace context for a browser/frontend action after its neutral
    /// Target owner has been resolved.
    pub fn owner_navigation_trace_context(
        &self,
        owner: &BrowserPageOwnerKey,
        browser_action_id: BrowserActionId,
        origin: BrowserNavigationTraceSource,
        source_document: Option<RendererDocumentLifecycleIdentity>,
    ) -> Option<BrowserNavigationTraceContext> {
        if !moli_trace::browser_owner_trace_enabled() {
            return None;
        }
        let source_page = self
            .page_residences
            .identity(&self.target_runtimes, owner)?;
        Some(BrowserNavigationTraceContext::new(
            self.browser_instance_id,
            browser_action_id,
            origin,
            source_page,
            source_document,
        ))
    }
}

/// One immutable trace transition emitted from the current production path.
#[derive(Clone, Debug)]
pub struct BrowserNavigationTraceEvent {
    stage: &'static str,
    source: BrowserNavigationTraceSource,
    owner_state_before: &'static str,
    owner_state_after: &'static str,
    navigation_request_id: Option<BrowserNavigationRequestId>,
    renderer_agent_attachment_id: Option<RendererAgentAttachmentId>,
    page: Option<PageResidenceIdentity>,
    document: Option<RendererDocumentLifecycleIdentity>,
    browser_fact_sequence: Option<u64>,
    frontend_projection_sequence: Option<u64>,
    renderer_lifecycle_sequence: Option<u64>,
}

impl BrowserNavigationTraceEvent {
    pub fn new(
        stage: &'static str,
        source: BrowserNavigationTraceSource,
        owner_state_before: &'static str,
        owner_state_after: &'static str,
    ) -> Self {
        Self {
            stage,
            source,
            owner_state_before,
            owner_state_after,
            navigation_request_id: None,
            renderer_agent_attachment_id: None,
            page: None,
            document: None,
            browser_fact_sequence: None,
            frontend_projection_sequence: None,
            renderer_lifecycle_sequence: None,
        }
    }

    pub fn with_navigation(mut self, navigation: &BrowserDocumentNavigation) -> Self {
        self.navigation_request_id = Some(navigation.request_id());
        self
    }

    pub fn with_renderer_agent_attachment(
        mut self,
        attachment_id: Option<RendererAgentAttachmentId>,
    ) -> Self {
        self.renderer_agent_attachment_id = attachment_id;
        self
    }

    pub fn with_page(mut self, page: PageResidenceIdentity) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_document(mut self, document: RendererDocumentLifecycleIdentity) -> Self {
        self.document = Some(document);
        self
    }

    pub fn with_browser_fact_sequence(mut self, sequence: u64) -> Self {
        self.browser_fact_sequence = Some(sequence);
        self
    }

    pub fn with_frontend_projection_sequence(mut self, sequence: u64) -> Self {
        self.frontend_projection_sequence = Some(sequence);
        self
    }

    pub fn with_renderer_lifecycle_sequence(mut self, sequence: u64) -> Self {
        self.renderer_lifecycle_sequence = Some(sequence);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_ids_are_nonzero_and_distinct() {
        let first = BrowserInstanceId::allocate();
        let second = BrowserInstanceId::allocate();

        assert_ne!(first, second);
        assert_ne!(first.get(), 0);
    }

    #[test]
    fn source_labels_are_stable_and_frontend_neutral() {
        assert_eq!(
            BrowserNavigationTraceSource::FrontendCommand.label(),
            "frontend-command"
        );
        assert_eq!(
            BrowserNavigationTraceSource::RendererIntent.label(),
            "renderer-intent"
        );
        assert_eq!(BrowserNavigationTraceSource::Network.label(), "network");
        assert_eq!(BrowserNavigationTraceSource::Lifecycle.label(), "lifecycle");
    }
}
