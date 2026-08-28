use moli_core::{
    browser_host::{
        BrowserNavigationHistoryPageSnapshot, BrowserPageReplacement, BrowserPageRuntimeOwner,
    },
    page::{Page, RendererMainDocumentCommit, RendererPageLifetimeOwner},
};
use url::Url;

use super::{CdpConnection, DocumentNavigationToken, LoadedNavigationRendererAttachmentCommit};

/// Synchronous Browser/physical replacement result before retired Page
/// disposal has necessarily completed.
pub(crate) enum LoadedPageReplacementStart {
    Pending(PendingLoadedPageReplacement),
    Ready(LoadedPageReplacementOutcome),
}

/// Move-owned wait for exactly one Page that no longer has physical
/// residence.
///
/// The Browser Core request/Page/history mutation and the protocol Page-slot
/// projection have already either committed or been rejected before this
/// value is created. Waiting therefore needs no `CdpConnection` access and
/// cannot change which Document is current.
pub(crate) struct PendingLoadedPageReplacement {
    page: Option<Page>,
    renderer_page_owner: Option<RendererPageLifetimeOwner>,
    outcome: LoadedPageReplacementOutcome,
}

pub(crate) enum LoadedPageReplacementOutcome {
    Committed(Box<BrowserPageReplacement>),
    Rejected,
    Failed(anyhow::Error),
}

impl PendingLoadedPageReplacement {
    fn new(page: Page, outcome: LoadedPageReplacementOutcome) -> Self {
        Self {
            page: Some(page),
            renderer_page_owner: None,
            outcome,
        }
    }

    fn with_retired_renderer_page(
        page: Option<Page>,
        renderer_page_owner: Option<RendererPageLifetimeOwner>,
        outcome: LoadedPageReplacementOutcome,
    ) -> Self {
        Self {
            page,
            renderer_page_owner,
            outcome,
        }
    }

    pub(crate) fn committed_owner(&self) -> Option<&moli_core::browser_host::BrowserPageOwnerKey> {
        self.outcome.committed_owner()
    }

    pub(crate) async fn wait(self) -> LoadedPageReplacementOutcome {
        if let Some(owner) = self.renderer_page_owner {
            let _ = owner.close_async().await;
        }
        if let Some(page) = self.page {
            let _ = page.close_async().await;
        }
        self.outcome
    }
}

impl LoadedPageReplacementOutcome {
    pub(crate) fn committed_owner(&self) -> Option<&moli_core::browser_host::BrowserPageOwnerKey> {
        match self {
            Self::Committed(replacement) => Some(replacement.owner()),
            Self::Rejected | Self::Failed(_) => None,
        }
    }

    #[cfg(test)]
    fn into_legacy_result(self) -> Option<anyhow::Result<BrowserPageReplacement>> {
        match self {
            Self::Committed(replacement) => Some(Ok(*replacement)),
            Self::Rejected => None,
            Self::Failed(error) => Some(Err(error)),
        }
    }
}

