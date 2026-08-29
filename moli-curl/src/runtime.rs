use std::{
    collections::{HashMap, VecDeque},
    fmt,
    num::NonZeroUsize,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{self, Receiver, Sender};
use curl::{
    easy::{Easy2, Handler},
    multi::{Easy2Handle, Multi, MultiWaker},
};
use parking_lot::Mutex;
use tracing::debug;

use crate::dns_adapter::{
    CurlDnsOwnerCompletion, CurlDnsOwnerResidence, CurlDnsReady, CurlDnsResolution,
};

const DEFAULT_RUNTIME_POLL_INTERVAL: Duration = Duration::from_millis(50);
static NEXT_CURL_TRANSFER_ID: AtomicUsize = AtomicUsize::new(1);

/// Opaque process-wide identity of one curl transfer.
///
/// The non-zero value is installed as libcurl's private token once the
/// transfer becomes active, so the same identity follows the request through
/// every residence and back in its terminal completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CurlTransferId(NonZeroUsize);

impl CurlTransferId {
    fn new(value: NonZeroUsize) -> Self {
        Self(value)
    }

    fn from_token(token: usize) -> Option<Self> {
        Some(Self::new(NonZeroUsize::new(token)?))
    }

    fn token(self) -> usize {
        self.0.get()
    }
}

impl fmt::Display for CurlTransferId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Origin key used by the curl scheduler for per-origin active transfer caps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CurlOriginKey {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
}

/// Configuration for a multi-request curl runtime.
#[derive(Debug, Clone)]
pub struct CurlMultiRuntimeConfig {
    pub max_active: NonZeroUsize,
    /// Scheduler-side per-origin active transfer cap.
    ///
    /// Keep this separate from `max_host_connections`: the latter is a curl
    /// transport connection-pool cap and should not throttle HTTP/2 streams.
    pub max_host_active: Option<NonZeroUsize>,
    /// libcurl per-host connection cap, matching Chromium's HTTP/1 socket-pool
    /// concept when configured by the higher fetch runtime.
    pub max_host_connections: Option<NonZeroUsize>,
    pub max_total_connections: Option<NonZeroUsize>,
    pub max_concurrent_streams: Option<NonZeroUsize>,
    pub poll_interval: Duration,
    pub multiplex: bool,
    pub thread_name: String,
}

impl Default for CurlMultiRuntimeConfig {
    fn default() -> Self {
        Self {
            max_active: NonZeroUsize::new(8).expect("default active transfer cap is non-zero"),
            max_host_active: None,
            max_host_connections: None,
            max_total_connections: None,
            max_concurrent_streams: None,
            poll_interval: DEFAULT_RUNTIME_POLL_INTERVAL,
            multiplex: true,
            thread_name: "lm-curl-multi".to_owned(),
        }
    }
}

impl CurlMultiRuntimeConfig {
    pub fn validate(&self) -> Result<()> {
        if self.poll_interval.is_zero() {
            return Err(anyhow!("curl multi runtime poll interval must be non-zero"));
        }
        if self.thread_name.is_empty() {
            return Err(anyhow!("curl multi runtime thread name must not be empty"));
        }
        Ok(())
    }
}

/// A configured curl transfer plus scheduler metadata.
pub struct CurlMultiJob<H: Handler, C> {
    pub easy: Easy2<H>,
    pub context: C,
    pub origin: Option<CurlOriginKey>,
    /// DNS ownership chosen by the caller before this transfer enters curl.
    ///
    /// A curl-managed policy preserves libcurl's resolver behavior. A shared
    /// origin policy parks the transfer outside the curl multi handle set until
    /// the bounded system resolver publishes an answer.
    pub dns_resolution: CurlDnsResolution,
    /// Higher values start before lower values when jobs are queued.
    pub priority: u8,
    pub label: String,
}

impl<H: Handler, C> fmt::Debug for CurlMultiJob<H, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CurlMultiJob")
            .field("origin", &self.origin)
            .field("dns_resolution", &self.dns_resolution)
            .field("priority", &self.priority)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// Completion emitted by `CurlMultiRuntime`.
pub struct CurlMultiCompletion<H: Handler, C> {
    pub transfer_id: CurlTransferId,
    pub easy: Option<Easy2<H>>,
    pub context: C,
    pub result: Result<()>,
}

impl<H: Handler, C> fmt::Debug for CurlMultiCompletion<H, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CurlMultiCompletion")
            .field("transfer_id", &self.transfer_id)
            .field("has_easy", &self.easy.is_some())
            .field("result", &self.result.as_ref().map(|_| ()))
            .finish_non_exhaustive()
    }
}

