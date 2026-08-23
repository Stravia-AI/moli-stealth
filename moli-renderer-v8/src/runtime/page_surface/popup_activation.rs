use std::sync::Arc;

use super::{
    RendererDocumentLifecycleIdentity, RendererTopLevelNavigationRequest,
    RendererWindowDocumentSource,
};
use crate::{
    SharedWebStorageStore,
    runtime::{
        PageId, RendererOutputResidenceIdentity, RendererOwnerLocalHostId,
        RendererPendingAuxiliaryPage, RendererScriptAgentAdmission,
    },
};

/// Exact already-live renderer Page selected for a popup navigation.
///
/// Named browsing-context lookup is a renderer Page-group operation. Carrying
/// both residence coordinates prevents the protocol layer from repeating
/// that lookup through its eventually-consistent target-name projection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RendererResolvedPopupTarget {
    owner_local_host_id: RendererOwnerLocalHostId,
    page_id: PageId,
}

/// Renderer-decided browsing-context-group policy for one newly created
/// auxiliary Page.
///
/// This is decided beside named-target lookup. The protocol layer may adopt
/// the reserved Page and expose a DevTools target, but must not infer whether
/// that Page can participate in the creator's related-name lookup from a
/// later target-name projection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RendererPopupNewTargetDisposition {
    /// The new Page remains in the creator's related Page group.
    Related,
    /// Opener suppression created a fresh group without a browsing-context
    /// name (`""` or `_blank`).
    FreshUnnamed,
    /// Opener suppression created a fresh group whose first realm must retain
    /// the requested ordinary browsing-context name.
    FreshNamed,
}

impl RendererPopupNewTargetDisposition {
    pub const fn is_fresh(self) -> bool {
        matches!(self, Self::FreshUnnamed | Self::FreshNamed)
    }

    pub const fn carries_initial_name(self) -> bool {
        matches!(self, Self::FreshNamed)
    }
}

impl RendererResolvedPopupTarget {
    pub(crate) const fn from_residence(residence: RendererOutputResidenceIdentity) -> Option<Self> {
        match residence {
            RendererOutputResidenceIdentity::Page {
                owner_local_host_id,
                page_id,
            } => Some(Self {
                owner_local_host_id,
                page_id,
            }),
            RendererOutputResidenceIdentity::SharedWorker { .. }
            | RendererOutputResidenceIdentity::ServiceWorker { .. } => None,
        }
    }

    pub const fn owner_local_host_id(self) -> RendererOwnerLocalHostId {
        self.owner_local_host_id
    }

    pub const fn page_id(self) -> PageId {
        self.page_id
    }
}

/// Exact renderer-side initiator of one auxiliary browsing-context action.
///
/// Window-originated actions retain the root lifecycle identity as causal
/// metadata plus the concrete source Window/Document. `exposes_opener`
/// records the already-decided `noopener`/`noreferrer` policy; protocol code
/// must not reconstruct it from a later target or DOM state.
///
/// Browser-context actions are produced by APIs such as
/// `Clients.openWindow()` and notification navigation. They intentionally have
/// no Window opener and must not be projected as if the current root frame had
/// initiated them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RendererPopupActivationSource {
    Window {
        root_document: RendererDocumentLifecycleIdentity,
        window: RendererWindowDocumentSource,
        exposes_opener: bool,
    },
    BrowserContext,
}

/// Browser-owner selection policy for an accepted auxiliary browsing context.
///
/// This records only whether the target should become the active target. It
/// deliberately does not distinguish tab and window chrome, which the
/// renderer target model does not expose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RendererPopupDisposition {
    Foreground,
    Background,
}

