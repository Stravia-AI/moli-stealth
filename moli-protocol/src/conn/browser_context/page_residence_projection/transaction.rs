use moli_core::{
    browser_host::{
        BrowserPageOwnerKey, BrowserPageResidenceTransition, BrowserPageResidenceTransitionKind,
        BrowserPageResidenceTransitionPermit,
    },
    page::RendererPageCreationArtifacts,
};
use url::Url;

use crate::conn::{BackgroundTarget, CdpConnection};

use super::super::{
    page_residence_projection_error::PageResidenceProjectionError,
    target_context_projection::StagedPhysicalBrowserContext,
    target_projection_error::TargetProjectionError,
    target_session_owner::clear_top_level_target_loaded_document_session_state,
};

enum StagedPhysicalPageTarget {
    Active,
    Background {
        index: usize,
        target: Box<BackgroundTarget>,
    },
}

/// Exact physical participant removed from Protocol storage before Browser
/// Core commits a Page-residence transition.
///
/// No callback or await can observe the vacant BrowserContext slot. A Core
/// rejection restores the exact context and background-target indices; a Core
/// success projects through the already-resolved participant without another
/// fallible id lookup.
pub(super) struct StagedPhysicalPageResidenceProjection {
    context: StagedPhysicalBrowserContext,
    target: StagedPhysicalPageTarget,
}

impl StagedPhysicalPageResidenceProjection {
    pub(super) fn restore(mut self, connection: &mut CdpConnection) {
        if let StagedPhysicalPageTarget::Background { index, target } = self.target {
            let insert_index = index.min(self.context.browser_context.background_targets.len());
            debug_assert_eq!(
                insert_index, index,
                "same-turn Page projection must preserve the background Target vector"
            );
            self.context
                .browser_context
                .background_targets
                .insert(insert_index, *target);
        }
        connection.restore_physical_browser_context_after_target_projection(self.context);
    }

    pub(super) fn project_initial_document_after_browser_owner_commit(
        mut self,
        connection: &mut CdpConnection,
        owner: &BrowserPageOwnerKey,
        page_creation_artifacts: RendererPageCreationArtifacts,
        loader_id: String,
        transition: &BrowserPageResidenceTransition,
    ) {
        debug_assert_eq!(transition.owner(), owner);
        debug_assert_eq!(
            transition.kind(),
            BrowserPageResidenceTransitionKind::InitialDocumentMaterialization
        );
        let lifecycle_ingress = match &mut self.target {
            StagedPhysicalPageTarget::Active => {
                self.context
                    .browser_context
                    .project_initial_document_page_after_browser_owner_commit(transition);
                self.context
                    .browser_context
                    .active_target
                    .runtime_slot
                    .page_slot_mut()
                    .bind_renderer_document_lifecycle_with_ingress(
                        page_creation_artifacts,
                        None,
                        owner.target_id().to_owned(),
                        loader_id,
                    )
            }
            StagedPhysicalPageTarget::Background { target, .. } => {
                self.context
                    .browser_context
                    .mutate_parked_target_owner_state(owner.target_id(), |owner_state| {
                        owner_state.clear_committed_document_navigation_state();
                    });
                clear_top_level_target_loaded_document_session_state(
                    &mut self.context.browser_context,
                    owner.target_id(),
                );
                target.project_initial_document_page_after_browser_owner_commit(transition);
                target.runtime_slot.reset_subresource_cursor();
                target.runtime_slot.clear_websocket_artifacts();
                target
                    .runtime_slot
                    .page_slot_mut()
                    .bind_renderer_document_lifecycle_with_ingress(
                        page_creation_artifacts,
                        None,
                        owner.target_id().to_owned(),
                        loader_id,
                    )
            }
        };
        self.restore(connection);
        connection.record_authoritative_renderer_document_lifecycle_facts(
            Some(transition.current_page()),
            None,
            lifecycle_ingress.authoritative(),
        );
    }