/// Error returned when a job cannot be submitted and is returned to the caller.
pub struct CurlSubmitError<H: Handler, C> {
    pub job: CurlMultiJob<H, C>,
    pub error: anyhow::Error,
}

impl<H: Handler, C> fmt::Debug for CurlSubmitError<H, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CurlSubmitError")
            .field("job", &self.job)
            .field("error", &self.error)
            .finish()
    }
}

/// Cloneable handle for a single libcurl multi owner thread.
#[derive(Debug)]
pub struct CurlMultiRuntime<H: Handler + Send + 'static, C: Send + 'static> {
    inner: Arc<CurlMultiRuntimeInner<H, C>>,
}

impl<H: Handler + Send + 'static, C: Send + 'static> Clone for CurlMultiRuntime<H, C> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[derive(Debug)]
struct CurlMultiRuntimeInner<H: Handler + Send + 'static, C: Send + 'static> {
    command_tx: Sender<CurlRuntimeCommand<H, C>>,
    owner_waker: MultiWaker,
    shutdown_requested: Arc<AtomicBool>,
    #[cfg(test)]
    owner_started: Arc<AtomicBool>,
    owner_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

#[derive(Debug)]
enum CurlRuntimeCommand<H: Handler, C> {
    Request {
        transfer_id: CurlTransferId,
        job: CurlMultiJob<H, C>,
    },
    Shutdown,
}

enum CurlOwnerEvent<H: Handler, C> {
    Command(std::result::Result<CurlRuntimeCommand<H, C>, crossbeam_channel::RecvError>),
    Dns(std::result::Result<CurlDnsOwnerCompletion<CurlTransferId>, crossbeam_channel::RecvError>),
}

impl<H: Handler + Send + 'static, C: Send + 'static> CurlMultiRuntime<H, C> {
    pub fn new(
        config: CurlMultiRuntimeConfig,
    ) -> Result<(Self, Receiver<CurlMultiCompletion<H, C>>)> {
        config.validate()?;
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (completion_tx, completion_rx) = crossbeam_channel::unbounded();
        let (waker_tx, waker_rx) = crossbeam_channel::bounded(1);
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let owner_started = Arc::new(AtomicBool::new(false));
        let owner = CurlRuntimeOwner {
            config,
            command_rx,
            completion_tx,
            waker_tx,
            shutdown_requested: Arc::clone(&shutdown_requested),
            #[cfg(test)]
            owner_started: Arc::clone(&owner_started),
        };
        let thread_name = owner.config.thread_name.clone();
        let owner_handle = thread::Builder::new()
            .name(thread_name)
            .spawn(move || owner.run())
            .context("failed to spawn curl multi runtime owner thread")?;
        let owner_waker = waker_rx
            .recv()
            .context("curl multi runtime owner did not publish a waker")?;
        let runtime = Self {
            inner: Arc::new(CurlMultiRuntimeInner {
                command_tx,
                owner_waker,
                shutdown_requested,
                #[cfg(test)]
                owner_started,
                owner_handle: Mutex::new(Some(owner_handle)),
            }),
        };
        Ok((runtime, completion_rx))
    }