/// A renderer-accepted request to create or reuse an auxiliary browsing
/// context.
///
/// Special targets (`_self`, `_parent`, `_top`) are not valid values here:
/// they navigate an existing browsing context and use the corresponding
/// navigation authority instead. Keeping this carrier auxiliary-only prevents
/// protocol code from deciding the target from a later current session.
#[derive(Debug, Clone)]
pub struct RendererPendingPopupActivation {
    source: RendererPopupActivationSource,
    disposition: RendererPopupDisposition,
    popup_id: Option<u64>,
    request: Box<RendererTopLevelNavigationRequest>,
    target_name: String,
    /// Heap-owned so adding popup policy facts does not inflate every
    /// `RendererOwnerAction` variant and its async orchestration frames.
    referrers: Option<Box<RendererPopupNavigationReferrers>>,
    pending_auxiliary_page: Option<RendererPendingAuxiliaryPage>,
    resolved_target_page: Option<RendererResolvedPopupTarget>,
    new_target_disposition: Option<RendererPopupNewTargetDisposition>,
    session_storage_store: Option<SharedWebStorageStore>,
    initial_empty_document_storage_key: Option<moli_storage_key::MoliStorageKey>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RendererPopupNavigationReferrers {
    /// Frozen Referer value for the destination navigation.
    ///
    /// `Some("")` explicitly suppresses the header. `None` is reserved for
    /// legacy/browser-context producers that still rely on target-local
    /// inference; protocol code must otherwise use this creator-side result
    /// instead of deriving a referrer from the popup's initial about:blank.
    navigation: String,
    /// Frozen referrer for the auxiliary context's initial empty Document.
    ///
    /// This is the creator's full URL unless `noreferrer` applies, independent
    /// of HTTP header eligibility and the destination navigation policy.
    initial_document: String,
    /// Frozen script-visible referrer for the committed destination Document.
    ///
    /// This differs from `navigation_referrer` for non-HTTP destinations. In
    /// particular, a noopener initial `about:blank` keeps the creator's full
    /// URL even though no HTTP Referer header can be emitted.
    document: String,
}

impl RendererPendingPopupActivation {
    pub fn window(
        root_document: RendererDocumentLifecycleIdentity,
        window: RendererWindowDocumentSource,
        exposes_opener: bool,
        popup_id: Option<u64>,
        url: String,
        target_name: String,
        disposition: RendererPopupDisposition,
    ) -> Self {
        assert!(
            !is_special_browsing_context_target(&target_name),
            "popup activation must not carry an existing-context special target"
        );
        Self {
            source: RendererPopupActivationSource::Window {
                root_document,
                window,
                exposes_opener,
            },
            disposition,
            popup_id,
            request: Box::new(RendererTopLevelNavigationRequest::get(url)),
            target_name,
            referrers: None,
            pending_auxiliary_page: None,
            resolved_target_page: None,
            new_target_disposition: None,
            session_storage_store: None,
            initial_empty_document_storage_key: None,
        }
    }

    pub fn browser_context(
        popup_id: Option<u64>,
        url: String,
        target_name: String,
        disposition: RendererPopupDisposition,
    ) -> Self {
        assert!(
            !is_special_browsing_context_target(&target_name),
            "browser-context popup activation must not carry a special target"
        );
        Self {
            source: RendererPopupActivationSource::BrowserContext,
            disposition,
            popup_id,
            request: Box::new(RendererTopLevelNavigationRequest::get(url)),
            target_name,
            referrers: None,
            pending_auxiliary_page: None,
            resolved_target_page: None,
            new_target_disposition: None,
            session_storage_store: None,
            initial_empty_document_storage_key: None,
        }
    }

    /// Attaches the state captured when the auxiliary browsing context was
    /// accepted in the renderer.
    ///
    /// The cloned session-storage namespace and initial about:blank storage
    /// key belong to this exact popup action. They must travel with the action
    /// rather than be reconstructed from whichever target is current when
    /// protocol output is emitted. `Page.windowOpen` is a separate concrete
    /// observation recorded beside this action at the renderer production
    /// boundary; it must not be hidden inside an after-response owner action.
    pub fn with_initial_auxiliary_state(
        mut self,
        session_storage_store: Option<SharedWebStorageStore>,
        initial_empty_document_storage_key: Option<moli_storage_key::MoliStorageKey>,
    ) -> Self {
        self.session_storage_store = session_storage_store;
        self.initial_empty_document_storage_key = initial_empty_document_storage_key;
        self
    }

