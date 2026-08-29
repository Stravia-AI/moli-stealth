use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    num::NonZeroUsize,
    time::{Duration, Instant},
};

use curl::{
    easy::Handler,
    multi::{Easy2Handle, Multi},
};
use tracing::debug;

use crate::dns_adapter::CurlDnsOwnerResidence;

use super::{CurlMultiJob, CurlOriginKey, CurlTransferId};

pub(super) struct CurlOwnerState<H: Handler, C> {
    pub(super) closed: bool,
    pub(super) pending: VecDeque<CurlPendingJob<H, C>>,
    pub(super) dns: CurlDnsOwnerResidence<CurlTransferId, CurlPendingJob<H, C>>,
    pub(super) active: HashMap<CurlTransferId, CurlActiveTransfer<H, C>>,
}

impl<H: Handler, C> Default for CurlOwnerState<H, C> {
    fn default() -> Self {
        Self {
            closed: false,
            pending: VecDeque::new(),
            dns: CurlDnsOwnerResidence::default(),
            active: HashMap::new(),
        }
    }
}

pub(super) struct CurlActiveTransfer<H: Handler, C> {
    pub(super) handle: Easy2Handle<H>,
    pub(super) context: C,
    pub(super) origin: Option<CurlOriginKey>,
    pub(super) priority: u8,
    pub(super) label: String,
    pub(super) started_at: Instant,
    pub(super) queued_for: Duration,
}

pub(super) struct CurlPendingJob<H: Handler, C> {
    pub(super) transfer_id: CurlTransferId,
    pub(super) job: CurlMultiJob<H, C>,
    pub(super) enqueued_at: Instant,
}

pub(super) fn enqueue_pending_job<H: Handler, C>(
    pending: &mut VecDeque<CurlPendingJob<H, C>>,
    transfer_id: CurlTransferId,
    job: CurlMultiJob<H, C>,
) {
    enqueue_existing_pending_job(
        pending,
        CurlPendingJob {
            transfer_id,
            job,
            enqueued_at: Instant::now(),
        },
    );
}

pub(super) fn enqueue_existing_pending_job<H: Handler, C>(
    pending: &mut VecDeque<CurlPendingJob<H, C>>,
    pending_job: CurlPendingJob<H, C>,
) {
    if let Some(index) = pending
        .iter()
        .position(|queued| pending_job.job.priority > queued.job.priority)
    {
        pending.insert(index, pending_job);
    } else {
        pending.push_back(pending_job);
    }
}

pub(super) fn job_is_eligible<H: Handler, C>(
    origin: Option<&CurlOriginKey>,
    state: &CurlOwnerState<H, C>,
    max_active_per_host: Option<NonZeroUsize>,
) -> bool {
    match (origin, max_active_per_host) {
        (Some(origin), Some(limit)) => active_origin_count(&state.active, origin) < limit.get(),
        _ => true,
    }
}

pub(super) fn active_origin_count<H: Handler, C>(
    active: &HashMap<CurlTransferId, CurlActiveTransfer<H, C>>,
    origin: &CurlOriginKey,
) -> usize {
    active
        .values()
        .filter(|active| active.origin.as_ref() == Some(origin))
        .count()
}

pub(super) fn pending_origin_count<H: Handler, C>(
    pending: &VecDeque<CurlPendingJob<H, C>>,
    origin: &CurlOriginKey,
) -> usize {
    pending
        .iter()
        .filter(|pending| pending.job.origin.as_ref() == Some(origin))
        .count()
}

pub(super) fn completed_transfers<H: Handler, C>(
    multi: &Multi,
    active: &HashMap<CurlTransferId, CurlActiveTransfer<H, C>>,
) -> Vec<(CurlTransferId, std::result::Result<(), curl::Error>)> {
    let mut completed = Vec::new();
    multi.messages(|message| {
        let Ok(token) = message.token() else {
            debug!("ignored curl completion whose private token could not be read");
            return;
        };
        let Some(transfer_id) = CurlTransferId::from_token(token) else {
            debug!("ignored curl completion with an empty private token");
            return;
        };
        let Some(transfer) = active.get(&transfer_id) else {
            debug!(%transfer_id, "ignored stale curl completion");
            return;
        };
        if let Some(result) = message.result_for2(&transfer.handle) {
            completed.push((transfer_id, result));
        }
    });
    completed
}