    pub fn submit(
        &self,
        job: CurlMultiJob<H, C>,
    ) -> std::result::Result<CurlTransferId, CurlSubmitError<H, C>> {
        if self.inner.shutdown_requested.load(Ordering::SeqCst) {
            return Err(CurlSubmitError {
                job,
                error: anyhow!("curl multi runtime is shutting down"),
            });
        }
        let transfer_id = match next_transfer_id() {
            Ok(transfer_id) => transfer_id,
            Err(error) => return Err(CurlSubmitError { job, error }),
        };
        match self
            .inner
            .command_tx
            .send(CurlRuntimeCommand::Request { transfer_id, job })
        {
            Ok(()) => {
                let _ = self.inner.owner_waker.wakeup();
                Ok(transfer_id)
            }
            Err(error) => {
                let CurlRuntimeCommand::Request { job, .. } = error.into_inner() else {
                    unreachable!("submit only sends request commands");
                };
                Err(CurlSubmitError {
                    job,
                    error: anyhow!("curl multi runtime is shutting down"),
                })
            }
        }
    }

    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    #[cfg(test)]
    pub fn owner_count_for_testing(&self) -> usize {
        usize::from(self.inner.owner_started.load(Ordering::SeqCst))
    }
}

impl<H: Handler + Send + 'static, C: Send + 'static> Drop for CurlMultiRuntimeInner<H, C> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl<H: Handler + Send + 'static, C: Send + 'static> CurlMultiRuntimeInner<H, C> {
    fn shutdown(&self) {
        if self.shutdown_requested.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.command_tx.send(CurlRuntimeCommand::Shutdown);
        let _ = self.owner_waker.wakeup();
        let Some(owner_handle) = self.owner_handle.lock().take() else {
            return;
        };
        let _ = owner_handle.join();
    }
}

struct CurlRuntimeOwner<H: Handler + Send + 'static, C: Send + 'static> {
    config: CurlMultiRuntimeConfig,
    command_rx: Receiver<CurlRuntimeCommand<H, C>>,
    completion_tx: Sender<CurlMultiCompletion<H, C>>,
    waker_tx: Sender<MultiWaker>,
    shutdown_requested: Arc<AtomicBool>,
    #[cfg(test)]
    owner_started: Arc<AtomicBool>,
}

impl<H: Handler + Send + 'static, C: Send + 'static> CurlRuntimeOwner<H, C> {
    fn run(self) {
        #[cfg(test)]
        self.owner_started.store(true, Ordering::SeqCst);
        let mut multi = make_runtime_multi(&self.config);
        let _ = self.waker_tx.send(multi.waker());
        let mut state = CurlOwnerState::default();

        loop {
            self.drain_commands(&mut state, &mut multi);
            self.drain_dns_completions(&mut state);
            self.start_eligible_jobs(&mut state, &mut multi);
            self.process_completed_transfers(&mut state, &mut multi);

            if state.closed
                && state.pending.is_empty()
                && state.dns.is_empty()
                && state.active.is_empty()
            {
                return;
            }

            if state.active.is_empty() && state.pending.is_empty() {
                self.wait_for_next_owner_event(&mut state, &mut multi);
            } else if !state.active.is_empty() {
                self.wait_for_curl_progress(&multi);
            }
        }
    }

    fn drain_commands(&self, state: &mut CurlOwnerState<H, C>, multi: &mut Multi) {
        loop {
            match self.command_rx.try_recv() {
                Ok(command) => self.handle_command(state, multi, command),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.close(state, multi);
                    break;
                }
            }
        }
    }

    fn wait_for_next_owner_event(&self, state: &mut CurlOwnerState<H, C>, multi: &mut Multi) {
        if state.closed {
            return;
        }
        if state.dns.is_empty() {
            match self.command_rx.recv() {
                Ok(command) => self.handle_command(state, multi, command),
                Err(_) => self.close(state, multi),
            }
            return;
        }
        let event = crossbeam_channel::select! {
            recv(self.command_rx) -> command => CurlOwnerEvent::Command(command),
            recv(state.dns.completion_receiver()) -> completion => CurlOwnerEvent::Dns(completion),
        };
        match event {
            CurlOwnerEvent::Command(command) => match command {
                Ok(command) => self.handle_command(state, multi, command),
                Err(_) => self.close(state, multi),
            },
            CurlOwnerEvent::Dns(completion) => {
                if let Ok(completion) = completion {
                    self.claim_dns_completion(state, completion);
                }
            }
        }
    }

