use moli_core::browser_host::BrowserAuxiliaryNavigationKind;

use crate::conn::{CdpConnection, TargetPageResidenceIdentity};

use super::{PageCommandTaskStep, start_session_owner_navigation_from_renderer_request_with_trace};

/// Starts one actor-selected auxiliary browsing-context navigation.
///
/// Initial-Document work is generation-scoped because it exists only to
/// replace that exact bootstrap Document. Named-target reuse is Target/Page-
/// slot scoped: a navigation accepted later for the same browsing context
/// must survive an earlier queued navigation replacing its Document.
pub(crate) fn start_page_owned_auxiliary_navigation(
    conn: &mut CdpConnection,
    owner: &TargetPageResidenceIdentity,
    url: &str,
    kind: BrowserAuxiliaryNavigationKind,
) -> Option<PageCommandTaskStep> {
    let owner_route = match kind {
        BrowserAuxiliaryNavigationKind::InitialDocument => {
            conn.target_page_owner_route_if_current(owner)
        }
        BrowserAuxiliaryNavigationKind::NamedTargetReuse => {
            conn.target_page_owner_route_if_same_slot(owner)
        }
    };
    let Some(owner_route) = owner_route else {
        tracing::debug!(
            browser_context_id = owner.browser_context_id(),
            target_id = owner.target_id(),
            loaded_page_generation = owner.loaded_page_generation(),
            url,
            ?kind,
            "dropping auxiliary navigation produced for a stale Page authority"
        );
        return None;
    };
    let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
    let conn = owner_scope.conn_mut();
    if !conn
        .runtime_session_owner_slot(None)
        .is_ok_and(|slot| slot.has_loaded_page())
    {
        tracing::debug!(
            browser_context_id = owner.browser_context_id(),
            target_id = owner.target_id(),
            loaded_page_generation = owner.loaded_page_generation(),
            url,
            ?kind,
            "dropping auxiliary navigation after its physical Page retired"
        );
        return None;
    }
    if kind == BrowserAuxiliaryNavigationKind::InitialDocument
        && !conn.runtime_session_owner_should_start_initial_document_navigation(None)
    {
        return None;
    }
    Some(
        start_session_owner_navigation_from_renderer_request_with_trace(
            conn,
            None,
            url,
            "GET",
            None,
            &[],
            moli_fetch::BrowserNavigationRequestKind::Navigate,
            None,
        ),
    )
}
