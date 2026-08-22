use super::{
    CdpConnection, CdpTargetHostLifecycleDelta, PreparedTargetHostDelta, TargetEventPlan,
    browser_fact_projection::BrowserTargetMetadataFactProjection,
};

impl CdpConnection {
    /// Projects one already-claimed Browser metadata occurrence into the
    /// current frontend's Target graph.
    ///
    /// The fact freezes Browser URL/title values and exact Page identity.
    /// Attached state, discovery/reporting state and the tab facade remain
    /// frontend-local and are joined only at this boundary.
    pub(crate) fn project_target_metadata_changed_fact(
        &mut self,
        projection: BrowserTargetMetadataFactProjection,
    ) -> TargetEventPlan {
        let envelope = projection.envelope();
        let page = envelope.page_residence();
        let target_id = envelope.target_id().as_str();
        let snapshot = match self.capture_browser_top_level_target_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::error!(
                    ?error,
                    browser_context_id = page.browser_context_id(),
                    target_id,
                    "cannot project Target metadata fact from divergent Browser topology"
                );
                return TargetEventPlan::default();
            }
        };
        let Some(target_snapshot) = snapshot
            .context(page.browser_context_id())
            .and_then(|context| context.target(target_id))
            .cloned()
        else {
            tracing::debug!(
                browser_context_id = page.browser_context_id(),
                target_id,
                "dropping Target metadata fact after its Target retired"
            );
            return TargetEventPlan::default();
        };
        if target_snapshot.page_residence() != page {
            tracing::debug!(
                browser_context_id = page.browser_context_id(),
                target_id,
                fact_page_generation = page.loaded_page_generation(),
                current_page_generation = target_snapshot.page_residence().loaded_page_generation(),
                "dropping Target metadata fact after its exact Page was replaced"
            );
            return TargetEventPlan::default();
        }
        let mut target_info = match self.project_top_level_target_snapshot(&target_snapshot) {
            Ok(target_info) => target_info,
            Err(error) => {
                tracing::error!(
                    ?error,
                    browser_context_id = page.browser_context_id(),
                    target_id,
                    "cannot join Target metadata fact with its physical projection"
                );
                return TargetEventPlan::default();
            }
        };
        target_info.url = projection.transition().url().to_owned();
        target_info.title = projection.transition().title().to_owned();

        self.notify_target_host_lifecycle(CdpTargetHostLifecycleDelta::InfoChanged(
            target_info.clone(),
        ));
        if !self.has_any_target_info_observer() {
            return TargetEventPlan::default();
        }
        let prepared = self
            .project_tab_page_target_infos(target_info)
            .into_iter()
            .filter_map(|target_info| {
                let target_id = target_info.target_id.as_ref()?.as_str().to_owned();
                Some(PreparedTargetHostDelta::info_changed(
                    target_id,
                    Some(target_info),
                ))
            });
        self.prepared_target_host_deltas_event_plan(prepared)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use moli_core::browser_host::BrowserNavigationHistoryPageSnapshot;
    use parking_lot::Mutex;

    use super::*;
    use crate::conn::{CdpTargetHostLifecycleDelta, CdpTargetHostLifecycleObserver};

    fn committed_metadata_projection(
        conn: &mut CdpConnection,
        url: &str,
        title: &str,
    ) -> BrowserTargetMetadataFactProjection {
        conn.install_default_browser_target();
        let owner = conn
            .target_page_owner_key_for_session(None)
            .expect("default Target owner");
        let navigation = conn
            .start_document_navigation_for_session_owner(None, "LID-metadata".to_owned())
            .expect("default Target should accept navigation");
        let permit = conn
            .browser_host_state
            .navigation_owner()
            .prepare_loaded_page_replacement(&owner, &navigation)
            .expect("current navigation should prepare replacement");
        let replacement = conn
            .browser_host_state
            .navigation_owner_mut()
            .commit_loaded_page_replacement_without_renderer_owner_for_testing(
                permit,
                BrowserNavigationHistoryPageSnapshot::new(url, title),
            )
            .expect("current navigation should commit replacement");
        conn.take_navigation_commit_facts(&navigation, replacement.current_page())
            .expect("frontend should claim the exact commit fact pair");
        conn.take_navigation_target_metadata_changed_fact(&navigation, replacement.current_page())
            .expect("frontend should claim the exact committed metadata fact")
    }

    #[test]
    fn metadata_fact_freezes_browser_values_before_frontend_projection() {
        let mut conn = CdpConnection::default();
        let expected_url = "https://example.test/frozen";
        let expected_title = "Frozen title";
        let projection = committed_metadata_projection(&mut conn, expected_url, expected_title);
        conn.browser_context
            .as_mut()
            .expect("default BrowserContext")
            .set_target_url("https://example.test/drift".to_owned());

        let observed_metadata = Arc::new(Mutex::new(Vec::new()));
        let callback_metadata = Arc::clone(&observed_metadata);
        conn.set_target_host_lifecycle_observer(CdpTargetHostLifecycleObserver::new(
            move |delta| {
                if let CdpTargetHostLifecycleDelta::InfoChanged(target_info) = delta {
                    callback_metadata
                        .lock()
                        .push((target_info.url, target_info.title));
                }
            },
        ));
        let plan = conn.project_target_metadata_changed_fact(projection);

        assert!(plan.into_iter().next().is_none());
        assert_eq!(
            *observed_metadata.lock(),
            vec![(expected_url.to_owned(), expected_title.to_owned())],
            "frontend projection must use immutable Browser fact values, not later physical drift",
        );
    }

    #[test]
    fn metadata_fact_stale_drops_after_exact_document_replacement() {
        let mut conn = CdpConnection::default();
        let projection =
            committed_metadata_projection(&mut conn, "https://example.test/stale", "Stale title");
        conn.target_page_residence_handle_for_session(None)
            .expect("default Page residence handle")
            .advance_generation_for_test_fixture();

        let observed = Arc::new(Mutex::new(0_u32));
        let callback_count = Arc::clone(&observed);
        conn.set_target_host_lifecycle_observer(CdpTargetHostLifecycleObserver::new(move |_| {
            *callback_count.lock() += 1;
        }));
        let plan = conn.project_target_metadata_changed_fact(projection);

        assert!(plan.into_iter().next().is_none());
        assert_eq!(*observed.lock(), 0);
    }

    #[test]
    fn frontend_attachment_delta_does_not_publish_browser_metadata_fact() {
        let mut conn = CdpConnection::default();
        conn.install_default_browser_target();
        let before = conn.browser_fact_snapshot_for_test();

        let target_id = conn.default_target_id().to_owned();
        let _ = conn.frontend_attachment_info_changed_event_plan(&target_id);

        assert_eq!(conn.browser_fact_snapshot_for_test(), before);
        assert!(before.iter().all(|envelope| !matches!(
            envelope.fact(),
            moli_core::browser_host::BrowserFact::TargetMetadataChanged { .. }
        )));
    }
}