    fn handle_command(
        &self,
        state: &mut CurlOwnerState<H, C>,
        multi: &mut Multi,
        command: CurlRuntimeCommand<H, C>,
    ) {
        match command {
            CurlRuntimeCommand::Request { transfer_id, job } if state.closed => {
                self.send_completion(CurlMultiCompletion {
                    transfer_id,
                    easy: Some(job.easy),
                    context: job.context,
                    result: Err(anyhow!("curl multi runtime is shutting down")),
                });
            }
            CurlRuntimeCommand::Request { transfer_id, job } => {
                enqueue_pending_job(&mut state.pending, transfer_id, job)
            }
            CurlRuntimeCommand::Shutdown => self.close(state, multi),
        }
    }

    fn close(&self, state: &mut CurlOwnerState<H, C>, multi: &mut Multi) {
        if state.closed {
            return;
        }
        state.closed = true;
        self.shutdown_requested.store(true, Ordering::SeqCst);
        while let Some(pending) = state.pending.pop_front() {
            let CurlPendingJob {
                transfer_id, job, ..
            } = pending;
            self.send_completion(CurlMultiCompletion {
                transfer_id,
                easy: Some(job.easy),
                context: job.context,
                result: Err(anyhow!("curl multi runtime is shutting down")),
            });
        }
        for pending in state.dns.drain() {
            let CurlPendingJob {
                transfer_id, job, ..
            } = pending;
            self.send_completion(CurlMultiCompletion {
                transfer_id,
                easy: Some(job.easy),
                context: job.context,
                result: Err(anyhow!(
                    "curl multi runtime DNS request cancelled during shutdown"
                )),
            });
        }
        for (transfer_id, active) in state.active.drain() {
            let easy = multi.remove2(active.handle).ok();
            self.send_completion(CurlMultiCompletion {
                transfer_id,
                easy,
                context: active.context,
                result: Err(anyhow!(
                    "curl multi runtime request cancelled during shutdown"
                )),
            });
        }
    }

    fn start_eligible_jobs(&self, state: &mut CurlOwnerState<H, C>, multi: &mut Multi) {
        loop {
            if state.closed || state.active.len() >= self.config.max_active.get() {
                return;
            }
            let Some(index) = state.pending.iter().position(|pending| {
                job_is_eligible(
                    pending.job.origin.as_ref(),
                    state,
                    self.config.max_host_active,
                )
            }) else {
                return;
            };
            let pending = state
                .pending
                .remove(index)
                .expect("pending curl job index should exist");
            let dns_target = pending.job.dns_resolution.target().cloned();
            match dns_target {
                Some(target) => {
                    state
                        .dns
                        .start(pending.transfer_id, pending, target, multi.waker())
                }
                None => self.start_job(state, multi, pending),
            }
        }
    }

    fn drain_dns_completions(&self, state: &mut CurlOwnerState<H, C>) {
        while let Some(ready) = state.dns.try_claim_next() {
            self.handle_dns_completion(state, ready);
        }
    }

    fn claim_dns_completion(
        &self,
        state: &mut CurlOwnerState<H, C>,
        completion: CurlDnsOwnerCompletion<CurlTransferId>,
    ) {
        let Some(ready) = state.dns.claim(completion) else {
            return;
        };
        self.handle_dns_completion(state, ready);
    }

