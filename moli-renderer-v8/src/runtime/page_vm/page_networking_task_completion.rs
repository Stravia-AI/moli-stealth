//! Completion boundary for migrated tasks in the shared Networking source.
//!
//! Text-track loads, stylesheet terminals, Worker host-bridge records and
//! resource terminals submit their typed completion here. A shared FIFO does
//! not imply a shared checkpoint policy: each family first produces its own
//! exact-owner action, then maps that post-execution fact to task completion.

use anyhow::Result;

use crate::page_task_queue::PageNetworkingTurnAction;

use super::{IntoPageTaskCompletion, PageVm};

impl PageVm {
    pub(super) async fn finish_selected_page_networking_task(
        &mut self,
        action: PageNetworkingTurnAction,
        loader: &crate::network::ResourceRequestClient,
    ) -> Result<()> {
        match action {
            PageNetworkingTurnAction::ResourceCompletion(action) => {
                self.finish_selected_page_resource_completion_task(action)?;
            }
            PageNetworkingTurnAction::StyleElementEvent(action) => {
                self.finish_selected_page_task_completion(
                    action.into_page_task_completion(),
                    loader,
                )
                .await?;
            }
            PageNetworkingTurnAction::TextTrackLoad(action) => {
                self.finish_selected_page_task_completion(
                    action.into_page_task_completion(),
                    loader,
                )
                .await?;
            }
            PageNetworkingTurnAction::StylesheetCompletion(action) => {
                self.finish_selected_page_task_completion(
                    action.into_page_task_completion(),
                    loader,
                )
                .await?;
                // A stylesheet terminal may release a parser created by
                // document.write()/Page.setDocumentContent after the initial
                // phase-one driver has retired. Resume that parser at the
                // resource-completion boundary, before the independently
                // queued link/style load event can be selected.
                self.run_ready_document_write_stylesheet_blocked_script()
                    .await?;
            }
            PageNetworkingTurnAction::WorkerHostBridge(action) => {
                self.finish_selected_page_task_completion(
                    action.into_page_task_completion(),
                    loader,
                )
                .await?;
            }
            PageNetworkingTurnAction::MainParserContinuation(_) => {}
        }
        Ok(())
    }
}
