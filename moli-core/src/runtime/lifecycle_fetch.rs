//! Core fetch bridge for synchronous renderer lifecycle-target decisions.
//!
//! The renderer owns the exact DCL/load boundary. This module owns the async
//! host-side budget transition: the original fetch deadline remains active
//! until the synchronous decider chooses `FollowNextDocument`, at which point a
//! separate successor-navigation budget replaces it.

use super::{
    Browser, FetchedDocument, PageVmInitStage, RenderedDomWaitUntil, RendererLifecycleDecider,
    RendererLifecycleDecision, RendererLifecycleSnapshot, RendererReplyBoundary,
};
use anyhow::{Context, Result, anyhow};
use moli_fetch::Request;
use std::{future::Future, time::Duration};
use tokio::sync::oneshot;
use tracing::warn;

pub(super) struct FollowTimeout {
    follow_rx: oneshot::Receiver<()>,
    successor_timeout: Duration,
}

impl Browser {
    /// Fetches an executable document with a synchronous one-shot policy at
    /// the exact requested lifecycle target.
    ///
    /// The decision runs in the renderer owner turn that observes DCL/load;
    /// it does not expose an intermediate Page or require a second owner
    /// command. `successor_timeout` reserves the independent budget used
    /// after a decision chooses to follow a successor Document.
    pub async fn fetch_document_with_lifecycle_decider<F>(
        &self,
        request: Request,
        wait_until: RenderedDomWaitUntil,
        timeout: Duration,
        successor_timeout: Duration,
        decider: F,
    ) -> Result<FetchedDocument>
    where
        F: FnOnce(RendererLifecycleSnapshot) -> Result<RendererLifecycleDecision> + Send + 'static,
    {
        anyhow::ensure!(
            matches!(
                wait_until,
                RenderedDomWaitUntil::DomContentLoaded
                    | RenderedDomWaitUntil::Load
                    | RenderedDomWaitUntil::Done
            ),
            "a lifecycle decider requires DCL, load, or done"
        );
        let (follow_tx, follow_rx) = oneshot::channel();
        let decider = RendererLifecycleDecider::new(move |target| {
            let decision = decider(target)?;
            if matches!(
                decision,
                RendererLifecycleDecision::FollowNextDocument { .. }
            ) {
                let _ = follow_tx.send(());
            }
            Ok(decision)
        });
        self.fetch_document_with_wait(
            request,
            wait_until,
            timeout,
            RendererReplyBoundary::Stage,
            Some(decider),
            Some(FollowTimeout {
                follow_rx,
                successor_timeout,
            }),
        )
        .await
        .with_context(|| {
            anyhow!(
                "failed while applying the {wait_until:?} lifecycle-target decision or following its successor navigation"
            )
        })
    }

    pub(super) async fn materialize_with_follow_timeout<T, F>(
        &self,
        raw_url: &str,
        wait_until: RenderedDomWaitUntil,
        timeout: Duration,
        stage: PageVmInitStage,
        initial_timeout: Duration,
        extension: Option<FollowTimeout>,
        future: F,
    ) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        let Some(mut extension) = extension else {
            return self
                .fetch_document_wait_timeout(
                    raw_url,
                    wait_until,
                    timeout,
                    stage,
                    initial_timeout,
                    future,
                )
                .await;
        };

        let initial_deadline = tokio::time::Instant::now()
            .checked_add(initial_timeout)
            .unwrap_or_else(tokio::time::Instant::now);
        let initial_deadline_sleep = tokio::time::sleep_until(initial_deadline);
        tokio::pin!(initial_deadline_sleep);
        tokio::pin!(future);

        let follow = tokio::select! {
            biased;
            result = &mut future => return result,
            selected = &mut extension.follow_rx => selected,
            _ = &mut initial_deadline_sleep => {
                return Err(self.fetch_document_wait_timeout_error(
                    raw_url, wait_until, timeout, stage,
                ));
            }
        };

        if follow.is_err() {
            // Finish drops the extension sender in the same callback turn.
            // Keep the original deadline while the Page creation reply is
            // finalized; only Follow is allowed to replace this budget.
            return match tokio::time::timeout_at(initial_deadline, &mut future).await {
                Ok(result) => result,
                Err(_) => {
                    Err(self.fetch_document_wait_timeout_error(raw_url, wait_until, timeout, stage))
                }
            };
        }

        match tokio::time::timeout(extension.successor_timeout, &mut future).await {
            Ok(result) => result,
            Err(_) => {
                warn!(
                    url = %raw_url,
                    wait_until = ?wait_until,
                    timeout_ms = extension.successor_timeout.as_millis(),
                    stage = ?stage,
                    "successor navigation lifecycle target timed out"
                );
                Err(anyhow!(
                    "successor navigation to {stage:?} timed out after {} ms for `{raw_url}`",
                    extension.successor_timeout.as_millis()
                ))
            }
        }
    }
}