    fn handle_dns_completion(
        &self,
        state: &mut CurlOwnerState<H, C>,
        ready: CurlDnsReady<CurlPendingJob<H, C>>,
    ) {
        let mut pending = ready.pending;
        if state.closed {
            let CurlPendingJob {
                transfer_id, job, ..
            } = pending;
            self.send_completion(CurlMultiCompletion {
                transfer_id,
                easy: Some(job.easy),
                context: job.context,
                result: Err(anyhow!("curl multi runtime is shutting down")),
            });
            return;
        }
        match ready.result {
            Ok(addresses) => {
                if let Err(error) = pending
                    .job
                    .dns_resolution
                    .install(&mut pending.job.easy, addresses.as_ref())
                {
                    let CurlPendingJob {
                        transfer_id, job, ..
                    } = pending;
                    self.send_completion(CurlMultiCompletion {
                        transfer_id,
                        easy: Some(job.easy),
                        context: job.context,
                        result: Err(error),
                    });
                    return;
                }
                enqueue_existing_pending_job(&mut state.pending, pending);
            }
            Err(error) => {
                let CurlPendingJob {
                    transfer_id, job, ..
                } = pending;
                self.send_completion(CurlMultiCompletion {
                    transfer_id,
                    easy: Some(job.easy),
                    context: job.context,
                    result: Err(anyhow!(error.to_string())),
                });
            }
        }
    }

    fn start_job(
        &self,
        state: &mut CurlOwnerState<H, C>,
        multi: &mut Multi,
        pending: CurlPendingJob<H, C>,
    ) {
        let transfer_id = pending.transfer_id;
        let queued_for = pending.enqueued_at.elapsed();
        let job = pending.job;
        let label = job.label.clone();
        match multi
            .add2(job.easy)
            .with_context(|| anyhow!("failed to add curl easy handle for {label}"))
        {
            Ok(mut handle) => {
                if let Err(error) = handle.set_token(transfer_id.token()) {
                    let easy = multi.remove2(handle).ok();
                    self.send_completion(CurlMultiCompletion {
                        transfer_id,
                        easy,
                        context: job.context,
                        result: Err(anyhow!(
                            "failed to install token for curl transfer {transfer_id}: {error}"
                        )),
                    });
                    return;
                }
                if curl_runtime_trace_enabled() {
                    let origin = job.origin.as_ref();
                    tracing::info!(
                        target: "moli_cdp_nav_timing",
                        transfer_id = %transfer_id,
                        label = %label,
                        origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
                        origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
                        origin_port = ?origin.and_then(|origin| origin.port),
                        origin = ?job.origin,
                        priority = job.priority,
                        queued_ms = queued_for.as_millis(),
                        active_before = state.active.len(),
                        active_same_origin_before = origin
                            .map(|origin| active_origin_count(&state.active, origin))
                            .unwrap_or(0),
                        pending_after = state.pending.len(),
                        pending_same_origin_after = origin
                            .map(|origin| pending_origin_count(&state.pending, origin))
                            .unwrap_or(0),
                        max_active = self.config.max_active.get(),
                        max_host_active = ?self.config.max_host_active.map(NonZeroUsize::get),
                        max_host_connections = ?self.config.max_host_connections.map(NonZeroUsize::get),
                        max_total_connections = ?self.config.max_total_connections.map(NonZeroUsize::get),
                        max_concurrent_streams = ?self.config.max_concurrent_streams.map(NonZeroUsize::get),
                        multiplex = self.config.multiplex,
                        stage = "curl_runtime_job_start",
                    );
                }
                let previous = state.active.insert(
                    transfer_id,
                    CurlActiveTransfer {
                        handle,
                        context: job.context,
                        origin: job.origin,
                        priority: job.priority,
                        label,
                        started_at: Instant::now(),
                        queued_for,
                    },
                );
                assert!(previous.is_none(), "curl transfer identity is unique");
            }
            Err(error) => self.send_completion(CurlMultiCompletion {
                transfer_id,
                easy: None,
                context: job.context,
                result: Err(error),
            }),
        }
    }

    fn process_completed_transfers(&self, state: &mut CurlOwnerState<H, C>, multi: &mut Multi) {
        if let Err(error) = multi.perform() {
            debug!("curl multi runtime perform failed: {error}");
        }
        let completed = completed_transfers(multi, &state.active);
        for (transfer_id, active, result) in
            take_transfers_in_notification_order(&mut state.active, completed)
        {
            self.finish_active_transfer(
                state,
                multi,
                transfer_id,
                active,
                result.map_err(Into::into),
            );
        }
    }

