use std::num::NonZeroU64;

use crate::page::RendererDocumentSourcedTopLevelLocationNavigation;

use super::{
    BrowserAuxiliaryNavigationInput, BrowserAuxiliaryNavigationKind, BrowserContextHandle,
    BrowserHistoryTraversalDestination, BrowserInitialTargetNavigationCommandInput,
    BrowserInitialTargetNavigationInput, BrowserNavigationTraceContext,
    BrowserTargetTerminationRequest, PageResidenceIdentity,
};

/// Protocol-neutral input accepted by the Browser Owner execution lane.
///
/// This value deliberately has no frontend session, command id, domain
/// subscription, output queue or socket state. Frontends and renderer output
/// projectors may publish it, but only the Browser Owner queue may consume it.
#[derive(Debug)]
pub enum BrowserOwnerInput {
    FrontendCommand(BrowserFrontendCommand),
    RendererIntent(RendererBrowserIntent),
    InitialTargetNavigation(BrowserInitialTargetNavigationInput),
    PageTermination(BrowserPageTerminationInput),
    TargetTermination(BrowserTargetTerminationInput),
}

impl BrowserOwnerInput {
    pub fn frontend_navigate(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        url: String,
        referrer: Option<String>,
    ) -> Self {
        Self::FrontendCommand(BrowserFrontendCommand::Navigate(
            BrowserNavigateCommandInput::new(command_id, page_owner, url, referrer),
        ))
    }

    pub fn frontend_reload(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        ignore_cache: bool,
        script_to_evaluate_on_load: Option<String>,
    ) -> Self {
        Self::FrontendCommand(BrowserFrontendCommand::Reload(
            BrowserReloadCommandInput::new(
                command_id,
                page_owner,
                ignore_cache,
                script_to_evaluate_on_load,
            ),
        ))
    }

    pub fn frontend_history_traversal_to_entry(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        entry_id: i32,
    ) -> Self {
        Self::frontend_history_traversal(
            command_id,
            page_owner,
            BrowserHistoryTraversalDestination::Entry(entry_id),
        )
    }

    pub fn frontend_history_traversal_by_delta(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        delta: i64,
    ) -> Self {
        Self::frontend_history_traversal(
            command_id,
            page_owner,
            BrowserHistoryTraversalDestination::Delta(delta),
        )
    }

    pub fn frontend_history_traversal(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        destination: BrowserHistoryTraversalDestination,
    ) -> Self {
        Self::FrontendCommand(BrowserFrontendCommand::TraverseHistory(
            BrowserHistoryTraversalCommandInput::new(command_id, page_owner, destination),
        ))
    }

    /// Admits one frontend request to stop the currently loading top-level
    /// Document in an existing browsing-context slot.
    ///
    /// Unlike a navigation completion, stop-loading is slot-scoped: Browser
    /// Owner resolves the current Document only after this command wins its
    /// turn. The captured slot instance still rejects a removed/recreated
    /// Target that reuses the same public id.
    pub fn frontend_stop_loading(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
    ) -> Self {
        Self::FrontendCommand(BrowserFrontendCommand::StopLoading(
            BrowserStopLoadingCommandInput::new(command_id, page_owner),
        ))
    }

    /// Admits a frontend dependency on replacement of one exact Target's
    /// initial empty Document.
    ///
    /// The opaque command id recovers only the frontend continuation. Browser
    /// Owner selects and revalidates the exact Page and immutable creation URL
    /// before any navigation participant starts.
    pub fn frontend_ensure_initial_target_navigation(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        url: String,
    ) -> Self {
        Self::FrontendCommand(BrowserFrontendCommand::EnsureInitialTargetNavigation(
            BrowserInitialTargetNavigationCommandInput::new(command_id, page_owner, url),
        ))
    }

    /// Admits disposal of one exact BrowserContext instance.
    ///
    /// The public context id remains reusable after disposal, so queued work
    /// must carry the Core-issued instance capability. Frontend response
    /// correlation and protocol event routing remain outside Browser Owner.
    pub fn frontend_dispose_browser_context(
        command_id: BrowserCommandId,
        browser_context_handle: BrowserContextHandle,
    ) -> Self {
        Self::FrontendCommand(BrowserFrontendCommand::DisposeBrowserContext(
            BrowserContextDisposalCommandInput::new(command_id, browser_context_handle),
        ))
    }

    /// Admits one frontend decision for an already-paused top-level
    /// navigation.
    ///
    /// The paused request payload and frontend response route remain outside
    /// Core. Browser Owner receives only the exact Page capability and the
    /// browser-level decision that must be serialized with replacement and
    /// termination work for that Page.
    pub fn frontend_paused_navigation_decision(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        decision: BrowserPausedNavigationDecision,
    ) -> Self {
        Self::FrontendCommand(BrowserFrontendCommand::ResolvePausedNavigation(
            BrowserPausedNavigationDecisionInput::new(command_id, page_owner, decision),
        ))
    }

