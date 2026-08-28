use moli_core::{browser_host::BrowserTargetTermination, page::Page};

use crate::conn::{
    BackgroundProtocolEvent, BackgroundTarget, CdpConnection, state::TargetPageAbsenceReason,
};

use super::BrowserContext;
use super::target_session_owner::{
    ClosedPageTarget, TargetSessionOwnerMut, clear_top_level_target_loaded_document_session_state,
};

pub(crate) struct ProjectedClosedPageTarget {
    closed: ClosedPageTarget,
    retired_page: Option<Page>,
}

impl ProjectedClosedPageTarget {
    pub(crate) fn into_parts(self) -> (ClosedPageTarget, Option<Page>) {
        (self.closed, self.retired_page)
    }
}

pub(crate) struct ProjectedActiveTargetClose {
    closed: ClosedPageTarget,
    retired_page: Option<Page>,
    promoted_target_id: Option<String>,
}

impl ProjectedActiveTargetClose {
    pub(crate) fn into_parts(self) -> (ClosedPageTarget, Option<Page>, Option<String>) {
        (self.closed, self.retired_page, self.promoted_target_id)
    }
}

impl BrowserContext {
    fn project_active_target_page_absence_after_browser_owner_commit(
        &mut self,
        reason: TargetPageAbsenceReason,
        termination: &BrowserTargetTermination,
    ) -> Option<Page> {
        let previous = self
            .active_target
            .runtime_slot
            .project_target_termination_after_browser_owner_commit(reason, termination);
        self.ingest_active_target_output_updates();
        self.active_target
            .owner_state
            .clear_loaded_document_context_state();
        self.clear_active_target_loaded_document_session_state();
        previous
    }

    pub(super) fn project_active_target_crash_after_browser_owner_commit(
        &mut self,
        termination: &BrowserTargetTermination,
    ) -> Option<Page> {
        self.active_target
            .owner_state
            .target_crash_state
            .mark_crashed();
        self.clear_renderer_document_protocol_state_for_active_target();
        let page = self.project_active_target_page_absence_after_browser_owner_commit(
            TargetPageAbsenceReason::TargetCrashed,
            termination,
        );
        self.clear_pending_fetch_state();
        self.clear_session_scoped_network_observation_artifacts();
        page
    }

    pub(super) fn project_active_target_after_page_close_browser_owner_commit(
        &mut self,
        termination: &BrowserTargetTermination,
    ) -> Option<Page> {
        if let Some(target_id) = self.active_target_id_owned() {
            self.forget_target_opener_references_for_target(&target_id);
            self.forget_target_window_names_for_target(&target_id);
            self.forget_target_popup_id_for_target(&target_id);
        }
        self.clear_active_target_session_scoped_state_fields();
        self.active_target.owner_state.target_crash_state.clear();
        self.clear_active_target_id();
        self.clear_renderer_document_protocol_state_for_active_target();
        self.detach_active_session();
        let page = self.project_active_target_page_absence_after_browser_owner_commit(
            TargetPageAbsenceReason::TargetClosed,
            termination,
        );
        self.active_target.owner_state.clear_page_local_state();
        self.reset_target_identity_to_about_blank();
        self.reset_target_scoped_network_artifacts();
        self.active_target
            .owner_state
            .clear_observable_output_state();
        self.active_target
            .runtime_slot
            .request_id_allocator()
            .reset_subresource_fetch_request_counter();
        page
    }

    pub(super) fn project_active_target_slot_to_empty_after_browser_owner_commit(
        &mut self,
        termination: &BrowserTargetTermination,
    ) -> Option<Page> {
        self.clear_active_target_session_scoped_state_fields();
        self.active_target.owner_state.target_crash_state.clear();
        if let Some(target_id) = self.active_target_id_owned() {
            self.forget_target_opener_references_for_target(&target_id);
            self.forget_target_window_names_for_target(&target_id);
            self.forget_target_popup_id_for_target(&target_id);
        }
        self.detach_active_session();
        self.clear_active_target_id();
        self.clear_renderer_document_protocol_state_for_active_target();
        let page = self.project_active_target_page_absence_after_browser_owner_commit(
            TargetPageAbsenceReason::TargetClosed,
            termination,
        );
        self.clear_pending_fetch_state();
        self.active_target.owner_state.clear_page_local_state();
        self.restore_raw_cookie_manager_surface_without_loaded_page_sync(Default::default());
        self.reset_target_identity_to_new_tab();
        self.reset_target_scoped_network_artifacts();
        self.active_target
            .owner_state
            .clear_observable_output_state();
        self.active_target
            .runtime_slot
            .request_id_allocator()
            .reset_subresource_fetch_request_counter();
        page
    }
}