    /// Replaces the default GET navigation with the exact request selected by
    /// the renderer producer.
    ///
    /// The URL must remain the one used for synchronous target creation and
    /// `Page.windowOpen`; only the request metadata is enriched. Keeping this
    /// request whole is required for auxiliary form POSTs.
    pub fn with_navigation_request(mut self, request: RendererTopLevelNavigationRequest) -> Self {
        assert_eq!(
            self.request.url(),
            request.url(),
            "popup target selection and navigation request must carry one URL"
        );
        self.request = Box::new(request);
        self
    }

    /// Binds the renderer-owned browsing-context and Page identities reserved
    /// when this action synchronously created a new auxiliary context.
    pub fn with_pending_auxiliary_page(
        mut self,
        pending_auxiliary_page: Option<RendererPendingAuxiliaryPage>,
    ) -> Self {
        assert!(
            pending_auxiliary_page.is_none() || self.resolved_target_page.is_none(),
            "a popup action cannot create and reuse a renderer Page"
        );
        self.pending_auxiliary_page = pending_auxiliary_page;
        self
    }

    /// Binds the exact already-live renderer Page selected by related-page
    /// named browsing-context lookup.
    pub fn with_resolved_target_page(
        mut self,
        resolved_target_page: RendererResolvedPopupTarget,
    ) -> Self {
        assert!(
            self.pending_auxiliary_page.is_none(),
            "a popup action cannot reuse and create a renderer Page"
        );
        self.resolved_target_page = Some(resolved_target_page);
        self
    }

    /// Records that the renderer completed target lookup and deliberately
    /// selected the attached new Page reservation.
    ///
    /// Unmigrated producers may still reserve a Page before the protocol's
    /// legacy name projection chooses between new and existing targets. They
    /// must not set this fact.
    pub fn with_new_target_disposition(
        mut self,
        disposition: RendererPopupNewTargetDisposition,
    ) -> Self {
        assert!(
            self.pending_auxiliary_page.is_some(),
            "a renderer-selected new popup target requires its Page reservation"
        );
        assert!(
            self.resolved_target_page.is_none(),
            "an existing popup target cannot also be selected as new"
        );
        assert!(
            !disposition.carries_initial_name()
                || (!self.target_name.is_empty()
                    && !is_special_browsing_context_target(&self.target_name)),
            "a fresh named popup requires an ordinary browsing-context name"
        );
        let admission = self
            .pending_auxiliary_page
            .expect("new popup target reservation")
            .page_reservation()
            .script_agent_admission();
        assert!(
            matches!(
                (disposition, admission),
                (
                    RendererPopupNewTargetDisposition::Related,
                    RendererScriptAgentAdmission::RelatedAuxiliaryPage { .. }
                ) | (
                    RendererPopupNewTargetDisposition::FreshUnnamed
                        | RendererPopupNewTargetDisposition::FreshNamed,
                    RendererScriptAgentAdmission::Fresh
                )
            ),
            "popup group disposition must match its Page reservation admission"
        );
        assert!(
            !disposition.is_fresh() || self.popup_id.is_none(),
            "a Fresh popup Page cannot retain an opener-local lightweight owner"
        );
        self.new_target_disposition = Some(disposition);
        self
    }

    /// Attaches the creator-resolved network, initial-empty-Document, and
    /// destination-Document referrers for this exact activation.
    pub fn with_navigation_referrers(
        mut self,
        navigation_referrer: String,
        initial_document_referrer: String,
        document_referrer: String,
    ) -> Self {
        self.referrers = Some(Box::new(RendererPopupNavigationReferrers {
            navigation: navigation_referrer,
            initial_document: initial_document_referrer,
            document: document_referrer,
        }));
        self
    }

