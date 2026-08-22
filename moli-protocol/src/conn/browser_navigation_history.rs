use moli_core::{
    browser_host::{
        BrowserExactHistoryTraversalResolutionError, BrowserHistoryTraversalDestination,
        BrowserHistoryTraversalResolution, BrowserHistoryTraversalResolutionError,
        BrowserNavigationHistoryPageSnapshot, BrowserNavigationHistorySeed, BrowserPageOwnerKey,
        BrowserSameDocumentNavigationCommitError,
    },
    page::SameDocumentHistoryUpdate,
};
use url::Url;

use super::{
    BackgroundTarget, BrowserContext, CdpConnection, PageNavigationHistoryEntry,
    TargetPageResidenceIdentity,
};

enum PhysicalSameDocumentTargetIdentityProjection<'a> {
    Active(&'a mut BrowserContext),
    Background(&'a mut BackgroundTarget),
}

impl PhysicalSameDocumentTargetIdentityProjection<'_> {
    fn commit(self, url: &Url) {
        let next_url = url.to_string();
        let security_origin = url.origin().ascii_serialization();
        match self {
            Self::Active(browser_context) => {
                browser_context.set_target_url(next_url);
                browser_context.set_target_security_origin(security_origin);
            }
            Self::Background(target) => {
                target.set_target_url(next_url);
                target.set_target_security_origin(security_origin);
            }
        }
    }
}

/// A same-Document renderer fact must commit in Browser Core before Protocol
/// projects physical Target metadata or frontend events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SameDocumentNavigationProjectionError {
    PageResidenceRouteMismatch {
        expected: Box<TargetPageResidenceIdentity>,
        current: Option<Box<TargetPageResidenceIdentity>>,
    },
    PhysicalBrowserContextUnavailable {
        browser_context_id: String,
    },
    PhysicalTargetUnavailable {
        browser_context_id: String,
        target_id: String,
    },
    BrowserHistory(BrowserSameDocumentNavigationCommitError),
}

impl std::fmt::Display for SameDocumentNavigationProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PageResidenceRouteMismatch { expected, current } => write!(
                formatter,
                "same-Document navigation route no longer addresses Page {expected:?}; current={current:?}"
            ),
            Self::PhysicalBrowserContextUnavailable { browser_context_id } => write!(
                formatter,
                "physical BrowserContext {browser_context_id:?} is unavailable for same-Document navigation"
            ),
            Self::PhysicalTargetUnavailable {
                browser_context_id,
                target_id,
            } => write!(
                formatter,
                "physical Target {target_id:?} in BrowserContext {browser_context_id:?} is unavailable for same-Document navigation"
            ),
            Self::BrowserHistory(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SameDocumentNavigationProjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BrowserHistory(error) => Some(error),
            _ => None,
        }
    }
}

impl From<BrowserSameDocumentNavigationCommitError> for SameDocumentNavigationProjectionError {
    fn from(error: BrowserSameDocumentNavigationCommitError) -> Self {
        Self::BrowserHistory(error)
    }
}

/// Migration adapter from frontend/session routing to Browser Core history.
///
/// Session ids are resolved once into a protocol-neutral target key. The core
/// registry remains the sole owner of entries, cursor, allocators, and pending
/// replace/traversal state.
impl CdpConnection {
    fn navigation_history_loaded_page_snapshot_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<BrowserNavigationHistoryPageSnapshot> {
        let page = self
            .runtime_session_owner_slot(session_id)
            .ok()?
            .loaded_page()?;
        Some(BrowserNavigationHistoryPageSnapshot::new(
            page.final_url().to_string(),
            page.document_title(),
        ))
    }

    fn navigation_history_same_document_page_snapshot_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<BrowserNavigationHistoryPageSnapshot> {
        let title = self
            .runtime_session_owner_slot(session_id)
            .ok()?
            .loaded_page()?
            .document_title();
        Some(BrowserNavigationHistoryPageSnapshot::new(
            self.runtime_session_owner_target_url(session_id)?,
            title,
        ))
    }

    fn navigation_history_query_seed_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<BrowserNavigationHistorySeed> {
        self.navigation_history_loaded_page_snapshot_for_session_owner(session_id)
            .map(BrowserNavigationHistorySeed::page_snapshot)
    }

