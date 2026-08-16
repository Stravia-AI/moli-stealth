//! CLI policy for replacing an HTTP error Document with its next navigation.
//!
//! HTTP status interpretation and timeout configuration belong to the CLI
//! layer. The renderer receives only the generic lifecycle-target decision.

use anyhow::Result;
use moli_core::runtime::{
    Browser, FetchedDocument, RenderedDomWaitUntil, RendererLifecycleDecision,
};
use moli_fetch::Request;
use std::time::Duration;

pub(super) fn is_http_error_status(status: u16) -> bool {
    (400..=599).contains(&status)
}

pub(super) async fn fetch_with_http_error_navigation(
    browser: &Browser,
    request: Request,
    wait_until: RenderedDomWaitUntil,
    stage_timeout: Duration,
    navigation_grace: Duration,
) -> Result<FetchedDocument> {
    let navigation_grace_ms = navigation_grace.as_millis().min(u128::from(u64::MAX)) as u64;
    // The first DCL/load is delivered to this synchronous decision normally.
    // A 4xx/5xx gets one grace window to start a replacement navigation; the
    // successor then has its own complete budget to reach the same stage.
    browser
        .fetch_document_with_lifecycle_decider(
            request,
            wait_until,
            stage_timeout,
            navigation_grace.saturating_add(stage_timeout),
            move |target| {
                Ok(if is_http_error_status(target.status) {
                    RendererLifecycleDecision::FollowNextDocument {
                        navigation_grace_ms,
                    }
                } else {
                    RendererLifecycleDecision::Finish
                })
            },
        )
        .await
}
