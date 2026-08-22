use crate::conn::{CdpConnection, TargetPageResidenceIdentity};

use super::{PageCommandTaskStep, start_session_owner_navigation_from_renderer_request_with_trace};

/// Starts one actor-selected replacement of a Target's initial empty
/// Document.
///
/// Trigger commands have already returned or are returning independently.
/// This boundary resolves only the exact browser Page captured by the neutral
/// input, revalidates Core's initial-Document state, and starts the existing
/// navigation participant without consulting any frontend attachment.
pub(crate) fn start_page_owned_initial_target_navigation(
    conn: &mut CdpConnection,
    owner: &TargetPageResidenceIdentity,
    url: &str,
) -> Option<PageCommandTaskStep> {
    if !conn.browser_owner_accepts_initial_target_navigation(owner, url) {
        return None;
    }
    let Some(owner_route) = conn.target_page_owner_route_if_current(owner) else {
        tracing::debug!(
            browser_context_id = owner.browser_context_id(),
            target_id = owner.target_id(),
            loaded_page_generation = owner.loaded_page_generation(),
            url,
            "dropping initial Target navigation produced for a stale Page authority"
        );
        return None;
    };
    let mut owner_scope = conn.scoped_none_session_owner_route_override(owner_route);
    Some(
        start_session_owner_navigation_from_renderer_request_with_trace(
            owner_scope.conn_mut(),
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
