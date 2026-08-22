use moli_core::{RendererOutputResidenceIdentity, browser_host::BrowserPageOwnerKey};

use crate::conn::{CdpConnection, CdpSessionRoute, RendererPageResidenceIdentity};

/// Protocol owner frozen when one renderer stream opens.
///
/// A Page can publish its final batch before the asynchronous transport is
/// drained, while the protocol owner has already installed its replacement
/// Page. Resolving every batch through the *current* Page would therefore lose
/// the old stream's target. The target identity is stable across that window;
/// the concrete session remains deliberately dynamic because sessions may
/// attach or detach without restarting the renderer stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RendererPublicationOwner {
    PageTarget {
        page_owner: BrowserPageOwnerKey,
        renderer_page: RendererPageResidenceIdentity,
    },
    BrowserContext {
        browser_context_id: String,
    },
}

/// Exact protocol delivery route selected for one renderer publication.
///
/// An attached session is already an exact route. A publication without a
/// session instead carries the owner route needed to enter the correct parked
/// target without promoting it into the active target slot. This type contains
/// no output payload and grants no renderer execution authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RendererPublicationRoute {
    AttachedSession {
        session_id: String,
        projection: RendererPublicationProjection,
    },
    UnattachedOwner {
        owner_route: CdpSessionRoute,
        projection: RendererPublicationProjection,
    },
}

/// A current Page can project its complete renderer stream. A replaced Page
/// remains routable only for final Network facts whose request correlations
/// are retained by the target; every other historical record is stale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RendererPublicationProjection {
    CurrentPage,
    RetiringNetworkOnly,
}

impl RendererPublicationRoute {
    fn for_target(
        browser_context_id: String,
        target_id: Option<String>,
        session_id: Option<String>,
        projection: RendererPublicationProjection,
    ) -> Self {
        if let Some(session_id) = session_id {
            return Self::AttachedSession {
                session_id,
                projection,
            };
        }
        let owner_route = match target_id {
            Some(target_id) => CdpSessionRoute::BackgroundTarget {
                browser_context_id,
                target_id,
            },
            None => CdpSessionRoute::ActiveTarget {
                browser_context_id,
                target_id: None,
            },
        };
        Self::UnattachedOwner {
            owner_route,
            projection,
        }
    }
}

pub(crate) fn renderer_publication_owners(
    conn: &CdpConnection,
    residence: RendererOutputResidenceIdentity,
) -> Vec<RendererPublicationOwner> {
    match residence {
        // A Page stream is bound by the navigation/initial-document
        // transaction that reserved that exact renderer Page. Inferring its
        // target from the mutable inventory at `Opened` time is ambiguous:
        // protocol can transiently retain two handles to the same Page while
        // moving a target between active/background residence. Leave Page
        // discovery empty and let the explicit binding win in either
        // open-before-bind or bind-before-open order.
        RendererOutputResidenceIdentity::Page { .. } => Vec::new(),
        RendererOutputResidenceIdentity::SharedWorker {
            browser_context_runtime_id,
            ..
        }
        | RendererOutputResidenceIdentity::ServiceWorker {
            browser_context_runtime_id,
            ..
        } => conn
            .browser_context
            .iter()
            .chain(conn.inactive_browser_contexts.iter())
            .filter(|browser_context| {
                browser_context.routes_renderer_browser_context_runtime(browser_context_runtime_id)
            })
            .map(|browser_context| RendererPublicationOwner::BrowserContext {
                browser_context_id: browser_context.id.clone(),
            })
            .collect(),
    }
}

