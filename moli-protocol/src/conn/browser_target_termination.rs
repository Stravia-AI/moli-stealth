use moli_core::{
    browser_host::{
        BrowserPageOwnerKey, BrowserTargetResidence, BrowserTargetTerminationKind,
        BrowserTargetTerminationRequest,
    },
    page::{Page, RendererPageLifetimeOwner},
};

use super::{
    BackgroundProtocolEvent, CdpConnection, ClosedPageTarget, ProjectedActiveTargetClose,
    ProjectedClosedPageTarget,
    browser_fact_projection::BrowserTargetTerminationFactProjection,
    browser_target_engine_handoff::{
        BrowserTargetPromotionStart, CompletedBrowserTargetPromotion, PendingBrowserTargetPromotion,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrowserTargetTerminationProjectionKind {
    Crash,
    PageClose,
    TargetClose,
}

impl BrowserTargetTerminationProjectionKind {
    fn browser_kind(self) -> BrowserTargetTerminationKind {
        match self {
            Self::Crash => BrowserTargetTerminationKind::Crash,
            Self::PageClose | Self::TargetClose => BrowserTargetTerminationKind::Close,
        }
    }
}

pub(crate) enum BrowserTargetTerminationProjection {
    Crashed {
        inspector_session_ids: Vec<Option<String>>,
        browser_fact: Option<BrowserTargetTerminationFactProjection>,
    },
    Closed {
        closed: ClosedPageTarget,
        browser_fact: Option<BrowserTargetTerminationFactProjection>,
    },
}

enum TargetTerminationAsyncCleanup {
    Crash {
        inspector_session_ids: Vec<Option<String>>,
        retired_page: Option<Page>,
        retired_renderer_page_owner: Option<RendererPageLifetimeOwner>,
        browser_fact: Option<BrowserTargetTerminationFactProjection>,
    },
    Close {
        projected: ProjectedClosedPageTarget,
        retired_renderer_page_owner: Option<RendererPageLifetimeOwner>,
        browser_fact: Option<BrowserTargetTerminationFactProjection>,
    },
    ActiveTargetClose {
        projected: ProjectedActiveTargetClose,
        retired_renderer_page_owner: Option<RendererPageLifetimeOwner>,
        browser_fact: Option<BrowserTargetTerminationFactProjection>,
    },
}

struct RetiredRendererPageCleanup {
    page: Option<Page>,
    owner: Option<RendererPageLifetimeOwner>,
}

impl RetiredRendererPageCleanup {
    fn new(page: Option<Page>, owner: Option<RendererPageLifetimeOwner>) -> Self {
        Self { page, owner }
    }

    fn is_empty(&self) -> bool {
        self.page.is_none() && self.owner.is_none()
    }

    async fn wait(self) {
        if let Some(owner) = self.owner {
            let _ = owner.close_async().await;
        }
        if let Some(page) = self.page {
            let _ = page.close_async().await;
        }
    }
}

/// Result of synchronously committing and projecting one Page-originated
/// Browser termination turn.
///
/// Core authority and physical Page absence are already committed when this
/// value is returned. Only destruction of the retired renderer Page may remain
/// as a move-owned participant.
pub(crate) enum BrowserPageTargetTerminationStart {
    Complete(BrowserTargetTerminationProjection),
    Pending(PendingBrowserPageTargetTermination),
}

/// Move-owned disposal participant for an actor-selected Page termination.
pub(crate) struct PendingBrowserPageTargetTermination {
    projection: BrowserTargetTerminationProjection,
    cleanup: RetiredRendererPageCleanup,
}

impl PendingBrowserPageTargetTermination {
    pub(crate) async fn wait(self) -> BrowserTargetTerminationProjection {
        self.cleanup.wait().await;
        self.projection
    }
}

/// Result of committing one exact `Target.closeTarget` in Browser Host.
pub(crate) enum BrowserTargetCloseStart {
    Complete(BrowserTargetTerminationProjection),
    Pending(PendingBrowserTargetClose),
}

pub(crate) struct PendingBrowserTargetClose {
    stage: PendingBrowserTargetCloseStage,
}

enum PendingBrowserTargetCloseStage {
    RetiredPage {
        cleanup: RetiredRendererPageCleanup,
        continuation: BrowserTargetCloseContinuation,
    },
    Promotion {
        pending: PendingBrowserTargetPromotion,
        projection: BrowserTargetTerminationProjection,
    },
}

pub(crate) struct CompletedBrowserTargetClose {
    stage: CompletedBrowserTargetCloseStage,
}

enum CompletedBrowserTargetCloseStage {
    RetiredPage(BrowserTargetCloseContinuation),
    Promotion {
        completed: CompletedBrowserTargetPromotion,
        projection: BrowserTargetTerminationProjection,
    },
}

struct BrowserTargetCloseContinuation {
    projection: BrowserTargetTerminationProjection,
    owner_browser_context_id: String,
    promoted_target_id: Option<String>,
    synchronize_promoted_target: bool,
}

impl PendingBrowserTargetClose {
    pub(crate) async fn wait(self) -> CompletedBrowserTargetClose {
        let stage = match self.stage {
            PendingBrowserTargetCloseStage::RetiredPage {
                cleanup,
                continuation,
            } => {
                cleanup.wait().await;
                CompletedBrowserTargetCloseStage::RetiredPage(continuation)
            }
            PendingBrowserTargetCloseStage::Promotion {
                pending,
                projection,
            } => CompletedBrowserTargetCloseStage::Promotion {
                completed: pending.wait().await,
                projection,
            },
        };
        CompletedBrowserTargetClose { stage }
    }
}

impl CdpConnection {
    pub(crate) fn capture_browser_target_termination_for_owner(
        &self,
        owner: &BrowserPageOwnerKey,
        kind: BrowserTargetTerminationProjectionKind,
    ) -> Option<BrowserTargetTerminationRequest> {
        self.browser_host_state
            .navigation_owner()
            .capture_target_termination(owner, kind.browser_kind())
    }

    pub(crate) fn capture_browser_target_termination_for_session_owner(
        &self,
        session_id: Option<&str>,
        kind: BrowserTargetTerminationProjectionKind,
    ) -> Option<BrowserTargetTerminationRequest> {
        let owner = self.target_page_owner_key_for_session(session_id)?;
        self.browser_host_state
            .navigation_owner()
            .capture_target_termination(&owner, kind.browser_kind())
    }

    pub(crate) fn capture_browser_target_termination_for_target(
        &self,
        target_id: &str,
        kind: BrowserTargetTerminationProjectionKind,
    ) -> Option<BrowserTargetTerminationRequest> {
        let browser_context = self.browser_context.as_ref()?;
        if !browser_context.is_active_target(target_id)
            && browser_context.background_target(target_id).is_none()
        {
            return None;
        }
        let owner = BrowserPageOwnerKey::new(browser_context.id.clone(), target_id.to_owned());
        self.browser_host_state
            .navigation_owner()
            .capture_target_termination(&owner, kind.browser_kind())
    }

    /// Starts one actor-selected Page.crash/Page.close transition without
    /// crossing an async boundary.
    ///
    /// This is deliberately narrower than explicit Target.closeTarget: the
    /// latter may first require an asynchronous BrowserContext engine handoff.
    /// Page-originated input already carries an exact route and can therefore
    /// commit Core authority and project physical absence in one short owner
    /// turn. Retired renderer destruction is returned as a participant.
    pub(crate) fn start_browser_page_target_termination(
        &mut self,
        request: BrowserTargetTerminationRequest,
        projection_kind: BrowserTargetTerminationProjectionKind,
        out: &mut Vec<BackgroundProtocolEvent>,
        close_reason: &'static str,
    ) -> Option<BrowserPageTargetTerminationStart> {
        if !matches!(
            projection_kind,
            BrowserTargetTerminationProjectionKind::Crash
                | BrowserTargetTerminationProjectionKind::PageClose
        ) || request.kind() != projection_kind.browser_kind()
        {
            return None;
        }

        let cleanup = self.commit_browser_target_termination_projection(
            request,
            projection_kind,
            out,
            close_reason,
        )?;
        self.debug_assert_browser_target_topology_projection();
        let (projection, retired_cleanup) = match cleanup {
            TargetTerminationAsyncCleanup::Crash {
                inspector_session_ids,
                retired_page,
                retired_renderer_page_owner,
                browser_fact,
            } => (
                BrowserTargetTerminationProjection::Crashed {
                    inspector_session_ids,
                    browser_fact,
                },
                RetiredRendererPageCleanup::new(retired_page, retired_renderer_page_owner),
            ),
            TargetTerminationAsyncCleanup::Close {
                projected,
                retired_renderer_page_owner,
                browser_fact,
            } => {
                let (closed, retired_page) = projected.into_parts();
                (
                    BrowserTargetTerminationProjection::Closed {
                        closed,
                        browser_fact,
                    },
                    RetiredRendererPageCleanup::new(retired_page, retired_renderer_page_owner),
                )
            }
            TargetTerminationAsyncCleanup::ActiveTargetClose { .. } => {
                tracing::error!(
                    ?projection_kind,
                    "Page termination unexpectedly requested Target.closeTarget projection"
                );
                return None;
            }
        };

        Some(if retired_cleanup.is_empty() {
            BrowserPageTargetTerminationStart::Complete(projection)
        } else {
            BrowserPageTargetTerminationStart::Pending(PendingBrowserPageTargetTermination {
                projection,
                cleanup: retired_cleanup,
            })
        })
    }

    /// Starts an exact top-level Target close in one short owner turn.
    ///
    /// Context selection, Core authority and matching physical absence commit
    /// synchronously. Retired Page disposal and promoted-Page renderer state
    /// synchronization are returned as move-owned participants.
    pub(crate) fn start_browser_target_close(
        &mut self,
        request: BrowserTargetTerminationRequest,
        out: &mut Vec<BackgroundProtocolEvent>,
        close_reason: &'static str,
    ) -> Option<BrowserTargetCloseStart> {
        if request.kind() != BrowserTargetTerminationKind::Close {
            return None;
        }
        let owner_browser_context_id = request.owner().browser_context_id().to_owned();
        let restore_browser_context_id = self
            .browser_context
            .as_ref()
            .map(|browser_context| browser_context.id.clone());
        let activated_for_target_close =
            restore_browser_context_id.as_deref() != Some(owner_browser_context_id.as_str());
        if activated_for_target_close
            && !self.activate_browser_context_by_id(&owner_browser_context_id)
        {
            return None;
        }

        let cleanup = self.commit_browser_target_termination_projection(
            request,
            BrowserTargetTerminationProjectionKind::TargetClose,
            out,
            close_reason,
        );
        let Some(cleanup) = cleanup else {
            self.restore_target_termination_browser_context(
                activated_for_target_close
                    .then_some(restore_browser_context_id.as_deref())
                    .flatten(),
            );
            return None;
        };
        self.debug_assert_browser_target_topology_projection();
        let (closed, retired_cleanup, promoted_target_id, browser_fact) = match cleanup {
            TargetTerminationAsyncCleanup::Close {
                projected,
                retired_renderer_page_owner,
                browser_fact,
            } => {
                let (closed, retired_page) = projected.into_parts();
                (
                    closed,
                    RetiredRendererPageCleanup::new(retired_page, retired_renderer_page_owner),
                    None,
                    browser_fact,
                )
            }
            TargetTerminationAsyncCleanup::ActiveTargetClose {
                projected,
                retired_renderer_page_owner,
                browser_fact,
            } => {
                let (closed, retired_page, promoted_target_id) = projected.into_parts();
                (
                    closed,
                    RetiredRendererPageCleanup::new(retired_page, retired_renderer_page_owner),
                    promoted_target_id,
                    browser_fact,
                )
            }
            TargetTerminationAsyncCleanup::Crash { .. } => {
                tracing::error!("Target.closeTarget unexpectedly produced a crash projection");
                self.restore_target_termination_browser_context(
                    activated_for_target_close
                        .then_some(restore_browser_context_id.as_deref())
                        .flatten(),
                );
                return None;
            }
        };
        let continuation = BrowserTargetCloseContinuation {
            projection: BrowserTargetTerminationProjection::Closed {
                closed,
                browser_fact,
            },
            owner_browser_context_id,
            promoted_target_id,
            synchronize_promoted_target: true,
        };
        Some(if !retired_cleanup.is_empty() {
            self.restore_target_termination_browser_context(
                activated_for_target_close
                    .then_some(restore_browser_context_id.as_deref())
                    .flatten(),
            );
            BrowserTargetCloseStart::Pending(PendingBrowserTargetClose {
                stage: PendingBrowserTargetCloseStage::RetiredPage {
                    cleanup: retired_cleanup,
                    continuation,
                },
            })
        } else {
            self.continue_browser_target_close_after_page_disposal(
                continuation,
                activated_for_target_close
                    .then_some(restore_browser_context_id)
                    .flatten(),
            )
        })
    }

    /// Starts one exact Target close as part of whole-Context disposal.
    ///
    /// The Context may be physically inactive and is already reserved against
    /// new owner work. Projection therefore uses the exact Page route without
    /// changing global Context selection. If closing its active Target
    /// promotes a retained target, that target is deliberately not
    /// synchronized with frontend settings because the same disposal chain
    /// will close it next.
    pub(crate) fn start_browser_target_close_for_context_disposal(
        &mut self,
        request: BrowserTargetTerminationRequest,
        out: &mut Vec<BackgroundProtocolEvent>,
        close_reason: &'static str,
    ) -> Option<BrowserTargetCloseStart> {
        if request.kind() != BrowserTargetTerminationKind::Close {
            return None;
        }
        let owner_browser_context_id = request.owner().browser_context_id().to_owned();
        let cleanup = self.commit_browser_target_termination_projection(
            request,
            BrowserTargetTerminationProjectionKind::TargetClose,
            out,
            close_reason,
        )?;
        self.debug_assert_browser_target_topology_projection();
        let (closed, retired_cleanup, promoted_target_id, browser_fact) = match cleanup {
            TargetTerminationAsyncCleanup::Close {
                projected,
                retired_renderer_page_owner,
                browser_fact,
            } => {
                let (closed, retired_page) = projected.into_parts();
                (
                    closed,
                    RetiredRendererPageCleanup::new(retired_page, retired_renderer_page_owner),
                    None,
                    browser_fact,
                )
            }
            TargetTerminationAsyncCleanup::ActiveTargetClose {
                projected,
                retired_renderer_page_owner,
                browser_fact,
            } => {
                let (closed, retired_page, promoted_target_id) = projected.into_parts();
                (
                    closed,
                    RetiredRendererPageCleanup::new(retired_page, retired_renderer_page_owner),
                    promoted_target_id,
                    browser_fact,
                )
            }
            TargetTerminationAsyncCleanup::Crash { .. } => {
                tracing::error!(
                    "BrowserContext disposal Target close unexpectedly produced a crash projection"
                );
                return None;
            }
        };
        let continuation = BrowserTargetCloseContinuation {
            projection: BrowserTargetTerminationProjection::Closed {
                closed,
                browser_fact,
            },
            owner_browser_context_id,
            promoted_target_id,
            synchronize_promoted_target: false,
        };
        Some(if retired_cleanup.is_empty() {
            self.continue_browser_target_close_after_page_disposal(continuation, None)
        } else {
            BrowserTargetCloseStart::Pending(PendingBrowserTargetClose {
                stage: PendingBrowserTargetCloseStage::RetiredPage {
                    cleanup: retired_cleanup,
                    continuation,
                },
            })
        })
    }

    pub(crate) fn continue_browser_target_close(
        &mut self,
        completed: CompletedBrowserTargetClose,
    ) -> BrowserTargetCloseStart {
        match completed.stage {
            CompletedBrowserTargetCloseStage::RetiredPage(continuation) => {
                self.continue_browser_target_close_after_page_disposal(continuation, None)
            }
            CompletedBrowserTargetCloseStage::Promotion {
                completed,
                projection,
            } => {
                if let Err(error) =
                    self.finish_promote_background_target_to_active_for_connection(completed)
                {
                    tracing::warn!(
                        %error,
                        "failed to synchronize the promoted Target after Target.closeTarget"
                    );
                }
                BrowserTargetCloseStart::Complete(projection)
            }
        }
    }

    fn continue_browser_target_close_after_page_disposal(
        &mut self,
        continuation: BrowserTargetCloseContinuation,
        restore_browser_context_id: Option<String>,
    ) -> BrowserTargetCloseStart {
        let BrowserTargetCloseContinuation {
            projection,
            owner_browser_context_id,
            promoted_target_id,
            synchronize_promoted_target,
        } = continuation;
        if !synchronize_promoted_target {
            return BrowserTargetCloseStart::Complete(projection);
        }
        let selected_browser_context_id = self
            .browser_context
            .as_ref()
            .map(|browser_context| browser_context.id.clone());
        let activated_for_continuation =
            selected_browser_context_id.as_deref() != Some(owner_browser_context_id.as_str());
        let restore_browser_context_id = restore_browser_context_id.or_else(|| {
            activated_for_continuation
                .then_some(selected_browser_context_id)
                .flatten()
        });
        if activated_for_continuation
            && !self.activate_browser_context_by_id(&owner_browser_context_id)
        {
            return BrowserTargetCloseStart::Complete(projection);
        }

        if let Some(promoted_target_id) = promoted_target_id.as_deref() {
            match self.start_promote_background_target_to_active_for_connection(promoted_target_id)
            {
                Ok(BrowserTargetPromotionStart::Complete(_)) => {}
                Ok(BrowserTargetPromotionStart::Pending(pending)) => {
                    self.restore_target_termination_browser_context(
                        restore_browser_context_id.as_deref(),
                    );
                    return BrowserTargetCloseStart::Pending(PendingBrowserTargetClose {
                        stage: PendingBrowserTargetCloseStage::Promotion {
                            pending,
                            projection,
                        },
                    });
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "failed to promote a retained Target after Target.closeTarget"
                    );
                }
            }
        } else {
            self.refresh_active_browser_context_loader();
        }
        self.restore_target_termination_browser_context(restore_browser_context_id.as_deref());
        BrowserTargetCloseStart::Complete(projection)
    }

    /// Commits Browser Core authority before synchronously projecting the
    /// physical Target/Page absence. Only retired Page disposal and optional
    /// active-target promotion may await afterward.
    #[cfg(test)]
    pub(crate) async fn commit_browser_target_termination_async(
        &mut self,
        request: BrowserTargetTerminationRequest,
        projection_kind: BrowserTargetTerminationProjectionKind,
        out: &mut Vec<BackgroundProtocolEvent>,
        close_reason: &'static str,
    ) -> Option<BrowserTargetTerminationProjection> {
        if projection_kind == BrowserTargetTerminationProjectionKind::TargetClose {
            let mut step = self.start_browser_target_close(request, out, close_reason)?;
            loop {
                match step {
                    BrowserTargetCloseStart::Complete(projection) => return Some(projection),
                    BrowserTargetCloseStart::Pending(pending) => {
                        step = self.continue_browser_target_close(pending.wait().await);
                    }
                }
            }
        }
        match self.start_browser_page_target_termination(
            request,
            projection_kind,
            out,
            close_reason,
        )? {
            BrowserPageTargetTerminationStart::Complete(projection) => Some(projection),
            BrowserPageTargetTerminationStart::Pending(pending) => Some(pending.wait().await),
        }
    }

    fn restore_target_termination_browser_context(
        &mut self,
        restore_browser_context_id: Option<&str>,
    ) {
        if let Some(restore_browser_context_id) = restore_browser_context_id
            && self.has_browser_context_id(restore_browser_context_id)
        {
            let _ = self.activate_browser_context_by_id(restore_browser_context_id);
        }
    }

    fn commit_browser_target_termination_projection(
        &mut self,
        request: BrowserTargetTerminationRequest,
        projection_kind: BrowserTargetTerminationProjectionKind,
        out: &mut Vec<BackgroundProtocolEvent>,
        close_reason: &'static str,
    ) -> Option<TargetTerminationAsyncCleanup> {
        let owner_browser_context_id = request.owner().browser_context_id().to_owned();
        let owner_target_id = request.owner().target_id().to_owned();
        let owner_route = self.target_page_owner_route_if_current(request.page())?;
        let permit = self
            .browser_host_state
            .navigation_owner()
            .prepare_target_termination(request)?;

        let mut route_scope = self.scoped_none_session_owner_route_override(owner_route);
        let conn = route_scope.conn_mut();
        let inspector_session_ids = (projection_kind
            == BrowserTargetTerminationProjectionKind::Crash)
            .then(|| conn.page_event_session_ids_for_session_owner(None));

        // There is deliberately no await, frontend flush, or callback between
        // this authoritative commit and the matching physical projection.
        let mut termination = match conn.browser_host_state.commit_target_termination(permit) {
            Ok(termination) => termination,
            Err(error) => {
                tracing::warn!(
                    target_id = owner_target_id,
                    browser_context_id = owner_browser_context_id,
                    %error,
                    "Browser Owner rejected a prepared Target termination"
                );
                return None;
            }
        };
        let browser_fact = match conn.take_target_termination_fact(&termination) {
            Ok(projection) => Some(projection),
            Err(error) => {
                tracing::error!(
                    %error,
                    target_id = owner_target_id,
                    browser_context_id = owner_browser_context_id,
                    "Target terminal committed without an exact frontend Browser fact"
                );
                None
            }
        };
        if let Some(inspector_session_ids) = inspector_session_ids.as_ref() {
            for inspector_session_id in inspector_session_ids {
                let _ = conn.with_target_devtools_session_state_for_session_mut(
                    inspector_session_id.as_deref(),
                    |state| {
                        state
                            .runtime_session_state
                            .record_inspector_target_crashed();
                    },
                );
            }
        }
        Some(match projection_kind {
            BrowserTargetTerminationProjectionKind::Crash => {
                let retired_page = conn
                    .project_target_crash_for_none_session_owner_after_browser_owner_commit(
                        &termination,
                    )
                    .expect("prepared crash route must remain available after owner commit");
                TargetTerminationAsyncCleanup::Crash {
                    inspector_session_ids: inspector_session_ids
                        .expect("crash projection must capture inspector sessions"),
                    retired_page,
                    retired_renderer_page_owner: termination.take_retired_renderer_page_owner(),
                    browser_fact,
                }
            }
            BrowserTargetTerminationProjectionKind::PageClose => {
                let projected = conn
                    .project_page_close_for_none_session_owner_after_browser_owner_commit(
                        &termination,
                    )
                    .expect("prepared Page.close route must remain available after owner commit");
                TargetTerminationAsyncCleanup::Close {
                    projected,
                    retired_renderer_page_owner: termination.take_retired_renderer_page_owner(),
                    browser_fact,
                }
            }
            BrowserTargetTerminationProjectionKind::TargetClose => {
                match termination
                    .closed_target_residence()
                    .expect("Target.close must commit a Core Target topology removal")
                {
                    BrowserTargetResidence::Active => {
                        let projected = conn
                            .project_active_target_close_after_browser_owner_commit(&termination)
                            .expect(
                                "prepared active Target.close route must remain available after owner commit",
                            );
                        TargetTerminationAsyncCleanup::ActiveTargetClose {
                            projected,
                            retired_renderer_page_owner: termination
                                .take_retired_renderer_page_owner(),
                            browser_fact,
                        }
                    }
                    BrowserTargetResidence::Background => {
                        let projected = conn
                            .project_background_target_close_after_browser_owner_commit(
                                &termination,
                                out,
                                close_reason,
                            )
                            .expect(
                                "prepared background Target.close route must remain available after owner commit",
                            );
                        TargetTerminationAsyncCleanup::Close {
                            projected,
                            retired_renderer_page_owner: termination
                                .take_retired_renderer_page_owner(),
                            browser_fact,
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::{BrowserContext, LoadedNavigationRendererAttachmentCommit};
    use moli_core::browser_host::{BrowserFact, BrowserHostActor, BrowserOwnerInput};
    use moli_core::page::RendererMainDocumentCommit;

    #[tokio::test]
    async fn page_close_uses_exact_browser_owner_after_frontend_session_replacement() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let target_id = conn
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id)
            .expect("default target")
            .to_owned();
        conn.browser_context
            .as_mut()
            .expect("browser context")
            .attach_active_session("SID-old".to_owned());
        let page_residence = conn
            .target_page_residence_handle_for_session(Some("SID-old"))
            .expect("active target residence");
        let previous_generation = page_residence.generation();
        let request = conn
            .capture_browser_target_termination_for_session_owner(
                Some("SID-old"),
                BrowserTargetTerminationProjectionKind::PageClose,
            )
            .expect("live Page.close should capture exact Browser owner");
        let previous_page = request.page().clone();

        let browser_context = conn.browser_context.as_mut().expect("browser context");
        assert_eq!(
            browser_context.detach_active_session().as_deref(),
            Some("SID-old")
        );
        browser_context.attach_active_session("SID-new".to_owned());

        let mut events = Vec::new();
        let projection = conn
            .commit_browser_target_termination_async(
                request,
                BrowserTargetTerminationProjectionKind::PageClose,
                &mut events,
                "Page closed",
            )
            .await
            .expect("session replacement must not cancel Browser Target close");
        let BrowserTargetTerminationProjection::Closed {
            closed,
            browser_fact,
        } = projection
        else {
            panic!("Page.close must project a closed Target");
        };
        assert!(browser_fact.is_some());

        assert_eq!(closed.target_id, target_id);
        assert_eq!(closed.primary_session_id.as_deref(), Some("SID-new"));
        assert_eq!(page_residence.generation(), previous_generation + 1);
        assert!(
            conn.browser_context
                .as_ref()
                .is_some_and(|browser_context| !browser_context.has_active_target())
        );
        let fact = conn
            .browser_fact_snapshot_for_test()
            .into_iter()
            .find(|fact| matches!(fact.fact(), BrowserFact::TargetClosed { .. }))
            .expect("Page.close production path should publish TargetClosed");
        assert_eq!(
            fact.fact(),
            &BrowserFact::TargetClosed {
                previous_page,
                pending_navigation: None,
            }
        );
        assert_eq!(
            fact.page_residence().loaded_page_generation(),
            previous_generation + 1
        );
    }

    #[tokio::test]
    async fn crash_advances_one_generation_and_rejects_second_terminal_capture() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let page_residence = conn
            .target_page_residence_handle_for_session(None)
            .expect("default target residence");
        let previous_generation = page_residence.generation();
        let request = conn
            .capture_browser_target_termination_for_session_owner(
                None,
                BrowserTargetTerminationProjectionKind::Crash,
            )
            .expect("live Page.crash should capture exact Browser owner");
        let previous_page = request.page().clone();
        let mut events = Vec::new();

        let projection = conn
            .commit_browser_target_termination_async(
                request,
                BrowserTargetTerminationProjectionKind::Crash,
                &mut events,
                "Page crashed",
            )
            .await
            .expect("exact crash should project");
        let BrowserTargetTerminationProjection::Crashed { browser_fact, .. } = projection else {
            panic!("Page.crash must project a crashed Target");
        };
        assert!(browser_fact.is_some());

        assert_eq!(page_residence.generation(), previous_generation + 1);
        let fact = conn
            .browser_fact_snapshot_for_test()
            .into_iter()
            .find(|fact| matches!(fact.fact(), BrowserFact::TargetCrashed { .. }))
            .expect("Page.crash production path should publish TargetCrashed");
        assert_eq!(
            fact.fact(),
            &BrowserFact::TargetCrashed {
                previous_page,
                pending_navigation: None,
            }
        );
        assert_eq!(
            fact.page_residence().loaded_page_generation(),
            previous_generation + 1
        );
        assert!(
            conn.capture_browser_target_termination_for_session_owner(
                None,
                BrowserTargetTerminationProjectionKind::Crash,
            )
            .is_none(),
            "crashed Target must not commit another terminal transition"
        );
    }

    #[tokio::test]
    async fn crashed_target_remains_closable_through_browser_owner_transaction() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let target_id = conn
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id)
            .expect("default target")
            .to_owned();
        let page_residence = conn
            .target_page_residence_handle_for_session(None)
            .expect("default target residence");
        let crash = conn
            .capture_browser_target_termination_for_session_owner(
                None,
                BrowserTargetTerminationProjectionKind::Crash,
            )
            .expect("live Page should capture crash");
        let mut events = Vec::new();
        assert!(matches!(
            conn.commit_browser_target_termination_async(
                crash,
                BrowserTargetTerminationProjectionKind::Crash,
                &mut events,
                "Page crashed",
            )
            .await,
            Some(BrowserTargetTerminationProjection::Crashed { .. })
        ));

        let close = conn
            .capture_browser_target_termination_for_target(
                &target_id,
                BrowserTargetTerminationProjectionKind::TargetClose,
            )
            .expect("crashed Target should remain closable");
        let projection = conn
            .commit_browser_target_termination_async(
                close,
                BrowserTargetTerminationProjectionKind::TargetClose,
                &mut events,
                "Target closed",
            )
            .await
            .expect("crashed Target close should commit");
        let BrowserTargetTerminationProjection::Closed {
            closed,
            browser_fact,
        } = projection
        else {
            panic!("Target.closeTarget must project a closed crashed Target");
        };
        assert!(browser_fact.is_some());

        assert_eq!(closed.target_id, target_id);
        assert_eq!(page_residence.generation(), 2);
        assert!(
            conn.browser_context
                .as_ref()
                .is_some_and(|browser_context| !browser_context.has_active_target())
        );
    }

    #[tokio::test]
    async fn replaced_page_makes_delayed_page_close_stale_without_closing_target() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let page_residence = conn
            .target_page_residence_handle_for_session(None)
            .expect("default target residence");
        let request = conn
            .capture_browser_target_termination_for_session_owner(
                None,
                BrowserTargetTerminationProjectionKind::PageClose,
            )
            .expect("live Page.close should capture exact Browser owner");
        page_residence.advance_generation_for_test_fixture();
        let mut events = Vec::new();

        assert!(
            conn.commit_browser_target_termination_async(
                request,
                BrowserTargetTerminationProjectionKind::PageClose,
                &mut events,
                "Page closed",
            )
            .await
            .is_none()
        );
        assert!(
            conn.browser_context
                .as_ref()
                .is_some_and(BrowserContext::has_active_target)
        );
    }

    #[tokio::test]
    async fn browser_host_drops_queued_page_close_after_page_generation_replacement() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let target_id = conn
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id)
            .expect("default target")
            .to_owned();
        let page_residence = conn
            .target_page_residence_handle_for_session(None)
            .expect("default target residence");
        let request = conn
            .capture_browser_target_termination_for_session_owner(
                None,
                BrowserTargetTerminationProjectionKind::PageClose,
            )
            .expect("live Page.close should capture exact Browser owner");
        let (mut browser_host, handle) = BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(handle);
        conn.publish_browser_owner_input(BrowserOwnerInput::page_termination(request))
            .expect("live Browser Host should accept Page.close");

        page_residence.advance_generation_for_test_fixture();
        let dispatch = browser_host
            .complete_next_turn(&mut conn)
            .expect("queued Page.close Browser Host turn");
        let outcome = conn.finish_browser_host_turn_for_test(dispatch).await;
        let (events, scheduler_events, predecessor) = outcome.into_protocol_event_parts();

        assert!(events.is_empty());
        assert!(scheduler_events.is_empty());
        assert!(predecessor.is_none());
        assert_eq!(
            conn.browser_context
                .as_ref()
                .and_then(BrowserContext::active_target_id),
            Some(target_id.as_str()),
            "a queued old-Page close must not terminate its replacement"
        );
    }

    #[tokio::test]
    async fn target_close_commits_only_after_browser_host_selects_its_turn() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let target_id = conn
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id)
            .expect("default target")
            .to_owned();
        let request = conn
            .capture_browser_target_termination_for_target(
                &target_id,
                BrowserTargetTerminationProjectionKind::TargetClose,
            )
            .expect("live Target should capture exact close authority");
        let (mut browser_host, handle) = BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(handle);
        conn.publish_browser_owner_input(BrowserOwnerInput::target_termination(request))
            .expect("live Browser Host should accept Target.closeTarget");

        assert_eq!(browser_host.ready_len(), 1);
        assert_eq!(
            conn.browser_context
                .as_ref()
                .and_then(BrowserContext::active_target_id),
            Some(target_id.as_str()),
            "publishing an input cannot execute Target.closeTarget"
        );

        let dispatch = browser_host
            .complete_next_turn(&mut conn)
            .expect("queued Target.closeTarget Browser Host turn");
        let outcome = conn.finish_browser_host_turn_for_test(dispatch).await;
        let (_, _, predecessor) = outcome.into_protocol_event_parts();

        assert!(predecessor.is_none());
        assert!(
            conn.browser_context
                .as_ref()
                .is_some_and(|browser_context| !browser_context.has_active_target()),
            "only the actor-selected turn may commit Target retirement"
        );
    }

    #[tokio::test]
    async fn target_close_participants_do_not_hold_temporary_context_selection() {
        let mut conn = CdpConnection::default();
        for (browser_context_id, target_id) in [
            ("context-a", "target-a"),
            ("context-b", "target-b"),
            ("context-c", "target-c"),
        ] {
            let mut browser_context = conn.new_browser_context(browser_context_id.to_owned());
            browser_context.set_active_target_id(target_id.to_owned());
            conn.try_insert_browser_context(browser_context)
                .expect("test BrowserContext should register");
        }

        assert!(conn.activate_browser_context_by_id("context-b"));
        let navigation = conn
            .start_document_navigation_for_session_owner(None, "LOADER-target-b".to_owned())
            .expect("target B navigation should start");
        let page = conn
            .load_page_via_runtime_async("data:text/html,<title>target-b</title>")
            .await
            .expect("target B Page should load");
        let final_url = page.final_url().clone();
        let document_commit = RendererMainDocumentCommit {
            frame_id: "target-b".to_owned(),
            loader_id: navigation.loader_id().to_owned(),
            url: final_url.to_string(),
            unreachable_url: None,
            security_origin: final_url.origin().ascii_serialization(),
            secure_context_type: "InsecureScheme".to_owned(),
            timestamp: 0.0,
        };
        conn.commit_loaded_page_replacement_for_session_owner_async(
            None,
            &navigation,
            page,
            &final_url,
            &document_commit,
            LoadedNavigationRendererAttachmentCommit::Prepare(None),
        )
        .await
        .expect("target B replacement should produce an outcome")
        .expect("target B replacement should commit");
        let request = conn
            .capture_browser_target_termination_for_target(
                "target-b",
                BrowserTargetTerminationProjectionKind::TargetClose,
            )
            .expect("loaded target B should capture exact close authority");
        let previous_page = request.page().clone();
        assert!(conn.activate_browser_context_by_id("context-a"));

        let mut events = Vec::new();
        let BrowserTargetCloseStart::Pending(pending) = conn
            .start_browser_target_close(request, &mut events, "Target closed")
            .expect("target B close should commit")
        else {
            panic!("retired target B Page should become a participant");
        };
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .selected_browser_context_id(),
            Some("context-a"),
            "starting a participant must restore the pre-turn selection"
        );
        let fact = conn
            .browser_fact_snapshot_for_test()
            .into_iter()
            .find(|fact| {
                fact.target_id().as_str() == "target-b"
                    && matches!(fact.fact(), BrowserFact::TargetClosed { .. })
            })
            .expect("Core TargetClosed fact must precede retired Page disposal");
        assert_eq!(
            fact.fact(),
            &BrowserFact::TargetClosed {
                previous_page,
                pending_navigation: None,
            }
        );

        assert!(conn.activate_browser_context_by_id("context-c"));
        let completed = pending.wait().await;
        assert!(matches!(
            conn.continue_browser_target_close(completed),
            BrowserTargetCloseStart::Complete(_)
        ));
        assert_eq!(
            conn.browser_host_state
                .navigation_owner()
                .selected_browser_context_id(),
            Some("context-c"),
            "an old completion must preserve a newer BrowserContext selection"
        );
        assert_eq!(
            conn.browser_context
                .as_ref()
                .map(|browser_context| browser_context.id.as_str()),
            Some("context-c")
        );
    }

    #[tokio::test]
    async fn browser_host_drops_queued_target_close_after_page_generation_replacement() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let target_id = conn
            .browser_context
            .as_ref()
            .and_then(BrowserContext::active_target_id)
            .expect("default target")
            .to_owned();
        let page_residence = conn
            .target_page_residence_handle_for_session(None)
            .expect("default target residence");
        let request = conn
            .capture_browser_target_termination_for_target(
                &target_id,
                BrowserTargetTerminationProjectionKind::TargetClose,
            )
            .expect("live Target should capture exact close authority");
        let (mut browser_host, handle) = BrowserHostActor::new(conn.browser_host_state());
        conn.install_browser_host_handle(handle);
        conn.publish_browser_owner_input(BrowserOwnerInput::target_termination(request))
            .expect("live Browser Host should accept Target.closeTarget");

        page_residence.advance_generation_for_test_fixture();
        let dispatch = browser_host
            .complete_next_turn(&mut conn)
            .expect("queued Target.closeTarget Browser Host turn");
        let outcome = conn.finish_browser_host_turn_for_test(dispatch).await;
        let (events, scheduler_events, predecessor) = outcome.into_protocol_event_parts();

        assert!(events.is_empty());
        assert!(scheduler_events.is_empty());
        assert!(predecessor.is_none());
        assert_eq!(
            conn.browser_context
                .as_ref()
                .and_then(BrowserContext::active_target_id),
            Some(target_id.as_str()),
            "a queued old-Page Target close must not terminate its replacement"
        );
    }

    #[tokio::test]
    async fn background_target_close_survives_primary_session_replacement() {
        let mut conn = CdpConnection::default();
        let mut browser_context = BrowserContext::new("BID-termination".to_owned());
        browser_context.set_active_target_id("TID-active".to_owned());
        browser_context.stage_background_target(
            "TID-background".to_owned(),
            Some("SID-old".to_owned()),
            "about:blank".to_owned(),
            Some("about:blank".to_owned()),
            None,
        );
        conn.insert_browser_context(browser_context);
        let request = conn
            .capture_browser_target_termination_for_session_owner(
                Some("SID-old"),
                BrowserTargetTerminationProjectionKind::TargetClose,
            )
            .expect("background target should capture close");
        let browser_context = conn.browser_context.as_mut().expect("browser context");
        assert_eq!(
            browser_context
                .replace_primary_session_for_target("TID-background", None)
                .as_deref(),
            Some("SID-old")
        );
        browser_context
            .replace_primary_session_for_target("TID-background", Some("SID-new".to_owned()));
        let mut events = Vec::new();

        let projection = conn
            .commit_browser_target_termination_async(
                request,
                BrowserTargetTerminationProjectionKind::TargetClose,
                &mut events,
                "Target closed",
            )
            .await
            .expect("session replacement must not cancel background Target close");
        let BrowserTargetTerminationProjection::Closed {
            closed,
            browser_fact,
        } = projection
        else {
            panic!("Target.closeTarget must project a closed Target");
        };
        assert!(browser_fact.is_some());

        assert_eq!(closed.target_id, "TID-background");
        assert_eq!(closed.primary_session_id.as_deref(), Some("SID-new"));
        let browser_context = conn.browser_context.as_ref().expect("browser context");
        assert!(browser_context.is_active_target("TID-active"));
        assert!(
            browser_context
                .background_target("TID-background")
                .is_none()
        );
    }
}