    pub fn source(&self) -> &RendererPopupActivationSource {
        &self.source
    }

    pub fn disposition(&self) -> RendererPopupDisposition {
        self.disposition
    }

    pub fn popup_id(&self) -> Option<u64> {
        self.popup_id
    }

    pub fn url(&self) -> &str {
        self.request.url()
    }

    pub fn request_method(&self) -> &str {
        self.request.request_method()
    }

    pub fn request_body(&self) -> Option<&[u8]> {
        self.request.request_body()
    }

    pub fn request_headers(&self) -> &[(String, String)] {
        self.request.request_headers()
    }

    pub fn browser_navigation_kind(&self) -> moli_fetch::BrowserNavigationRequestKind {
        self.request.browser_navigation_kind()
    }

    pub fn target_name(&self) -> &str {
        &self.target_name
    }

    pub fn navigation_referrer(&self) -> Option<&str> {
        self.referrers
            .as_deref()
            .map(|referrers| referrers.navigation.as_str())
    }

    pub fn initial_document_referrer(&self) -> Option<&str> {
        self.referrers
            .as_deref()
            .map(|referrers| referrers.initial_document.as_str())
    }

    pub fn document_referrer(&self) -> Option<&str> {
        self.referrers
            .as_deref()
            .map(|referrers| referrers.document.as_str())
    }

    pub fn pending_auxiliary_page(&self) -> Option<RendererPendingAuxiliaryPage> {
        self.pending_auxiliary_page
    }

    pub fn resolved_target_page(&self) -> Option<RendererResolvedPopupTarget> {
        self.resolved_target_page
    }

    pub fn new_target_disposition(&self) -> Option<RendererPopupNewTargetDisposition> {
        self.new_target_disposition
    }

    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        self,
    ) -> (
        RendererPopupActivationSource,
        RendererPopupDisposition,
        Option<u64>,
        RendererTopLevelNavigationRequest,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<RendererPendingAuxiliaryPage>,
        Option<RendererResolvedPopupTarget>,
        Option<RendererPopupNewTargetDisposition>,
        Option<SharedWebStorageStore>,
        Option<moli_storage_key::MoliStorageKey>,
    ) {
        let (navigation_referrer, initial_document_referrer, document_referrer) = self
            .referrers
            .map(|referrers| {
                (
                    Some(referrers.navigation),
                    Some(referrers.initial_document),
                    Some(referrers.document),
                )
            })
            .unwrap_or((None, None, None));
        (
            self.source,
            self.disposition,
            self.popup_id,
            *self.request,
            self.target_name,
            navigation_referrer,
            initial_document_referrer,
            document_referrer,
            self.pending_auxiliary_page,
            self.resolved_target_page,
            self.new_target_disposition,
            self.session_storage_store,
            self.initial_empty_document_storage_key,
        )
    }
}

impl PartialEq for RendererPendingPopupActivation {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
            && self.disposition == other.disposition
            && self.popup_id == other.popup_id
            && self.request == other.request
            && self.target_name == other.target_name
            && self.referrers == other.referrers
            && self.pending_auxiliary_page == other.pending_auxiliary_page
            && self.resolved_target_page == other.resolved_target_page
            && self.new_target_disposition == other.new_target_disposition
            && match (&self.session_storage_store, &other.session_storage_store) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            }
            && self.initial_empty_document_storage_key == other.initial_empty_document_storage_key
    }
}

impl Eq for RendererPendingPopupActivation {}

fn is_special_browsing_context_target(target_name: &str) -> bool {
    target_name.eq_ignore_ascii_case("_self")
        || target_name.eq_ignore_ascii_case("_parent")
        || target_name.eq_ignore_ascii_case("_top")
}