    fn finish_active_transfer(
        &self,
        state: &CurlOwnerState<H, C>,
        multi: &mut Multi,
        transfer_id: CurlTransferId,
        active: CurlActiveTransfer<H, C>,
        result: Result<()>,
    ) {
        let easy = match multi.remove2(active.handle) {
            Ok(easy) => Some(easy),
            Err(error) => {
                self.send_completion(CurlMultiCompletion {
                    transfer_id,
                    easy: None,
                    context: active.context,
                    result: Err(anyhow!(
                        "failed to remove curl easy handle for {}: {error}",
                        active.label
                    )),
                });
                return;
            }
        };
        if curl_runtime_trace_enabled() {
            let origin = active.origin.as_ref();
            match &result {
                Ok(()) => {
                    tracing::info!(
                        target: "moli_cdp_nav_timing",
                        transfer_id = %transfer_id,
                        label = %active.label,
                        origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
                        origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
                        origin_port = ?origin.and_then(|origin| origin.port),
                        origin = ?active.origin,
                        priority = active.priority,
                        ok = true,
                        active_ms = active.started_at.elapsed().as_millis(),
                        queued_ms = active.queued_for.as_millis(),
                        active_remaining = state.active.len(),
                        active_same_origin_remaining = origin
                            .map(|origin| active_origin_count(&state.active, origin))
                            .unwrap_or(0),
                        pending_after = state.pending.len(),
                        pending_same_origin_after = origin
                            .map(|origin| pending_origin_count(&state.pending, origin))
                            .unwrap_or(0),
                        stage = "curl_runtime_job_done",
                    );
                }
                Err(error) => {
                    tracing::info!(
                        target: "moli_cdp_nav_timing",
                        transfer_id = %transfer_id,
                        label = %active.label,
                        origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
                        origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
                        origin_port = ?origin.and_then(|origin| origin.port),
                        origin = ?active.origin,
                        priority = active.priority,
                        ok = false,
                        error = %error,
                        active_ms = active.started_at.elapsed().as_millis(),
                        queued_ms = active.queued_for.as_millis(),
                        active_remaining = state.active.len(),
                        active_same_origin_remaining = origin
                            .map(|origin| active_origin_count(&state.active, origin))
                            .unwrap_or(0),
                        pending_after = state.pending.len(),
                        pending_same_origin_after = origin
                            .map(|origin| pending_origin_count(&state.pending, origin))
                            .unwrap_or(0),
                        stage = "curl_runtime_job_done",
                    );
                }
            }
        }
        let result = result.with_context(|| {
            anyhow!(
                "curl request failed for {} after active={}ms queued={}ms",
                active.label,
                active.started_at.elapsed().as_millis(),
                active.queued_for.as_millis()
            )
        });
        self.send_completion(CurlMultiCompletion {
            transfer_id,
            easy,
            context: active.context,
            result,
        });
    }

    fn wait_for_curl_progress(&self, multi: &Multi) {
        let wait_timeout = runtime_wait_timeout(multi, self.config.poll_interval)
            .unwrap_or(self.config.poll_interval);
        if wait_timeout.is_zero() {
            return;
        }
        if let Err(error) = multi.poll(&mut [], wait_timeout) {
            debug!("curl multi runtime poll failed: {error}");
        }
    }

    fn send_completion(&self, completion: CurlMultiCompletion<H, C>) {
        let _ = self.completion_tx.send(completion);
    }
}

