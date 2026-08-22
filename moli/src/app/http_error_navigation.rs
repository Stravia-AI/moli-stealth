//! CLI policy for replacing an HTTP error Document with its next navigation.
//!
//! HTTP status interpretation and timeout configuration belong to the CLI
//! layer. The renderer receives only the generic lifecycle-target decision.

use anyhow::{Context, Result};
use moli_core::runtime::{
    Browser, FetchDeadline, FetchedDocument, RenderedDomWaitUntil, RendererLifecycleDecision,
};
use moli_fetch::Request;
use std::time::{Duration, Instant};

pub(super) fn is_http_error_status(status: u16) -> bool {
    (400..=599).contains(&status)
}

pub(super) async fn fetch_with_http_error_navigation(
    browser: &Browser,
    request: Request,
    wait_until: RenderedDomWaitUntil,
    deadline: FetchDeadline,
    minimum_navigation_wait: Duration,
) -> Result<FetchedDocument> {
    let minimum_navigation_deadline = Instant::now()
        .checked_add(minimum_navigation_wait)
        .context("HTTP error replacement-navigation wait exceeds the supported range")?;
    // The first DCL/load is delivered to this synchronous decision normally.
    // A 4xx/5xx keeps running until the configured minimum time from fetch
    // start has elapsed, giving client-side challenges a chance to replace the
    // error Document. The initial lifecycle load therefore consumes this
    // window instead of receiving a fresh grace period afterward. The outer
    // readiness deadline still caps this wait and any successor lifecycle.
    browser
        .fetch_document_with_lifecycle_decider_and_deadline(
            request,
            wait_until,
            deadline,
            move |target| {
                Ok(if is_http_error_status(target.status) {
                    RendererLifecycleDecision::FollowNextDocumentOrFinish {
                        navigation_grace_ms: remaining_wait_milliseconds(
                            minimum_navigation_deadline,
                            Instant::now(),
                        ),
                    }
                } else {
                    RendererLifecycleDecision::Finish
                })
            },
        )
        .await
}

fn remaining_wait_milliseconds(deadline: Instant, now: Instant) -> u64 {
    deadline
        .saturating_duration_since(now)
        .as_nanos()
        .div_ceil(1_000_000)
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::{is_http_error_status, remaining_wait_milliseconds};
    use std::time::{Duration, Instant};

    #[test]
    fn http_error_status_covers_only_four_hundred_and_five_hundred_ranges() {
        assert!(!is_http_error_status(399));
        assert!(is_http_error_status(400));
        assert!(is_http_error_status(499));
        assert!(is_http_error_status(500));
        assert!(is_http_error_status(599));
        assert!(!is_http_error_status(600));
    }

    #[test]
    fn replacement_wait_is_the_unconsumed_minimum_from_fetch_start() {
        let started = Instant::now();
        let deadline = started + Duration::from_millis(1_000);

        assert_eq!(remaining_wait_milliseconds(deadline, started), 1_000);
        assert_eq!(
            remaining_wait_milliseconds(deadline, started + Duration::from_millis(275)),
            725
        );
        assert_eq!(
            remaining_wait_milliseconds(deadline, started + Duration::from_millis(1_000)),
            0
        );
        assert_eq!(
            remaining_wait_milliseconds(deadline, started + Duration::from_millis(1_500)),
            0
        );
    }

    #[test]
    fn replacement_wait_rounds_up_a_partial_millisecond() {
        let now = Instant::now();
        let deadline = now + Duration::from_nanos(1_000_001);

        assert_eq!(remaining_wait_milliseconds(deadline, now), 2);
    }
}
