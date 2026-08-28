use moli_core::browser_host::{
    BrowserAuxiliaryNavigationKind, BrowserContextHandle, BrowserTargetHandle,
    PageResidenceIdentity,
};

use super::{CdpConnection, CdpSchedulerEvent};

/// Delayed foreground selection for one exact auxiliary Target instance.
///
/// Creation is projected before the opener command response. Activation can
/// replace the opener renderer, so the scheduler retains this capability
/// until the ordinary client-turn predecessor has completed. Stable Core
/// handles prevent a retired Context or Target id from being retargeted.
#[derive(Debug)]
pub(crate) struct PopupTargetActivationAction {
    browser_context: BrowserContextHandle,
    target: BrowserTargetHandle,
    navigation: Option<PopupTargetActivationNavigation>,
}

#[derive(Debug)]
struct PopupTargetActivationNavigation {
    page_owner: PageResidenceIdentity,
    url: String,
    kind: BrowserAuxiliaryNavigationKind,
}

impl PopupTargetActivationAction {
    pub(crate) fn capture(
        conn: &CdpConnection,
        browser_context_id: &str,
        target_id: &str,
    ) -> Option<Self> {
        let browser_context = conn.browser_context_by_id(browser_context_id)?;
        let browser_context_handle = browser_context.browser_context_handle().clone();
        let target_handle = browser_context.top_level_target_handle(target_id)?.clone();
        let browser_host_state = conn.browser_host_state();
        let owner = browser_host_state.navigation_owner();
        let context_selected = owner.selected_browser_context_id() == Some(browser_context_id);
        let context_current = owner.browser_context_handle_is_current(&browser_context_handle);
        let target_current = owner.target_handle_is_current(&target_handle);
        (context_selected && context_current && target_current).then_some(Self {
            browser_context: browser_context_handle,
            target: target_handle,
            navigation: None,
        })
    }

    pub(crate) fn capture_after_navigation(
        conn: &CdpConnection,
        browser_context_id: &str,
        target_id: &str,
        url: String,
        kind: BrowserAuxiliaryNavigationKind,
    ) -> Option<Self> {
        let mut action = Self::capture(conn, browser_context_id, target_id)?;
        let page_owner = conn
            .browser_host_state()
            .navigation_owner()
            .capture_page_residence(browser_context_id, target_id)?;
        action.navigation = Some(PopupTargetActivationNavigation {
            page_owner,
            url,
            kind,
        });
        Some(action)
    }

    pub(crate) fn browser_context_id(&self) -> &str {
        self.browser_context.browser_context_id()
    }

    pub(crate) fn target_id(&self) -> &str {
        self.target.target_id()
    }

    pub(crate) fn into_browser_owner_input(self) -> moli_core::browser_host::BrowserOwnerInput {
        match self.navigation {
            Some(navigation) => moli_core::browser_host::BrowserOwnerInput::renderer_auxiliary_navigation_and_target_activation(
                navigation.page_owner,
                navigation.url,
                navigation.kind,
                self.browser_context,
                self.target,
            ),
            None => moli_core::browser_host::BrowserOwnerInput::renderer_auxiliary_target_activation(
                self.browser_context,
                self.target,
            ),
        }
    }
}

impl CdpConnection {
    pub(crate) fn publish_popup_target_activation_action(
        &mut self,
        action: PopupTargetActivationAction,
    ) {
        let publish_sequence = self
            .scheduler_state
            .allocate_protocol_work_publish_sequence();
        let work = crate::domains::activity::ProtocolSchedulerWork::popup_target_activation_action(
            publish_sequence,
            action,
        );
        self.scheduler_state
            .push_scheduler_event(CdpSchedulerEvent::ProtocolWorkPublished { work });
    }
}
