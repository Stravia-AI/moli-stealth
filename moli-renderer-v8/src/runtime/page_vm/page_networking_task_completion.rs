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
                let connected_style_event_settled_current_owner = action.settled_current_owner();
                self.finish_selected_page_task_completion(
                    action.into_page_task_completion(),
                    loader,
                )
                .await?;
                if connected_style_event_settled_current_owner {
                    // A style-element event is the last observable action
                    // owned by the completed stylesheet. Resume a parser
                    // parked at that stylesheet boundary only after the event
                    // has dispatched and released its exact load-delay
                    // binding.
                    self.run_ready_document_write_stylesheet_blocked_script()
                        .await?;
                }
            }
            PageNetworkingTurnAction::TextTrackLoad(action) => {
                self.finish_selected_page_task_completion(
                    action.into_page_task_completion(),
                    loader,
                )
                .await?;
            }
            PageNetworkingTurnAction::StylesheetCompletion(action) => {
                let should_resume_parser_created_style = matches!(
                    action.target_effect,
                    crate::page_task_queue::PageStylesheetNetworkingTargetEffect::AppliedToCurrentOwner
                ) && self
                    .vm()
                    .document_runtime
                    .has_pending_document_write_parser_created_style_import_pause();
                self.finish_selected_page_task_completion(
                    action.into_page_task_completion(),
                    loader,
                )
                .await?;
                if should_resume_parser_created_style {
                    // Parser-created <style> owners are intentionally held
                    // out of the connected-owner initial scan while their
                    // parser boundary is active. Their @import completion
                    // therefore releases the parser directly after the
                    // stylesheet terminal's task-end checkpoint; the later
                    // connected-style scan owns any load/error event.
                    self.run_ready_document_write_stylesheet_blocked_script()
                        .await?;
                }
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