struct CurlOwnerState<H: Handler, C> {
    closed: bool,
    pending: VecDeque<CurlPendingJob<H, C>>,
    dns: CurlDnsOwnerResidence<CurlTransferId, CurlPendingJob<H, C>>,
    active: HashMap<CurlTransferId, CurlActiveTransfer<H, C>>,
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

struct CurlActiveTransfer<H: Handler, C> {
    handle: Easy2Handle<H>,
    context: C,
    origin: Option<CurlOriginKey>,
    priority: u8,
    label: String,
    started_at: Instant,
    queued_for: Duration,
}

struct CurlPendingJob<H: Handler, C> {
    transfer_id: CurlTransferId,
    job: CurlMultiJob<H, C>,
    enqueued_at: Instant,
}

fn enqueue_pending_job<H: Handler, C>(
    pending: &mut VecDeque<CurlPendingJob<H, C>>,
    transfer_id: CurlTransferId,
    job: CurlMultiJob<H, C>,
) {
    if curl_runtime_trace_enabled() {
        let origin = job.origin.as_ref();
        tracing::info!(
            target: "moli_cdp_nav_timing",
            transfer_id = %transfer_id,
            label = %job.label,
            origin_scheme = origin.map(|origin| origin.scheme.as_str()).unwrap_or(""),
            origin_host = origin.map(|origin| origin.host.as_str()).unwrap_or(""),
            origin_port = ?origin.and_then(|origin| origin.port),
            origin = ?job.origin,
            priority = job.priority,
            pending_before = pending.len(),
            pending_same_origin_before = origin
                .map(|origin| pending_origin_count(pending, origin))
                .unwrap_or(0),
            stage = "curl_runtime_job_queued",
        );
    }
    let pending_job = CurlPendingJob {
        transfer_id,
        job,
        enqueued_at: Instant::now(),
    };
    if let Some(index) = pending
        .iter()
        .position(|queued| pending_job.job.priority > queued.job.priority)
    {
        pending.insert(index, pending_job);
    } else {
        pending.push_back(pending_job);
    }
}

fn enqueue_existing_pending_job<H: Handler, C>(
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

fn curl_runtime_trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        env_flag_enabled("MOLI_CDP_NAV_TIMING") || env_flag_enabled("MOLI_CURL_RUNTIME_TRACE")
    })
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

fn job_is_eligible<H: Handler, C>(
    origin: Option<&CurlOriginKey>,
    state: &CurlOwnerState<H, C>,
    max_active_per_host: Option<NonZeroUsize>,
) -> bool {
    match (origin, max_active_per_host) {
        (Some(origin), Some(limit)) => active_origin_count(&state.active, origin) < limit.get(),
        _ => true,
    }
}

fn active_origin_count<H: Handler, C>(
    active: &HashMap<CurlTransferId, CurlActiveTransfer<H, C>>,
    origin: &CurlOriginKey,
) -> usize {
    active
        .values()
        .filter(|active| active.origin.as_ref() == Some(origin))
        .count()
}

fn pending_origin_count<H: Handler, C>(
    pending: &VecDeque<CurlPendingJob<H, C>>,
    origin: &CurlOriginKey,
) -> usize {
    pending
        .iter()
        .filter(|pending| pending.job.origin.as_ref() == Some(origin))
        .count()
}

fn completed_transfers<H: Handler, C>(
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
fn take_transfers_in_notification_order<K, T, E>(
    active: &mut HashMap<K, T>,
    completed: Vec<(K, E)>,
) -> Vec<(K, T, E)>
where
    K: Copy + Eq + std::hash::Hash,
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

fn next_transfer_id() -> Result<CurlTransferId> {
    let value = next_nonzero_usize(&NEXT_CURL_TRANSFER_ID)
        .context("curl transfer identity space exhausted")?;
    Ok(CurlTransferId::new(value))
}

fn next_nonzero_usize(counter: &AtomicUsize) -> Result<NonZeroUsize> {
    let sequence = counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| anyhow!("identity space exhausted"))?;
    NonZeroUsize::new(sequence).ok_or_else(|| anyhow!("identity must be non-zero"))
}

