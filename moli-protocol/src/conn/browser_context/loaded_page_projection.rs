use moli_core::{
    browser_host::BrowserPageReplacement,
    page::{Page, RendererMainDocumentCommit},
};
use url::Url;

use crate::conn::{
    CdpConnection, LoadedNavigationRendererAttachmentCommit, PreparedLoadedNavigationPageCommit,
    RendererPageResidenceIdentity, state::prepare_renderer_call_replacements_for_devtools_sessions,
};

use super::target_session_owner::{
    TargetSessionOwnerMut, clear_top_level_target_loaded_document_session_state,
};

/// Active/background protocol participant for one loaded Page replacement.
///
/// Renderer attachment preparation happens before Browser Core mutates its
/// authoritative request/Page/history state. Projection happens immediately
/// after that owner commit and is synchronous; only disposal of the retired
/// physical Page may await.
impl TargetSessionOwnerMut<'_> {
    pub(super) fn prepare_loaded_navigation_page_commit(
        &mut self,
        mut page: Page,
        renderer_attachment_commit: LoadedNavigationRendererAttachmentCommit,
    ) -> Option<anyhow::Result<PreparedLoadedNavigationPageCommit>> {
        match self {
            Self::ActiveTarget {
                browser_context, ..
            } => Some(
                browser_context
                    .prepare_loaded_navigation_page_commit(page, renderer_attachment_commit),
            ),
            Self::BackgroundTarget {
                browser_context,
                target_id,
                ..
            } => {
                let retiring_renderer_page = browser_context
                    .background_target(target_id)?
                    .runtime_slot()
                    .loaded_renderer_page_residence();
                let primary_session_id = browser_context
                    .primary_session_id_for_target(target_id)
                    .map(str::to_owned);
                let (previous_attachment, new_attachment_id) = {
                    let target = browser_context.background_target_mut(target_id)?;
                    let previous_attachment = match renderer_attachment_commit {
                        LoadedNavigationRendererAttachmentCommit::Prepare(
                            renderer_agent_candidate,
                        ) => match target
                            .runtime_slot
                            .commit_loaded_navigation_renderer_attachment(
                                &mut page,
                                renderer_agent_candidate,
                            ) {
                            Ok(previous) => previous,
                            Err(error) => return Some(Err(error.into())),
                        },
                        LoadedNavigationRendererAttachmentCommit::AlreadyCommitted(transaction) => {
                            if let Err(error) = target
                                .runtime_slot
                                .bind_page_to_committed_renderer_agent_candidate(
                                    &mut page,
                                    &transaction,
                                )
                            {
                                return Some(Err(error.into()));
                            }
                            transaction.previous()
                        }
                    };
                    let new_attachment_id = page
                        .renderer_agent_attachment_id()
                        .expect("committed navigation Page must have a renderer attachment");
                    (previous_attachment, new_attachment_id)
                };
                if let Some(previous_attachment) = previous_attachment
                    && previous_attachment.id() != new_attachment_id
                {
                    let (primary, auxiliary) =
                        browser_context.devtools_session_states_for_target_mut(target_id)?;
                    let replacements =
                        match prepare_renderer_call_replacements_for_devtools_sessions(
                            primary_session_id.as_deref(),
                            primary,
                            auxiliary,
                            previous_attachment.id(),
                            new_attachment_id,
                        ) {
                            Ok(replacements) => replacements,
                            Err(error) => return Some(Err(error.into())),
                        };
                    browser_context
                        .background_target_mut(target_id)?
                        .runtime_slot
                        .install_pending_renderer_call_replacements(replacements);
                }
                Some(Ok(PreparedLoadedNavigationPageCommit::new(
                    page,
                    retiring_renderer_page,
                )))
            }
            Self::NoLoadedBrowserContext => None,
        }
    }

    /// Applies every protocol projection in the same actor turn and returns
    /// the retired Page for later asynchronous disposal.
    pub(super) fn project_loaded_navigation_page_after_browser_owner_commit(
        &mut self,
        target_url: &Url,
        main_document_commit: &RendererMainDocumentCommit,
        replacement: &BrowserPageReplacement,
        retiring_renderer_page: Option<RendererPageResidenceIdentity>,
    ) -> Option<()> {
        let next_url = target_url.to_string();
        let security_origin = main_document_commit.security_origin.clone();
        let secure_context_type = main_document_commit.secure_context_type.clone();
        match self {
            Self::ActiveTarget {
                browser_context, ..
            } => {
                browser_context.set_target_url(next_url);
                browser_context.set_target_security_origin(security_origin);
                browser_context.set_target_secure_context_type(secure_context_type);
                browser_context.project_loaded_navigation_page_after_browser_owner_commit(
                    replacement,
                    retiring_renderer_page,
                );
                Some(())
            }
            Self::BackgroundTarget {
                browser_context,
                target_id,
                ..
            } => {
                {
                    let target = browser_context.background_target_mut(target_id)?;
                    target.set_target_url(next_url);
                    target.set_target_security_origin(security_origin);
                    target.set_target_secure_context_type(secure_context_type);
                }
                browser_context.mutate_parked_target_owner_state(target_id, |owner_state| {
                    owner_state.clear_committed_document_navigation_state();
                });
                clear_top_level_target_loaded_document_session_state(browser_context, target_id);
                {
                    let target = browser_context.background_target_mut(target_id)?;
                    target.project_loaded_page_after_browser_owner_commit(
                        replacement,
                        retiring_renderer_page,
                    );
                    target.runtime_slot.reset_subresource_cursor();
                    target.runtime_slot.clear_websocket_artifacts();
                }
                Some(())
            }
            Self::NoLoadedBrowserContext => None,
        }
    }
}

impl CdpConnection {
    pub(crate) fn prepare_loaded_navigation_page_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        page: Page,
        renderer_attachment_commit: LoadedNavigationRendererAttachmentCommit,
    ) -> Option<anyhow::Result<PreparedLoadedNavigationPageCommit>> {
        self.target_session_owner_mut(session_id)?
            .prepare_loaded_navigation_page_commit(page, renderer_attachment_commit)
    }

    pub(crate) fn project_loaded_navigation_page_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        target_url: &Url,
        main_document_commit: &RendererMainDocumentCommit,
        replacement: &BrowserPageReplacement,
        retiring_renderer_page: Option<RendererPageResidenceIdentity>,
    ) -> Option<()> {
        self.target_session_owner_mut(session_id)?
            .project_loaded_navigation_page_after_browser_owner_commit(
                target_url,
                main_document_commit,
                replacement,
                retiring_renderer_page,
            )
    }
}