impl BackgroundTarget {
    fn project_target_termination_after_browser_owner_commit(
        &mut self,
        reason: TargetPageAbsenceReason,
        termination: &BrowserTargetTermination,
    ) -> Option<Page> {
        self.runtime_slot
            .project_target_termination_after_browser_owner_commit(reason, termination)
    }
}

impl TargetSessionOwnerMut<'_> {
    pub(super) fn project_target_crash_after_browser_owner_commit(
        &mut self,
        termination: &BrowserTargetTermination,
    ) -> Option<Option<Page>> {
        match self {
            Self::ActiveTarget {
                browser_context, ..
            } => Some(
                browser_context.project_active_target_crash_after_browser_owner_commit(termination),
            ),
            Self::BackgroundTarget {
                browser_context,
                target_id,
                ..
            } => {
                browser_context.background_target(target_id)?;
                browser_context.mutate_parked_target_owner_state(target_id, |owner_state| {
                    owner_state.target_crash_state.mark_crashed();
                    owner_state.clear_loaded_document_context_state();
                });
                clear_top_level_target_loaded_document_session_state(browser_context, target_id);
                let previous = {
                    let target = browser_context.background_target_mut(target_id)?;
                    target.runtime_slot.clear_renderer_document_protocol_state();
                    let previous = target.project_target_termination_after_browser_owner_commit(
                        TargetPageAbsenceReason::TargetCrashed,
                        termination,
                    );
                    target.runtime_slot.reset_subresource_cursor();
                    target
                        .runtime_slot
                        .reset_all_target_scoped_network_artifacts();
                    previous
                };
                browser_context.replace_parked_fetch_state(target_id.clone(), Default::default());
                browser_context
                    .replace_parked_network_artifacts(target_id.clone(), Default::default());
                Some(previous)
            }
            Self::NoLoadedBrowserContext => None,
        }
    }

    pub(super) fn project_page_close_after_browser_owner_commit(
        &mut self,
        termination: &BrowserTargetTermination,
    ) -> Option<ProjectedClosedPageTarget> {
        match self {
            Self::ActiveTarget {
                browser_context, ..
            } => {
                let target_id = browser_context.active_target_id_owned()?;
                let primary_session_id = browser_context.active_session_id_owned();
                let auxiliary_session_ids =
                    browser_context.remove_auxiliary_sessions_for_target(&target_id);
                let retired_page = browser_context
                    .project_active_target_after_page_close_browser_owner_commit(termination);
                Some(ProjectedClosedPageTarget {
                    closed: ClosedPageTarget {
                        target_id,
                        primary_session_id,
                        auxiliary_session_ids,
                        promoted_target_id: None,
                    },
                    retired_page,
                })
            }
            Self::BackgroundTarget {
                browser_context,
                target_id,
                ..
            } => {
                let primary_session_id = browser_context
                    .primary_session_id_for_target(target_id)
                    .map(str::to_owned);
                let auxiliary_session_ids =
                    browser_context.remove_auxiliary_sessions_for_target(target_id);
                browser_context.take_top_level_target_attachment_for_target(target_id);
                let mut target = browser_context.remove_background_target(target_id)?;
                browser_context.forget_target_opener_references_for_target(target_id);
                browser_context.forget_target_window_names_for_target(target_id);
                browser_context.forget_target_popup_id_for_target(target_id);
                let _ = browser_context.take_parked_target_aux_state(target_id);
                let retired_page = target.project_target_termination_after_browser_owner_commit(
                    TargetPageAbsenceReason::TargetClosed,
                    termination,
                );
                Some(ProjectedClosedPageTarget {
                    closed: ClosedPageTarget {
                        target_id: target_id.clone(),
                        primary_session_id,
                        auxiliary_session_ids,
                        promoted_target_id: None,
                    },
                    retired_page,
                })
            }
            Self::NoLoadedBrowserContext => None,
        }
    }
}

impl CdpConnection {
    pub(crate) fn project_target_crash_for_none_session_owner_after_browser_owner_commit(
        &mut self,
        termination: &BrowserTargetTermination,
    ) -> Option<Option<Page>> {
        self.target_session_owner_mut(None)?
            .project_target_crash_after_browser_owner_commit(termination)
    }

