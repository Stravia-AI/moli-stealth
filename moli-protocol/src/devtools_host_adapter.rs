use crate::{
    CdpConnection,
    conn::{BrowserHostTurnExecution, BrowserHostTurnExecutorOwner},
};

/// Application-owned DevTools adapter for one Browser Host.
///
/// This is deliberately distinct from a CDP socket frontend. The application
/// owner task keeps this adapter alive beside the Browser Host owner lane,
/// while any number of short-lived frontend endpoints may attach and detach.
/// Browser Core remains authoritative for browser identity, topology,
/// navigation, Page lifetime and browser-global policy; the wrapped
/// [`CdpConnection`] retains the migration-period renderer/DevTools projection
/// needed to translate commands and facts.
///
/// The wrapper is intentionally not `Clone`. There is one mutable adapter
/// residence per owner task, and a frontend endpoint never receives it.
pub struct DevToolsHostAdapter {
    connection: CdpConnection,
}

impl DevToolsHostAdapter {
    /// Transfers a protocol adapter into its application owner residence.
    pub fn for_application_owner(connection: CdpConnection) -> Self {
        Self { connection }
    }

    /// Binds the physical projection to one exact Browser Host execution
    /// authority. The wrapped connection is not otherwise exposed mutably.
    pub(crate) fn bind_browser_host_turn<'a>(
        &'a mut self,
        owner: &'a mut BrowserHostTurnExecutorOwner,
    ) -> BrowserHostTurnExecution<'a> {
        owner.bind_turn(&mut self.connection)
    }

    pub fn browser_host_state(&self) -> moli_core::browser_host::BrowserHostState {
        self.connection.browser_host_state()
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn browser_host_handle_for_test_support(
        &self,
    ) -> Option<moli_core::browser_host::BrowserHostHandle> {
        self.connection.browser_host_handle_for_test_support()
    }

    pub fn default_target_id(&self) -> &'static str {
        self.connection.default_target_id()
    }

    pub fn worker_target_id_for_session(&self, session_id: Option<&str>) -> Option<String> {
        self.connection.worker_target_id_for_session(session_id)
    }

    pub fn has_pending_javascript_dialog(&self) -> bool {
        self.connection.has_pending_javascript_dialog()
    }

    pub fn renderer_document_navigation_is_suspended_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> bool {
        self.connection
            .renderer_document_navigation_is_suspended_for_session_owner(session_id)
    }

    pub fn devtools_context_routes_to_top_level_target(
        &self,
        context: &crate::devtools_runtime::DevToolsCommandContext,
    ) -> bool {
        self.connection
            .devtools_context_routes_to_top_level_target(context)
    }

    pub fn renderer_output_cursor_is_projected(
        &self,
        cursor: moli_core::RendererOutputCursor,
    ) -> bool {
        self.connection.renderer_output_cursor_is_projected(cursor)
    }

    pub fn admit_runtime_command_output_barrier(
        &self,
        barriers: &mut crate::RuntimeCommandOutputBarriers,
        command_id: u64,
        session_id: Option<&str>,
    ) -> Option<crate::RuntimeCommandOutputBarrierPermit> {
        barriers.admit(&self.connection, command_id, session_id)
    }

    pub fn observes_main_document_load_for_devtools_context(
        &self,
        work: &crate::ProtocolSchedulerWork,
        context: &crate::devtools_runtime::DevToolsCommandContext,
    ) -> bool {
        work.observes_main_document_load_for_devtools_context(&self.connection, context)
    }

    pub fn background_event_route_is_current(
        &self,
        event: &crate::BackgroundProtocolEvent,
    ) -> bool {
        event.route_is_current(&self.connection)
    }

    pub fn set_target_host_lifecycle_observer(
        &mut self,
        observer: crate::CdpTargetHostLifecycleObserver,
    ) {
        self.connection.set_target_host_lifecycle_observer(observer);
    }

    pub fn set_automation_javascript_dialog_handler_enabled(&mut self, enabled: bool) -> bool {
        self.connection
            .set_automation_javascript_dialog_handler_enabled(enabled)
    }

    pub fn enable_webdriver_bidi_download_events(&mut self) -> bool {
        self.connection.enable_webdriver_bidi_download_events()
    }

    pub fn disable_webdriver_bidi_download_events(&mut self) -> bool {
        self.connection.disable_webdriver_bidi_download_events()
    }

    pub fn enable_webdriver_bidi_target_lifecycle_projection(&mut self) -> bool {
        self.connection
            .enable_webdriver_bidi_target_lifecycle_projection()
    }

    pub fn disable_webdriver_bidi_target_lifecycle_projection(&mut self) -> bool {
        self.connection
            .disable_webdriver_bidi_target_lifecycle_projection()
    }

    pub fn set_background_event_sender(&mut self, sender: crate::BackgroundEventSender) {
        self.connection.set_background_event_sender(sender);
    }

    pub fn set_runtime_inspector_response_ready_sender(
        &mut self,
        sender: crate::conn::RuntimeInspectorResponseReadySender,
    ) {
        self.connection
            .set_runtime_inspector_response_ready_sender(sender);
    }

    pub fn set_background_navigation_completion_sender(
        &mut self,
        sender: tokio::sync::mpsc::UnboundedSender<crate::BackgroundNavigationCompletion>,
    ) {
        self.connection
            .set_background_navigation_completion_sender(sender);
    }

    pub fn set_renderer_publication_sender(
        &mut self,
        sender: moli_core::RendererOutputTransportSender,
    ) {
        self.connection.set_renderer_publication_sender(sender);
    }

    pub fn install_default_browser_target(&mut self) {
        self.connection.install_default_browser_target();
    }

    pub fn enable_default_target_on_auto_attach(&mut self) {
        self.connection.enable_default_target_on_auto_attach();
    }

    pub fn replace_root_target_discovery_enabled(&mut self, enabled: bool) -> bool {
        self.connection
            .replace_root_target_discovery_enabled(enabled)
    }

    pub fn enable_network_listener_for_target(&mut self, target_id: &str) -> bool {
        self.connection
            .enable_network_listener_for_target(target_id)
    }

    pub fn disable_network_listener_for_target(&mut self, target_id: &str) -> bool {
        self.connection
            .disable_network_listener_for_target(target_id)
    }

    pub fn enable_file_dialog_opened_listener_for_target(&mut self, target_id: &str) -> bool {
        self.connection
            .enable_file_dialog_opened_listener_for_target(target_id)
    }

    pub fn disable_file_dialog_opened_listener_for_target(&mut self, target_id: &str) -> bool {
        self.connection
            .disable_file_dialog_opened_listener_for_target(target_id)
    }

    pub async fn enable_runtime_listener_for_target(
        &mut self,
        target_id: &str,
    ) -> Option<crate::CdpTurnOutcome> {
        self.connection
            .enable_runtime_listener_for_target(target_id)
            .await
    }

    pub async fn disable_runtime_listener_for_target(
        &mut self,
        target_id: &str,
    ) -> Option<crate::CdpTurnOutcome> {
        self.connection
            .disable_runtime_listener_for_target(target_id)
            .await
    }

    pub fn page_residence_identity_for_devtools_context(
        &mut self,
        context: &crate::devtools_runtime::DevToolsCommandContext,
    ) -> Option<crate::DevToolsPageResidenceIdentity> {
        self.connection
            .page_residence_identity_for_devtools_context(context)
    }

    pub fn devtools_context_document_navigation_state(
        &mut self,
        context: &crate::devtools_runtime::DevToolsCommandContext,
    ) -> crate::DevToolsDocumentNavigationState {
        self.connection
            .devtools_context_document_navigation_state(context)
    }

    pub fn capture_devtools_document_lifecycle_wait_key(
        &mut self,
        context: &crate::devtools_runtime::DevToolsCommandContext,
        expected_loader_id: &str,
        milestone: moli_core::page::RendererDocumentLifecycleMilestone,
    ) -> Option<crate::DevToolsDocumentLifecycleWaitKey> {
        self.connection
            .capture_devtools_document_lifecycle_wait_key(context, expected_loader_id, milestone)
    }

    pub fn capture_browser_fact_wake(
        &mut self,
        through: moli_core::browser_host::BrowserFactSequence,
    ) -> Result<(), crate::conn::BrowserFactProjectionError> {
        self.connection.capture_browser_fact_wake(through)
    }

    pub fn begin_command_response_flush_permit(
        &mut self,
    ) -> (
        crate::CommandResponseFlushPermit,
        crate::CommandResponseFlushContext,
    ) {
        self.connection.begin_command_response_flush_permit()
    }

    pub fn take_scheduler_events(&mut self) -> Vec<crate::CdpSchedulerEvent> {
        self.connection.take_scheduler_events()
    }

    pub fn page_screencast_subscription_status(
        &mut self,
        registration: &crate::PageScreencastRegistration,
    ) -> crate::PageScreencastSubscriptionStatus {
        self.connection
            .page_screencast_subscription_status(registration)
    }

    pub fn start_page_screencast_frame_capture(
        &mut self,
        registration: &crate::PageScreencastRegistration,
        known_visual_state: Option<moli_core::page::RendererVisualStateToken>,
    ) -> crate::PageScreencastCaptureStart {
        self.connection
            .start_page_screencast_frame_capture(registration, known_visual_state)
    }

    pub fn complete_page_screencast_frame_capture(
        &mut self,
        completed: crate::CompletedPageScreencastCapture,
    ) -> crate::PageScreencastCaptureCompletion {
        self.connection
            .complete_page_screencast_frame_capture(completed)
    }

    pub fn route_registered_runtime_inspector_response_into(
        &mut self,
        response: crate::conn::RuntimeInspectorResponseReady,
        response_events: &mut Vec<crate::BackgroundProtocolEvent>,
        background_events: &mut Vec<crate::BackgroundProtocolEvent>,
    ) {
        self.connection
            .route_registered_runtime_inspector_response_into(
                response,
                response_events,
                background_events,
            );
    }

    pub fn start_parsed_command_dispatch_with_context(
        &mut self,
        command: &crate::ParsedCdpCommand,
        command_context: &mut crate::CommandDispatchContext,
    ) -> crate::CdpCommandTaskStep {
        self.connection
            .start_parsed_command_dispatch_with_context(command, command_context)
    }

    pub async fn complete_pending_command_dispatch_with_context(
        &mut self,
        completed: crate::CompletedCdpCommandDispatch,
        command_context: &mut crate::CommandDispatchContext,
    ) -> crate::CdpCommandTaskStep {
        self.connection
            .complete_pending_command_dispatch_with_context(completed, command_context)
            .await
    }

    pub async fn process_message_with_turn_outcome_async(
        &mut self,
        raw: &str,
    ) -> crate::CdpTurnOutcome {
        self.connection
            .process_message_with_turn_outcome_async(raw)
            .await
    }

    pub async fn execute_devtools_command_with_protocol_events_with_background_command_id(
        &mut self,
        command: crate::devtools_runtime::DevToolsCommand,
        background_command_id: Option<u64>,
    ) -> crate::DevToolsCommandDispatchOutcome {
        self.connection
            .execute_devtools_command_with_protocol_events_with_background_command_id(
                command,
                background_command_id,
            )
            .await
    }

    pub async fn try_start_devtools_browser_owner_navigation_command(
        &mut self,
        command: crate::devtools_runtime::DevToolsCommand,
        background_command_id: Option<u64>,
    ) -> Result<
        crate::DevToolsBrowserOwnerNavigationCommandTaskStep,
        crate::devtools_runtime::DevToolsCommand,
    > {
        self.connection
            .try_start_devtools_browser_owner_navigation_command(command, background_command_id)
            .await
    }

    pub async fn complete_devtools_browser_owner_navigation_command(
        &mut self,
        completed: crate::CompletedDevToolsBrowserOwnerNavigationCommand,
    ) -> crate::DevToolsCommandDispatchOutcome {
        self.connection
            .complete_devtools_browser_owner_navigation_command(completed)
            .await
    }

    pub async fn try_start_devtools_browser_owner_context_disposal_command(
        &mut self,
        command: crate::devtools_runtime::DevToolsCommand,
    ) -> Result<
        crate::DevToolsBrowserOwnerContextDisposalCommandTaskStep,
        crate::devtools_runtime::DevToolsCommand,
    > {
        self.connection
            .try_start_devtools_browser_owner_context_disposal_command(command)
            .await
    }

    pub async fn complete_devtools_browser_owner_context_disposal_command(
        &mut self,
        completed: crate::CompletedDevToolsBrowserOwnerContextDisposalCommand,
    ) -> crate::DevToolsCommandDispatchOutcome {
        self.connection
            .complete_devtools_browser_owner_context_disposal_command(completed)
            .await
    }

    pub async fn try_start_devtools_fetch_command_task(
        &mut self,
        command: crate::devtools_runtime::DevToolsCommand,
    ) -> Result<crate::DevToolsFetchCommandTaskStep, crate::devtools_runtime::DevToolsCommand> {
        self.connection
            .try_start_devtools_fetch_command_task(command)
            .await
    }

    pub async fn complete_devtools_fetch_command_task(
        &mut self,
        completed: crate::CompletedDevToolsFetchCommand,
    ) -> crate::DevToolsCommandDispatchOutcome {
        self.connection
            .complete_devtools_fetch_command_task(completed)
            .await
    }

    pub async fn start_devtools_runtime_command_dispatch(
        &mut self,
        command: crate::devtools_runtime::DevToolsCommand,
    ) -> crate::DevToolsRuntimeCommandTaskStep {
        self.connection
            .start_devtools_runtime_command_dispatch(command)
            .await
    }

    pub async fn complete_devtools_runtime_command_dispatch(
        &mut self,
        completed: crate::CompletedDevToolsRuntimeCommandDispatch,
    ) -> crate::DevToolsRuntimeCommandTaskStep {
        self.connection
            .complete_devtools_runtime_command_dispatch(completed)
            .await
    }

    pub async fn complete_ready_protocol_scheduler_work_turn(
        &mut self,
        work: crate::ProtocolSchedulerWork,
    ) -> crate::CdpTurnOutcome {
        self.connection
            .complete_ready_protocol_scheduler_work_turn(work)
            .await
    }

    pub async fn ingest_renderer_output_turn_async(
        &mut self,
        publication: moli_core::RendererOutputTransportMessage,
        barriers: &mut crate::RuntimeCommandOutputBarriers,
    ) -> crate::CdpTurnOutcome {
        self.connection
            .ingest_renderer_output_turn_async(publication, barriers)
            .await
    }

    pub async fn project_protocol_local_command_outputs_turn_async(
        &mut self,
        session_id: Option<&str>,
    ) -> crate::CdpTurnOutcome {
        self.connection
            .project_protocol_local_command_outputs_turn_async(session_id)
            .await
    }

    pub async fn release_runtime_command_output_barrier_turn_async(
        &mut self,
        barriers: &mut crate::RuntimeCommandOutputBarriers,
        permit: crate::RuntimeCommandOutputBarrierPermit,
    ) -> crate::RuntimeCommandOutputBarrierCompletion {
        self.connection
            .release_runtime_command_output_barrier_turn_async(barriers, permit)
            .await
    }

    pub async fn drain_background_navigation_completion_turn_async(
        &mut self,
        completion: crate::BackgroundNavigationCompletion,
    ) -> (
        crate::CdpTurnOutcome,
        crate::BackgroundNavigationTurnDisposition,
    ) {
        self.connection
            .drain_background_navigation_completion_turn_async(completion)
            .await
    }

    pub async fn complete_deferred_main_document_load_completion_for_scheduler(
        &mut self,
        completion: crate::CompletedDeferredMainDocumentLoadCompletion,
    ) -> crate::CdpTurnOutcome {
        self.connection
            .complete_deferred_main_document_load_completion_for_scheduler(completion)
            .await
    }

    pub async fn route_runtime_deferred_inspector_response(
        &mut self,
        pending: &mut crate::PendingDevToolsRuntimeCommandDispatch,
        response: crate::conn::RuntimeInspectorResponseReady,
    ) -> bool {
        pending
            .route_scheduler_deferred_inspector_response(&mut self.connection, response)
            .await
    }

    pub fn complete_runtime_deferred_inspector_reply(
        &mut self,
        pending: crate::PendingDevToolsRuntimeCommandDispatch,
    ) -> crate::CompletedDevToolsRuntimeCommandDispatch {
        pending.complete_scheduler_deferred_inspector_reply(&mut self.connection)
    }

    pub fn forget_runtime_deferred_inspector_reply(
        &mut self,
        pending: crate::PendingDevToolsRuntimeCommandDispatch,
    ) {
        pending.forget_scheduler_deferred_inspector_reply(&mut self.connection);
    }

    pub async fn route_cdp_deferred_inspector_response(
        &mut self,
        pending: &mut crate::PendingCdpCommandDispatch,
        response: crate::conn::RuntimeInspectorResponseReady,
    ) -> bool {
        pending
            .route_scheduler_deferred_inspector_response(&mut self.connection, response)
            .await
    }

    pub fn complete_cdp_deferred_inspector_reply(
        &mut self,
        pending: crate::PendingCdpCommandDispatch,
    ) -> crate::CompletedCdpCommandDispatch {
        pending.complete_scheduler_deferred_inspector_reply(&mut self.connection)
    }

    pub fn forget_cdp_deferred_inspector_reply(
        &mut self,
        pending: crate::PendingCdpCommandDispatch,
    ) {
        pending.forget_scheduler_deferred_inspector_reply(&mut self.connection);
    }

    #[cfg(test)]
    pub(crate) fn into_connection_for_test(self) -> CdpConnection {
        self.connection
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_owner_adapter_preserves_the_exact_browser_host_residence() {
        let connection = CdpConnection::new();
        let browser_instance_id = connection
            .browser_host_state()
            .navigation_owner()
            .browser_instance_id();

        let adapter = DevToolsHostAdapter::for_application_owner(connection);

        assert_eq!(
            adapter
                .browser_host_state()
                .navigation_owner()
                .browser_instance_id(),
            browser_instance_id
        );
        assert_eq!(
            adapter
                .into_connection_for_test()
                .browser_host_state()
                .navigation_owner()
                .browser_instance_id(),
            browser_instance_id
        );
    }
}