    pub fn renderer_top_level_location_navigation(
        page_owner: PageResidenceIdentity,
        navigation: RendererDocumentSourcedTopLevelLocationNavigation,
        trace: Option<BrowserNavigationTraceContext>,
    ) -> Self {
        Self::RendererIntent(RendererBrowserIntent::TopLevelLocationNavigation(
            RendererTopLevelLocationNavigationInput::new(page_owner, navigation, trace),
        ))
    }

    /// Admits one renderer-requested joint session-history traversal.
    ///
    /// The renderer contributes only its relative delta. Browser Owner
    /// resolves that delta against the authoritative history cursor after the
    /// exact Page wins its mailbox turn; no frontend route participates.
    pub fn renderer_top_level_history_traversal(
        page_owner: PageResidenceIdentity,
        delta: i64,
    ) -> Self {
        Self::RendererIntent(RendererBrowserIntent::TopLevelHistoryTraversal(
            RendererTopLevelHistoryTraversalInput::new(page_owner, delta),
        ))
    }

    /// Admits one renderer-accepted auxiliary browsing-context navigation.
    ///
    /// Target creation or named-target resolution has already occurred. Only
    /// the exact Page, destination and browser-level navigation kind enter the
    /// mailbox; opener/frontend routing remains outside Browser Owner.
    pub fn renderer_auxiliary_navigation(
        page_owner: PageResidenceIdentity,
        url: String,
        kind: BrowserAuxiliaryNavigationKind,
    ) -> Self {
        Self::RendererIntent(RendererBrowserIntent::AuxiliaryNavigation(
            BrowserAuxiliaryNavigationInput::new(page_owner, url, kind),
        ))
    }

    /// Admits replacement of one Target's initial empty Document with its
    /// immutable creation URL.
    ///
    /// Target creation, debugger resume, and Page-domain enable may all expose
    /// the same browser transition. They publish this one neutral input; the
    /// selected Browser Host turn revalidates exact Page and initial-Document
    /// state before it starts any network or renderer participant.
    pub fn initial_target_navigation(page_owner: PageResidenceIdentity, url: String) -> Self {
        Self::InitialTargetNavigation(BrowserInitialTargetNavigationInput::new(page_owner, url))
    }

    /// Admits one exact Page-originated terminal transition.
    ///
    /// The request already captures BrowserContext, Target and Page generation
    /// authority. Protocol may delay publication until an exact renderer
    /// output predecessor has crossed ingress, but no frontend identity or
    /// response route enters the Browser Host mailbox.
    pub fn page_termination(request: BrowserTargetTerminationRequest) -> Self {
        Self::PageTermination(BrowserPageTerminationInput::new(request))
    }

    /// Admits one exact browser Target terminal transition.
    ///
    /// Unlike `PageTermination`, this action removes the Target topology and
    /// may promote a retained background Target after the exact close commits.
    /// Frontend response correlation and Target-domain event projection remain
    /// outside this protocol-neutral input.
    pub fn target_termination(request: BrowserTargetTerminationRequest) -> Self {
        Self::TargetTermination(BrowserTargetTerminationInput::new(request))
    }