    pub(crate) fn project_page_close_for_none_session_owner_after_browser_owner_commit(
        &mut self,
        termination: &BrowserTargetTermination,
    ) -> Option<ProjectedClosedPageTarget> {
        let target_id = termination.owner().target_id();
        let mut collected_network_data_artifacts = self
            .target_session_owner_ref(None)?
            .runtime_slot()?
            .collected_network_data_artifacts();
        if let Some(browser_context) =
            self.browser_context_by_id_mut(termination.owner().browser_context_id())
        {
            collected_network_data_artifacts.extend(
                browser_context
                    .take_parked_network_artifacts(target_id)
                    .collected_network_data_artifacts(),
            );
        }
        let projected = self
            .target_session_owner_mut(None)?
            .project_page_close_after_browser_owner_commit(termination)?;
        self.record_collected_network_data_artifacts(collected_network_data_artifacts);
        Some(projected)
    }

    pub(crate) fn project_background_target_close_after_browser_owner_commit(
        &mut self,
        termination: &BrowserTargetTermination,
        out: &mut Vec<BackgroundProtocolEvent>,
        reason: &'static str,
    ) -> Option<ProjectedClosedPageTarget> {
        let target_id = termination.owner().target_id();
        let (
            mut target,
            primary_session_id,
            auxiliary_session_ids,
            collected_network_data_artifacts,
        ) = {
            let browser_context =
                self.browser_context_by_id_mut(termination.owner().browser_context_id())?;
            let primary_session_id = browser_context
                .primary_session_id_for_target(target_id)
                .map(str::to_owned);
            let auxiliary_session_ids = browser_context.auxiliary_session_ids_for_target(target_id);
            let mut affected_sessions = primary_session_id
                .as_deref()
                .into_iter()
                .collect::<Vec<_>>();
            affected_sessions.extend(auxiliary_session_ids.iter().map(String::as_str));
            if let Some((primary, auxiliary)) =
                browser_context.devtools_session_states_for_target_mut(target_id)
            {
                CdpConnection::fail_pending_inspector_awaits_from_top_level_session_states_for_sessions_background_events_into(
                    out,
                    primary,
                    auxiliary,
                    primary_session_id.as_deref(),
                    &affected_sessions,
                    reason,
                );
            }
            browser_context.remove_auxiliary_sessions_for_target(target_id);
            browser_context.take_top_level_target_attachment_for_target(target_id);
            let target = browser_context.remove_background_target(target_id)?;
            let mut collected_network_data_artifacts =
                target.runtime_slot().collected_network_data_artifacts();
            collected_network_data_artifacts.extend(
                browser_context
                    .take_parked_network_artifacts(target_id)
                    .collected_network_data_artifacts(),
            );
            browser_context.forget_target_opener_references_for_target(target_id);
            browser_context.forget_target_window_names_for_target(target_id);
            browser_context.forget_target_popup_id_for_target(target_id);
            drop(browser_context.take_parked_target_aux_state(target_id));
            (
                target,
                primary_session_id,
                auxiliary_session_ids,
                collected_network_data_artifacts,
            )
        };
        let retired_page = target.project_target_termination_after_browser_owner_commit(
            TargetPageAbsenceReason::TargetClosed,
            termination,
        );
        self.record_collected_network_data_artifacts(collected_network_data_artifacts);

        Some(ProjectedClosedPageTarget {
            closed: ClosedPageTarget {
                target_id: target_id.to_owned(),
                primary_session_id,
                auxiliary_session_ids,
                promoted_target_id: None,
            },
            retired_page,
        })
    }

    pub(crate) fn project_active_target_close_after_browser_owner_commit(
        &mut self,
        termination: &BrowserTargetTermination,
    ) -> Option<ProjectedActiveTargetClose> {
        let (
            target_id,
            primary_session_id,
            auxiliary_session_ids,
            promoted_target_id,
            collected_network_data_artifacts,
            retired_page,
        ) = {
            let browser_context =
                self.browser_context_by_id_mut(termination.owner().browser_context_id())?;
            let target_id = browser_context.active_target_id_owned()?;
            let primary_session_id = browser_context.active_session_id_owned();
            let auxiliary_session_ids =
                browser_context.remove_auxiliary_sessions_for_target(&target_id);
            let promoted_target_id = browser_context.last_promotable_background_target_id();
            let collected_network_data_artifacts = browser_context
                .active_target
                .runtime_slot
                .collected_network_data_artifacts();
            let retired_page = browser_context
                .project_active_target_slot_to_empty_after_browser_owner_commit(termination);
            (
                target_id,
                primary_session_id,
                auxiliary_session_ids,
                promoted_target_id,
                collected_network_data_artifacts,
                retired_page,
            )
        };
        self.record_collected_network_data_artifacts(collected_network_data_artifacts);
        Some(ProjectedActiveTargetClose {
            closed: ClosedPageTarget {
                target_id,
                primary_session_id,
                auxiliary_session_ids,
                promoted_target_id: promoted_target_id.clone(),
            },
            retired_page,
            promoted_target_id,
        })
    }
}