/// Removes exact active transfers while preserving libcurl's notification
/// order. Unknown IDs are stale terminals for already-retired residences and
/// cannot recover or disturb a newer transfer.
pub(super) fn take_transfers_in_notification_order<K, T, E>(
    active: &mut HashMap<K, T>,
    completed: Vec<(K, E)>,
) -> Vec<(K, T, E)>
where
    K: Copy + Eq + Hash,
{
    completed
        .into_iter()
        .filter_map(|(transfer_id, result)| {
            active
                .remove(&transfer_id)
                .map(|transfer| (transfer_id, transfer, result))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use curl::{easy::Easy2, multi::Multi};

    use crate::dns_adapter::CurlDnsResolution;

    use super::*;

    #[derive(Debug)]
    struct TestHandler;

    impl Handler for TestHandler {}

    fn test_job(
        label: &str,
        priority: u8,
        origin: Option<CurlOriginKey>,
    ) -> CurlMultiJob<TestHandler, String> {
        CurlMultiJob {
            easy: Easy2::new(TestHandler),
            context: label.to_owned(),
            origin,
            dns_resolution: CurlDnsResolution::curl_managed(),
            priority,
            label: label.to_owned(),
        }
    }

    fn test_origin(host: &str) -> CurlOriginKey {
        CurlOriginKey {
            scheme: "https".to_owned(),
            host: host.to_owned(),
            port: Some(443),
        }
    }

    fn test_transfer_id(sequence: usize) -> CurlTransferId {
        CurlTransferId::from_token(sequence).expect("test transfer ID is non-zero")
    }

    #[test]
    fn pending_jobs_are_ordered_by_priority() {
        let mut pending = VecDeque::new();

        enqueue_pending_job(
            &mut pending,
            test_transfer_id(1),
            test_job("auto-a", 1, None),
        );
        enqueue_pending_job(&mut pending, test_transfer_id(2), test_job("low", 0, None));
        enqueue_pending_job(
            &mut pending,
            test_transfer_id(3),
            test_job("high-a", 2, None),
        );
        enqueue_pending_job(
            &mut pending,
            test_transfer_id(4),
            test_job("auto-b", 1, None),
        );
        enqueue_pending_job(
            &mut pending,
            test_transfer_id(5),
            test_job("high-b", 2, None),
        );

        let ordered = pending
            .iter()
            .map(|job| (job.transfer_id.token(), job.job.label.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            [
                (3, "high-a"),
                (5, "high-b"),
                (1, "auto-a"),
                (4, "auto-b"),
                (2, "low"),
            ]
        );
    }

    #[test]
    fn completed_jobs_preserve_libcurl_notification_order() {
        let mut active = HashMap::from([
            (test_transfer_id(1), "first"),
            (test_transfer_id(2), "second"),
            (test_transfer_id(3), "third"),
            (test_transfer_id(4), "fourth"),
        ]);

        // libcurl reported the newest transfer first, followed by the oldest.
        // Hash-map storage must not leak its iteration order to completions.
        let completed = take_transfers_in_notification_order(
            &mut active,
            vec![
                (test_transfer_id(4), "fourth-result"),
                (test_transfer_id(1), "first-result"),
            ],
        );

        assert_eq!(
            completed,
            vec![
                (test_transfer_id(4), "fourth", "fourth-result"),
                (test_transfer_id(1), "first", "first-result"),
            ]
        );
        assert_eq!(active.len(), 2);
        assert_eq!(active[&test_transfer_id(2)], "second");
        assert_eq!(active[&test_transfer_id(3)], "third");
    }

    #[test]
    fn stale_completion_cannot_remove_a_live_transfer() {
        let mut active = HashMap::from([(test_transfer_id(2), "live")]);

        let completed = take_transfers_in_notification_order(
            &mut active,
            vec![(test_transfer_id(1), "stale-result")],
        );

        assert!(completed.is_empty());
        assert_eq!(active[&test_transfer_id(2)], "live");
    }

    #[test]
    fn per_origin_cap_blocks_only_matching_origin() {
        let capped_origin = test_origin("example.test");
        let other_origin = test_origin("other.test");
        let multi = Multi::new();
        let active = CurlActiveTransfer {
            handle: multi
                .add2(Easy2::new(TestHandler))
                .expect("test handle should add to multi"),
            context: "active".to_owned(),
            origin: Some(capped_origin.clone()),
            priority: 1,
            label: "active".to_owned(),
            started_at: Instant::now(),
            queued_for: Duration::ZERO,
        };
        let state = CurlOwnerState {
            closed: false,
            pending: VecDeque::new(),
            dns: CurlDnsOwnerResidence::default(),
            active: HashMap::from([(test_transfer_id(1), active)]),
        };
        let cap = NonZeroUsize::new(1);

        assert!(!job_is_eligible(Some(&capped_origin), &state, cap));
        assert!(job_is_eligible(Some(&other_origin), &state, cap));
    }
}