impl RendererPublicationOwner {
    /// Resolves the current session projection for a stable renderer owner.
    ///
    /// Returning `None` means the target/browser context was retired after
    /// this stream opened. Its already-admitted cursor remains settled, but no
    /// historical output may be projected into a replacement owner.
    pub(crate) fn resolve(&self, conn: &CdpConnection) -> Option<RendererPublicationRoute> {
        match self {
            Self::BrowserContext { browser_context_id } => {
                let browser_context = conn
                    .browser_context
                    .iter()
                    .chain(conn.inactive_browser_contexts.iter())
                    .find(|browser_context| browser_context.id == *browser_context_id)?;
                Some(RendererPublicationRoute::UnattachedOwner {
                    owner_route: CdpSessionRoute::ActiveTarget {
                        browser_context_id: browser_context.id.clone(),
                        target_id: None,
                    },
                    projection: RendererPublicationProjection::CurrentPage,
                })
            }
            Self::PageTarget {
                page_owner,
                renderer_page,
                ..
            } => conn
                .browser_context
                .iter()
                .chain(conn.inactive_browser_contexts.iter())
                .filter(|browser_context| browser_context.id == page_owner.browser_context_id())
                .find_map(|browser_context| {
                    let (runtime_slot, route_target_id, session_id) =
                        if browser_context.active_target_id() == Some(page_owner.target_id()) {
                            (
                                &browser_context.active_target.runtime_slot,
                                None,
                                browser_context.active_session_id_owned(),
                            )
                        } else {
                            let target =
                                browser_context.background_target(page_owner.target_id())?;
                            (
                                target.runtime_slot(),
                                Some(page_owner.target_id().to_owned()),
                                browser_context
                                    .primary_session_id_for_target(page_owner.target_id())
                                    .map(str::to_owned),
                            )
                        };
                    let projection = if runtime_slot.routes_renderer_page(*renderer_page) {
                        RendererPublicationProjection::CurrentPage
                    } else if runtime_slot.routes_retiring_renderer_page(*renderer_page) {
                        RendererPublicationProjection::RetiringNetworkOnly
                    } else {
                        return None;
                    };
                    Some(RendererPublicationRoute::for_target(
                        browser_context.id.clone(),
                        route_target_id,
                        session_id,
                        projection,
                    ))
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use moli_core::{PageId, RendererOwnerLocalHostId, page::RendererDevToolsAgentToken};

    use crate::conn::{BrowserContext, CdpConnection};

    use super::*;

    #[test]
    fn pending_renderer_page_route_is_independent_of_current_page_generation() {
        let mut conn = CdpConnection::default();
        let mut browser_context = BrowserContext::new("BID-publication".to_owned());
        browser_context.set_active_target_id("TID-publication");
        browser_context.attach_active_session("SID-publication");
        let renderer_page = RendererPageResidenceIdentity::new(
            RendererOwnerLocalHostId::new_for_testing(7),
            PageId::new_for_testing(11),
        );
        conn.browser_context = Some(browser_context);
        let navigation = conn
            .start_document_navigation_for_session_owner(
                Some("SID-publication"),
                "LOADER-publication".to_owned(),
            )
            .expect("pending navigation should start");
        let candidate = conn
            .prepare_renderer_agent_candidate_token_for_session_owner(
                Some("SID-publication"),
                &navigation,
                RendererDevToolsAgentToken::allocate(),
            )
            .expect("pending navigation should accept a renderer agent candidate");
        conn.commit_renderer_agent_candidate_for_session_owner(
            Some("SID-publication"),
            candidate,
            renderer_page,
        )
        .expect("committed candidate should bind the exact pending renderer Page");
        let owner = RendererPublicationOwner::PageTarget {
            page_owner: BrowserPageOwnerKey::new("BID-publication", "TID-publication"),
            renderer_page,
        };

        assert!(matches!(
            owner.resolve(&conn),
            Some(RendererPublicationRoute::AttachedSession { ref session_id, .. })
                if session_id == "SID-publication"
        ));

        conn.browser_context
            .as_mut()
            .unwrap()
            .active_target
            .runtime_slot
            .bump_loaded_page_generation();
        assert!(
            owner.resolve(&conn).is_some(),
            "a future renderer Page route must not borrow the old current Page generation"
        );

        conn.start_document_navigation_for_session_owner(
            Some("SID-publication"),
            "LOADER-successor".to_owned(),
        )
        .expect("successor navigation should start");
        assert!(
            owner.resolve(&conn).is_none(),
            "superseding the exact renderer Page reservation must retire its output route"
        );
    }
}