    pub(super) fn project_failed_navigation_after_browser_owner_commit(
        mut self,
        connection: &mut CdpConnection,
        owner: &BrowserPageOwnerKey,
        final_url: &Url,
        transition: &BrowserPageResidenceTransition,
    ) {
        debug_assert_eq!(transition.owner(), owner);
        debug_assert_eq!(
            transition.kind(),
            BrowserPageResidenceTransitionKind::FailedNavigationDiscard
        );
        let next_url = final_url.to_string();
        let security_origin = final_url.origin().ascii_serialization();
        match &mut self.target {
            StagedPhysicalPageTarget::Active => {
                self.context.browser_context.set_target_url(next_url);
                self.context
                    .browser_context
                    .set_target_security_origin(security_origin);
                self.context
                    .browser_context
                    .active_target
                    .runtime_slot
                    .clear_renderer_document_protocol_state();
                self.context
                    .browser_context
                    .clear_active_target_runtime_remote_object_tracking();
                self.context
                    .browser_context
                    .project_failed_navigation_page_absence_after_browser_owner_commit(transition);
            }
            StagedPhysicalPageTarget::Background { target, .. } => {
                self.context
                    .browser_context
                    .mutate_parked_target_owner_state(owner.target_id(), |owner_state| {
                        owner_state.clear_committed_document_navigation_state();
                    });
                clear_top_level_target_loaded_document_session_state(
                    &mut self.context.browser_context,
                    owner.target_id(),
                );
                target.set_target_url(next_url);
                target.set_target_security_origin(security_origin);
                target.runtime_slot.clear_renderer_document_protocol_state();
                target
                    .project_failed_navigation_page_absence_after_browser_owner_commit(transition);
                target.runtime_slot.reset_subresource_cursor();
                target.runtime_slot.clear_websocket_artifacts();
            }
        }
        self.restore(connection);
    }
}

impl CdpConnection {
    pub(super) fn stage_physical_page_residence_projection(
        &mut self,
        permit: &BrowserPageResidenceTransitionPermit,
        require_absent_page: bool,
    ) -> Result<StagedPhysicalPageResidenceProjection, PageResidenceProjectionError> {
        let owner = permit.owner();
        let mut context =
            self.take_physical_browser_context_for_target_projection(owner.browser_context_id())?;
        let background_index =
            if context.browser_context.active_target_id() == Some(owner.target_id()) {
                None
            } else {
                let Some(index) = context
                    .browser_context
                    .background_targets
                    .iter()
                    .position(|target| target.is_target(owner.target_id()))
                else {
                    self.restore_physical_browser_context_after_target_projection(context);
                    return Err(TargetProjectionError::PhysicalTargetMissing {
                        browser_context_id: owner.browser_context_id().to_owned(),
                        target_id: owner.target_id().to_owned(),
                    }
                    .into());
                };
                Some(index)
            };

        let (residence_matches, has_loaded_page) = match background_index {
            None => {
                let runtime_slot = &context.browser_context.active_target.runtime_slot;
                (
                    runtime_slot
                        .page_residence_handle()
                        .is_current(permit.previous_page()),
                    runtime_slot.has_loaded_page(),
                )
            }
            Some(index) => {
                let target = &context.browser_context.background_targets[index];
                (
                    target
                        .runtime_slot()
                        .page_residence_handle()
                        .is_current(permit.previous_page()),
                    target.has_loaded_page(),
                )
            }
        };
        if !residence_matches {
            self.restore_physical_browser_context_after_target_projection(context);
            return Err(PageResidenceProjectionError::PhysicalResidenceMismatch {
                browser_context_id: owner.browser_context_id().to_owned(),
                target_id: owner.target_id().to_owned(),
            });
        }
        if require_absent_page && has_loaded_page {
            self.restore_physical_browser_context_after_target_projection(context);
            return Err(
                PageResidenceProjectionError::InitialDocumentPageAlreadyPresent {
                    browser_context_id: owner.browser_context_id().to_owned(),
                    target_id: owner.target_id().to_owned(),
                },
            );
        }

        let target = match background_index {
            None => StagedPhysicalPageTarget::Active,
            Some(index) => StagedPhysicalPageTarget::Background {
                index,
                target: Box::new(context.browser_context.background_targets.remove(index)),
            },
        };
        Ok(StagedPhysicalPageResidenceProjection { context, target })
    }
}
