//! Completion boundary for the shared DOM-manipulation source.
//!
//! Every current source variant has completed its P5 migration. Each domain
//! maps its typed, post-execution action to `PageTaskCompletion` in the module
//! that owns that action; this family coordinator only submits the resulting
//! boundary. Source membership therefore cannot become checkpoint policy.

use anyhow::Result;

use crate::page_task_queue::PageDomManipulationTurnAction;

use super::{IntoPageTaskCompletion, PageVm};

impl PageVm {
    pub(super) async fn finish_selected_page_dom_manipulation_task(
        &mut self,
        action: PageDomManipulationTurnAction,
        loader: &crate::network::ResourceRequestClient,
    ) -> Result<()> {
        let connected_style_event_settled_current_owner = matches!(
            action,
            PageDomManipulationTurnAction::ConnectedStyleEvent(action)
                if action.settled_current_owner()
        );
        let completion = match action {
            PageDomManipulationTurnAction::BroadcastChannel(action) => {
                action.into_page_task_completion()
            }
            PageDomManipulationTurnAction::StorageEvent(action) => {
                action.into_page_task_completion()
            }
            PageDomManipulationTurnAction::HashChange(action) => action.into_page_task_completion(),
            PageDomManipulationTurnAction::ElementToggle(action) => {
                action.into_page_task_completion()
            }
            PageDomManipulationTurnAction::FileEntryFileCallback(action) => {
                action.into_page_task_completion()
            }
            PageDomManipulationTurnAction::ImageLoadEvent(action) => {
                action.into_page_task_completion()
            }
            PageDomManipulationTurnAction::PopupLoadEvent(action) => {
                action.into_page_task_completion()
            }
            PageDomManipulationTurnAction::ConnectedStyleEvent(action) => {
                action.into_page_task_completion()
            }
            PageDomManipulationTurnAction::TextTrackDefaultMode(action) => {
                action.into_page_task_completion()
            }
            PageDomManipulationTurnAction::TextTrackLoad(action) => {
                action.into_page_task_completion()
            }
            PageDomManipulationTurnAction::ViewTransitionUpdate(action) => {
                action.into_page_task_completion()
            }
        };
        self.finish_selected_page_task_completion(completion, loader)
            .await?;
        if connected_style_event_settled_current_owner {
            // The connected-style event is the last observable action owned
            // by the completed stylesheet. Once it has dispatched and
            // released its exact load-delay binding, resume a parser parked at
            // that stylesheet boundary before lifecycle admission is
            // reconsidered for this owner turn.
            self.run_ready_document_write_stylesheet_blocked_script()
                .await?;
        }
        Ok(())
    }
}