    fn navigation_history_same_document_seed_for_session_owner(
        &self,
        session_id: Option<&str>,
    ) -> Option<BrowserNavigationHistorySeed> {
        self.navigation_history_same_document_page_snapshot_for_session_owner(session_id)
            .map(BrowserNavigationHistorySeed::page_snapshot)
    }

    fn navigation_history_owner_for_session(
        &self,
        session_id: Option<&str>,
    ) -> Option<BrowserPageOwnerKey> {
        self.target_page_owner_key_for_session(session_id)
    }

    pub(crate) fn target_session_owner_navigation_history_snapshot(
        &mut self,
        session_id: Option<&str>,
    ) -> Option<(usize, Vec<PageNavigationHistoryEntry>)> {
        let owner = self.navigation_history_owner_for_session(session_id)?;
        let seed = self.navigation_history_query_seed_for_session_owner(session_id);
        Some(
            self.browser_host_state
                .navigation_owner_mut()
                .navigation_history_snapshot(&owner, seed),
        )
    }

    pub(crate) fn resolve_navigation_history_traversal_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        destination: BrowserHistoryTraversalDestination,
    ) -> Option<Result<BrowserHistoryTraversalResolution, BrowserHistoryTraversalResolutionError>>
    {
        let owner = self.navigation_history_owner_for_session(session_id)?;
        let seed = self.navigation_history_query_seed_for_session_owner(session_id);
        Some(
            self.browser_host_state
                .navigation_owner_mut()
                .resolve_navigation_history_traversal(&owner, seed, destination),
        )
    }

    pub(crate) fn resolve_exact_navigation_history_traversal_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        expected_page: &TargetPageResidenceIdentity,
        destination: BrowserHistoryTraversalDestination,
    ) -> Result<BrowserHistoryTraversalResolution, BrowserExactHistoryTraversalResolutionError>
    {
        let seed = self.navigation_history_query_seed_for_session_owner(session_id);
        self.browser_host_state
            .navigation_owner_mut()
            .resolve_exact_navigation_history_traversal(expected_page, seed, destination)
    }

    pub(crate) fn reset_navigation_history_for_session_owner(
        &mut self,
        session_id: Option<&str>,
    ) -> Option<bool> {
        let owner = self.navigation_history_owner_for_session(session_id)?;
        let seed = self.navigation_history_query_seed_for_session_owner(session_id);
        Some(
            self.browser_host_state
                .navigation_owner_mut()
                .reset_navigation_history(&owner, seed),
        )
    }

    pub(crate) fn can_reset_navigation_history_for_session_owner(
        &mut self,
        session_id: Option<&str>,
    ) -> Option<bool> {
        let owner = self.navigation_history_owner_for_session(session_id)?;
        let seed = self.navigation_history_query_seed_for_session_owner(session_id);
        Some(
            self.browser_host_state
                .navigation_owner_mut()
                .can_reset_navigation_history(&owner, seed),
        )
    }

    pub(crate) fn mark_next_navigation_history_replace_current_for_session_owner(
        &mut self,
        session_id: Option<&str>,
    ) -> Option<()> {
        let owner = self.navigation_history_owner_for_session(session_id)?;
        self.browser_host_state
            .navigation_owner_mut()
            .mark_next_navigation_history_replace_current(&owner);
        Some(())
    }

    pub(crate) fn mark_next_navigation_history_replace_initial_empty_document_for_owner(
        &mut self,
        owner: &BrowserPageOwnerKey,
    ) {
        self.browser_host_state
            .navigation_owner_mut()
            .mark_next_navigation_history_replace_initial_empty_document(owner);
    }

    pub(crate) fn mark_next_navigation_history_traverse_to_entry_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        entry_id: i32,
    ) -> Option<()> {
        let owner = self.navigation_history_owner_for_session(session_id)?;
        self.browser_host_state
            .navigation_owner_mut()
            .mark_next_navigation_history_traverse_to_entry(&owner, entry_id);
        Some(())
    }

    pub(crate) fn clear_pending_navigation_history_update_for_session_owner(
        &mut self,
        session_id: Option<&str>,
    ) -> Option<()> {
        let owner = self.navigation_history_owner_for_session(session_id)?;
        self.browser_host_state
            .navigation_owner_mut()
            .clear_pending_navigation_history_update(&owner);
        Some(())
    }

    pub(crate) fn record_same_document_navigation_for_session_owner(
        &mut self,
        session_id: Option<&str>,
        expected_page: &TargetPageResidenceIdentity,
        url: &Url,
        history_update: SameDocumentHistoryUpdate,
    ) -> Result<String, SameDocumentNavigationProjectionError> {
        let current_page = self.target_page_residence_identity_for_session(session_id);
        if current_page.as_ref() != Some(expected_page) {
            return Err(
                SameDocumentNavigationProjectionError::PageResidenceRouteMismatch {
                    expected: Box::new(expected_page.clone()),
                    current: current_page.map(Box::new),
                },
            );
        }
        let browser_context_id = expected_page.browser_context_id().to_owned();
        let Some(target_id) = expected_page.target_id().map(str::to_owned) else {
            return Err(
                SameDocumentNavigationProjectionError::PhysicalTargetUnavailable {
                    browser_context_id,
                    target_id: "<none>".to_owned(),
                },
            );
        };
        let seed = self.navigation_history_same_document_seed_for_session_owner(session_id);
        let title = self
            .runtime_session_owner_slot(session_id)
            .ok()
            .and_then(|slot| slot.loaded_page())
            .map(|page| page.document_title())
            .unwrap_or_default();

        // Split the connection's Browser Owner and physical Target fields so
        // the exact physical participant remains mutably borrowed across the
        // Core commit. No lookup or fallible participant remains after Core
        // accepts the history update.
        let browser_host_state = self.browser_host_state.clone();
        let browser_context = if self
            .browser_context
            .as_ref()
            .is_some_and(|context| context.id == browser_context_id)
        {
            self.browser_context.as_mut()
        } else {
            self.inactive_browser_contexts
                .iter_mut()
                .find(|context| context.id == browser_context_id)
        }
        .ok_or_else(|| {
            SameDocumentNavigationProjectionError::PhysicalBrowserContextUnavailable {
                browser_context_id: browser_context_id.clone(),
            }
        })?;
        let physical_target = if browser_context.is_active_target(&target_id) {
            PhysicalSameDocumentTargetIdentityProjection::Active(browser_context)
        } else {
            let target = browser_context
                .background_target_mut(&target_id)
                .ok_or_else(|| {
                    SameDocumentNavigationProjectionError::PhysicalTargetUnavailable {
                        browser_context_id: browser_context_id.clone(),
                        target_id: target_id.clone(),
                    }
                })?;
            PhysicalSameDocumentTargetIdentityProjection::Background(target)
        };

        browser_host_state
            .navigation_owner_mut()
            .commit_same_document_navigation_history(
                expected_page,
                seed,
                url.to_string(),
                title,
                history_update,
            )?;
        physical_target.commit(url);
        Ok(target_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_document_history_is_seeded_by_core_through_target_adapter() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();

        let (current_index, entries) = conn
            .target_session_owner_navigation_history_snapshot(None)
            .expect("default target should expose navigation history");

        assert_eq!(current_index, 0);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 1);
        assert_eq!(entries[0].url, "about:blank");
        assert_eq!(entries[0].transition_type, "auto_toplevel");
    }

    #[test]
    fn rejected_same_document_history_does_not_project_physical_target_identity() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let expected_page = conn
            .target_page_residence_identity_for_session(None)
            .expect("default target should expose its exact Page residence");
        let before_history = conn
            .target_session_owner_navigation_history_snapshot(None)
            .expect("default target should expose navigation history");
        let before_url = conn
            .browser_context
            .as_ref()
            .expect("default BrowserContext")
            .target_url()
            .to_owned();
        let before_security_origin = conn
            .browser_context
            .as_ref()
            .expect("default BrowserContext")
            .target_security_origin()
            .to_owned();

        let error = conn
            .record_same_document_navigation_for_session_owner(
                None,
                &expected_page,
                &Url::parse("https://example.test/missing").expect("valid test URL"),
                SameDocumentHistoryUpdate::Traverse { delta: -1 },
            )
            .expect_err("out-of-range traversal must reject the joint projection");

        assert!(matches!(
            error,
            SameDocumentNavigationProjectionError::BrowserHistory(
                BrowserSameDocumentNavigationCommitError::History(
                    moli_core::browser_host::BrowserSameDocumentHistoryUpdateError::TraversalTargetOutsideHistory {
                        current_index: 0,
                        delta: -1,
                        entry_count: 1,
                    }
                )
            )
        ));
        let browser_context = conn
            .browser_context
            .as_ref()
            .expect("default BrowserContext");
        assert_eq!(browser_context.target_url(), before_url);
        assert_eq!(
            browser_context.target_security_origin(),
            before_security_origin
        );
        assert_eq!(
            conn.target_session_owner_navigation_history_snapshot(None),
            Some(before_history)
        );
    }

    #[test]
    fn protocol_page_slot_cleanup_does_not_own_or_clear_target_history() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let before = conn
            .target_session_owner_navigation_history_snapshot(None)
            .expect("default target should expose navigation history");

        conn.browser_context
            .as_mut()
            .expect("default browser context")
            .active_target
            .owner_state
            .clear_page_local_state();

        assert_eq!(
            conn.target_session_owner_navigation_history_snapshot(None),
            Some(before)
        );
    }

    #[test]
    fn protocol_adapter_clears_only_the_pending_history_update() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let key = conn
            .target_page_owner_key_for_session(None)
            .expect("default target owner");
        let _ = conn
            .target_session_owner_navigation_history_snapshot(None)
            .expect("seed initial history");
        conn.browser_host_state
            .navigation_owner_mut()
            .record_loaded_page_navigation_history(
                &key,
                BrowserNavigationHistoryPageSnapshot::new("https://old.example/", "old"),
            );
        conn.mark_next_navigation_history_replace_current_for_session_owner(None)
            .expect("mark pending replace");
        conn.clear_pending_navigation_history_update_for_session_owner(None)
            .expect("clear pending replace");
        conn.browser_host_state
            .navigation_owner_mut()
            .record_loaded_page_navigation_history(
                &key,
                BrowserNavigationHistoryPageSnapshot::new("https://new.example/", "new"),
            );

        let (_, entries) = conn
            .target_session_owner_navigation_history_snapshot(None)
            .expect("history snapshot");
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.url.as_str())
                .collect::<Vec<_>>(),
            vec![
                "about:blank",
                "https://old.example/",
                "https://new.example/"
            ]
        );
    }

    #[tokio::test]
    async fn crashed_target_history_does_not_reseed_from_protocol_metadata() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let _ = conn
            .target_session_owner_navigation_history_snapshot(None)
            .expect("seed initial history");

        let termination = conn
            .capture_browser_target_termination_for_session_owner(
                None,
                crate::conn::BrowserTargetTerminationProjectionKind::Crash,
            )
            .expect("default target should capture crash");
        let mut events = Vec::new();
        assert!(matches!(
            conn.commit_browser_target_termination_async(
                termination,
                crate::conn::BrowserTargetTerminationProjectionKind::Crash,
                &mut events,
                "Page crashed",
            )
            .await,
            Some(crate::conn::BrowserTargetTerminationProjection::Crashed { .. })
        ));

        let (_, entries) = conn
            .target_session_owner_navigation_history_snapshot(None)
            .expect("crashed target remains addressable");
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn failed_navigation_page_discard_preserves_target_history() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let before = conn
            .target_session_owner_navigation_history_snapshot(None)
            .expect("seed initial history");
        let previous_page = conn
            .target_page_residence_identity_for_session(None)
            .expect("default target should expose its loaded Page residence");
        let navigation = conn
            .start_document_navigation_for_session_owner(None, "LOADER-failed".to_owned())
            .expect("default Target should start failed navigation");

        conn.discard_loaded_page_after_failed_navigation_for_session_owner_async(
            None,
            &navigation,
            moli_core::browser_host::BrowserNavigationFailure::Network {
                error_text: "net::ERR_FAILED".to_owned(),
            },
            &Url::parse("https://failed.example/").expect("failure URL"),
        )
        .await
        .expect("default target should discard failed Page");

        assert_eq!(
            conn.target_session_owner_navigation_history_snapshot(None),
            Some(before)
        );
        let absent_page = conn
            .target_page_residence_identity_for_session(None)
            .expect("failed navigation keeps the Target Page slot registered");
        assert_eq!(
            absent_page.loaded_page_generation(),
            previous_page.loaded_page_generation() + 1,
            "Core must advance the failed-navigation Page generation exactly once"
        );
        assert!(
            conn.target_page_owner_route_if_current(&previous_page)
                .is_none(),
            "failed navigation must invalidate work captured from the discarded Page"
        );
        assert!(
            conn.runtime_session_owner_slot(None)
                .expect("default Target remains routable")
                .loaded_page()
                .is_none(),
            "Protocol must synchronously project Page absence after the Core commit"
        );
    }
}
