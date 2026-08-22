use super::{BackgroundEventSender, RuntimeInspectorResponseReadySender};

#[derive(Default)]
pub(super) struct CdpSchedulerHooks {
    background_event_sender: Option<BackgroundEventSender>,
    background_navigation_completion_sender: Option<
        tokio::sync::mpsc::UnboundedSender<crate::domains::page::BackgroundNavigationCompletion>,
    >,
    runtime_inspector_response_ready_sender: Option<RuntimeInspectorResponseReadySender>,
}

impl CdpSchedulerHooks {
    pub(super) fn set_background_event_sender(&mut self, sender: BackgroundEventSender) {
        self.background_event_sender = Some(sender);
    }

    pub(super) fn background_event_sender(&self) -> Option<BackgroundEventSender> {
        self.background_event_sender.clone()
    }

    pub(super) fn set_runtime_inspector_response_ready_sender(
        &mut self,
        sender: RuntimeInspectorResponseReadySender,
    ) {
        self.runtime_inspector_response_ready_sender = Some(sender);
    }

    pub(super) fn runtime_inspector_response_ready_sender(
        &self,
    ) -> Option<RuntimeInspectorResponseReadySender> {
        self.runtime_inspector_response_ready_sender.clone()
    }

    pub(super) fn set_background_navigation_completion_sender(
        &mut self,
        sender: tokio::sync::mpsc::UnboundedSender<
            crate::domains::page::BackgroundNavigationCompletion,
        >,
    ) {
        self.background_navigation_completion_sender = Some(sender);
    }

    pub(super) fn background_navigation_completion_sender(
        &self,
    ) -> Option<
        tokio::sync::mpsc::UnboundedSender<crate::domains::page::BackgroundNavigationCompletion>,
    > {
        self.background_navigation_completion_sender.clone()
    }

    pub(super) fn has_background_navigation_completion_sender(&self) -> bool {
        self.background_navigation_completion_sender.is_some()
    }
}