    pub fn kind(&self) -> BrowserOwnerInputKind {
        match self {
            Self::FrontendCommand(BrowserFrontendCommand::Navigate(_)) => {
                BrowserOwnerInputKind::FrontendNavigate
            }
            Self::FrontendCommand(BrowserFrontendCommand::Reload(_)) => {
                BrowserOwnerInputKind::FrontendReload
            }
            Self::FrontendCommand(BrowserFrontendCommand::TraverseHistory(_)) => {
                BrowserOwnerInputKind::FrontendHistoryTraversal
            }
            Self::FrontendCommand(BrowserFrontendCommand::StopLoading(_)) => {
                BrowserOwnerInputKind::FrontendStopLoading
            }
            Self::FrontendCommand(BrowserFrontendCommand::DisposeBrowserContext(_)) => {
                BrowserOwnerInputKind::FrontendDisposeBrowserContext
            }
            Self::FrontendCommand(BrowserFrontendCommand::ResolvePausedNavigation(_)) => {
                BrowserOwnerInputKind::FrontendPausedNavigationDecision
            }
            Self::FrontendCommand(BrowserFrontendCommand::EnsureInitialTargetNavigation(_)) => {
                BrowserOwnerInputKind::FrontendEnsureInitialTargetNavigation
            }
            Self::RendererIntent(RendererBrowserIntent::TopLevelLocationNavigation(_)) => {
                BrowserOwnerInputKind::RendererTopLevelLocationNavigation
            }
            Self::RendererIntent(RendererBrowserIntent::TopLevelHistoryTraversal(_)) => {
                BrowserOwnerInputKind::RendererTopLevelHistoryTraversal
            }
            Self::RendererIntent(RendererBrowserIntent::AuxiliaryNavigation(_)) => {
                BrowserOwnerInputKind::RendererAuxiliaryNavigation
            }
            Self::InitialTargetNavigation(_) => BrowserOwnerInputKind::InitialTargetNavigation,
            Self::PageTermination(_) => BrowserOwnerInputKind::PageTermination,
            Self::TargetTermination(_) => BrowserOwnerInputKind::TargetTermination,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserOwnerInputKind {
    FrontendNavigate,
    FrontendReload,
    FrontendHistoryTraversal,
    FrontendStopLoading,
    FrontendDisposeBrowserContext,
    FrontendPausedNavigationDecision,
    FrontendEnsureInitialTargetNavigation,
    RendererTopLevelLocationNavigation,
    RendererTopLevelHistoryTraversal,
    RendererAuxiliaryNavigation,
    InitialTargetNavigation,
    PageTermination,
    TargetTermination,
}

/// Exact Page terminal action selected by Browser Host.
///
/// `BrowserTargetTerminationRequest` is move-only and generation-scoped, so a
/// delayed Page.close/Page.crash cannot be retargeted through a later session
/// lookup or terminate a replacement Document.
#[derive(Debug)]
pub struct BrowserPageTerminationInput {
    request: BrowserTargetTerminationRequest,
}

impl BrowserPageTerminationInput {
    fn new(request: BrowserTargetTerminationRequest) -> Self {
        Self { request }
    }

    pub fn request(&self) -> &BrowserTargetTerminationRequest {
        &self.request
    }

    pub fn into_request(self) -> BrowserTargetTerminationRequest {
        self.request
    }
}

/// Exact top-level Target close selected by Browser Host.
///
/// The request freezes BrowserContext, Target instance and Page generation.
/// A delayed `Target.closeTarget` therefore cannot be redirected through a
/// later frontend attachment or close a replacement Target with the same id.
#[derive(Debug)]
pub struct BrowserTargetTerminationInput {
    request: BrowserTargetTerminationRequest,
}

impl BrowserTargetTerminationInput {
    fn new(request: BrowserTargetTerminationRequest) -> Self {
        Self { request }
    }

    pub fn request(&self) -> &BrowserTargetTerminationRequest {
        &self.request
    }

    pub fn into_request(self) -> BrowserTargetTerminationRequest {
        self.request
    }
}

/// Opaque correlation identity for one protocol-neutral Browser command.
///
/// The identity does not encode a CDP request id, frontend session or output
/// route. During migration the physical executor uses it only to recover the
/// frontend projection that was prepared before the command entered the
/// Browser Host mailbox.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BrowserCommandId(NonZeroU64);

impl BrowserCommandId {
    pub fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Browser command admitted from any frontend.
///
/// Frontend correlation and response projection deliberately remain outside
/// this value. The Browser Owner receives only the action and its exact Page
/// authority.
#[derive(Debug)]
pub enum BrowserFrontendCommand {
    Navigate(BrowserNavigateCommandInput),
    Reload(BrowserReloadCommandInput),
    TraverseHistory(BrowserHistoryTraversalCommandInput),
    StopLoading(BrowserStopLoadingCommandInput),
    DisposeBrowserContext(BrowserContextDisposalCommandInput),
    ResolvePausedNavigation(BrowserPausedNavigationDecisionInput),
    EnsureInitialTargetNavigation(BrowserInitialTargetNavigationCommandInput),
}

/// Exact BrowserContext-scoped disposal command.
///
/// A frontend-visible id alone is insufficient because it may be reused
/// while this command waits behind earlier Browser Owner work. The handle is
/// move-owned by the selected turn and cannot authorize a later Context with
/// the same id.
#[derive(Debug)]
pub struct BrowserContextDisposalCommandInput {
    command_id: BrowserCommandId,
    browser_context_handle: BrowserContextHandle,
}

impl BrowserContextDisposalCommandInput {
    fn new(command_id: BrowserCommandId, browser_context_handle: BrowserContextHandle) -> Self {
        Self {
            command_id,
            browser_context_handle,
        }
    }

    pub fn command_id(&self) -> BrowserCommandId {
        self.command_id
    }

    pub fn browser_context_handle(&self) -> &BrowserContextHandle {
        &self.browser_context_handle
    }

    pub fn into_parts(self) -> (BrowserCommandId, BrowserContextHandle) {
        (self.command_id, self.browser_context_handle)
    }
}

/// Exact Page-scoped top-level navigation command.
///
/// The Browser Owner classifies the command as same-Document or
/// cross-Document only after selecting and validating this exact Page.
#[derive(Debug)]
pub struct BrowserNavigateCommandInput {
    command_id: BrowserCommandId,
    page_owner: PageResidenceIdentity,
    url: String,
    referrer: Option<String>,
}

impl BrowserNavigateCommandInput {
    fn new(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        url: String,
        referrer: Option<String>,
    ) -> Self {
        Self {
            command_id,
            page_owner,
            url,
            referrer,
        }
    }

    pub fn command_id(&self) -> BrowserCommandId {
        self.command_id
    }

    pub fn page_owner(&self) -> &PageResidenceIdentity {
        &self.page_owner
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn referrer(&self) -> Option<&str> {
        self.referrer.as_deref()
    }

    pub fn into_parts(
        self,
    ) -> (
        BrowserCommandId,
        PageResidenceIdentity,
        String,
        Option<String>,
    ) {
        (self.command_id, self.page_owner, self.url, self.referrer)
    }
}

/// Exact Page-scoped top-level reload command.
///
/// The current URL is deliberately absent. Browser Owner resolves it only
/// after selecting and validating `page_owner`, so a queued reload cannot be
/// retargeted to a replacement Page.
#[derive(Debug)]
pub struct BrowserReloadCommandInput {
    command_id: BrowserCommandId,
    page_owner: PageResidenceIdentity,
    ignore_cache: bool,
    script_to_evaluate_on_load: Option<String>,
}

impl BrowserReloadCommandInput {
    fn new(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        ignore_cache: bool,
        script_to_evaluate_on_load: Option<String>,
    ) -> Self {
        Self {
            command_id,
            page_owner,
            ignore_cache,
            script_to_evaluate_on_load,
        }
    }

    pub fn command_id(&self) -> BrowserCommandId {
        self.command_id
    }

    pub fn page_owner(&self) -> &PageResidenceIdentity {
        &self.page_owner
    }

    pub fn ignore_cache(&self) -> bool {
        self.ignore_cache
    }

    pub fn script_to_evaluate_on_load(&self) -> Option<&str> {
        self.script_to_evaluate_on_load.as_deref()
    }

    pub fn into_parts(
        self,
    ) -> (
        BrowserCommandId,
        PageResidenceIdentity,
        bool,
        Option<String>,
    ) {
        (
            self.command_id,
            self.page_owner,
            self.ignore_cache,
            self.script_to_evaluate_on_load,
        )
    }
}

/// Exact Page-scoped joint session-history traversal command.
///
/// Only an entry identity or relative delta crosses the frontend boundary.
/// Destination URL lookup, cursor-relative delta resolution and
/// same/cross-Document classification happen after Browser Owner selects and
/// validates `page_owner`.
#[derive(Debug)]
pub struct BrowserHistoryTraversalCommandInput {
    command_id: BrowserCommandId,
    page_owner: PageResidenceIdentity,
    destination: BrowserHistoryTraversalDestination,
}

impl BrowserHistoryTraversalCommandInput {
    fn new(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        destination: BrowserHistoryTraversalDestination,
    ) -> Self {
        Self {
            command_id,
            page_owner,
            destination,
        }
    }

    pub fn command_id(&self) -> BrowserCommandId {
        self.command_id
    }

    pub fn page_owner(&self) -> &PageResidenceIdentity {
        &self.page_owner
    }

    pub fn destination(&self) -> BrowserHistoryTraversalDestination {
        self.destination
    }

    pub fn into_parts(
        self,
    ) -> (
        BrowserCommandId,
        PageResidenceIdentity,
        BrowserHistoryTraversalDestination,
    ) {
        (self.command_id, self.page_owner, self.destination)
    }
}

/// Target/Page-slot-scoped request to stop the active top-level load.
///
/// The instance capability in `page_owner` identifies the slot captured by the
/// frontend. Its generation records the Document current at admission but does
/// not freeze that Document: Browser Owner deliberately resolves the slot's
/// current generation when the command is selected.
#[derive(Debug)]
pub struct BrowserStopLoadingCommandInput {
    command_id: BrowserCommandId,
    page_owner: PageResidenceIdentity,
}

impl BrowserStopLoadingCommandInput {
    fn new(command_id: BrowserCommandId, page_owner: PageResidenceIdentity) -> Self {
        Self {
            command_id,
            page_owner,
        }
    }

    pub fn command_id(&self) -> BrowserCommandId {
        self.command_id
    }

    pub fn page_owner(&self) -> &PageResidenceIdentity {
        &self.page_owner
    }

    pub fn into_parts(self) -> (BrowserCommandId, PageResidenceIdentity) {
        (self.command_id, self.page_owner)
    }
}

/// Protocol-neutral resolution of one paused top-level navigation.
///
/// This enum is deliberately about browser behavior, not Fetch/CDP command
/// spelling. Later frontends can publish the same decision without entering
/// Browser Owner's execution state.
#[derive(Debug)]
pub enum BrowserPausedNavigationDecision {
    Fail { error_text: String },
    Continue(BrowserPausedNavigationContinueDecision),
    ContinueResponse(BrowserPausedNavigationResponseDecision),
    Fulfill(BrowserPausedNavigationFulfillDecision),
    Auth(BrowserPausedNavigationAuthDecision),
}

impl BrowserPausedNavigationDecision {
    pub fn fail(error_text: String) -> Self {
        Self::Fail { error_text }
    }

    pub fn continue_request(
        url: Option<url::Url>,
        method: Option<String>,
        post_data: Option<String>,
        headers: Option<Vec<(String, String)>>,
        intercept_response: bool,
    ) -> Self {
        Self::Continue(BrowserPausedNavigationContinueDecision {
            url,
            method,
            post_data,
            headers,
            intercept_response,
        })
    }

    pub fn continue_response(
        response_code: Option<u16>,
        response_headers: Vec<(String, String)>,
    ) -> Self {
        Self::ContinueResponse(BrowserPausedNavigationResponseDecision {
            response_code,
            response_headers,
        })
    }

    pub fn fulfill(
        response_code: u16,
        response_headers: Vec<(String, String)>,
        response_body: Option<Vec<u8>>,
    ) -> Self {
        Self::Fulfill(BrowserPausedNavigationFulfillDecision {
            response_code,
            response_headers,
            response_body,
        })
    }

    pub fn fail_auth() -> Self {
        Self::Auth(BrowserPausedNavigationAuthDecision::Fail)
    }

    pub fn cancel_auth() -> Self {
        Self::Auth(BrowserPausedNavigationAuthDecision::Cancel)
    }

    pub fn continue_auth(credentials: crate::page::SubresourceAuthCredentials) -> Self {
        Self::Auth(BrowserPausedNavigationAuthDecision::Continue(credentials))
    }
}

/// Browser-level response overrides supplied while releasing a paused
/// top-level response transfer.
///
/// Fetch request ids, protocol response phrases and frontend routing remain
/// outside Core. An absent status and empty header list preserve the received
/// response head.
#[derive(Debug)]
pub struct BrowserPausedNavigationResponseDecision {
    response_code: Option<u16>,
    response_headers: Vec<(String, String)>,
}

impl BrowserPausedNavigationResponseDecision {
    pub fn into_parts(self) -> (Option<u16>, Vec<(String, String)>) {
        (self.response_code, self.response_headers)
    }
}

/// Browser-level synthetic response supplied while resolving a paused
/// top-level navigation.
///
/// Fetch request ids, response phrases and frontend routing remain outside
/// Core. The optional body preserves the protocol distinction between an
/// omitted body and explicitly supplied bytes until the physical executor
/// starts the synthetic Document build.
#[derive(Debug)]
pub struct BrowserPausedNavigationFulfillDecision {
    response_code: u16,
    response_headers: Vec<(String, String)>,
    response_body: Option<Vec<u8>>,
}

impl BrowserPausedNavigationFulfillDecision {
    pub fn into_parts(self) -> (u16, Vec<(String, String)>, Option<Vec<u8>>) {
        (
            self.response_code,
            self.response_headers,
            self.response_body,
        )
    }
}

/// Browser-level resolution of an HTTP authentication challenge that paused
/// one top-level navigation.
///
/// `Fail` aborts the navigation, `Cancel` exposes the challenged response, and
/// `Continue` retries with already-resolved browser credentials. Protocol
/// request ids, session routing and wire action names stay outside Core.
#[derive(Debug)]
pub enum BrowserPausedNavigationAuthDecision {
    Fail,
    Cancel,
    Continue(crate::page::SubresourceAuthCredentials),
}

/// Browser-level request overrides supplied while resuming a paused
/// top-level navigation.
///
/// `Option` preserves the difference between "leave the original value" and
/// an explicit empty body/header set. URL syntax is validated by the frontend
/// before Browser Owner admission; the owner receives a parsed URL rather than
/// protocol text.
#[derive(Debug)]
pub struct BrowserPausedNavigationContinueDecision {
    url: Option<url::Url>,
    method: Option<String>,
    post_data: Option<String>,
    headers: Option<Vec<(String, String)>>,
    intercept_response: bool,
}

impl BrowserPausedNavigationContinueDecision {
    pub fn into_parts(
        self,
    ) -> (
        Option<url::Url>,
        Option<String>,
        Option<String>,
        Option<Vec<(String, String)>>,
        bool,
    ) {
        (
            self.url,
            self.method,
            self.post_data,
            self.headers,
            self.intercept_response,
        )
    }
}

/// Exact Page-scoped input for resolving a paused top-level navigation.
#[derive(Debug)]
pub struct BrowserPausedNavigationDecisionInput {
    command_id: BrowserCommandId,
    page_owner: PageResidenceIdentity,
    decision: BrowserPausedNavigationDecision,
}

impl BrowserPausedNavigationDecisionInput {
    fn new(
        command_id: BrowserCommandId,
        page_owner: PageResidenceIdentity,
        decision: BrowserPausedNavigationDecision,
    ) -> Self {
        Self {
            command_id,
            page_owner,
            decision,
        }
    }

    pub fn command_id(&self) -> BrowserCommandId {
        self.command_id
    }

    pub fn page_owner(&self) -> &PageResidenceIdentity {
        &self.page_owner
    }

    pub fn decision(&self) -> &BrowserPausedNavigationDecision {
        &self.decision
    }

    pub fn into_parts(
        self,
    ) -> (
        BrowserCommandId,
        PageResidenceIdentity,
        BrowserPausedNavigationDecision,
    ) {
        (self.command_id, self.page_owner, self.decision)
    }
}

/// Immutable browser action produced by one renderer turn.
#[derive(Debug)]
pub enum RendererBrowserIntent {
    TopLevelLocationNavigation(RendererTopLevelLocationNavigationInput),
    TopLevelHistoryTraversal(RendererTopLevelHistoryTraversalInput),
    AuxiliaryNavigation(BrowserAuxiliaryNavigationInput),
}

/// Exact Page-scoped top-level location navigation submitted to Browser Owner.
///
/// `source_document` remains part of `navigation` as causal metadata. Browser
/// execution authorization uses the exact Page residence, so a same-Page
/// `document.open()` does not discard an already-published action while a Page
/// replacement makes it stale.
#[derive(Debug)]
pub struct RendererTopLevelLocationNavigationInput {
    page_owner: PageResidenceIdentity,
    navigation: RendererDocumentSourcedTopLevelLocationNavigation,
    trace: Option<BrowserNavigationTraceContext>,
}

impl RendererTopLevelLocationNavigationInput {
    fn new(
        page_owner: PageResidenceIdentity,
        navigation: RendererDocumentSourcedTopLevelLocationNavigation,
        trace: Option<BrowserNavigationTraceContext>,
    ) -> Self {
        Self {
            page_owner,
            navigation,
            trace,
        }
    }

    pub fn page_owner(&self) -> &PageResidenceIdentity {
        &self.page_owner
    }

    pub fn navigation(&self) -> &RendererDocumentSourcedTopLevelLocationNavigation {
        &self.navigation
    }

    pub fn into_parts(
        self,
    ) -> (
        PageResidenceIdentity,
        RendererDocumentSourcedTopLevelLocationNavigation,
        Option<BrowserNavigationTraceContext>,
    ) {
        (self.page_owner, self.navigation, self.trace)
    }
}

/// Exact Page-scoped joint session-history traversal produced by a renderer
/// turn.
///
/// A Page replacement makes the intent stale. The target URL and history
/// entry are deliberately absent: Browser Owner resolves them from its
/// current cursor only after selecting this input.
#[derive(Debug)]
pub struct RendererTopLevelHistoryTraversalInput {
    page_owner: PageResidenceIdentity,
    delta: i64,
}

impl RendererTopLevelHistoryTraversalInput {
    fn new(page_owner: PageResidenceIdentity, delta: i64) -> Self {
        Self { page_owner, delta }
    }

    pub fn page_owner(&self) -> &PageResidenceIdentity {
        &self.page_owner
    }

    pub fn delta(&self) -> i64 {
        self.delta
    }

    pub fn into_parts(self) -> (PageResidenceIdentity, i64) {
        (self.page_owner, self.delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_reload_keeps_exact_page_and_neutral_options() {
        let command_id = BrowserCommandId::new(NonZeroU64::new(7).expect("non-zero command id"));
        let input = BrowserOwnerInput::frontend_reload(
            command_id,
            PageResidenceIdentity::new(
                "context-reload".to_owned(),
                Some("target-reload".to_owned()),
                11,
            ),
            true,
            Some("globalThis.reloaded = true".to_owned()),
        );

        assert_eq!(input.kind(), BrowserOwnerInputKind::FrontendReload);
        let BrowserOwnerInput::FrontendCommand(BrowserFrontendCommand::Reload(input)) = input
        else {
            panic!("expected frontend reload input");
        };
        assert_eq!(input.command_id(), command_id);
        assert_eq!(input.page_owner().target_id(), Some("target-reload"));
        assert_eq!(input.page_owner().loaded_page_generation(), 11);
        assert!(input.ignore_cache());
        assert_eq!(
            input.script_to_evaluate_on_load(),
            Some("globalThis.reloaded = true")
        );
    }

    #[test]
    fn frontend_history_traversal_keeps_exact_page_and_browser_destination() {
        let command_id = BrowserCommandId::new(NonZeroU64::new(8).expect("non-zero command id"));
        let input = BrowserOwnerInput::frontend_history_traversal_to_entry(
            command_id,
            PageResidenceIdentity::new(
                "context-history".to_owned(),
                Some("target-history".to_owned()),
                12,
            ),
            37,
        );

        assert_eq!(
            input.kind(),
            BrowserOwnerInputKind::FrontendHistoryTraversal
        );
        let BrowserOwnerInput::FrontendCommand(BrowserFrontendCommand::TraverseHistory(input)) =
            input
        else {
            panic!("expected frontend history traversal input");
        };
        assert_eq!(input.command_id(), command_id);
        assert_eq!(input.page_owner().target_id(), Some("target-history"));
        assert_eq!(input.page_owner().loaded_page_generation(), 12);
        assert_eq!(
            input.destination(),
            BrowserHistoryTraversalDestination::Entry(37)
        );

        let delta_input = BrowserOwnerInput::frontend_history_traversal_by_delta(
            command_id,
            PageResidenceIdentity::new(
                "context-history".to_owned(),
                Some("target-history".to_owned()),
                12,
            ),
            -2,
        );
        let BrowserOwnerInput::FrontendCommand(BrowserFrontendCommand::TraverseHistory(input)) =
            delta_input
        else {
            panic!("expected delta history traversal input");
        };
        assert_eq!(
            input.destination(),
            BrowserHistoryTraversalDestination::Delta(-2)
        );
    }

    #[test]
    fn frontend_stop_loading_keeps_slot_authority_without_frontend_identity() {
        let command_id = BrowserCommandId::new(NonZeroU64::new(9).expect("non-zero command id"));
        let input = BrowserOwnerInput::frontend_stop_loading(
            command_id,
            PageResidenceIdentity::new(
                "context-stop".to_owned(),
                Some("target-stop".to_owned()),
                13,
            ),
        );

        assert_eq!(input.kind(), BrowserOwnerInputKind::FrontendStopLoading);
        let BrowserOwnerInput::FrontendCommand(BrowserFrontendCommand::StopLoading(input)) = input
        else {
            panic!("expected frontend stop-loading input");
        };
        assert_eq!(input.command_id(), command_id);
        assert_eq!(input.page_owner().browser_context_id(), "context-stop");
        assert_eq!(input.page_owner().target_id(), Some("target-stop"));
        assert_eq!(input.page_owner().loaded_page_generation(), 13);
    }

    #[test]
    fn frontend_context_disposal_keeps_exact_context_without_frontend_identity() {
        let command_id = BrowserCommandId::new(NonZeroU64::new(15).expect("non-zero command id"));
        let handle = BrowserContextHandle::staged("context-dispose");
        let input = BrowserOwnerInput::frontend_dispose_browser_context(command_id, handle.clone());

        assert_eq!(
            input.kind(),
            BrowserOwnerInputKind::FrontendDisposeBrowserContext
        );
        let BrowserOwnerInput::FrontendCommand(BrowserFrontendCommand::DisposeBrowserContext(
            input,
        )) = input
        else {
            panic!("expected frontend BrowserContext disposal input");
        };
        assert_eq!(input.command_id(), command_id);
        assert_eq!(input.browser_context_handle(), &handle);
        let (_, actual_handle) = input.into_parts();
        assert_eq!(actual_handle, handle);
    }

    #[test]
    fn paused_navigation_decision_keeps_exact_page_and_neutral_failure() {
        let command_id = BrowserCommandId::new(NonZeroU64::new(10).expect("non-zero command id"));
        let input = BrowserOwnerInput::frontend_paused_navigation_decision(
            command_id,
            PageResidenceIdentity::new(
                "context-fetch".to_owned(),
                Some("target-fetch".to_owned()),
                14,
            ),
            BrowserPausedNavigationDecision::fail("net::ERR_ABORTED".to_owned()),
        );

        assert_eq!(
            input.kind(),
            BrowserOwnerInputKind::FrontendPausedNavigationDecision
        );
        let BrowserOwnerInput::FrontendCommand(BrowserFrontendCommand::ResolvePausedNavigation(
            input,
        )) = input
        else {
            panic!("expected paused-navigation decision input");
        };
        assert_eq!(input.command_id(), command_id);
        assert_eq!(input.page_owner().browser_context_id(), "context-fetch");
        assert_eq!(input.page_owner().target_id(), Some("target-fetch"));
        assert_eq!(input.page_owner().loaded_page_generation(), 14);
        assert!(matches!(
            input.decision(),
            BrowserPausedNavigationDecision::Fail { error_text }
                if error_text == "net::ERR_ABORTED"
        ));
    }

    #[test]
    fn paused_navigation_continue_keeps_browser_request_overrides() {
        let decision = BrowserPausedNavigationDecision::continue_request(
            Some(url::Url::parse("https://example.test/continued").expect("continued URL")),
            Some("POST".to_owned()),
            Some("payload".to_owned()),
            Some(vec![("X-Test".to_owned(), "owner".to_owned())]),
            true,
        );
        let BrowserPausedNavigationDecision::Continue(decision) = decision else {
            panic!("expected continue decision");
        };
        let (url, method, post_data, headers, intercept_response) = decision.into_parts();
        assert_eq!(
            url.as_ref().map(url::Url::as_str),
            Some("https://example.test/continued")
        );
        assert_eq!(method.as_deref(), Some("POST"));
        assert_eq!(post_data.as_deref(), Some("payload"));
        assert_eq!(
            headers,
            Some(vec![("X-Test".to_owned(), "owner".to_owned())])
        );
        assert!(intercept_response);
    }

    #[test]
    fn paused_navigation_fulfill_keeps_protocol_neutral_synthetic_response() {
        let decision = BrowserPausedNavigationDecision::fulfill(
            201,
            vec![("content-type".to_owned(), "text/plain".to_owned())],
            Some(vec![0, 0xff, b'a']),
        );
        let BrowserPausedNavigationDecision::Fulfill(decision) = decision else {
            panic!("expected fulfill decision");
        };
        let (status, headers, body) = decision.into_parts();
        assert_eq!(status, 201);
        assert_eq!(
            headers,
            vec![("content-type".to_owned(), "text/plain".to_owned())]
        );
        assert_eq!(body, Some(vec![0, 0xff, b'a']));
    }

    #[test]
    fn paused_navigation_auth_decision_keeps_browser_credentials() {
        let credentials = crate::page::SubresourceAuthCredentials {
            target: crate::page::SubresourceAuthTarget::Server,
            scheme: crate::page::SubresourceAuthScheme::Basic,
            username: "owner".to_owned(),
            password: "secret".to_owned(),
        };
        let decision = BrowserPausedNavigationDecision::continue_auth(credentials.clone());
        let BrowserPausedNavigationDecision::Auth(BrowserPausedNavigationAuthDecision::Continue(
            actual,
        )) = decision
        else {
            panic!("expected auth continue decision");
        };
        assert_eq!(actual, credentials);
    }

    #[test]
    fn auxiliary_navigation_keeps_exact_page_and_browser_kind() {
        let input = BrowserOwnerInput::renderer_auxiliary_navigation(
            PageResidenceIdentity::new(
                "context-popup".to_owned(),
                Some("target-popup".to_owned()),
                13,
            ),
            "https://example.test/popup".to_owned(),
            BrowserAuxiliaryNavigationKind::InitialDocument,
        );

        assert_eq!(
            input.kind(),
            BrowserOwnerInputKind::RendererAuxiliaryNavigation
        );
        let BrowserOwnerInput::RendererIntent(RendererBrowserIntent::AuxiliaryNavigation(input)) =
            input
        else {
            panic!("expected auxiliary navigation input");
        };
        assert_eq!(input.page_owner().target_id(), Some("target-popup"));
        assert_eq!(input.page_owner().loaded_page_generation(), 13);
        assert_eq!(input.url(), "https://example.test/popup");
        assert_eq!(
            input.kind(),
            BrowserAuxiliaryNavigationKind::InitialDocument
        );
    }

    #[test]
    fn initial_target_navigation_contains_no_frontend_identity() {
        let input = BrowserOwnerInput::initial_target_navigation(
            PageResidenceIdentity::new(
                "context-created".to_owned(),
                Some("target-created".to_owned()),
                7,
            ),
            "https://example.test/created".to_owned(),
        );

        assert_eq!(input.kind(), BrowserOwnerInputKind::InitialTargetNavigation);
        let BrowserOwnerInput::InitialTargetNavigation(input) = input else {
            panic!("expected initial Target navigation input");
        };
        assert_eq!(input.page_owner().target_id(), Some("target-created"));
        assert_eq!(input.page_owner().loaded_page_generation(), 7);
        assert_eq!(input.url(), "https://example.test/created");
    }

    #[test]
    fn frontend_initial_target_prerequisite_keeps_only_opaque_correlation_and_exact_page() {
        let command_id = BrowserCommandId::new(NonZeroU64::new(19).expect("non-zero command id"));
        let input = BrowserOwnerInput::frontend_ensure_initial_target_navigation(
            command_id,
            PageResidenceIdentity::new(
                "context-created".to_owned(),
                Some("target-created".to_owned()),
                9,
            ),
            "https://example.test/created".to_owned(),
        );

        assert_eq!(
            input.kind(),
            BrowserOwnerInputKind::FrontendEnsureInitialTargetNavigation
        );
        let BrowserOwnerInput::FrontendCommand(
            BrowserFrontendCommand::EnsureInitialTargetNavigation(input),
        ) = input
        else {
            panic!("expected frontend initial Target prerequisite");
        };
        assert_eq!(input.command_id(), command_id);
        assert_eq!(input.page_owner().target_id(), Some("target-created"));
        assert_eq!(input.page_owner().loaded_page_generation(), 9);
        assert_eq!(input.url(), "https://example.test/created");
    }

    #[test]
    fn renderer_history_traversal_contains_only_exact_page_and_delta() {
        let input = BrowserOwnerInput::renderer_top_level_history_traversal(
            PageResidenceIdentity::new(
                "context-history".to_owned(),
                Some("target-history".to_owned()),
                13,
            ),
            -2,
        );

        assert_eq!(
            input.kind(),
            BrowserOwnerInputKind::RendererTopLevelHistoryTraversal
        );
        let BrowserOwnerInput::RendererIntent(RendererBrowserIntent::TopLevelHistoryTraversal(
            input,
        )) = input
        else {
            panic!("expected renderer history traversal input");
        };
        assert_eq!(input.page_owner().target_id(), Some("target-history"));
        assert_eq!(input.page_owner().loaded_page_generation(), 13);
        assert_eq!(input.delta(), -2);
    }
}