fn make_runtime_multi(config: &CurlMultiRuntimeConfig) -> Multi {
    let mut multi = Multi::new();
    if let Some(max_host_connections) = config.max_host_connections
        && let Err(error) = multi.set_max_host_connections(max_host_connections.get())
    {
        debug!("failed to configure curl multi max_host_connections: {error}");
    }
    if let Some(max_total_connections) = config.max_total_connections {
        let max_total_connections = max_total_connections.get();
        if let Err(error) = multi.set_max_total_connections(max_total_connections) {
            debug!("failed to configure curl multi max_total_connections: {error}");
        }
        if let Err(error) = multi.set_max_connects(max_total_connections) {
            debug!("failed to configure curl multi max_connects: {error}");
        }
    }
    let max_concurrent_streams = config.max_concurrent_streams.map(NonZeroUsize::get);
    if let Some(max_concurrent_streams) = max_concurrent_streams
        && let Err(error) = multi.set_max_concurrent_streams(max_concurrent_streams)
    {
        debug!("failed to configure curl multi max_concurrent_streams: {error}");
    }
    if config.multiplex
        && let Err(error) = multi.pipelining(false, true)
    {
        debug!("failed to enable curl multi multiplexing: {error}");
    }
    multi
}

fn runtime_wait_timeout(multi: &Multi, poll_interval: Duration) -> Result<Duration> {
    let curl_timeout = multi
        .get_timeout()
        .context("failed to read curl multi timeout")?;
    Ok(curl_timeout
        .map(|timeout| timeout.min(poll_interval))
        .unwrap_or(poll_interval))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

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
        CurlTransferId::new(NonZeroUsize::new(sequence).expect("test transfer ID is non-zero"))
    }

    #[test]
    fn runtime_config_rejects_zero_poll_interval() {
        let config = CurlMultiRuntimeConfig {
            poll_interval: Duration::ZERO,
            ..CurlMultiRuntimeConfig::default()
        };

        let error = config
            .validate()
            .expect_err("zero runtime poll interval should fail")
            .to_string();

        assert!(
            error.contains("poll interval must be non-zero"),
            "unexpected error: {error}"
        );
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
    fn transfer_identity_uses_the_same_nonzero_value_as_its_token() {
        let transfer_id = test_transfer_id(7);

        assert_eq!(transfer_id.token(), 7);
        assert_eq!(CurlTransferId::from_token(7), Some(transfer_id));
        assert_eq!(CurlTransferId::from_token(0), None);
    }

    #[test]
    fn transfer_sequence_never_wraps_or_reuses_zero() {
        let next = AtomicUsize::new(1);
        assert_eq!(next_nonzero_usize(&next).unwrap().get(), 1);
        assert_eq!(next_nonzero_usize(&next).unwrap().get(), 2);

        let exhausted = AtomicUsize::new(usize::MAX);
        assert!(next_nonzero_usize(&exhausted).is_err());
        assert_eq!(exhausted.load(Ordering::Relaxed), usize::MAX);
    }

    #[test]
    fn submitted_identity_reaches_the_matching_runtime_completion() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .expect("test HTTP listener should bind to a local port");
        let address = listener
            .local_addr()
            .expect("test HTTP listener should have an address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("curl should connect to the test HTTP listener");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("test HTTP connection should accept a read timeout");
            let mut request = [0; 4096];
            let _ = stream
                .read(&mut request)
                .expect("test HTTP request should be readable");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("test HTTP response should be writable");
        });

        let (runtime, completion_rx) = CurlMultiRuntime::new(CurlMultiRuntimeConfig {
            poll_interval: Duration::from_millis(5),
            ..CurlMultiRuntimeConfig::default()
        })
        .expect("test curl runtime should start");
        let mut easy = Easy2::new(TestHandler);
        easy.url(&format!("http://{address}/identity"))
            .expect("test curl URL should be valid");
        let transfer_id = runtime
            .submit(CurlMultiJob {
                easy,
                context: "matching-context".to_owned(),
                origin: None,
                dns_resolution: CurlDnsResolution::curl_managed(),
                priority: 1,
                label: "identity-test".to_owned(),
            })
            .expect("test curl transfer should be accepted");

        let completion = completion_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("test curl transfer should reach terminal completion");
        assert_eq!(completion.transfer_id, transfer_id);
        assert_eq!(completion.context, "matching-context");
        assert!(completion.easy.is_some());
        completion
            .result
            .expect("test curl transfer should complete successfully");

        runtime.shutdown();
        server.join().expect("test HTTP server should finish");
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