/// Migration adapter for an authoritative Browser Core Page replacement.
///
/// CDP/session routing selects the browser Target once. Browser Core then
/// authorizes and commits request identity, Page generation, and joint
/// history. Renderer attachment and physical `Page` storage remain protocol
/// participants during this migration, but they cannot decide whether the
/// navigation request is current.
impl CdpConnection {
    pub(crate) fn start_loaded_page_replacement_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        navigation: &DocumentNavigationToken,
        page: Page,
        target_url: &Url,
        main_document_commit: &RendererMainDocumentCommit,
        renderer_attachment_commit: LoadedNavigationRendererAttachmentCommit,
    ) -> LoadedPageReplacementStart {
        let Some(owner) = self.target_page_owner_key_for_session(session_id) else {
            return self.reject_loaded_page_replacement(
                session_id,
                navigation,
                page,
                renderer_attachment_commit,
                "Browser Core replacement owner is no longer routed",
            );
        };
        let history_page = BrowserNavigationHistoryPageSnapshot::new(
            target_url.to_string(),
            page.document_title(),
        );
        let Some(permit) = self
            .browser_host_state
            .navigation_owner()
            .prepare_loaded_page_replacement(&owner, navigation)
        else {
            return self.reject_loaded_page_replacement(
                session_id,
                navigation,
                page,
                renderer_attachment_commit,
                "Browser Core replacement permit is stale",
            );
        };

        let prepared = match self.prepare_loaded_navigation_page_for_session_owner(
            session_id,
            page,
            renderer_attachment_commit,
        ) {
            None => {
                return LoadedPageReplacementStart::Ready(LoadedPageReplacementOutcome::Rejected);
            }
            Some(Ok(prepared)) => prepared,
            Some(Err(error)) => {
                return LoadedPageReplacementStart::Ready(LoadedPageReplacementOutcome::Failed(
                    error,
                ));
            }
        };
        let retiring_renderer_page = prepared.retiring_renderer_page();
        let mut page_runtime_owner = Some(prepared.into_page_runtime_owner());
        let mut renderer_page_owner = page_runtime_owner
            .as_mut()
            .and_then(BrowserPageRuntimeOwner::take_renderer_lifetime_owner);
        if renderer_page_owner.is_none() {
            let page = page_runtime_owner
                .take()
                .and_then(BrowserPageRuntimeOwner::into_page);
            let outcome = LoadedPageReplacementOutcome::Failed(anyhow::anyhow!(
                "loaded Page candidate has no renderer lifetime owner"
            ));
            return match page {
                Some(page) => LoadedPageReplacementStart::Pending(
                    PendingLoadedPageReplacement::new(page, outcome),
                ),
                None => LoadedPageReplacementStart::Ready(outcome),
            };
        }

        // From here through the physical Page projection there is deliberately
        // no await or external callback. The connection actor therefore cannot
        // expose a core successor while protocol storage still names the old
        // Page.
        let mut replacement = match self.browser_host_state.commit_loaded_page_replacement(
            permit,
            history_page,
            &mut renderer_page_owner,
            &mut page_runtime_owner,
        ) {
            Ok(replacement) => replacement,
            Err(error) => {
                tracing::debug!(
                    %error,
                    session_id,
                    loader_id = navigation.loader_id(),
                    "prepared Browser Core Page replacement became stale before commit"
                );
                let detached_owner = match (renderer_page_owner.take(), page_runtime_owner.as_mut())
                {
                    (Some(owner), Some(runtime)) => {
                        runtime.try_restore_renderer_lifetime_owner(owner).err()
                    }
                    (owner, None) => owner,
                    (None, _) => None,
                };
                let page = page_runtime_owner
                    .take()
                    .and_then(BrowserPageRuntimeOwner::into_page);
                return LoadedPageReplacementStart::Pending(
                    PendingLoadedPageReplacement::with_retired_renderer_page(
                        page,
                        detached_owner,
                        LoadedPageReplacementOutcome::Rejected,
                    ),
                );
            }
        };
        if self
            .project_loaded_navigation_page_for_session_owner(
                session_id,
                target_url,
                main_document_commit,
                &replacement,
                retiring_renderer_page,
            )
            .is_none()
        {
            tracing::error!(
                session_id,
                loader_id = navigation.loader_id(),
                "committed Browser Page replacement lost its same-turn frontend projection route"
            );
        }

        let retired_renderer_page_owner = replacement.take_retired_renderer_page_owner();

        let outcome = LoadedPageReplacementOutcome::Committed(Box::new(replacement));
        match retired_renderer_page_owner {
            None => LoadedPageReplacementStart::Ready(outcome),
            renderer_page_owner => LoadedPageReplacementStart::Pending(
                PendingLoadedPageReplacement::with_retired_renderer_page(
                    None,
                    renderer_page_owner,
                    outcome,
                ),
            ),
        }
    }

    #[cfg(test)]
    pub(crate) async fn commit_loaded_page_replacement_for_session_owner_async(
        &mut self,
        session_id: Option<&str>,
        navigation: &DocumentNavigationToken,
        page: Page,
        target_url: &Url,
        main_document_commit: &RendererMainDocumentCommit,
        renderer_attachment_commit: LoadedNavigationRendererAttachmentCommit,
    ) -> Option<anyhow::Result<BrowserPageReplacement>> {
        let outcome = match self.start_loaded_page_replacement_for_session_owner(
            session_id,
            navigation,
            page,
            target_url,
            main_document_commit,
            renderer_attachment_commit,
        ) {
            LoadedPageReplacementStart::Pending(pending) => pending.wait().await,
            LoadedPageReplacementStart::Ready(outcome) => outcome,
        };
        outcome.into_legacy_result()
    }

    fn reject_loaded_page_replacement(
        &mut self,
        session_id: Option<&str>,
        navigation: &DocumentNavigationToken,
        page: Page,
        renderer_attachment_commit: LoadedNavigationRendererAttachmentCommit,
        reason: &'static str,
    ) -> LoadedPageReplacementStart {
        if let LoadedNavigationRendererAttachmentCommit::AlreadyCommitted(transaction) =
            renderer_attachment_commit
            && let Err(error) = self.rollback_committed_renderer_agent_candidate_for_session_owner(
                session_id,
                transaction,
            )
        {
            tracing::warn!(
                %error,
                session_id,
                loader_id = navigation.loader_id(),
                reason,
                "failed to roll back renderer attachment for stale Browser Core replacement"
            );
        }
        LoadedPageReplacementStart::Pending(PendingLoadedPageReplacement::new(
            page,
            LoadedPageReplacementOutcome::Rejected,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::state::RendererPageResidenceIdentity;
    use moli_core::browser_host::BrowserFact;

    fn main_document_commit(url: &Url, loader_id: &str) -> RendererMainDocumentCommit {
        RendererMainDocumentCommit {
            frame_id: "TID-1".to_owned(),
            loader_id: loader_id.to_owned(),
            url: url.to_string(),
            unreachable_url: None,
            security_origin: url.origin().ascii_serialization(),
            secure_context_type: "InsecureScheme".to_owned(),
            timestamp: 0.0,
        }
    }

    #[tokio::test]
    async fn adapter_commits_request_history_and_exactly_one_page_generation() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let residence = conn
            .target_page_residence_handle_for_session(None)
            .expect("default target Page residence");
        let previous_page = conn
            .load_page_via_runtime_async("data:text/html,<title>previous</title>")
            .await
            .expect("previous Page should load");
        conn.browser_context
            .as_mut()
            .expect("default BrowserContext")
            .set_loaded_page_async(previous_page)
            .await;
        let previous_generation = residence.generation();
        let navigation = conn
            .start_document_navigation_for_session_owner(None, "LOADER-replacement".to_owned())
            .expect("default target should start navigation");
        let page = conn
            .load_page_via_runtime_async("data:text/html,<!doctype html><title>replacement</title>")
            .await
            .expect("replacement Page should load");
        let replacement_renderer_page_id = page.page_id();
        let final_url = page.final_url().clone();
        let document_commit = main_document_commit(&final_url, navigation.loader_id());

        let replacement_start = conn.start_loaded_page_replacement_for_session_owner(
            None,
            &navigation,
            page,
            &final_url,
            &document_commit,
            LoadedNavigationRendererAttachmentCommit::Prepare(None),
        );

        assert_eq!(
            residence.generation(),
            previous_generation + 1,
            "Browser and physical replacement must commit before Page disposal waits"
        );
        assert!(conn.accepts_committed_document_navigation_for_session_owner(None, &navigation));
        let commit_facts = conn.browser_fact_snapshot_for_test();
        let commit_fact = commit_facts
            .iter()
            .find(|envelope| {
                matches!(
                    envelope.fact(),
                    BrowserFact::NavigationCommitted {
                        navigation: fact_navigation,
                        ..
                    } if fact_navigation == &navigation
                )
            })
            .expect(
                "Core commit must publish NavigationCommitted before predecessor disposal waits",
            );
        assert_eq!(
            commit_fact.page_residence().loaded_page_generation(),
            previous_generation + 1
        );

        let replacement_outcome = match replacement_start {
            LoadedPageReplacementStart::Pending(pending) => pending.wait().await,
            LoadedPageReplacementStart::Ready(outcome) => outcome,
        };
        let replacement = match replacement_outcome {
            LoadedPageReplacementOutcome::Committed(replacement) => replacement,
            LoadedPageReplacementOutcome::Rejected => panic!("exact replacement was rejected"),
            LoadedPageReplacementOutcome::Failed(error) => {
                panic!("replacement preparation failed: {error}")
            }
        };

        assert_eq!(
            replacement.current_page().loaded_page_generation(),
            previous_generation + 1
        );
        assert!(residence.is_current(replacement.current_page()));
        let owner = conn
            .target_page_owner_key_for_session(None)
            .expect("replacement Target owner");
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .renderer_page_id_for_owner(&owner),
            Some(replacement_renderer_page_id),
            "loaded replacement must transfer its renderer lifetime into Browser Core"
        );
        assert_eq!(commit_fact.page_residence(), replacement.current_page());
        assert!(matches!(
            commit_fact.fact(),
            BrowserFact::NavigationCommitted {
                previous_page,
                navigation: fact_navigation,
            } if previous_page == replacement.previous_page()
                && fact_navigation == replacement.navigation()
        ));
        assert!(!conn.has_pending_document_navigation_for_session_owner(None));
        assert_eq!(
            conn.runtime_session_owner_target_url(None).as_deref(),
            Some(final_url.as_str())
        );
        let (_, history) = conn
            .target_session_owner_navigation_history_snapshot(None)
            .expect("replacement should record joint history");
        assert_eq!(
            history.last().map(|entry| entry.url.as_str()),
            Some(final_url.as_str())
        );

        let predecessor_access = conn
            .browser_context
            .as_ref()
            .expect("default BrowserContext")
            .active_target
            .runtime_slot
            .loaded_page_runtime_access_for_test()
            .expect("committed replacement runtime access");
        let predecessor_lease = predecessor_access
            .checkout_page()
            .expect("committed replacement Page can be checked out");
        let predecessor_renderer_page =
            RendererPageResidenceIdentity::from_page(&predecessor_lease);
        assert_eq!(predecessor_lease.page_id(), replacement_renderer_page_id);
        assert!(
            predecessor_access.is_live(),
            "checking out the payload must not change Core Page residence"
        );
        assert!(
            predecessor_access.checkout_page().is_none(),
            "one mutable Page payload cannot have two concurrent leases"
        );
        assert_eq!(
            conn.browser_context.as_ref().and_then(|context| context
                .active_target
                .runtime_slot
                .loaded_renderer_page_residence()),
            Some(predecessor_renderer_page),
            "stable renderer identity must remain readable while the Page payload is checked out"
        );

        let next_navigation = conn
            .start_document_navigation_for_session_owner(None, "LOADER-next".to_owned())
            .expect("successor navigation should start");
        let next_page = conn
            .load_page_via_runtime_async("data:text/html,<title>next replacement</title>")
            .await
            .expect("next replacement Page should load");
        let next_page_id = next_page.page_id();
        let next_url = next_page.final_url().clone();
        let next_commit = main_document_commit(&next_url, next_navigation.loader_id());
        let next_pending = match conn.start_loaded_page_replacement_for_session_owner(
            None,
            &next_navigation,
            next_page,
            &next_url,
            &next_commit,
            LoadedNavigationRendererAttachmentCommit::Prepare(None),
        ) {
            LoadedPageReplacementStart::Pending(pending) => pending,
            LoadedPageReplacementStart::Ready(_) => {
                panic!("successor replacement must retire its renderer Page owner")
            }
        };

        assert!(
            !predecessor_access.is_live(),
            "Core replacement must invalidate stale frontend access in the commit turn"
        );
        assert!(
            predecessor_access.checkout_page().is_none(),
            "a stale access cannot concurrently reacquire its checked-out Page"
        );
        drop(predecessor_lease);
        assert!(
            predecessor_access.checkout_page().is_none(),
            "a late lease drop must not restore the predecessor Page after replacement"
        );

        assert!(matches!(
            next_pending.wait().await,
            LoadedPageReplacementOutcome::Committed(_)
        ));
        assert_eq!(
            conn.browser_context
                .as_ref()
                .and_then(|context| context.loaded_page())
                .map(|page| page.page_id()),
            Some(next_page_id),
            "the successor frontend projection must resolve the Core-owned Page payload"
        );
    }

    #[tokio::test]
    async fn stale_core_replacement_rolls_back_precommitted_renderer_attachment() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let navigation_a = conn
            .start_document_navigation_for_session_owner(None, "LOADER-A".to_owned())
            .expect("navigation A should start");
        let mut page = conn
            .load_page_via_runtime_async("data:text/html,<title>stale A</title>")
            .await
            .expect("candidate Page A should load");
        let final_url = page.final_url().clone();
        let main_document_commit = main_document_commit(&final_url, navigation_a.loader_id());
        let renderer_page = RendererPageResidenceIdentity::from_page(&page);
        let candidate = conn
            .prepare_renderer_agent_candidate_for_session_owner(None, &navigation_a, &mut page)
            .expect("renderer candidate A should prepare");
        let transaction = conn
            .commit_renderer_agent_candidate_for_session_owner(None, candidate, renderer_page)
            .expect("renderer candidate A should precommit");
        let previous_attachment_id = transaction.previous().map(|attachment| attachment.id());

        let navigation_b = conn
            .start_document_navigation_for_session_owner(None, "LOADER-B".to_owned())
            .expect("navigation B should supersede Browser Core request A");
        assert!(
            conn.commit_loaded_page_replacement_for_session_owner_async(
                None,
                &navigation_a,
                page,
                &final_url,
                &main_document_commit,
                LoadedNavigationRendererAttachmentCommit::AlreadyCommitted(transaction),
            )
            .await
            .is_none(),
            "stale request A must not replace the current Page"
        );

        assert_eq!(
            conn.current_renderer_agent_attachment_id_for_session_owner(None),
            previous_attachment_id,
            "stale precommitted attachment A must roll back to its exact predecessor"
        );
        assert!(conn.accepts_pending_document_navigation_for_session_owner(None, &navigation_b));
        assert!(
            conn.browser_fact_snapshot_for_test()
                .iter()
                .all(|envelope| !matches!(
                    envelope.fact(),
                    BrowserFact::NavigationCommitted {
                        navigation: fact_navigation,
                        ..
                    } if fact_navigation == &navigation_a
                )),
            "stale request A must not publish a navigation commit fact"
        );
    }
}
